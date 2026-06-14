# ADR 02 — Type-Erasure & Registry Substrate (origin-agnostic)  `[registry-substrate]`

> **Note (2026-06-14):** the optional *dynamic-WASM source / WASM-"classpath"* mentioned below is **DROPPED** — leaf autodiscovery is native link-time registration only. Ignore WASM/dynamic-module references here; they are struck from the active design.


## Decision
ADOPT a TWO-PHASE registry whose runtime value is the upstream `ErasedBean = Arc<dyn Any+Send+Sync>` (the ownership-model ADR's currency — this decision does NOT introduce a second handle) and whose KEYING is a dual TypeId-primary / name-overlay index built coherent ONCE at freeze over a dense `BeanId(u32)` slot space. Concretely, in leaf-core (the ultra-stable kernel ABI macros hard-code `::leaf_core` paths to):

```
// ── KEY SPACE ── one dense, append-only-then-frozen slot id; the SHARED join key both indices point at
#[derive(Clone, Copy, PartialEq, Eq, Hash)] pub struct BeanId(pub u32);   // dense 0..N over the frozen registry; bitset/Vec-indexable
pub type BeanName = std::sync::Arc<str>;                                  // interned at freeze; cheap-clone identity (not String churn)

// ── THE COMPOSITE RESOLUTION KEY ── what get_erased / candidate queries take (origin-agnostic)
pub enum BeanKey {
    ByType(std::any::TypeId),                 // primary lane: concrete TypeId OR a declared dyn-Svc TypeId row
    ByName(BeanName),                          // name lane: aliases, name-keyed Map injection, &FactoryBean deref, name-as-qualifier
    ByTypeAndName(std::any::TypeId, BeanName), // disambiguated single resolution
}

// ── DESCRIPTOR ── the const, origin-blind registry ROW one macro emits per bean via ::leaf_core paths (thin-macro §2.10)
pub struct Descriptor {
    pub contract: ContractId,                  // STABLE author-assigned identity (never a serialized TypeId) — see below
    pub self_type: std::any::TypeId,           // const TypeId::of (1.91); exact-match concrete key
    pub provides: &'static [TypeRow],          // dyn-Svc upcast rows (one per declared injectable supertrait, trait upcasting 1.86)
    pub declared_name: Option<&'static str>,   // explicit #[component(name=…)]; else NameGenerator-derived at freeze
    pub aliases: &'static [&'static str],
    pub scope: ScopeDef,                        // (Multiplicity, StoreSource, TeardownPolicy) from the ownership-model ADR
    pub meta: &'static AnnotationMetadata,      // flat const attr/marker tables (annotation-model ADR) — qualifiers/markers live here
    pub origin: Origin,                         // Native | DynamicWasm | TestDouble — DIAGNOSTIC ONLY; resolution never branches on it
}
pub struct TypeRow { pub view: std::any::TypeId, pub upcast: UpcastFn } // UpcastFn: fn(ErasedBean) -> ErasedBean (Arc<dyn Any>→Arc<dyn Svc as Any>)

// ── CONTRACTID ── the cross-crate / WASM / persisted identity. TypeId is NOT stable across compiler builds (nightly-doc §453),
//    so it is used ONLY as an in-process fast key; ContractId is the durable one the macro mints from author-assigned data.
#[derive(Clone, Copy, PartialEq, Eq, Hash)] pub struct ContractId(pub u64); // const FNV/FxHash of (crate-stable-name :: ident :: declared-name)
//    const { assert!(no collisions) } guard at freeze; WASM WIT-world contracts key on the SAME ContractId, never TypeId.

// ── FROZEN REGISTRY ── coherence built ONCE; the two indices and the store all key on BeanId, so they can never dangle
pub struct Registry {
    rows:        Box<[Descriptor]>,                                 // indexed by BeanId.0
    by_type:     std::collections::HashMap<std::any::TypeId, smallvec::SmallVec<[BeanId; 1]>>, // concrete + every emitted dyn-Svc row
    by_name:     indexmap::IndexMap<BeanName, BeanId>,              // canonical names, insertion-ordered (deterministic listing)
    aliases:     std::collections::HashMap<BeanName, BeanId>,       // alias → target slot
    by_contract: std::collections::HashMap<ContractId, BeanId>,     // stable-id lane (WASM/dynamic/persisted)
    providers:   Box<[std::sync::Arc<dyn Provider>]>,              // indexed by BeanId.0; native/WASM-proxy/test-double identical
}
```

REGISTRY VALUE = `ErasedBean` (singleton/scoped) or `Published::Owned` (prototype) — unchanged from the ownership-model ADR; this decision owns KEYING & STORAGE LAYOUT, not the handle. The singleton store is SLOT-INDEXED: `singletons: Box<[OnceCell<ErasedBean>]>` indexed by `BeanId.0` (lock-free read, at-most-once init by data shape — no global lock, charter §2.5); context-scope `InstanceStore`s are `HashMap<BeanId, ErasedBean>` keyed by the SAME slot id.

KEYING POLICY (settles register §2's name-vs-type tension): **TypeId-primary, name-overlay.** By-type single/collection resolution hits `by_type` directly (the common, zero-string path). The name overlay exists ONLY for the five Spring semantics that genuinely need a string identity — aliases, name-collision-as-loud-error, name-keyed `Map<String,T>` injection, `&FactoryBean` deref, and bean-name-as-implicit-qualifier — and these are folded onto the SAME `BeanId`, so the two schemes are coherent BY CONSTRUCTION (the freeze builds them together; there is no transactional multi-structure mutation to desync). The fixed selection ladder (primary→name-match→qualifier→@Priority→…) is NOT owned here — this substrate hands autowiring-resolution the candidate `SmallVec<BeanId>` + name/alias maps; `determine_winner` is its concern.

"INJECTABLE AS ANY SUPERTYPE" = macro-emitted upcast rows. For every declared injectable supertrait `dyn Svc`, the macro emits one `TypeRow { view: TypeId::of::<dyn Svc>(), upcast: |e| e.upcast_svc() }` using trait upcasting (1.86): a concrete bean's `ErasedBean` is registered under both its concrete `TypeId` (exact match) AND each `dyn Svc` view's `TypeId`. A concrete-struct injection matches ONLY the exact `TypeId`; a `dyn Svc` injection matches the upcast row. This is the unavoidable erasure-without-reflection asymmetry, made cheap (vtable adjust, no alloc) by 1.86.

NULLBEAN (settled WITH the substrate, register §2): present-but-null is a `Provider` that publishes `Published::Shared(NULL_BEAN)` where `NULL_BEAN: ErasedBean` is a leaf-core canonical `Arc<NullMarker>` singleton. It occupies a real `BeanId` slot (so it satisfies presence checks and shadows ancestors) and `get<T>` maps it to `Ok(None)`/typed absence at the typed boundary — NOT a side-table, NOT map-absence (which is reserved for NoSuchBean). This keeps "present-and-deliberately-empty" distinct from "absent/DCE'd."

ORIGIN-AGNOSTICISM is structural: `by_type`/`by_name`/`by_contract` store `BeanId`s; `providers[id]` is `Arc<dyn Provider>`; the registry has NO way to ask "is this native or WASM." The WASM host-proxy and a test double register an identical `Descriptor` + `Provider` and publish identical `ErasedBean`. The `Origin` field exists for diagnostics only and is never read on a resolution path.

## Rationale
- DX#1 + intent parity (charter §2.1, §2.2): Spring's BeanFactory IS name-keyed-with-a-type-index, and its felt DX (aliases, name-collision errors, name-keyed Map injection, &factory deref, field-name-as-implicit-qualifier) all REQUIRE a string identity. But Rust's idiomatic, zero-overhead lookup is by TypeId. Inverting Spring's default to TypeId-PRIMARY with a name-OVERLAY keeps the common typed path string-free (overhead #2) while preserving every name-dependent Spring semantic on the overlay — intent parity without mechanism parity. The 01 design names exactly these five name-needing semantics; folding them onto a shared BeanId is the minimum that satisfies all five.
- It is the only keying scheme the confirmed-stable envelope makes BOTH cheap AND coherent with zero nightly (charter §2.8). const TypeId::of (1.91) lets Descriptor rows embed their own type key as const data; Arc<dyn Any>+downcast (1.0/1.29) is the in-process value lookup; trait upcasting (1.86) makes the dyn-Svc rows free vtable adjusts. Verified in rust-nightly-features-tradeoffs.md §16/§81/§453. No part of this substrate needs a nightly feature.
- Freezing into a DENSE BeanId(u32) slot space DISSOLVES the name↔type coherence bug class that the 01 design flags as 'a latent bug class Spring gets implicitly from the JVM.' Because both indices and all three stores key on the SAME u32 slot, and the join is built ONCE at freeze (not maintained transactionally on every mutation), a type_index entry can never point at a removed/renamed def. This is the R2 'immutable snapshot builds coherence once' insight from 01 promoted to the substrate decision, and it simultaneously yields the dense bean-index space register §2 asks for (bitset candidate filtering, array-indexed singleton storage = the cheapest hot read).
- It is origin-agnostic BY CONSTRUCTION, satisfying the hard brief constraint and the WASM exploration's load-bearing move ('WASM-ness stops at the proxy', wasm §55): the indices store BeanIds, the store stores ErasedBean, providers are Arc<dyn Provider>. The registry literally cannot express 'where did this come from' on a resolution path — native, test-double, FactoryBean-product, and the directory-scanned WASM host-proxy are the same Descriptor+Provider+ErasedBean triple. The two-source duality (static link-time rows + dynamic WASM 'classpath') is invisible to the key.
- Separating ContractId (stable, author-assigned) from TypeId (in-process only) honors the hard constraint that TypeId is NOT stable across compiler builds (nightly-doc §453: 'never serialize a TypeId or rely on cross-build ordering'). The WASM contract identity, any persisted/cross-crate auto-config name, and the conditional/exclusion semver surface ALL key on ContractId; TypeId is a fast in-process accelerator only. This is what lets a WASM module compiled by a different toolchain bind to a native injection point — they agree on ContractId, never on TypeId.
- It composes cleanly with the already-decided neighbours rather than reopening them: the annotation-model ADR's flat const AnnotationMetadata (with TypeId/MarkerId-keyed attr tables) drops straight into Descriptor.meta; the ownership-model ADR's ErasedBean/Published is the unchanged value; the ScopeDef triple is carried verbatim. This substrate is the KEYING/STORAGE layer those decisions presupposed, supplied without contradicting any of them — the unification the charter §2.11 demands.
- It keeps the per-bean creation guard lock-free by data shape, not discipline (charter §2.5 no-global-lock): OnceCell-per-slot gives at-most-once init and lock-free ready reads intrinsically, the §2.4 reactive hot-read path, with first-creation serialization scoped to the single slot — exactly what enables background-bootstrap and wave-parallel eager init without a global registry mutex.

## Options considered & rejected
- **Name-keyed PRIMARY with a TypeId index layered on (Spring's literal model)** — rejected: Faithful to Spring's internals but pessimal for Rust DX#2/overhead: every by-type injection (the overwhelmingly common case in idiomatic Rust DI) would route through a String/Arc<str> name lookup and a second hop to the type index, paying string hashing on the hot resolution path for a semantic (name identity) that only ~5 features actually need. It also makes the name the join key, so a generated/synthetic name must exist for every anonymous bean and name-collision handling becomes load-bearing for ALL beans rather than only named ones. TypeId-primary inverts this to pay the string cost ONLY where name semantics are genuinely invoked, while still carrying the full name overlay — strictly better overhead at equal Spring-semantic coverage.
- **TypeId-keyed ONLY, drop the name layer entirely (pure-Rust minimalism)** — rejected: Breaks five concrete, named Spring semantics the 01 design enumerates: aliases, name-collision-as-loud-error (allow-override), name-keyed Map<String,T> injection (collection-injection), the & FactoryBean dereference (factory-bean), and bean-name-as-implicit-qualifier (autowiring step 5, where 'renaming a field silently changes which bean injects' is intended coupling). These are intent-parity requirements, not optional sugar. A TypeId-only registry would force each of these into a bolt-on side-channel — MORE total mechanism than carrying one coherent name overlay built at freeze. DX#1 forbids dropping them.
- **Mutable concurrent map registry (DashMap<TypeId,…> + DashMap<BeanName,…>) maintained transactionally, never frozen** — rejected: This is 01's R1/R3 realization. It leans on a concurrency-map crate (Arc-heavy, against §2.5's heap/Arc-churn caution) AND, fatally for THIS substrate, requires every mutation path (register/override/alias) to update name map + type index + alias map + store transactionally — the exact 'two keying schemes must stay coherent' bug class the JVM hides from Spring and Rust does not. The dense-slot freeze model gets the same dynamic-registration capability via an explicit append-then-freeze (and a dynamic second-epoch lane for WASM) while making mid-life desync UNREPRESENTABLE. Mutable-during-steady-state coherence is a latent bug we decline to own.
- **Arena + generation-index (BeanRef<T>) as the registry value AND key (skip TypeId/Any entirely)** — rejected: Already rejected as the PRIMARY handle by the upstream ownership-model ADR (it can't express request-scoped or WASM-proxy origins and gives no soundness for scoped lifetimes). As a KEY it also fails: a bare slot index is meaningless across container hierarchies (a parent's BeanId is not a child's) and cannot be the cross-crate/WASM identity (no stable meaning outside its arena). We DO adopt a dense BeanId for in-process candidate bitsets and slot-indexed storage — but it is an internal, per-registry, frozen index, NOT the public key and NOT the cross-crate identity (that is ContractId). The opt-in BeanRef fast-lane stays deferred to Phase 3 exactly as the ownership ADR placed it.
- **One physical Box<dyn Any>+TypeId store with NO name overlay and name resolution done by linear scan of descriptors** — rejected: Avoids the second index but makes name-keyed Map injection, alias lookup, and &-deref O(N) over all beans on every name resolution — and makes name-collision detection a full scan at registration. The IndexMap<BeanName,BeanId> overlay is O(1), insertion-ordered (deterministic listing for collection injection), and built once. The scan 'saves' one HashMap at the cost of turning every name operation into a linear pass — wrong altitude for a kernel substrate.

## Consequences
- FORCES the freeze/seal boundary (compile-vs-runtime split, register §4) to be the moment the dense BeanId space + both indices are materialized: descriptors are appended during the cold definition phase (synchronous, single-threaded, around main — they are const data, no pre-main code), then SEALED into the immutable `Registry` (Box<[…]> + frozen maps). Post-freeze edits are a typestate/diagnostic error. A DYNAMIC second epoch (the WASM directory-scan lane) appends new BeanIds in a controlled re-freeze, NOT mid-steady-state mutation — the freeze model must expose this second-epoch seam to the cross-crate/WASM concept.
- FORCES the anti-DCE expected-vs-found self-check to be LOAD-BEARING (cross-crate §152, owner-decision #1): because the registry is RECOGNITION (downcast/TypeId match) not VALIDATION, a silently-DCE'd crate produces an ABSENT BeanId, indistinguishable at lookup from a genuine NoSuchBean. With maximal-magic discovery and no declared scan-scope, the link-collected Descriptor set MUST be cross-checked against a build-time/app-root expected-crate manifest so a dropped crate surfaces as a loud NoSuchBean naming the missing source. The dense-index freeze is exactly where 'expected count vs found count' is computed.
- FORCES the metadata/codegen boundary (register §3) to emit a flat const Descriptor (self TypeId via const TypeId::of, ContractId via const-hash, provides[] upcast rows, meta tables) through absolute ::leaf_core paths, with ZERO merge/index/freeze logic in the macro — all of that lives in leaf-core (thin-macro §2.10). The macro's only non-trivial job is emitting one TypeRow + UpcastFn per declared injectable supertrait.
- FORCES autowiring-resolution / candidate-resolution to consume `SmallVec<BeanId>` candidate sets and the name/alias overlay, and to own the fixed determine_winner ladder + bitset filtering over the dense index — this substrate deliberately STOPS at 'here are the matching BeanIds and the name keys'; it does not pick winners. The dense BeanId space is the substrate's gift that makes candidate bitsets and qualifier-filter masks cheap.
- FORCES every cross-crate / WASM / persisted identity to key on ContractId, never TypeId (the substrate inherits and propagates the TypeId-instability constraint, register §2's last line). WASM WIT-world contracts, auto-config exclusion names, and conditional ContractId references all bind on the stable id; the const-hash collision guard at freeze must be present or two beans could alias one ContractId silently.
- FORCES the 'injectable as any supertype' surface onto trait upcasting (1.86) with a known asymmetry: a concrete-struct handle matches by exact TypeId only, while a dyn Svc view must be EXPLICITLY published as a TypeRow (the macro emits one upcast entry per declared injectable supertrait). This is inherent to erasure-without-reflection and composes at zero extra alloc with 1.86, but it means an undeclared supertrait is NOT injectable — the macro's supertrait-emission policy (all declared vs only-used) is a real Phase-3 detail.
- SETTLES the NullBean question inside the substrate: present-but-null is a canonical NULL_BEAN ErasedBean occupying a real BeanId slot (so presence checks and ancestor-shadowing work), mapped to typed absence at get<T>. This keeps 'present-and-deliberately-empty' (NullBean) crisply distinct from 'absent/NoSuchBean' (map-miss) and from 'condition-not-met' (no slot ever minted) — three states the diagnostics model (register §6) must keep separate.
- ACCEPTS a named, bounded overhead (charter §2.5, confronted): each by-type resolution is a HashMap<TypeId,…> probe + an Arc refcount bump + a downcast (TypeId compare + vtable check); each name resolution adds an Arc<str>-keyed probe. Mitigated by: (a) constructor-injected collaborators resolved once at wiring and stored as typed Ref<T> fields (steady-state touches neither map nor downcast); (b) slot-indexed singleton store (singletons[id]) = bounds-checked array read, no hash, on the ready path; (c) the deferred Phase-3 BeanRef fast-lane for hot leaf-internal infra. We do NOT make any of these the default key.
- ACCEPTS that the registry is ORIGIN-BLIND on the resolution path by design, which means a misbehaving WASM provider is diagnosable only via the Origin field (diagnostic-only) and the Provider's own trap/error surface — the substrate intentionally refuses to special-case origin, pushing WASM failure semantics entirely into the Provider impl and the TeardownLedger (a WASM-layer Phase-3 concern).

## API / type sketch

// ── leaf-core: the KEYING & STORAGE substrate over the ownership-model ADR's ErasedBean ──

// dense, frozen slot id — the SHARED join key; bitset/Vec indexable; per-registry, NOT cross-crate
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] pub struct BeanId(pub u32);
pub type BeanName = std::sync::Arc<str>;                       // interned at freeze; cheap-clone

// stable author-assigned identity — the CROSS-CRATE / WASM / persisted key (TypeId is in-process only)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] pub struct ContractId(pub u64); // const FNV/FxHash, collision-guarded at freeze

// composite resolution key (origin-agnostic; the dynamic/WASM lane takes this)
pub enum BeanKey {
    ByType(std::any::TypeId),
    ByName(BeanName),
    ByContract(ContractId),                                    // dynamic/WASM/cross-crate
    ByTypeAndName(std::any::TypeId, BeanName),
}

// upcast row: register the concrete bean under each declared dyn-Svc view (trait upcasting 1.86)
pub type UpcastFn = fn(crate::ErasedBean) -> crate::ErasedBean; // Arc<dyn Any> as concrete → Arc<dyn Svc> as Any
pub struct TypeRow { pub view: std::any::TypeId, pub upcast: UpcastFn }

// the const, ORIGIN-BLIND row one thin macro emits per bean via ::leaf_core paths
pub struct Descriptor {
    pub contract:      ContractId,
    pub self_type:     std::any::TypeId,                        // const TypeId::of (1.91) — exact-match concrete key
    pub provides:      &'static [TypeRow],                      // injectable supertrait views
    pub declared_name: Option<&'static str>,
    pub aliases:       &'static [&'static str],
    pub scope:         crate::ScopeDef,                         // ownership-model ADR triple
    pub meta:          &'static crate::AnnotationMetadata,      // annotation-model ADR const tables (qualifiers/markers)
    pub origin:        Origin,                                  // DIAGNOSTIC ONLY
}
pub enum Origin { Native, DynamicWasm, TestDouble }

// the FROZEN registry — coherence built once over BeanId; nothing can dangle
pub struct Registry {
    rows:        Box<[Descriptor]>,                                          // [BeanId.0]
    by_type:     std::collections::HashMap<std::any::TypeId, smallvec::SmallVec<[BeanId; 1]>>,
    by_name:     indexmap::IndexMap<BeanName, BeanId>,                       // insertion-ordered = deterministic listing
    aliases:     std::collections::HashMap<BeanName, BeanId>,
    by_contract: std::collections::HashMap<ContractId, BeanId>,
    providers:   Box<[std::sync::Arc<dyn crate::Provider>]>,                // [BeanId.0]; native/WASM-proxy/test-double identical
    singletons:  Box<[once_cell::sync::OnceCell<crate::ErasedBean>]>,       // [BeanId.0]; lock-free ready read, at-most-once init
}
impl Registry {
    pub fn candidates(&self, ty: std::any::TypeId) -> &[BeanId];            // concrete + dyn-Svc rows; autowiring picks the winner
    pub fn resolve_id(&self, key: &BeanKey) -> Result<BeanId, ResolveError>;// name/alias/contract → slot (coherent by construction)
    pub fn descriptor(&self, id: BeanId) -> &Descriptor;
    pub fn provider(&self, id: BeanId) -> &std::sync::Arc<dyn crate::Provider>;
}

// COLD-PHASE builder (synchronous, around main) → seal into the immutable Registry; a DYNAMIC second epoch re-freezes for WASM
pub struct RegistryBuilder { /* append-only descriptors+providers */ }
impl RegistryBuilder {
    pub fn register(&mut self, d: Descriptor, p: std::sync::Arc<dyn crate::Provider>) -> Result<BeanId, RegisterError>; // dup-name/contract = loud error
    pub fn freeze(self) -> Result<Registry, AssemblyError>;   // builds dense index, runs ContractId collision guard + anti-DCE expected-vs-found self-check
    pub fn reopen_epoch(reg: Registry) -> Self;               // dynamic/WASM second-epoch append → re-freeze
}

// NULLBEAN — present-but-null occupies a real slot; get<T> maps to typed absence (NOT map-miss = NoSuchBean)
pub struct NullMarker;
pub static NULL_BEAN: once_cell::sync::Lazy<crate::ErasedBean> =
    once_cell::sync::Lazy::new(|| std::sync::Arc::new(NullMarker) as crate::ErasedBean);

// ── DEFERRED to Phase 3 (NOT the default key) ── the opt-in singleton-only fast lane from the ownership ADR:
//    BeanRef<T> { slot: u32, generation: u32 } deref'd against a per-registry arena; never the public key, never cross-crate.

## Forces on other concepts
- **Scope / Lifetime / Ownership Model (upstream ownership-model ADR)**: Honored unchanged: the registry VALUE is the ADR's ErasedBean (singleton/scoped) / Published::Owned (prototype); this decision adds NO second handle. The singleton store is slot-indexed OnceCell<ErasedBean>[BeanId.0]; context-scope InstanceStores key on the SAME BeanId. ScopeDef rides in Descriptor verbatim. The deferred BeanRef fast-lane stays Phase-3, never the key.
- **Metadata / Codegen Boundary & Container Shape (register §3, §9)**: The macro emits ONE flat const Descriptor (self_type via const TypeId::of, ContractId via const-hash, one TypeRow+UpcastFn per declared injectable supertrait, AnnotationMetadata reference) through absolute ::leaf_core paths; ALL index-building/merge/freeze logic lives in leaf-core. No reflection, no per-bean builder calls beyond the const row. The Provider seam is origin-agnostic so FactoryBean-as-Provider and WASM/dynamic sources are just more rows.
- **Annotation / Merged-Annotation Model (annotation-model ADR)**: Its flat const AnnotationMetadata (TypeId/MarkerId-keyed attr+presence tables) IS Descriptor.meta. Qualifiers and custom-qualifier markers are read from meta during candidate matching; the MarkerId/AttributeEntry ABI must stay ultra-stable (emitted code hard-codes paths). Trait-upcasting is used ONLY for the registry's 'injectable as supertype' coercion (the TypeRow upcast), not as the whole annotation model — exactly as that ADR recommended.
- **Autowiring / Candidate Resolution & Collection Injection**: Consumes candidates(ty) -> &[BeanId] plus the name/alias overlay; OWNS the fixed determine_winner ladder (primary→name→qualifier→@Priority→default→resolvableDependency) and bitset filtering over the dense BeanId space. resolvableDependencies infrastructure handles are stored as the SAME ErasedBean under synthetic BeanIds. Collection injection gets the full unfiltered candidate set + insertion-ordered names; primary/fallback collapse is NOT applied to collections.
- **Bean Naming, Aliases & Collisions**: BeanName is Arc<str> interned at freeze (cheap-clone identity, not String churn). Name-collision is a loud RegisterError at register()/freeze(); aliases resolve to the target BeanId. NameGenerator policy (declared name vs derived) is applied during the cold phase before freeze. Cross-registry (hierarchy) name identity is per-registry — a parent BeanId is not a child BeanId; hierarchy delegation is by BeanKey, not by raw slot.
- **Cross-Crate Composition & Anti-DCE (register §7, owner-decision #1)**: freeze() MUST run the expected-vs-found self-check: the link-collected Descriptor set is cross-checked against an app-root/build-time expected-crate manifest so a DCE-dropped crate surfaces as a loud NoSuchBean naming the missing source, NOT a silent absent BeanId. ContractId (author-assigned, collision-guarded) is the stable semver/exclusion/WASM identity; TypeId is in-process only and must never be serialized.
- **WASM Optional Layer / Two-Source Duality (wasm exploration)**: The host-proxy registers an identical Descriptor (origin: DynamicWasm, diagnostic-only) + Arc<dyn Provider> publishing the identical ErasedBean — 'WASM-ness stops at the proxy.' The directory-scanned dynamic 'classpath' enters via reopen_epoch()→re-freeze, appending new BeanIds in a controlled second epoch (never mid-steady-state). WASM contracts key on ContractId, never TypeId. WASM provider trap/teardown semantics are pushed into the Provider impl + TeardownLedger (Phase-3, WASM-local).
- **Compile-vs-Runtime Resolution Split & Container Refresh Lifecycle**: The freeze boundary is where the dense BeanId space + both indices materialize (immutable snapshot = coherence-once, safe-publication-for-free across tasks). Descriptors append during the synchronous cold definition phase; post-freeze edits are typestate/diagnostic errors; the dynamic/WASM second epoch is the sanctioned re-freeze seam. This forces refresh-lifecycle to expose a definition-phase → seal → (optional dynamic epoch) → steady-state progression.
- **FactoryBean Indirection**: A FactoryBean is just another Provider; its product is findable pre-construction because the macro emits the produced type's TypeId as a provides[] row pointing at the factory's BeanId. The &FactoryBean dereference is a ByName lookup with a deref flag resolved on the name overlay (the factory itself vs its product). No special-case storage — one BeanId, two type-index entries (factory type + product type).

## Open sub-questions (→ Phase 3)
- The macro's supertrait-emission policy for TypeRows: emit an upcast row for EVERY declared injectable supertrait, or only those actually used at an injection point (which the macro can't see cross-crate)? And how that set interacts with collection/Map injection over a supertype. Likely 'all declared injectable supertraits' for cross-crate safety, measured for row-count bloat. Phase 3.
- Exact ContractId derivation inputs and collision policy: which 'crate-stable name' feeds the const-hash (Cargo package name + module path + ident + declared-name?), how renaming/re-exporting affects it as a semver surface, and whether the freeze-time collision guard hard-errors or salts. Phase 3, co-decided with auto-config exclusion identity.
- The dynamic second-epoch re-freeze mechanics for the WASM lane: whether new BeanIds are appended to the existing dense space (preserving prior slots) or a fresh snapshot is built, how in-flight resolutions against the old snapshot are reconciled, and the ordering/precedence of dynamic rows vs static. Feature-local to the WASM/dynamic-source layer.
- NullBean's exact typed-boundary mapping: how get<T>() distinguishes NULL_BEAN→Ok(None) for an Option<Ref<T>> injection point vs a hard error for a mandatory Ref<T> point, and whether NULL_BEAN participates in collection injection counts. Couples to the injection-styles arity model. Phase 3.
- Hierarchy BeanId/BeanKey delegation precision: 'lowest-level-with-candidates wins' and local-shadows-parent must be encoded as an explicit BeanKey walk policy across per-registry snapshots (no global type graph). Exact tie-break when a local non-unique set faces a unique parent match. Phase 3, co-decided with container-hierarchy.
- Whether by_type should additionally carry a precomputed candidate bitset per common query (eager) vs computing SmallVec<BeanId> on demand (lazy), and where the dense-index bitset for qualifier-filter masks is materialized — a freeze-time vs first-resolution performance call, measured before built. Phase 3.
