//! The R4 auto-proxy `after_init` install (proxy-interception phase3/08 +
//! declarative-advice phase3/09): bind the macro-emitted [`AdvisorDescriptor`]
//! rows to live beans, resolve each advisor's [`Interceptor`] via its
//! [`MakeInterceptor`](leaf_core::MakeInterceptor) bean bridge, and freeze a
//! `BeanId`-keyed table of ready [`AdviceChain`]s.
//!
//! This RESOLVES the cross-crate proxy NOTE the macros left
//! (`leaf-codegen/src/advisor.rs`: the public `__leaf_advisor_<Ident>` order
//! pairing const + the `ADVISORS` identity row are JOINed here into a live
//! [`AdvisorDescriptor`], whose `&'static dyn Pointcut` + `MakeInterceptor` are
//! supplied by the binary because they are NOT const-constructible at macro time).
//!
//! ## The pairing JOIN
//!
//! The binary (`#[leaf::main]`) supplies one [`AdvisorPairing`] per advisor — the
//! analogue of [`SeedPairing`](crate::SeedPairing)/[`GuardPairing`](crate::GuardPairing)
//! from the lower units. Each pairs the macro-emitted identity (`ADVISORS` row's
//! `contract`, the `__leaf_advisor_<Ident>` `OrderKey`) with the runtime advice
//! shape (`role`, the `&'static dyn Pointcut`, the `MakeInterceptor`).
//!
//! ## The install
//!
//! [`InstalledProxies::install`] consumes the frozen [`ProxyPlan`] (computed at
//! `seal()` by [`ProxyPlan::freeze`](leaf_core::ProxyPlan::freeze) — the O(1)
//! `advisors_for` table) plus the advisor descriptors and, for each advised bean,
//! resolves its already-`cmp_chain`-sorted [`AdvisorRef`] chain into a live
//! [`AdviceChain`] (each `make_interceptor` resolving its aspect/advisor bean
//! through the engine). The result is the `BeanId`-keyed
//! [`InstalledProxies::chain_for`] table the call site routes through.

use std::collections::HashMap;
use std::sync::Arc;

use leaf_core::{
    AdviceChain, AdvisorDescriptor, BeanId, Cardinality, Container, ContractId, Engine, ErasedBean,
    Interceptor, LeafError, OrderKey, Pointcut, ProxyPlan, Published, Registry, Role, Strictness,
};

// ─────────────────────────────── AdvisorPairing ─────────────────────────────

/// The macro→runtime advisor JOIN row (the proxy analogue of
/// [`SeedPairing`](crate::SeedPairing)): pairs the macro-emitted advisor IDENTITY
/// (the `ADVISORS` row's `contract` + the `__leaf_advisor_<Ident>` `OrderKey`)
/// with the runtime advice shape the macro cannot emit as a const (the
/// `&'static dyn Pointcut` + the [`MakeInterceptor`](leaf_core::MakeInterceptor)
/// bean bridge).
///
/// The binary (`#[leaf::main]`) builds the table; [`InstalledProxies::install`]
/// reifies each into a live [`AdvisorDescriptor`].
pub struct AdvisorPairing {
    /// The advisor's stable identity (the `ADVISORS` row's `contract`).
    pub contract: ContractId,
    /// The chain order (the `__leaf_advisor_<Ident>` pairing const).
    pub order: OrderKey,
    /// Framework-vs-application provenance (the `RoleTier` source).
    pub role: Role,
    /// The typed-combinator pointcut predicate.
    pub pointcut: &'static dyn Pointcut,
    /// The bean bridge that resolves this advisor's interceptor at refresh.
    pub make_interceptor: leaf_core::MakeInterceptor,
}

impl AdvisorPairing {
    /// Build a pairing from the macro identity + the runtime advice shape.
    #[must_use]
    pub fn new(
        contract: ContractId,
        order: OrderKey,
        role: Role,
        pointcut: &'static dyn Pointcut,
        make_interceptor: leaf_core::MakeInterceptor,
    ) -> Self {
        AdvisorPairing { contract, order, role, pointcut, make_interceptor }
    }

    /// Reify into the live [`AdvisorDescriptor`] the proxy plan freezes over.
    #[must_use]
    pub fn into_descriptor(self) -> AdvisorDescriptor {
        AdvisorDescriptor {
            id: self.contract,
            order: self.order,
            role: self.role,
            pointcut: self.pointcut,
            make_interceptor: self.make_interceptor,
        }
    }
}

