//! The container shape: the bare inert [`Engine`] + the [`Context`] façade, and
//! the ONE concrete `Engine::create` creation driver (registry-core
//! `container-core` + bean-lifecycle `bean-instantiation`).
//!
//! Two named types, exactly as fixed by ADR-05:
//!
//! - [`Engine`] is the bare inert DI engine (DefaultListableBeanFactory analogue):
//!   it HAS-A the frozen [`Registry`] (the dense rows/providers/`singletons`
//!   `OnceCell` store), an [`EnginePolicy`], a [`Selector`], and the ONE container
//!   [`TeardownLedger`]. There is NO `dyn Engine` trait and NO pluggable-strategy
//!   kernel — `create` is one concrete driver.
//! - [`Context`] is the façade (ApplicationContext analogue) that HAS-A exactly
//!   one [`Engine`] + the context-service handles (here: the env + parent link;
//!   richer services land in leaf-boot), and delegates the BeanFactory surface by
//!   hand-forwarded inherent methods (no re-declared public trait, to avoid
//!   surface-drift).
//!
//! ## The single-phase `Engine::create` driver
//!
//! `create(id, cx)` reads `scope.multiplicity` and runs ONE pipeline:
//! `provide → run_init → publish`, with the proxy/after-init swap left to the
//! seam owner (leaf-boot's ProxyPlan). Populate is FUSED into `provide` (the
//! macro-emitted factory's typed params ARE the injection points), so there is no
//! separate populate step and no early-exposure middle cache (single-phase).
//!
//! - [`Multiplicity::Once`] → the per-slot `OnceCell` is the at-most-once
//!   publication guard. Concurrent first-creators race; exactly one commits and
//!   every later observer gets the SAME `Arc` (the atomic `Arc` clone IS the
//!   happens-before edge — no singleton mutex, no global lock). A managed-teardown
//!   singleton registers a [`Destroyer`] on the container ledger.
//! - [`Multiplicity::PerResolution`] → the prototype Owned-move lane: run the
//!   pipeline once, write no store slot, register no destroyer — a
//!   [`Published::Owned`] hand-off the container retains NOTHING of.
//! - [`Multiplicity::PerContextKey`] → resolve the ambient [`InstanceStore`] for
//!   the scope kind via the `cx` and memoize there (the store holds the bare
//!   [`ErasedBean`]).
//!
//! ## Publication / `OnceCell` note
//!
//! `Provider::provide` is async but [`once_cell::sync::OnceCell`] has no async
//! initializer on stable. The single-phase guard is therefore *build-then-commit*:
//! read the cell (lock-free ready path); on a miss, run the async pipeline OFF any
//! held guard (no `.await` under a lock — Spring's `@PostConstruct`-under-lock
//! deadlock dissolves), then `set` the cell. If `set` loses the race, the winner's
//! already-published `Arc` is returned and the loser's build is dropped. This is
//! the "race into the slot, exactly one wins, lock-free read after" contract; the
//! only cost is a rare redundant build of an independent bean (the wave plan
//! proves intra-wave independence, so genuine same-bean contention is rare).

use std::any::TypeId;
use std::sync::Arc;

use crate::definition::{Descriptor, Multiplicity, ScopeDef, TeardownPolicy};
use crate::env::Env;
use crate::error::{Cause, ErrorKind, LeafError};
use crate::handle::{downcast_owned, downcast_ref, ErasedBean, Published, Ref};
use crate::identity::{BeanId, BeanKey};
use crate::injection::Selector;
use crate::lifecycle_engine::{run_init, Destroyer, LifecyclePlan, TeardownLedger};
use crate::provider::ResolveCtx;
use crate::registry::{Registry, RegistryBuilder};

// ─────────────────────────── EnginePolicy ───────────────────────────────────

/// The bare inert engine's three policy toggles (registry-core `container-core`).
///
/// `allow_override` mirrors the builder's name-collision tolerance; `allow_circular`
/// is the (off-by-default) constructor-cycle escape; `strict_locking` is a
/// reserved knob (the per-slot `OnceCell` is the only guard regardless). All
/// default to the fail-fast/loud setting.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EnginePolicy {
    /// Tolerate a duplicate canonical name (a later registration replaces an
    /// earlier one). The genuinely-loud override case; auto-config soft override
    /// rides `CandidateRole::FALLBACK`, never this.
    pub allow_override: bool,
    /// Tolerate a constructor-injection cycle (off by default; the only sanctioned
    /// break is a deferral edge).
    pub allow_circular: bool,
    /// Reserved: the per-slot `OnceCell` is the only creation guard regardless.
    pub strict_locking: bool,
}

// ─────────────────────── how the lifecycle plan is found ─────────────────────

