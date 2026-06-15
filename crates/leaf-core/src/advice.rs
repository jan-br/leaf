//! The shared declarative-advice ABI: service traits + const attribute data.
//!
//! Realizes the leaf-core surface of declarative-advice (phase3/09): the
//! ultra-stable ABI HOMES for the SHARED shapes only. The wrap primitive
//! (`Interceptor`/`Next`/`AdviceChain`/`Pointcut`/`ProxyPlan`/`AdvisorDescriptor`)
//! is CONSUMED from the proxy substrate unchanged; this module authors the
//! SERVICE traits and the const attribute structs the seven concerns emit:
//!
//! - **Transactions:** [`TransactionManager`] + [`TxAttribute`] / [`TxPropagation`]
//!   / [`Isolation`], the ONE ambient [`TxResourceKey`] ([`CxKey`]), the
//!   transaction-synchronization seam ([`TxPhase`] / [`TxSyncRegistry`]).
//! - **Caching:** [`CacheManager`] / [`Cache`] + [`CacheKey`] / [`StoredValue`] /
//!   [`CacheOpMeta`].
//! - **Validation:** the object-safe collect-all [`Validate`] trait (reused by
//!   method-validation AND config-binding) + [`Violation`].
//! - **Retry / resilience:** the imperative [`RetryTemplate`] + [`RetryPolicy`] /
//!   [`BackoffPolicy`] primitive.
//! - **Exception translation:** [`DataAccessExceptionTranslator`] + the
//!   [`DataAccessKind`] payload that rides
//!   [`ErrorKind::Integration`](crate::ErrorKind::Integration).
//! - **Transactional events:** the [`TxDeferral`] field a `ListenerDescriptor`
//!   carries.
//!
//! Scope note (this unit): these are pure-ABI traits + const data. The live
//! managers/caches/validators live in leaf-tx/leaf-cache/leaf-validation/
//! leaf-resilience; the `ADVISORS`/`SCHEDULED` linkme channels are owned by the
//! discovery unit. `DataAccessKind` rides the open
//! [`ErrorKind::Integration`](crate::ErrorKind::Integration) arm (no separate
//! error ABI). The chain-order `*_ORDER` consts already live in
//! [`crate::order`]. Async at every `dyn` seam is a [`BoxFuture`].

use std::any::{Any, TypeId};
use std::sync::Arc;
use std::time::Duration;

use crate::cx::{CxKey, Propagation};
use crate::error::LeafError;
use crate::exec::MethodKey;
use crate::future::BoxFuture;
use crate::identity::ContractId;
use crate::provider::ResolveCtx;

// ═══════════════════════════ transactions ═══════════════════════════════════

/// Transaction propagation behavior (transaction-management) — the Spring
/// `Propagation` semantics.
///
/// Named `TxPropagation` (NOT bare `Propagation`) to avoid colliding with the
/// ambient-context [`Propagation`] enum, which is an
/// orthogonal `{Inherit, Isolate}` axis on a [`CxKey`]. The two never mix.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum TxPropagation {
    /// Join an existing tx, or start one (the default).
    #[default]
    Required,
    /// Always start a NEW tx, suspending any current one.
    RequiresNew,
    /// Run in a nested (savepoint) tx if one is active.
    Nested,
    /// Join an existing tx; run non-transactionally if none.
    Supports,
    /// Suspend any current tx; run non-transactionally.
    NotSupported,
    /// Join an existing tx; ERROR if none is active.
    Mandatory,
    /// ERROR if a tx is active; run non-transactionally otherwise.
    Never,
}

/// Transaction isolation level (transaction-management).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Isolation {
    /// Use the underlying datastore's default isolation.
    #[default]
    Default,
    /// Dirty reads possible.
    ReadUncommitted,
    /// Dirty reads prevented.
    ReadCommitted,
    /// Non-repeatable reads prevented.
    RepeatableRead,
    /// Full serializability.
    Serializable,
}

