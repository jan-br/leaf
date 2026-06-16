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
    AdviceChain, AdviceError, AdvisorDescriptor, BeanId, BeanJoinPoints, BeanJoinPointsSpec,
    BeanKey, Call, Cardinality, Container, ContractId, Engine, ErasedArgs, ErasedBean, ErasedRet,
    FixedTarget, Interceptor, LeafError, MethodJoinPoint, MethodKey, MethodTable, OrderKey,
    Pointcut, ProxyPlan, Published, Registry, ResolveCtx, Role, Strictness, Tail,
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

// ─────────────────────────────── JoinPointPairing ───────────────────────────

/// The macro→runtime per-bean join-point JOIN row (the proxy analogue of
/// [`SeedPairing`](crate::SeedPairing)/[`AdvisorPairing`]): pairs an advisable bean's
/// IDENTITY (its `ContractId`) with the PUBLIC `::leaf_core::BeanJoinPointsSpec` const
/// (`__leaf_joinpoints_<Ident>`) the `#[advisable]`/`#[aspect]` macro emits beside the
/// bean's `Descriptor`.
///
/// leaf-core's frozen `Descriptor` carries no join-point view, so — exactly like the
/// `ProviderSeed`/`CondExpr`/advisor JOINs — the binary crate (`#[leaf::main]`)
/// supplies one row per advisable bean and the proxy-assembly pass
/// ([`build_join_points`]) JOINs each to its frozen `BeanId` by `contract` and
/// reifies the const spec into the runtime [`BeanJoinPoints`]
/// [`ProxyPlan::freeze`](leaf_core::ProxyPlan::freeze) runs every admitted advisor's
/// pointcut over — so the proxy plan is built from REAL macro-emitted per-bean data,
/// never a hand-mirrored view.
#[derive(Clone, Copy)]
pub struct JoinPointPairing {
    /// The advisable bean's stable identity (the JOIN key against the frozen registry).
    pub contract: ContractId,
    /// The macro-emitted const per-bean join-point spec (bean_type + markers + methods).
    pub spec: &'static BeanJoinPointsSpec,
}

impl JoinPointPairing {
    /// Build a join-point pairing from an advisable bean's identity + its macro-emitted
    /// const [`BeanJoinPointsSpec`].
    #[must_use]
    pub fn new(contract: ContractId, spec: &'static BeanJoinPointsSpec) -> Self {
        JoinPointPairing { contract, spec }
    }
}

impl std::fmt::Debug for JoinPointPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinPointPairing")
            .field("contract", &self.contract)
            .finish_non_exhaustive()
    }
}

/// The owned reification of one bean's join points (the `SmallVec`-built
/// [`MethodJoinPoint`]s borrowed by a [`BeanJoinPoints`] view). Kept alive across the
/// [`ProxyPlan::freeze`](leaf_core::ProxyPlan::freeze) call (the view borrows it).
pub struct ReifiedJoinPoints {
    /// The bean's frozen slot id.
    id: BeanId,
    /// The bean's concrete `TypeId`.
    bean_type: std::any::TypeId,
    /// The bean's flat annotation metadata.
    markers: &'static leaf_core::AnnotationMetadata,
    /// The reified runtime method join points (the `SmallVec`s the view borrows).
    methods: Vec<MethodJoinPoint>,
}

impl ReifiedJoinPoints {
    /// Borrow this reification as a leaf-core [`BeanJoinPoints`] view (for
    /// [`ProxyPlan::freeze`](leaf_core::ProxyPlan::freeze)).
    #[must_use]
    pub fn view(&self) -> BeanJoinPoints<'_> {
        BeanJoinPoints {
            bean_type: self.bean_type,
            markers: self.markers,
            methods: &self.methods,
        }
    }

    /// The bean's frozen slot id (the `HashMap<BeanId, BeanJoinPoints>` key).
    #[must_use]
    pub fn id(&self) -> BeanId {
        self.id
    }
}

