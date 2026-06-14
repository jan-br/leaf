# Nightly Rust Features for leaf — Tradeoffs & Recommendations

> Standing engineering reference for the leaf framework (async-first DI/IoC container, Spring-Boot-intent, stable-by-default). Verified against **stable 1.96.0 (2026-05-28)** and **nightly 1.98.0**, snapshot date **2026-06-14**. The governing invariant: *stable by default; adopt a nightly feature only where it delivers a marked, justified win, and always know the stable fallback.*

## TL;DR — Recommendation Table

### Adopt now (stable, or stable-with-edition/MSRV)

| Feature | Gate | Status | leaf use | Recommendation |
|---|---|---|---|---|
| Trait upcasting (dyn upcasting coercion) | (stable, 1.86) | recently-stabilized | Upcast `Arc<dyn Bean>` → supertrait service objects in the type registry; coerce to `dyn Any` for downcast | **adopt** |
| RPITIT (return-position impl Trait in traits) | (stable, 1.75) | recently-stabilized | Zero-cost opaque futures on bean/lifecycle traits (static-dispatch path) | **adopt** |
| AFIT (async fn in traits) | (stable, 1.75) | recently-stabilized | Ergonomic `async fn init/post_process` on bean traits | **adopt** |
| Async closures (`AsyncFn*`) | (stable, 1.85) | recently-stabilized | Async bean factories / provider closures | **adopt** |
| General const-eval baseline | (stable) | recently-stabilized | Compile-time keys, metadata structs, build-fail validation | **adopt** |
| `const TypeId::of` | (stable, 1.91) | recently-stabilized | Const-promoted registry rows embedding their own type key | **adopt** |
| `Any` + downcast | (stable, 1.0+) | recently-stabilized | Runtime backbone: `HashMap<TypeId, Arc<dyn Any + Send + Sync>>` | **adopt** |
| `type_name` / `type_name_of_val` | (stable) | recently-stabilized | Human-facing container diagnostics only | **adopt** |
| `#[used]` (plain) | (stable, 1.30) | recently-stabilized | Object-file retention substrate under linkme/inventory | **adopt** |
| `#[diagnostic::on_unimplemented]` | (stable, 1.78) | recently-stabilized | Branded "cannot inject" trait-bound errors | **adopt** |
| `#[diagnostic::do_not_recommend]` | (stable, 1.85) | recently-stabilized | Suppress blanket-impl noise in wiring errors | **adopt** |
| `proc_macro::Span` location APIs | (stable, 1.88) | recently-stabilized | Embed declaration-site provenance in metadata | **adopt** |
| `inventory` (submit!/iter) | (stable crate) | stable | Primary DCE-robust, wasm-capable component registry | **adopt** |

### Adopt behind a feature flag / with caveats

| Feature | Gate | Status | leaf use | Recommendation |
|---|---|---|---|---|
| `#[link_section]` | (stable) | recently-stabilized | Distributed-slice substrate (via linkme, not hand-rolled) | **behind-flag** (use linkme) |
| `linkme` (distributed slices) | (stable crate) | stable | Optional zero-overhead link-time registry backend | **behind-flag** |
| `let_chains` | (stable, 1.88, edition 2024) | recently-stabilized | Flatter conditionals in runtime/generated code | **behind-flag** (edition-gated) |
| `if_let_guard` | (stable, 1.95, all editions) | recently-stabilized | Fallible secondary lookups in match arms | **behind-flag** (MSRV 1.95) |

### Watch (right long-term answer; not yet shippable)

| Feature | Gate | Status | leaf use | Recommendation |
|---|---|---|---|---|
| ATPIT (impl Trait in assoc type) | `impl_trait_in_assoc_type` | stalled-nightly | Store bean futures un-boxed as named assoc type | **watch** |
| TAIT (type alias impl Trait) | `type_alias_impl_trait` | stalled-nightly | Shared opaque type across generated container code | **watch** |
| Return-type notation (RTN) | `return_type_notation` | stalled-nightly (PR closed unmerged) | `where B::init(..): Send` for spawning bean futures | **watch** |
| `adt_const_params` (+`unsized_const_params`) | `adt_const_params` | experimental-incomplete | Enum/string type-level bean keys | **watch** |
| const traits | `const_trait_impl` | progressing-nightly | Generic compile-time construction/validation | **watch** |
| `derive(CoercePointee)` | `derive_coerce_pointee` | stalled-nightly | Branded custom smart-pointer bean handles | **watch** |
| `never_type` (`!` in type position) | `never_type` | progressing-nightly (FCP 2026-06-11, not merged) | Infallible provider error types | **watch** |
| `gen` blocks / `async gen` | `gen_blocks` | progressing-nightly | Ergonomic event-stream producers | **watch** |
| `AsyncIterator` (std) | `async_iterator` | experimental-incomplete | std-blessed event streams | **watch** |

### Avoid (instability not worth it; stable alternative wins)

| Feature | Gate | Status | leaf use | Recommendation |
|---|---|---|---|---|
| `specialization` (full) | `specialization` | stalled-nightly, UNSOUND | Default bean behavior overridable per type | **avoid** |
| `min_specialization` | `min_specialization` | stalled-nightly | Sound default-override (but rustc-internal extensions closed to users) | **avoid** |
| `negative_impls` | `negative_impls` | stalled-nightly | "Not a bean" opt-out | **avoid** |
| `auto_traits` | `auto_traits` | stalled-nightly (perma-unstable) | Structural `Injectable` propagation | **avoid** |
| `generic_const_exprs` | `generic_const_exprs` | stalled-nightly, incomplete/unsound | Const-computed table sizes | **avoid** |
| `const_cmp_type_id` | `const_cmp_type_id` | stalled-nightly | Const-time type-equality dispatch | **avoid** |
| Non-`'static` TypeId | `non_static_type_id` | removed/retracted | Key borrow-holding services | **avoid** |
| `allocator_api` | `allocator_api` | stalled-nightly (PR reverted to draft 2026-06-01) | Arena storage for scoped beans | **avoid** (use allocator-api2/bumpalo) |
| `ptr_metadata` | `ptr_metadata` | stalled-nightly (blocked on Sized-Hierarchy) | Custom thin/fat bean pointers | **avoid** |
| CoerceUnsized / Unsize / DispatchFromDyn | `coerce_unsized`, `unsize`, `dispatch_from_dyn` | stalled-nightly | Custom dyn-coercible handles | **avoid** |
| `dyn*` (dyn-star) | — | **REMOVED from compiler (1.90)** | Inline type-erased handles | **avoid (gone)** |
| Async Drop (`AsyncDrop`) | `async_drop` | experimental-incomplete (tokio-shutdown crashes) | Async `@PreDestroy` | **avoid** |
| Coroutines / generators | `coroutines` | stalled-nightly (no path) | Hand-rolled state machines | **avoid** |
| `#[used(linker)]` / `#[used(compiler)]` | `used_with_arg` | stalled-nightly (4y design concerns) | DCE-safe registration | **avoid** |
| `#[no_mangle]` (for registration) | (stable) | recently-stabilized | (wrong tool for mass registration) | **avoid** (for this use) |
| `ctor` (raw life-before-main) | (stable crate) | stable | Raw pre-main hooks | **avoid** (use inventory) |
| `try_blocks` | `try_blocks` | stalled-nightly | Scoped `?` in lifecycle code | **avoid** |
| `proc_macro::Diagnostic` emit API | `proc_macro_diagnostic` | stalled-nightly | Multi-span macro diagnostics | **avoid** |
| Provider / Request (`error_generic_member_access`) | `error_generic_member_access` | stalled-nightly (FCP cancelled 2026-02-26) | Typed error context | **avoid** |

---

## Already stable (no nightly needed)

Read this section first. It is the single most important takeaway: **leaf can be a fully async-first, codegen-heavy, link-time-registered DI framework on 100% stable Rust today.** Do not gate on nightly for any of the following — they have shipped and are load-bearing for leaf.

- **Async trait surface — stable since 1.75 (Dec 2023):** `RPITIT` and `AFIT` give zero-cost opaque futures on bean/lifecycle/post-processor traits for the static-dispatch path; precise capturing `use<>` (1.82) refines lifetime capture. This is leaf's async-first baseline.
- **Async factories — stable since 1.85 (Feb 2025):** `async |ctx| { ... }` closures (`AsyncFn`/`AsyncFnMut`/`AsyncFnOnce`, now in prelude) directly model the async bean factory primitive.
- **Trait upcasting — stable since 1.86 (Apr 2025):** `Arc<dyn Bean>` → `Arc<dyn Service>` is a cheap vtable adjustment. This is the unambiguous win for the bean type registry: register once, satisfy queries for any declared supertrait. Pair with `Any` for concrete-type downcast. ("Drop the principal of trait objects," stable 1.84, eases auto-trait-only erasure.)
- **Type identity as keys — `const TypeId::of` stable since 1.91 (Oct 2025):** registry rows can be fully-const `static`s embedding `TypeId::of::<T>()`. Runtime resolution rides `Any` + `downcast` over `HashMap<TypeId, Arc<dyn Any + Send + Sync>>` (stable since 1.0, with `Arc<dyn Any>::downcast` since 1.29).
- **Const-eval baseline — stable and growing:** `const fn` with loops/branches/mutable locals, inline `const { ... }` blocks (1.79), compile-time `assert!`-based validation, and basic const generics (integer/char/bool). Enough for compile-time keys (u64 hashes), const metadata structs, and build-fail config validation.
- **Link-time registration — stable since 1.30 (2018):** `#[used]`, `#[link_section]`, `#[no_mangle]` underpin both `linkme` (MSRV 1.71) and `inventory` (MSRV 1.68), which are stable crates. Component auto-discovery needs **no nightly**.
- **Diagnostics & DX — stable:** `#[diagnostic::on_unimplemented]` (1.78) and `#[diagnostic::do_not_recommend]` (1.85) deliver branded, focused compile errors; `proc_macro::Span` location APIs `start/end/line/column/file/local_file` (1.88) and `source_text()` give precise macro diagnostics and source provenance. `if_let_guard` (1.95, all editions) and `let_chains` (1.88, edition 2024) are ergonomic sugar.

One language primitive leaf genuinely wants is **not** stable and has **no** safe substitute at the language level: **async Drop**. Treat its absence as a first-class design constraint (explicit container-driven `shutdown()`), not a gap to be filled by nightly.

---

## SPECIALIZATION

Bottom line: **avoid both** in any shipping build. The entire value proposition (zero-cost type-keyed selection, default-impl override per bean type, optional-capability detection) is already achievable on stable through leaf's committed codegen + linkme/inventory stack. Specialization buys leaf nothing it cannot already do at codegen time, and its instability directly violates the "predictable for end users" constraint.

### specialization (full)

