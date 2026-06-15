//! `leaf_core::proxy` — the proxy & interception substrate (RUNTIME side).
//!
//! The ONE wrap primitive on which all transparent, DI-seam-attached interception
//! rests (ADR-08 proxy-substrate, phase3/08-proxy-interception). There is exactly
//! ONE proxy shape — a compile-time-generated transparent delegating newtype per
//! advisable service trait (emitted by `leaf-macros`, NOT here) — and exactly ONE
//! call-routing primitive: an ordered, replayable, short-circuitable
//! [`Interceptor`] chain ([`AdviceChain`]) whose innermost step is a
//! [`TargetSource`] (the per-call target supplier).
//!
//! This module is the hand-writable runtime side the macro pins to:
//!
//! - CALL-ROUTING — [`Interceptor`] (wraps one [`Call`] → [`ErasedRet`]), [`Call`],
//!   [`Next`] (REPLAYABLE for retry + SKIPPABLE for cache), and the
//!   [`AdviceChain`] that walks them then finally hits the [`TargetSource`].
//! - TARGETSOURCE — the per-call target supplier unifying singleton
//!   ([`FixedTarget`]), scoped re-resolution ([`ScopeTarget`]), `@Lazy`, and the
//!   advised-prototype [`OwnedTarget`] lane.
//! - DYNAMIC FALLBACK — [`ErasedProxy`] + [`MethodTable`]/[`MethodEntry`] for the
//!   origin-agnostic case with no compile-time type.
//! - ADVISOR MODEL — the flat const [`AdvisorDescriptor`] (collected via the
//!   `ADVISORS` linkme slice), [`Pointcut`]/[`JoinPointMeta`] + the typed
//!   combinators ([`within`]/[`annotated_marker`]/[`returns`] composing with
//!   `&`/`|`/`!`).
//! - AUTO-PROXY-CREATOR — the frozen [`ProxyPlan`] computed at `seal()`
//!   ([`ProxyPlan::freeze`], sorted by [`cmp_chain`]), the
//!   O(1) [`ProxyPlan::advisors_for`] decoration lookup, [`AdvisorRef`], and the
//!   binary-root-assembled [`CreatorPolicy`] capability lattice.
//! - ERRORS — [`AdviceError`] flowing into the one [`LeafError`] chain.
//!
//! It SHARES with the events multicaster ONLY [`cmp_chain`] +
//! the [`RoleTier`] grade — the events `DispatchInterceptor` is a
//! STRUCTURALLY DISTINCT sibling trait (C5), not this one-[`Call`] [`Interceptor`].

use std::any::{Any, TypeId};
use std::sync::Arc;

use smallvec::SmallVec;

use crate::definition::Role;
use crate::error::{Cause, ErrorKind, LeafError};
use crate::exec::MethodKey;
use crate::future::BoxFuture;
use crate::handle::ErasedBean;
use crate::identity::{BeanId, BeanKey, ContractId, MarkerId};
use crate::injection::Container;
use crate::order::{cmp_chain, ChainKey, OrderKey, RoleTier};
use crate::provider::ResolveCtx;
use crate::registry::Registry;

// ─────────────────────────── erased args / return ───────────────────────────

/// The erased argument pack handed to an [`Interceptor`] / [`MethodEntry`].
///
/// ## The settled pack/unpack ABI (proxy-interception §R2 — the Phase-4 measure)
///
/// The representation is a SINGLE owned `Box<dyn Any + Send + Sync>` carrying the
/// method's POSITIONAL ARGUMENT TUPLE `(A0, A1, …)` — `()` for a no-arg method,
/// `(A0,)` for one arg. On the COMMON path the generated newtype keeps that typed
/// tuple boxed exactly ONCE (a MOVE of the whole tuple, NOT a per-arg box); typed
/// around-advice + the auto-installed transparent proxy's [`MethodEntry`] thunk
/// reach the concrete args by downcasting back to the SAME per-method tuple via
/// [`ErasedArgs::unpack`]. This is the one stable kernel shape both the
/// macro-emitted glue (the `__leaf_methods_<Ident>` downcast thunks `leaf-codegen`
/// emits) and the dynamic [`ErasedProxy`] fallback use; the design's deferred
/// Phase-4 ABI question is settled here as this minimal sound owned carrier.
///
/// The `+ Sync` bound is load-bearing: the [`Call`] is shared by reference
/// (`&Call`) across the REPLAYABLE [`Next::proceed`] boxed `Send` futures, so the
/// whole `Call` — and thus the arg pack — must be `Sync`. Async DI method args
/// are `Send + Sync + 'static` already (they ride the same publication contract
/// as beans), so this is the natural bound, not an extra restriction.
pub struct ErasedArgs(pub Box<dyn Any + Send + Sync>, CloneArgs);

/// The per-pack clone thunk: re-pack a FRESH [`ErasedArgs`] from a borrow of the
/// erased tuple. Monomorphized at [`ErasedArgs::pack`] over the concrete tuple `T`
/// (which is `Clone` — the advised-arg bound), so a replay re-supplies a fresh
/// owned clone of the SAME args without the caller knowing the concrete type.
type CloneArgs = fn(&(dyn Any + Send + Sync)) -> ErasedArgs;

impl ErasedArgs {
    /// Pack a typed argument tuple (the generated per-method glue calls this).
    ///
    /// The caller packs the method's POSITIONAL ARGUMENT TUPLE `(A0, A1, …)` — a
    /// no-arg method packs `()` (or uses [`ErasedArgs::none`]), a one-arg method
    /// packs `(a0,)` — and the matching [`MethodEntry`] thunk recovers it with
    /// `unpack::<(A0, …)>()`. This is the settled positional-tuple ABI.
    ///
    /// ## The advised-arg bound (`Clone + Send + Sync + 'static`)
    ///
    /// The packed tuple is `Clone` (the LOAD-BEARING advised-method-argument
    /// constraint — Spring's args are re-invocable objects): the [`Call`] keeps the
    /// args inspectable for the whole chain AND a REPLAYABLE [`Next::proceed`] (retry)
    /// re-runs the args-bearing target by CLONING a fresh copy per attempt via the
    /// monomorphized clone thunk baked in here. `Send + Sync` rides the shared `Call`;
    /// `'static` rides the erased box. This is the natural bound on a DI method arg
    /// (it already rides the bean publication contract), not an extra restriction.
    #[must_use]
    pub fn pack<T: Any + Send + Sync + Clone>(args: T) -> Self {
        ErasedArgs(Box::new(args), |a: &(dyn Any + Send + Sync)| {
            // The downcast cannot fail: this thunk is only ever paired with a box of
            // its own monomorphized `T` (set together at the same `pack::<T>` site).
            let typed = a.downcast_ref::<T>().expect("ErasedArgs clone thunk type pairing");
            ErasedArgs::pack(typed.clone())
        })
    }

    /// An empty (unit `()`) argument pack — the carrier for a no-arg advised method.
    #[must_use]
    pub fn none() -> Self {
        ErasedArgs::pack(())
    }

    /// Recover the typed argument tuple by downcast (consumes the pack).
    ///
    /// # Errors
    /// Returns the original box if the concrete tuple type is not exactly `T`.
    pub fn unpack<T: Any>(self) -> Result<T, Box<dyn Any + Send + Sync>> {
        self.0.downcast::<T>().map(|b| *b)
    }

    /// Borrow + downcast the packed tuple to `&T` (the non-consuming typed read an
    /// interceptor uses to INSPECT args off the [`Call`] — route on arg #0, build a
    /// cache key, validate a `@Valid` arg — without taking ownership).
    #[must_use]
    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }

    /// Re-pack a FRESH owned clone of these args (the replay primitive): the tail
    /// thunk calls this per [`Next::proceed`] so a retrying interceptor re-runs an
    /// args-bearing target with a fresh copy each attempt. Sound because [`pack`]
    /// requires the tuple to be `Clone` (the advised-arg bound).
    ///
    /// [`pack`]: ErasedArgs::pack
    #[must_use]
    pub fn replay(&self) -> ErasedArgs {
        (self.1)(&*self.0)
    }

    /// The concrete `TypeId` of the packed tuple (diagnostics / dynamic dispatch).
    #[must_use]
    pub fn type_id_of(&self) -> TypeId {
        (*self.0).type_id()
    }
}

impl Clone for ErasedArgs {
    /// Clone via the monomorphized clone thunk (the args tuple is `Clone` — the
    /// advised-arg bound), so a `Call` carrying `ErasedArgs` is itself replayable.
    fn clone(&self) -> Self {
        self.replay()
    }
}