/// JOIN the macro-emitted [`JoinPointPairing`]s to the frozen registry: for each
/// pairing whose `contract` is a registered bean, reify its const
/// [`BeanJoinPointsSpec`] into an owned [`ReifiedJoinPoints`] (building each method's
/// `SmallVec`). A pairing whose contract is not registered (a bean gated off by a
/// condition / never registered) is silently skipped (not a fault — it has no slot to
/// advise).
///
/// The returned `Vec` OWNS the reified method join points; the caller borrows each
/// into a `HashMap<BeanId, BeanJoinPoints>` (the [`ProxyPlan::freeze`](leaf_core::ProxyPlan::freeze)
/// input) via [`ReifiedJoinPoints::view`], so the owned `Vec` must outlive the freeze.
#[must_use]
pub fn build_join_points(
    pairings: &[JoinPointPairing],
    registry: &Registry,
) -> Vec<ReifiedJoinPoints> {
    pairings
        .iter()
        .filter_map(|p| {
            let id = registry.by_contract(p.contract)?;
            Some(ReifiedJoinPoints {
                id,
                bean_type: p.spec.bean_type,
                markers: p.spec.markers,
                methods: p.spec.reify_methods(),
            })
        })
        .collect()
}

// ─────────────────────────────── MethodTablePairing ─────────────────────────

/// The macro→runtime per-bean METHOD-TABLE JOIN row (the proxy analogue of
/// [`SeedPairing`](crate::SeedPairing)/[`JoinPointPairing`]): pairs an advised bean's
/// IDENTITY (its `ContractId`) with the PUBLIC `&'static ::leaf_core::MethodTable`
/// (`__leaf_methods_<Ident>`) the `#[advisable]`/`#[aspect]` macro emits beside the
/// bean's `Descriptor`.
///
/// The [`MethodTable`] carries the per-method downcast-and-invoke thunks
/// ([`MethodEntry`](leaf_core::MethodEntry)) that drive the REAL method over the
/// resolved [`ErasedBean`] + the [`ErasedArgs`]. It is what makes the auto-installed
/// proxy TRANSPARENT: [`InstalledProxies::invoke`] routes a call by [`MethodKey`]
/// through the bean's `cmp_chain`-sorted [`AdviceChain`] and terminates in the
/// matching `MethodEntry.invoke` thunk — so a `#[advisable]` bean is advised with NO
/// hand-written `Call`/`Tail`/`FixedTarget` in user code.
///
/// As with the `__leaf_joinpoints_<Ident>` JOIN, the binary crate (`#[leaf::main]`)
/// supplies one row per advised bean and [`InstalledProxies::install`] JOINs each to
/// its frozen `BeanId` by `contract`.
#[derive(Clone, Copy)]
pub struct MethodTablePairing {
    /// The advised bean's stable identity (the JOIN key against the frozen registry).
    pub contract: ContractId,
    /// The macro-emitted const per-bean method table (the downcast invoke thunks).
    pub table: &'static MethodTable,
}

impl MethodTablePairing {
    /// Build a method-table pairing from an advised bean's identity + its
    /// macro-emitted const [`MethodTable`].
    #[must_use]
    pub fn new(contract: ContractId, table: &'static MethodTable) -> Self {
        MethodTablePairing { contract, table }
    }
}

impl std::fmt::Debug for MethodTablePairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MethodTablePairing")
            .field("contract", &self.contract)
            .field("methods", &self.table.len())
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

#[leaf_macros::async_impl]
impl Container for EngineContainer<'_> {
    async fn resolve(
        &self,
        key: leaf_core::BeanKey,
        _strictness: Strictness,
        _cardinality: Cardinality,
    ) -> Result<Published, LeafError> {
        let bean = self.engine.get_erased(key).await?;
        Ok(Published::shared(bean))
    }
}

// ─────────────────────────────── InstalledProxies ───────────────────────────

/// The frozen R4 `after_init` table: a `BeanId`-keyed map of live
/// [`AdviceChain`]s (one per advised bean), each built by resolving the bean's
/// `cmp_chain`-sorted advisor refs into live [`Interceptor`]s via their
/// [`MakeInterceptor`](leaf_core::MakeInterceptor) bridge, PLUS the per-bean
/// [`MethodTable`] that makes the install TRANSPARENT (so an advised call routes
/// through the chain by [`MethodKey`] via [`InstalledProxies::invoke`] with no
/// hand-written `Call`/`Tail`).
///
/// A bean with no matching advisor mints no entry (it passes through UNWRAPPED).
#[derive(Default)]
pub struct InstalledProxies {
    by_bean: HashMap<BeanId, Arc<AdviceChain>>,
    /// The macro-emitted per-bean method tables (the downcast invoke thunks), JOINed
    /// by `BeanId` from the [`MethodTablePairing`] rows — the transparent-invoke seam.
    tables: HashMap<BeanId, &'static MethodTable>,
}

