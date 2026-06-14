# ADR 05 — Container Shape & the Provider Unification  `[container-shape]`

> **Note (2026-06-14):** the optional *dynamic-WASM source / WASM-"classpath"* mentioned below is **DROPPED** — leaf autodiscovery is native link-time registration only. Ignore WASM/dynamic-module references here; they are struck from the active design.


## Decision
ADOPT a TWO-NAMED-TYPE container shape — ONE concrete `Engine` struct (the BeanFactory/DefaultListableBeanFactory analogue) HAS-A'd by ONE concrete `Context` façade (the ApplicationContext analogue) — and RESOLVE the "everything is a Provider" unification as: there is exactly ONE *creation/registry* primitive, the upstream `Provider` trait already minted in leaf-core by the ownership-model ADR; the three flagged R-variants (factory-bean R2 "Provider-of-Provider", bean-registry R3 "provider-store kernel", bean-instantiation R3 "pluggable-creation-strategy") are NOT three primitives and NOT three SPIs — they collapse into that ONE `Provider` seam plus a SINGLE concrete (non-pluggable) creation driver living in `Engine`. The *consumer-side* `ObjectProvider<T>`/`Lazy<T>`/jakarta-`Provider<T>` family (deferral-primitives) is a SEPARATE concept (a typed lookup handle), explicitly NOT the same primitive, and is renamed at its leaf-core boundary to avoid the name collision.