- **What it is:** RFC 1210. A blanket `impl<T> Trait for T { default fn .. }` whose `default` items can be overridden by `impl Trait for ConcreteType { fn .. }`; also `default impl` blocks and (partial) specializable associated types. Resolution governed by a specialization lattice.
- **Status:** stalled-nightly. On 1.98.0-nightly the compiler prints *"the feature `specialization` is incomplete and may not be safe to use and/or cause compiler crashes."* Stable 1.96.0 rejects with E0554. Tracking issue #31844 open since 2016, locked, labeled I-unsound + S-tracking-design-concerns + S-tracking-needs-deep-research; last meaningful update 2026-04-21. 2024–2026 traffic is ICE reports (#152405, #150751, #150857, #132519, #156484), not design progress.
- **What it enables for leaf:** a default `impl<B> Lifecycle for B` overridable per concrete bean; default `impl<B> Qualifier for B`; specializable assoc types for per-bean wiring; generic container code picking an optimized path for beans with an optional capability.
- **Pros:** most direct expression of "default, override per type" at zero runtime cost (resolved at monomorphization); works inside generic contexts (unlike the autoref trick); eliminates macro-generated-impl boilerplate.
- **Cons:** officially **UNSOUND** (UB in safe code) — the classic `&'static str` vs `&'a str` dispatch hole, exactly the lifetime/scope distinctions a DI container hits; compiler-flagged crash-prone; `default` associated-type projections break inference; recurring ICEs with RPITIT/coherence; nightly with no timeline and effectively abandoned design work — pins leaf to nightly forever.
- **Stable alternative + cost:** proc-macro/build.rs emits the exact concrete `impl Trait for ConcreteBean` per bean — literally what specialization would produce at monomorphization, but sound, stable, and inspectable. Cost: more generated code; leaf owns the lattice logic in the macro. (Plus the autoref/autoderef trick for macro call sites, and TypeId-keyed dyn dispatch tables via inventory/linkme.)
- **Recommendation: avoid.** Unsound + crash-flagged + abandoned design = disqualifying for a stability-first framework. Not needed.

### min_specialization

- **What it is:** the sound subset std uses internally (Vec/Extend, ToString, FusedIterator). Restricts specializing impls to those "always applicable" regardless of lifetime choice, enforced by four impl-wf checks.
- **Status:** stalled-nightly. On 1.98.0-nightly it compiles **without** the incomplete/crash warning (confirming it is treated as sound); stable 1.96.0 still rejects with E0554. Shares #31844; introduced via PR #68970. No standalone stabilization push — it exists as a std-internal tool, not a path being driven to stable for third parties.
- **What it enables for leaf:** a sound, zero-cost default-impl-override where the override is "always applicable" (no lifetime/`'static` dependence). Optional-capability fast paths *would* need `#[rustc_specialization_trait]`.
- **Pros:** sound, battle-tested in std, same zero cost as full specialization for allowed cases.
- **Cons:** still nightly with no stabilization timeline for external use — violates stable-by-default just as much for shipping; its useful extensions (`#[rustc_specialization_trait]`, `#[rustc_unsafe_specialization_marker]`) are **rustc-internal attributes off-limits to user crates**, so the optional-capability angle is effectively closed to leaf; cannot specialize on lifetimes/`'static`; sparse docs (semantics defined by compiler source).
- **Stable alternative + cost:** identical to full specialization — proc-macro-generated per-type impls (sound, stable), autoref trick at macro call sites, inventory/linkme dispatch tables. Cost: codegen volume.
- **Recommendation: avoid.** Even the sound variant pins leaf to nightly with no third-party path, and its leverage points are closed.

---

## TRAIT-SYSTEM FEATURES

### Trait upcasting (dyn upcasting coercion)