impl std::fmt::Debug for ErasedArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ErasedArgs(..)")
    }
}

/// The erased return value an [`Interceptor`] / the real method yields.
///
/// The generated newtype's per-method glue downcasts it back to the typed
/// return on the way out (`.unpack()`); a cache/short-circuit interceptor mints
/// one directly without calling [`Next::proceed`].
pub struct ErasedRet(pub Box<dyn Any + Send>);

impl ErasedRet {
    /// Pack a typed return value.
    #[must_use]
    pub fn pack<T: Any + Send>(value: T) -> Self {
        ErasedRet(Box::new(value))
    }

    /// Recover the typed return by downcast.
    ///
    /// # Errors
    /// Returns the original box if the concrete type is not exactly `T`.
    pub fn unpack<T: Any>(self) -> Result<T, Box<dyn Any + Send>> {
        self.0.downcast::<T>().map(|b| *b)
    }

    /// The concrete `TypeId` of the packed return.
    #[must_use]
    pub fn type_id_of(&self) -> TypeId {
        (*self.0).type_id()
    }
}

impl std::fmt::Debug for ErasedRet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ErasedRet(..)")
    }
}

// ─────────────────────────────── AdviceError ────────────────────────────────

/// The error shape at every interception seam — flows into [`LeafError`].
///
/// `#[non_exhaustive]` so later units add arms without an ABI break. Each arm
/// maps to a [`LeafError`] via [`AdviceError::into_leaf_error`] /
/// `From<AdviceError> for LeafError`.
#[non_exhaustive]
#[derive(Debug)]
pub enum AdviceError {
    /// The generated/erased glue failed to downcast args or the return value.
    DowncastMismatch {
        /// The method whose pack/unpack failed.
        method: MethodKey,
    },
    /// Resolving an advisor's [`Interceptor`] (its aspect bean) failed.
    AdvisorResolution(LeafError),
    /// The innermost [`TargetSource::get`] failed.
    TargetResolution(LeafError),
    /// The around-advice body itself returned an error.
    AroundBody(LeafError),
}

impl AdviceError {
    /// Collapse into the one [`LeafError`] chain.
    #[must_use]
    pub fn into_leaf_error(self) -> LeafError {
        match self {
            AdviceError::DowncastMismatch { method } => {
                LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
                    "advice pack/unpack",
                    format!("erased downcast mismatch for method {method:?}"),
                ))
            }
            AdviceError::AdvisorResolution(e)
            | AdviceError::TargetResolution(e)
            | AdviceError::AroundBody(e) => e,
        }
    }
}

impl std::fmt::Display for AdviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdviceError::DowncastMismatch { method } => {
                write!(f, "advice downcast mismatch for {method:?}")
            }
            AdviceError::AdvisorResolution(e) => write!(f, "advisor resolution failed: {e}"),
            AdviceError::TargetResolution(e) => write!(f, "target resolution failed: {e}"),
            AdviceError::AroundBody(e) => write!(f, "around-advice body failed: {e}"),
        }
    }
}

impl std::error::Error for AdviceError {}

impl From<AdviceError> for LeafError {
    fn from(e: AdviceError) -> Self {
        e.into_leaf_error()
    }
}

/// The innermost sink an [`AdviceChain`] / [`Next`] terminates in: resolve the
/// [`TargetSource`] then invoke the real method over the (unchanged) [`Call`].
///
/// A higher-ranked `Fn` so the returned future borrows the per-`proceed` `Call`
/// (not the chain's `'a`), which is what makes [`Next::proceed`] REPLAYABLE: each
/// replay passes a fresh borrow of the same `Call`. The generated newtype's tail
/// downcasts the resolved bean + typed args; the [`ErasedProxy`] tail drives the
/// [`MethodTable`]. `Sync` so it rides the `Send` interceptor futures by ref.
pub type Tail = dyn for<'c> Fn(&'c Call<'c>) -> BoxFuture<'c, Result<ErasedRet, AdviceError>> + Sync;

/// The error type at the [`TargetSource`] seam — an alias over the one chain.
///
/// The design names a distinct `ResolveError`; it is a thin alias over
/// [`LeafError`] (the injection unit established that resolution errors ARE
/// `LeafError`s), so there is exactly one error chain.
pub type ResolveError = LeafError;

// ─────────────────────────────── TargetSource ───────────────────────────────

/// The innermost step of every [`AdviceChain`]: the per-call target supplier.
///
/// Unifies singleton ([`FixedTarget`]), scoped re-resolution ([`ScopeTarget`]),
/// `@Lazy`, and the advised-prototype [`OwnedTarget`] lane. `get` yields a fresh
/// [`ErasedBean`] PER CALL (re-resolved for scoped targets, shared for singletons),
/// so a holder can never stash a stale target through the transparent face.
///
/// Boxed-future seam (AFIT/RPITIT not `dyn`-compatible); `Send + Sync` so it
/// rides the proxy's `Arc<dyn TargetSource>`.
pub trait TargetSource: Send + Sync {
    /// Yield the live target for this call, reading the ambient `Cx` via `cx`.
    ///
    /// # Errors
    /// A [`ResolveError`] if the scoped/prototype target cannot be (re-)resolved.
    fn get<'a>(&'a self, cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<ErasedBean, ResolveError>>;
}

/// The singleton-advised target: a fixed, already-built [`ErasedBean`].
///
/// `get` clones the same `Arc` on every call (the singleton is built once at
/// `after_init`; the chain fires over the SAME instance).
pub struct FixedTarget(ErasedBean);

impl FixedTarget {
    /// Wrap an already-built shared bean as a fixed target.
    #[must_use]
    pub fn new(bean: ErasedBean) -> Self {
        FixedTarget(bean)
    }
}

impl TargetSource for FixedTarget {
    fn get<'a>(
        &'a self,
        _cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<ErasedBean, ResolveError>> {
        let bean = Arc::clone(&self.0);
        Box::pin(async move { Ok(bean) })
    }
}

/// The scoped target: re-resolves the LIVE instance from the ambient `Cx` per
/// call via a [`Weak`](std::sync::Weak) [`Container`] back-reference.
///
/// Holds `scope` + `key` + a `Weak<dyn Container>` — NEVER the live target — so
/// it is a safely-published singleton-shaped NON-OWNING re-resolver; the `Arc`
/// strong-count governs the real free, and request/session teardown stays the
/// `TeardownLedger`'s concern. A dropped container surfaces honestly as a
/// [`ResolveError`] ([`ErrorKind::Cancelled`]), never a swallowed absence.
pub struct ScopeTarget {
    scope: crate::definition::ScopeKind,
    key: BeanKey,
    weak: crate::injection::ContainerRef,
}

impl ScopeTarget {
    /// Build a scoped re-resolving target.
    #[must_use]
    pub fn new(
        scope: crate::definition::ScopeKind,
        key: BeanKey,
        weak: crate::injection::ContainerRef,
    ) -> Self {
        ScopeTarget { scope, key, weak }
    }

    /// The scope this target re-resolves within.
    #[must_use]
    pub fn scope(&self) -> crate::definition::ScopeKind {
        self.scope
    }

    /// The bean key this target re-resolves.
    #[must_use]
    pub fn key(&self) -> &BeanKey {
        &self.key
    }
}

impl TargetSource for ScopeTarget {
    fn get<'a>(
        &'a self,
        _cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<ErasedBean, ResolveError>> {
        let key = self.key.clone();
        let weak = self.weak.clone();
        Box::pin(async move {
            let Some(container) = weak.upgrade() else {
                return Err(LeafError::new(ErrorKind::Cancelled).caused_by(Cause::plain(
                    "scoped target re-resolution",
                    "the owning container has been dropped",
                )));
            };
            // Shared cardinality: the scoped instance is a Published::Shared the
            // ambient InstanceStore memoizes per Cx-key.
            let published = container
                .resolve(
                    key,
                    crate::injection::Strictness::Strict,
                    crate::injection::Cardinality::Single,
                )
                .await?;
            published.into_shared().ok_or_else(|| {
                LeafError::new(ErrorKind::ScopeMismatch).caused_by(Cause::plain(
                    "scoped target re-resolution",
                    "scoped target unexpectedly yielded an owned (prototype) publication",
                ))
            })
        })
    }
}

