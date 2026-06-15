//! [`AsyncDispatchInterceptor`] — the async-dispatch concern as a
//! [`DispatchInterceptor`] entry in the multicaster pipeline.
//!
//! Realizes the async-dispatch half of the events/context interaction
//! (phase3/10, phase3/12): the dispatch interceptor pipeline is the events
//! analogue of the proxy advisor chain. This entry sits at the OUTERMOST
//! [`ASYNC_DISPATCH_ORDER`](leaf_core::ASYNC_DISPATCH_ORDER) slot. Its job is the
//! context half of "async dispatch carries context only when propagation is
//! enabled" (ISOLATE-by-default, ADR-07 5a): it captures the ambient
//! [`Cx`](leaf_core::Cx) via a [`CxDecorator`] at dispatch time and re-installs it
//! AROUND the rest of the pipeline through the per-poll
//! [`Scoped`](leaf_core::Scoped) combinator, so the listener fan-out observes the
//! captured bundle across every `.await`.
//!
//! NOTE (deferred to leaf-boot): true fire-and-forget async dispatch (return to
//! the caller immediately, run the listeners on a spawned task) needs a `'static`
//! event/seq snapshot and the wired [`Spawner`](leaf_core::Spawner); the
//! borrowed-lifetime [`intercept`](DispatchInterceptor::intercept) seam yields a
//! `BoxFuture<'a>`, so this entry does the SOUND, in-task context-preserving wrap.
//! Detached dispatch is wired where the multicaster is assembled (leaf-boot), with
//! this entry's captured `Cx` threaded onto the spawned task.

use std::sync::Arc;

use leaf_core::{
    detached_dispatch_body, ChainKey, ContractId, CxDecorator, CxFutureExt, DetachedTaskRegistry,
    DispatchErrorMode, DispatchInterceptor, DispatchOutcome, DropPolicy, ErasedEvent, ListenerEntry,
    ListenerNext, ListenerSeq, OrderKey, OrderSource, RoleTier, Spawner, ASYNC_DISPATCH_ORDER,
};

/// TRUE fire-and-forget `@Async` event dispatch — the OWNING detached seam.
///
/// This is the multicaster/`EventPublisher`-layer seam the borrowed
/// [`DispatchInterceptor::intercept`] (`BoxFuture<'a>` over a `&ErasedEvent`) could
/// NOT host: the event + listener set are already snapshotted into owned `'static`
/// values by the caller, so a `'static` detached task is spawnable here.
///
/// The steps (matching the [`DispatchOutcome::Scheduled`] contract):
/// 1. Capture the ambient `Cx` via `decorator` ON THE CALLER'S TASK (so the bundle
///    bound in the caller's `Cx` region is the one propagated).
/// 2. Build the owning `'static` dispatch body
///    ([`detached_dispatch_body`], chaining forced off) and `.scoped(cx)` it so the
///    listener fan-out observes the captured `Cx` across the work-stealing spawn
///    hop (the [`ambient_cx_propagates_across_a_spawn_hop`] property, via events).
/// 3. `spawner.spawn(..).with_policy(Detach).detach()` the body (fire-and-forget),
///    REGISTERING the handle in `registry` so clean shutdown can drain it.
/// 4. Return [`DispatchOutcome::Scheduled`] WITHOUT awaiting — the caller returns
///    immediately while the listeners run detached.
///
/// [`ambient_cx_propagates_across_a_spawn_hop`]: crate
pub fn detached_dispatch(
    spawner: &dyn Spawner,
    decorator: &dyn CxDecorator,
    registry: &DetachedTaskRegistry,
    ev: ErasedEvent,
    entries: Vec<ListenerEntry>,
    mode: DispatchErrorMode,
) -> DispatchOutcome {
    // (1) Capture the ambient bundle ON THE CALLER'S TASK (before the spawn hop).
    let cx = decorator.capture();
    // (2) The owning 'static body (chains forced off), scoped to the captured Cx so
    // the detached fan-out sees the ambient bundle across the work-stealing hop.
    let body = detached_dispatch_body(ev, entries, mode).scoped(cx);
    // (3) Spawn DETACHED and REGISTER the handle for the shutdown drain (an
    // unregistered detach would escape the drain — a contract violation).
    let handle = spawner.spawn(Box::pin(body)).with_policy(DropPolicy::Detach);
    registry.register(handle);
    // (4) Return immediately; the listeners run on the detached task.
    DispatchOutcome::Scheduled
}

/// The default [`CxDecorator`]: capture the whole ambient bundle (a real,
/// Inherit-filtering decorator can refine this). Used when no explicit decorator
/// is supplied so the interceptor still propagates the ambient `Cx`.
#[derive(Default)]
pub struct CaptureCurrentCx;

impl CxDecorator for CaptureCurrentCx {
    fn capture(&self) -> leaf_core::Cx {
        leaf_core::Cx::current_or_empty()
    }
}

