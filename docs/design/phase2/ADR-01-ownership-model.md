# ADR 01 — Scope / Lifetime / Ownership Model — the uniform erased shared handle + scope-keyed publication  `[ownership-model]`

> **Note (2026-06-14):** the optional *dynamic-WASM source / WASM-"classpath"* mentioned below is **DROPPED** — leaf autodiscovery is native link-time registration only. Ignore WASM/dynamic-module references here; they are struck from the active design.


## Decision
ADOPT a UNIFORM ERASED SHARED HANDLE as leaf's single shared-bean currency, with PUBLICATION (not the handle) carrying the genuine ownership differences between scopes. Concretely, in leaf-core (the ultra-stable kernel ABI every macro hard-codes paths to):

```
pub type ErasedBean = std::sync::Arc<dyn std::any::Any + Send + Sync>;   // THE canonical stored/published SHARED shape — ONE type
#[derive(Clone)] pub struct Ref<T: ?Sized>(std::sync::Arc<T>);            // typed sugar over ErasedBean; Ref<dyn Svc> or Ref<Concrete>
pub trait Bean: std::any::Any + Send + Sync {}                           // every service trait `: Bean`; Arc<dyn Bean> upcasts to any declared service trait AND to dyn Any (1.86)
```

The handle is uniform; the DIVERGENCE rides a small total `Published` enum produced by the publish step, keyed by scope multiplicity:

```
pub enum Published {
    Shared(ErasedBean),                   // singleton + EVERY context-scope (request/session/custom) instance
    Owned(Box<dyn std::any::Any + Send>), // prototype: an owned MOVE — never stored, never refcounted
}
```

So there are exactly TWO publication shapes and ONE shared-handle type. Multiplicity maps totally and tinily onto publication: `{Once, PerContextKey} -> Shared(ErasedBean)`, `PerResolution -> Owned(Box)`. Singleton and every context-scope instance are the SAME `Shared(ErasedBean)`; they differ ONLY in WHICH STORE holds the Arc and WHEN that store is dropped — data on three orthogonal axes `(Multiplicity, StoreSource, TeardownPolicy)`, never in the handle type.

`get<T>` RETURN SHAPE (honest about ownership, two flavors only):
```
impl Container {
    pub async fn get<T: Bean + ?Sized>(&self) -> Result<Ref<T>, ResolveError>;  // singleton/scoped: Ref<T> (downcast for concrete, trait-upcast for dyn Svc)
    pub async fn get_owned<T: 'static>(&self) -> Result<T, ResolveError>;        // prototype: owned T (downcast the Box)
    pub async fn get_erased(&self, key: BeanKey) -> Result<ErasedBean, ResolveError>; // origin-agnostic / dynamic / WASM lane
}
```

CONSTRUCTION seam is origin-agnostic and dyn, so the boxed future is forced (AFIT/RPITIT not dyn-compatible — true regardless of nightly):
```
pub trait Provider: Send + Sync {
    fn descriptor(&self) -> &Descriptor;
    fn provide<'a>(&'a self, cx: &'a ResolveCtx<'a>) -> futures::future::BoxFuture<'a, Result<Published, ResolveError>>;
}
```
Native beans, FactoryBean products, test doubles, AND the WASM host-proxy ALL implement `Provider` and publish the IDENTICAL `Arc<dyn Any+Send+Sync>` — "WASM-ness stops at the proxy"; the container cannot tell origins apart because the stored shape is one type.

CONTEXT SCOPES: a per-context InstanceStore (`HashMap<TypeKey, ErasedBean>` keyed inside the ambient context, reached via the context-propagation primitive, NEVER a ThreadLocal). Opened at the scope boundary, DROPPED at scope-end (refcount->0 unless in-flight work still holds a clone — the Arc is exactly what makes "referenced past the boundary by in-flight work" SOUND). A TeardownLedger of the same ErasedBeans drives async destruction callbacks (no async Drop). Singleton store is the container's; its ledger drains at context-close via explicit `shutdown().await`.