/// The advised-prototype lane: constructs-and-owns a FRESH instance per call.
///
/// Wraps a factory closure (the prototype `Provider` lane) that yields a fresh
/// shared-shaped [`ErasedBean`] per resolution, so the prototype owned-move
/// invariant is never silently violated through the transparent face.
pub struct OwnedTarget {
    #[allow(clippy::type_complexity)]
    factory: Box<
        dyn Fn() -> BoxFuture<'static, Result<ErasedBean, ResolveError>> + Send + Sync + 'static,
    >,
}

impl OwnedTarget {
    /// Build an advised-prototype target from a fresh-per-call factory.
    #[must_use]
    pub fn new<F>(factory: F) -> Self
    where
        F: Fn() -> BoxFuture<'static, Result<ErasedBean, ResolveError>> + Send + Sync + 'static,
    {
        OwnedTarget { factory: Box::new(factory) }
    }
}

impl TargetSource for OwnedTarget {
    fn get<'a>(
        &'a self,
        _cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<ErasedBean, ResolveError>> {
        (self.factory)()
    }
}

// ───────────────────────────── Call / Next / chain ──────────────────────────

/// One in-flight intercepted method call.
///
/// Carries the [`MethodKey`] identity, the bean [`BeanKey`], the erased argument
/// pack, the innermost [`TargetSource`], and the resolution context. Moves
/// through the chain; an interceptor reads `method`/`bean` for matching and
/// `take_args`/`args_ref` for typed arg access.
pub struct Call<'a> {
    /// The intercepted method's stable identity.
    pub method: MethodKey,
    /// The bean being invoked.
    pub bean: BeanKey,
    /// The erased argument pack (a single tuple box on the common path).
    pub args: ErasedArgs,
    /// The innermost target supplier (proceed()'s fixed inner step).
    pub source: &'a dyn TargetSource,
    /// The resolution context (ambient `Cx`, engine back-ref).
    pub cx: &'a ResolveCtx<'a>,
}

impl<'a> Call<'a> {
    /// Build a call over the given method/bean/args/source/cx.
    #[must_use]
    pub fn new(
        method: MethodKey,
        bean: BeanKey,
        args: ErasedArgs,
        source: &'a dyn TargetSource,
        cx: &'a ResolveCtx<'a>,
    ) -> Self {
        Call { method, bean, args, source, cx }
    }
}

impl std::fmt::Debug for Call<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Call")
            .field("method", &self.method)
            .field("bean", &self.bean)
            .finish_non_exhaustive()
    }
}

/// The continuation handed to an [`Interceptor`]: REPLAYABLE + SKIPPABLE.
///
/// `proceed(&mut self)` walks the remaining interceptors then finally invokes
/// the [`TargetSource`] + real method. It takes `&mut self` and produces a FRESH
/// future each call, so an interceptor may call it ZERO times (cache
/// short-circuit) or MANY times (retry). The tail thunk re-resolves the target
/// per proceed, so a replay re-reads the ambient `Cx`.
pub struct Next<'a> {
    /// The remaining interceptors, outermost-first (index 0 runs next).
    remaining: &'a [Arc<dyn Interceptor>],
    /// The shared tail thunk: resolve the target + invoke the real method.
    tail: &'a Tail,
}

impl<'a> Next<'a> {
    /// Construct a continuation over `remaining` interceptors and a `tail` sink.
    #[must_use]
    pub fn new(remaining: &'a [Arc<dyn Interceptor>], tail: &'a Tail) -> Self {
        Next { remaining, tail }
    }

    /// Advance the chain by one hop over the (unchanged) `call`.
    ///
    /// REPLAYABLE: each invocation builds a fresh future; an interceptor may call
    /// it multiple times (retry) or skip it entirely (cache). The borrow of
    /// `call` is per-proceed, so the same `Call` drives every replay.
    ///
    /// # Errors
    /// Propagates an [`AdviceError`] from an inner interceptor or the target.
    pub fn proceed<'c>(
        &mut self,
        call: &'c Call<'c>,
    ) -> BoxFuture<'c, Result<ErasedRet, AdviceError>>
    where
        'a: 'c,
    {
        match self.remaining.split_first() {
            Some((head, rest)) => {
                let next = Next { remaining: rest, tail: self.tail };
                head.intercept(call, next)
            }
            None => (self.tail)(call),
        }
    }

    /// `true` iff there are no more interceptors before the target (the next
    /// `proceed` hits the [`TargetSource`] directly).
    #[must_use]
    pub fn is_innermost(&self) -> bool {
        self.remaining.is_empty()
    }
}

/// ONE wrap step: wraps a single [`Call`] → [`ErasedRet`].
///
/// `intercept` receives the [`Call`] (by shared ref, so it survives replays) and
/// the REPLAYABLE/SKIPPABLE [`Next`]. Boxed-future seam; `Send + Sync` so it
/// rides the chain's `Arc<dyn Interceptor>`. This is the one-`Call` advice shape
/// — the events `DispatchInterceptor` (which fans out over N listeners) is a
/// STRUCTURALLY DISTINCT sibling trait, sharing only `cmp_chain` + RoleTier.
pub trait Interceptor: Send + Sync {
    /// Wrap one call: do work before/after/around `next.proceed(call)`.
    ///
    /// # Errors
    /// An [`AdviceError`] from this advisor or any inner hop.
    fn intercept<'a>(
        &'a self,
        call: &'a Call<'a>,
        next: Next<'a>,
    ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>>;
}

/// The ordered, replayable, short-circuitable interceptor chain.
///
/// `ordered` is outermost-first (already sorted by [`cmp_chain`]
/// at [`ProxyPlan::freeze`]). [`AdviceChain::invoke`] walks the chain then finally
/// resolves the [`TargetSource`] + invokes the real method via the supplied tail.
pub struct AdviceChain {
    ordered: Box<[Arc<dyn Interceptor>]>,
}

impl AdviceChain {
    /// Build a chain from an already-ordered (outermost-first) interceptor list.
    #[must_use]
    pub fn new(ordered: Box<[Arc<dyn Interceptor>]>) -> Self {
        AdviceChain { ordered }
    }

    /// An empty chain (no advisors) — the unwrapped pass-through degenerate case.
    #[must_use]
    pub fn empty() -> Self {
        AdviceChain { ordered: Box::new([]) }
    }

    /// The number of interceptors in the chain.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ordered.len()
    }

    /// `true` iff the chain has no interceptors.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    /// Invoke the chain over `call`, with `tail` as the innermost sink (resolve
    /// the [`TargetSource`] + dispatch the real method).
    ///
    /// The generated newtype supplies a `tail` that calls `call.source.get(cx)`
    /// then the typed method; the [`ErasedProxy`] supplies one driven by its
    /// [`MethodTable`]. Kept generic over the tail so the kernel hard-codes no
    /// dispatch mechanism.
    ///
    /// # Errors
    /// An [`AdviceError`] from any interceptor or the tail.
    pub fn invoke<'a>(
        &'a self,
        call: &'a Call<'a>,
        tail: &'a Tail,
    ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
        let mut next = Next::new(&self.ordered, tail);
        next.proceed(call)
    }
}

impl std::fmt::Debug for AdviceChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdviceChain").field("len", &self.ordered.len()).finish()
    }
}

// ─────────────────────── dynamic-source-only fallback ───────────────────────

/// One method entry in a dynamic [`ErasedProxy`]'s [`MethodTable`].
///
/// Used ONLY where no compile-time type exists (origin-agnostic per the
/// ownership-model): `invoke` is a downcast thunk that drives the real method
/// over the resolved [`ErasedBean`] + the [`ErasedArgs`].
pub struct MethodEntry {
    /// The method this entry dispatches.
    pub key: MethodKey,
    /// The downcast thunk: resolve-target-already-done, invoke the real method.
    #[allow(clippy::type_complexity)]
    pub invoke: fn(
        &ErasedBean,
        ErasedArgs,
        &ResolveCtx<'_>,
    ) -> BoxFuture<'static, Result<ErasedRet, AdviceError>>,
}

impl std::fmt::Debug for MethodEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MethodEntry").field("key", &self.key).finish_non_exhaustive()
    }
}