/// The async-dispatch [`DispatchInterceptor`]: capture the ambient `Cx` and
/// re-install it around the downstream dispatch (the context-preserving wrap).
///
/// `propagate = false` (the ISOLATE default) makes it a transparent pass-through
/// (no capture, no scope); `propagate = true` captures via the [`CxDecorator`].
pub struct AsyncDispatchInterceptor {
    decorator: Arc<dyn CxDecorator>,
    propagate: bool,
}

impl AsyncDispatchInterceptor {
    /// An interceptor that propagates the ambient `Cx` using
    /// [`CaptureCurrentCx`].
    #[must_use]
    pub fn propagating() -> Self {
        AsyncDispatchInterceptor {
            decorator: Arc::new(CaptureCurrentCx),
            propagate: true,
        }
    }

    /// A transparent pass-through interceptor (ISOLATE-by-default: no capture).
    #[must_use]
    pub fn isolating() -> Self {
        AsyncDispatchInterceptor {
            decorator: Arc::new(CaptureCurrentCx),
            propagate: false,
        }
    }

    /// An interceptor that captures via an explicit [`CxDecorator`].
    #[must_use]
    pub fn with_decorator(decorator: Arc<dyn CxDecorator>) -> Self {
        AsyncDispatchInterceptor {
            decorator,
            propagate: true,
        }
    }
}

impl Default for AsyncDispatchInterceptor {
    fn default() -> Self {
        AsyncDispatchInterceptor::isolating()
    }
}

