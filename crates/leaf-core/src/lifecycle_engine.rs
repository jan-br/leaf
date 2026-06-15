//! The bean-lifecycle metamodel + teardown machinery (bean-lifecycle phase3/04).
//!
//! This is the const, thin-macro-emitted lifecycle ABI the one `Engine::create`
//! driver consumes, plus the LIFO `TeardownLedger` that bracket-closes every
//! shared bean at shutdown. It rests entirely on the already-frozen toolkit
//! primitives — there is no new mechanism here, only typed consumers of them:
//!
//! - ONE const lifecycle metamodel: [`LifecyclePlan`] carries the mirrored
//!   init/destroy [`LifecycleStep`] tables (run forward / reverse), an
//!   [`AwareFlags`] bitset, the `smart_init` flag, and the `@DependsOn`
//!   [`ContractId`] list — all flat const beside the `Descriptor`. Callbacks are
//!   origin-agnostic [`LifecycleFn`] fn-pointers (`fn(&dyn Any, &Cx) ->
//!   BoxFuture`), participation is DATA (a flag/bitset), never a runtime
//!   `instanceof`.
//! - The typed escape-hatch traits ([`InitializingBean`]/[`DisposableBean`]/
//!   [`Closeable`]/[`AwareReady`]/[`AfterSingletonsReady`]) — a bean stays a POJO
//!   if it uses the annotation form, but these feed the SAME const table.
//! - ONE teardown path: the [`TeardownLedger`] of [`Destroyer`] entries, drained
//!   LIFO by `Context::shutdown().await`. A singleton publish pushes a destroyer;
//!   a prototype pushes NOTHING (never-destroyed, structural). There is no async
//!   `Drop` — the awaited drain IS the teardown.
//! - The concurrency-contract doctrine carried by [`ShareableBean`]'s
//!   `#[diagnostic::on_unimplemented]` (scope is the concurrency lever), NOT a
//!   raw trait-solver error.

use std::any::Any;
use std::sync::Mutex;

use crate::cx::Cx;
use crate::future::BoxFuture;
use crate::handle::ErasedBean;
use crate::identity::{BeanId, BeanName, ContractId};

// ─────────────────────────── CallbackError ──────────────────────────────────

/// The typed failure of an init / destroy / aware lifecycle callback.
///
/// Bridges into the one [`LeafError`](crate::LeafError) chain at the engine seam
/// via [`CallbackError::into_leaf_error`]; the variants preserve the bean, the
/// phase, and the offending [`StepId`] for a rich diagnostic.
#[derive(Debug, Clone)]
pub enum CallbackError {
    /// An init-phase callback failed (`@PostConstruct`/`afterPropertiesSet`/init).
    Init {
        /// The bean whose init callback failed.
        bean: BeanName,
        /// The init phase the failing step belonged to.
        phase: LifecyclePhase,
        /// The macro-assigned stable id of the failing step.
        step: StepId,
        /// The underlying cause.
        cause: crate::LeafError,
    },
    /// A destroy-phase callback failed (reported, never aborts the LIFO drain).
    Destroy {
        /// The bean whose destroy callback failed.
        bean: BeanName,
        /// The destroy phase the failing step belonged to.
        phase: LifecyclePhase,
        /// The underlying cause.
        cause: crate::LeafError,
    },
    /// An aware/`on_context_ready` callback failed.
    Aware {
        /// The bean whose aware callback failed.
        bean: BeanName,
        /// The underlying cause.
        cause: crate::LeafError,
    },
}

impl CallbackError {
    /// The bean name this callback failure is attributed to.
    #[must_use]
    pub fn bean(&self) -> &BeanName {
        match self {
            CallbackError::Init { bean, .. }
            | CallbackError::Destroy { bean, .. }
            | CallbackError::Aware { bean, .. } => bean,
        }
    }