/// A typed error-class matcher for rollback rules (transaction-management) —
/// matches over the typed [`ErrorKind`](crate::ErrorKind) / a stable id rather
/// than a stringly exception class name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ErrorMatch {
    /// Match any error of an integration taxonomy id.
    Integration(ContractId),
    /// Match by a stable matcher id (a custom predicate keyed by contract).
    Kind(ContractId),
}

/// The const transaction attribute the `#[transactional(..)]` macro emits onto
/// the advisor row (transaction-management). Flat, `Copy`, const-constructible.
#[derive(Clone, Copy, Debug)]
pub struct TxAttribute {
    /// Propagation behavior.
    pub propagation: TxPropagation,
    /// Isolation level.
    pub isolation: Isolation,
    /// Whether the tx is read-only (an optimization hint).
    pub read_only: bool,
    /// Optional timeout.
    pub timeout: Option<Duration>,
    /// Error classes that TRIGGER a rollback.
    pub rollback_on: &'static [ErrorMatch],
    /// Error classes that do NOT trigger a rollback (override).
    pub no_rollback_on: &'static [ErrorMatch],
    /// The qualifier of the [`TransactionManager`] bean to use.
    pub manager: Option<&'static str>,
}

impl TxAttribute {
    /// The default attribute: `Required`, datastore-default isolation,
    /// read-write, no timeout, no rules, default manager.
    pub const DEFAULT: TxAttribute = TxAttribute {
        propagation: TxPropagation::Required,
        isolation: Isolation::Default,
        read_only: false,
        timeout: None,
        rollback_on: &[],
        no_rollback_on: &[],
        manager: None,
    };
}

impl Default for TxAttribute {
    fn default() -> Self {
        TxAttribute::DEFAULT
    }
}

/// The opaque per-transaction state a [`TransactionManager`] mints at `begin`
/// and threads to `commit`/`rollback` (transaction-management). Type-erased so
/// the manager owns its own representation; carried on the ambient
/// [`TxResourceKey`].
#[derive(Clone)]
pub struct TxState(Arc<dyn Any + Send + Sync>);

impl TxState {
    /// Wrap a manager-specific state value.
    #[must_use]
    pub fn new<T: Any + Send + Sync>(value: T) -> Self {
        TxState(Arc::new(value))
    }

    /// Downcast to the manager-specific state type.
    #[must_use]
    pub fn downcast_ref<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for TxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxState").finish_non_exhaustive()
    }
}

/// The transaction definition handed to [`TransactionManager::begin`] (the
/// resolved attribute for one advised call).
#[derive(Clone, Copy, Debug)]
pub struct TxDefinition {
    /// The resolved const attribute.
    pub attribute: TxAttribute,
}

/// THE transaction manager SPI (transaction-management) — origin-agnostic,
/// `Send + Sync`, async at the `dyn` seam.
pub trait TransactionManager: Send + Sync {
    /// Begin (or join) a transaction per `def`.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the transaction cannot be started.
    fn begin<'a>(
        &'a self,
        def: &'a TxDefinition,
        cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<TxState, LeafError>>;

    /// Commit the transaction.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the commit fails.
    fn commit(&self, st: TxState) -> BoxFuture<'_, Result<(), LeafError>>;

    /// Roll the transaction back.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the rollback fails.
    fn rollback(&self, st: TxState) -> BoxFuture<'_, Result<(), LeafError>>;

    /// The synchronization registry for this transaction (the tx-events seam).
    fn synchronizations<'a>(&'a self, st: &'a TxState) -> &'a TxSyncRegistry;
}

/// The ONE tx-resource ambient key (transaction-management) — read by nested
/// data-access, the transactional-events dispatch branch, and `current_tx`.
///
/// `POLICY = Isolate` (NEVER inherited across a spawn): a tx is structurally
/// task-scoped, so an `@Async` hop cannot smuggle the active tx into a child.
pub struct TxResourceKey;

impl CxKey for TxResourceKey {
    type Value = TxState;
    const NAME: &'static str = "leaf.tx.resource";
    const POLICY: Propagation = Propagation::Isolate;
}