DEFERRAL HANDLES & CONTAINER BACK-REFERENCES are Arc-shaped but of a DIFFERENT type: a re-resolving closure `Resolve = Arc<dyn Fn(&InjectionPoint, Arity) -> BoxFuture<Result<Published, ResolveError>> + Send + Sync>` whose captured container reference is a `Weak<ContainerCore>` (no Arc cycle). Re-resolution yields a `Published`, so a provider transparently returns `Ref<T>` for shared targets and owned `T` for prototype targets.

CIRCULAR REFERENCES are broken by the deferral handle (the register's R3 posture): a deferral-typed edge is removed from the construction-time graph and resolves the FULLY-built target post-build, so there is NEVER an early erased handle to retroactively swap for a wrapped one — the Rust ownership impedance with no sound answer is sidestepped; constructor cycles remain fatal at the plan/startup pass.

The `Send + Sync + 'static` bound RIDES `ErasedBean`/`Bean`, which is exactly where the concurrency contract says it must live: the atomic Arc clone IS the happens-before edge (replacing Spring's singleton-mutex JMM edge, no global lock), and the borrow checker forbidding `&self`-field mutation through the Arc IS "guard your own mutation" made un-forgettable.

The arena+generation-index handle (`BeanRef<T>`) is EXPLICITLY REJECTED as the primary model and DEFERRED to Phase 3 as an optional, opt-in, singleton-only performance escape — never the default, never the published currency.

## Rationale
- DX#1 + unification (charter §2.1, §2.11): ONE shared-handle type answers the ~20 register-Cluster-1 features that each ask 'what is the stored/handed-out type' (registry value, publish output, scope-store value, WASM-proxy output, resolvableDependencies infra-handle, deferral re-resolution result) with the SAME Arc<dyn Any+Send+Sync>. The macro emits one descriptor shape; nothing in the engine branches on origin. This is the maximal-shared-machinery the charter demands phases 2-4 deliver.
- It is the ONLY shape the confirmed-stable envelope makes both keyable AND origin-agnostic with zero nightly (charter §2.8). Verified in rust-nightly-features-tradeoffs.md: Arc<dyn Any+Send+Sync>+downcast (1.0/1.29), trait upcasting (1.86 — Arc<dyn Bean>->Arc<dyn Svc> and ->dyn Any as cheap vtable adjustments), const TypeId::of (1.91 for const descriptor rows). An arena+DefIdx slot cannot express a request-scoped instance whose lifetime is not a fixed slot nor a WASM host-proxy; a container-owned-borrow leaks lifetimes into every get<T> signature and cannot be stored on a deferral handle that outlives one resolution. The uniform Arc serves singleton, every context-scope, deferral-stored, AND WASM origins identically.
- It honors the genuinely-different ownership shapes the keystone NAMES (register §1) instead of forcing one: prototype is an OWNED MOVE (Box, never Arc), which is strictly better than Spring's GC hand-off-and-forget — the container retains NOTHING and registers NO teardown, so Spring's 'init runs, destroy never' falls out as structural ABSENCE and the caller gets &mut by exclusive ownership with zero synchronization. Refcounting an exclusively-owned value would be the exact 'needless Arc' charter §2.5 forbids, and would re-introduce the leak the move avoids. Singleton and context-scopes are the SAME Shared(Arc) because they genuinely ARE both shared — they differ only in store ownership/lifetime (StoreSource + TeardownPolicy), which is DATA, not handle type.
- It carries the concurrency contract FOR FREE on the type (06 §1316, the contract's R1): Send+Sync+'static on the shared handle IS the rust-native safe-publication proof — the atomic Arc clone is the happens-before edge, giving Spring's final-like visibility with NO singleton mutex and NO global lock (charter §2.5), and the borrow checker makes 'guard your shared mutation' a compile error rather than advisory. The contract's lead phase2Question ('WHERE does the bound live?') is answered: it rides ErasedBean/Bean in ONE ultra-stable leaf-core location, propagating to every cross-crate contribution automatically (cross-crate §99-102) with zero per-crate restating.
- It is origin-agnostic by construction, satisfying the hard brief constraint and the WASM exploration's load-bearing move ('WASM-ness stops at the proxy', wasm §55): the host-proxy publishes the identical Arc<dyn Any+Send+Sync>; native, test-double, and FactoryBean-product all do too. The two-source duality (static link-time registry + optional dynamic directory-scanned WASM 'classpath') is INVISIBLE to the handle — both are just Providers of the same erased value.
- It rejects the arena-index as the PRIMARY model on principled grounds, not by oversight: BeanRef is genuinely cheaper for hot singleton re-reads, but the register itself flags it 'breaks for prototype/scoped', it presupposes a single app-root arena (breaks parent/child hierarchies), and a bare u32 index gives no soundness for scoped lifetimes where Rust ownership can't catch a stale slot. Making it the default would contradict origin-agnosticism and the WASM/scoped/deferral requirements. Keeping it as an opt-in Phase-3 escape for tight steady-state loops preserves the optimization without paying its costs in the kernel ABI — the right altitude for a keystone decision.

## Options considered & rejected
- **PURE arena + generation-index (BeanRef<T>) as the primary handle (the 'arena-index' proposal)** — rejected: Cannot express the full scope/origin matrix the keystone must serve: a request-scoped instance has no fixed slot, a WASM host-proxy is minted outside any arena, a deferral handle stored on a bean outlives one resolution, and a bare u32 gives no lifetime soundness for scoped beans (the proposal itself concedes 'unsafe for scoped' and falls back to Arc-in-store for scopes anyway). It also presupposes ONE app-root arena, which breaks parent/child container hierarchies (the proposal flags this as an open Phase-3 sub-question). The cache-friendly win is real ONLY for the same-ref-dereffed-many-times case and is equal-or-worse for once-per-request beans. So even its own design uses Arc for scoped/prototype/WASM and the arena only for singletons — i.e. it is NOT a unifying handle, it is an optimization. Demoting it to an opt-in Phase-3 escape (which this decision does) captures its upside without contaminating the kernel ABI with two ways to hold a singleton and a generation-check on the steady-state path.
- **SCOPE-DIFFERENTIATED handle family Bean<T, S: ScopeMarker> with Shared<T>/Owned<T>/Scoped<T> as THREE distinct typed shapes (the 'scope-differentiated' proposal)** — rejected: It correctly matches ownership to scope but pays for it with a THREE-shape resolution surface (resolve_shared / resolve_owned / resolve_scoped) and a Scoped<T> lifetime story the proposal itself calls 'the thinnest' (in-flight work outliving the boundary). This decision keeps that proposal's genuine insight — publication is the only divergence point, prototype is an owned move — but observes that Shared<T> and Scoped<T> are the SAME Arc shape differing only in which store owns them and when it drops; encoding that difference in the TYPE (a third handle) buys nothing the StoreSource+TeardownPolicy data axes don't, while tripling the public get<T> surface and forcing a marker-type generic into every signature. Collapsing Shared and Scoped into one Ref<T> (over Shared(ErasedBean)) and distinguishing only Owned (prototype) gives the same correctness with TWO get flavors instead of three — strictly better DX#1 at equal soundness.
- **Container-owned values handed out as borrows (&T / &dyn Trait with lifetimes)** — rejected: Leaks a lifetime into every get<T> signature (register §1, §9), making the resolution API viral and incompatible with storing a re-resolving deferral handle on a bean for the bean's whole life (the handle would need a 'static container reference a borrow cannot give). It also cannot model prototype's owned move nor a WASM proxy whose lifetime is not tied to a container borrow. Rust ownership idiom says 'a shared thing is an Arc' — fighting that here costs DX everywhere to save refcount bumps that constructor-injection already amortizes to wiring time.
- **A single universal envelope (force prototype into Arc too, so the publish step is literally one-typed)** — rejected: Re-introduces the exact 'needless Arc' the charter §2.5 forbids precisely where it bites: prototype is hand-off-and-forget, exclusively owned, needs no synchronization and no teardown. Wrapping it in Arc would add an atomic refcount for a value with one owner AND would require the container to track it (the moment you Arc it, the temptation to cache/teardown it returns — the very GC-tracked leak the owned move avoids). The two-arm Published enum is a deliberate, bounded non-uniformity (a total map from Multiplicity, one match at the publish/get seam) that is MORE faithful to Spring's intent than a fake uniformity.

## Consequences
- FORCES a startup self-check to be load-bearing: ErasedBean is RECOGNITION (downcast) not VALIDATION — Arc<dyn Any> proves nothing at wiring time about whether type T was registered, so a missing bean is a runtime/startup downcast-miss, not a compile error. This couples directly to owner-decision #1 (maximal-magic discovery with no declared scan-scope): the metadata/codegen layer plus the mandatory anti-DCE expected-vs-found self-check (cross-crate §152) MUST exist to turn a silently-DCE'd crate into a loud NoSuchBean naming the missing source. The erased handle makes this self-check non-negotiable.
- FORCES Send+Sync+'static on every SHARED-scope bean (stricter than Spring — a non-Sync singleton becomes a compile error at its shared scope). This is mostly a win (the #1 latent-race footgun becomes un-shippable) but the DX hit for genuinely single-threaded beans must be absorbed by a doctrine-citing #[diagnostic::on_unimplemented] message steering to a narrower scope ('make it prototype/request-scoped'), NOT a raw trait-solver error. A single-threaded-runtime ?Send carve-out is deferred to the §5/§6 pluggable-runtime question; bimodality (charter §2.3) is resisted.
- FORCES the async-across-dyn boxing standard (register §5b): the origin-agnostic Provider::provide MUST return BoxFuture (AFIT/RPITIT not dyn-compatible). Cold for singletons; a real per-resolution alloc for prototype/scoped hot paths. A monomorphized typed fast-path remains available as a Phase-3 escape for known-concrete sites, but the boxed dyn path is the origin-agnostic default this decision commits leaf to.
- FORCES the scope model (Bean Scopes) to be three orthogonal data axes — Multiplicity(Once|PerResolution|PerContextKey), StoreSource(which store the ambient yields), TeardownPolicy(AtClose|Never|AtKeyEnd) — with the total map {Once,PerContextKey}->Shared / PerResolution->Owned. Custom scopes contribute a (Multiplicity, StoreSource, TeardownPolicy) triple as a const descriptor through the same opt-in/non-global discovery substrate; they pick where instances live, never a whole new handle type.
- FORCES the context-propagation model (§5a) to provide an ambient store reference that survives .await: the per-context InstanceStore reference rides the context-propagation primitive (06 §1132), NEVER a ThreadLocal. The Arc strong-count — not an index — governs the actual free, which is what makes in-flight work referencing a scoped bean past its boundary SOUND. Teardown (no async Drop) is a container-driven async ledger drain, not Drop.
- FORCES deferral handles and all container back-references to capture Weak<ContainerCore>, not Arc, to avoid an Arc cycle between a bean and the container that owns it (register §1, resolvableDependencies §1/§9). Re-resolution yields Published, so the deferral surface transparently returns Ref<T> for shared targets and owned T for prototype targets; the strictness ladder (None/One/Ambiguous) maps onto Result/Option per method.
- FORCES the config-family handles to follow the same idiom for coherence: Environment is a cheap-clone Env(Arc<EnvCore>) Send+Sync handle (matching doc 02 R3), and Ref<T>-style sharing applies to MessageSource/cached-value/origin handles — the keystone's 'shared = Arc handle' answer settles the §1 sub-questions for env-read-handle, message-return-type, cached-value-ownership, and parent-link-lifetime by the same rule.
- ACCEPTS a real, named overhead cost (charter §2.5, confronted not hidden): every shared resolution is an atomic refcount bump + a downcast (TypeId compare + vtable check). Mitigated in priority order: (a) prototype is exempt (owned move); (b) constructor-injected collaborators are resolved ONCE at the holder's construction and stored as a typed Ref<T> field, so steady-state method calls touch NO refcount and NO downcast (the cost is paid at WIRING time, mirroring Spring's 'config fields need no volatile'); (c) the optional Phase-3 arena/BeanRef escape for hot leaf-internal infra. We explicitly do NOT make an arena the primary model.
- ACCEPTS that TypeId is NOT stable across compiler builds (nightly-doc §453): fine as an in-process key, but WASM contract identity and any persisted descriptor must key on author-assigned stable ids, never TypeId — a constraint the uniform handle inherits and passes to the cross-crate-identity sub-question.
- ACCEPTS a trait-upcast-vs-concrete-downcast registry asymmetry: a concrete-struct handle matches only by exact TypeId, while a dyn Svc injectable must be EXPLICITLY published as a dyn Svc type-index entry (the macro emits one upcast entry per declared injectable supertrait). Inherent to erasure-without-reflection; the uniform handle makes it unavoidable surface but it composes with trait upcasting (1.86) at zero extra alloc.

## API / type sketch

// ── leaf-core: THE one shared-handle currency (ultra-stable ABI; macros hard-code these paths) ──
pub type ErasedBean = std::sync::Arc<dyn std::any::Any + Send + Sync>;   // canonical stored/published SHARED shape

#[derive(Clone)]
pub struct Ref<T: ?Sized>(std::sync::Arc<T>);                            // typed sugar over ErasedBean
impl<T: ?Sized> std::ops::Deref for Ref<T> { type Target = T; /* .. */ }

// every service trait `: Bean` so Arc<dyn Bean> upcasts to any service trait AND to dyn Any (1.86)
pub trait Bean: std::any::Any + Send + Sync {}

// PUBLICATION: one shared handle for everything shared; owned move for prototype (total 2-arm map)
pub enum Published {
    Shared(ErasedBean),                   // singleton + request/session/custom-scope
    Owned(Box<dyn std::any::Any + Send>), // prototype: owned MOVE, never stored, never refcounted
}

// origin-agnostic construction seam — dyn => BoxFuture (AFIT not dyn-compatible); native/WASM/test identical
pub trait Provider: Send + Sync {
    fn descriptor(&self) -> &Descriptor;
    fn provide<'a>(&'a self, cx: &'a ResolveCtx<'a>)
        -> futures::future::BoxFuture<'a, Result<Published, ResolveError>>;
}