    /// Fold this callback failure into the one [`LeafError`](crate::LeafError)
    /// chain, classified as a [`ConstructionFailed`](crate::ErrorKind::ConstructionFailed)
    /// runtime fault with a narrative naming the bean and phase.
    #[must_use]
    pub fn into_leaf_error(self) -> crate::LeafError {
        use crate::{Cause, ErrorKind, LeafError};
        match self {
            CallbackError::Init { bean, phase, step, cause } => {
                LeafError::new(ErrorKind::ConstructionFailed)
                    .caused_by(Cause::plain(
                        "running init callback",
                        format!("bean `{bean}` failed at {phase:?} (step {})", step.0),
                    ))
                    .caused_by(Cause::plain("cause", cause.to_string()))
            }
            CallbackError::Destroy { bean, phase, cause } => {
                LeafError::new(ErrorKind::ConstructionFailed)
                    .caused_by(Cause::plain(
                        "running destroy callback",
                        format!("bean `{bean}` failed at {phase:?}"),
                    ))
                    .caused_by(Cause::plain("cause", cause.to_string()))
            }
            CallbackError::Aware { bean, cause } => LeafError::new(ErrorKind::ConstructionFailed)
                .caused_by(Cause::plain(
                    "running aware callback",
                    format!("bean `{bean}` failed on_context_ready"),
                ))
                .caused_by(Cause::plain("cause", cause.to_string())),
        }
    }
}

impl std::fmt::Display for CallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallbackError::Init { bean, phase, .. } => {
                write!(f, "init callback failed for `{bean}` at {phase:?}")
            }
            CallbackError::Destroy { bean, phase, .. } => {
                write!(f, "destroy callback failed for `{bean}` at {phase:?}")
            }
            CallbackError::Aware { bean, .. } => {
                write!(f, "aware callback failed for `{bean}`")
            }
        }
    }
}

impl std::error::Error for CallbackError {}

impl From<CallbackError> for crate::LeafError {
    fn from(e: CallbackError) -> Self {
        e.into_leaf_error()
    }
}

// ─────────────────────────── LifecyclePhase ─────────────────────────────────

/// The canonical lifecycle phase ordering (bean-lifecycle `lifecycle-callbacks`).
///
/// Init phases run FORWARD in discriminant order
/// (`PostConstruct < AfterPropertiesSet < InitMethod`); destroy phases run in
/// REVERSE (`PreDestroy < DisposableDestroy < DestroyMethod`, drained last-first).
/// The discriminant IS the order, so [`cmp_order`](crate::cmp_order)-free sorting
/// is a stable sort on `as u8`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum LifecyclePhase {
    // ── init phases (forward) ──
    /// `@PostConstruct` — runs first.
    PostConstruct = 0,
    /// `InitializingBean::after_properties_set`.
    AfterPropertiesSet = 1,
    /// A declared custom init method.
    InitMethod = 2,
    // ── destroy phases (reverse) ──
    /// `@PreDestroy` — the first destroy phase (drained last).
    PreDestroy = 3,
    /// `DisposableBean::destroy`.
    DisposableDestroy = 4,
    /// A declared custom destroy method (or inferred `Closeable::close`).
    DestroyMethod = 5,
}

impl LifecyclePhase {
    /// `true` iff this is an init-side phase (`PostConstruct`/`AfterPropertiesSet`/
    /// `InitMethod`).
    #[must_use]
    pub const fn is_init(self) -> bool {
        matches!(
            self,
            LifecyclePhase::PostConstruct
                | LifecyclePhase::AfterPropertiesSet
                | LifecyclePhase::InitMethod
        )
    }

    /// `true` iff this is a destroy-side phase.
    #[must_use]
    pub const fn is_destroy(self) -> bool {
        !self.is_init()
    }
}

// ─────────────────────────── LifecycleFn / StepId ───────────────────────────

/// The origin-agnostic lifecycle callback fn-pointer (bean-lifecycle).
///
/// `fn(&(dyn Any + Send + Sync), &Cx) -> BoxFuture<Result<(), CallbackError>>` —
/// boxed-future-on-`dyn` (the fixed async-across-`dyn` standard, since AFIT is not
/// `dyn`-compatible). The macro emits ONE such fn-pointer per annotated init/
/// destroy method (downcasting the erased handle to the concrete type inside),
/// so a plain annotated method stays a POJO.
pub type LifecycleFn =
    for<'a> fn(&'a (dyn Any + Send + Sync), &'a Cx) -> BoxFuture<'a, Result<(), CallbackError>>;

/// A macro-assigned stable id per source method (bean-lifecycle dedup).
///
/// Identity for DEDUP: a method serving two mechanisms (e.g. both `@PostConstruct`
/// and a custom init) collapses to ONE step keyed by this id (earlier init phase
/// wins / later destroy phase wins). NOT fn-pointer equality (which has
/// codegen-unit caveats).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct StepId(pub u32);

