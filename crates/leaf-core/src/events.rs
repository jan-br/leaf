//! `leaf_core::events` — the in-process observer bus + the dispatch multicaster.
//!
//! leaf's publish/subscribe model (phase3/12-events): `@EventListener` /
//! [`ApplicationListener`] registration, the single replaceable multicaster that
//! owns dispatch policy (sync-ordered by [`cmp_order`](crate::cmp_order), opt-in
//! async, return-value-as-new-event chaining, error isolation), and the built-in
//! lifecycle/availability facts that ride the SAME bus.
//!
//! ## A SEPARATE shape from advice (C5)
//!
//! The dispatch concern pipeline is a NEW [`DispatchInterceptor`] sibling
//! around-advice trait that fans OUT over N listeners — structurally DISTINCT from
//! the proxy substrate's one-[`Call`](crate::proxy::Call)
//! [`Interceptor`](crate::proxy::Interceptor). It SHARES with AOP only
//! [`cmp_chain`](crate::cmp_chain) + the [`RoleTier`](crate::RoleTier) grade, NOT
//! the `Interceptor` type. Async / error-isolation / context-propagation / metrics
//! are [`DispatchInterceptor`] chain entries ordered by `cmp_chain` over the pinned
//! `ASYNC_DISPATCH`/`ERROR_ISOLATION`/`CONTEXT_PROP`/`METRICS` consts, with the
//! inline-await [`CoreDispatch`] loop as the innermost sink.
//!
//! ## Availability over watch RunState
//!
//! Availability liveness/readiness is the upstream `watch`-shaped reactive cell
//! ([`WatchSender`](crate::WatchSender)/[`WatchReceiver`](crate::WatchReceiver) —
//! the ONE primitive the lifecycle unit minted), NOT a second mechanism. A single
//! projector re-publishes each transition as an [`AvailabilityChanged`] event so
//! the bus stays stateless.

use std::any::{Any, TypeId};
use std::sync::Arc;

use smallvec::SmallVec;

use crate::error::LeafError;
use crate::future::BoxFuture;
use crate::handle::{Bean, ErasedBean};
use crate::identity::ContractId;
use crate::lifecycle::{watch_channel, WatchReceiver, WatchSender};
use crate::order::{cmp_chain, cmp_order, ChainKey, OrderKey, RoleTier};

// ───────────────────────────── event currency ───────────────────────────────

/// A container's stable identity (= [`ContractId`]) — the event origin.
pub type ContainerId = ContractId;

/// An erased event for the chaining / dynamic re-publish path.
///
/// A chained event of a DIFFERENT type re-enters publish as an `ErasedEvent`
/// (`Box<dyn Any + Send>` + its `TypeId`); the re-publish pays one downcast at the
/// next channel boundary. Carries the optional source handle + the originating
/// container id.
pub struct ErasedEvent {
    /// The boxed event payload.
    ///
    /// `+ Sync` is load-bearing: the event is shared by reference (`&ErasedEvent`
    /// / `&dyn Any`) across the `Send` dispatch / listener futures, so the payload
    /// must be `Sync`. Bus events ride the same `Send + Sync + 'static` publication
    /// contract as beans, so this is the natural bound.
    pub value: Box<dyn Any + Send + Sync>,
    /// The payload's concrete type (the channel key).
    pub type_id: TypeId,
    /// The optional source bean handle (the `Event<P>` envelope's `source`).
    pub source: Option<ErasedBean>,
    /// The originating container's id (hierarchy double-delivery disambiguation).
    pub origin: ContainerId,
}

impl ErasedEvent {
    /// Erase a typed event into the chaining/dynamic shape.
    #[must_use]
    pub fn new<E: Any + Send + Sync>(event: E, origin: ContainerId) -> Self {
        ErasedEvent {
            value: Box::new(event),
            type_id: TypeId::of::<E>(),
            source: None,
            origin,
        }
    }

    /// Attach a source bean handle (builder style).
    #[must_use]
    pub fn with_source(mut self, source: ErasedBean) -> Self {
        self.source = Some(source);
        self
    }

    /// Borrow the payload as `&(dyn Any + Send + Sync)` (what an adapter downcasts).
    #[must_use]
    pub fn payload(&self) -> &(dyn Any + Send + Sync) {
        &*self.value
    }
}

impl std::fmt::Debug for ErasedEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErasedEvent")
            .field("type_id", &self.type_id)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

/// A listener's outcome — carries return-value chaining (return-as-new-event).
///
/// [`ListenerOutcome::Emit`] re-publishes the carried events at the next channel
/// boundary; the `#[event_listener]` macro hard-ERRORS if an async/spawn-dispatched
/// listener declares a chaining return (the "disabled on async" caveat is a
/// COMPILE error, not a runtime check).
#[derive(Debug)]
pub enum ListenerOutcome {
    /// The listener emitted nothing to chain.
    None,
    /// The listener returned one or more events to re-publish.
    Emit(SmallVec<[ErasedEvent; 1]>),
}