impl std::fmt::Debug for AdvisorPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdvisorPairing")
            .field("contract", &self.contract)
            .field("order", &self.order)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

// ───────────────────────── a Container over the Engine ───────────────────────

/// A thin [`Container`] adapter over a live [`Engine`], so a
/// [`MakeInterceptor`](leaf_core::MakeInterceptor) bean bridge can resolve its
/// aspect/advisor collaborator through the engine's `Selector`.
///
/// `make_interceptor` is `for<'a> fn(&'a dyn Container) -> …`; the engine is not
/// itself a `Container`, so this adapter bridges the seam without leaking the
/// engine type into the proxy ABI.
pub struct EngineContainer<'a> {
    engine: &'a Engine,
}

impl<'a> EngineContainer<'a> {
    /// Wrap a live engine as a `Container`.
    #[must_use]
    pub fn new(engine: &'a Engine) -> Self {
        EngineContainer { engine }
    }
}

impl Container for EngineContainer<'_> {
    fn resolve(
        &self,
        key: leaf_core::BeanKey,
        _strictness: Strictness,
        _cardinality: Cardinality,
    ) -> leaf_core::BoxFuture<'_, Result<Published, LeafError>> {
        Box::pin(async move {
            let bean = self.engine.get_erased(key).await?;
            Ok(Published::shared(bean))
        })
    }
}

// ─────────────────────────────── InstalledProxies ───────────────────────────

/// The frozen R4 `after_init` table: a `BeanId`-keyed map of live
/// [`AdviceChain`]s (one per advised bean), each built by resolving the bean's
/// `cmp_chain`-sorted advisor refs into live [`Interceptor`]s via their
/// [`MakeInterceptor`](leaf_core::MakeInterceptor) bridge.
///
/// A bean with no matching advisor mints no entry (it passes through UNWRAPPED).
#[derive(Default)]
pub struct InstalledProxies {
    by_bean: HashMap<BeanId, Arc<AdviceChain>>,
}

impl InstalledProxies {
    /// An empty install (the bare-engine parity case: no advised bean).
    #[must_use]
    pub fn empty() -> Self {
        InstalledProxies { by_bean: HashMap::new() }
    }

    /// Install the auto-proxy table (R4): for each advised bean in `plan`, resolve
    /// its `cmp_chain`-sorted advisor chain into a live [`AdviceChain`] by calling
    /// each advisor's [`MakeInterceptor`](leaf_core::MakeInterceptor) over a
    /// [`Container`] view of `engine`.
    ///
    /// `advisors` is the JOINed [`AdvisorDescriptor`] set (the proxy plan keyed by
    /// `ContractId`); only those referenced by an advised bean's chain are resolved.
    ///
    /// # Errors
    /// A [`LeafError`] if an advisor's `make_interceptor` fails to resolve its
    /// aspect bean (the proxy install hard-fails the refresh — an advised bean
    /// MUST be wrapped, never silently unwrapped).
    pub async fn install(
        engine: &Engine,
        plan: &ProxyPlan,
        advisors: &[AdvisorDescriptor],
    ) -> Result<InstalledProxies, LeafError> {
        let by_id: HashMap<ContractId, &AdvisorDescriptor> =
            advisors.iter().map(|a| (a.id, a)).collect();
        let container = EngineContainer::new(engine);
        let mut by_bean: HashMap<BeanId, Arc<AdviceChain>> = HashMap::new();

        for id in engine.registry().ids() {
            let refs = plan.advisors_for(id);
            if refs.is_empty() {
                continue;
            }
            // The advisor refs are already cmp_chain-sorted at ProxyPlan::freeze;
            // resolve each into its live Interceptor (outermost-first).
            let mut chain: Vec<Arc<dyn Interceptor>> = Vec::with_capacity(refs.len());
            for adv_ref in refs {
                let descriptor = by_id.get(&adv_ref.id).ok_or_else(|| unknown_advisor(adv_ref.id))?;
                let interceptor = (descriptor.make_interceptor)(&container)
                    .await
                    .map_err(leaf_core::AdviceError::AdvisorResolution)?;
                chain.push(interceptor);
            }
            by_bean.insert(id, Arc::new(AdviceChain::new(chain.into_boxed_slice())));
        }

        Ok(InstalledProxies { by_bean })
    }