- **What it is:** coerce a trait object to a declared supertrait object through any pointer-like type (`&dyn`, `Box`, `Arc`, `Rc`, `*const`). The subtrait vtable stores supertrait vtable pointers, so the coercion is a cheap pointer/vtable adjustment.
- **Status:** **recently-stabilized in 1.86.0 (2025-04-03)** via PR #134367; tracking #65991 closed. An earlier attempt (#118133) was reverted over soundness (#120248), fixed before 1.86. Solid on 1.96.0.
- **What it enables for leaf:** the type-registry/bean-lookup model — store `Arc<dyn Bean>`, upcast to `Arc<dyn MyService>` on lookup; combine with `Any` for downcast. This is exactly Spring's "a bean is injectable as any of its supertypes."
- **Pros:** fully stable; low overhead (vtable-pointer adjustment, no alloc); removes per-trait-view boilerplate; composes with `Arc` (the natural singleton type).
- **Cons:** requires the supertrait relationship to be declared (leaf's macros must arrange service traits `: Bean`); only declared supertraits (unrelated-trait queries still need `Any` + registry); raw-pointer (`*const dyn`) upcasting has an unresolved vtable-validity UB caveat.
- **Stable alternative + cost:** it *is* the stable path. Pre-1.86 fallbacks (generated `as_supertrait` methods, or per-trait-view registration) are now strictly worse.
- **Recommendation: adopt.** Architect the registry around a supertrait hierarchy (every service trait `: Bean`, store `Arc<dyn Bean>`, upcast on lookup). MSRV 1.86 is cheap given stable is 1.96. **Avoid `*const dyn` upcasts in unsafe internals.**

### Return-type notation (RTN)

- **What it is:** lets callers bound the anonymous future returned by an `async fn`/`-> impl Trait` trait method: `where T::method(..): Send` or `T: Trait<method(..): Send>`. The intended native fix for the Send-bound problem.
- **Status:** stalled-nightly. RFC 3654 approved; lang team cleared the last concerns 2025-04-02; but stabilization PR **#138424 was CLOSED UNMERGED on 2025-12-27** when its sole author (compiler-errors) left the project. Tracking #109417 still open, implementation-incomplete. Functional on nightly; ownerless. Narrow even on nightly: lifetime-generic methods only (no type/const generics), outermost-impl-Trait, bound-position only.
- **What it enables for leaf:** the cleanest expression of the cross-container Send requirement — `where B::init(..): Send` at the spawn site so bean futures can run on tokio's work-stealing executor, without forcing every bean trait method to be `Send` and without a dual-trait split.
- **Pros:** exactly targets leaf's Send-across-the-container need; keeps generated traits as idiomatic `async fn`; no over-constraint; RFC-approved so semantics are stable-if-it-lands.
- **Cons:** stabilization ownerless with unknown (possibly long) timeline; nightly conflicts with stable-by-default; scope limits may not cover all generated bean signatures; can't name the future elsewhere.
- **Stable alternative + cost:** `trait_variant` crate generates a `Send` variant whose methods return `impl Future + Send`; bound against it at spawn sites (cost: two trait identities; implementors of the Send variant are forced Send). Or explicit GAT future types (verbose but fully general, covers type/const generics RTN cannot). Or `async-trait` boxing (`Pin<Box<dyn Future + Send>>`) — simplest but allocates per call.
- **Recommendation: watch.** Ship on stable with `trait_variant` (DX) or explicit GATs (generality); keep the async surface behind an internal abstraction so you can migrate to `where B::method(..): Send` later. Track #109417 for a new champion. Do **not** gate leaf's public API on RTN.

### Negative trait impls

- **What it is:** `impl !Trait for Type {}` asserts a type definitively does not implement a trait; for auto traits it both records the SemVer guarantee and disables the auto impl. Used in std for Pin soundness.
- **Status:** stalled-nightly. Tracking #68318 open, B-unstable. Unresolved semantics: conditional negative impls don't work in the trait checker; auto-trait interaction unsettled; no "doesn't implement now but may later." Soundness report #74629 (negative_impls + auto_traits can let impls overlap). No FCP.
- **What it enables for leaf:** in principle, "this type is deliberately NOT a candidate bean" for auto-discovery. In practice the orphan rule limits it to leaf's own types, and conditional negative reasoning (the interesting case) is exactly what doesn't work.
- **Pros:** first-class opt-out signal; compiler-checked SemVer guarantee; battle-tested for the narrow Pin case.
- **Cons:** nightly, no path; unresolved conditional-impl/auto-trait semantics; orphan-rule-limited (cannot exclude third-party types — most of what a DI container sees); coherence interactions have produced unsoundness reports; removing a negative impl is breaking.
- **Stable alternative + cost:** sealed traits + attribute-driven include/exclude at macro/registration time — leaf's proc-macro decides candidacy from `#[bean]`/`#[exclude]` and emits inventory/linkme registrations only for included types. Cost: "negative reasoning" lives in codegen, not the type system (which is fully general and cross-crate).
- **Recommendation: avoid.** Unresolved semantics + orphan limits + coherence unsoundness reports make it a dangerous foundation.

### Auto traits (opt-in built-in traits)

- **What it is:** `auto trait Foo {}` — a user-defined trait auto-implemented when all components implement it (the Send/Sync mechanism). Formerly `optin_builtin_traits`.
- **Status:** stalled-nightly and **explicitly perma-unstable** — tracking #13231 is tagged S-tracking-perma-unstable ("will stay unstable indefinitely"). Open problems: cross-library opt-out coordination, coherence with trait objects, PhantomData interaction. The scoped "default auto traits" experiment (#138781) exists for Rust-for-Linux, not general stabilization.
- **What it enables for leaf:** a structurally-propagating `auto trait Injectable {}` so composite types are auto-discoverable when their parts are. Elegant in theory.
- **Pros:** automatic structural propagation; no per-type registration for the marker itself.
- **Cons:** **declared perma-unstable** (no stabilization will come); unsound/undecided cross-crate opt-out — fatal for a framework spanning many user crates; coherence/PhantomData interactions open; pairs with `negative_impls`, inheriting its instability.
- **Stable alternative + cost:** ordinary marker traits implemented explicitly by leaf's derive/proc-macro + inventory/linkme registration. Cost: the macro emits marker impls rather than getting them "for free" structurally — but it's stable, cross-crate-correct, and zero-overhead.
- **Recommendation: avoid.** Officially permanent nightly; building on it contradicts stable-by-default outright.

---

## IMPL-TRAIT / OPAQUE TYPES (TAIT, ATPIT, RPITIT/AFIT, RTN)

The dyn-compatibility constraint is load-bearing: **RPITIT/AFIT traits are NOT object-safe.** A DI container that stores heterogeneous beans behind `dyn` will route async bean methods through a boxed/`trait_variant` shim at the dyn boundary *regardless of nightly features* — which is precisely why ATPIT/RTN only help the rarer fully-monomorphized paths and should not gate the framework.

### RPITIT — return-position impl Trait in traits

- **What it is:** trait methods returning `-> impl Trait`; each impl gets its own compiler-generated opaque type (anonymous GAT). Static dispatch, monomorphized, zero overhead, no boxing on the static path.
- **Status:** **recently-stabilized in 1.75 (2023-12-28)**, PR #115822; lifetime capture refined by `use<>` in 1.82.
- **What it enables for leaf:** async methods on bean/lifecycle/post-processor traits with zero-cost opaque futures on the static path — the foundation for async-first without the async-trait boxing tax where monomorphized.
- **Pros:** stable; zero-cost when monomorphized; native syntax (no macro on the trait); underpins the whole cluster.
- **Cons:** resulting traits are **not** dyn-compatible (a problem since leaf leans on dyn dispatch — must box/`trait_variant` at the dyn path); can't name the future without nightly; can't generically require `Send` without nightly.
- **Stable alternative + cost:** this *is* the baseline; for the dyn path, fall back to `async-trait`/`Box<dyn Future>`.
- **Recommendation: adopt.** Make it the default surface of leaf's public bean traits.

### AFIT — async fn in traits

- **What it is:** `async fn` in a trait, pure sugar for an RPITIT method returning `-> impl Future`. Same desugaring, same zero-cost static dispatch.
- **Status:** **recently-stabilized in 1.75 (2023-12-28)**, same PR #115822. Caveat: a discouragement lint fires for bare `async fn` in public traits (callers can't add `Send`).
- **What it enables for leaf:** ergonomic `async fn init(&self, ctx: &Container)`, `async fn post_process(&self, bean)` — Spring-style lifecycle hooks as plain async fns.
- **Pros:** most ergonomic async-trait syntax on stable; same zero-cost desugaring; no external macro for the static path.
- **Cons:** same dyn-incompatibility; future's `Send`-ness invisible to callers (tokio multi-thread needs Send); public-trait discouragement lint.
- **Stable alternative + cost:** `trait_variant::make` (Send variant) and/or `async-trait` (boxes future) at the dyn + Send boundary. Both stable; both add a Box on the dyn path.
- **Recommendation: adopt.**

### ATPIT — impl Trait in associated type

- **What it is:** a trait impl uses `impl Trait` as an associated-type value (`type Fut = impl Future<…>;`), naming an opaque future so it can be stored/bounded while staying one concrete monomorphized type. The trait-targeted subset of RFC 2515.
- **Status:** stalled-nightly. Stabilization PR #120700 open, S-blocked/S-waiting-on-concerns, explicitly gated on the next-gen trait solver (#107374; optimistically H1 2026, no firm date). Tracking #63063. Defining-scope reworked via `#[define_opaque]` (PR #128440).
- **What it enables for leaf:** the cleanest way to STORE a bean's async future as a concrete, named, un-boxed container field (e.g. caching an in-flight init future keyed by bean type without `Box<dyn Future>`).
- **Pros:** removes a Box on the "store the future" path; lets leaf expose named future assoc types; trait-scoped (narrower/safer than TAIT).
- **Cons:** nightly, repeatedly slipped; tied to a trait-solver timeline leaf doesn't control; `#[define_opaque]` model still settling → churn risk.
- **Stable alternative + cost:** named boxed assoc type `type Fut = Pin<Box<dyn Future<Output=T> + Send + '_>>`, or store `Box<dyn Future>`. Cost: one heap alloc + vtable per stored future — acceptable since beans are semantic singletons and init runs once.
- **Recommendation: watch.** Documented future optimization behind a `nightly` feature flag that drops the Box on the static path; never default. If you ever take a nightly path here, prefer ATPIT over full TAIT.

### TAIT — type alias impl Trait

- **What it is:** module-level `type Alias = impl Trait;`, hidden type inferred from designated defining sites (now requiring `#[define_opaque(Alias)]`). Full RFC 2515; ATPIT is its trait-scoped subset.
- **Status:** stalled-nightly and the **least mature** of the cluster. Tracking #63063; blocked on next-gen trait solver (#107374). RFC accepted 2019; multiple semantic redesigns; no stabilization version.
- **What it enables for leaf:** a single shared name for an opaque future/iterator across multiple generated container functions and struct fields (e.g. `type BeanInitFut = impl Future<…>` reused by codegen).
- **Pros:** most expressive opaque-naming tool; pairs with heavy codegen.
- **Cons:** most unstable item in the cluster; `#[define_opaque]` ergonomics verbose and changing → generated-code churn liability; ATPIT covers most trait-interface needs with safer semantics.
- **Stable alternative + cost:** box the future / keep it anonymous via RPITIT where a single function defines it; emit a concrete boxed alias for shared codegen types. Cost: heap alloc + dyn dispatch (tolerable for once-per-singleton init).
- **Recommendation: watch.** Prefer ATPIT if a nightly path is ever justified.

### RTN — return type notation

Same feature as the trait-system cluster's RTN; here flagged at higher urgency because its stabilization PR (#138424) was **closed unmerged 2025-12-27** after its author left.

- **Recommendation: avoid / watch.** Do not plan around RTN; lean on `trait_variant`. Re-evaluate when it regains a champion *and* the next-gen trait solver stabilizes.

---

## CONST FEATURES

The stable const-eval baseline is enough for nearly all of leaf's compile-time needs. None of the four nightly features in this cluster is stable; treat the whole cluster as nightly-only ergonomics, not core mechanism.

### General const evaluation baseline (already stable)

- **What it is:** `const fn` with loops/branches/mutable locals/`&mut`-to-local, compile-time panics/`assert!` incl. inline `const { }` blocks, a large const-stable std API surface, and basic const generics (`const N: usize/char/bool`).
- **Status:** recently-stabilized and continually expanded (inline const blocks since 1.79; const generics MVP since 1.51).
- **What it enables for leaf:** compile-time bean metadata structs/keys in `const`/`static` (and linkme/inventory slices); compile-time string-hash keys via a const-fn hasher; const-evaluated config validation that fails the build; basic integer type-level keys.
- **Pros:** zero instability; already powerful enough for keys/registries/build-fail validation; composes with proc-macro/build.rs/linkme; no `allow(incomplete_features)`, no syntax churn.
- **Cons:** cannot call trait methods in generic const fn (the const-traits gap); const generic params limited to integer/char/bool; no arbitrary const-in-type arithmetic.
- **Stable alternative + cost:** N/A — this *is* the stable alternative the other three would replace.
- **Recommendation: adopt.**

### adt_const_params (+ unsized_const_params)

- **What it is:** user structs/enums (and `&str`/`[u8]` via the split-off `unsized_const_params`) as const generic params, gated by `ConstParamTy`/`UnsizedConstParamTy` + structural eq. Enables type-level keys like `Bean<const K: BeanKey>` or `get<const NAME: &'static str>()`.
- **Status:** experimental-incomplete. Requires `#![feature(adt_const_params)]` + `#![allow(incomplete_features)]`. Tracking #95174 (open since 2022). "Largely complete" per the 2026 Full Const Generics goal; RFC + stabilization is a 2026 goal with no committed date. Open: valtrees, structural-eq, symbol mangling for arbitrary values, ICEs; `&'static str` regressed 1.81→1.82 and now routes through `unsized_const_params`.
- **What it enables for leaf:** true type-level bean keys/qualifiers (enum/struct/string), monomorphized zero-cost lookup, compile-time distinctness.
- **Pros:** genuinely type-level keys at zero cost; closest-to-viable in this cluster; rich enum/struct keys improve DX.
- **Cons:** requires `allow(incomplete_features)` — a loud stable-by-default violation; symbol mangling not finalized (matters for linkme/inventory symbol names); `&str` needs a second nightly gate; history of ICEs/regressions.
- **Stable alternative + cost:** stable basic const generics with **integer keys** — hash the type path + qualifier in a `const fn` (FxHash/FNV), use `const KEY: u64`, guard with a `const { assert!(no collisions) }`. Cost: keys are opaque u64s not rich enums; string identity is by-hash not by-value — but 100% stable and mangling-safe.
- **Recommendation: watch.** Reconsider rich type-level keys when the RFC lands + lang FCP completes. Until then, keep behind an off-by-default `nightly` cargo feature only.

### generic_const_exprs

- **What it is:** arbitrary const expressions over generics in type position (`[u8; N+1]`, `[T; size_of::<U>()]`).
- **Status:** stalled-nightly. Tracking #76560; documented incomplete and "may cause compiler crashes." Widely characterized as fundamentally broken with **no path to stabilization**; the project pivoted to the narrower `min_generic_const_args` (#132980, itself still nightly). Numerous ICEs (#142209, #137917, #117657) and coherence/soundness issues (#92186).
- **What it enables for leaf:** computing registry array sizes from generic counts — but leaf has little need for arithmetic-in-types.
- **Pros:** most expressive const-in-type computation if it worked.
- **Cons:** incomplete, unsound, crash-prone; no stabilization path; coherence/overlap bugs dangerous around trait-based DI dispatch.
- **Stable alternative + cost:** generate concrete sizes/tables in build.rs or a proc-macro, emit plain `const`/`static` array literals or linkme slices. Cost: computation lives in codegen, not the type system.
- **Recommendation: avoid.** Treat as permanently off-limits for the core.

### const traits / const fn in traits

- **What it is:** `const trait`, `impl const Trait`, conditionally-const bounds `T: [const] Trait`, plus `const Destruct` and `#[derive_const]` — lets generic const fns call trait methods.
- **Status:** progressing-nightly. `#![feature(const_trait_impl)]`. Active tracking #143874 tied to RFC 3762 (still **open/unaccepted**). "Promising implementation" + stdlib adoption; syntax migrated in-tree from `~const` to `[const]` but **not settled**. 2026 const-traits project goal = finalize RFC + pave for stabilization; "firmly experimental," needs-design-proposal.
- **What it enables for leaf:** const construction of registries/metadata needing generic behavior (const `Default`, `From`/`Into`, `PartialEq`/`Hash` for compile-time key dedup); generic const constructors over bean types; const-evaluated validation calling user trait impls.
- **Pros:** removes the biggest const-fn limitation; active implementation + stdlib adoption signal eventual stabilization; would replace per-type macro-expansion with ordinary generic const fns.
- **Cons:** no accepted RFC; syntax in flux → guaranteed churn for early adopters; not close to stabilization despite implementation maturity; pulling it into the core makes leaf nightly-only.
- **Stable alternative + cost:** proc-macros/build.rs emit concrete (non-generic) const constructors + validation per bean; keep runtime dispatch dyn for generic needs; const validation via inherent const fns + `const { assert! }`. Cost: more codegen, logic duplicated across monomorphizations; no const trait-method calls.
- **Recommendation: watch.** Reconsider generic const-fn construction/validation when RFC 3762 is accepted and `[const]` syntax frozen.

---

## ASYNC FEATURES

The dyn + Send decision is the single most important call in this cluster: a DI container erases bean types behind `dyn` and spawns futures on multi-threaded tokio, so leaf **cannot** expose bare `async fn` in its public dyn-dispatched trait surface. Standardize on `trait_variant` (Send variant) + `dynosaur` (dyn bridge), or hand-emit `Pin<Box<dyn Future + Send + '_>>` from leaf's proc-macros.

### Async closures — see "Already stable." Recommendation: adopt.

`async |ctx| { … }` (1.85) directly models async bean factories. Residual sharp edge: higher-ranked `AsyncFn` bounds across borrows may force boxing (ergonomics, not correctness). To store factories type-erased you still box `Box<dyn Fn(&Ctx) -> Pin<Box<dyn Future + Send>>>` — which is what leaf stores internally anyway.

### AFIT + RPITIT — see IMPL-TRAIT cluster. Recommendation: adopt.

### Return type notation (RTN)

- **Status:** progressing-on-nightly per implementation but **regressed in late 2025** — stabilization PR #138424 closed unmerged 2025-12-27. Tracking #109417, RFC 3654 approved; restricted to lifetime-generic methods on nightly.
- **Recommendation: watch.** When it stabilizes (needs a new champion + next-gen trait solver), leaf can drop the `trait_variant` layer. Until then, `trait_variant::make(SendBeanTrait: Send)` is the shipping default.

### Async Drop (AsyncDrop)

- **What it is:** `trait AsyncDrop { async fn drop(self: Pin<&mut Self>); }` — the would-be primitive for async destructors.
- **Status:** experimental-incomplete. `feature(async_drop)`, codegen tracking #126482 active. A type with `AsyncDrop` must also implement sync `Drop` (PR #142606); **reported show-stopping crash on tokio runtime shutdown** (nightly 2025-11); dyn-Trait async drop unresolved. Far from stabilization.
- **What it enables for leaf:** automatic async `@PreDestroy`/`DisposableBean` (close connections, flush, await graceful shutdown) on scope/container teardown. **Its absence is the key design constraint of this cluster.**
- **Pros:** would match Spring's `@PreDestroy` ergonomics; integrates with scope/ownership.
- **Cons:** experimental-incomplete with known crashes on **leaf's own runtime**; requires paired sync Drop (not pure async); no dyn story (leaf's beans are dyn-erased); adopting now is reckless for a stability promise.
- **Stable alternative + cost:** explicit container-driven async teardown — `trait DisposableBean { fn shutdown(&self) -> Pin<Box<dyn Future<Output=()> + Send + '_>>; }`, awaited in reverse dependency order under a structured-concurrency scope with per-bean timeouts and error aggregation. Cost: cleanup is not automatic on drop — the container must own teardown explicitly (which a DI lifecycle manager should anyway). This is strictly better: deterministic ordering, timeouts, observability that Drop cannot give.
- **Recommendation: avoid.** Design around its absence; mirror `@PostConstruct` with an `async fn init`/`InitializingBean` invoked right after construction+injection.