impl ListenerOutcome {
    /// Wrap a single chained event.
    #[must_use]
    pub fn emit_one(event: ErasedEvent) -> Self {
        let mut v = SmallVec::new();
        v.push(event);
        ListenerOutcome::Emit(v)
    }

    /// `true` iff the listener emitted nothing.
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, ListenerOutcome::None)
    }

    /// The chained events (empty if [`ListenerOutcome::None`]).
    #[must_use]
    pub fn emitted(&self) -> &[ErasedEvent] {
        match self {
            ListenerOutcome::None => &[],
            ListenerOutcome::Emit(v) => v,
        }
    }
}

// ─────────────────────────── listener authoring ─────────────────────────────

/// The classic interface listener — a [`Bean`].
///
/// `on` is async (boxed at the `dyn` boundary; AFIT not `dyn`-compatible) and
/// returns a [`ListenerOutcome`] for chaining. The preferred POJO
/// `#[event_listener]` form compiles to the SAME [`ListenerEntry`] shape.
pub trait ApplicationListener<E: 'static>: Bean {
    /// Handle one event of type `E`.
    ///
    /// # Errors
    /// A [`LeafError`] if the listener body fails (isolated or propagated per the
    /// active [`DispatchErrorMode`]).
    fn on<'a>(&'a self, e: &'a E) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>>;
}

/// The erased adapter fn the macro emits beside a listener method: downcast the
/// host `Arc` + the event `&(dyn Any + Send + Sync)`, invoke the typed body.
pub type ErasedAdapterFn = for<'a> fn(
    ErasedBean,
    &'a (dyn Any + Send + Sync),
) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>>;

/// A const predicate fn deciding whether an erased entry supports a runtime
/// `TypeId` (the cross-type / marker-trait / supertrait erased-fallback case).
pub type SupportsFn = fn(TypeId) -> bool;

/// The const row the thin `#[event_listener]` macro emits into the
/// `EVENT_LISTENERS` linkme slice (the COMPONENTS pipeline) — DATA only.
///
/// Note: the frozen discovery slice currently carries the minimal
/// `EventListenerRow { contract, order }`; this richer descriptor is the runtime
/// ABI the events layer binds at refresh (the macro emits both — the minimal row
/// for the anti-DCE self-check, this descriptor for the channel plan). They share
/// the `contract` + `order` identity. The two are reconciled when the events
/// codegen unit lands.
pub struct ListenerDescriptor {
    /// The bean hosting the listener method (bound to a `BeanId` at refresh).
    pub host: ContractId,
    /// The event type channel key (`TypeId::of::<E>()`).
    pub event_type: TypeId,
    /// `Some` => an erased fallback entry (cross-type / marker / supertrait).
    pub supports: Option<SupportsFn>,
    /// Dispatch order WITHIN the event type — the pure [`cmp_order`].
    pub order: OrderKey,
    /// `false` for async/spawn-dispatched listeners (the macro hard-errors if
    /// such a listener also declares a chaining return).
    pub chains: bool,
    /// The erased adapter (downcast host + event, invoke the typed body).
    pub adapter: ErasedAdapterFn,
}

impl std::fmt::Debug for ListenerDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerDescriptor")
            .field("host", &self.host)
            .field("event_type", &self.event_type)
            .field("order", &self.order)
            .field("chains", &self.chains)
            .finish_non_exhaustive()
    }
}

/// One resolved, dispatch-ready listener entry (both authoring models compile to
/// this): the live host handle + the adapter + order/chaining flags.
///
/// Frozen per channel at refresh; lock-free reads on the hot path.
pub struct ListenerEntry {
    /// The live host bean handle (resolved at refresh from the descriptor's host).
    pub host: ErasedBean,
    /// The erased adapter to invoke.
    pub adapter: ErasedAdapterFn,
    /// Dispatch order WITHIN the event type.
    pub order: OrderKey,
    /// Whether this listener's chaining return is honored (false for async).
    pub chains: bool,
}

impl ListenerEntry {
    /// Build an entry from a resolved host + a descriptor's adapter/flags.
    #[must_use]
    pub fn new(host: ErasedBean, adapter: ErasedAdapterFn, order: OrderKey, chains: bool) -> Self {
        ListenerEntry { host, adapter, order, chains }
    }

    /// Invoke this listener over the erased event payload.
    ///
    /// # Errors
    /// A [`LeafError`] from the listener body.
    pub fn dispatch<'a>(
        &'a self,
        payload: &'a (dyn Any + Send + Sync),
    ) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>> {
        (self.adapter)(Arc::clone(&self.host), payload)
    }
}