/// The tx-synchronization phase a deferred callback fires at
/// (transaction-synchronization, consumed by transactional-events).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TxPhase {
    /// Before the commit `.await` (a failure here can veto the commit).
    BeforeCommit,
    /// After a successful commit.
    AfterCommit,
    /// After a rollback.
    AfterRollback,
    /// After completion, regardless of outcome.
    AfterCompletion,
}

/// The outcome handed to a tx-synchronization callback.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxOutcome {
    /// The transaction committed.
    Committed,
    /// The transaction rolled back.
    RolledBack,
}

/// A boxed, async, one-shot tx-synchronization callback.
pub type TxSyncCallback =
    Box<dyn FnOnce(TxOutcome) -> BoxFuture<'static, Result<(), LeafError>> + Send>;

/// The per-phase callback buckets the tx interceptor fires
/// (transaction-synchronization). The concrete bucket storage is fleshed out by
/// leaf-tx; this pins the registration seam transactional-events depends on.
#[derive(Default)]
pub struct TxSyncRegistry {
    // The concrete per-phase buckets are an implementation detail of the tx
    // manager (they need interior mutability + ordering); leaf-tx owns the
    // storage. This unit pins the `register` seam shape.
    _private: (),
}

impl TxSyncRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        TxSyncRegistry::default()
    }

    /// Register a callback to fire at `phase`.
    ///
    /// Scope note: the live bucket storage + firing order is leaf-tx's; this is
    /// the seam shape (the kernel placeholder is a no-op that proves the ABI is
    /// object-safe and the callback type composes).
    pub fn register(&self, _phase: TxPhase, _cb: TxSyncCallback) {
        // leaf-tx overrides via its own concrete registry; the kernel shape
        // exists so transactional-events can name the type + signature.
    }
}

impl std::fmt::Debug for TxSyncRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxSyncRegistry").finish_non_exhaustive()
    }
}

// ═══════════════════════════ caching ════════════════════════════════════════

/// A computed cache key (caching) — the method identity + a typed-hash payload.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CacheKey {
    /// The advised method's stable identity.
    pub method: MethodKey,
    /// The key payload (a typed hash over the args).
    pub payload: Box<[u8]>,
}

impl CacheKey {
    /// Build a cache key.
    #[must_use]
    pub fn new(method: MethodKey, payload: impl Into<Box<[u8]>>) -> Self {
        CacheKey { method, payload: payload.into() }
    }
}

/// A stored cache value (caching) — `TypeId`-checked so a stale cast is caught.
pub struct StoredValue {
    ty: TypeId,
    val: Box<dyn Any + Send + Sync>,
}

impl StoredValue {
    /// Store a typed value.
    #[must_use]
    pub fn new<T: Any + Send + Sync>(value: T) -> Self {
        StoredValue { ty: TypeId::of::<T>(), val: Box::new(value) }
    }

    /// The stored value's `TypeId`.
    #[must_use]
    pub fn type_id(&self) -> TypeId {
        self.ty
    }

    /// Downcast to `T` (returns `None` on a type mismatch).
    #[must_use]
    pub fn downcast_ref<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.val.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for StoredValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredValue").field("ty", &self.ty).finish_non_exhaustive()
    }
}

/// The const cache-operation metadata the `#[cacheable(..)]`/`#[cache_evict]`/
/// `#[cache_put]` macros emit onto the advisor row (caching).
#[derive(Clone, Copy, Debug)]
pub struct CacheOpMeta {
    /// The cache name(s) this op targets.
    pub cache_names: &'static [&'static str],
    /// Whether eviction clears the whole cache (`#[cache_evict(all)]`).
    pub all_entries: bool,
    /// Whether the op runs BEFORE the method body (`before_invocation`).
    pub before_invocation: bool,
    /// Whether `sync` (single-flight) semantics are requested.
    pub sync: bool,
}

impl CacheOpMeta {
    /// The default op metadata.
    pub const DEFAULT: CacheOpMeta = CacheOpMeta {
        cache_names: &[],
        all_entries: false,
        before_invocation: false,
        sync: false,
    };
}