/// A const dynamic dispatch table (the `ErasedProxy` method index).
#[derive(Clone, Copy, Debug)]
pub struct MethodTable(pub &'static [MethodEntry]);

impl MethodTable {
    /// Find the entry for `key` (linear over the small const table).
    #[must_use]
    pub fn lookup(&self, key: MethodKey) -> Option<&'static MethodEntry> {
        self.0.iter().find(|e| e.key == key)
    }

    /// The number of methods in the table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` iff the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The dynamic-source-only proxy fallback: an [`AdviceChain`] + a
/// [`TargetSource`] + a const [`MethodTable`].
///
/// Used where no compile-time service-trait type exists; pays per-call erased-args
/// boxing (the common path uses the generated typed newtype instead).
pub struct ErasedProxy {
    /// The wrap chain (already ordered).
    pub chain: Arc<AdviceChain>,
    /// The innermost target supplier.
    pub source: Arc<dyn TargetSource>,
    /// The dynamic method index.
    pub methods: &'static MethodTable,
}

impl std::fmt::Debug for ErasedProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErasedProxy")
            .field("chain", &self.chain)
            .field("methods", &self.methods)
            .finish_non_exhaustive()
    }
}

// ───────────────────────────── Pointcut model ───────────────────────────────

/// The per-call match context a [`Pointcut`] predicate reads — pure DATA, no
/// reflection.
///
/// Built from the macro-emitted [`MethodTable`]/[`AnnotationMetadata`](crate::AnnotationMetadata)
/// (`bean_type`, `method`, `markers`, `arg_types`, `ret_type`).
pub struct JoinPointMeta<'a> {
    /// The bean's concrete `TypeId`.
    pub bean_type: TypeId,
    /// The method's stable identity.
    pub method: MethodKey,
    /// The bean's flat const annotation table (qualifiers/markers).
    pub markers: &'a crate::definition::AnnotationMetadata,
    /// The method's argument `TypeId`s.
    pub arg_types: &'a [TypeId],
    /// The method's return `TypeId`.
    pub ret_type: TypeId,
}

/// A pointcut: a pure predicate over [`JoinPointMeta`] (typed combinators
/// primary; an optional expression front-end lowers to the SAME predicate).
///
/// `Send + Sync` and stored as `&'static dyn Pointcut` on the const
/// [`AdvisorDescriptor`], so the combinator algebra is const-constructible.
pub trait Pointcut: Send + Sync {
    /// `true` iff this pointcut matches the join point.
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool;
}

/// `within::<T>()` — matches when the bean's concrete type is exactly `T`.
///
/// (A `dyn Svc` view matches by the proxy's declared service-trait `TypeId`.)
#[must_use]
pub fn within<T: ?Sized + 'static>() -> Within {
    Within { ty: TypeId::of::<T>() }
}

/// The pointcut produced by [`within`].
#[derive(Clone, Copy, Debug)]
pub struct Within {
    ty: TypeId,
}

impl Pointcut for Within {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        jp.bean_type == self.ty
    }
}

/// `annotated::<A>()` — matches when the bean carries the marker `A`.
///
/// The marker identity is the interned [`MarkerId`] over `A`'s canonical path
/// (coherent with the frozen `AnnotationMetadata.markers` ABI — `MarkerId`, NOT
/// a `TypeId`). The macro mints the id at the const site.
#[must_use]
pub fn annotated_marker(marker: MarkerId) -> Annotated {
    Annotated { marker }
}

/// The pointcut produced by [`annotated_marker`] — matches a declared marker.
#[derive(Clone, Copy, Debug)]
pub struct Annotated {
    marker: MarkerId,
}

impl Pointcut for Annotated {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        jp.markers.markers.contains(&self.marker) || jp.markers.qualifiers.contains(&self.marker)
    }
}

/// `returns::<R>()` — matches when the method's return type is exactly `R`.
#[must_use]
pub fn returns<R: 'static>() -> Returns {
    Returns { ty: TypeId::of::<R>() }
}

/// The pointcut produced by [`returns`].
#[derive(Clone, Copy, Debug)]
pub struct Returns {
    ty: TypeId,
}

impl Pointcut for Returns {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        jp.ret_type == self.ty
    }
}

/// `a & b` — matches when BOTH match.
pub struct And<A, B>(pub A, pub B);
impl<A: Pointcut, B: Pointcut> Pointcut for And<A, B> {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        self.0.matches(jp) && self.1.matches(jp)
    }
}

/// `a | b` — matches when EITHER matches.
pub struct Or<A, B>(pub A, pub B);
impl<A: Pointcut, B: Pointcut> Pointcut for Or<A, B> {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        self.0.matches(jp) || self.1.matches(jp)
    }
}

/// `!a` — matches when `a` does NOT match.
pub struct Not<A>(pub A);
impl<A: Pointcut> Pointcut for Not<A> {
    fn matches(&self, jp: &JoinPointMeta<'_>) -> bool {
        !self.0.matches(jp)
    }
}

/// A pointcut matching EVERY join point (the `Role::Infrastructure` floor / a
/// `within(*)` analogue).
#[derive(Clone, Copy, Debug)]
pub struct Anything;
impl Pointcut for Anything {
    fn matches(&self, _jp: &JoinPointMeta<'_>) -> bool {
        true
    }
}

// ───────────────────────────── advisor model ────────────────────────────────

/// A const factory that resolves an advisor's [`Interceptor`] (its aspect bean)
/// through the same `Provider`/resolve machinery — the bean bridge that lets
/// advice inject collaborators without the advisor itself being a bean.
pub type MakeInterceptor =
    for<'a> fn(&'a dyn Container) -> BoxFuture<'a, Result<Arc<dyn Interceptor>, ResolveError>>;

/// The flat const advisor row — NOT a bean — collected via the `ADVISORS` slice.
///
/// `before`/`after`/`after_returning`/`after_throwing` COLLAPSE to one
/// around-shaped [`Interceptor`] at lowering (the chain sees ONE kind). The
/// `pointcut` is a pure predicate over the macro-emitted metadata; the optional
/// `condition` is gated at `seal()`.
pub struct AdvisorDescriptor {
    /// Stable cross-build advisor identity (the chain tie-break).
    pub id: ContractId,
    /// The integer chain order (set from the pinned `*_ORDER` consts).
    pub order: OrderKey,
    /// Framework-vs-application provenance (the PRIMARY chain key via `RoleTier`).
    pub role: Role,
    /// The typed-combinator pointcut predicate.
    pub pointcut: &'static dyn Pointcut,
    /// The bean bridge that resolves this advisor's interceptor at refresh.
    pub make_interceptor: MakeInterceptor,
}

impl AdvisorDescriptor {
    /// The composite chain key this advisor sorts by (`RoleTier`, `order`, `id`).
    #[must_use]
    pub fn chain_key(&self) -> ChainKey {
        ChainKey { tier: RoleTier::of(self.role), order: self.order, id: self.id }
    }
}

impl std::fmt::Debug for AdvisorDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdvisorDescriptor")
            .field("id", &self.id)
            .field("order", &self.order)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

/// A resolved reference to one advisor in a bean's frozen chain (the O(1)
/// decoration row — identity + order + role, NO live interceptor).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AdvisorRef {
    /// The advisor's stable identity.
    pub id: ContractId,
    /// The integer chain order.
    pub order: OrderKey,
    /// The advisor's role (the `RoleTier` source).
    pub role: Role,
}

impl AdvisorRef {
    /// The composite chain key (`RoleTier`, `cmp_order`, `ContractId`).
    #[must_use]
    pub fn chain_key(&self) -> ChainKey {
        ChainKey { tier: RoleTier::of(self.role), order: self.order, id: self.id }
    }
}

/// The binary-root-assembled capability lattice gating which advisor tiers wire
/// up (the `@EnableAspectJAutoProxy` analogue).
///
/// `admit_infrastructure` is the framework floor (default ON: tx/cache/validation
/// always apply); `admit_application` is the upgrade that admits user `@Aspect`s.
/// Assembled at the binary/app-root where the full enabled-feature set is visible,
/// NEVER as a racing post-processor bean.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CreatorPolicy {
    /// Admit `Role::Infrastructure` / `Role::Support` advisors (the floor).
    pub admit_infrastructure: bool,
    /// Admit `Role::Application` (user `@Aspect`) advisors.
    pub admit_application: bool,
}

impl CreatorPolicy {
    /// The default floor: infrastructure advice on, application aspects off.
    pub const INFRASTRUCTURE_ONLY: CreatorPolicy =
        CreatorPolicy { admit_infrastructure: true, admit_application: false };

    /// The upgraded lattice admitting application aspects too.
    pub const ALL: CreatorPolicy =
        CreatorPolicy { admit_infrastructure: true, admit_application: true };