impl std::fmt::Debug for ListenerEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerEntry")
            .field("order", &self.order)
            .field("chains", &self.chains)
            .finish_non_exhaustive()
    }
}

/// The frozen, [`cmp_order`]-sorted listener sequence for one event type (typed +
/// matching erased entries merged ONCE at refresh).
///
/// A forward-only borrow handed to [`CoreDispatch`] / a [`DispatchInterceptor`]'s
/// [`ListenerNext`]. The hot publish path never sorts (the merge is precomputed).
pub struct ListenerSeq<'a> {
    entries: &'a [ListenerEntry],
}

impl<'a> ListenerSeq<'a> {
    /// Build a sequence over an already-`cmp_order`-sorted slice.
    #[must_use]
    pub fn new(entries: &'a [ListenerEntry]) -> Self {
        ListenerSeq { entries }
    }

    /// The number of listeners.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` iff there are no listeners (a cheap silent no-op publish).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The underlying entries (already `cmp_order`-sorted).
    #[must_use]
    pub fn entries(&self) -> &'a [ListenerEntry] {
        self.entries
    }
}

/// Sort listener entries by the ONE pure [`cmp_order`] (lower = earlier), with a
/// stable insertion-order fallback — the once-at-refresh merge step.
///
/// Listeners are NOT `RoleTier`-graded, so `cmp_order` (NOT `cmp_chain`) is the
/// correct comparator here, identical to the autowiring tie-break.
pub fn sort_listener_entries(entries: &mut [ListenerEntry]) {
    entries.sort_by(|a, b| cmp_order(&a.order, &b.order));
}

// ───────────────────────────── dispatch outcome ─────────────────────────────

/// The result of one multicaster dispatch.
#[derive(Debug)]
pub enum DispatchOutcome {
    /// Every listener completed; the collected chained events (for re-publish).
    Completed(SmallVec<[ErasedEvent; 1]>),
    /// A listener faulted under abort-on-first; the propagated error.
    Aborted(LeafError),
    /// The dispatch was handed to the upstream `Spawner` (opt-in async); no
    /// caller-visible outcome and chaining is forced off.
    Scheduled,
}

impl DispatchOutcome {
    /// `true` iff every listener completed.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        matches!(self, DispatchOutcome::Completed(_))
    }

    /// The chained events collected (empty unless [`DispatchOutcome::Completed`]).
    #[must_use]
    pub fn emitted(&self) -> &[ErasedEvent] {
        match self {
            DispatchOutcome::Completed(v) => v,
            _ => &[],
        }
    }
}

/// The per-dispatch error policy (one model, per-dispatch override).
#[derive(Debug, Clone, Default)]
pub enum DispatchErrorMode {
    /// Default for application events: abort on the first error and propagate it
    /// (Spring-faithful, fail-loud).
    #[default]
    AbortAndPropagate,
    /// Isolate each route: catch + route to a sink, continue the sequence.
    IsolateEach,
    /// Milestone facts: observe the outcome and route any error into refresh's
    /// cancel path (fail-startup).
    ObserveAndFailStartup,
}

// ──────────────────────── the dispatch pipeline (C5) ─────────────────────────

/// The innermost dispatch sink: the sequential inline-await loop over the
/// [`cmp_order`]-sorted [`ListenerSeq`], collecting [`ListenerOutcome::Emit`] for
/// re-publish.
///
/// With NO [`DispatchInterceptor`] present, `dispatch` awaits each listener
/// SEQUENTIALLY on the caller's task (the ambient `Cx` stays in scope) and honors
/// the [`DispatchErrorMode`].
#[derive(Debug, Default)]
pub struct CoreDispatch {
    /// The error policy this dispatch observes.
    pub mode: DispatchErrorMode,
}

impl CoreDispatch {
    /// A core dispatch with the given error mode.
    #[must_use]
    pub fn new(mode: DispatchErrorMode) -> Self {
        CoreDispatch { mode }
    }

    /// Run the inline-await loop over `seq` for `ev`.
    pub fn dispatch<'a>(
        &'a self,
        ev: &'a ErasedEvent,
        seq: ListenerSeq<'a>,
    ) -> BoxFuture<'a, DispatchOutcome> {
        Box::pin(async move {
            let mut collected: SmallVec<[ErasedEvent; 1]> = SmallVec::new();
            for entry in seq.entries() {
                match entry.dispatch(ev.payload()).await {
                    Ok(outcome) => {
                        if let (true, ListenerOutcome::Emit(events)) = (entry.chains, outcome) {
                            collected.extend(events);
                        }
                    }
                    Err(e) => match self.mode {
                        DispatchErrorMode::AbortAndPropagate
                        | DispatchErrorMode::ObserveAndFailStartup => {
                            return DispatchOutcome::Aborted(e);
                        }
                        // Isolate: drop the error (a real sink routes it); keep going.
                        DispatchErrorMode::IsolateEach => {}
                    },
                }
            }
            DispatchOutcome::Completed(collected)
        })
    }
}