impl Default for CacheOpMeta {
    fn default() -> Self {
        CacheOpMeta::DEFAULT
    }
}

/// The cache-lookup facade (caching) — resolves a named cache.
pub trait CacheManager: Send + Sync {
    /// The cache registered under `name`, if any.
    fn cache(&self, name: &str) -> Option<Arc<dyn Cache>>;
}

/// One named cache (caching) — async at every `dyn` seam.
pub trait Cache: Send + Sync {
    /// Get the value for `k`, if present.
    ///
    /// # Errors
    /// Returns a [`LeafError`] on a backing-store fault.
    fn get<'a>(&'a self, k: &'a CacheKey)
        -> BoxFuture<'a, Result<Option<StoredValue>, LeafError>>;

    /// Put `v` under `k`.
    ///
    /// # Errors
    /// Returns a [`LeafError`] on a backing-store fault.
    fn put(&self, k: CacheKey, v: StoredValue) -> BoxFuture<'_, Result<(), LeafError>>;

    /// Evict the entry for `k`.
    ///
    /// # Errors
    /// Returns a [`LeafError`] on a backing-store fault.
    fn evict<'a>(&'a self, k: &'a CacheKey) -> BoxFuture<'a, Result<(), LeafError>>;

    /// Clear the whole cache.
    ///
    /// # Errors
    /// Returns a [`LeafError`] on a backing-store fault.
    fn clear(&self) -> BoxFuture<'_, Result<(), LeafError>>;
}

// ═══════════════════════════ validation ═════════════════════════════════════

/// One constraint violation (validation) — the collect-all sink entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Violation {
    /// The property path that failed (`order.items[0].qty`).
    pub path: String,
    /// The stable id of the violated constraint.
    pub constraint_id: ContractId,
    /// The message key for i18n rendering.
    pub message_key: &'static str,
    /// The constraint parameters (`{min}`, `{max}`).
    pub params: Box<[(&'static str, String)]>,
    /// The rejected value, rendered.
    pub rejected: String,
}

/// The collect-all validation context (validation) — accumulates [`Violation`]s
/// over a nested path. The concrete path-stack / visited-set is fleshed out by
/// leaf-validation; this pins the sink seam the [`Validate`] trait writes to.
#[derive(Default)]
pub struct ValidationContext {
    violations: Vec<Violation>,
    path: Vec<String>,
}

impl ValidationContext {
    /// A fresh, empty context.
    #[must_use]
    pub fn new() -> Self {
        ValidationContext::default()
    }

    /// Record a violation at the current path.
    pub fn add(&mut self, v: Violation) {
        self.violations.push(v);
    }

    /// Push a path segment (cascade into a nested object/field).
    pub fn enter(&mut self, segment: impl Into<String>) {
        self.path.push(segment.into());
    }

    /// Pop a path segment.
    pub fn leave(&mut self) {
        self.path.pop();
    }

    /// The current dotted path.
    #[must_use]
    pub fn current_path(&self) -> String {
        self.path.join(".")
    }

    /// All accumulated violations.
    #[must_use]
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// Whether validation passed (no violations).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.violations.is_empty()
    }
}

impl std::fmt::Debug for ValidationContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidationContext")
            .field("violations", &self.violations.len())
            .finish_non_exhaustive()
    }
}

/// THE validation engine trait (validation) — object-safe, SYNC, collect-all.
///
/// Reused by method-validation (the `#[validated]` advisor) AND config-binding
/// (the binder calls `Validate::validate` as face 3) — one engine, never two.
pub trait Validate {
    /// Validate `self`, accumulating violations into `cx`.
    fn validate(&self, cx: &mut ValidationContext);
}

// ═══════════════════════════ retry / resilience ═════════════════════════════

/// A retry policy (retry/resilience) — max attempts + a typed retryability
/// predicate over the one [`LeafError`].
#[derive(Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum attempts (including the first).
    pub max_attempts: u32,
    /// Whether an error is retryable.
    pub is_retryable: fn(&LeafError) -> bool,
}