/// How the engine obtains a bean's const [`LifecyclePlan`].
///
/// The macro emits the plan beside the `Descriptor`; until a `Descriptor` carries
/// a `lifecycle` field (a later additive ABI change), the engine consults this
/// resolver (defaulting to [`LifecyclePlan::EMPTY`] — the POJO case). Tests and
/// leaf-boot install a richer resolver. Kept as a boxed `Fn` so the plan lookup is
/// origin-agnostic and does not require widening the frozen `Descriptor` row yet.
type PlanResolver = Box<dyn Fn(BeanId, &Descriptor) -> LifecyclePlan + Send + Sync>;

// ─────────────────────────── the Engine ─────────────────────────────────────

/// The bare inert DI engine (registry-core `container-core`).
///
/// Owns the frozen [`Registry`] (with its slot-indexed `OnceCell` singleton
/// store), an [`EnginePolicy`], a [`Selector`], the ONE container
/// [`TeardownLedger`], and the lifecycle-plan resolver. It runs ZERO decorators
/// (the `Context` installs infrastructure). The ONE concrete creation driver is
/// [`Engine::create`].
pub struct Engine {
    registry: Registry,
    policy: EnginePolicy,
    #[allow(dead_code)] // exercised once candidate-narrowing wiring lands in leaf-boot
    selector: Selector,
    ledger: Arc<TeardownLedger>,
    plan_of: PlanResolver,
}

impl Engine {
    /// Build an engine over a frozen [`Registry`] with default policy.
    #[must_use]
    pub fn new(registry: Registry) -> Self {
        Engine::with_policy(registry, EnginePolicy::default())
    }

    /// Build an engine over a frozen [`Registry`] with an explicit [`EnginePolicy`].
    #[must_use]
    pub fn with_policy(registry: Registry, policy: EnginePolicy) -> Self {
        Engine {
            registry,
            policy,
            selector: Selector,
            ledger: Arc::new(TeardownLedger::new()),
            plan_of: Box::new(|_, _| LifecyclePlan::EMPTY),
        }
    }

    /// Install a lifecycle-plan resolver (the macro-emitted plan source); builder
    /// style. Until the `Descriptor` row carries a `lifecycle` field, this is how
    /// leaf-boot/tests wire init/destroy callbacks into the driver.
    #[must_use]
    pub fn with_plan_resolver(
        mut self,
        resolver: impl Fn(BeanId, &Descriptor) -> LifecyclePlan + Send + Sync + 'static,
    ) -> Self {
        self.plan_of = Box::new(resolver);
        self
    }

    /// The frozen registry this engine drives.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The engine's policy.
    #[must_use]
    pub fn policy(&self) -> EnginePolicy {
        self.policy
    }

    /// The ONE container teardown ledger (drained LIFO at shutdown).
    #[must_use]
    pub fn ledger(&self) -> &Arc<TeardownLedger> {
        &self.ledger
    }

    /// `true` iff `key` resolves to at least one registered bean.
    #[must_use]
    pub fn contains(&self, key: &BeanKey) -> bool {
        self.registry.contains(key)
    }

    /// Resolve + create a SHARED bean by concrete type, returning a typed [`Ref<T>`].
    ///
    /// For singleton/scoped beans (the [`Published::Shared`] lane). A prototype
    /// type resolved here is an error (use [`get_owned`](Engine::get_owned)).
    ///
    /// # Errors
    /// [`ErrorKind::NoSuchBean`]/[`ErrorKind::NoUniqueBean`] on resolution, a
    /// construction fault, or a type mismatch.
    pub async fn get<T: crate::handle::Bean>(&self) -> Result<Ref<T>, LeafError> {
        let id = self.registry.resolve_id(&BeanKey::ByType(TypeId::of::<T>()))?;
        let cx = ResolveCtx::root();
        let published = self.create(id, &cx).await?;
        match published.into_shared() {
            Some(bean) => downcast_ref::<T>(bean).map_err(|_| type_mismatch::<T>()),
            None => Err(owned_via_shared::<T>()),
        }
    }