/// The forward-only continuation over the remaining [`ListenerSeq`] + the
/// [`CoreDispatch`] sink — the events analogue of the proxy
/// [`Next`](crate::proxy::Next), but it fans OUT over N listeners rather than
/// wrapping one [`Call`](crate::proxy::Call).
pub struct ListenerNext<'a> {
    /// The remaining dispatch interceptors (outermost-first).
    remaining: &'a [Arc<dyn DispatchInterceptor>],
    /// The innermost inline-await sink.
    core: &'a CoreDispatch,
}

impl<'a> ListenerNext<'a> {
    /// Construct a continuation over `remaining` interceptors and the `core` sink.
    #[must_use]
    pub fn new(remaining: &'a [Arc<dyn DispatchInterceptor>], core: &'a CoreDispatch) -> Self {
        ListenerNext { remaining, core }
    }

    /// Advance: run the next dispatch interceptor, or hit [`CoreDispatch`].
    pub fn proceed(self, ev: &'a ErasedEvent, seq: ListenerSeq<'a>) -> BoxFuture<'a, DispatchOutcome> {
        match self.remaining.split_first() {
            Some((head, rest)) => {
                let next = ListenerNext { remaining: rest, core: self.core };
                head.intercept(ev, seq, next)
            }
            None => self.core.dispatch(ev, seq),
        }
    }

    /// `true` iff the next `proceed` hits [`CoreDispatch`] directly.
    #[must_use]
    pub fn is_innermost(&self) -> bool {
        self.remaining.is_empty()
    }
}

/// The NEW sibling around-advice trait that fans OUT over N listeners (C5).
///
/// STRUCTURALLY DISTINCT from the proxy [`Interceptor`](crate::proxy::Interceptor)
/// (which wraps ONE [`Call`](crate::proxy::Call)): it wraps the whole dispatch over
/// a [`ListenerSeq`]. Async-dispatch / error-isolation / context-propagation /
/// metrics are entries ordered by [`cmp_chain`](crate::cmp_chain). It SHARES with
/// AOP only `cmp_chain` + the [`RoleTier`](crate::RoleTier) grade.
pub trait DispatchInterceptor: Send + Sync {
    /// Wrap the dispatch: do work before/after/around `next.proceed(ev, seq)`.
    fn intercept<'a>(
        &'a self,
        ev: &'a ErasedEvent,
        seq: ListenerSeq<'a>,
        next: ListenerNext<'a>,
    ) -> BoxFuture<'a, DispatchOutcome>;

    /// The composite chain key this interceptor sorts by (`RoleTier`, order, id).
    ///
    /// Default: an `Infrastructure`-tier implicit key. Concrete dispatch concerns
    /// override with their pinned `*_ORDER` const + stable id.
    fn chain_key(&self) -> ChainKey {
        ChainKey {
            tier: RoleTier::Infrastructure,
            order: OrderKey::implicit(),
            id: ContractId(0),
        }
    }
}

/// The single replaceable dispatch SEAM (the magic-named
/// `applicationEventMulticaster`).
pub trait Multicaster: Send + Sync {
    /// Dispatch `ev` over the resolved (merged + `cmp_order`-sorted) `seq`.
    fn dispatch<'a>(
        &'a self,
        ev: &'a ErasedEvent,
        seq: ListenerSeq<'a>,
    ) -> BoxFuture<'a, DispatchOutcome>;
}

/// The DEFAULT multicaster: the [`DispatchInterceptor`] pipeline over a
/// [`CoreDispatch`] sink.
///
/// The chain is sorted by [`cmp_chain`](crate::cmp_chain) at construction
/// ([`PipelineMulticaster::new`]); "replace the whole multicaster" is the
/// degenerate case of swapping [`CoreDispatch`].
pub struct PipelineMulticaster {
    core: CoreDispatch,
    chain: Box<[Arc<dyn DispatchInterceptor>]>,
}

impl PipelineMulticaster {
    /// Build a multicaster over a `core` sink and a dispatch-interceptor `chain`.
    ///
    /// The chain is sorted by `cmp_chain` (outermost-first) here, so the pinned
    /// `ASYNC_DISPATCH < ERROR_ISOLATION < CONTEXT_PROP < METRICS` ordering holds
    /// regardless of link/registration order.
    #[must_use]
    pub fn new(core: CoreDispatch, mut chain: Vec<Arc<dyn DispatchInterceptor>>) -> Self {
        chain.sort_by(|a, b| cmp_chain(&a.chain_key(), &b.chain_key()));
        PipelineMulticaster { core, chain: chain.into_boxed_slice() }
    }