// read side — shared returns Ref<T> (downcast concrete OR trait-upcast to dyn Svc); prototype returns owned T
impl Container {
    pub async fn get<T: Bean + ?Sized>(&self) -> Result<Ref<T>, ResolveError>;        // singleton/scoped
    pub async fn get_owned<T: 'static>(&self) -> Result<T, ResolveError>;             // prototype
    pub async fn get_erased(&self, key: BeanKey) -> Result<ErasedBean, ResolveError>; // dynamic/WASM lane
}

// scope = three orthogonal data axes; Multiplicity drives the publication arm (total, tiny map)
pub enum Multiplicity { Once, PerResolution, PerContextKey } // {Once,PerContextKey}=>Shared; PerResolution=>Owned
pub enum StoreSource  { ContainerStore, AmbientStore(ScopeKind) }
pub enum TeardownPolicy { AtClose, Never, AtKeyEnd }
pub struct ScopeDef { pub multiplicity: Multiplicity, pub store: StoreSource, pub teardown: TeardownPolicy }
const SINGLETON: ScopeDef = ScopeDef { multiplicity: Multiplicity::Once,         store: StoreSource::ContainerStore, teardown: TeardownPolicy::AtClose };
const PROTOTYPE: ScopeDef = ScopeDef { multiplicity: Multiplicity::PerResolution, store: StoreSource::ContainerStore, teardown: TeardownPolicy::Never };
const REQUEST:   ScopeDef = ScopeDef { multiplicity: Multiplicity::PerContextKey, store: StoreSource::AmbientStore(ScopeKind::Request), teardown: TeardownPolicy::AtKeyEnd };