    /// `true` iff an advisor of `role` is admitted by this policy.
    #[must_use]
    pub const fn admits(&self, role: Role) -> bool {
        match role {
            Role::Application => self.admit_application,
            Role::Support | Role::Infrastructure => self.admit_infrastructure,
        }
    }
}

impl Default for CreatorPolicy {
    fn default() -> Self {
        CreatorPolicy::INFRASTRUCTURE_ONLY
    }
}

/// The frozen, `BeanId`-keyed decoration table computed ONCE at `seal()`.
///
/// `freeze` runs each advisor's pointcut over each bean's join-point metadata
/// (a pure predicate, never a reflective walk), filters by the [`CreatorPolicy`],
/// computes the UNION, and sorts by [`cmp_chain`]. At
/// `after_init` the creator does the O(1) [`ProxyPlan::advisors_for`] lookup;
/// `getEarlyBeanReference` consults the SAME plan so early-ref == final.
#[derive(Debug, Default)]
pub struct ProxyPlan {
    by_bean: std::collections::HashMap<BeanId, Box<[AdvisorRef]>>,
}

/// One bean's join-point metadata as the proxy plan matches it (the macro-emitted
/// view): the bean's concrete type + its declared method join points.
///
/// `freeze` needs the bean's `TypeId`, its provided `dyn Svc` views, and per-method
/// metadata to run pointcuts. This is the minimal const-shaped view; the macro
/// emits one per advisable bean, the registry binds it by `BeanId`.
pub struct BeanJoinPoints<'a> {
    /// The bean's concrete `TypeId`.
    pub bean_type: TypeId,
    /// The bean's annotation metadata (markers/qualifiers).
    pub markers: &'a crate::definition::AnnotationMetadata,
    /// The bean's method join points (one [`JoinPointMeta`]-shaped row each).
    pub methods: &'a [MethodJoinPoint],
}

/// One method's static join-point row (used to run pointcuts at `freeze`).
#[derive(Clone, Debug)]
pub struct MethodJoinPoint {
    /// The method's stable identity.
    pub method: MethodKey,
    /// The method's argument `TypeId`s.
    pub arg_types: SmallVec<[TypeId; 4]>,
    /// The method's return `TypeId`.
    pub ret_type: TypeId,
}

/// The CONST-CONSTRUCTIBLE per-method join-point row the macro emits (the const
/// twin of [`MethodJoinPoint`]).
///
/// [`MethodJoinPoint::arg_types`] is a [`SmallVec`] (`SmallVec::new()` is not a
/// `const fn`), so it cannot ride a `const`/`static` initializer the macro emits.
/// This spec uses a `&'static [TypeId]` instead — fully const-constructible via the
/// inline-`const { TypeId::of::<_>() }` seam — and [`MethodJoinPointSpec::reify`]
/// builds the runtime [`MethodJoinPoint`] (the `SmallVec`) at the leaf-boot JOIN.
#[derive(Clone, Copy, Debug)]
pub struct MethodJoinPointSpec {
    /// The method's stable identity.
    pub method: MethodKey,
    /// The method's argument `TypeId`s (a const slice, reified into a `SmallVec`).
    pub arg_types: &'static [TypeId],
    /// The method's return `TypeId`.
    pub ret_type: TypeId,
}

impl MethodJoinPointSpec {
    /// Reify into the runtime [`MethodJoinPoint`] (building the `SmallVec`).
    #[must_use]
    pub fn reify(&self) -> MethodJoinPoint {
        MethodJoinPoint {
            method: self.method,
            arg_types: SmallVec::from_slice(self.arg_types),
            ret_type: self.ret_type,
        }
    }
}

/// The CONST-CONSTRUCTIBLE per-bean join-point descriptor the macro emits beside an
/// `#[advisable]`/`#[aspect]` bean's `Descriptor` (the const twin of
/// [`BeanJoinPoints`]) — the proxy analogue of the `__leaf_seed_<Ident>` /
/// `__leaf_guard_<Ident>` pairing consts.
///
/// `#[advisable]`/`#[aspect]` emits one PUBLIC `__leaf_joinpoints_<Ident>` const of
/// this type (the bean's concrete type + its flat `AnnotationMetadata` + its const
/// method specs). leaf-boot JOINs it to the bean's frozen `BeanId` by `ContractId`
/// and [`reify`](BeanJoinPointsSpec::reify_methods)es it into the runtime
/// [`BeanJoinPoints`] [`ProxyPlan::freeze`] runs pointcuts over — so the proxy plan
/// is built from REAL macro-emitted per-bean data, never a hand-mirrored view.
#[derive(Clone, Copy, Debug)]
pub struct BeanJoinPointsSpec {
    /// The bean's concrete `TypeId` (the `within::<T>()` pointcut key).
    pub bean_type: TypeId,
    /// The bean's flat annotation metadata (the `annotated::<A>()` pointcut key).
    pub markers: &'static crate::definition::AnnotationMetadata,
    /// The bean's const method join-point specs (one per advisable method).
    pub methods: &'static [MethodJoinPointSpec],
}

impl BeanJoinPointsSpec {
    /// Reify this spec's const method specs into the runtime [`MethodJoinPoint`]s
    /// (building each `SmallVec`) — the leaf-boot JOIN calls this, then borrows the
    /// result to build a [`BeanJoinPoints`] for [`ProxyPlan::freeze`].
    #[must_use]
    pub fn reify_methods(&self) -> Vec<MethodJoinPoint> {
        self.methods.iter().map(MethodJoinPointSpec::reify).collect()
    }
}

impl ProxyPlan {
    /// A plan with no decorations (the bare-engine parity case: no creator).
    #[must_use]
    pub fn empty() -> Self {
        ProxyPlan { by_bean: std::collections::HashMap::new() }
    }

    /// Compute the frozen plan: match every admitted advisor against every bean's
    /// join points, union the matches, and sort each bean's chain by `cmp_chain`.
    ///
    /// `join_points` supplies the macro-emitted per-bean join-point view keyed by
    /// `BeanId` (the registry binds the const rows at `seal()`). A bean with no
    /// matching advisor mints no entry (it passes through UNWRAPPED at `after_init`).
    ///
    /// # Errors
    /// An [`AssemblyError`](crate::AssemblyError) currently never returned (the
    /// match is pure); the `Result` is kept so the seam can grow loud collision /
    /// policy faults without an ABI break.
    pub fn freeze(
        advisors: &[AdvisorDescriptor],
        registry: &Registry,
        policy: &CreatorPolicy,
        join_points: &std::collections::HashMap<BeanId, BeanJoinPoints<'_>>,
    ) -> Result<ProxyPlan, crate::AssemblyError> {
        let _ = registry; // bound by id via join_points; reserved for richer matching
        let mut by_bean: std::collections::HashMap<BeanId, Box<[AdvisorRef]>> =
            std::collections::HashMap::new();

        for (&bean_id, jp) in join_points {
            let mut matched: Vec<AdvisorRef> = Vec::new();
            for adv in advisors {
                if !policy.admits(adv.role) {
                    continue;
                }
                // A bean matches an advisor iff ANY of its method join points match.
                let hit = jp.methods.iter().any(|m| {
                    let meta = JoinPointMeta {
                        bean_type: jp.bean_type,
                        method: m.method,
                        markers: jp.markers,
                        arg_types: &m.arg_types,
                        ret_type: m.ret_type,
                    };
                    adv.pointcut.matches(&meta)
                });
                if hit {
                    matched.push(AdvisorRef { id: adv.id, order: adv.order, role: adv.role });
                }
            }
            if !matched.is_empty() {
                matched.sort_by(|a, b| cmp_chain(&a.chain_key(), &b.chain_key()));
                by_bean.insert(bean_id, matched.into_boxed_slice());
            }
        }

        Ok(ProxyPlan { by_bean })
    }

    /// The O(1) decoration lookup: the (already `cmp_chain`-sorted) advisors for
    /// `bean`, or an empty slice (un-advised → passes through UNWRAPPED).
    #[must_use]
    pub fn advisors_for(&self, bean: BeanId) -> &[AdvisorRef] {
        self.by_bean.get(&bean).map_or(&[], |b| b)
    }

    /// `true` iff `bean` is advised (has at least one matching advisor).
    #[must_use]
    pub fn is_advised(&self, bean: BeanId) -> bool {
        self.by_bean.contains_key(&bean)
    }