    /// A bare multicaster with just the inline `core` sink (no interceptors).
    #[must_use]
    pub fn bare(core: CoreDispatch) -> Self {
        PipelineMulticaster { core, chain: Box::new([]) }
    }

    /// The number of dispatch interceptors in the pipeline.
    #[must_use]
    pub fn chain_len(&self) -> usize {
        self.chain.len()
    }
}

impl Multicaster for PipelineMulticaster {
    fn dispatch<'a>(
        &'a self,
        ev: &'a ErasedEvent,
        seq: ListenerSeq<'a>,
    ) -> BoxFuture<'a, DispatchOutcome> {
        let next = ListenerNext::new(&self.chain, &self.core);
        next.proceed(ev, seq)
    }
}

impl std::fmt::Debug for PipelineMulticaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineMulticaster")
            .field("chain_len", &self.chain.len())
            .finish_non_exhaustive()
    }
}

// ─────────────────────────── built-in lifecycle facts ───────────────────────

/// Why a context closed (the [`Closed`] fact's reason).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CloseReason {
    /// A normal, explicit `shutdown()`.
    Normal,
    /// An external signal (SIGTERM/SIGINT) drove the close.
    Signal,
    /// A management endpoint requested the close.
    Management,
}

/// The container finished a refresh. CAN fire more than once (the `generation`
/// counter makes the multi-fire reality explicit and guardable).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Refreshed {
    /// The originating container.
    pub container: ContainerId,
    /// The refresh generation (re-refresh increments it).
    pub generation: u32,
}

/// The runtime-lifecycle `start_all()` completed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Started;

/// The runtime-lifecycle `stop_all()` completed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stopped;

/// The context closed cleanly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Closed {
    /// Why it closed.
    pub reason: CloseReason,
}

/// A bring-up step faulted; the cancel-cascade ran. Fired INSTEAD of
/// [`Refreshed`]/[`Closed`] (the cancel-vs-close fork is structural).
#[derive(Clone, Debug)]
pub struct StartupFailed {
    /// The refresh phase that faulted (e.g. `"R5"`).
    pub phase: &'static str,
    /// The fault.
    pub error: Arc<LeafError>,
}

/// One availability axis changed (the projector re-publishes each transition).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AvailabilityChanged {
    /// Which axis transitioned.
    pub kind: AvailabilityKind,
    /// The previous state.
    pub old: AvailabilityState,
    /// The new state.
    pub new: AvailabilityState,
    /// The component that flipped it.
    pub source: &'static str,
}

/// Which availability axis an [`AvailabilityChanged`] refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AvailabilityKind {
    /// The liveness axis.
    Liveness,
    /// The readiness axis.
    Readiness,
}

// ─────────────────────────── availability state ─────────────────────────────

/// The liveness axis (a k8s liveness probe reads this).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LivenessState {
    /// The application is internally consistent and live (the default).
    #[default]
    Correct,
    /// The application is broken and should be restarted.
    Broken,
}

/// The readiness axis (a k8s readiness probe reads this). Orthogonal to liveness
/// (readiness flips while `Running`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ReadinessState {
    /// Ready to accept traffic (the default once started).
    #[default]
    AcceptingTraffic,
    /// Refusing traffic (quiescing / not yet ready).
    RefusingTraffic,
}

/// A unified state value carried by [`AvailabilityChanged`] (either axis).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AvailabilityState {
    /// A liveness value.
    Liveness(LivenessState),
    /// A readiness value.
    Readiness(ReadinessState),
}

/// The retained reactive availability STATE — TWO orthogonal watch cells (NOT a
/// single composite machine), reusing the ONE watch primitive.
///
/// Reads are cold-path lock-free point reads (a probe handler); subscription is
/// reactive (woken on change). Writes are validated transitions that wake
/// subscribers. The cell is updated strictly BEFORE the projector publishes the
/// [`AvailabilityChanged`] event (the ordering contract that defuses state/event
/// skew).
#[derive(Clone)]
pub struct AvailabilityHandle {
    liveness: WatchSender<LivenessState>,
    readiness: WatchSender<ReadinessState>,
}

impl AvailabilityHandle {
    /// Build a fresh handle at the default `(Correct, AcceptingTraffic)`.
    #[must_use]
    pub fn new() -> Self {
        AvailabilityHandle::with_states(LivenessState::Correct, ReadinessState::AcceptingTraffic)
    }

    /// Build a handle at explicit initial states.
    #[must_use]
    pub fn with_states(liveness: LivenessState, readiness: ReadinessState) -> Self {
        AvailabilityHandle {
            liveness: watch_channel(liveness).0,
            readiness: watch_channel(readiness).0,
        }
    }