/// One lifecycle step: a phase, a callback, and its dedup [`StepId`].
#[derive(Clone, Copy)]
pub struct LifecycleStep {
    /// The phase this step belongs to (drives forward/reverse ordering).
    pub phase: LifecyclePhase,
    /// The origin-agnostic callback fn-pointer.
    pub call: LifecycleFn,
    /// The stable dedup id of the source method.
    pub id: StepId,
}

impl std::fmt::Debug for LifecycleStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The fn-pointer is not meaningfully printable.
        f.debug_struct("LifecycleStep")
            .field("phase", &self.phase)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

// ─────────────────────────── AwareFlags (bitset) ────────────────────────────

/// A cheap bitset of which aware capabilities a bean wants (bean-lifecycle
/// `aware-callbacks`).
///
/// The macro emits this beside the `Descriptor`; `run_aware` does a cheap bitmask
/// skip (no speculative downcast). Constructor injection is the PRIMARY aware
/// door — these flags drive only the residual post-population
/// [`AwareReady::on_context_ready`] grouped hook.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AwareFlags(pub u16);

impl AwareFlags {
    /// No aware capabilities wanted (the common case).
    pub const NONE: AwareFlags = AwareFlags(0);
    /// The bean wants the grouped `on_context_ready` post-population hook.
    pub const CONTEXT_READY: AwareFlags = AwareFlags(1 << 0);

    /// `true` iff NO aware capability is wanted (the cheap skip predicate).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `true` iff every bit in `other` is set in `self`.
    #[must_use]
    pub const fn contains(self, other: AwareFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union of two flag sets.
    #[must_use]
    pub const fn union(self, other: AwareFlags) -> AwareFlags {
        AwareFlags(self.0 | other.0)
    }
}

impl std::fmt::Debug for AwareFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AwareFlags(0b{:016b})", self.0)
    }
}

// ─────────────────────────── Bootstrap ──────────────────────────────────────

/// Whether a bean's eager construction is spawned in the background
/// (bean-lifecycle `background-bootstrap`).
///
/// A descriptor-side flag, NOT an API surface: `Background` => the bean's
/// `Engine::create` future is `Spawner::spawn`ed during the eager wave and joined
/// before refresh; `Default` => constructed inline on the bootstrap task.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Bootstrap {
    /// Constructed inline on the bootstrap task (the default).
    #[default]
    Default,
    /// Spawned on the bootstrap executor (`Spawner`); joined before refresh.
    Background,
}

// ─────────────────────────── LifecyclePlan ──────────────────────────────────

/// THE const lifecycle metamodel — one per bean, macro-emitted beside the
/// `Descriptor` (bean-lifecycle `lifecycle-callbacks`).
///
/// Two MIRRORED chains as DATA: `init` runs FORWARD in phase order,
/// `destroy` runs in REVERSE. `aware_wants` is the cheap-skip bitset, `smart_init`
/// gates the `after_singletons_ready` participation, and `depends_on` carries the
/// `@DependsOn` forced construction edges (folded into the construction graph at
/// `App<Wired>::validate()`). A flat const record (the same thin-macro/`::leaf_core`
/// discipline as the `Descriptor`); the mirror cannot drift because both chains
/// live in one table.
#[derive(Clone, Copy, Debug)]
pub struct LifecyclePlan {
    /// Init steps in canonical phase order, run FORWARD.
    pub init: &'static [LifecycleStep],
    /// Destroy steps, run in REVERSE.
    pub destroy: &'static [LifecycleStep],
    /// The cheap-skip aware-want bitset.
    pub aware_wants: AwareFlags,
    /// Whether the bean participates in `after_singletons_ready`.
    pub smart_init: bool,
    /// `@DependsOn` forced construction edges, by stable cross-build identity.
    pub depends_on: &'static [ContractId],
    /// Whether eager construction is spawned in the background.
    pub bootstrap: Bootstrap,
}

impl LifecyclePlan {
    /// The empty/default plan: no callbacks, no awares, not smart-init, no
    /// depends-on, foreground bootstrap. The common POJO case.
    pub const EMPTY: LifecyclePlan = LifecyclePlan {
        init: &[],
        destroy: &[],
        aware_wants: AwareFlags::NONE,
        smart_init: false,
        depends_on: &[],
        bootstrap: Bootstrap::Default,
    };