    /// Resolve + create a PROTOTYPE bean by concrete type, returning an owned `T`
    /// (the [`Published::Owned`] move lane).
    ///
    /// # Errors
    /// As [`get`](Engine::get); also errors if the resolved bean is shared, not
    /// owned (use [`get`](Engine::get)).
    pub async fn get_owned<T: 'static>(&self) -> Result<T, LeafError> {
        let id = self.registry.resolve_id(&BeanKey::ByType(TypeId::of::<T>()))?;
        let cx = ResolveCtx::root();
        let published = self.create(id, &cx).await?;
        match published.into_owned() {
            Some(boxed) => downcast_owned::<T>(boxed).map_err(|_| type_mismatch::<T>()),
            None => Err(shared_via_owned::<T>()),
        }
    }

    /// Resolve + create a bean by [`BeanKey`], returning the origin-agnostic
    /// shared [`ErasedBean`] (the dynamic lane).
    ///
    /// # Errors
    /// As [`get`](Engine::get); errors if the resolved bean is a prototype
    /// (owned-move), which has no shared handle.
    pub async fn get_erased(&self, key: BeanKey) -> Result<ErasedBean, LeafError> {
        let id = self.registry.resolve_id(&key)?;
        let cx = ResolveCtx::root();
        let published = self.create(id, &cx).await?;
        published.into_shared().ok_or_else(|| {
            LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
                "resolving erased bean",
                "resolved bean is a prototype (owned move); it has no shared handle",
            ))
        })
    }

    /// THE one concrete creation driver (bean-instantiation): single-phase
    /// `provide → run_init → publish`, branching on the scope's [`Multiplicity`].
    ///
    /// # Errors
    /// A resolution fault, a constructor-body fault, or an init-callback fault
    /// (with the partial-destroy mirror run on the unwind for a shared bean).
    pub async fn create(&self, id: BeanId, cx: &ResolveCtx<'_>) -> Result<Published, LeafError> {
        let scope = self.registry.descriptor(id).scope;
        match scope.multiplicity {
            Multiplicity::Once => self.create_singleton(id, scope, cx).await,
            Multiplicity::PerResolution => self.create_prototype(id, cx).await,
            Multiplicity::PerContextKey => self.create_scoped(id, scope, cx).await,
        }
    }

    // ── Multiplicity::Once — the OnceCell singleton publication guard ──

    async fn create_singleton(
        &self,
        id: BeanId,
        scope: ScopeDef,
        cx: &ResolveCtx<'_>,
    ) -> Result<Published, LeafError> {
        // Lock-free ready read: a published singleton is a bounds-checked index.
        if let Some(existing) = self.registry.singleton_cell(id).get() {
            return Ok(Published::Shared(existing.clone()));
        }

        // Miss: build OFF any held guard (no `.await` under a lock), then commit.
        let bean = self.publish_pipeline_shared(id, cx).await?;

        // Commit into the per-slot OnceCell. `set` is the at-most-once arbiter: if
        // we lost the race a concurrent first-creator already published, so we
        // return the WINNER's Arc and drop our redundant build.
        let cell = self.registry.singleton_cell(id);
        match cell.set(bean.clone()) {
            Ok(()) => {
                // We won the publication: register the destroyer for a managed
                // shared bean (a prototype/None-teardown bean pushes nothing).
                if scope.teardown == TeardownPolicy::Managed {
                    self.register_destroyer(id, &bean);
                }
                Ok(Published::Shared(bean))
            }
            Err(_lost) => {
                // The winner is now in the cell; hand back its handle.
                let winner = cell.get().expect("OnceCell set by the race winner").clone();
                Ok(Published::Shared(winner))
            }
        }
    }

    // ── Multiplicity::PerResolution — the prototype Owned-move lane ──

    async fn create_prototype(
        &self,
        id: BeanId,
        cx: &ResolveCtx<'_>,
    ) -> Result<Published, LeafError> {
        // Run the pipeline once; write no store slot, register no destroyer — the
        // container retains NOTHING of a prototype (a Published::Owned hand-off).
        let provider = self.registry.provider(id);
        let published = provider.provide(cx).await?;
        // A prototype provider yields Published::Owned. Init for owned beans is
        // the provider's own concern (no shared handle to run erased callbacks on);
        // the container does not store or tear it down.
        if published.is_owned() {
            Ok(published)
        } else {
            // A shared publication from a prototype-scoped slot is a provider bug;
            // surface it honestly rather than silently storing it.
            Err(LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
                "creating prototype",
                "prototype provider published a shared handle; expected an owned move",
            )))
        }
    }

    // ── Multiplicity::PerContextKey — the ambient InstanceStore lane ──

    async fn create_scoped(
        &self,
        _id: BeanId,
        _scope: ScopeDef,
        _cx: &ResolveCtx<'_>,
    ) -> Result<Published, LeafError> {
        // The ambient store is reached through the Cx scope binding (a CxKey the
        // web/request layer installs at the scope boundary). The ResolveCtx does
        // not yet carry that accessor (a later leaf-boot wiring): the pipeline that
        // memoizes a scoped bean (`InstanceStore::get_or_init` over the bare
        // ErasedBean, identical to the singleton publish) is the same shape, but
        // without an installed store there is nowhere to put it. So a scoped bean
        // resolved on a bare engine is a loud ScopeMismatch, never a silent
        // singleton. TODO(leaf-core): thread the `cx.scope_store(ScopeKind)`
        // accessor through ResolveCtx so this lane memoizes via the InstanceStore.
        Err(LeafError::new(ErrorKind::ScopeMismatch).caused_by(Cause::plain(
            "creating context-scoped bean",
            "no ambient InstanceStore is installed for this scope (install one via the request/session Cx binding)",
        )))
    }

    // ── the shared publish pipeline: provide → run_init → publish ──

    async fn publish_pipeline_shared(
        &self,
        id: BeanId,
        cx: &ResolveCtx<'_>,
    ) -> Result<ErasedBean, LeafError> {
        let provider = self.registry.provider(id);
        let published = provider.provide(cx).await?;
        let bean = published.into_shared().ok_or_else(|| {
            LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
                "publishing shared bean",
                "provider for a shared-scope bean published an owned move",
            ))
        })?;

        // run_init: aware + init callbacks run AFTER construct, BEFORE publish,
        // under NO held guard. The erased handle is downcast inside each callback.
        let descriptor = self.registry.descriptor(id);
        let plan = (self.plan_of)(id, descriptor);
        if plan.has_callbacks() || !plan.aware_wants.is_empty() {
            let ambient = crate::cx::Cx::current_or_empty();
            // The init chain runs over the shared bean handle (as &dyn Any).
            run_init(&plan, bean.as_ref(), &ambient)
                .await
                .map_err(crate::lifecycle_engine::CallbackError::into_leaf_error)?;
        }
        Ok(bean)
    }

    fn register_destroyer(&self, id: BeanId, bean: &ErasedBean) {
        let descriptor = self.registry.descriptor(id);
        let plan = (self.plan_of)(id, descriptor);
        let ambient = crate::cx::Cx::current_or_empty();
        self.ledger
            .push(Destroyer::for_plan(id, plan, bean.clone(), ambient));
    }

    /// Drain the container teardown ledger LIFO (the engine-level teardown).
    ///
    /// Reverse publication order (a `@DependsOn` target tears down after its
    /// dependent). A destroy fault is collected, never aborts the drain. There is
    /// no async `Drop` — this awaited drain IS the teardown.
    pub async fn shutdown(&self) -> crate::lifecycle_engine::TeardownOutcome {
        self.ledger.drain().await
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("beans", &self.registry.len())
            .field("policy", &self.policy)
            .field("pending_teardown", &self.ledger.len())
            .finish_non_exhaustive()
    }
}