// per-context store yields the SAME ErasedBean; dropped at boundary (Arc strong-count governs the real free)
pub trait InstanceStore: Send + Sync {
    fn get_or_init<'a>(&'a self, key: TypeKey, p: &'a dyn Provider, cx: &'a ResolveCtx<'a>)
        -> futures::future::BoxFuture<'a, Result<ErasedBean, ResolveError>>;
    fn ledger(&self) -> &TeardownLedger;   // async destruction callbacks; NO async Drop
}

// deferral / live handles: Arc-closure yielding Published; back-ref is Weak (no Arc cycle with the container)
type Resolve = std::sync::Arc<
    dyn Fn(&InjectionPoint, Arity) -> futures::future::BoxFuture<'static, Result<Published, ResolveError>>
        + Send + Sync>;
pub struct ObjectProvider<T: ?Sized> { resolve: Resolve, ip: InjectionPoint, _t: std::marker::PhantomData<fn() -> T> }
impl<T: Bean + ?Sized> ObjectProvider<T> {
    pub async fn get(&self) -> Result<Ref<T>, ResolveError>;                       // strict
    pub async fn get_if_available(&self) -> Result<Option<Ref<T>>, ResolveError>;  // None=absent, Err=ambiguous
    pub async fn get_if_unique(&self) -> Option<Ref<T>>;                           // both tolerated
}
// container back-ref captured by the closure is Weak<ContainerCore> — set on the container, never an Arc cycle.