    /// The O(1) `after_init` lookup: the live [`AdviceChain`] for `bean`, or `None`
    /// (un-advised → the call passes through UNWRAPPED).
    #[must_use]
    pub fn chain_for(&self, bean: BeanId) -> Option<&Arc<AdviceChain>> {
        self.by_bean.get(&bean)
    }

    /// `true` iff `bean` is advised (has an installed chain).
    #[must_use]
    pub fn is_advised(&self, bean: BeanId) -> bool {
        self.by_bean.contains_key(&bean)
    }

    /// The number of advised beans (installed chains).
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_bean.len()
    }

    /// `true` iff no bean is advised.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_bean.is_empty()
    }

    /// Resolve a [`FixedTarget`](leaf_core::FixedTarget) over the already-built
    /// singleton for `bean` (the R4 singleton-advised target the chain fires over).
    ///
    /// # Errors
    /// A [`LeafError`] if the singleton has not yet been published into its slot.
    pub fn fixed_target_for(registry: &Registry, bean: BeanId) -> Result<ErasedBean, LeafError> {
        registry.singleton_cell(bean).get().cloned().ok_or_else(|| {
            LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(
                leaf_core::Cause::plain(
                    "auto-proxy after_init install",
                    "the advised singleton has not been published into its slot yet \
                     (the auto-proxy install runs after eager wave-instantiation)",
                ),
            )
        })
    }
}

impl std::fmt::Debug for InstalledProxies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstalledProxies")
            .field("advised_beans", &self.by_bean.len())
            .finish()
    }
}