    /// The current liveness (lock-free point read).
    #[must_use]
    pub fn liveness(&self) -> LivenessState {
        self.liveness.borrow()
    }

    /// The current readiness (lock-free point read).
    #[must_use]
    pub fn readiness(&self) -> ReadinessState {
        self.readiness.borrow()
    }

    /// Flip readiness; wakes subscribers. The `source` names the flipping
    /// component (for the projected [`AvailabilityChanged`] event).
    pub fn set_readiness(&self, s: ReadinessState, _source: &'static str) {
        self.readiness.send(s);
    }

    /// Flip liveness; wakes subscribers.
    pub fn set_liveness(&self, s: LivenessState, _source: &'static str) {
        self.liveness.send(s);
    }

    /// Reactively subscribe to readiness transitions (async-context-model 5f).
    #[must_use]
    pub fn watch_readiness(&self) -> WatchReceiver<ReadinessState> {
        self.readiness.subscribe()
    }

    /// Reactively subscribe to liveness transitions.
    #[must_use]
    pub fn watch_liveness(&self) -> WatchReceiver<LivenessState> {
        self.liveness.subscribe()
    }
}

impl Default for AvailabilityHandle {
    fn default() -> Self {
        AvailabilityHandle::new()
    }
}

impl std::fmt::Debug for AvailabilityHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvailabilityHandle")
            .field("liveness", &self.liveness())
            .field("readiness", &self.readiness())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
    use std::sync::Mutex;

    fn origin() -> ContainerId {
        ContractId::of("test::Container")
    }

    // A test event type.
    #[derive(Debug)]
    struct OrderPlaced {
        amount: i64,
    }

    // A test host bean carrying a shared log.
    struct LogHost {
        log: Arc<Mutex<Vec<String>>>,
        name: &'static str,
    }

    // The adapter the macro would emit for `fn on(&self, e: &OrderPlaced)`.
    fn log_adapter<'a>(
        host: ErasedBean,
        event: &'a (dyn Any + Send + Sync),
    ) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>> {
        Box::pin(async move {
            let host = host.downcast::<LogHost>().expect("host is LogHost");
            let e = event.downcast_ref::<OrderPlaced>().expect("event is OrderPlaced");
            host.log.lock().unwrap().push(format!("{}:{}", host.name, e.amount));
            Ok(ListenerOutcome::None)
        })
    }

    fn entry(host: Arc<LogHost>, order: i32) -> ListenerEntry {
        ListenerEntry::new(
            host as ErasedBean,
            log_adapter,
            OrderKey { value: order, source: crate::order::OrderSource::Annotation },
            true,
        )
    }

    // ── listener dispatch ─────────────────────────────────────────────────────

    #[test]
    fn core_dispatch_runs_listeners_in_cmp_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        // Register them OUT of order; the sort must fix it.
        let mut entries = vec![
            entry(Arc::new(LogHost { log: Arc::clone(&log), name: "third" }), 300),
            entry(Arc::new(LogHost { log: Arc::clone(&log), name: "first" }), 100),
            entry(Arc::new(LogHost { log: Arc::clone(&log), name: "second" }), 200),
        ];
        sort_listener_entries(&mut entries);
        let seq = ListenerSeq::new(&entries);
        let ev = ErasedEvent::new(OrderPlaced { amount: 5 }, origin());
        let core = CoreDispatch::default();
        let outcome = futures::executor::block_on(core.dispatch(&ev, seq));
        assert!(outcome.is_completed());
        // Listeners fired in cmp_order (100, 200, 300).
        assert_eq!(
            *log.lock().unwrap(),
            vec!["first:5".to_string(), "second:5".to_string(), "third:5".to_string()]
        );
    }

    #[test]
    fn publish_to_zero_listeners_is_a_silent_completed_noop() {
        let entries: Vec<ListenerEntry> = Vec::new();
        let seq = ListenerSeq::new(&entries);
        assert!(seq.is_empty());
        let ev = ErasedEvent::new(OrderPlaced { amount: 1 }, origin());
        let core = CoreDispatch::default();
        let outcome = futures::executor::block_on(core.dispatch(&ev, seq));
        assert!(outcome.is_completed());
        assert!(outcome.emitted().is_empty());
    }

    // ── return-value-as-new-event chaining ────────────────────────────────────

    #[test]
    fn listener_return_value_chains_as_a_new_event() {
        // A chaining adapter: emits a NotificationSent event.
        #[derive(Debug)]
        struct NotificationSent;
        fn chaining_adapter<'a>(
            _host: ErasedBean,
            _event: &'a (dyn Any + Send + Sync),
        ) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>> {
            Box::pin(async move {
                Ok(ListenerOutcome::emit_one(ErasedEvent::new(
                    NotificationSent,
                    ContractId::of("test::Container"),
                )))
            })
        }
        let host: ErasedBean = Arc::new(LogHost { log: Arc::new(Mutex::new(Vec::new())), name: "n" });
        let entries =
            vec![ListenerEntry::new(host, chaining_adapter, OrderKey::implicit(), true)];
        let seq = ListenerSeq::new(&entries);
        let ev = ErasedEvent::new(OrderPlaced { amount: 9 }, origin());
        let core = CoreDispatch::default();
        let outcome = futures::executor::block_on(core.dispatch(&ev, seq));
        let emitted = outcome.emitted();
        assert_eq!(emitted.len(), 1, "the chained event is collected for re-publish");
        assert_eq!(emitted[0].type_id, TypeId::of::<NotificationSent>());
    }

    #[test]
    fn async_listener_chaining_return_is_dropped_when_chains_false() {
        #[derive(Debug)]
        struct Chained;
        fn chaining_adapter<'a>(
            _host: ErasedBean,
            _event: &'a (dyn Any + Send + Sync),
        ) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>> {
            Box::pin(async move {
                Ok(ListenerOutcome::emit_one(ErasedEvent::new(
                    Chained,
                    ContractId::of("test::Container"),
                )))
            })
        }
        let host: ErasedBean = Arc::new(LogHost { log: Arc::new(Mutex::new(Vec::new())), name: "n" });
        // chains = false (async/spawn-dispatched): the chaining return is ignored.
        let entries =
            vec![ListenerEntry::new(host, chaining_adapter, OrderKey::implicit(), false)];
        let seq = ListenerSeq::new(&entries);
        let ev = ErasedEvent::new(OrderPlaced { amount: 1 }, origin());
        let core = CoreDispatch::default();
        let outcome = futures::executor::block_on(core.dispatch(&ev, seq));
        assert!(outcome.emitted().is_empty(), "chains=false drops the chained event");
    }

    // ── error policy ──────────────────────────────────────────────────────────

    #[test]
    fn abort_and_propagate_stops_on_first_error() {
        fn failing_adapter<'a>(
            _host: ErasedBean,
            _event: &'a (dyn Any + Send + Sync),
        ) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>> {
            Box::pin(async move { Err(LeafError::new(crate::ErrorKind::ConstructionFailed)) })
        }
        let host: ErasedBean = Arc::new(LogHost { log: Arc::new(Mutex::new(Vec::new())), name: "n" });
        // First listener fails; under AbortAndPropagate the second (count_adapter,
        // which bumps the COUNT static) must NOT run.
        let entries = vec![
            ListenerEntry::new(Arc::clone(&host), failing_adapter, OrderKey { value: 1, source: crate::order::OrderSource::Annotation }, true),
            ListenerEntry::new(host, count_adapter, OrderKey { value: 2, source: crate::order::OrderSource::Annotation }, true),
        ];
        COUNT.store(0, AtomicOrdering::SeqCst);
        let seq = ListenerSeq::new(&entries);
        let ev = ErasedEvent::new(OrderPlaced { amount: 1 }, origin());
        let core = CoreDispatch::new(DispatchErrorMode::AbortAndPropagate);
        let outcome = futures::executor::block_on(core.dispatch(&ev, seq));
        assert!(matches!(outcome, DispatchOutcome::Aborted(_)));
        assert_eq!(COUNT.load(AtomicOrdering::SeqCst), 0, "second listener must not run");
    }

    static COUNT: AtomicU32 = AtomicU32::new(0);
    fn count_adapter<'a>(
        _host: ErasedBean,
        _event: &'a (dyn Any + Send + Sync),
    ) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>> {
        COUNT.fetch_add(1, AtomicOrdering::SeqCst);
        Box::pin(async move { Ok(ListenerOutcome::None) })
    }

    #[test]
    fn isolate_each_continues_past_a_failing_listener() {
        fn failing_adapter<'a>(
            _host: ErasedBean,
            _event: &'a (dyn Any + Send + Sync),
        ) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>> {
            Box::pin(async move { Err(LeafError::new(crate::ErrorKind::ConstructionFailed)) })
        }
        let host: ErasedBean = Arc::new(LogHost { log: Arc::new(Mutex::new(Vec::new())), name: "n" });
        let entries = vec![
            ListenerEntry::new(Arc::clone(&host), failing_adapter, OrderKey { value: 1, source: crate::order::OrderSource::Annotation }, true),
            ListenerEntry::new(host, count_adapter, OrderKey { value: 2, source: crate::order::OrderSource::Annotation }, true),
        ];
        COUNT.store(0, AtomicOrdering::SeqCst);
        let seq = ListenerSeq::new(&entries);
        let ev = ErasedEvent::new(OrderPlaced { amount: 1 }, origin());
        let core = CoreDispatch::new(DispatchErrorMode::IsolateEach);
        let outcome = futures::executor::block_on(core.dispatch(&ev, seq));
        assert!(outcome.is_completed(), "isolate completes despite the failure");
        assert_eq!(COUNT.load(AtomicOrdering::SeqCst), 1, "second listener still runs");
    }

    // ── DispatchInterceptor fan-out ordering by cmp_chain ─────────────────────

    struct OrderRecorder {
        name: &'static str,
        order: i32,
        log: Arc<Mutex<Vec<String>>>,
    }
    impl DispatchInterceptor for OrderRecorder {
        fn intercept<'a>(
            &'a self,
            ev: &'a ErasedEvent,
            seq: ListenerSeq<'a>,
            next: ListenerNext<'a>,
        ) -> BoxFuture<'a, DispatchOutcome> {
            Box::pin(async move {
                self.log.lock().unwrap().push(format!("{}:enter", self.name));
                let r = next.proceed(ev, seq).await;
                self.log.lock().unwrap().push(format!("{}:exit", self.name));
                r
            })
        }
        fn chain_key(&self) -> ChainKey {
            ChainKey {
                tier: RoleTier::Infrastructure,
                order: OrderKey { value: self.order, source: crate::order::OrderSource::Annotation },
                id: ContractId::of(self.name),
            }
        }
    }

    #[test]
    fn dispatch_interceptors_run_outermost_first_by_cmp_chain() {
        let log = Arc::new(Mutex::new(Vec::new()));
        // Provide them OUT of order; PipelineMulticaster::new must sort by cmp_chain.
        let chain: Vec<Arc<dyn DispatchInterceptor>> = vec![
            Arc::new(OrderRecorder {
                name: "metrics",
                order: crate::METRICS_ORDER,
                log: Arc::clone(&log),
            }),
            Arc::new(OrderRecorder {
                name: "async_dispatch",
                order: crate::ASYNC_DISPATCH_ORDER,
                log: Arc::clone(&log),
            }),
            Arc::new(OrderRecorder {
                name: "error_isolation",
                order: crate::ERROR_ISOLATION_ORDER,
                log: Arc::clone(&log),
            }),
        ];
        let mc = PipelineMulticaster::new(CoreDispatch::default(), chain);
        assert_eq!(mc.chain_len(), 3);
        let entries: Vec<ListenerEntry> = Vec::new();
        let seq = ListenerSeq::new(&entries);
        let ev = ErasedEvent::new(OrderPlaced { amount: 0 }, origin());
        let _ = futures::executor::block_on(mc.dispatch(&ev, seq));
        // Outermost = lowest order (async_dispatch=100) enters first, exits last.
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "async_dispatch:enter",
                "error_isolation:enter",
                "metrics:enter",
                "metrics:exit",
                "error_isolation:exit",
                "async_dispatch:exit",
            ]
        );
    }

    // ── availability over watch RunState ──────────────────────────────────────

    #[test]
    fn availability_handle_point_reads_and_flips() {
        let h = AvailabilityHandle::new();
        assert_eq!(h.liveness(), LivenessState::Correct);
        assert_eq!(h.readiness(), ReadinessState::AcceptingTraffic);
        h.set_readiness(ReadinessState::RefusingTraffic, "test");
        assert_eq!(h.readiness(), ReadinessState::RefusingTraffic);
        h.set_liveness(LivenessState::Broken, "test");
        assert_eq!(h.liveness(), LivenessState::Broken);
    }

    #[test]
    fn availability_watch_wakes_on_change() {
        let h = AvailabilityHandle::new();
        let mut rx = h.watch_readiness();
        assert_eq!(rx.borrow(), ReadinessState::AcceptingTraffic);
        h.set_readiness(ReadinessState::RefusingTraffic, "quiesce");
        // The reactive subscription observes the new value (changed() resolves to it).
        let new_value = futures::executor::block_on(rx.changed());
        assert_eq!(new_value, ReadinessState::RefusingTraffic);
        assert_eq!(rx.borrow(), ReadinessState::RefusingTraffic);
    }

    #[test]
    fn lifecycle_facts_are_plain_typed_events() {
        let r = Refreshed { container: origin(), generation: 1 };
        assert_eq!(r.generation, 1);
        let c = Closed { reason: CloseReason::Normal };
        assert_eq!(c.reason, CloseReason::Normal);
        // StartupFailed carries an Arc<LeafError> and is fired INSTEAD of Refreshed.
        let f = StartupFailed {
            phase: "R5",
            error: Arc::new(LeafError::new(crate::ErrorKind::ConstructionFailed)),
        };
        assert_eq!(f.phase, "R5");
    }
}