    /// `true` iff this bean has any init or destroy callbacks at all.
    #[must_use]
    pub const fn has_callbacks(&self) -> bool {
        !self.init.is_empty() || !self.destroy.is_empty()
    }
}

impl Default for LifecyclePlan {
    fn default() -> Self {
        LifecyclePlan::EMPTY
    }
}

// ─────────────────────── typed escape-hatch traits ──────────────────────────

/// The typed `afterPropertiesSet` escape hatch (bean-lifecycle). A bean MAY impl
/// this instead of annotating a method; both feed the same const [`LifecyclePlan`].
pub trait InitializingBean: Send + Sync {
    /// Run after all properties are populated (the `AfterPropertiesSet` phase).
    ///
    /// # Errors
    /// Returns a [`CallbackError`] if initialization fails.
    fn after_properties_set(&self) -> BoxFuture<'_, Result<(), CallbackError>>;
}

/// The typed `destroy` escape hatch (bean-lifecycle), mirrored to
/// [`InitializingBean`].
pub trait DisposableBean: Send + Sync {
    /// Logically destroy this bean (the `DisposableDestroy` phase).
    ///
    /// # Errors
    /// Returns a [`CallbackError`] if destruction fails.
    fn destroy(&self) -> BoxFuture<'_, Result<(), CallbackError>>;
}

/// The destroy-method inference target (bean-lifecycle): a bean exposing
/// `close()` gets a synthetic `DestroyMethod` step UNLESS `#[no_destroy_inference]`.
pub trait Closeable: Send + Sync {
    /// Close this resource at shutdown.
    ///
    /// # Errors
    /// Returns a [`CallbackError`] if closing fails.
    fn close(&self) -> BoxFuture<'_, Result<(), CallbackError>>;
}

/// The grouped post-population aware hook (bean-lifecycle `aware-callbacks`).
///
/// The SECONDARY door for registration-facts knowable only after registration
/// (a bean's own name, the Context handle); constructor injection is the primary
/// door. Gated by [`AwareFlags::CONTEXT_READY`] for a cheap skip.
pub trait AwareReady: Send + Sync {
    /// Fire at the aware slot of `run_init` with the always-present infra bundle.
    ///
    /// # Errors
    /// Returns a [`CallbackError`] if the aware step fails.
    fn on_context_ready(&self) -> BoxFuture<'_, Result<(), CallbackError>>;
}

/// The lock-free, complete-graph, once-per-refresh smart-init hook
/// (bean-lifecycle `smart-initializing`). Fires after the final eager wave,
/// in `cmp_order`, outside any creation guard.
pub trait AfterSingletonsReady: Send + Sync {
    /// Fire once after all eager singletons are published.
    ///
    /// # Errors
    /// Returns a [`CallbackError`]; a failure aborts start all-or-nothing.
    fn after_singletons_ready(&self) -> BoxFuture<'_, Result<(), CallbackError>>;
}

// ─────────────────────── ShareableBean doctrine ─────────────────────────────

/// The concurrency-contract doctrine, carried as a TYPE BOUND with a steering
/// diagnostic (bean-lifecycle `concurrency-contract`).
///
/// No code of its own: the contract IS the `Send + Sync + 'static` bound that
/// rides [`ErasedBean`]/[`Bean`](crate::Bean). A shared-scope bean that holds
/// mutable per-interaction state should be made prototype/request-scoped — scope
/// is the concurrency lever. The diagnostic steers there instead of a cryptic
/// trait-solver error. Blanket-impl'd for every `Send + Sync + 'static` type.
#[diagnostic::on_unimplemented(
    message = "shared-scope bean `{Self}` must be `Send + Sync` (it is shared across executor threads)",
    note = "if it holds mutable per-interaction state, make it prototype- or request-scoped — scope is the concurrency lever"
)]
pub trait ShareableBean: Send + Sync + 'static {}

impl<T: Send + Sync + 'static> ShareableBean for T {}

// ─────────────────────────── Destroyer ──────────────────────────────────────