### Coroutines / generators

- **What it is:** low-level stackless `yield` primitive underlying async/await and gen blocks, via `#[coroutine]` closures.
- **Status:** stalled-nightly, "extra-unstable." Tracking #43122 (RFC 2033, explicitly experimental). Direction shifted to gen blocks; the raw gate has **no stabilization path**.
- **What it enables for leaf:** essentially nothing async/await + `Stream` don't already cover.
- **Pros:** maximal control over hand-rolled state machines (irrelevant to leaf).
- **Cons:** extra-unstable, no path; wrong abstraction level; major instability liability.
- **Stable alternative + cost:** async/await + `futures::Stream`; `async-stream` for incremental producers. Cost: none meaningful.
- **Recommendation: avoid.**

### gen blocks / gen fn (and async gen)

- **What it is:** `gen { yield … }`/`gen fn` desugaring to `Iterator` (and, in async form, to `AsyncIterator`/Stream). RFC 3513.
- **Status:** progressing-nightly. Tracking #117078, RFC-approved. Sync gen implements `Iterator`/`FusedIterator`; open questions remain (Iterator vs IntoIterator, self-referential gen, size_hint, edition/keyword). async gen is the least settled. No date.
- **What it enables for leaf:** `async gen { for e in source { yield e } }` for ergonomic event-stream sources.
- **Pros:** ergonomic iterator/stream authoring; sync form RFC-approved.
- **Cons:** nightly; async gen ties to the unstable `AsyncIterator`; open design questions; violates stable-by-default.
- **Stable alternative + cost:** `async-stream`'s `stream! { yield … }` gives the exact ergonomics on stable, producing a `futures::Stream`. Cost: a crate dependency (already implied).
- **Recommendation: watch.**

### AsyncIterator (std)

- **What it is:** std's `AsyncIterator` (`poll_next`), the would-be std home of `Stream`. RFC 2996.
- **Status:** experimental-incomplete. `feature(async_iterator)`, tracking #79024. Exposes `poll_next` (not `async fn next`), dyn-compatible, **no combinators** in std. Open questions before stabilization; no date.
- **What it enables for leaf:** a std-blessed type for streaming application events / reactive bean outputs.
- **Pros:** canonical long-term std type; dyn-compatible (unlike AFIT).
- **Cons:** nightly, incomplete, combinator-less; `futures::Stream` is the de-facto standard and needs interop shims for years; no benefit over Stream today.
- **Stable alternative + cost:** `futures::Stream` + `StreamExt` (and `tokio-stream`) — stable, combinator-rich, universally interoperable. Cost: a non-std dependency (already implied by async-first on tokio).
- **Recommendation: watch.** Standardize leaf's event-streaming API on `futures::Stream`.

---

## LINK-TIME REGISTRATION & LIFE-BEFORE-MAIN

Nightly is **not needed** for component auto-discovery. The architectural choice is between `inventory` (life-before-main, DCE-robust, wasm) and `linkme` (zero-cost link-time, DCE-sensitive, no wasm).

### #[used] attribute (plain form)

- **What it is:** forces the compiler to keep a `static` in the emitted object file even if unreferenced. Guarantee is object-file-level only — the linker may still strip it.
- **Status:** **recently-stabilized in 1.30.0 (2018-10-25)**, RFC 2386. Rock solid; used transitively by linkme/inventory.
- **What it enables for leaf:** the base under any link-time registry; used transitively, not directly.
- **Pros:** stable since 1.30; zero runtime cost; relied on by both candidate crates.
- **Cons:** does **not** by itself protect against linker `--gc-sections` (the "works in debug, empty registry in release" trap); statics only; the object-file-only guarantee can mislead.
- **Stable alternative + cost:** it *is* the primitive; the more precise nightly variant is `used(linker)`.
- **Recommendation: adopt** (transitively, via linkme/inventory).

### #[used(linker)] / #[used(compiler)] (used_with_arg)

- **What it is:** refinements disambiguating object-file-vs-linker retention; `#[used(linker)]` instructs the linker to retain against `--gc-sections` (ELF `SHF_GNU_RETAIN`).
- **Status:** stalled-nightly. Tracking #93798 (opened 2022-02-09, S-tracking-design-concerns; ~4 comments, last update 2026-04-19). Unresolved: plain-`#[used]` default, older-linker reliability, whether `linker` implies `compiler`, Mach-O handling. Stalled ~4 years.
- **What it enables for leaf:** the "clean" way to survive `--gc-sections` across crate boundaries.
- **Pros:** semantically-correct fix for the #1 link-time hazard; would let a from-scratch registry avoid the crates' workarounds.
- **Cons:** nightly, stalled 4+ years; default semantics may still change; linker-version-dependent; zero net benefit over the crates.
- **Stable alternative + cost:** `linkme` (engineers its own retention) or `inventory` (sidesteps DCE via life-before-main). Cost: depend on a crate's linker shims rather than a language guarantee — acceptable given dtolnay's track record.
- **Recommendation: avoid.**

### #[link_section] attribute

