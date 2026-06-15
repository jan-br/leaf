//! The R3 event-publisher install (events phase3/12): build the live
//! [`Multicaster`] over the [`DispatchInterceptor`] chain, bind each macro-emitted
//! [`ListenerDescriptor`] to its live host bean + per-event-type
//! [`ListenerEntry`], and freeze the `TypeId`-keyed channel table the live
//! [`EventPublisher`] dispatches through.
//!
//! This RESOLVES the cross-crate events NOTE the macros left
//! (`leaf-codegen/src/listener.rs`: the `EVENT_LISTENERS` identity row + the
//! `__leaf_listener_<Ident>` dispatch-metadata pairing const are JOINed here into
//! a live [`ListenerDescriptor`], whose `host`/`event_type`/`adapter`/`condition`
//! are resolved at refresh against the live registry).

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use leaf_core::{
    sort_listener_entries, BeanKey, CoreDispatch, DispatchErrorMode, DispatchInterceptor,
    DispatchOutcome, Engine, ErasedEvent, LeafError, ListenerDescriptor, ListenerEntry, ListenerSeq,
    Multicaster, PipelineMulticaster,
};

// ─────────────────────────────── EventPublisher ─────────────────────────────

/// The always-present structural event-publisher service the R3 refresh installs
/// onto the live context (the events subsystem's `applicationEventMulticaster` +
/// the frozen per-event-type [`ListenerSeq`] channels).
///
/// Built once at refresh ([`EventPublisher::install`]): the macro-emitted
/// [`ListenerDescriptor`]s are bound to live host beans + sorted into
/// `cmp_order` channels, and the [`DispatchInterceptor`] chain is composed into a
/// [`PipelineMulticaster`]. The hot [`EventPublisher::publish`] path never sorts.
pub struct EventPublisher {
    /// The dispatch SEAM (the `DispatchInterceptor` pipeline over a `CoreDispatch`).
    multicaster: Box<dyn Multicaster>,
    /// The frozen, `cmp_order`-sorted listener entries per event-type channel.
    channels: HashMap<TypeId, Vec<ListenerEntry>>,
    /// The container identity (the event origin).
    origin: leaf_core::ContainerId,
}

impl EventPublisher {
    /// Install the event publisher at refresh R3: bind each [`ListenerDescriptor`]
    /// to its live host bean (resolved by `host` [`ContractId`](leaf_core::ContractId)
    /// against the live registry), build a per-event-type channel of
    /// `cmp_order`-sorted [`ListenerEntry`]s, and compose the
    /// [`DispatchInterceptor`] `chain` into a [`PipelineMulticaster`].
    ///
    /// A listener whose host bean has not been published (e.g. a lazy host) is
    /// bound against the live singleton store; an unresolvable host is a loud
    /// [`LeafError`] (a silently-absent listener is the asymmetric DCE hazard the
    /// events subsystem guards against).
    ///
    /// # Errors
    /// A [`LeafError`] if a listener's host bean cannot be resolved.
    pub async fn install(
        engine: &Engine,
        listeners: &[ListenerDescriptor],
        chain: Vec<Arc<dyn DispatchInterceptor>>,
        mode: DispatchErrorMode,
        origin: leaf_core::ContainerId,
    ) -> Result<EventPublisher, LeafError> {
        let mut channels: HashMap<TypeId, Vec<ListenerEntry>> = HashMap::new();
        for d in listeners {
            // Bind the host ContractId to the live host bean handle.
            let host = engine.get_erased(BeanKey::ByContract(d.host)).await.map_err(|e| {
                missing_host(d.host, e)
            })?;
            let entry = ListenerEntry::new(host, d.adapter, d.order, d.chains)
                .with_condition(d.condition);
            channels.entry(d.event_type).or_default().push(entry);
        }
        // Freeze each channel: cmp_order-sort once (the hot path never sorts).
        for entries in channels.values_mut() {
            sort_listener_entries(entries);
        }

        let multicaster: Box<dyn Multicaster> = if chain.is_empty() {
            Box::new(PipelineMulticaster::bare(CoreDispatch::new(mode)))
        } else {
            Box::new(PipelineMulticaster::new(CoreDispatch::new(mode), chain))
        };

        Ok(EventPublisher { multicaster, channels, origin })
    }

    /// A bare publisher with no listeners + an inline `IsolateEach` dispatch (the
    /// empty-graph parity case + the early-event buffer drain target).
    #[must_use]
    pub fn bare(origin: leaf_core::ContainerId) -> Self {
        EventPublisher {
            multicaster: Box::new(PipelineMulticaster::bare(CoreDispatch::new(
                DispatchErrorMode::IsolateEach,
            ))),
            channels: HashMap::new(),
            origin,
        }
    }

    /// Publish a typed event over its channel (resolving the channel by
    /// `TypeId::of::<E>()`), returning the [`DispatchOutcome`]. A type with no
    /// listeners is a silent completed no-op.
    pub async fn publish<E: Any + Send + Sync>(&self, event: E) -> DispatchOutcome {
        let ev = ErasedEvent::new(event, self.origin);
        self.dispatch(&ev).await
    }

    /// Publish an already-erased event (the chaining / lifecycle-fact path).
    pub async fn publish_erased(&self, ev: &ErasedEvent) -> DispatchOutcome {
        self.dispatch(ev).await
    }