    /// The number of advised beans in the plan.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_bean.len()
    }

    /// `true` iff no bean is advised.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_bean.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::AnnotationMetadata;
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

    // ── shared test helpers ──────────────────────────────────────────────────

    fn ctx() -> ResolveCtx<'static> {
        ResolveCtx::root()
    }

    fn mkey(p: &str) -> MethodKey {
        MethodKey::of(p)
    }

    /// A concrete target bean.
    #[derive(Debug)]
    struct Svc {
        base: i64,
    }

    /// Build a FixedTarget over a Svc.
    fn fixed_svc(base: i64) -> FixedTarget {
        let bean: ErasedBean = Arc::new(Svc { base });
        FixedTarget::new(bean)
    }

    /// The canonical tail: resolve the target, downcast to Svc, add the arg.
    fn svc_tail<'c>(call: &'c Call<'c>) -> BoxFuture<'c, Result<ErasedRet, AdviceError>> {
        Box::pin(async move {
            let bean = call.source.get(call.cx).await.map_err(AdviceError::TargetResolution)?;
            let svc = bean
                .downcast_ref::<Svc>()
                .ok_or(AdviceError::DowncastMismatch { method: call.method })?;
            // The packed arg is an i64 addend.
            let add = *call
                .args
                .0
                .downcast_ref::<i64>()
                .ok_or(AdviceError::DowncastMismatch { method: call.method })?;
            Ok(ErasedRet::pack(svc.base + add))
        })
    }

    // An interceptor that records its name into a shared log on entry & exit.
    struct Recorder {
        name: &'static str,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl Interceptor for Recorder {
        fn intercept<'a>(
            &'a self,
            call: &'a Call<'a>,
            mut next: Next<'a>,
        ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
            Box::pin(async move {
                self.log.lock().unwrap().push(format!("{}:enter", self.name));
                let r = next.proceed(call).await;
                self.log.lock().unwrap().push(format!("{}:exit", self.name));
                r
            })
        }
    }

    // ── TargetSource ─────────────────────────────────────────────────────────

    #[test]
    fn fixed_target_yields_the_same_shared_bean() {
        let t = fixed_svc(10);
        let cx = ctx();
        let a = futures::executor::block_on(t.get(&cx)).expect("get");
        let b = futures::executor::block_on(t.get(&cx)).expect("get");
        // Same allocation each call (shared singleton target).
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.downcast_ref::<Svc>().unwrap().base, 10);
    }

    #[test]
    fn owned_target_yields_a_fresh_instance_per_call() {
        let counter = Arc::new(AtomicU32::new(0));
        let c2 = Arc::clone(&counter);
        let t = OwnedTarget::new(move || {
            let n = c2.fetch_add(1, AtomicOrdering::SeqCst);
            Box::pin(async move {
                let bean: ErasedBean = Arc::new(Svc { base: n as i64 });
                Ok(bean)
            })
        });
        let cx = ctx();
        let a = futures::executor::block_on(t.get(&cx)).expect("get");
        let b = futures::executor::block_on(t.get(&cx)).expect("get");
        // Distinct allocations (fresh prototype per resolution).
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(a.downcast_ref::<Svc>().unwrap().base, 0);
        assert_eq!(b.downcast_ref::<Svc>().unwrap().base, 1);
    }

    // A fake container that re-resolves a FRESH Svc per call (scoped re-resolution).
    struct FakeContainer {
        calls: AtomicU32,
    }
    impl Container for FakeContainer {
        fn resolve(
            &self,
            _key: BeanKey,
            _strictness: crate::injection::Strictness,
            _cardinality: crate::injection::Cardinality,
        ) -> BoxFuture<'_, Result<crate::handle::Published, LeafError>> {
            let n = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            Box::pin(async move { Ok(crate::handle::Published::shared_value(Svc { base: n as i64 })) })
        }
    }

    #[test]
    fn scope_target_reresolves_live_target_each_call() {
        let container: Arc<dyn Container> = Arc::new(FakeContainer { calls: AtomicU32::new(0) });
        let weak = Arc::downgrade(&container);
        let t = ScopeTarget::new(
            crate::definition::ScopeKind::REQUEST,
            BeanKey::ByType(TypeId::of::<Svc>()),
            weak,
        );
        let cx = ctx();
        let a = futures::executor::block_on(t.get(&cx)).expect("get");
        let b = futures::executor::block_on(t.get(&cx)).expect("get");
        // Re-resolved each call (request scope yields a fresh live instance).
        assert_eq!(a.downcast_ref::<Svc>().unwrap().base, 0);
        assert_eq!(b.downcast_ref::<Svc>().unwrap().base, 1);
    }

    #[test]
    fn scope_target_with_dropped_container_fails_honestly() {
        let container: Arc<dyn Container> = Arc::new(FakeContainer { calls: AtomicU32::new(0) });
        let weak = Arc::downgrade(&container);
        let t = ScopeTarget::new(
            crate::definition::ScopeKind::REQUEST,
            BeanKey::ByType(TypeId::of::<Svc>()),
            weak,
        );
        drop(container); // the owning container is gone
        let cx = ctx();
        let err = futures::executor::block_on(t.get(&cx)).expect_err("must fail loudly");
        assert_eq!(err.kind, ErrorKind::Cancelled, "dropped container is honest, not swallowed");
    }

    // ── chain order + short-circuit + replay ─────────────────────────────────

    #[test]
    fn empty_chain_invokes_the_target_directly() {
        let chain = AdviceChain::empty();
        let source = fixed_svc(100);
        let cx = ctx();
        let call = Call::new(mkey("svc::add"), BeanKey::ByType(TypeId::of::<Svc>()), ErasedArgs::pack(5_i64), &source, &cx);
        let out = futures::executor::block_on(chain.invoke(&call, &svc_tail)).expect("ok");
        assert_eq!(out.unpack::<i64>().unwrap(), 105);
    }

    #[test]
    fn chain_runs_interceptors_outermost_first_then_unwinds() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let chain = AdviceChain::new(Box::new([
            Arc::new(Recorder { name: "outer", log: Arc::clone(&log) }) as Arc<dyn Interceptor>,
            Arc::new(Recorder { name: "inner", log: Arc::clone(&log) }) as Arc<dyn Interceptor>,
        ]));
        let source = fixed_svc(1);
        let cx = ctx();
        let call = Call::new(mkey("svc::add"), BeanKey::ByType(TypeId::of::<Svc>()), ErasedArgs::pack(2_i64), &source, &cx);
        let out = futures::executor::block_on(chain.invoke(&call, &svc_tail)).expect("ok");
        assert_eq!(out.unpack::<i64>().unwrap(), 3);
        // Outermost enters first, exits last (proper nesting).
        let order = log.lock().unwrap().clone();
        assert_eq!(order, vec!["outer:enter", "inner:enter", "inner:exit", "outer:exit"]);
    }

    // A cache-style interceptor: short-circuit WITHOUT calling proceed.
    struct ShortCircuit {
        value: i64,
    }
    impl Interceptor for ShortCircuit {
        fn intercept<'a>(
            &'a self,
            _call: &'a Call<'a>,
            _next: Next<'a>,
        ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
            let v = self.value;
            Box::pin(async move { Ok(ErasedRet::pack(v)) })
        }
    }

    #[test]
    fn interceptor_can_short_circuit_skipping_proceed_and_target() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let chain = AdviceChain::new(Box::new([
            Arc::new(ShortCircuit { value: 999 }) as Arc<dyn Interceptor>,
            // This inner recorder must NEVER run (the cache short-circuits).
            Arc::new(Recorder { name: "inner", log: Arc::clone(&log) }) as Arc<dyn Interceptor>,
        ]));
        let source = fixed_svc(7);
        let cx = ctx();
        let call = Call::new(mkey("svc::add"), BeanKey::ByType(TypeId::of::<Svc>()), ErasedArgs::pack(1_i64), &source, &cx);
        let out = futures::executor::block_on(chain.invoke(&call, &svc_tail)).expect("ok");
        assert_eq!(out.unpack::<i64>().unwrap(), 999, "cache value, target never ran");
        assert!(log.lock().unwrap().is_empty(), "inner interceptor must not run");
    }

    // A retry-style interceptor: call proceed N times (REPLAYABLE).
    struct Retry {
        attempts: AtomicU32,
        target_calls: Arc<AtomicU32>,
    }
    impl Interceptor for Retry {
        fn intercept<'a>(
            &'a self,
            call: &'a Call<'a>,
            mut next: Next<'a>,
        ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
            Box::pin(async move {
                // Proceed twice; the second result wins.
                let _first = next.proceed(call).await;
                self.attempts.fetch_add(1, AtomicOrdering::SeqCst);
                let second = next.proceed(call).await;
                self.attempts.fetch_add(1, AtomicOrdering::SeqCst);
                let _ = &self.target_calls;
                second
            })
        }
    }

    #[test]
    fn next_is_replayable_proceed_can_run_the_target_twice() {
        let target_calls = Arc::new(AtomicU32::new(0));
        let tc2 = Arc::clone(&target_calls);
        // A tail that counts how many times the real method ran. Boxed as the
        // HRTB `Tail` so its returned future borrows the per-proceed `Call`.
        let tail: Box<Tail> = Box::new(move |call: &Call<'_>| {
            tc2.fetch_add(1, AtomicOrdering::SeqCst);
            let add = *call.args.0.downcast_ref::<i64>().unwrap();
            Box::pin(async move { Ok(ErasedRet::pack(add + 1)) }) as BoxFuture<'_, _>
        });
        let retry = Retry { attempts: AtomicU32::new(0), target_calls: Arc::clone(&target_calls) };
        let chain = AdviceChain::new(Box::new([Arc::new(retry) as Arc<dyn Interceptor>]));
        let source = fixed_svc(0);
        let cx = ctx();
        let call = Call::new(mkey("svc::add"), BeanKey::ByType(TypeId::of::<Svc>()), ErasedArgs::pack(41_i64), &source, &cx);
        let out = futures::executor::block_on(chain.invoke(&call, &*tail)).expect("ok");
        assert_eq!(out.unpack::<i64>().unwrap(), 42);
        // The target ran exactly twice (the replay).
        assert_eq!(target_calls.load(AtomicOrdering::SeqCst), 2);
    }

    // ── ErasedArgs: cloneable / replayable args (the advised-arg ABI) ─────────

    #[test]
    fn erased_args_clone_re_supplies_a_fresh_owned_copy() {
        // The advised-arg bound (Clone): the args pack clones into a fresh owned copy
        // a replay can consume, without the caller knowing the concrete tuple type.
        let args = ErasedArgs::pack((7_i64, "hi".to_string()));
        let cloned = args.clone();
        // The original still inspectable (clone did not consume it).
        assert_eq!(args.downcast_ref::<(i64, String)>().unwrap().0, 7);
        // The clone is an independent owned value (unpack consumes it).
        let (n, s) = cloned.unpack::<(i64, String)>().expect("typed");
        assert_eq!((n, s.as_str()), (7, "hi"));
        // The original is STILL intact after the clone was consumed.
        assert_eq!(args.downcast_ref::<(i64, String)>().unwrap().1, "hi");
    }

    #[test]
    fn erased_args_replay_yields_independent_clones() {
        let args = ErasedArgs::pack((41_i64,));
        let a = args.replay();
        let b = args.replay();
        assert_eq!(a.unpack::<(i64,)>().unwrap().0, 41);
        assert_eq!(b.unpack::<(i64,)>().unwrap().0, 41);
        // The source survives N replays.
        assert_eq!(args.downcast_ref::<(i64,)>().unwrap().0, 41);
    }

    // A retrying interceptor that re-proceeds N times; the tail recovers fresh typed
    // args off `Call.args` PER attempt (the args-bearing replay the take-once cell
    // could not do). Each attempt sees the SAME args.
    struct RetryN {
        n: u32,
    }
    impl Interceptor for RetryN {
        fn intercept<'a>(
            &'a self,
            call: &'a Call<'a>,
            mut next: Next<'a>,
        ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
            let n = self.n;
            Box::pin(async move {
                let mut last = next.proceed(call).await;
                for _ in 1..n {
                    last = next.proceed(call).await;
                }
                last
            })
        }
    }

    #[test]
    fn a_retrying_interceptor_re_proceeds_an_args_bearing_method_each_attempt() {
        // The headline: an args-bearing advised method is re-proceeded N times, each
        // attempt recovering a FRESH typed clone of the args off `Call.args` — the
        // replayable args-bearing retry the design requires.
        let target_calls = Arc::new(AtomicU32::new(0));
        let seen_args = Arc::new(std::sync::Mutex::new(Vec::<i64>::new()));
        let tc = Arc::clone(&target_calls);
        let sa = Arc::clone(&seen_args);
        // The tail re-derives typed args off Call.args via a FRESH replay per proceed
        // (what the boot tail does), so a non-Copy / owned arg is re-runnable too.
        let tail: Box<Tail> = Box::new(move |call: &Call<'_>| {
            tc.fetch_add(1, AtomicOrdering::SeqCst);
            let fresh = call.args.replay();
            let (add,) = fresh.unpack::<(i64,)>().expect("fresh typed args");
            sa.lock().unwrap().push(add);
            Box::pin(async move { Ok(ErasedRet::pack(add + 1)) }) as BoxFuture<'_, _>
        });
        let chain = AdviceChain::new(Box::new([Arc::new(RetryN { n: 3 }) as Arc<dyn Interceptor>]));
        let source = fixed_svc(0);
        let cx = ctx();
        // The args ride `Call.args` (inspectable + replayable), NOT a take-once cell.
        let call = Call::new(
            mkey("svc::add"),
            BeanKey::ByType(TypeId::of::<Svc>()),
            ErasedArgs::pack((41_i64,)),
            &source,
            &cx,
        );
        let out = futures::executor::block_on(chain.invoke(&call, &*tail)).expect("ok");
        assert_eq!(out.unpack::<i64>().unwrap(), 42);
        // The target ran 3 times (the args-bearing replay) — each seeing the SAME arg.
        assert_eq!(target_calls.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(*seen_args.lock().unwrap(), vec![41, 41, 41]);
        // And the args are STILL inspectable on the Call after all replays.
        assert_eq!(call.args.downcast_ref::<(i64,)>().unwrap().0, 41);
    }

    #[test]
    fn an_interceptor_reads_arg_zero_off_the_call_and_routes_on_it() {
        // A CacheInterceptor-style read: route on arg #0 WITHOUT consuming the args.
        struct RouteOnArg0;
        impl Interceptor for RouteOnArg0 {
            fn intercept<'a>(
                &'a self,
                call: &'a Call<'a>,
                mut next: Next<'a>,
            ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
                // Read arg #0 off the Call (inspectable, NOT taken).
                let routed = call.args.downcast_ref::<(i64,)>().map(|(a,)| *a);
                Box::pin(async move {
                    if routed == Some(0) {
                        // Short-circuit on a sentinel arg WITHOUT running the target.
                        return Ok(ErasedRet::pack(-1_i64));
                    }
                    next.proceed(call).await
                })
            }
        }
        let chain = AdviceChain::new(Box::new([Arc::new(RouteOnArg0) as Arc<dyn Interceptor>]));
        let source = fixed_svc(100);
        let cx = ctx();
        // A regular arg proceeds; the sentinel 0 short-circuits.
        let call = Call::new(
            mkey("svc::add"),
            BeanKey::ByType(TypeId::of::<Svc>()),
            ErasedArgs::pack((5_i64,)),
            &source,
            &cx,
        );
        // svc_tail expects a bare i64 addend; use a small tuple-aware tail here.
        let tail: Box<Tail> = Box::new(|call: &Call<'_>| {
            Box::pin(async move {
                let bean = call.source.get(call.cx).await.map_err(AdviceError::TargetResolution)?;
                let svc = bean.downcast_ref::<Svc>().unwrap();
                let (add,) = call.args.downcast_ref::<(i64,)>().copied().unwrap();
                Ok(ErasedRet::pack(svc.base + add))
            })
        });
        let out = futures::executor::block_on(chain.invoke(&call, &*tail)).expect("ok");
        assert_eq!(out.unpack::<i64>().unwrap(), 105, "a non-sentinel arg proceeds (100 + 5)");

        let sentinel = Call::new(
            mkey("svc::add"),
            BeanKey::ByType(TypeId::of::<Svc>()),
            ErasedArgs::pack((0_i64,)),
            &source,
            &cx,
        );
        let out = futures::executor::block_on(chain.invoke(&sentinel, &*tail)).expect("ok");
        assert_eq!(out.unpack::<i64>().unwrap(), -1, "arg #0 routed to the short-circuit");
    }

    // ── Pointcut combinators ─────────────────────────────────────────────────

    fn jp_for<T: 'static>(ret: TypeId, markers: &'static AnnotationMetadata) -> JoinPointMeta<'static> {
        JoinPointMeta {
            bean_type: TypeId::of::<T>(),
            method: mkey("svc::m"),
            markers,
            arg_types: &[],
            ret_type: ret,
        }
    }

    #[test]
    fn within_matches_exact_bean_type() {
        let pc = within::<Svc>();
        assert!(pc.matches(&jp_for::<Svc>(TypeId::of::<i64>(), &AnnotationMetadata::EMPTY)));
        assert!(!pc.matches(&jp_for::<String>(TypeId::of::<i64>(), &AnnotationMetadata::EMPTY)));
    }

    #[test]
    fn returns_matches_exact_return_type() {
        let pc = returns::<i64>();
        assert!(pc.matches(&jp_for::<Svc>(TypeId::of::<i64>(), &AnnotationMetadata::EMPTY)));
        assert!(!pc.matches(&jp_for::<Svc>(TypeId::of::<String>(), &AnnotationMetadata::EMPTY)));
    }

    static TXN_META: AnnotationMetadata = AnnotationMetadata {
        qualifiers: &[],
        markers: &[MarkerId::of("test::Transactional")],
        depends_on: &[],
        candidate_role: crate::definition::CandidateRole::NORMAL,
        autowire_candidate: true,
    };

    #[test]
    fn annotated_matches_declared_marker() {
        let pc = annotated_marker(MarkerId::of("test::Transactional"));
        assert!(pc.matches(&jp_for::<Svc>(TypeId::of::<i64>(), &TXN_META)));
        assert!(!pc.matches(&jp_for::<Svc>(TypeId::of::<i64>(), &AnnotationMetadata::EMPTY)));
    }

    #[test]
    fn combinators_and_or_not_compose() {
        // within::<Svc>() & returns::<i64>()
        let pc = And(within::<Svc>(), returns::<i64>());
        assert!(pc.matches(&jp_for::<Svc>(TypeId::of::<i64>(), &AnnotationMetadata::EMPTY)));
        assert!(!pc.matches(&jp_for::<Svc>(TypeId::of::<String>(), &AnnotationMetadata::EMPTY)));
        // within::<Svc>() | within::<String>()
        let pc = Or(within::<Svc>(), within::<String>());
        assert!(pc.matches(&jp_for::<String>(TypeId::of::<i64>(), &AnnotationMetadata::EMPTY)));
        // !within::<Svc>()
        let pc = Not(within::<Svc>());
        assert!(!pc.matches(&jp_for::<Svc>(TypeId::of::<i64>(), &AnnotationMetadata::EMPTY)));
        assert!(pc.matches(&jp_for::<String>(TypeId::of::<i64>(), &AnnotationMetadata::EMPTY)));
    }

    // ── ProxyPlan::freeze ────────────────────────────────────────────────────

    fn dummy_make() -> MakeInterceptor {
        |_c| Box::pin(async { Err(LeafError::new(ErrorKind::ConstructionFailed)) })
    }

    #[test]
    fn proxy_plan_orders_advisors_by_cmp_chain_and_filters_by_policy() {
        // Two infrastructure advisors (tx=500, cache=400) + one application aspect.
        let cache = AdvisorDescriptor {
            id: ContractId::of("test::CacheAdvisor"),
            order: OrderKey { value: crate::CACHE_ORDER, source: crate::OrderSource::Annotation },
            role: Role::Infrastructure,
            pointcut: &ANY,
            make_interceptor: dummy_make(),
        };
        let tx = AdvisorDescriptor {
            id: ContractId::of("test::TxAdvisor"),
            order: OrderKey { value: crate::TX_ORDER, source: crate::OrderSource::Annotation },
            role: Role::Infrastructure,
            pointcut: &ANY,
            make_interceptor: dummy_make(),
        };
        let app = AdvisorDescriptor {
            id: ContractId::of("test::AppAspect"),
            order: OrderKey { value: 0, source: crate::OrderSource::Annotation },
            role: Role::Application,
            pointcut: &ANY,
            make_interceptor: dummy_make(),
        };
        let advisors = [tx, cache, app];

        // Build a registry with one bean so we have a real BeanId.
        let registry = build_single_bean_registry();
        let bean_id = registry.ids().next().unwrap();

        let methods = vec![MethodJoinPoint {
            method: mkey("svc::add"),
            arg_types: SmallVec::new(),
            ret_type: TypeId::of::<i64>(),
        }];
        let mut jps = std::collections::HashMap::new();
        jps.insert(
            bean_id,
            BeanJoinPoints {
                bean_type: TypeId::of::<Svc>(),
                markers: &AnnotationMetadata::EMPTY,
                methods: &methods,
            },
        );

        // INFRASTRUCTURE_ONLY: the app aspect is filtered OUT.
        let plan = ProxyPlan::freeze(&advisors, &registry, &CreatorPolicy::INFRASTRUCTURE_ONLY, &jps)
            .expect("freeze");
        let chain = plan.advisors_for(bean_id);
        assert_eq!(chain.len(), 2, "app aspect filtered by INFRASTRUCTURE_ONLY");
        // cache (400) sorts before tx (500) within the Infrastructure tier.
        assert_eq!(chain[0].id, ContractId::of("test::CacheAdvisor"));
        assert_eq!(chain[1].id, ContractId::of("test::TxAdvisor"));

        // ALL: the app aspect is admitted; Application tier sorts INNERMOST (last).
        let plan_all =
            ProxyPlan::freeze(&advisors, &registry, &CreatorPolicy::ALL, &jps).expect("freeze");
        let chain_all = plan_all.advisors_for(bean_id);
        assert_eq!(chain_all.len(), 3);
        assert_eq!(chain_all[2].id, ContractId::of("test::AppAspect"), "app innermost");
    }

    #[test]
    fn proxy_plan_unmatched_bean_passes_through_unwrapped() {
        let nomatch = AdvisorDescriptor {
            id: ContractId::of("test::NoMatch"),
            order: OrderKey::implicit(),
            role: Role::Infrastructure,
            pointcut: &NEVER,
            make_interceptor: dummy_make(),
        };
        let registry = build_single_bean_registry();
        let bean_id = registry.ids().next().unwrap();
        let methods = vec![MethodJoinPoint {
            method: mkey("svc::add"),
            arg_types: SmallVec::new(),
            ret_type: TypeId::of::<i64>(),
        }];
        let mut jps = std::collections::HashMap::new();
        jps.insert(
            bean_id,
            BeanJoinPoints {
                bean_type: TypeId::of::<Svc>(),
                markers: &AnnotationMetadata::EMPTY,
                methods: &methods,
            },
        );
        let plan = ProxyPlan::freeze(&[nomatch], &registry, &CreatorPolicy::ALL, &jps).expect("freeze");
        assert!(!plan.is_advised(bean_id));
        assert!(plan.advisors_for(bean_id).is_empty());
        assert!(plan.is_empty());
    }

    // Pointcut singletons for the plan tests.
    static ANY: Anything = Anything;
    struct Never;
    impl Pointcut for Never {
        fn matches(&self, _jp: &JoinPointMeta<'_>) -> bool {
            false
        }
    }
    static NEVER: Never = Never;

    // Build a frozen registry with exactly one Svc bean.
    fn build_single_bean_registry() -> Registry {
        use crate::definition::{AnnotationMetadata as AM, Descriptor, Role as R, ScopeDef};
        use crate::provider::{Provider, ResolveCtx as RC};
        use crate::handle::Published;
        use crate::registry::RegistryBuilder;

        struct P(Descriptor);
        impl Provider for P {
            fn descriptor(&self) -> &Descriptor {
                &self.0
            }
            fn provide<'a>(
                &'a self,
                _cx: &'a RC<'a>,
            ) -> BoxFuture<'a, Result<Published, LeafError>> {
                Box::pin(async { Ok(Published::shared_value(Svc { base: 0 })) })
            }
        }
        let desc = Descriptor {
            contract: ContractId::of("test::Svc"),
            self_type: TypeId::of::<Svc>(),
            provides: &[],
            declared_name: Some("svc"),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: R::Application,
            meta: &AM::EMPTY,
            parent: None,
            origin: crate::error::Origin::Native { crate_name: Some("leaf-core") },
        };
        let mut b = RegistryBuilder::new();
        b.register(desc, Arc::new(P(desc)) as Arc<dyn Provider>).expect("register");
        b.freeze().expect("freeze")
    }
}