impl InstalledProxies {
    /// An empty install (the bare-engine parity case: no advised bean).
    #[must_use]
    pub fn empty() -> Self {
        InstalledProxies { by_bean: HashMap::new(), tables: HashMap::new() }
    }

    /// Install the auto-proxy table (R4): for each advised bean in `plan`, resolve
    /// its `cmp_chain`-sorted advisor chain into a live [`AdviceChain`] by calling
    /// each advisor's [`MakeInterceptor`](leaf_core::MakeInterceptor) over a
    /// [`Container`] view of `engine`.
    ///
    /// `advisors` is the JOINed [`AdvisorDescriptor`] set (the proxy plan keyed by
    /// `ContractId`); only those referenced by an advised bean's chain are resolved.
    ///
    /// No method tables are JOINed by this entry point (the chain is built but the
    /// transparent-invoke seam is inert); use [`install_with_tables`](InstalledProxies::install_with_tables)
    /// to thread the macro-emitted [`MethodTablePairing`]s.
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
        Self::install_with_tables(engine, plan, advisors, &[]).await
    }

    /// Install the auto-proxy table (R4) AND JOIN the macro-emitted per-bean
    /// [`MethodTablePairing`]s by `ContractId` — so [`InstalledProxies::invoke`]
    /// routes a call by [`MethodKey`] through the auto-installed chain TRANSPARENTLY.
    ///
    /// `method_tables` is the JOINed per-bean method-table set (one row per advised
    /// bean whose methods are transparently invocable); a bean with a chain but no
    /// table can still be inspected (`is_advised`) but not transparently invoked.
    ///
    /// # Errors
    /// As [`install`](InstalledProxies::install).
    pub async fn install_with_tables(
        engine: &Engine,
        plan: &ProxyPlan,
        advisors: &[AdvisorDescriptor],
        method_tables: &[MethodTablePairing],
    ) -> Result<InstalledProxies, LeafError> {
        let by_id: HashMap<ContractId, &AdvisorDescriptor> =
            advisors.iter().map(|a| (a.id, a)).collect();
        let container = EngineContainer::new(engine);
        let registry = engine.registry();
        let mut by_bean: HashMap<BeanId, Arc<AdviceChain>> = HashMap::new();

        for id in registry.ids() {
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

        // JOIN the per-bean method tables to their frozen BeanIds (by ContractId). A
        // pairing whose contract is unregistered is silently skipped (no slot).
        let tables: HashMap<BeanId, &'static MethodTable> = method_tables
            .iter()
            .filter_map(|p| registry.by_contract(p.contract).map(|id| (id, p.table)))
            .collect();

        Ok(InstalledProxies { by_bean, tables })
    }

    /// TRANSPARENTLY invoke an advised method (R4 after_init routing): route a call
    /// to `method` on the advised singleton `bean` through its auto-installed
    /// [`AdviceChain`], terminating in the bean's macro-emitted
    /// [`MethodEntry`](leaf_core::MethodEntry) downcast thunk over the published
    /// singleton ([`FixedTarget`]).
    ///
    /// This is the run-engine's "the proxy wraps the bean" half: a `#[advisable]`
    /// bean's method is invocable through the interceptor chain with NO hand-written
    /// `Call`/`Tail`/`FixedTarget` — the macro-emitted [`MethodTable`] supplies the
    /// real-method dispatch.
    ///
    /// # Errors
    /// An [`AdviceError`] if the bean is not advised / has no method table, the
    /// method is not in the table, the singleton is not yet published, or any
    /// interceptor / the real method faults.
    pub async fn invoke(
        &self,
        registry: &Registry,
        engine: &Engine,
        bean: BeanId,
        method: MethodKey,
        args: ErasedArgs,
    ) -> Result<ErasedRet, AdviceError> {
        let chain = self.by_bean.get(&bean).ok_or(AdviceError::DowncastMismatch { method })?;
        let table = self.tables.get(&bean).ok_or(AdviceError::DowncastMismatch { method })?;
        let entry = table.lookup(method).ok_or(AdviceError::DowncastMismatch { method })?;

        // The innermost target is the already-published singleton (FixedTarget); the
        // tail drives the macro-emitted downcast thunk over it.
        let target = FixedTarget::new(
            Self::fixed_target_for(registry, bean).map_err(AdviceError::TargetResolution)?,
        );
        let cx = ResolveCtx::for_engine(engine);
        // `Call.args` carries the REAL (cloneable) args (the advised-arg ABI): every
        // interceptor in the chain can INSPECT them (cache-key-from-arg-#0, validate a
        // @Valid arg), and a REPLAYABLE `Next::proceed` (retry) re-runs the
        // args-bearing target by re-cloning a fresh copy off `Call.args` per attempt —
        // so an args-bearing advised method is now genuinely re-proceedable (the
        // take-once-cell limitation is dissolved).
        let call = Call::new(
            method,
            BeanKey::ByContract(registry.descriptor(bean).contract),
            args,
            &target,
            &cx,
        );
        let invoke = entry.invoke;
        let tail: Box<Tail> = Box::new(move |call: &Call<'_>| {
            // Re-resolve the singleton per proceed, then drive the macro-emitted
            // downcast thunk over a FRESH clone of the args (re-cloned each replay, so
            // a retry re-runs an args-bearing method with the same args every attempt).
            let resolved = call.source.get(call.cx);
            let fresh = call.args.replay();
            Box::pin(async move {
                let bean = resolved.await.map_err(AdviceError::TargetResolution)?;
                invoke(&bean, fresh, call.cx).await
            })
        });
        chain.invoke(&call, &*tail).await
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
    #[leaf_macros::async_impl]
    impl leaf_core::Interceptor for Recorder {
        async fn intercept(
            &self,
            call: &Call<'_>,
            mut next: Next<'_>,
        ) -> Result<ErasedRet, AdviceError> {
            LOG.lock().unwrap().push("enter");
            let r = next.proceed(call).await;
            LOG.lock().unwrap().push("exit");
            r
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

    // A dedicated recorder for the transparent-invoke test (its own log so it does
    // not race the shared `LOG` the install_resolves test clears + asserts on).
    static TI_LOG: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    struct TiRecorder;
    #[leaf_macros::async_impl]
    impl leaf_core::Interceptor for TiRecorder {
        async fn intercept(
            &self,
            call: &Call<'_>,
            mut next: Next<'_>,
        ) -> Result<ErasedRet, AdviceError> {
            TI_LOG.lock().unwrap().push("enter");
            let r = next.proceed(call).await;
            TI_LOG.lock().unwrap().push("exit");
            r
        }
    }

    #[test]
    fn install_with_tables_routes_a_transparent_invoke_through_the_chain() {
        TI_LOG.lock().unwrap().clear();

        let mut builder = RegistryBuilder::new();
        let d = svc_desc();
        let id = builder.register(d, Arc::new(SvcProv(d))).unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        block(engine.get::<Svc>()).unwrap(); // publish the singleton

        let advisor = AdvisorPairing::new(
            ContractId::of("test::RecorderAdvisor"),
            OrderKey::implicit(),
            Role::Application,
            &ANY,
            |_c: &dyn Container| Box::pin(async { Ok(Arc::new(TiRecorder) as Arc<dyn Interceptor>) }),
        )
        .into_descriptor();
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

        // The macro-emitted MethodTable: the downcast invoke thunk for Svc::add.
        fn add_invoke(
            bean: &ErasedBean,
            args: ErasedArgs,
            _cx: &ResolveCtx<'_>,
        ) -> leaf_core::BoxFuture<'static, Result<ErasedRet, AdviceError>> {
            let svc = Arc::clone(bean).downcast::<Svc>().expect("Svc");
            Box::pin(async move {
                let add = args
                    .unpack::<i64>()
                    .map_err(|_| AdviceError::DowncastMismatch { method: MethodKey::of("test::Svc::add") })?;
                Ok(ErasedRet::pack(svc.base + add))
            })
        }
        static TABLE: MethodTable = MethodTable(&[leaf_core::MethodEntry {
            key: MethodKey::of("test::Svc::add"),
            invoke: add_invoke,
        }]);
        let tables = [MethodTablePairing::new(ContractId::of("test::Svc"), &TABLE)];

        // INSTALL WITH TABLES (R4): the transparent-invoke seam is now live.
        let installed =
            block(InstalledProxies::install_with_tables(&engine, &plan, &[advisor], &tables)).unwrap();
        assert!(installed.is_advised(id));

        // TRANSPARENT invoke: no hand-written Call/Tail — the macro-emitted thunk drives it.
        let out = block(installed.invoke(
            engine.registry(),
            &engine,
            id,
            MethodKey::of("test::Svc::add"),
            ErasedArgs::pack(5_i64),
        ))
        .unwrap();
        assert_eq!(out.unpack::<i64>().unwrap(), 105, "the real method ran (100 + 5)");
        assert_eq!(*TI_LOG.lock().unwrap(), vec!["enter", "exit"], "routed through the chain");
    }

    // A retrying interceptor: re-`proceed`s the call N times (the substrate's
    // REPLAYABLE `Next`) — the args-bearing replay the take-once cell could not do.
    struct RetryThrice;
    #[leaf_macros::async_impl]
    impl leaf_core::Interceptor for RetryThrice {
        async fn intercept(
            &self,
            call: &Call<'_>,
            mut next: Next<'_>,
        ) -> Result<ErasedRet, AdviceError> {
            let mut last = next.proceed(call).await;
            last = next.proceed(call).await.or(last);
            next.proceed(call).await.or(last)
        }
    }

    #[test]
    fn transparent_invoke_carries_real_args_and_a_retry_re_proceeds_an_args_bearing_method() {
        // THE headline gap fix: a transparent `invoke` of an args-bearing method
        // (1) carries the REAL typed args on `Call.args` (inspectable by the chain),
        // and (2) is RE-PROCEEDABLE — a retrying interceptor re-runs the args-bearing
        // method 3x, each attempt re-cloning a fresh copy of the args off `Call.args`.
        let mut builder = RegistryBuilder::new();
        let d = svc_desc();
        let id = builder.register(d, Arc::new(SvcProv(d))).unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        block(engine.get::<Svc>()).unwrap(); // publish the singleton

        // The advisor: a retry that re-proceeds an args-bearing call 3 times.
        let advisor = AdvisorPairing::new(
            ContractId::of("test::RetryAdvisor"),
            OrderKey::implicit(),
            Role::Application,
            &ANY,
            |_c: &dyn Container| Box::pin(async { Ok(Arc::new(RetryThrice) as Arc<dyn Interceptor>) }),
        )
        .into_descriptor();
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

        // The macro-emitted thunk: count the REAL-method invocations + assert the
        // args reached the thunk fresh every attempt (the args-bearing tuple `(i64,)`).
        static TARGET_RUNS: AtomicU32 = AtomicU32::new(0);
        TARGET_RUNS.store(0, Ordering::SeqCst);
        fn add_invoke(
            bean: &ErasedBean,
            args: ErasedArgs,
            _cx: &ResolveCtx<'_>,
        ) -> leaf_core::BoxFuture<'static, Result<ErasedRet, AdviceError>> {
            TARGET_RUNS.fetch_add(1, Ordering::SeqCst);
            let svc = Arc::clone(bean).downcast::<Svc>().expect("Svc");
            Box::pin(async move {
                let (add,) = args.unpack::<(i64,)>().map_err(|_| {
                    AdviceError::DowncastMismatch { method: MethodKey::of("test::Svc::add") }
                })?;
                Ok(ErasedRet::pack(svc.base + add))
            })
        }
        static TABLE: MethodTable = MethodTable(&[leaf_core::MethodEntry {
            key: MethodKey::of("test::Svc::add"),
            invoke: add_invoke,
        }]);
        let tables = [MethodTablePairing::new(ContractId::of("test::Svc"), &TABLE)];

        let installed =
            block(InstalledProxies::install_with_tables(&engine, &plan, &[advisor], &tables)).unwrap();

        // TRANSPARENT invoke of an ARGS-BEARING method through the retry chain.
        let out = block(installed.invoke(
            engine.registry(),
            &engine,
            id,
            MethodKey::of("test::Svc::add"),
            ErasedArgs::pack((5_i64,)),
        ))
        .unwrap();
        // The real method ran 3 times (the args-bearing replay), each over (100 + 5).
        assert_eq!(out.unpack::<i64>().unwrap(), 105, "the real method ran (100 + 5)");
        assert_eq!(
            TARGET_RUNS.load(Ordering::SeqCst),
            3,
            "the args-bearing method was RE-PROCEEDED 3 times (replay re-clones the args)"
        );
    }

    // An interceptor that INSPECTS arg #0 off `Call.args` and routes on it (the
    // cache-key / validation read shape) — short-circuits on a sentinel arg.
    struct RouteOnArgZero;
    impl leaf_core::Interceptor for RouteOnArgZero {
        fn intercept<'a>(
            &'a self,
            call: &'a Call<'a>,
            mut next: Next<'a>,
        ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
            // Read arg #0 WITHOUT consuming the args (they survive for the tail).
            let arg0 = call.args.downcast_ref::<(i64,)>().map(|(a,)| *a);
            Box::pin(async move {
                if arg0 == Some(-1) {
                    // Route a sentinel arg to a short-circuit (the body never runs).
                    return Ok(ErasedRet::pack(7777_i64));
                }
                next.proceed(call).await
            })
        }
    }

    #[test]
    fn transparent_invoke_lets_an_interceptor_read_arg_zero_and_route_on_it() {
        let mut builder = RegistryBuilder::new();
        let d = svc_desc();
        let id = builder.register(d, Arc::new(SvcProv(d))).unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        block(engine.get::<Svc>()).unwrap();

        let advisor = AdvisorPairing::new(
            ContractId::of("test::RouteAdvisor"),
            OrderKey::implicit(),
            Role::Application,
            &ANY,
            |_c: &dyn Container| {
                Box::pin(async { Ok(Arc::new(RouteOnArgZero) as Arc<dyn Interceptor>) })
            },
        )
        .into_descriptor();
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

        fn add_invoke(
            bean: &ErasedBean,
            args: ErasedArgs,
            _cx: &ResolveCtx<'_>,
        ) -> leaf_core::BoxFuture<'static, Result<ErasedRet, AdviceError>> {
            let svc = Arc::clone(bean).downcast::<Svc>().expect("Svc");
            Box::pin(async move {
                let (add,) = args.unpack::<(i64,)>().map_err(|_| {
                    AdviceError::DowncastMismatch { method: MethodKey::of("test::Svc::add") }
                })?;
                Ok(ErasedRet::pack(svc.base + add))
            })
        }
        static TABLE: MethodTable = MethodTable(&[leaf_core::MethodEntry {
            key: MethodKey::of("test::Svc::add"),
            invoke: add_invoke,
        }]);
        let tables = [MethodTablePairing::new(ContractId::of("test::Svc"), &TABLE)];
        let installed =
            block(InstalledProxies::install_with_tables(&engine, &plan, &[advisor], &tables)).unwrap();

        // A non-sentinel arg proceeds to the real method (100 + 5).
        let out = block(installed.invoke(
            engine.registry(),
            &engine,
            id,
            MethodKey::of("test::Svc::add"),
            ErasedArgs::pack((5_i64,)),
        ))
        .unwrap();
        assert_eq!(out.unpack::<i64>().unwrap(), 105, "a non-sentinel arg proceeds (100 + 5)");

        // The sentinel arg #0 = -1 routes to the short-circuit (body never runs).
        let out = block(installed.invoke(
            engine.registry(),
            &engine,
            id,
            MethodKey::of("test::Svc::add"),
            ErasedArgs::pack((-1_i64,)),
        ))
        .unwrap();
        assert_eq!(out.unpack::<i64>().unwrap(), 7777, "arg #0 routed to the short-circuit");
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