/// One teardown unit pushed onto the [`TeardownLedger`] when a SHARED bean is
/// published (bean-lifecycle `lifecycle-callbacks`).
///
/// A prototype publish pushes NOTHING (never-destroyed, structural). `run` is a
/// boxed `FnOnce` closing over the destroy chain + the bean's [`ErasedBean`]; the
/// drain awaits each in LIFO order. No async `Drop` — this awaited closure IS the
/// teardown.
pub struct Destroyer {
    /// The bean whose destruction this entry runs (diagnostics / reverse order).
    pub bean_id: BeanId,
    /// The boxed async teardown closure (the destroy chain over the handle).
    pub run: Box<dyn FnOnce() -> BoxFuture<'static, Result<(), CallbackError>> + Send>,
}

impl Destroyer {
    /// Build a destroyer for `bean_id` from a teardown closure.
    pub fn new(
        bean_id: BeanId,
        run: impl FnOnce() -> BoxFuture<'static, Result<(), CallbackError>> + Send + 'static,
    ) -> Self {
        Destroyer { bean_id, run: Box::new(run) }
    }

    /// Build a destroyer that runs the const [`LifecyclePlan::destroy`] chain (in
    /// REVERSE phase order) over a shared `bean` handle and its ambient `cx`.
    ///
    /// This is the canonical singleton destroyer the publish step registers; a
    /// plan with no destroy steps still produces a no-op destroyer (so the ledger
    /// ordering — LIFO over the publication order — is uniform).
    #[must_use]
    pub fn for_plan(bean_id: BeanId, plan: LifecyclePlan, bean: ErasedBean, cx: Cx) -> Self {
        Destroyer::new(bean_id, move || {
            Box::pin(async move {
                run_destroy(&plan, bean.as_ref(), &cx).await
            })
        })
    }

    /// Run this destroyer's teardown closure (consuming it).
    pub fn run(self) -> BoxFuture<'static, Result<(), CallbackError>> {
        (self.run)()
    }
}

impl std::fmt::Debug for Destroyer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Destroyer").field("bean_id", &self.bean_id).finish_non_exhaustive()
    }
}

/// Run a plan's INIT chain FORWARD over `bean`, stopping at the first failure.
///
/// Returns the index of the step that ran-up-to-but-not-including on failure (for
/// the partial-destroy mirror), so `Engine::create` can run the destroy steps for
/// the init phases that already completed on the unwind.
///
/// # Errors
/// Returns the failing step's [`CallbackError`].
pub async fn run_init(
    plan: &LifecyclePlan,
    bean: &(dyn Any + Send + Sync),
    cx: &Cx,
) -> Result<(), CallbackError> {
    for step in plan.init {
        (step.call)(bean, cx).await?;
    }
    Ok(())
}