// ─────────────────────────── the Context façade ─────────────────────────────

/// The ApplicationContext-analogue façade (registry-core `container-core`).
///
/// HAS-A exactly one [`Engine`] + the always-present context services (here the
/// [`Env`] + the `Option<Ref<Context>>` parent link; richer services — events,
/// messages, resources — land in leaf-boot). The BeanFactory surface is delegated
/// by hand-forwarded inherent methods (NOT a re-declared public trait), and the
/// `refresh()`/teardown TEMPLATE itself is leaf-boot's; this façade provides the
/// primitives that template drives.
pub struct Context {
    engine: Engine,
    env: Env,
    parent: Option<Ref<Context>>,
}

impl Context {
    /// Build a root context over an [`Engine`] and an [`Env`] (no parent).
    #[must_use]
    pub fn new(engine: Engine, env: Env) -> Self {
        Context { engine, env, parent: None }
    }

    /// Attach a parent context (the hierarchy link; builder style).
    ///
    /// Hierarchy is a relationship BETWEEN per-registry contexts: a local miss
    /// delegates upward (lowest-factory-wins). The `Ref<Context>` strong-count
    /// keeps the parent alive until children drain.
    #[must_use]
    pub fn with_parent(mut self, parent: Ref<Context>) -> Self {
        self.parent = Some(parent);
        self
    }

    /// The HAS-A'd engine.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The always-present environment service.
    #[must_use]
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// The parent context, if any (the hierarchy link).
    #[must_use]
    pub fn parent(&self) -> Option<&Ref<Context>> {
        self.parent.as_ref()
    }

    /// `true` iff `key` resolves in THIS context's local registry only.
    #[must_use]
    pub fn contains_local(&self, key: &BeanKey) -> bool {
        self.engine.contains(key)
    }

    /// `true` iff `key` resolves locally OR in any ancestor.
    #[must_use]
    pub fn contains(&self, key: &BeanKey) -> bool {
        if self.engine.contains(key) {
            return true;
        }
        self.parent.as_ref().is_some_and(|p| p.contains(key))
    }

    /// Resolve a SHARED bean by concrete type — local first, then delegating to
    /// the parent on a local miss (lowest-factory-wins).
    ///
    /// # Errors
    /// As [`Engine::get`]; a local ambiguity fails LOCALLY (never silently falls
    /// through to the parent).
    pub async fn get<T: crate::handle::Bean>(&self) -> Result<Ref<T>, LeafError> {
        match self.engine.get::<T>().await {
            Ok(r) => Ok(r),
            Err(e) if e.kind == ErrorKind::NoSuchBean => match self.parent.as_ref() {
                // A local MISS (NoSuchBean) delegates upward; a local AMBIGUITY
                // (NoUniqueBean) or a construction fault never falls through.
                Some(parent) => Box::pin(parent.get::<T>()).await,
                None => Err(e),
            },
            Err(e) => Err(e),
        }
    }