fn unknown_advisor(id: ContractId) -> LeafError {
    LeafError::new(leaf_core::ErrorKind::ConstructionFailed).caused_by(leaf_core::Cause::plain(
        "auto-proxy after_init install",
        format!(
            "the proxy plan references advisor {id:?} but no AdvisorDescriptor was JOINed for it \
             (a macro-emitted ADVISORS row with no matching AdvisorPairing in the binary)"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    use leaf_core::{
        AdviceError, AnnotationMetadata, Anything, BeanKey, BoxFuture, Call, Cardinality, Container,
        CreatorPolicy, Descriptor, ErasedArgs, ErasedRet, MethodKey, Next, OrderKey, Origin,
        Provider, ProxyPlan, RegistryBuilder, ResolveCtx, Role, ScopeDef, Strictness, Tail,
    };

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[derive(Debug)]
    struct Svc {
        base: i64,
    }
    impl leaf_core::Bean for Svc {}

    struct SvcProv(Descriptor);
    impl Provider for SvcProv {
        fn descriptor(&self) -> &Descriptor {
            &self.0
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(Svc { base: 100 })) })
        }
    }

    fn svc_desc() -> Descriptor {
        Descriptor {
            contract: ContractId::of("test::Svc"),
            self_type: TypeId::of::<Svc>(),
            provides: &[],
            declared_name: Some("svc"),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("test") },
        }
    }

    // A recording around-interceptor: pushes enter/exit into a shared log.
    static LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    struct Recorder;
    impl leaf_core::Interceptor for Recorder {
        fn intercept<'a>(
            &'a self,
            call: &'a Call<'a>,
            mut next: Next<'a>,
        ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
            Box::pin(async move {
                LOG.lock().unwrap().push("enter");
                let r = next.proceed(call).await;
                LOG.lock().unwrap().push("exit");
                r
            })
        }
    }

    // The make_interceptor bean bridge: resolves the aspect bean (here a trivial
    // Recorder, but it COULD resolve a collaborator through the container).
    static RESOLVES: AtomicU32 = AtomicU32::new(0);
    fn make_recorder() -> leaf_core::MakeInterceptor {
        |_c: &dyn Container| {
            Box::pin(async {
                RESOLVES.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(Recorder) as Arc<dyn Interceptor>)
            })
        }
    }

    static ANY: Anything = Anything;

    #[test]
    fn install_resolves_an_advised_beans_chain_and_a_call_routes_through_it() {
        LOG.lock().unwrap().clear();
        RESOLVES.store(0, Ordering::SeqCst);

        // A single advised bean.
        let mut builder = RegistryBuilder::new();
        let d = svc_desc();
        let id = builder.register(d, Arc::new(SvcProv(d))).unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        // Publish the singleton (the after_init install runs over published beans).
        block(engine.get::<Svc>()).unwrap();

        // The JOINed advisor (the macro emits the ADVISORS row + order const; the
        // binary supplies the pointcut + make_interceptor).
        let advisor = AdvisorPairing::new(
            ContractId::of("test::RecorderAdvisor"),
            OrderKey::implicit(),
            Role::Application,
            &ANY,
            make_recorder(),
        )
        .into_descriptor();

        // The ProxyPlan (computed at seal()) marks the bean as advised.
        let mut jps = std::collections::HashMap::new();
        let methods = vec![leaf_core::MethodJoinPoint {
            method: MethodKey::of("test::Svc::add"),
            arg_types: Default::default(),
            ret_type: TypeId::of::<i64>(),
        }];
        jps.insert(
            id,
            leaf_core::BeanJoinPoints {
                bean_type: TypeId::of::<Svc>(),
                markers: &AnnotationMetadata::EMPTY,
                methods: &methods,
            },
        );
        let plan = ProxyPlan::freeze(
            std::slice::from_ref(&advisor),
            engine.registry(),
            &CreatorPolicy::ALL,
            &jps,
        )
        .unwrap();

        // INSTALL (R4): resolve the chain.
        let installed = block(InstalledProxies::install(&engine, &plan, &[advisor])).unwrap();
        assert!(installed.is_advised(id), "the bean has an installed chain");
        assert_eq!(RESOLVES.load(Ordering::SeqCst), 1, "make_interceptor resolved once");

        // A call routes through the chain → the recorder logs enter/exit, and the
        // real method runs over the FixedTarget (the published singleton).
        let chain = installed.chain_for(id).unwrap();
        let target_bean = InstalledProxies::fixed_target_for(engine.registry(), id).unwrap();
        let source = leaf_core::FixedTarget::new(target_bean);
        let cx = ResolveCtx::for_engine(&engine);
        let call = Call::new(
            MethodKey::of("test::Svc::add"),
            BeanKey::ByType(TypeId::of::<Svc>()),
            ErasedArgs::pack(5_i64),
            &source,
            &cx,
        );
        let tail: Box<Tail> = Box::new(|call: &Call<'_>| {
            Box::pin(async move {
                let bean = call.source.get(call.cx).await.map_err(AdviceError::TargetResolution)?;
                let svc = bean
                    .downcast_ref::<Svc>()
                    .ok_or(AdviceError::DowncastMismatch { method: call.method })?;
                let add = *call.args.0.downcast_ref::<i64>().unwrap();
                Ok(ErasedRet::pack(svc.base + add))
            })
        });
        let out = block(chain.invoke(&call, &*tail)).unwrap();
        assert_eq!(out.unpack::<i64>().unwrap(), 105, "the real method ran (100 + 5)");
        assert_eq!(*LOG.lock().unwrap(), vec!["enter", "exit"], "the call routed through the chain");
    }

    #[test]
    fn an_unadvised_bean_has_no_chain() {
        let mut builder = RegistryBuilder::new();
        let d = svc_desc();
        let id = builder.register(d, Arc::new(SvcProv(d))).unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        let installed =
            block(InstalledProxies::install(&engine, &ProxyPlan::empty(), &[])).unwrap();
        assert!(!installed.is_advised(id));
        assert!(installed.chain_for(id).is_none());
        assert!(installed.is_empty());
    }

    #[test]
    fn engine_container_resolves_an_aspect_collaborator() {
        // make_interceptor CAN resolve a collaborator through the container — prove
        // EngineContainer bridges to the engine's resolve.
        let mut builder = RegistryBuilder::new();
        let d = svc_desc();
        builder.register(d, Arc::new(SvcProv(d))).unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        let container = EngineContainer::new(&engine);
        let published = block(container.resolve(
            BeanKey::ByType(TypeId::of::<Svc>()),
            Strictness::Strict,
            Cardinality::Single,
        ))
        .unwrap();
        let bean = published.into_shared().unwrap();
        assert_eq!(bean.downcast_ref::<Svc>().unwrap().base, 100);
    }
}