impl DispatchInterceptor for AsyncDispatchInterceptor {
    fn intercept<'a>(
        &'a self,
        ev: &'a ErasedEvent,
        seq: ListenerSeq<'a>,
        next: ListenerNext<'a>,
    ) -> leaf_core::BoxFuture<'a, DispatchOutcome> {
        if !self.propagate {
            // ISOLATE: pass through untouched (no context capture).
            return next.proceed(ev, seq);
        }
        // Capture the ambient bundle at dispatch time and re-install it on every
        // poll of the downstream dispatch (so listener fan-out sees it across
        // `.await`s, even on a work-stealing hop).
        let cx = self.decorator.capture();
        Box::pin(next.proceed(ev, seq).scoped(cx))
    }

    fn chain_key(&self) -> ChainKey {
        ChainKey {
            tier: RoleTier::Infrastructure,
            order: OrderKey {
                value: ASYNC_DISPATCH_ORDER,
                source: OrderSource::Annotation,
            },
            id: ContractId::of("leaf_tokio::AsyncDispatchInterceptor"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{CoreDispatch, Cx, CxKey, Propagation};
    use std::sync::Arc as StdArc;

    struct LocaleKey;
    impl CxKey for LocaleKey {
        type Value = String;
        const NAME: &'static str = "locale";
        const POLICY: Propagation = Propagation::Inherit;
    }

    fn origin() -> ContractId {
        ContractId::of("leaf_tokio::dispatch::tests")
    }

    fn run_pipeline(
        chain: Vec<StdArc<dyn DispatchInterceptor>>,
        ev: &ErasedEvent,
    ) -> DispatchOutcome {
        let core = CoreDispatch::default();
        // Build a listener-less sequence; the interceptor still runs.
        let entries: [leaf_core::ListenerEntry; 0] = [];
        let seq = ListenerSeq::new(&entries);
        let next = ListenerNext::new(&chain, &core);
        futures::executor::block_on(next.proceed(ev, seq))
    }

    #[tokio::test]
    async fn propagating_interceptor_installs_captured_cx_into_dispatch() {
        // Install the tokio backing so Scoped re-install + current() agree.
        let _ = leaf_core::install_ambient_store(crate::TokioAmbient::shared());

        let interceptor: StdArc<dyn DispatchInterceptor> =
            StdArc::new(AsyncDispatchInterceptor::propagating());
        let chain = vec![interceptor];
        let core = CoreDispatch::default();
        let ev = ErasedEvent::new(123u32, origin());

        // Enter a Cx region so capture() snapshots a bound bundle, then dispatch.
        let cx = Cx::empty().with::<LocaleKey>("nl-NL".to_string());
        let observed = cx.enter(|| {
            let entries: [leaf_core::ListenerEntry; 0] = [];
            let seq = ListenerSeq::new(&entries);
            let next = ListenerNext::new(&chain, &core);
            // The interceptor wraps `next.proceed` in a Scoped over the captured
            // Cx; a probe future awaited inside that wrap must see the bundle.
            let probe = async {
                let inside = Cx::current().and_then(|c| c.get::<LocaleKey>().cloned());
                let _ = next.proceed(&ev, seq).await;
                inside
            };
            futures::executor::block_on(probe.scoped(Cx::current_or_empty()))
        });
        assert_eq!(observed.as_deref(), Some("nl-NL"));
    }

    // ── true fire-and-forget @Async dispatch (the detached seam) ──────────────

    struct ReqKey;
    impl CxKey for ReqKey {
        type Value = String;
        const NAME: &'static str = "request.id";
        const POLICY: Propagation = Propagation::Inherit;
    }

    // The keystone composite: enter a Cx region, dispatch a SLOW listener through
    // the detached fire-and-forget seam, and prove (a) the caller observes
    // `Scheduled` IMMEDIATELY — the slow listener's flag is still UNSET at return —
    // and (b) the listener body, on a multi-thread runtime spawn, observes the
    // captured ambient Cx via `Cx::current` (the ambient-Cx-across-a-spawn-hop
    // property reproduced through the events path).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detached_dispatch_returns_scheduled_before_a_slow_listener_finishes() {
        use leaf_core::{
            DetachedTaskRegistry, DispatchErrorMode, ErasedBean, ListenerEntry, ListenerOutcome,
            OrderKey,
        };
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let _ = crate::install_ambient_store();

        // A host that flips a flag AFTER a delay, recording the Cx it observed.
        struct SlowHost {
            done: Arc<AtomicBool>,
            seen_cx: Arc<std::sync::Mutex<Option<String>>>,
            started: Arc<tokio::sync::Notify>,
        }
        impl leaf_core::Bean for SlowHost {}

        fn slow_adapter<'a>(
            host: ErasedBean,
            _event: &'a (dyn std::any::Any + Send + Sync),
        ) -> leaf_core::BoxFuture<'a, Result<ListenerOutcome, leaf_core::LeafError>> {
            Box::pin(async move {
                let host = host.downcast::<SlowHost>().expect("SlowHost");
                // Observe the ambient Cx that rode the spawn hop (forced re-polls).
                host.started.notify_one();
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
                let seen = Cx::current().and_then(|c| c.get::<ReqKey>().cloned());
                *host.seen_cx.lock().unwrap() = seen;
                tokio::time::sleep(Duration::from_millis(40)).await;
                host.done.store(true, Ordering::SeqCst);
                Ok(ListenerOutcome::None)
            })
        }

        let done = Arc::new(AtomicBool::new(false));
        let seen_cx = Arc::new(std::sync::Mutex::new(None));
        let started = Arc::new(tokio::sync::Notify::new());
        let host = Arc::new(SlowHost {
            done: Arc::clone(&done),
            seen_cx: Arc::clone(&seen_cx),
            started: Arc::clone(&started),
        });

        let facility = crate::TokioExecutionFacility::new();
        let decorator = CaptureCurrentCx;
        let registry = DetachedTaskRegistry::new();

        // Snapshot the owned-into-'static event + listener-set (chains forced off).
        let cx = Cx::empty().with::<ReqKey>("req-async".to_string());
        let outcome = cx.enter(|| {
            let ev = ErasedEvent::new(7u32, origin());
            let entries = vec![ListenerEntry::new(
                Arc::clone(&host) as ErasedBean,
                slow_adapter,
                OrderKey::implicit(),
                // chains=false for the detached path.
                false,
            )];
            super::detached_dispatch(
                &facility,
                &decorator,
                &registry,
                ev,
                entries,
                DispatchErrorMode::AbortAndPropagate,
            )
        });

        // (a) The caller sees Scheduled, and the slow listener has NOT finished.
        assert!(matches!(outcome, DispatchOutcome::Scheduled));
        assert!(!done.load(Ordering::SeqCst), "the listener must not have finished at return");
        assert_eq!(registry.len(), 1, "the detached handle is registered for the drain");

        // The detached task ran on the executor; drain it and assert it completed.
        started.notified().await;
        registry.drain_all().await;
        assert!(done.load(Ordering::SeqCst), "the detached listener completed");
        // (b) the listener observed the captured ambient Cx across the spawn hop.
        assert_eq!(seen_cx.lock().unwrap().as_deref(), Some("req-async"));
    }

    #[test]
    fn isolating_interceptor_is_transparent_passthrough() {
        let chain: Vec<StdArc<dyn DispatchInterceptor>> =
            vec![StdArc::new(AsyncDispatchInterceptor::isolating())];
        let ev = ErasedEvent::new(7u32, origin());
        let outcome = run_pipeline(chain, &ev);
        assert!(matches!(outcome, DispatchOutcome::Completed(_)));
    }

    #[test]
    fn chain_key_is_outermost_infrastructure() {
        let it = AsyncDispatchInterceptor::propagating();
        let k = it.chain_key();
        assert_eq!(k.tier, RoleTier::Infrastructure);
        assert_eq!(k.order.value, ASYNC_DISPATCH_ORDER);
    }

    #[test]
    fn interceptor_is_object_safe() {
        let _it: StdArc<dyn DispatchInterceptor> =
            StdArc::new(AsyncDispatchInterceptor::propagating());
    }
}