    async fn dispatch(&self, ev: &ErasedEvent) -> DispatchOutcome {
        match self.channels.get(&ev.type_id) {
            Some(entries) => {
                let seq = ListenerSeq::new(entries);
                self.multicaster.dispatch(ev, seq).await
            }
            None => {
                let empty: [ListenerEntry; 0] = [];
                let seq = ListenerSeq::new(&empty);
                self.multicaster.dispatch(ev, seq).await
            }
        }
    }

    /// The number of distinct event-type channels with at least one listener.
    #[must_use]
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// The number of listeners bound to the channel for `E` (test/diagnostics).
    #[must_use]
    pub fn listener_count<E: Any>(&self) -> usize {
        self.channels.get(&TypeId::of::<E>()).map_or(0, Vec::len)
    }
}

impl std::fmt::Debug for EventPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventPublisher")
            .field("channels", &self.channels.len())
            .finish_non_exhaustive()
    }
}

fn missing_host(host: leaf_core::ContractId, cause: LeafError) -> LeafError {
    LeafError::new(leaf_core::ErrorKind::NoSuchBean).caused_by(leaf_core::Cause::plain(
        "refresh R3: binding an event listener to its host bean",
        format!(
            "an `@EventListener`'s host bean {host:?} could not be resolved \
             (a silently-absent listener is a correctness hazard — force-link its crate): {cause}"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    use leaf_core::{
        AnnotationMetadata, BoxFuture, ContractId, Descriptor, ErasedBean, ListenerOutcome, OrderKey,
        Origin, Provider, Published, RegistryBuilder, ResolveCtx, Role, ScopeDef,
    };

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    // The event + the host bean carrying a running total.
    #[derive(Debug)]
    struct OrderPlaced {
        amount: i64,
    }
    #[derive(Debug)]
    struct Totaller {
        total: AtomicI64,
    }
    impl leaf_core::Bean for Totaller {}

    struct TotallerProv(Descriptor);
    impl Provider for TotallerProv {
        fn descriptor(&self) -> &Descriptor {
            &self.0
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(Totaller { total: AtomicI64::new(0) })) })
        }
    }

    // The adapter the macro would emit for `fn on(&self, e: &OrderPlaced)`.
    fn total_adapter<'a>(
        host: ErasedBean,
        event: &'a (dyn std::any::Any + Send + Sync),
    ) -> BoxFuture<'a, Result<ListenerOutcome, LeafError>> {
        Box::pin(async move {
            let host = host.downcast::<Totaller>().expect("host is Totaller");
            let e = event.downcast_ref::<OrderPlaced>().expect("event is OrderPlaced");
            host.total.fetch_add(e.amount, Ordering::SeqCst);
            Ok(ListenerOutcome::None)
        })
    }

    fn totaller_desc() -> Descriptor {
        Descriptor {
            contract: ContractId::of("test::Totaller"),
            self_type: TypeId::of::<Totaller>(),
            provides: &[],
            declared_name: Some("totaller"),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("test") },
        }
    }

    #[test]
    fn install_binds_a_listener_to_its_host_and_publish_dispatches_it() {
        let mut builder = RegistryBuilder::new();
        let d = totaller_desc();
        builder.register(d, Arc::new(TotallerProv(d))).unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        // Publish the host bean (refresh R5 happens before the R3 listener bind).
        let host = block(engine.get::<Totaller>()).unwrap();

        let listener = ListenerDescriptor {
            host: ContractId::of("test::Totaller"),
            event_type: TypeId::of::<OrderPlaced>(),
            supports: None,
            order: OrderKey::implicit(),
            condition: None,
            chains: true,
            adapter: total_adapter,
        };

        let publisher = block(EventPublisher::install(
            &engine,
            &[listener],
            Vec::new(),
            DispatchErrorMode::AbortAndPropagate,
            ContractId::of("test::Container"),
        ))
        .unwrap();
        assert_eq!(publisher.listener_count::<OrderPlaced>(), 1);

        // Publish → the bound listener fires over the live host bean.
        let outcome = block(publisher.publish(OrderPlaced { amount: 7 }));
        assert!(outcome.is_completed());
        assert_eq!(host.total.load(Ordering::SeqCst), 7, "the listener fired over the host");

        let _ = block(publisher.publish(OrderPlaced { amount: 5 }));
        assert_eq!(host.total.load(Ordering::SeqCst), 12, "a second publish accumulates");
    }

    #[test]
    fn publish_to_an_unknown_channel_is_a_silent_noop() {
        let engine = Engine::from_builder(RegistryBuilder::new()).unwrap();
        let publisher = block(EventPublisher::install(
            &engine,
            &[],
            Vec::new(),
            DispatchErrorMode::IsolateEach,
            ContractId::of("test::Container"),
        ))
        .unwrap();
        let outcome = block(publisher.publish(OrderPlaced { amount: 1 }));
        assert!(outcome.is_completed());
        assert_eq!(publisher.channel_count(), 0);
    }

    #[test]
    fn an_unresolvable_host_is_a_loud_error() {
        let engine = Engine::from_builder(RegistryBuilder::new()).unwrap();
        let listener = ListenerDescriptor {
            host: ContractId::of("test::Ghost"),
            event_type: TypeId::of::<OrderPlaced>(),
            supports: None,
            order: OrderKey::implicit(),
            condition: None,
            chains: false,
            adapter: total_adapter,
        };
        let err = block(EventPublisher::install(
            &engine,
            &[listener],
            Vec::new(),
            DispatchErrorMode::IsolateEach,
            ContractId::of("test::Container"),
        ))
        .expect_err("an absent host is loud, never a silently-dropped listener");
        assert_eq!(err.kind, leaf_core::ErrorKind::NoSuchBean);
    }
}