    /// Resolve an erased bean by [`BeanKey`] — local first, then parent on miss.
    ///
    /// # Errors
    /// As [`Engine::get_erased`].
    pub async fn get_erased(&self, key: BeanKey) -> Result<ErasedBean, LeafError> {
        match self.engine.get_erased(key.clone()).await {
            Ok(b) => Ok(b),
            Err(e) if e.kind == ErrorKind::NoSuchBean => match self.parent.as_ref() {
                Some(parent) => Box::pin(parent.get_erased(key)).await,
                None => Err(e),
            },
            Err(e) => Err(e),
        }
    }

    /// Drain this context's teardown ledger (the engine-level drain). The
    /// `refresh()`/full-teardown template (stop_all, in-flight drain, RunState
    /// transitions) is leaf-boot's; this is the ledger-drain primitive it calls.
    pub async fn shutdown(&self) -> crate::lifecycle_engine::TeardownOutcome {
        self.engine.shutdown().await
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("engine", &self.engine)
            .field("has_parent", &self.parent.is_some())
            .finish_non_exhaustive()
    }
}

// ─────────────────────── AssemblyError / AssemblyReport ──────────────────────

/// One aggregated assembly fault (registry-core: the `App<Wired>::validate()`
/// Tier-2 pass collects these into an [`AssemblyReport`]).
///
/// A thin newtype over the one [`LeafError`] chain so the aggregating pass can
/// gather NoSuchBean / NoUniqueBean / scope-mismatch / cycle / config-bind faults
/// into ONE report rather than failing on the first. The full validate template
/// lives in leaf-boot; this is the report shape it accumulates into.
#[derive(Debug, Clone)]
pub struct AssemblyError(pub LeafError);

impl AssemblyError {
    /// Wrap a [`LeafError`] as an assembly fault.
    #[must_use]
    pub fn new(error: LeafError) -> Self {
        AssemblyError(error)
    }

    /// The underlying [`LeafError`].
    #[must_use]
    pub fn error(&self) -> &LeafError {
        &self.0
    }
}

impl From<LeafError> for AssemblyError {
    fn from(e: LeafError) -> Self {
        AssemblyError(e)
    }
}

impl std::fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for AssemblyError {}

/// The aggregated Tier-2 assembly report (registry-core): every wiring/config
/// fault gathered in one pass, so a whole-graph validation surfaces ALL problems
/// at once rather than fail-on-first.
#[derive(Debug, Default)]
pub struct AssemblyReport {
    faults: Vec<AssemblyError>,
}

impl AssemblyReport {
    /// A fresh, empty report.
    #[must_use]
    pub fn new() -> Self {
        AssemblyReport { faults: Vec::new() }
    }

    /// Record one assembly fault.
    pub fn push(&mut self, fault: impl Into<AssemblyError>) {
        self.faults.push(fault.into());
    }

    /// `true` iff no faults were recorded (the graph validated clean).
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.faults.is_empty()
    }

    /// The number of recorded faults.
    #[must_use]
    pub fn len(&self) -> usize {
        self.faults.len()
    }

    /// `true` iff the report is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.faults.is_empty()
    }

    /// The recorded faults.
    #[must_use]
    pub fn faults(&self) -> &[AssemblyError] {
        &self.faults
    }

    /// Collapse the report into a `Result`: `Ok(())` if clean, else the FIRST
    /// fault as the representative [`LeafError`] (with the count noted).
    ///
    /// # Errors
    /// The first recorded fault if any were pushed.
    pub fn into_result(self) -> Result<(), LeafError> {
        match self.faults.into_iter().next() {
            None => Ok(()),
            Some(first) => Err(first.0),
        }
    }
}

// ─────────────────────────── error constructors ─────────────────────────────

fn type_mismatch<T: 'static>() -> LeafError {
    LeafError::new(ErrorKind::NoSuchBean).caused_by(Cause::plain(
        "resolving bean",
        format!("resolved bean is not of the requested type `{}`", std::any::type_name::<T>()),
    ))
}

fn owned_via_shared<T: 'static>() -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
        "resolving shared bean",
        format!("`{}` resolved to a prototype (owned move); use get_owned", std::any::type_name::<T>()),
    ))
}

fn shared_via_owned<T: 'static>() -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
        "resolving owned bean",
        format!("`{}` resolved to a shared singleton; use get", std::any::type_name::<T>()),
    ))
}