// WASM host-proxy publishes the IDENTICAL ErasedBean — origin-agnostic by construction:
//   impl Provider for WasmHostProxy { fn provide(..) -> .. { Ok(Published::Shared(Arc::new(proxy) as ErasedBean)) } }

// concurrency-contract diagnostic rides the bound on the handle (doctrine-citing, not a raw trait error):
#[diagnostic::on_unimplemented(
  message = "shared-scope bean `{Self}` must be Send + Sync (shared across executor threads); \
             if it holds mutable per-interaction state, make it prototype- or request-scoped")]
pub trait ShareableBean: Send + Sync + 'static {}

// Env follows the same idiom — a cheap-clone shared handle, not a borrow:
pub struct Env(std::sync::Arc<EnvCore>);   // Clone + Send + Sync; crosses .await; lock-free reads after seal

// DEFERRED to Phase 3 (NOT the default, NOT the published currency): an opt-in singleton-only fast lane
//   #[derive(Clone, Copy)] pub struct BeanRef<T: ?Sized> { slot: u32, generation: u32, _t: PhantomData<fn()->T> }
//   arena.deref(beanref) -> Result<&T, StaleRef>;  // for tight steady-state loops only; never required.

## Forces on other concepts
- **Type-Erasure & Registry Substrate (register §2)**: The canonical erased handle IS ErasedBean = Arc<dyn Any+Send+Sync>; the registry is TypeId-keyed (const TypeId::of, 1.91) with a name overlay for aliases/collisions/name-keyed-Map injection. 'Injectable as any supertype' is macro-emitted: one dyn-Svc type-index upcast entry per declared injectable supertrait (trait upcasting 1.86). Concrete handles match by exact TypeId only. TypeId must never be serialized; cross-crate/WASM identity keys on author-assigned stable ids.
- **Async Execution & Context-Propagation (register §5a, §5b, §5d)**: Must supply an ambient store reference that survives .await (NOT ThreadLocal) for context-scope InstanceStores. The dyn Provider seam mandates the boxed-future standard (§5b) — accepted as the origin-agnostic default with a monomorphized escape for known-concrete sites. Teardown is container-driven async ledger drain (no async Drop, §5d); prototype registers no teardown (cancellation mid-build drops a half-built value cleanly).
- **Container Concurrency Contract (06 concurrency-contract)**: The Send+Sync+'static safe-publication bound physically RIDES ErasedBean/Bean in one leaf-core location — the contract is satisfied by the handle type, not re-expressed per feature. The atomic Arc clone is the happens-before edge; the borrow checker enforces 'guard your mutation'. The doctrine-citing diagnostic is owned here via #[diagnostic::on_unimplemented].
- **Bean Scopes & Scope SPI**: Scope is the three data axes (Multiplicity, StoreSource, TeardownPolicy) with the total map {Once,PerContextKey}->Shared / PerResolution->Owned. Singleton store is the container's HashMap<TypeId,ErasedBean>+name overlay; context scopes are per-context InstanceStores dropped at boundary. Custom scopes contribute a const triple via the opt-in/non-global discovery substrate; they never define a new handle type. Prototype's hand-off-and-forget is structural ABSENCE (no store entry, no ledger).
- **Deferral Primitives & Circular References**: Deferral handles hold a Resolve closure capturing Weak<ContainerCore> (no Arc cycle); re-resolution yields Published so the same handle returns Ref<T> for shared targets and owned T for prototype targets. Circular references are broken by removing the deferral-typed edge from the construction graph and resolving the fully-built target post-build — there is never an early erased handle to swap for a wrapped one; constructor cycles stay fatal at the plan/startup pass.
- **Cross-Crate Composition & Anti-DCE (register §7, owner-decision #1)**: Because ErasedBean is recognition (downcast) not validation, the mandatory expected-vs-found startup self-check is load-bearing: with maximal-magic discovery and no declared scan-scope, a silently-DCE'd crate must surface as a loud NoSuchBean naming the missing source rather than a silent absence. The handle being erased FORCES this self-check to exist.
- **Config / Environment & related shared handles (register §1, §14)**: All 'shared' config handles follow the keystone idiom: Env is a cheap-clone Arc-backed Send+Sync handle (read view), MessageSource results and cached values are Ref<T>/Arc-shared, parent-container links are Arc<dyn ParentView> (or Weak where a cycle would form). The keystone settles the §1 sub-questions for env-read-handle, message-return-type, cached-value-ownership, origin handles, and parent-link-lifetime by one consistent rule: shared = Arc handle, owned-once = move.
- **Metadata / Codegen Boundary & Container Shape (register §3, §9)**: The macro emits ONE thin const Descriptor + a Provider factory seed via absolute ::leaf_core paths; merge/publish/get logic lives in leaf-core (thin-macro §2.10). The Provider seam is origin-agnostic so FactoryBean-as-Provider, the engine-vs-façade split, and dynamic/WASM sources are all just Providers of Published — the container shape decision (§9) inherits a single registration surface and need not branch on origin.

## Open sub-questions (→ Phase 3)
- The opt-in singleton-only arena/BeanRef<T> fast lane: exact API (deref-with-generation-check, who mints the index, StaleRef diagnostic), whether it is worth shipping at all, and how it pairs with parent/child container hierarchies (a BeanRef is only meaningful with its owning arena). Phase 3, performance-driven, measured before built.
- Whether get_owned::<T>() for prototype and get::<T>() -> Ref<T> for shared are two methods or one method whose return is inferred from the injection-point's declared type by the macro (the register's injection-point-shapes addendum). Likely macro-inferred at injection sites, explicit methods at the programmatic escape hatch.
- Exact InstanceStore lifecycle for nested context scopes (request nested in session): how the ambient models nesting/parenting of stores, and the precise teardown-on-cancellation ordering so request/session destruction callbacks still run when in-flight work holds a straggler Arc clone past the boundary.
- The single-threaded-runtime ?Send carve-out for shared-scope beans: deferred to the §5/§6 pluggable-runtime decision; whether to offer a relaxed bound at all without re-introducing the sync/async bimodality charter §2.3 resists.
- WASM host-proxy teardown ownership and trap/failure semantics behind the Arc<dyn Trait> (wasm exploration open questions §88-89): how a stateful resource handle's destruction interacts with the container's TeardownLedger and shutdown ordering — feature-local to the WASM layer, but the handle's erased uniformity must not assume native-only teardown.
- The trait-upcast type-index entry emission detail: exactly which declared supertraits the macro emits dyn-Svc registry entries for (all injectable supertraits vs only those used at an injection point), and how that interacts with collection/Map injection over a supertype.