(A) CONTAINER SHAPE = R1 (two named types), NOT R2 (one capability-Container) and NOT R3 (kernel+extension-SPI). In leaf-core:
- `Engine` — ONE concrete struct (no `dyn Engine` trait, no pluggable-strategy kernel). It owns the frozen `Registry` (registry-substrate ADR: BeanId-keyed indices, `singletons: Box<[OnceCell<ErasedBean>]>`, `providers: Box<[Arc<dyn Provider>]>`), the per-bean creation guard (OnceCell-per-slot), and the policy struct (allow_override, allow_circular, strict_locking). `Engine` is the embeddable escape hatch (charter §2.10), fully usable standalone, and INERT w.r.t. declarative features exactly as Spring's raw DefaultListableBeanFactory.
- `Context` — ONE concrete struct that OWNS exactly one `Engine` (HAS-A, never Deref-subclassing) plus the context-service handles. It re-exposes the resolution surface by delegation and adds `refresh().await`, which AUTO-DETECTS infrastructure providers (`Descriptor.role == Role::Infrastructure`) and installs them before any application bean is built — the inert→live switch.
- Context services (events, MessageSource, ResourceLoader, Environment, parent) are ALWAYS-PRESENT fields on `Context` (R1's hard guarantee), NOT R2 `Option<&dyn Trait>` capabilities. Their behavioral activation/back-off rides auto-config (owner-decision #2), not an Option at the container-shape level.
- The framework's own infrastructure is dogfooded as role=Infrastructure providers + a FIXED-ORDER refresh template owned by refresh-lifecycle — a CONCRETE sequence inside `Context::refresh()`, NOT R3's open `dyn ContextExtension` SPI. We take R3's insight (dogfood infrastructure as ordered descriptors) without its cost.
- `leaf::run()` sits ATOP `Context` (orchestration), NOT a third container type.

(B) THE PROVIDER UNIFICATION = ONE primitive, already minted. The primitive is the ownership-model ADR's `trait Provider { fn descriptor(&self) -> &Descriptor; fn provide<'a>(&'a self, cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Published, ResolveError>>; }`. This ADR introduces NO second Provider. It resolves the three R-variants ONTO that one trait:
- factory-bean R2: ADOPTED. A FactoryBean is a `Provider` whose provide realizes the user factory and delegates to factory.create. NO second registry slot; `&factory` is a `BeanKey::ByName` + Deref flag. The factory object stays a FULL bean (own BeanId, Aware/init/BPP) resolved THROUGH the one creation driver — fixing R2's "factory may not get full bean treatment" con.
- bean-registry R3: the origin-agnostic Provider-store part is ADOPTED (registry-substrate already made providers the store). The swappable-strategy SPI part is REJECTED: merge/store/guard/resolver are each ONE concrete implementation fixed by the upstream ADRs, not Box<dyn> SPIs.
- bean-instantiation R3: the "Provider returns raw, publication is the divergence" insight is ALREADY the ownership-model `Published` enum. ADOPT a SINGLE CONCRETE driver (`Engine::create`), not a Box<dyn Strategy> composition; prototype hand-off-and-forget is `Published::Owned` (a data value).

(C) THE NAME COLLISION between construction-side `Provider` and consumer-side jakarta-`Provider<T>` is RESOLVED by renaming the consumer family at the leaf-core boundary: `Lookup<T>` (ObjectProvider analogue), `LazyRef<T>` (@Lazy/ObjectFactory, OnceCell-cached for singleton targets), `Inject<T>` (jakarta Provider<T> JSR-330 strict-basic alias). We adopt deferral-primitives R1 (concrete typed handles, deferral visible in the type) backed by R2's single internal `resolve(Strictness, Cardinality)` driver so the most-missed `get_if_available` semantic cannot drift. R3's compile-time-presence arm is DEFERRED to Phase 3.

## Rationale
- DX#1 + intent parity (charter §2.1, §2.2): R1's two named types preserve the BeanFactory/ApplicationContext landmarks AND make the load-bearing behavior ('a bare engine is inert, a context auto-detects infrastructure and is live') STRUCTURAL rather than a runtime flag. R2 collapses this into a runtime state of one struct that still carries all fields, eroding the hard guarantee ('a Context ALWAYS has events') into an Option the call site must None-check. R1 keeps the guarantee structural.
- The escape-hatch invariant (charter §2.10) is satisfied exactly by R1: the bare Engine is a first-class public type giving embedders raw-DefaultListableBeanFactory control with provably nothing extra (R2 con: 'hard to express the minimal embeddable engine'; R3 con: 'the bare engine is an empty-extension-list special case rather than a named affordance').
- REJECTING the pluggable-strategy kernel is forced by the upstream ADRs having ALREADY decided what those strategies would be: ownership-model fixed publication (Published) and the creation seam (Provider/BoxFuture); registry-substrate fixed merge (freeze), the singleton store and creation guard (OnceCell-per-slot), keying, and the candidate resolver. A Box<dyn SingletonStore>/Box<dyn MergeStrategy> SPI re-opens settled decisions, widens the ultra-stable kernel ABI every macro hard-codes paths into, kills the monomorphized fast path (charter §2.2), and adds Arc/Box churn (charter §2.5) for A/B-ing flexibility nothing requests. The register itself flags pluggable-kernel as 'borders on pre-deciding the meta-architecture and widens the stable ABI, so it should follow 1–4' — and 1–4 have now obviated it.
- The 'everything is a Provider' unification (charter §2.11) is genuinely achieved by COLLAPSING onto the one existing primitive. The register diagnosed the three R-variants as 'effectively ONE bet on the registry/creation meta-architecture, not three independent options' (line 2383); the honest resolution of ONE bet is ONE primitive. leaf_core::Provider already serves native beans, FactoryBean products, test doubles, and the WASM host-proxy identically (ownership-model: 'WASM-ness stops at the proxy'). FactoryBean-R2 dissolves cleanly (one descriptor, no second slot). Maximal unification, zero new ABI.
- Keeping the CONSUMER-side deferral family DISTINCT from the construction-side Provider is correct, not a unification failure: Provider is a creation seam the ENGINE calls to produce a value; Lookup<T> is a typed handle a USER bean holds to re-resolve later (live-handle, holds no value, Weak container back-ref). Conflating them is the over-reach the register warned about. The only real problem was the NAME collision with jakarta Provider<T>, fixed by renaming the consumer family.
- Auto-detection of infrastructure as a CONCRETE refresh-template phase (not R3's open SPI) honors owner-decision #1 (maximal-magic discovery) + the mandatory anti-DCE self-check (cross-crate §152) at the right altitude: refresh() filters role=Infrastructure and installs in fixed order, then runs the expected-vs-found self-check registry-substrate already mandates. An open SPI would let user code re-order the framework's own bring-up — power leaf does not want in the kernel ABI (an SPI is far harder to remove later than to add).
- Async-honest + boxed-future standard (charter §2.3, ownership-model's forced consequence): Provider::provide returns BoxFuture because the origin-agnostic dyn seam forces it; cold for singletons; the monomorphized fast path stays available for known-concrete sites. The consumer family's .get().await makes deferred-lookup execution context explicit. One concrete engine means ONE place where refresh/eager-init execution context is reasoned about, not a fan-out of strategy impls.
- The starters+BOM owner-decision (#3) lands naturally on R1: a leaf-starter-* crate bundles libs + role=Infrastructure auto-config providers; the umbrella/facade crate (cross-crate §257) force-links them and pins versions ([workspace.dependencies] internally + a version-pinned facade downstream as the BOM analogue). The Context auto-detects and backs off to user beans (owner-decision #2) — all WITHOUT a pluggable kernel: a starter contributes Providers + Descriptors, never an Engine impl or a strategy.

## Options considered & rejected
- **R2 — single capability-configured Container (façade-vs-engine as a layer/capability toggle on ONE struct)** — rejected: Erodes the hard structural guarantee ('an ApplicationContext ALWAYS has events/messages/environment') into queryable Option<&dyn Trait> capabilities the call site must None-check, and pushes the struct toward Option<Arc<...>> fields (the heap/Arc churn charter §2.5 warns against) unless full typestate is paid (which explodes builder-type combinatorics and leaks the concrete builder type into cross-crate signatures). It cannot cleanly express 'the minimal embeddable engine with provably nothing extra' — the bare config is a runtime state of a struct that still carries every field. R1 gives the minimal Engine as a distinct, provably-minimal type.
- **R3 — tiny object-safe kernel + façade composed from pluggable dyn ContextExtensions (SPI-first)** — rejected: Over-generalizes the meta-architecture Phase 2 is supposed to RESOLVE, not widen. Every behavior behind a dyn ContextExtension forces boxed futures + Box/Arc churn at bring-up (charter §2.5) and threads the simple 'just get a bean' path through an extension-driven refresh. It pre-judges machinery the upstream ADRs already fixed, and exposing the framework's own bring-up ordering as a user-pluggable SPI is power leaf does not want in its ultra-stable kernel ABI. We KEEP R3's insight (dogfood infrastructure as ordered role-marked descriptors run by a fixed-order template) without its cost, by making that template a CONCRETE sequence in Context::refresh().
- **Engine as a dyn Engine trait with multiple impls (or the bean-registry-R3 / bean-instantiation-R3 pluggable-strategy kernels)** — rejected: The upstream ownership-model and registry-substrate ADRs ALREADY decided every one of these 'strategies' as a single concrete implementation (Published enum; OnceCell-per-slot store and guard; freeze-built merge; fixed determine_winner ladder). Re-exposing them as Box<dyn> SPIs re-opens settled decisions, widens the kernel ABI every macro hard-codes paths into, costs the monomorphized fast path (charter §2.2), and adds dispatch + heap churn (charter §2.5) — for A/B-ing flexibility the charter never requests. Origin-agnosticism is ALREADY delivered by the Provider seam + ErasedBean (a WASM proxy is a Provider, not a swapped Engine), so the pluggability buys nothing. The register orders this AFTER decisions 1–4, which have now obviated it.
- **A genuinely UNIVERSAL single Provider that is BOTH the creation seam AND the consumer-side deferral handle** — rejected: They are different primitives with opposite data-flow: the construction-side Provider is what the ENGINE calls to PRODUCE a value (provide -> Published); the consumer-side Lookup/jakarta-Provider is what a USER bean HOLDS to re-resolve LATER (live-handle, holds no value, Weak back-ref, strictness ladder). Forcing them into one type is the exact over-reach the register cautioned against ('one universal Provider abstraction partially designed in three places with three slightly different shapes'). The only real conflict was the NAME (jakarta Provider<T> vs leaf_core::Provider), resolved by renaming the consumer family. ONE creation primitive, ONE consumer-lookup family, no conflation.
- **deferral-primitives R3 — compile-time/startup-resolved presence as the primary model for the consumer family** — rejected: Its own T1 analysis concedes the multiple/cardinality arm (stream membership), the dynamic/WASM arm, and the scoped/getIfUnique arm are IRREDUCIBLY runtime — so R3 carries the full runtime resolver PLUS a static single-valued path (more total surface than R1) and splits a single Lookup<T>'s failure mode across two timings (boot error vs first-call None) unpredictably. We adopt R1's typed handles + R2's single internal driver (so the most-missed get_if_available semantic lives in one place) and DEFER R3's startup-validation as a Phase-3 optimization layered on the freeze, where it degrades gracefully.

## Consequences
- FORCES refresh-lifecycle to own the CONCRETE fixed-order refresh template inside Context::refresh() (defns-frozen → BFPP rewrites[cold] → auto-detect role=Infrastructure → BPP processors → init context services → eager-instantiate non-lazy singletons all-or-nothing → SmartInitializing). It is a concrete sequence, NOT a dyn ContextExtension list; a Phase-3 thin escape MAY expose ordered hooks over it, but the kernel ships the concrete template.
- FORCES the anti-DCE expected-vs-found self-check (registry-substrate freeze() + cross-crate §152) to be the gate that makes maximal-magic discovery (owner-decision #1) safe: because Engine is recognition (downcast) not validation and there is no declared scan-scope, Context::refresh() MUST run the self-check over the link-collected descriptor set against the app-root expected-crate manifest, surfacing a DCE-dropped crate as a loud NoSuchBean naming the missing source rather than a silent inert feature.
- FORCES the consumer-side deferral family to be RENAMED at the leaf-core boundary (Lookup<T>, LazyRef<T>, Inject<T>) to free the name Provider for the single construction seam. Deferral-primitives, lazy-initialization, and lookup-method injection all bind to these renamed handles; jakarta JSR-330 Provider<T> parity is delivered by Inject<T>. The strictness/cardinality ladder lives in ONE internal resolve(Strictness, Cardinality) driver (R2's insight) so the three-tier semantics cannot drift.
- FORCES FactoryBean Indirection to be ONE Provider impl (FactoryBeanProvider) with NO second registry slot: one Descriptor/BeanId for the product, the factory object a full bean reachable by BeanKey::ByName + a Deref flag. The factory gets full Aware/init/BPP treatment because FactoryBeanProvider resolves it THROUGH the engine's one creation driver. The product TypeId is emitted as a provides[] row so candidate-resolution finds it without realization.
- FORCES bean-instantiation to be ONE concrete creation driver (Engine::create = construct→expose-early→populate→init→publish); prototype's hand-off-and-forget is Published::Owned, singleton/scoped is Published::Shared(ErasedBean). The creation guard is registry-substrate's OnceCell-per-slot (no global lock, charter §2.5). bean-instantiation R3's strategy SPIs are explicitly NOT kernel ABI.
- FORCES the context-service handles (events, MessageSource, ResourceLoader, Environment, parent) to be ALWAYS-PRESENT fields on Context (structural guarantee), with activation/back-off riding auto-config (owner-decision #2), NOT R2-style Option<capability>. A bare Engine lacks them entirely (the structural BeanFactory-vs-ApplicationContext difference). Env is the ownership-model cheap-clone Arc<EnvCore> handle.
- FORCES leaf::run() to be a THIRD orchestration layer above Context, not a third container type: it builds Engine→Context, applies ContextInitializers, drives Context::refresh().await, invokes runners, signals ready, and owns FailureAnalysis + the cancel-vs-close fork. Teardown is the explicit container-driven Context::shutdown().await (no async Drop).
- FORCES the starters+BOM story (owner-decision #3) to be expressible WITHOUT a pluggable kernel: a leaf-starter-* crate contributes role=Infrastructure Providers + Descriptors only; the umbrella/facade crate force-links them (integration crates depend on leaf-core, never the umbrella — cross-crate §264) and is the BOM analogue (version-pinned facade + internal [workspace.dependencies]). No Engine impl or strategy is ever contributed by a starter.
- ACCEPTS R1's named delegation cost: Context must forward the resolution surface to its owned Engine (no inheritance), a surface-drift risk mitigated by keeping the shared surface a single inherent-method set on Engine that Context calls through (not a re-declared public trait). ACCEPTS two named types as more surface than one Container — justified by the structural guarantee and the provably-minimal embeddable Engine.
- ACCEPTS that origin-agnosticism is delivered entirely by the Provider seam + ErasedBean (a WASM host-proxy, FactoryBean product, and test double are all just Providers the one concrete Engine cannot tell apart) — so the WASM two-source duality enters via registry-substrate's reopen_epoch() re-freeze appending Provider rows, NOT via a swapped Engine or a ContextExtension. WASM provider trap/teardown push into the Provider impl + TeardownLedger (Phase-3 WASM-local).

## API / type sketch

// ── leaf-core: container shape — ONE concrete Engine + ONE concrete Context façade ──
// (Provider, ErasedBean, Published, Ref, ResolveCtx, BeanKey, Registry, Descriptor, ScopeDef,
//  Role come from the upstream ownership-model + registry-substrate ADRs — NOT re-minted here.)

pub struct EnginePolicy { pub allow_override: bool, pub allow_circular: bool, pub strict_locking: bool }

// THE bare DI engine (DefaultListableBeanFactory analogue) — ONE concrete struct, no `dyn Engine`.
pub struct Engine {
    registry: Registry,            // frozen: BeanId-keyed indices + singletons:[OnceCell<ErasedBean>] + providers:[Arc<dyn Provider>]
    policy:   EnginePolicy,        // creation guard IS the OnceCell-per-slot in registry.singletons (no global lock)
}
impl Engine {
    pub fn builder() -> EngineBuilder;                                   // append Descriptor + Arc<dyn Provider>, then .freeze()
    pub async fn get<T: Bean + ?Sized>(&self) -> Result<Ref<T>, ResolveError>;     // singleton/scoped
    pub async fn get_owned<T: 'static>(&self) -> Result<T, ResolveError>;          // prototype
    pub async fn get_erased(&self, key: BeanKey) -> Result<ErasedBean, ResolveError>; // dynamic/WASM/&-deref lane
    pub fn contains(&self, key: &BeanKey) -> bool;
    // THE one concrete creation driver (NOT a composition of pluggable strategies):
    async fn create(&self, id: BeanId, cx: &ResolveCtx<'_>) -> Result<Published, ResolveError>;
    //   = guard(slot) { publish(providers[id].provide(cx).await?) }   construct→expose-early→populate→init→publish
    //     prototype => Published::Owned, else Published::Shared(ErasedBean)
    pub fn install_provider(&mut self, d: Descriptor, p: Arc<dyn Provider>) -> Result<BeanId, RegisterError>; // escape hatch
    pub async fn shutdown(&self);   // explicit teardown (no async Drop), drains TeardownLedger
}

// ApplicationContext analogue — ONE concrete struct that HAS-A exactly one Engine.
pub struct Context {
    engine:    Engine,                       // HAS-A, never Deref-subclassing
    events:    EventPublisher,               // ALWAYS-PRESENT (structural guarantee), not Option<capability>
    messages:  MessageSource,
    resources: ResourceLoader,
    env:       Env,                          // Arc<EnvCore> cheap-clone handle (ownership-model)
    parent:    Option<Ref<Context>>,         // Option only because a ROOT has no parent
}
impl Context {
    // FIXED-ORDER refresh template (refresh-lifecycle owns the sequence; CONCRETE, not a dyn-extension list):
    //  defns-frozen → BFPP rewrites(cold) → AUTO-DETECT role=Infrastructure → BPP → init context services
    //  → eager-instantiate non-lazy singletons (all-or-nothing) → SmartInitializing
    pub async fn refresh(&mut self) -> Result<(), RefreshError>;        // runs anti-DCE expected-vs-found self-check
    pub async fn get<T: Bean + ?Sized>(&self) -> Result<Ref<T>, ResolveError> { self.engine.get().await } // delegated
    pub fn events(&self) -> &EventPublisher { &self.events }            // ALWAYS Some, not Option
    pub async fn shutdown(&self);
}
fn auto_detect_infrastructure(reg: &Registry) -> impl Iterator<Item = BeanId>   // the inert→live switch
    { reg.iter().filter(|d| d.role == Role::Infrastructure).map(|d| d.id) }

// ── leaf::run — orchestration layer ATOP Context (NOT a third container type) ──
pub async fn run<M: AppMain>() -> Result<Context, RunError>;           // Engine→Context→refresh→runners→ready

// ── CONSUMER-side deferral family — DISTINCT primitive, RENAMED to free `Provider` for the creation seam ──
// holds a Resolve closure (Weak<ContainerCore> back-ref, ownership-model); holds NO value; re-resolves each call.
#[derive(Clone)] pub struct Lookup<T: ?Sized> { resolve: Resolve, ip: InjectionPoint, _t: PhantomData<fn()->T> } // ObjectProvider
impl<T: Bean + ?Sized> Lookup<T> {
    pub async fn get(&self) -> Result<Ref<T>, ResolveError>;            // strict: None=>NoSuchBean, Ambiguous=>NoUniqueBean
    pub async fn get_if_available(&self) -> Result<Option<Ref<T>>, ResolveError>; // None=absent, Err=AMBIGUOUS (most-missed)
    pub async fn get_if_unique(&self) -> Option<Ref<T>>;               // both tolerated
    pub fn stream(&self) -> CandidateStream<T>;                         // lazy, registration-order, ALL candidates
    pub fn ordered_stream(&self) -> CandidateStream<T>;                 // lazy, @Order/@Priority comparator
}
pub struct LazyRef<T: ?Sized> { inner: Lookup<T>, cell: OnceCell<Ref<T>> } // @Lazy / ObjectFactory: cache-on-first-get
pub struct Inject<T: ?Sized>  { inner: Lookup<T> }                         // jakarta Provider<T> (JSR-330) strict-basic alias
// ONE internal driver behind the family (R2's insight): strictness/cardinality as data, ladder in one place.
enum Strictness { Strict, AbsenceTolerant, FullyTolerant }
enum Cardinality { Single, Multiple }
//   resolve(Strictness, Cardinality) maps autowiring's Resolved{None|One|Ambiguous} per the bits.

// ── FactoryBean = ONE Provider impl (factory-bean R2 onto the single primitive; no second slot) ──
pub struct FactoryBeanProvider<F: FactoryBean> { factory_id: BeanId } // product memo lives in registry.singletons[product_id]
impl<F: FactoryBean> Provider for FactoryBeanProvider<F> {
    fn descriptor(&self) -> &Descriptor { /* product descriptor; provides[] carries product TypeId */ }
    fn provide<'a>(&'a self, cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Published, ResolveError>> {
        Box::pin(async move {
            let f: Ref<F> = cx.engine().get_by_id(self.factory_id).await?; // factory is a FULL bean, via the ONE driver
            Ok(f.create(cx).await?)                                        // & deref = BeanKey::ByName + Deref flag
        })
    }
}
// REJECTED (not kernel ABI): `dyn Engine`, Box<dyn SingletonStore/MergeStrategy/CreationGuard/PublicationStrategy>,
//   `dyn ContextExtension` SPI, R2 capability-Option container. DEFERRED (Phase 3): startup-presence-validation for Lookup.

## Forces on other concepts
- **Container Refresh Lifecycle**: OWNS the concrete fixed-order refresh template inside Context::refresh() (defns-frozen → BFPP rewrites[cold] → auto-detect role=Infrastructure → BPP → init context services → eager-instantiate all-or-nothing → SmartInitializing). A CONCRETE sequence, not a dyn ContextExtension list. Must run the registry-substrate expected-vs-found anti-DCE self-check before building application beans, and drive shutdown().await (no async Drop) on cancel/close.
- **FactoryBean Indirection**: Is ONE Provider impl (FactoryBeanProvider) over the single creation primitive — no second registry slot. One Descriptor/BeanId for the product; product TypeId emitted as a provides[] row for type-match-without-realization. The factory object is a FULL bean (own BeanId, Aware/init/BPP) resolved THROUGH Engine::create, not a lifecycle re-implemented inside the Provider. &factory deref = BeanKey::ByName + Deref flag (already in registry-substrate).
- **Bean Instantiation & Singleton Caching**: Is ONE concrete creation driver (Engine::create = construct→expose-early→populate→init→publish), NOT a composition of pluggable Populate/Init/Publication/Guard strategies. Publication divergence is the ownership-model Published enum (Shared/Owned); the guard is registry-substrate's OnceCell-per-slot (no global lock). bean-instantiation R3's strategy SPIs are explicitly not kernel ABI.
- **Bean Definition Registry & Engine**: There is exactly ONE concrete Engine (no dyn Engine trait, no pluggable-strategy kernel). The registry is the frozen registry-substrate Registry; merge/store/guard/candidate-resolution are each ONE concrete implementation fixed by the upstream ADRs, never Box<dyn> SPIs. Origin-agnosticism comes from the Provider seam, not from swapping the Engine.
- **Deferral Primitives / Lazy Initialization / Lookup-method injection**: Bind to the RENAMED consumer-side family Lookup<T> (ObjectProvider), LazyRef<T> (@Lazy/ObjectFactory, OnceCell-cached for singleton targets), Inject<T> (jakarta Provider<T> JSR-330 alias) — distinct from leaf_core::Provider (the creation seam). One internal resolve(Strictness, Cardinality) driver hosts the three-tier ladder so get_if_available's tolerate-absence-but-error-on-ambiguity semantic lives in one place. Handles hold a Weak-captured Resolve closure (ownership-model), never a value.
- **Application Event System / MessageSource / Resource Loading / Environment**: Are ALWAYS-PRESENT fields on the concrete Context (structural BeanFactory-vs-ApplicationContext guarantee), NOT R2-style Option<&dyn capability>. A bare Engine lacks them entirely. Behavioral activation/back-off rides auto-config (owner-decision #2), not an Option at the container-shape level. Env is the ownership-model cheap-clone Arc<EnvCore> handle.
- **Cross-Crate Composition / Anti-DCE / Starters & BOM (owner-decisions #1, #2, #3)**: Starters (leaf-starter-*) contribute role=Infrastructure Providers + Descriptors only — never an Engine impl or a strategy (no pluggable kernel exists to plug into). The umbrella/facade crate force-links them (integration crates depend on leaf-core, never the umbrella) and is the BOM analogue (version-pinned facade + internal [workspace.dependencies]). Context::refresh() auto-detects infrastructure and runs the mandatory expected-vs-found self-check; auto-config beans back off to user beans.
- **Application Entry Point & Run Pipeline (leaf::run, bootstrap)**: Is a THIRD orchestration layer ATOP Context, not a third container type. It builds Engine→Context, applies ContextInitializers, drives Context::refresh().await, invokes runners, signals ready, and owns FailureAnalysis + the cancel-vs-close fork. It never tears down beans itself — delegates to Context::shutdown().await.
- **Parent/Child Container Hierarchies**: The parent link is an Option<Ref<Context>> field on Context (Option only because a root has no parent — NOT an R2 capability). Resolution delegates upward (child sees parent, local shadows, contains-local ignores ancestors) while listing stays local; hierarchy delegation is by BeanKey walk across per-registry snapshots (a parent BeanId is not a child BeanId), never a shared global registry.

## Open sub-questions (→ Phase 3)
- The exact delegation surface between Context and its owned Engine: a single inherent-method set forwarded by hand vs a thin macro-generated forwarder vs a shared private trait both call — chosen to minimize surface-drift risk without re-declaring a public trait. Phase 3, low-stakes.
- Whether leaf::run() returns Context or a richer RunningApplication wrapper (carrying exit-code/shutdown/keep-alive handles from 07-bootstrap's unowned keep-alive gap), and how withHook/AbandonedRunException test-slicing threads through. Phase 3, bootstrap-local.
- Whether deferral-primitives R3's startup-presence-validation should ship as a Phase-3 optimization layered on the freeze (a Lookup<T> over a statically-absent target failing at the validation pass rather than first .await), and the precise rule exempting @Lazy-as-cycle-breaker from validation. Measured before built.
- Exact Role taxonomy and the auto-detect filter precision: does role=Infrastructure suffice, or is a finer ordering tier needed (Spring's PriorityOrdered/Ordered/rest partition) for the refresh template's BFPP/BPP install order. Co-decided with post-processor-spi. Phase 3.
- Naming bikeshed for the consumer family at the public API (Lookup/LazyRef/Inject vs other spellings) and whether Inject<T> (jakarta parity) is worth shipping at all vs documenting Lookup<T> as the one tool. Phase 3, DX-driven.
- The precise FactoryBeanProvider product-memo placement: product singleton lives in registry.singletons[product_id] (uniform with all singletons) vs an internal cell — and how SmartFactoryBean multi-type provide_as(t) interacts with the candidate provides[] rows. Phase 3, factory-bean-local.