/// Run a plan's DESTROY chain in REVERSE phase order over `bean`.
///
/// Unlike init, a destroy fault is NOT fatal to the rest of the drain: each step
/// is attempted and the FIRST error is returned after all steps run (the ledger
/// drain reports faults, never aborts).
///
/// # Errors
/// Returns the first destroy step's [`CallbackError`], after attempting all steps.
pub async fn run_destroy(
    plan: &LifecyclePlan,
    bean: &(dyn Any + Send + Sync),
    cx: &Cx,
) -> Result<(), CallbackError> {
    let mut first_err: Option<CallbackError> = None;
    // Destroy steps run in REVERSE of the (forward) declared order.
    for step in plan.destroy.iter().rev() {
        if let Err(e) = (step.call)(bean, cx).await
            && first_err.is_none()
        {
            first_err = Some(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ─────────────────────────── TeardownLedger ─────────────────────────────────

/// The ONE teardown path: a LIFO ledger of [`Destroyer`] entries drained at
/// shutdown (bean-lifecycle `lifecycle-callbacks` / ownership-model).
///
/// `Engine::create`'s singleton publish step pushes a destroyer; a prototype
/// pushes nothing. The drain pops LIFO (reverse publication order = reverse
/// `@DependsOn`/wiring-wave order, so a depends-on target tears down AFTER its
/// dependent). A SINGLE serialization owner (the inner [`Mutex`]) so a push and a
/// drain never race. There is no async `Drop`: `Context::shutdown().await` calls
/// [`drain`](TeardownLedger::drain).
pub struct TeardownLedger {
    entries: Mutex<Vec<Destroyer>>,
}

impl TeardownLedger {
    /// A fresh, empty ledger.
    #[must_use]
    pub fn new() -> Self {
        TeardownLedger { entries: Mutex::new(Vec::new()) }
    }

    /// Push a [`Destroyer`] in publication order (drained LIFO).
    pub fn push(&self, destroyer: Destroyer) {
        self.entries
            .lock()
            .expect("TeardownLedger mutex poisoned")
            .push(destroyer);
    }

    /// The number of pending destroyers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().expect("TeardownLedger mutex poisoned").len()
    }

    /// `true` iff the ledger holds no pending destroyers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Take all pending destroyers in LIFO (reverse-publication) order, leaving
    /// the ledger empty. The drain runner awaits each `run()` in turn.
    #[must_use]
    pub fn take_lifo(&self) -> Vec<Destroyer> {
        let mut guard = self.entries.lock().expect("TeardownLedger mutex poisoned");
        let mut v = std::mem::take(&mut *guard);
        v.reverse();
        v
    }

    /// Drain the ledger LIFO, awaiting every destroyer.
    ///
    /// Each destroyer is run in turn (reverse publication order); a fault is
    /// COLLECTED, never aborts the rest of the drain. Returns the bean ids that
    /// were drained (publication order is the reverse of this) and any errors.
    pub async fn drain(&self) -> TeardownOutcome {
        let destroyers = self.take_lifo();
        let mut order = Vec::with_capacity(destroyers.len());
        let mut errors = Vec::new();
        for d in destroyers {
            let bean_id = d.bean_id;
            order.push(bean_id);
            if let Err(e) = d.run().await {
                errors.push(e);
            }
        }
        TeardownOutcome { order, errors }
    }
}

impl Default for TeardownLedger {
    fn default() -> Self {
        TeardownLedger::new()
    }
}

impl std::fmt::Debug for TeardownLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeardownLedger").field("pending", &self.len()).finish()
    }
}

/// The result of a [`TeardownLedger::drain`]: the LIFO drain order + any faults.
#[derive(Debug, Default)]
pub struct TeardownOutcome {
    /// The `BeanId`s in the LIFO order they were destroyed (drain order).
    pub order: Vec<BeanId>,
    /// Destroy faults collected during the drain (never aborted the drain).
    pub errors: Vec<CallbackError>,
}

impl TeardownOutcome {
    /// `true` iff every destroyer ran without error.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
}

// ─────────────────────────── InstanceStore ──────────────────────────────────

/// A per-context-scope (request/session/custom) bare-bean store, reached through
/// the ambient `Cx` (bean-lifecycle `scopes`).
///
/// Mirrors the singleton `OnceCell` but keyed INSIDE a per-context map and DROPPED
/// at scope end. It holds the BARE [`ErasedBean`] (proxying for a `PerContextKey`
/// bean happens at the injection seam over a `ScopeTarget`, per seam #1 R4 — the
/// store never holds a proxy). `get_or_init` mirrors the singleton at-most-once
/// contract; [`ledger`](InstanceStore::ledger) is the per-scope teardown drained
/// at scope end. NEVER a `Box<dyn Scope>` SPI — this is a concrete store reached
/// via a `CxKey`, never a five-method engine-called strategy.
pub trait InstanceStore: Send + Sync {
    /// Memoize-or-create the bean for `id` in this scope store.
    ///
    /// The closure builds the bean (the `Engine::create` publish pipeline for the
    /// scoped bean); `get_or_init` guarantees at-most-once per `id` in this scope.
    ///
    /// # Errors
    /// Propagates a [`LeafError`](crate::LeafError) from the build closure.
    fn get_or_init<'a>(
        &'a self,
        id: BeanId,
        build: BoxFuture<'a, Result<ErasedBean, crate::LeafError>>,
    ) -> BoxFuture<'a, Result<ErasedBean, crate::LeafError>>;

    /// The per-scope teardown ledger, drained at scope end.
    fn ledger(&self) -> &TeardownLedger;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    // ── LifecyclePhase ordering ──────────────────────────────────────────────

    #[test]
    fn init_phases_order_forward_destroy_after() {
        assert!(LifecyclePhase::PostConstruct < LifecyclePhase::AfterPropertiesSet);
        assert!(LifecyclePhase::AfterPropertiesSet < LifecyclePhase::InitMethod);
        assert!(LifecyclePhase::InitMethod < LifecyclePhase::PreDestroy);
        assert!(LifecyclePhase::PreDestroy < LifecyclePhase::DisposableDestroy);
        assert!(LifecyclePhase::DisposableDestroy < LifecyclePhase::DestroyMethod);
        assert!(LifecyclePhase::PostConstruct.is_init());
        assert!(LifecyclePhase::DestroyMethod.is_destroy());
        assert!(!LifecyclePhase::InitMethod.is_destroy());
    }

    // ── AwareFlags bitset ─────────────────────────────────────────────────────

    #[test]
    fn aware_flags_bitset_skip_and_union() {
        assert!(AwareFlags::NONE.is_empty());
        assert!(!AwareFlags::CONTEXT_READY.is_empty());
        assert!(AwareFlags::CONTEXT_READY.contains(AwareFlags::CONTEXT_READY));
        assert!(!AwareFlags::NONE.contains(AwareFlags::CONTEXT_READY));
        let both = AwareFlags::NONE.union(AwareFlags::CONTEXT_READY);
        assert!(both.contains(AwareFlags::CONTEXT_READY));
        assert!(format!("{both:?}").contains("AwareFlags"));
    }

    // ── LifecyclePlan const ───────────────────────────────────────────────────

    #[test]
    fn empty_plan_is_the_pojo_default() {
        let p = LifecyclePlan::EMPTY;
        assert!(p.init.is_empty());
        assert!(p.destroy.is_empty());
        assert!(p.aware_wants.is_empty());
        assert!(!p.smart_init);
        assert!(p.depends_on.is_empty());
        assert_eq!(p.bootstrap, Bootstrap::Default);
        assert!(!p.has_callbacks());
        assert!(!LifecyclePlan::default().smart_init);
    }

    // ── init/destroy callbacks run in mirrored order ─────────────────────────

    // A test bean recording the order in which its callbacks fired.
    struct Recorder {
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    fn post_construct<'a>(
        bean: &'a (dyn Any + Send + Sync),
        _cx: &'a Cx,
    ) -> BoxFuture<'a, Result<(), CallbackError>> {
        Box::pin(async move {
            let r = bean.downcast_ref::<Recorder>().expect("Recorder");
            r.log.lock().unwrap().push("post_construct");
            Ok(())
        })
    }
    fn init_method<'a>(
        bean: &'a (dyn Any + Send + Sync),
        _cx: &'a Cx,
    ) -> BoxFuture<'a, Result<(), CallbackError>> {
        Box::pin(async move {
            let r = bean.downcast_ref::<Recorder>().expect("Recorder");
            r.log.lock().unwrap().push("init_method");
            Ok(())
        })
    }
    fn pre_destroy<'a>(
        bean: &'a (dyn Any + Send + Sync),
        _cx: &'a Cx,
    ) -> BoxFuture<'a, Result<(), CallbackError>> {
        Box::pin(async move {
            let r = bean.downcast_ref::<Recorder>().expect("Recorder");
            r.log.lock().unwrap().push("pre_destroy");
            Ok(())
        })
    }
    fn destroy_method<'a>(
        bean: &'a (dyn Any + Send + Sync),
        _cx: &'a Cx,
    ) -> BoxFuture<'a, Result<(), CallbackError>> {
        Box::pin(async move {
            let r = bean.downcast_ref::<Recorder>().expect("Recorder");
            r.log.lock().unwrap().push("destroy_method");
            Ok(())
        })
    }

    static INIT_STEPS: &[LifecycleStep] = &[
        LifecycleStep { phase: LifecyclePhase::PostConstruct, call: post_construct, id: StepId(1) },
        LifecycleStep { phase: LifecyclePhase::InitMethod, call: init_method, id: StepId(2) },
    ];
    static DESTROY_STEPS: &[LifecycleStep] = &[
        LifecycleStep { phase: LifecyclePhase::PreDestroy, call: pre_destroy, id: StepId(3) },
        LifecycleStep { phase: LifecyclePhase::DestroyMethod, call: destroy_method, id: StepId(4) },
    ];

    #[test]
    fn init_runs_forward_destroy_runs_reverse() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let bean = Recorder { log: log.clone() };
        let plan = LifecyclePlan {
            init: INIT_STEPS,
            destroy: DESTROY_STEPS,
            ..LifecyclePlan::EMPTY
        };
        let cx = Cx::empty();
        block(run_init(&plan, &bean, &cx)).expect("init ok");
        block(run_destroy(&plan, &bean, &cx)).expect("destroy ok");
        let got = log.lock().unwrap().clone();
        // Init forward (PostConstruct then InitMethod); destroy reverse
        // (DestroyMethod then PreDestroy).
        assert_eq!(
            got,
            vec!["post_construct", "init_method", "destroy_method", "pre_destroy"]
        );
    }

    // ── TeardownLedger drains LIFO ────────────────────────────────────────────

    #[test]
    fn teardown_ledger_drains_lifo() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let ledger = TeardownLedger::new();
        for i in 0..3u32 {
            let order = order.clone();
            ledger.push(Destroyer::new(BeanId(i), move || {
                Box::pin(async move {
                    order.lock().unwrap().push(i);
                    Ok(())
                })
            }));
        }
        assert_eq!(ledger.len(), 3);
        let outcome = block(ledger.drain());
        // Pushed 0,1,2 → drained LIFO 2,1,0.
        assert_eq!(outcome.order, vec![BeanId(2), BeanId(1), BeanId(0)]);
        assert_eq!(*order.lock().unwrap(), vec![2, 1, 0]);
        assert!(outcome.is_clean());
        assert!(ledger.is_empty());
    }

    #[test]
    fn teardown_drain_collects_faults_without_aborting() {
        let ran = Arc::new(AtomicUsize::new(0));
        let ledger = TeardownLedger::new();
        // First-pushed (drained LAST) fails; both should still run.
        let r0 = ran.clone();
        ledger.push(Destroyer::new(BeanId(0), move || {
            Box::pin(async move {
                r0.fetch_add(1, Ordering::SeqCst);
                Err(CallbackError::Destroy {
                    bean: BeanName::from("b0"),
                    phase: LifecyclePhase::DestroyMethod,
                    cause: crate::LeafError::new(crate::ErrorKind::ConstructionFailed),
                })
            })
        }));
        let r1 = ran.clone();
        ledger.push(Destroyer::new(BeanId(1), move || {
            Box::pin(async move {
                r1.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }));
        let outcome = block(ledger.drain());
        assert_eq!(ran.load(Ordering::SeqCst), 2, "both ran despite a fault");
        assert_eq!(outcome.errors.len(), 1);
        assert!(!outcome.is_clean());
    }

    // ── Destroyer::for_plan runs the destroy chain ────────────────────────────

    #[test]
    fn destroyer_for_plan_runs_reverse_destroy_chain() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let bean: ErasedBean = Arc::new(Recorder { log: log.clone() });
        let plan = LifecyclePlan { destroy: DESTROY_STEPS, ..LifecyclePlan::EMPTY };
        let d = Destroyer::for_plan(BeanId(7), plan, bean, Cx::empty());
        assert_eq!(d.bean_id, BeanId(7));
        block(d.run()).expect("destroy ran");
        assert_eq!(*log.lock().unwrap(), vec!["destroy_method", "pre_destroy"]);
    }

    // ── typed escape-hatch traits are object-safe ─────────────────────────────

    struct TypedBean;
    impl InitializingBean for TypedBean {
        fn after_properties_set(&self) -> BoxFuture<'_, Result<(), CallbackError>> {
            Box::pin(async { Ok(()) })
        }
    }
    impl DisposableBean for TypedBean {
        fn destroy(&self) -> BoxFuture<'_, Result<(), CallbackError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn typed_lifecycle_traits_are_object_safe() {
        let init: Arc<dyn InitializingBean> = Arc::new(TypedBean);
        let disp: Arc<dyn DisposableBean> = Arc::new(TypedBean);
        block(init.after_properties_set()).expect("init");
        block(disp.destroy()).expect("destroy");
    }

    // ── CallbackError folds into the one LeafError chain ──────────────────────

    #[test]
    fn callback_error_folds_into_leaf_error() {
        let e = CallbackError::Init {
            bean: BeanName::from("svc"),
            phase: LifecyclePhase::PostConstruct,
            step: StepId(1),
            cause: crate::LeafError::new(crate::ErrorKind::ConstructionFailed),
        };
        assert_eq!(e.bean().as_ref(), "svc");
        let le: crate::LeafError = e.into();
        assert_eq!(le.kind, crate::ErrorKind::ConstructionFailed);
    }

    // ── ShareableBean doctrine bound ──────────────────────────────────────────

    #[test]
    fn shareable_bean_blanket_impls_for_send_sync_static() {
        fn assert_shareable<T: ShareableBean>() {}
        assert_shareable::<String>();
        assert_shareable::<Recorder>();
    }
}