impl RetryPolicy {
    /// A policy of `max_attempts` retrying on any error.
    #[must_use]
    pub const fn new(max_attempts: u32) -> Self {
        RetryPolicy { max_attempts, is_retryable: |_| true }
    }
}

impl std::fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("max_attempts", &self.max_attempts)
            .finish_non_exhaustive()
    }
}

/// A backoff policy (retry/resilience) — yields the delay before attempt `n`,
/// or `None` to stop.
pub trait BackoffPolicy: Send + Sync {
    /// The delay before `attempt` (1-based); `None` ends the retry loop.
    fn next_delay(&self, attempt: u32) -> Option<Duration>;
}

/// A fixed-delay backoff (the simplest built-in).
#[derive(Clone, Copy, Debug)]
pub struct FixedBackoff {
    /// The constant delay.
    pub delay: Duration,
}

impl BackoffPolicy for FixedBackoff {
    fn next_delay(&self, _attempt: u32) -> Option<Duration> {
        Some(self.delay)
    }
}

/// The imperative retry primitive (retry/resilience): the two retry advisors
/// and direct user code both drive this. `policy` + a boxed [`BackoffPolicy`].
#[derive(Clone)]
pub struct RetryTemplate {
    /// The retry policy.
    pub policy: RetryPolicy,
    /// The backoff policy.
    pub backoff: Arc<dyn BackoffPolicy>,
}

impl RetryTemplate {
    /// Build a template.
    #[must_use]
    pub fn new(policy: RetryPolicy, backoff: Arc<dyn BackoffPolicy>) -> Self {
        RetryTemplate { policy, backoff }
    }

    /// Whether a retry should be attempted after `attempt` failures, given the
    /// failing error — the pure decision the engine consults (the awaiting
    /// `execute` loop with real sleeps lives in leaf-resilience over the
    /// `ExecutionFacility`).
    #[must_use]
    pub fn should_retry(&self, attempt: u32, err: &LeafError) -> Option<Duration> {
        if attempt >= self.policy.max_attempts {
            return None;
        }
        if !(self.policy.is_retryable)(err) {
            return None;
        }
        self.backoff.next_delay(attempt)
    }
}

impl std::fmt::Debug for RetryTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryTemplate").field("policy", &self.policy).finish_non_exhaustive()
    }
}

// ═══════════════════════════ exception translation ══════════════════════════

/// The structured data-access error taxonomy (exception-translation). Rides the
/// open [`ErrorKind::Integration`](crate::ErrorKind::Integration) arm as a
/// payload — NO separate error ABI.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DataAccessKind {
    /// A duplicate-key / unique-constraint violation.
    DuplicateKey,
    /// An optimistic-locking version conflict.
    OptimisticLockingFailure,
    /// A data-integrity (FK / not-null / check) violation.
    DataIntegrityViolation,
    /// A transient fault that may succeed on retry.
    TransientDataAccess,
    /// A non-transient fault.
    NonTransient,
    /// A recoverable fault.
    Recoverable,
}

impl DataAccessKind {
    /// The stable [`ContractId`] this kind rides on the `Integration` arm.
    #[must_use]
    pub fn contract_id(self) -> ContractId {
        ContractId::of(match self {
            DataAccessKind::DuplicateKey => "leaf::dao::DuplicateKey",
            DataAccessKind::OptimisticLockingFailure => "leaf::dao::OptimisticLockingFailure",
            DataAccessKind::DataIntegrityViolation => "leaf::dao::DataIntegrityViolation",
            DataAccessKind::TransientDataAccess => "leaf::dao::TransientDataAccess",
            DataAccessKind::NonTransient => "leaf::dao::NonTransient",
            DataAccessKind::Recoverable => "leaf::dao::Recoverable",
        })
    }
}

/// The exception-translation SPI (exception-translation) — maps a raw native
/// error into a structured [`LeafError`]; `None` passes to the next translator.
pub trait DataAccessExceptionTranslator: Send + Sync {
    /// Translate `e`, or return `None` to defer to the next translator.
    fn translate(&self, e: &LeafError) -> Option<LeafError>;
}