// ─────────────────── convenience builder over RegistryBuilder ────────────────

impl Engine {
    /// Convenience: freeze a [`RegistryBuilder`] and build a default-policy engine.
    ///
    /// # Errors
    /// A freeze-time collision (duplicate name/contract, alias cycle).
    pub fn from_builder(builder: RegistryBuilder) -> Result<Engine, LeafError> {
        Ok(Engine::new(builder.freeze()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{AnnotationMetadata, Role};
    use crate::error::Origin;
    use crate::future::BoxFuture;
    use crate::handle::Bean;
    use crate::identity::ContractId;
    use crate::lifecycle_engine::{
        CallbackError, LifecyclePhase, LifecycleStep, StepId,
    };
    use crate::provider::Provider;
    use std::any::Any;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    // ── the tiny graph: A depends on B ──

    #[derive(Debug)]
    struct B {
        tag: &'static str,
    }
    impl Bean for B {}

    #[derive(Debug)]
    struct A {
        b: Ref<B>,
    }
    impl Bean for A {}

    fn descriptor(name: &'static str, contract: &str, self_type: TypeId, scope: ScopeDef) -> Descriptor {
        Descriptor {
            contract: ContractId::of(contract),
            self_type,
            provides: &[],
            declared_name: Some(name),
            aliases: &[],
            scope,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("test") },
        }
    }

    // B: a shared singleton that counts how many times its provider ran.
    struct BProvider {
        descriptor: Descriptor,
        builds: Arc<AtomicUsize>,
    }
    impl Provider for BProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async move {
                self.builds.fetch_add(1, Ordering::SeqCst);
                Ok(Published::shared_value(B { tag: "b" }))
            })
        }
    }

    // A: depends on B — resolves B from the engine inside its provide. The
    // resolver is ASYNC (a BoxFuture), mirroring how the real ResolveCtx engine
    // back-reference will drive nested `get` — no nested block_on.
    type AsyncRefResolver =
        Arc<dyn Fn() -> BoxFuture<'static, Result<Ref<B>, LeafError>> + Send + Sync>;
    struct AProvider {
        descriptor: Descriptor,
        resolve_b: AsyncRefResolver,
    }
    impl Provider for AProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async move {
                let b = (self.resolve_b)().await?;
                Ok(Published::shared_value(A { b }))
            })
        }
    }

    #[test]
    fn singleton_is_created_once_only() {
        let builds = Arc::new(AtomicUsize::new(0));
        let mut builder = RegistryBuilder::new();
        builder
            .register(
                descriptor("b", "test::B", TypeId::of::<B>(), ScopeDef::SINGLETON),
                Arc::new(BProvider { descriptor: descriptor("b", "test::B", TypeId::of::<B>(), ScopeDef::SINGLETON), builds: builds.clone() }),
            )
            .unwrap();
        let engine = Engine::from_builder(builder).unwrap();

        // Two resolves of the same singleton → ONE build, SAME Arc.
        let b1 = block(engine.get::<B>()).expect("first");
        let b2 = block(engine.get::<B>()).expect("second");
        assert_eq!(builds.load(Ordering::SeqCst), 1, "built exactly once");
        assert!(std::ptr::eq(b1.as_arc().as_ref(), b2.as_arc().as_ref()));
        assert_eq!(b1.tag, "b");
    }

    #[test]
    fn engine_resolves_a_depending_on_b() {
        let builds = Arc::new(AtomicUsize::new(0));
        let b_desc = descriptor("b", "test::B", TypeId::of::<B>(), ScopeDef::SINGLETON);
        let a_desc = descriptor("a", "test::A", TypeId::of::<A>(), ScopeDef::SINGLETON);

        // ONE engine holds both A and B. A's provider resolves B through a Weak
        // back-reference to the (Arc-held) engine — the shape the real ResolveCtx
        // engine handle will carry. The resolver yields a 'static async future
        // (no nested block_on), so A.create awaits B.create cleanly.
        let mut builder = RegistryBuilder::new();
        builder
            .register(b_desc, Arc::new(BProvider { descriptor: b_desc, builds: builds.clone() }))
            .unwrap();

        let engine_slot: Arc<once_cell::sync::OnceCell<Arc<Engine>>> =
            Arc::new(once_cell::sync::OnceCell::new());
        let slot = engine_slot.clone();
        let resolve_b: AsyncRefResolver = Arc::new(move || {
            let engine = slot.get().expect("engine installed").clone();
            Box::pin(async move { engine.get::<B>().await })
        });
        builder
            .register(a_desc, Arc::new(AProvider { descriptor: a_desc, resolve_b }))
            .unwrap();
        let engine = Arc::new(Engine::from_builder(builder).unwrap());
        engine_slot.set(engine.clone()).ok().expect("install");

        let a = block(engine.get::<A>()).expect("A resolves");
        assert_eq!(a.b.tag, "b");
        // B built once, shared into A and reusable directly (same Arc).
        let b = block(engine.get::<B>()).expect("B resolves");
        assert!(std::ptr::eq(a.b.as_arc().as_ref(), b.as_arc().as_ref()));
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }

    // ── prototype: a fresh owned move each time, container retains nothing ──

    #[derive(Debug, PartialEq)]
    struct Proto {
        n: u32,
    }

    struct ProtoProvider {
        descriptor: Descriptor,
        counter: Arc<AtomicUsize>,
    }
    impl Provider for ProtoProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async move {
                let n = self.counter.fetch_add(1, Ordering::SeqCst) as u32;
                Ok(Published::owned(Proto { n }))
            })
        }
    }

    #[test]
    fn prototype_is_a_fresh_owned_move_each_time() {
        let counter = Arc::new(AtomicUsize::new(0));
        let d = descriptor("proto", "test::Proto", TypeId::of::<Proto>(), ScopeDef::PROTOTYPE);
        let mut builder = RegistryBuilder::new();
        builder
            .register(d, Arc::new(ProtoProvider { descriptor: d, counter: counter.clone() }))
            .unwrap();
        let engine = Engine::from_builder(builder).unwrap();

        let p0 = block(engine.get_owned::<Proto>()).expect("first");
        let p1 = block(engine.get_owned::<Proto>()).expect("second");
        // Fresh each time (distinct values), and the container stored NOTHING (the
        // singleton cell for the prototype slot stays empty).
        assert_eq!(p0, Proto { n: 0 });
        assert_eq!(p1, Proto { n: 1 });
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        let id = engine.registry().resolve_id(&BeanKey::ByType(TypeId::of::<Proto>())).unwrap();
        assert!(engine.registry().singleton_cell(id).get().is_none(), "no store write");
        // Prototype registered no destroyer.
        assert!(engine.ledger().is_empty());
    }

    // ── teardown ledger drains LIFO after singletons publish ──

    // A bean with a destroy callback that records its tag.
    #[derive(Debug)]
    struct Closer {
        tag: &'static str,
        log: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Bean for Closer {}

    fn closer_destroy<'a>(
        bean: &'a (dyn Any + Send + Sync),
        _cx: &'a crate::cx::Cx,
    ) -> BoxFuture<'a, Result<(), CallbackError>> {
        Box::pin(async move {
            let c = bean.downcast_ref::<Closer>().expect("Closer");
            c.log.lock().unwrap().push(c.tag);
            Ok(())
        })
    }

    struct CloserProvider {
        descriptor: Descriptor,
        tag: &'static str,
        log: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Provider for CloserProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            let tag = self.tag;
            let log = self.log.clone();
            Box::pin(async move { Ok(Published::shared_value(Closer { tag, log })) })
        }
    }

    #[test]
    fn teardown_ledger_drains_lifo_in_reverse_publication_order() {
        let log = Arc::new(Mutex::new(Vec::new()));

        let first = descriptor("first", "test::First", TypeId::of::<Closer>(), ScopeDef::SINGLETON);
        // Two singletons of the SAME concrete type but distinct names/contracts so
        // both get slots; we resolve each by name.
        let second_ty = TypeId::of::<A>(); // distinct TypeId to avoid by-type ambiguity
        let _ = second_ty;

        let mut builder = RegistryBuilder::new();
        let id_first = builder
            .register(
                first,
                Arc::new(CloserProvider { descriptor: first, tag: "first", log: log.clone() }),
            )
            .unwrap();

        // A second closer under a different contract/name (still Closer type).
        let second = descriptor("second", "test::Second", TypeId::of::<Closer>(), ScopeDef::SINGLETON);
        let id_second = builder
            .register(
                second,
                Arc::new(CloserProvider { descriptor: second, tag: "second", log: log.clone() }),
            )
            .unwrap();

        // Install a plan resolver that gives each Closer a DestroyMethod step.
        static DESTROY: &[LifecycleStep] = &[LifecycleStep {
            phase: LifecyclePhase::DestroyMethod,
            call: closer_destroy,
            id: StepId(1),
        }];
        let engine = Engine::from_builder(builder)
            .unwrap()
            .with_plan_resolver(|_, _| LifecyclePlan { destroy: DESTROY, ..LifecyclePlan::EMPTY });

        // Publish FIRST then SECOND (publication order).
        let _ = block(engine.get_erased(BeanKey::ByName(crate::identity::BeanName::from("first")))).unwrap();
        let _ = block(engine.get_erased(BeanKey::ByName(crate::identity::BeanName::from("second")))).unwrap();
        assert_eq!(engine.ledger().len(), 2);

        let outcome = block(engine.shutdown());
        // LIFO: second tears down before first.
        assert_eq!(outcome.order, vec![id_second, id_first]);
        assert_eq!(*log.lock().unwrap(), vec!["second", "first"]);
        assert!(outcome.is_clean());
        assert!(engine.ledger().is_empty());
    }

    // ── init callbacks run during create, before publish ──

    #[derive(Debug)]
    struct Initialized {
        inited: Arc<AtomicUsize>,
    }
    impl Bean for Initialized {}

    fn run_init_cb<'a>(
        bean: &'a (dyn Any + Send + Sync),
        _cx: &'a crate::cx::Cx,
    ) -> BoxFuture<'a, Result<(), CallbackError>> {
        Box::pin(async move {
            let i = bean.downcast_ref::<Initialized>().expect("Initialized");
            i.inited.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    struct InitProvider {
        descriptor: Descriptor,
        inited: Arc<AtomicUsize>,
    }
    impl Provider for InitProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            let inited = self.inited.clone();
            Box::pin(async move { Ok(Published::shared_value(Initialized { inited })) })
        }
    }

    #[test]
    fn init_callbacks_run_once_during_singleton_create() {
        let inited = Arc::new(AtomicUsize::new(0));
        let d = descriptor("init", "test::Init", TypeId::of::<Initialized>(), ScopeDef::SINGLETON);
        let mut builder = RegistryBuilder::new();
        builder
            .register(d, Arc::new(InitProvider { descriptor: d, inited: inited.clone() }))
            .unwrap();
        static INIT: &[LifecycleStep] = &[LifecycleStep {
            phase: LifecyclePhase::PostConstruct,
            call: run_init_cb,
            id: StepId(1),
        }];
        let engine = Engine::from_builder(builder)
            .unwrap()
            .with_plan_resolver(|_, _| LifecyclePlan { init: INIT, ..LifecyclePlan::EMPTY });

        let _ = block(engine.get::<Initialized>()).unwrap();
        let _ = block(engine.get::<Initialized>()).unwrap();
        // Init ran exactly ONCE (during the single build), not per-resolve.
        assert_eq!(inited.load(Ordering::SeqCst), 1);
    }

    // ── Context façade delegates + hierarchy ──

    #[test]
    fn context_delegates_get_to_engine_and_parent_on_miss() {
        // Parent context owns B.
        let mut pbuilder = RegistryBuilder::new();
        let bdesc = descriptor("b", "test::B", TypeId::of::<B>(), ScopeDef::SINGLETON);
        pbuilder
            .register(bdesc, Arc::new(BProvider { descriptor: bdesc, builds: Arc::new(AtomicUsize::new(0)) }))
            .unwrap();
        let parent_engine = Engine::from_builder(pbuilder).unwrap();
        let parent = Ref::new(Context::new(parent_engine, crate::env::EnvBuilder::default().seal_env()));

        // Child context owns nothing of type B; resolves via parent.
        let child_engine = Engine::new(RegistryBuilder::new().freeze().unwrap());
        let child = Context::new(child_engine, crate::env::EnvBuilder::default().seal_env())
            .with_parent(parent.clone());

        assert!(!child.contains_local(&BeanKey::ByType(TypeId::of::<B>())));
        assert!(child.contains(&BeanKey::ByType(TypeId::of::<B>())));
        let b = block(child.get::<B>()).expect("delegated to parent");
        assert_eq!(b.tag, "b");
        assert!(child.parent().is_some());
    }

    // ── AssemblyReport aggregation ──

    #[test]
    fn assembly_report_aggregates_faults() {
        let mut report = AssemblyReport::new();
        assert!(report.is_ok());
        report.push(LeafError::new(ErrorKind::NoSuchBean));
        report.push(LeafError::new(ErrorKind::NoUniqueBean));
        assert!(!report.is_ok());
        assert_eq!(report.len(), 2);
        let err = report.into_result().expect_err("has faults");
        assert_eq!(err.kind, ErrorKind::NoSuchBean);
    }

    // ── scoped bean with no installed store is a loud ScopeMismatch ──

    #[derive(Debug)]
    struct Scoped;
    impl Bean for Scoped {}
    struct ScopedProvider {
        descriptor: Descriptor,
    }
    impl Provider for ScopedProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(Scoped)) })
        }
    }

    #[test]
    fn context_scoped_bean_without_store_is_scope_mismatch() {
        let d = descriptor("scoped", "test::Scoped", TypeId::of::<Scoped>(), ScopeDef::REQUEST);
        let mut builder = RegistryBuilder::new();
        builder.register(d, Arc::new(ScopedProvider { descriptor: d })).unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        let err = block(engine.get::<Scoped>()).expect_err("no store installed");
        assert_eq!(err.kind, ErrorKind::ScopeMismatch);
    }
}