- **What it is:** places a function/static into a named object-file section; combined with linker boundary symbols (`__start_/__stop_`), lets code iterate everything in that section — the mechanical heart of a distributed slice.
- **Status:** recently-stabilized (long stable; edition 2024 requires the `unsafe(link_section = "…")` qualifier — syntactic, still stable). 1.96.0 changed precedence when multiple `link_section` attrs are present (first wins).
- **What it enables for leaf:** the substrate for linkme distributed slices (descriptors → section → contiguous slice, zero runtime init).
- **Pros:** stable; zero runtime cost; enables contiguous `&'static [T]`; no life-before-main.
- **Cons:** section names are object-format-specific (must be abstracted — i.e. use linkme); subject to `--gc-sections`; cross-crate custom-section drop bug (rust-lang/rust#67209 — entries defined only in dependency crates can vanish); edition-2024 `unsafe()` churn.
- **Stable alternative + cost:** don't hand-roll — use linkme. Cost: a (well-maintained) dependency + its linker shims.
- **Recommendation: adopt-behind-feature** (via linkme).

### #[no_mangle] attribute

- **What it is:** disables name mangling; also forces export/reachability. Edition 2024 requires `unsafe(no_mangle)`.
- **Status:** recently-stabilized (long stable).
- **What it enables for leaf:** minor/indirect — leaf should **not** use it for registration.
- **Pros:** stable; forces export; predictable names when genuinely needed.
- **Cons:** symbol-collision risk scales badly with many auto-generated registrations; edition-2024 `unsafe()` requirement; pollutes the global namespace.
- **Stable alternative + cost:** `#[used]` + `#[link_section]` with mangled, crate-unique symbol names (linkme's approach). Cost: none — strictly better for registration.
- **Recommendation: avoid** (for the registration use case).

### linkme — distributed slices

- **What it is:** dtolnay's `#[distributed_slice]` — declare `static REGISTRY: [T]`, register elements anywhere with `#[distributed_slice(REGISTRY)]`; the linker gathers them into one contiguous `&'static [T]`. No life-before-main, no runtime init.
- **Status:** works on stable. v0.3.36, MSRV 1.71. Supports Linux/macOS/Windows/FreeBSD/OpenBSD/illumos; **does NOT support wasm32-unknown-unknown** (linkme #6).
- **What it enables for leaf:** the lowest-overhead component-scanning backend; container reads `&'static [ComponentDescriptor]` at startup with zero per-entry cost and no pre-main ordering issues.
- **Pros:** stable; zero runtime/startup cost; contiguous slice → trivial deterministic iteration; no constructor hazards; async-friendly (no pre-main async).
- **Cons:** DCE-sensitive (`--gc-sections`/lld changes can empty the slice — linkme #49); cross-crate section-drop bug (linkme #36 / rust-lang/rust#67209) — **a binary with zero local registrations can see an empty registry**, critical since leaf components live in library crates; no wasm.
- **Stable alternative + cost:** inventory (DCE-robust, wasm, but life-before-main); or a build.rs-generated central registry (no link tricks, but a generated "register all" call defeats the no-central-registry goal).
- **Recommendation: adopt-behind-feature.** Optional low-overhead backend. If used, **force a sentinel registration in a leaf-controlled crate that every binary links**, document `-z nostart-stop-gc`/DCE caveats, and gate wasm out.

### inventory — submit!/iter

- **What it is:** dtolnay's typed plugin registry — `collect!(T)`, `submit!{ value }`, `iter::<T>()`. Each `submit!` emits a life-before-main constructor that pushes into a global list.
- **Status:** works on stable. v0.3.24 (2026-03-30), MSRV 1.68. Broad platform support; **supports all WebAssembly targets** (requires the host to call `__wasm_call_ctors()`).
- **What it enables for leaf:** the DCE-robust, wasm-capable backend; components self-register via `submit!`, container enumerates via `iter::<ComponentDescriptor>()` at startup.
- **Pros:** stable; DCE-robust (constructors are reachable, not stripped — avoids linkme's empty-registry traps); wasm; cross-crate registration works regardless of where members are defined; simple API.
- **Cons:** life-before-main — constructors run before any async runtime/logger/config, so registration must be trivial, synchronous, non-panicking; **unspecified iteration order** (leaf must impose ordering); small per-entry startup cost; Miri incompatible (use LLVM sanitizers); pre-main "spooky action" surprises users.
- **Stable alternative + cost:** linkme (zero-cost but DCE-sensitive, no wasm); or build.rs-generated central registry (deterministic but reintroduces an aggregation step).
- **Recommendation: adopt** as the **primary** mechanism. DCE-robustness and cross-crate-safety make it the credible default for a framework that must "just work." Carry an explicit ordering key (declared priority / topo-sort) in the descriptor and sort deterministically — never rely on registration order.

### ctor crate (raw life-before-main)

- **What it is:** lower-level `#[ctor]`/`#[dtor]` to run functions before main / after main. inventory is a typed, safer layer over this.
- **Status:** works on stable. v1.0.x, now requires `unsafe` on constructors. Very broad platform list.
- **What it enables for leaf:** raw pre-main hooks — but leaf should not use it directly.
- **Pros:** stable; maximal platform coverage; full pre-main control.
- **Cons:** raw, unsafe, easy to misuse; all life-before-main hazards with none of inventory's typing/safety; runtime-less execution hostile to async-first; offers nothing over inventory.
- **Stable alternative + cost:** inventory. Cost: none.
- **Recommendation: avoid** (use inventory).

---

## MEMORY, LAYOUT & UNSIZING

Keep this entire layer on stable. The two relevant wins (trait upcasting 1.86, drop-the-principal 1.84) already shipped and are covered in "Already stable." Arena needs are met by crates; type erasure by `Arc<dyn Trait>` + `dyn Any`.

### allocator_api

- **What it is:** the `Allocator` trait + allocator-parametric containers (`Box<T,A>`, `Vec<T,A>`, `new_in`), `Global`/`System`.
- **Status:** stalled-nightly. `feature(allocator_api)`, tracking #32838. The minimal stabilization PR **#156882 was reverted to DRAFT on 2026-06-01** after fresh soundness bugs ("needs time to bake"). Not in FCP, not merged. ~10 years unstable; ~75 open WG issues.
- **What it enables for leaf:** arena/region storage for scoped beans with first-class `Box<T, ArenaAlloc>`, enabling bulk free of request/session-scoped beans.
- **Pros:** native zero-dependency arena storage; bulk dealloc matches DI scope lifetimes; composes with std collections.
- **Cons:** requires nightly for **all** end users; surface not settled (June 2026 soundness regression); non-ZST allocators inflate container size.
- **Stable alternative + cost:** `allocator-api2` (mirrors the nightly trait on stable 1.71+, 250M+ downloads, used by hashbrown/bumpalo) + `bumpalo` (with its `allocator-api2` feature). Cost: one extra dependency; the shim trait isn't std's `Allocator`, so std `Box`/`Vec` won't take it directly (use bumpalo's/hashbrown's). Slight friction, no nightly.
- **Recommendation: avoid** (use allocator-api2/bumpalo). Revisit only after the Sized-Hierarchy redesign.

### ptr_metadata

- **What it is:** generic pointer metadata — `Pointee`, `metadata()`, `from_raw_parts`, `DynMetadata`, `Thin`.
- **Status:** stalled-nightly. `feature(ptr_metadata)`, tracking #81513, "probably blocked on #144404" (Sized-Hierarchy). Last substantive update Oct 2024.
- **What it enables for leaf:** custom thin/fat pointers for type-erased beans; split/recombine `dyn Trait` into (data ptr, DynMetadata).
- **Pros:** principled fat-pointer construction; `DynMetadata` for advanced registry layouts.
- **Cons:** nightly; entangled with Sized-Hierarchy (long, uncertain); leaf doesn't actually need raw fat-pointer surgery — stable `dyn Any` + trait upcasting cover downcast.
- **Stable alternative + cost:** `Box<dyn Any + Send + Sync>` + downcast, strengthened by trait upcasting (1.86) so any `dyn Bean` coerces to `dyn Any` without manual `as_any`. For rare genuine need: the `ptr_meta` crate. Cost: downcast is a vtable+TypeId check (cheap); ptr_meta adds a dependency.
- **Recommendation: avoid.**

### CoerceUnsized / Unsize / DispatchFromDyn

- **What it is:** the unsizing machinery — `Unsize<U>`, `CoerceUnsized<U>` (custom pointer-like wrappers participate in coercion), `DispatchFromDyn` (custom self-receivers).
- **Status:** stalled-nightly. Tracking #27732 (open since 2015). A 2026 proposal would stabilize only `Unsize` (compiler-only, not user-implementable), explicitly excluding `CoerceUnsized` ("design still in progress"). Nothing landed.
- **What it enables for leaf:** a custom bean-handle smart pointer that still coerces `Handle<Concrete>` → `Handle<dyn BeanTrait>` and dispatches.
- **Pros:** bespoke handle types that work with dyn dispatch; could encode scope/registry tags while staying coercible.
- **Cons:** nightly, slated for redesign; only stdlib types can use `CoerceUnsized` today; the user-facing path (`derive(CoercePointee)`) is also unstable.
- **Stable alternative + cost:** wrap a std pointer that already implements these (`Arc<dyn BeanTrait>`, `Box<dyn …>`) inside a newtype, or store it directly; `Arc<Concrete>` → `Arc<dyn Trait>` is fully stable. Cost: cannot make your *own* raw pointer representation dyn-coercible — essentially free for a DI container.
- **Recommendation: avoid.**

### derive(CoercePointee) — RFC 3621

- **What it is:** a derive that auto-derives the unsizing impls for a user smart-pointer struct under a checked schema (`MyPtr<Concrete>` → `MyPtr<dyn Trait>`), without unsafe and without `alloc`.
- **Status:** stalled-nightly. `feature(derive_coerce_pointee)`, tracking #123430. Stabilization PR **#133820 is OPEN, not merged** (verified: state OPEN, mergedAt null), blocked on an `arbitrary_self_types` soundness bug (#136702). Renamed twice; a 2026 PR (#147068) proposes moving it to `core::ops`.
- **What it enables for leaf:** the ergonomic, safe way to define a branded custom bean-handle smart pointer.
- **Pros:** safe schema-checked custom smart pointers as dyn handles; no `alloc`; single derive.
- **Cons:** nightly; PR open with unresolved soundness blocker; API location still moving → import churn; mostly motivated by Rust-for-Linux, not DI (leaf gets ~95% from std `Arc<dyn Trait>`).
- **Stable alternative + cost:** newtype-wrap std `Arc<dyn BeanTrait>`/`Box<dyn …>`; coercion comes free. Cost: the handle can't have a fully custom field layout that still dyn-coerces — acceptable.
- **Recommendation: watch.** The only item in this cluster worth watching; revisit if #133820 merges and it appears in a stable release. Not needed for v1.

### dyn* (dyn-star) — REMOVED FROM THE COMPILER

> **Corrected status (verification override).** The survey listed dyn* as nightly/experimental-incomplete with tracking #102425 open. **That is refuted.** `dyn*` support was **removed** from the compiler by PR rust-lang/rust#143036 (merged, **Rust 1.90.0** milestone). The compiler no longer accepts `#![feature(dyn_star)]`; the gate now lives in `compiler/rustc_feature/src/removed.rs` with the reason *"removed as it was no longer necessary for AFIDT (async fn in dyn trait) support."* (The "1.65.0" on that line is the feature's original introduction version per the file's convention, **not** the removal version.) Tracking issue #102425 is now **CLOSED**.

- **What it was:** an experimental trait-object form storing the value inline in a pointer-width slot alongside its vtable; explored as substrate for async-fn-in-dyn-trait.
- **What it would have enabled for leaf:** inline-stored type-erased handles (avoid a heap indirection for small beans); a substrate for async methods on dyn bean traits.
- **Cons:** no longer exists as a usable feature; the async-in-dyn work moved to `async_fn_in_dyn_trait` (#133119, still nightly/incomplete).
- **Stable alternative + cost:** for async methods on dyn bean traits, `async-trait` (boxes the future; stable, mature) or hand-written `-> Pin<Box<dyn Future>>`; for type erasure, `Box`/`Arc<dyn Trait>`. Cost: a heap alloc per async call — acceptable for DI wiring; avoid on hot paths by keeping them concrete/static-dispatched.
- **Recommendation: avoid (the feature is gone).** Do not reference `dyn*` in any leaf design or documentation as a future option.

---

## TYPE IDENTITY & (PSEUDO-)REFLECTION

Central fact: **Rust has no runtime reflection.** You can *recognize* a type you named in source (`TypeId`, `Any`), but you cannot *discover* fields/methods/generics/attributes at runtime. leaf must **synthesize** reflection at build time (proc-macros emit metadata; linkme/inventory collect it) and use `TypeId` purely as a **key**.

### const TypeId::of — see "Already stable." Recommendation: adopt.

const-stabilized in **1.91.0 (2025-10-30)** via PR #144133 (sound provenance-based impl; CTFE/Miri error on bit inspection). Lets registry rows be fully-const `static`s embedding `TypeId::of::<T>()`. **Caveat:** `TypeId` size/layout/`Hash`/`Ord` are NOT stable across compiler releases — never serialize a TypeId or rely on cross-build ordering.

### const TypeId comparison (const_cmp_type_id)

- **What it is:** `impl const PartialEq/Ord for TypeId` — compile-time type-equality branching.
- **Status:** stalled-nightly. Tracking #101871/#73900; nightly docs show `impl PartialEq for TypeId` as `(const: unstable)`. Gated on **const-traits** (a long-horizon effort). Open design reservation: making comparison const would foreclose a future collision-resistant pointer-to-static TypeId scheme.
- **What it enables for leaf:** fully compile-time type dispatch/dedup/perfect-hash construction. Marginal — leaf wires the graph at startup, not in const.
- **Pros:** const-time type-equality checks; pairs with const `TypeId::of`.
- **Cons:** nightly, gated on high-churn const-traits; design may be withdrawn; negligible payoff (resolution is a startup activity).
- **Stable alternative + cost:** compare TypeIds at runtime (`==`/HashMap), exactly when leaf wires the graph. Compile-time wiring validation is better delivered by the proc-macro emitting trait-bound assertions. Cost: none meaningful.
- **Recommendation: avoid.**

### Non-'static TypeId

- **What it is:** would relax the `'static` bound so ids exist for types with non-`'static` lifetimes (lifetimes erased).
- **Status:** **removed/retracted.** RFC 1849 (tracking #41875) was retracted; never implemented, **not available even on nightly** (`TypeId::of` still requires `T: 'static`). Rationale: lifetime-erasure makes `S<'a,'b>` and `S<'b,'a>` share an id — unsound if used for downcast without external lifetime proof.
- **What it enables for leaf:** keying borrow-holding services — but leaf holds long-lived singletons (effectively `'static` via Arc), so the need is marginal.
- **Pros:** would allow type-keying of borrow-holding types.
- **Cons:** retracted (won't arrive); lifetime-erasing identity is unsound for downcast; conflicts with leaf's owned/Arc'd singleton model.
- **Stable alternative + cost:** the dtolnay `typeid` crate (`typeid::of::<T>()`, `ConstTypeId`) — **documented caveat:** equality does NOT prove same-type unless one side is `'static`, so use only as a hint/key, never a downcast-safety proof. Simplest: require `'static` on managed components (which leaf does anyway).
- **Recommendation: avoid.**

### type_name / type_name_of_val — see "Already stable." Recommendation: adopt.

`type_name` stable since 1.38; `type_name_of_val` since 1.76. **Diagnostic-only** — format unstable across compiler versions, doesn't resolve trait objects; never parse or branch on it. The const versions (`const_type_name`, #63084) remain nightly. For reliable name keys, have the proc-macro emit a canonical name string (module path + ident) into generated metadata.

### Any + downcast — see "Already stable." Recommendation: adopt.

Core stable since 1.0; `Arc/Rc<dyn Any>::downcast` since 1.29; downcast on `Send + Sync` variants stable. The runtime backbone: `HashMap<TypeId, Arc<dyn Any + Send + Sync>>` + `downcast`. **Recognition-only** — it does not reduce the need for codegen; keep all wiring guarantees at codegen time. Soundness note: hand-rolled raw-TypeId tricks risk unsoundness via fn-pointer variance — stick to the safe stdlib downcast APIs.

### Provider / Request generic member access (error_generic_member_access)

> **Corrected status (verification override).** The survey cited tracking issue **#96024** with FCP "pending." Both are wrong. The feature is still nightly-only (requires `#![feature(error_generic_member_access)]`), but: (1) the **correct tracking issue is #99301** (#96024 is the superseded `provide_any` Provider API); (2) the **FCP was CANCELLED on 2026-02-26** (libs-api lead ran `@rfcbot cancel`), because the team now prefers a language-level trait-to-trait casting solution (RFC 3885) over the provider API — a new concern about a "providable" trait was also raised 2026-01-20; (3) the "final naming demand/request" blocker is **outdated** — the Demand→Request rename already completed in PR #113464 (`Request` is settled). Accurate parts: the Provider trait was dropped and the API lives on `Error::provide(&self, &mut Request)`; the LLVM multi-`.provide_*` switch-table item is a real blocker; the long timeline (RFC 2895 ~2020, impl ~2022) is roughly right.

- **What it is:** type-driven access to context members on `dyn Error` via `Error::provide` + `Request`.
- **Status:** stalled-nightly, **FCP cancelled**, approach now in question (steering toward RFC 3885). Tracking **#99301**.
- **What it enables for leaf:** typed error context (`request_ref::<T>()`) — tangential; does not help DI wiring.
- **Pros:** standardized type-indexed error context *if* it ever stabilizes.
- **Cons:** nightly, long-stalled, FCP withdrawn, approach contested; scope is error context only.
- **Stable alternative + cost:** a bespoke `LeafError` enum/struct with explicit typed fields, or anyhow/eyre-style context chaining. Cost: define your own context — more predictable than a nightly type-indexed bag.
- **Recommendation: avoid.**

### Link-time registries (linkme / inventory) — the actual "reflection" substrate

This is how leaf gets "reflection" without language support: proc-macros emit per-component metadata (TypeId key + canonical name + dependency descriptors + constructor/async-factory), collected into a queryable catalog. Both crates are stable. See the LINK-TIME REGISTRATION cluster for the full inventory-vs-linkme decision. **Recommendation: adopt** (inventory primary, linkme behind-flag). Note the standing caveats: linkme's cross-crate discard pitfall, inventory's unspecified order (carry an explicit ordering key and topo-sort yourself).

---

## DIAGNOSTics & DX ERGONOMICS

The two pillars are stable and should be a first-class part of leaf's misconfiguration story.

### #[diagnostic::on_unimplemented] — see "Already stable." Recommendation: adopt.

Stable since **1.78** (RFC 3368). The single highest-leverage DX feature here. Annotate core capability traits (`Injectable`, `Component`, `AsyncBean`, `FromContext`) so a forgotten `#[derive(Component)]` or an incompatible wiring produces a leaf-authored, actionable message plus notes, instead of "trait bound not satisfied." Forward-compatible namespace (unknown inputs degrade to warnings). **Cons:** best-effort (deeply nested/blanket scenarios may still surface raw bounds); static text + limited format params (`{Self}`, `{GenericParameterName}`). Treat messages as tested API (UI tests via `trybuild`/compiletest).

### #[diagnostic::do_not_recommend] — see "Already stable." Recommendation: adopt.

Stable since **1.85** (PR #132056). Annotate leaf's internal blanket impls (those backing link-time registration) so rustc stops dumping every candidate impl, keeping the error focused on the user's type + the `on_unimplemented` message. Originated from Diesel, whose impl topology resembles a DI container's. Pair with `on_unimplemented` for full effect.

### let_chains

- **What it is:** `&&`-chaining `let` with boolean conditions inside `if`/`while`.
- **Status:** **recently-stabilized in 1.88 (2025-06-26), edition 2024 ONLY** (#132833) — depends on the edition-2024 if-let temporary-scope drop-order change.
- **What it enables for leaf:** flatter conditional logic in runtime and proc-macro-generated wiring (optional-dependency fallbacks, scope/qualifier matching).
- **Pros:** stable in edition 2024; flattens conditionals; leaf can mandate edition 2024 for its own crates.
- **Cons:** **edition-2024-only** — consumers on older editions can't use it; proc-macro output adopts the *call-site* crate's edition, so don't emit let_chains in generated tokens unless the call site is guaranteed edition 2024.
- **Stable alternative + cost:** nested `if let` / `let-else` / intermediate `match`. Cost: verbosity — but edition-agnostic, safer for generated code.
- **Recommendation: adopt-behind-feature.** Use in leaf's own (edition-2024) crates; keep generated tokens edition-agnostic.

### if_let_guard

- **What it is:** `if let` patterns in `match` arm guards.
- **Status:** **recently-stabilized in 1.95 (2026-04-16)**, PR #141295, **all editions** (no else-branch, so it avoids the drop-order issue). Guards don't count toward exhaustiveness (same as ordinary guards).
- **What it enables for leaf:** cleaner runtime dispatch / generated match arms resolving beans by qualifier/scope with a fallible secondary lookup.
- **Pros:** stable in all editions; eliminates nested-match workarounds.
- **Cons:** very recent (~2 months as of June 2026) — raises effective MSRV to 1.95; guard patterns don't aid exhaustiveness (a subtle footgun in generated code); lower payoff than the diagnostic attributes.
- **Stable alternative + cost:** move the `if let` into the arm body with a nested match / helper returning Option. Cost: an extra arm/helper — works on any toolchain (preferable while MSRV < 1.95).
- **Recommendation: adopt-behind-feature.** Gate behind MSRV 1.95 deliberately.

### try_blocks

- **What it is:** `try { … }` scopes where `?` short-circuits to the block result.
- **Status:** stalled-nightly. `feature(try_blocks)`, tracking #31436 (RFC 243, open since 2016). Ongoing type-inference work (PR #148725, Nov 2025) + a heterogeneous-try-block experiment (#149488). No FCP.
- **What it enables for leaf:** grouping several `?`-fallible init/wiring steps into a scoped fallible block — ergonomic, not load-bearing.
- **Pros:** cleaner localized error handling; less helper-fn boilerplate.
- **Cons:** nightly, no timeline; known inference pain (often needs annotations); design in flux; marginal benefit.
- **Stable alternative + cost:** extract an `async fn`/closure returning `Result`, or an IIFE `(|| { … })()` with `?`. Cost: slightly more verbose; fully stable.
- **Recommendation: avoid.**

### never_type (! in type position)

- **What it is:** `!` usable in arbitrary type position (`Result<T, !>`, generic/assoc positions). `!` as a *return type* is already stable.
- **Status:** progressing-nightly. `feature(never_type)`, tracking #35121 (open since 2016). Edition 2024 already changed inference fallback to `!` (1.85). Stabilization PR **#155499 entered FCP 2026-06-11 but is NOT merged** as of 2026-06-14; it aliases `Infallible = !` and changes fallback across all editions (a breaking change). History: twice-reverted over inference breakage.
- **What it enables for leaf:** precise infallibility — `Provider<Error = !>` / `Result<Bean, !>`, letting the compiler prune unreachable error arms.
- **Pros:** cleaner than `Infallible`; FCP suggests stabilization may land soon; edition-2024 fallback-to-`!` already stable and beneficial.
- **Cons:** `!`-in-type-position still nightly; historically fragile (FCP not yet merged); full stabilization is a cross-edition breaking change; benefit over `Infallible` is small.
- **Stable alternative + cost:** `std::convert::Infallible` (the plan literally aliases `Infallible = !`, so code written against it upgrades for free). Cost: slightly more verbose; marginally worse `From<Infallible>` ergonomics.
- **Recommendation: watch.** Use `Infallible` today; revisit `!` directly if #155499 merges into a stable release.

### proc_macro::Span inspection APIs — see "Already stable." Recommendation: adopt.

`Span::{line,column,start,end,file,local_file}` stable in **1.88** (PRs #139865, #140514); `source_text()` stable earlier. Embed declaration-site provenance into registered metadata so runtime wiring errors cite the exact source line — a Spring-grade DX touch without nightly. **Note:** `byte_range()`, `join()`, `parent()`, `source()`, `Literal::subspan()` remain nightly (`proc_macro_span`) — avoid in stable builds. MSRV ≥ 1.88.

### proc_macro::Diagnostic emit API + proc_macro_warning/LintId

- **What it is:** nightly `proc_macro::Diagnostic` for structured multi-level, multi-span diagnostics + a proposed `#[proc_macro_warning]`/`LintId` so macro warnings are lint-controllable.
- **Status:** stalled-nightly. `feature(proc_macro_diagnostic)`, tracking #54140 (open since 2018). Used by Rocket/Diesel/Maud on nightly. Unblocking PR #135432 (dtolnay) **not merged** — S-waiting-on-author, last activity April 2025, with a contested redesign. No stabilization PR.
- **What it enables for leaf:** native multi-span warnings/errors (point at a duplicate-bean def AND its conflict simultaneously), user-`#[allow]`-able lints.
- **Pros:** richest possible macro error UX; lint-controllable warnings; proven in Rocket/Diesel.
- **Cons:** nightly, no timeline; key PR stuck with contested redesign; the main advantage (multi-span emit) is exactly the unfinished part.
- **Stable alternative + cost:** `syn::Error` (multi-error via `Error::combine`, each with its own span) → `to_compile_error()`; for warnings, a deprecated-shim or dummy-const triggering a built-in lint (proc-macro-error2 pattern). Cost: warnings aren't truly `#[allow]`-able, and you can't point one diagnostic at multiple spans as cleanly. Covers ~90% of the desired UX.
- **Recommendation: avoid.** Build macro errors on `syn::Error` + `on_unimplemented`; accept the multi-span gap rather than going nightly.

---

## Standing policy

leaf is **stable by default**. A nightly feature may be adopted only when all of the following hold:

1. **Marked, justified win.** It delivers a benefit that is *materially* better than the best stable alternative — not merely more elegant. "Removes a Box on a once-per-singleton init path" is not marked; "is the only way to express a load-bearing public API" might be. Document the specific win and the rejected stable alternative in the same place as the feature flag.
2. **Sound and not crash-flagged.** Never adopt a feature the compiler flags as "incomplete and may cause crashes" or that carries an `I-unsound`/UB caveat (this excludes `specialization`, `generic_const_exprs`, and anything requiring `allow(incomplete_features)` in shipping code). Soundness is non-negotiable for a framework whose top promise is predictability.
3. **Plausible stabilization path.** There must be an accepted RFC and active, owned progress. Reject perma-unstable features (`auto_traits`), ownerless ones (RTN's closed PR), and retracted/removed ones (non-`'static` TypeId, `dyn*`).
4. **Off by default, behind a clearly-labeled `nightly` cargo feature.** The default build must compile on stable. Nightly features may only *drop an overhead* or *add an advanced capability* on an opt-in path that has a stable fallback compiled in by default. They may never gate leaf's public API surface or a correctness property.
5. **No rustc-internal attributes.** Anything requiring `#[rustc_*]` (e.g. `min_specialization`'s useful extensions) is closed to user crates and out of scope regardless of soundness.
6. **Encapsulated migration seam.** Where a nightly feature is the intended long-term answer (RTN, ATPIT, const traits, `!`), design the stable implementation behind an internal abstraction so the future migration is a localized swap, not an API break. Pin a tracking issue per such seam and re-evaluate when the gating milestone moves.
7. **The next-gen trait solver (`-Znext-solver`, #107374) is the single watch signal** for the impl-Trait/opaque cluster (TAIT, ATPIT, RTN). Do not adopt any of them before it stabilizes; re-evaluate the whole cluster the day it does.

Default posture per status: **recently-stabilized → adopt** (mind MSRV/edition); **progressing-nightly with accepted RFC → watch behind a seam**; **stalled/experimental/unsound/removed → avoid**. When in doubt, ship the stable alternative — leaf's committed stack (async-first stable primitives + RPITIT/AFIT + `trait_variant`/`async-trait`/Box at the dyn boundary + codegen + linkme/inventory + `Any`/`TypeId`/trait upcasting + the diagnostic namespace) already delivers a zero-nightly framework. The only genuinely missing language primitive is async Drop; design around it with explicit container-driven shutdown.

---

## Sources

- Tracking issue for specialization (RFC 1210) #31844 — https://github.com/rust-lang/rust/issues/31844
- min_specialization — Rust Unstable Book — https://doc.rust-lang.org/nightly/unstable-book/language-features/min-specialization.html
- specialization — Rust Unstable Book — https://doc.rust-lang.org/nightly/unstable-book/language-features/specialization.html
- rustc_hir_analysis::impl_wf_check::min_specialization — https://doc.rust-lang.org/nightly/nightly-rustc/rustc_hir_analysis/impl_wf_check/min_specialization/index.html
- RFC 1210: impl specialization — https://rust-lang.github.io/rfcs/1210-impl-specialization.html
- Aaron Turon — Shipping specialization: a story of soundness — https://aturon.github.io/blog/2017/07/08/lifetime-dispatch/
- specialization: restrictions around lifetime dispatch #45982 — https://github.com/rust-lang/rust/issues/45982
- Specialization and lifetime dispatch #40582 — https://github.com/rust-lang/rust/issues/40582
- PR #68970 — implement min_specialization — https://github.com/rust-lang/rust/pull/68970
- dtolnay/case-studies — autoref-based specialization — https://github.com/dtolnay/case-studies/blob/master/autoref-specialization/README.md
- Lukas Kalbertodt — Generalized Autoref-Based Specialization — http://lukaskalbertodt.github.io/2019/12/05/generalized-autoref-based-specialization.html
- ICE: expected specialization failed to hold (RPITIT) #150751 — https://github.com/rust-lang/rust/issues/150751
- Announcing Rust 1.86.0 (trait upcasting) — https://blog.rust-lang.org/2025/04/03/Rust-1.86.0/
- Tracking issue for dyn upcasting coercion #65991 — https://github.com/rust-lang/rust/issues/65991
- Stabilize feature(trait_upcasting) — PR #134367 — https://github.com/rust-lang/rust/pull/134367
- Tracking Issue for return type notation #109417 — https://github.com/rust-lang/rust/issues/109417
- Stabilize return type notation (RFC 3654) — PR #138424 (closed unmerged) — https://github.com/rust-lang/rust/pull/138424
- RFC 3654: Return Type Notation — https://rust-lang.github.io/rfcs/3654-return-type-notation.html
- RTN call for testing (Inside Rust, 2024-09-26) — https://blog.rust-lang.org/inside-rust/2024/09/26/rtn-call-for-testing/
- negative_impls — Rust Unstable Book — https://doc.rust-lang.org/nightly/unstable-book/language-features/negative-impls.html
- Tracking issue for negative impls #68318 — https://github.com/rust-lang/rust/issues/68318
- negative_impls and auto_traits allow trait impls to overlap #74629 — https://github.com/rust-lang/rust/issues/74629
- Tracking issue for auto traits #13231 (S-tracking-perma-unstable) — https://github.com/rust-lang/rust/issues/13231
- Tracking Issue for experiment with default auto traits #138781 — https://github.com/rust-lang/rust/issues/138781
- Announcing Rust 1.96.0 (latest stable, 2026-05-28) — https://blog.rust-lang.org/2026/05/28/Rust-1.96.0/
- Rust Versions — releases.rs — https://releases.rs/
- Project goals update — April 2026 — https://blog.rust-lang.org/2026/05/18/project-goals-2026-04/
- Announcing async fn and return-position impl Trait in traits (1.75) — https://blog.rust-lang.org/2023/12/21/async-fn-rpit-in-traits/
- Stabilize async fn and RPITIT — PR #115822 — https://github.com/rust-lang/rust/pull/115822
- Announcing Rust 1.75.0 — https://blog.rust-lang.org/2023/12/28/Rust-1.75.0/
- Announcing Rust 1.82.0 (precise capturing use<>) — https://blog.rust-lang.org/2024/10/17/Rust-1.82.0/
- RFC 3617 — Precise capturing — https://rust-lang.github.io/rfcs/3617-precise-capturing.html
- Unstable Book — impl_trait_in_assoc_type — https://doc.rust-lang.org/nightly/unstable-book/language-features/impl-trait-in-assoc-type.html
- Unstable Book — type_alias_impl_trait — https://doc.rust-lang.org/nightly/unstable-book/language-features/type-alias-impl-trait.html
- std — attr.define_opaque (nightly) — https://doc.rust-lang.org/nightly/std/prelude/v1/attr.define_opaque.html
- Tracking issue for RFC 2515 (TAIT / ATPIT) #63063 — https://github.com/rust-lang/rust/issues/63063
- Stabilize ATPIT — PR #120700 (open/blocked) — https://github.com/rust-lang/rust/pull/120700
- Add #[define_opaques] attribute — PR #128440 — https://github.com/rust-lang/rust/pull/128440
- Stabilize the next-generation trait solver — Project Goal #113 — https://github.com/rust-lang/rust-project-goals/issues/113
- Types Team Update and Roadmap — https://blog.rust-lang.org/2024/06/26/types-team-update/
- The Async Trait Problem: What Finally Works in 2026 — https://wrenlearnsrust.com/posts/async-traits-2026.html
- adt_const_params — Rust Unstable Book — https://doc.rust-lang.org/nightly/unstable-book/language-features/adt-const-params.html
- Tracking Issue for adt_const_params / unsized_const_params #95174 — https://github.com/rust-lang/rust/issues/95174
- unsized_const_params — Rust Unstable Book — https://doc.rust-lang.org/unstable-book/language-features/unsized-const-params.html
- adt_const_params + &'static str regression #138657 — https://github.com/rust-lang/rust/issues/138657
- Tracking Issue for generic_const_exprs #76560 — https://github.com/rust-lang/rust/issues/76560
- Tracking Issue for min_generic_const_args #132980 — https://github.com/rust-lang/rust/issues/132980
- Conflicting trait impl with generic_const_exprs #92186 — https://github.com/rust-lang/rust/issues/92186
- Tracking issue for RFC 2632 const_trait_impl #67792 (superseded) — https://github.com/rust-lang/rust/issues/67792
- Tracking issue for const traits / RFC 3762 #143874 — https://github.com/rust-lang/rust/issues/143874
- Const Traits — Rust Project Goals 2026 — https://rust-lang.github.io/rust-project-goals/2026/const-traits.html
- Full Const Generics — Rust Project Goals 2026 — https://rust-lang.github.io/rust-project-goals/2026/const-generics.html
- Rust 1.85.0 changelog (async closures) — https://releases.rs/docs/1.85.0/
- Stabilize async closures (RFC 3668) — #133596 — https://github.com/rust-lang/rust/issues/133596
- RFC 3668: async closures — https://rust-lang.github.io/rfcs/3668-async-closures.html
- Higher-ranked lifetime error with async closures #134997 — https://github.com/rust-lang/rust/issues/134997
- AsyncDrop in std::future (nightly docs) — https://doc.rust-lang.org/nightly/std/future/trait.AsyncDrop.html
- Tracking Issue for async drop codegen #126482 — https://github.com/rust-lang/rust/issues/126482
- support async drop trait #100280 — https://github.com/rust-lang/rust/issues/100280
- AsyncDrop without sync Drop generates an error — PR #142606 — https://github.com/rust-lang/rust/pull/142606
- Notes on Rust async drop (tokio shutdown crash) — https://www.monkeynut.org/async-drop/
- Tracking issue for RFC 2033: experimental coroutines #43122 — https://github.com/rust-lang/rust/issues/43122
- Tracking Issue for gen blocks and functions #117078 — https://github.com/rust-lang/rust/issues/117078
- gen blocks — Rust Unstable Book — https://doc.rust-lang.org/unstable-book/language-features/gen-blocks.html
- Tracking Issue for async_iterator #79024 — https://github.com/rust-lang/rust/issues/79024
- AsyncIterator in std::async_iter (nightly docs) — https://doc.rust-lang.org/nightly/std/async_iter/trait.AsyncIterator.html
- RFC 2996: async iterator — https://rust-lang.github.io/rfcs/2996-async-iterator.html
- dynosaur — dyn-compatible variant for async traits — https://docs.rs/dynosaur/latest/dynosaur/
- Dyn Async Traits series — Niko Matsakis — https://smallcultfollowing.com/babysteps/series/dyn-async-traits/
- Announcing Rust 1.92.0 (2025-12-11) — https://blog.rust-lang.org/2025/12/11/Rust-1.92.0/
- Rust 1.94.0 changelog (2026-03-05) — https://releases.rs/docs/1.94.0/
- Tracking issue for the #[used] attribute #40289 — https://github.com/rust-lang/rust/issues/40289
- Rust 1.30.0 changelog — https://releases.rs/docs/1.30.0/
- Announcing Rust 1.30 — https://blog.rust-lang.org/2018/10/25/Rust-1.30.0/
- RFC 2386: #[used] attribute — https://rust-lang.github.io/rfcs/2386-used.html
- Application binary interface — Rust Reference — https://doc.rust-lang.org/reference/abi.html
- Tracking Issue for used_with_arg #93798 — https://github.com/rust-lang/rust/issues/93798
- linkme crate documentation — https://docs.rs/linkme/latest/linkme/
- linkme on crates.io (v0.3.36, MSRV 1.71) — https://crates.io/crates/linkme
- dtolnay/linkme README — https://github.com/dtolnay/linkme
- linkme #49 — encapsulation symbol retention under --gc-sections — https://github.com/dtolnay/linkme/issues/49
- linkme #36 — dependency-crate members discarded (rust-lang/rust#67209) — https://github.com/dtolnay/linkme/issues/36
- linkme #6 — WASM support — https://github.com/dtolnay/linkme/issues/6
- inventory crate documentation — https://docs.rs/inventory/latest/inventory/
- inventory on crates.io (v0.3.24, MSRV 1.68) — https://crates.io/crates/inventory
- ctor crate documentation — https://docs.rs/ctor/latest/ctor/
- FAQ: Life-before and life-after main (rust-ctor wiki) — https://github.com/mmastrac/rust-ctor/wiki/FAQ:-Life%E2%80%90before-and-life%E2%80%90after-main
- There Is Life Before Main in Rust (2026-06-11) — https://grack.com/blog/2026/06/11/life-before-main/
- Linker garbage collection (MaskRay) — https://maskray.me/blog/2021-02-28-linker-garbage-collection
- Custom section generation under wasm32-unknown-unknown #56639 — https://github.com/rust-lang/rust/issues/56639
- alloc: stabilise Allocator — PR #156882 (reverted to draft 2026-06-01) — https://github.com/rust-lang/rust/pull/156882
- Tracking issue for allocator_api #32838 — https://github.com/rust-lang/rust/issues/32838
- The State of Allocators in 2026 (cetra3) — https://cetra3.github.io/blog/state-of-allocators-2026/
- allocator-api2 — Allocator API on stable Rust — https://docs.rs/allocator-api2
- bumpalo — fast bump arena — https://github.com/fitzgen/bumpalo
- Tracking Issue for ptr_metadata #81513 — https://github.com/rust-lang/rust/issues/81513
- ptr_metadata — Rust Unstable Book — https://doc.rust-lang.org/nightly/unstable-book/library-features/ptr-metadata.html
- Tracking issue for DST coercions (coerce_unsized, unsize) #27732 — https://github.com/rust-lang/rust/issues/27732
- Stabilization proposal: Unsize trait (Rust Internals) — https://internals.rust-lang.org/t/stabilization-proposal-unsize-trait/23827
- std::marker::derive.CoercePointee (nightly) — https://doc.rust-lang.org/std/marker/derive.CoercePointee.html
- Tracking issue for derive_coerce_pointee #123430 — https://github.com/rust-lang/rust/issues/123430
- Stabilize derive(CoercePointee) — PR #133820 (open, not merged) — https://github.com/rust-lang/rust/pull/133820
- **Remove support for `dyn*` from the compiler — PR #143036 (merged, 1.90.0)** — https://github.com/rust-lang/rust/pull/143036
- **compiler/rustc_feature/src/removed.rs (dyn_star removed entry)** — https://github.com/rust-lang/rust/blob/master/compiler/rustc_feature/src/removed.rs
- Tracking issue for dyn-star #102425 (now closed) — https://github.com/rust-lang/rust/issues/102425
- Tracking Issue for async_fn_in_dyn_trait #133119 — https://github.com/rust-lang/rust/issues/133119
- Announcing Rust 1.84.0 (drop the principal of trait objects) — https://blog.rust-lang.org/2025/01/09/Rust-1.84.0/
- async-trait crate — https://github.com/dtolnay/async-trait
- Rust 1.91.0 release announcement (const TypeId::of) — https://blog.rust-lang.org/2025/10/30/Rust-1.91.0/
- std::any::TypeId docs — https://doc.rust-lang.org/std/any/struct.TypeId.html
- core::any::TypeId nightly docs — https://doc.rust-lang.org/nightly/core/any/struct.TypeId.html
- PR #144133 Stabilize const TypeId::of — https://github.com/rust-lang/rust/pull/144133
- Tracking Issue for const fn type_id #77125 — https://github.com/rust-lang/rust/issues/77125
- revert const_type_id stabilization — PR #77083 — https://github.com/rust-lang/rust/pull/77083
- Tracking Issue for comparing TypeId in const #101871 — https://github.com/rust-lang/rust/issues/101871
- Comparison of TypeIds in const context #73900 — https://github.com/rust-lang/rust/issues/73900
- Tracking issue for const fn type_name #63084 — https://github.com/rust-lang/rust/issues/63084
- std::any::type_name_of_val docs — https://doc.rust-lang.org/stable/std/any/fn.type_name_of_val.html
- Tracking issue for any::type_name_of_val #66359 — https://github.com/rust-lang/rust/issues/66359
- RFC 1849 non-static TypeId (retracted) — https://rust-lang.github.io/rfcs/1849-non-static-type-id.html
- Tracking issue for non_static_type_id #41875 (retracted) — https://github.com/rust-lang/rust/issues/41875
- dtolnay/typeid crate — https://github.com/dtolnay/typeid
- **Tracking Issue for error_generic_member_access #99301 (FCP cancelled 2026-02-26)** — https://github.com/rust-lang/rust/issues/99301
- **error_generic_member_access — Rust Unstable Book** — https://doc.rust-lang.org/unstable-book/library-features/error-generic-member-access.html
- Tracking Issue for Provider API #96024 (superseded) — https://github.com/rust-lang/rust/issues/96024
- Diagnostic attributes — Rust Reference — https://doc.rust-lang.org/reference/attributes/diagnostics.html
- Announcing Rust 1.78.0 (#[diagnostic::on_unimplemented]) — https://blog.rust-lang.org/2024/05/02/Rust-1.78.0/
- RFC 3368: diagnostic attribute namespace — https://rust-lang.github.io/rfcs/3368-diagnostic-attribute-namespace.html
- Announcing Rust 1.85.0 and Rust 2024 (#[diagnostic::do_not_recommend]) — https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/
- Stabilize #[diagnostic::do_not_recommend] #133679 (PR #132056) — https://github.com/rust-lang/rust/issues/133679
- Rust 1.88.0 changelog (let_chains, proc_macro Span APIs) — https://releases.rs/docs/1.88.0/
- Rust 1.88.0 release blog post — https://blog.rust-lang.org/2025/06/26/Rust-1.88.0
- Stabilize let chains in the 2024 edition #139951 (PR #132833) — https://github.com/rust-lang/rust/issues/139951
- Stabilize if let guards — PR #141295 — https://github.com/rust-lang/rust/pull/141295
- Rust 1.95.0 changelog (if let guards, all editions) — https://releases.rs/docs/1.95.0/
- Announcing Rust 1.95.0 — https://blog.rust-lang.org/2026/04/16/Rust-1.95.0/
- try_blocks — Rust Unstable Book — https://doc.rust-lang.org/nightly/unstable-book/language-features/try-blocks.html
- Tracking issue for ? operator and try blocks #31436 — https://github.com/rust-lang/rust/issues/31436
- Tracking issue for promoting ! to a type (RFC 1216) #35121 — https://github.com/rust-lang/rust/issues/35121
- stabilize never type — PR #155499 (FCP June 2026, not merged) — https://github.com/rust-lang/rust/pull/155499
- Never type fallback change — Rust Edition Guide (2024) — https://doc.rust-lang.org/edition-guide/rust-2024/never-type-fallback.html
- Tracking issue for proc_macro::Span inspection APIs #54725 — https://github.com/rust-lang/rust/issues/54725
- Stabilize proc_macro::Span::{file, local_file} — PR #140514 — https://github.com/rust-lang/rust/pull/140514
- Stabilize proc_macro::Span::{start,end,line,column} — PR #139865 — https://github.com/rust-lang/rust/pull/139865
- Tracking Issue: Procedural Macro Diagnostics (RFC 1566) #54140 — https://github.com/rust-lang/rust/issues/54140
- Implement #[proc_macro_warning] to generate LintId — PR #135432 (not merged) — https://github.com/rust-lang/rust/pull/135432
- Diagnostic in proc_macro (nightly API docs) — https://doc.rust-lang.org/proc_macro/struct.Diagnostic.html
- proc_macro_diagnostic — Rust Unstable Book — https://doc.rust-lang.org/beta/unstable-book/library-features/proc-macro-diagnostic.html
- Design meeting 2025-01-15: Const trait impls — https://hackmd.io/@rust-lang-team/S1WHyOSP1g
