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
    ChainKey, ContractId, CxDecorator, CxFutureExt, DispatchInterceptor, DispatchOutcome,
    ErasedEvent, ListenerNext, ListenerSeq, OrderKey, OrderSource, RoleTier, ASYNC_DISPATCH_ORDER,
};

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