// ═══════════════════════════ transactional events ═══════════════════════════

/// The deferral data a transactional `ListenerDescriptor` carries
/// (transactional-events) — fired at a [`TxPhase`] boundary, NOT an interceptor.
#[derive(Clone, Copy, Debug)]
pub struct TxDeferral {
    /// The phase the listener fires at.
    pub phase: TxPhase,
    /// Whether to fire anyway when no tx is active (the fallback).
    pub fallback: bool,
}

// ═══════════════════════════ async metadata ═════════════════════════════════

/// The const `@Async` metadata the `#[async_(..)]` macro emits onto the advisor
/// row (async-execution).
#[derive(Clone, Copy, Debug)]
pub struct AsyncMeta {
    /// The executor qualifier to dispatch on (`None` = the default facility).
    pub qualifier: Option<&'static str>,
}

impl AsyncMeta {
    /// The default metadata (the default executor).
    pub const DEFAULT: AsyncMeta = AsyncMeta { qualifier: None };
}

impl Default for AsyncMeta {
    fn default() -> Self {
        AsyncMeta::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cx::Cx;
    use crate::error::ErrorKind;

    // ── transactions ─────────────────────────────────────────────────────────

    struct NopTxManager {
        sync: TxSyncRegistry,
    }
    impl TransactionManager for NopTxManager {
        fn begin<'a>(
            &'a self,
            _def: &'a TxDefinition,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<TxState, LeafError>> {
            Box::pin(async { Ok(TxState::new(7u64)) })
        }
        fn commit(&self, _st: TxState) -> BoxFuture<'_, Result<(), LeafError>> {
            Box::pin(async { Ok(()) })
        }
        fn rollback(&self, _st: TxState) -> BoxFuture<'_, Result<(), LeafError>> {
            Box::pin(async { Ok(()) })
        }
        fn synchronizations<'a>(&'a self, _st: &'a TxState) -> &'a TxSyncRegistry {
            &self.sync
        }
    }

    #[test]
    fn transaction_manager_begin_commit_round_trip() {
        let mgr = NopTxManager { sync: TxSyncRegistry::new() };
        let def = TxDefinition { attribute: TxAttribute::DEFAULT };
        let cx = ResolveCtx::root();
        let st = futures::executor::block_on(mgr.begin(&def, &cx)).unwrap();
        assert_eq!(st.downcast_ref::<u64>(), Some(&7));
        assert!(futures::executor::block_on(mgr.commit(st)).is_ok());
    }

    #[test]
    fn tx_attribute_default_is_required() {
        assert_eq!(TxAttribute::DEFAULT.propagation, TxPropagation::Required);
        assert_eq!(TxAttribute::DEFAULT.isolation, Isolation::Default);
        const { assert!(!TxAttribute::DEFAULT.read_only) };
    }

    #[test]
    fn tx_resource_key_is_an_isolate_cx_key() {
        // The whole tx-correctness point: a tx is NEVER inherited across a spawn.
        assert_eq!(<TxResourceKey as CxKey>::POLICY, Propagation::Isolate);
        assert_eq!(<TxResourceKey as CxKey>::NAME, "leaf.tx.resource");
    }

    #[test]
    fn tx_state_rides_the_ambient_cx_under_the_key() {
        let cx = Cx::empty().with::<TxResourceKey>(TxState::new(99u64));
        let st = cx.get::<TxResourceKey>().expect("tx state present");
        assert_eq!(st.downcast_ref::<u64>(), Some(&99));
    }

    #[test]
    fn tx_sync_registry_register_is_object_safe() {
        let reg = TxSyncRegistry::new();
        reg.register(
            TxPhase::AfterCommit,
            Box::new(|outcome: TxOutcome| {
                Box::pin(async move {
                    assert_eq!(outcome, TxOutcome::Committed);
                    Ok(())
                }) as BoxFuture<'static, Result<(), LeafError>>
            }),
        );
    }

    #[test]
    fn tx_deferral_carries_phase_and_fallback() {
        let d = TxDeferral { phase: TxPhase::AfterCommit, fallback: false };
        assert_eq!(d.phase, TxPhase::AfterCommit);
        assert!(!d.fallback);
    }

    // ── caching ──────────────────────────────────────────────────────────────

    struct MapCache {
        inner: std::sync::Mutex<std::collections::HashMap<CacheKey, u64>>,
    }
    impl Cache for MapCache {
        fn get<'a>(
            &'a self,
            k: &'a CacheKey,
        ) -> BoxFuture<'a, Result<Option<StoredValue>, LeafError>> {
            Box::pin(async move {
                Ok(self.inner.lock().unwrap().get(k).map(|v| StoredValue::new(*v)))
            })
        }
        fn put(&self, k: CacheKey, v: StoredValue) -> BoxFuture<'_, Result<(), LeafError>> {
            Box::pin(async move {
                if let Some(n) = v.downcast_ref::<u64>() {
                    self.inner.lock().unwrap().insert(k, *n);
                }
                Ok(())
            })
        }
        fn evict<'a>(&'a self, k: &'a CacheKey) -> BoxFuture<'a, Result<(), LeafError>> {
            Box::pin(async move {
                self.inner.lock().unwrap().remove(k);
                Ok(())
            })
        }
        fn clear(&self) -> BoxFuture<'_, Result<(), LeafError>> {
            Box::pin(async move {
                self.inner.lock().unwrap().clear();
                Ok(())
            })
        }
    }

    #[test]
    fn cache_put_get_evict_round_trip() {
        let cache = MapCache { inner: std::sync::Mutex::new(std::collections::HashMap::new()) };
        let key = CacheKey::new(MethodKey::of("svc::find"), vec![1u8, 2, 3]);
        futures::executor::block_on(cache.put(key.clone(), StoredValue::new(42u64))).unwrap();
        let got = futures::executor::block_on(cache.get(&key)).unwrap().unwrap();
        assert_eq!(got.downcast_ref::<u64>(), Some(&42));
        futures::executor::block_on(cache.evict(&key)).unwrap();
        assert!(futures::executor::block_on(cache.get(&key)).unwrap().is_none());
    }

    #[test]
    fn stored_value_is_type_id_checked() {
        let sv = StoredValue::new(7u32);
        assert_eq!(sv.type_id(), TypeId::of::<u32>());
        assert!(sv.downcast_ref::<u32>().is_some());
        assert!(sv.downcast_ref::<u64>().is_none());
    }

    struct OneCacheManager(Arc<MapCache>);
    impl CacheManager for OneCacheManager {
        fn cache(&self, name: &str) -> Option<Arc<dyn Cache>> {
            if name == "users" {
                Some(self.0.clone())
            } else {
                None
            }
        }
    }

    #[test]
    fn cache_manager_resolves_named_caches() {
        let cache = Arc::new(MapCache {
            inner: std::sync::Mutex::new(std::collections::HashMap::new()),
        });
        let mgr = OneCacheManager(cache);
        assert!(mgr.cache("users").is_some());
        assert!(mgr.cache("absent").is_none());
    }

    // ── validation ─────────────────────────────────────────────────────────

    struct User {
        name: String,
    }
    impl Validate for User {
        fn validate(&self, cx: &mut ValidationContext) {
            if self.name.is_empty() {
                cx.add(Violation {
                    path: cx.current_path(),
                    constraint_id: ContractId::of("leaf::validate::NotBlank"),
                    message_key: "user.name.notblank",
                    params: Box::new([]),
                    rejected: String::new(),
                });
            }
        }
    }

    #[test]
    fn validate_collects_all_violations() {
        let mut cx = ValidationContext::new();
        User { name: String::new() }.validate(&mut cx);
        assert!(!cx.is_valid());
        assert_eq!(cx.violations().len(), 1);
        assert_eq!(cx.violations()[0].message_key, "user.name.notblank");

        let mut ok = ValidationContext::new();
        User { name: "ok".into() }.validate(&mut ok);
        assert!(ok.is_valid());
    }

    #[test]
    fn validation_context_tracks_a_nested_path() {
        let mut cx = ValidationContext::new();
        cx.enter("order");
        cx.enter("customer");
        assert_eq!(cx.current_path(), "order.customer");
        cx.leave();
        assert_eq!(cx.current_path(), "order");
    }

    // ── retry / resilience ───────────────────────────────────────────────────

    #[test]
    fn retry_template_decides_retry_and_backoff() {
        let tmpl = RetryTemplate::new(
            RetryPolicy::new(3),
            Arc::new(FixedBackoff { delay: Duration::from_millis(10) }),
        );
        let err = LeafError::new(ErrorKind::ConstructionFailed);
        // attempts 1, 2 retry; attempt 3 (== max) stops.
        assert_eq!(tmpl.should_retry(1, &err), Some(Duration::from_millis(10)));
        assert_eq!(tmpl.should_retry(2, &err), Some(Duration::from_millis(10)));
        assert_eq!(tmpl.should_retry(3, &err), None, "max reached");
    }

    #[test]
    fn retry_policy_respects_retryability_predicate() {
        let policy = RetryPolicy { max_attempts: 5, is_retryable: |e| e.kind == ErrorKind::Cancelled };
        let tmpl = RetryTemplate::new(policy, Arc::new(FixedBackoff { delay: Duration::ZERO }));
        let retryable = LeafError::new(ErrorKind::Cancelled);
        let not = LeafError::new(ErrorKind::NoSuchBean);
        assert!(tmpl.should_retry(1, &retryable).is_some());
        assert!(tmpl.should_retry(1, &not).is_none(), "non-retryable error stops");
    }

    // ── exception translation ────────────────────────────────────────────────

    struct DupKeyTranslator;
    impl DataAccessExceptionTranslator for DupKeyTranslator {
        fn translate(&self, e: &LeafError) -> Option<LeafError> {
            if e.kind == ErrorKind::ConstructionFailed {
                Some(LeafError::new(ErrorKind::Integration {
                    kind_id: DataAccessKind::DuplicateKey.contract_id(),
                }))
            } else {
                None
            }
        }
    }

    #[test]
    fn translator_maps_to_integration_arm_or_passes() {
        let t: &dyn DataAccessExceptionTranslator = &DupKeyTranslator;
        let raw = LeafError::new(ErrorKind::ConstructionFailed);
        let translated = t.translate(&raw).expect("translated");
        match translated.kind {
            ErrorKind::Integration { kind_id } => {
                assert_eq!(kind_id, DataAccessKind::DuplicateKey.contract_id());
            }
            other => panic!("expected Integration arm, got {other:?}"),
        }
        // A non-matching error passes through (None).
        assert!(t.translate(&LeafError::new(ErrorKind::NoSuchBean)).is_none());
    }

    #[test]
    fn data_access_kinds_have_distinct_stable_ids() {
        assert_ne!(
            DataAccessKind::DuplicateKey.contract_id(),
            DataAccessKind::OptimisticLockingFailure.contract_id()
        );
    }

    // ── const attribute data ─────────────────────────────────────────────────

    #[test]
    fn const_attribute_structs_are_const_constructible() {
        const TX: TxAttribute = TxAttribute {
            propagation: TxPropagation::RequiresNew,
            isolation: Isolation::Serializable,
            read_only: true,
            timeout: None,
            rollback_on: &[ErrorMatch::Kind(ContractId::of("MyErr"))],
            no_rollback_on: &[],
            manager: Some("primaryTxManager"),
        };
        const CACHE: CacheOpMeta = CacheOpMeta {
            cache_names: &["users"],
            all_entries: false,
            before_invocation: false,
            sync: true,
        };
        const ASYNC: AsyncMeta = AsyncMeta { qualifier: Some("ioPool") };
        assert_eq!(TX.propagation, TxPropagation::RequiresNew);
        const { assert!(CACHE.sync) };
        assert_eq!(ASYNC.qualifier, Some("ioPool"));
    }
}
