# Constructor Injection via a Trait — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let any leaf bean mix injected dependencies with internal state by injecting an `#[inject]` constructor's *parameters* through a trait, with field injection as the fallback — and remove all type-name-based detection from the injection codegen.

**Architecture:** A new `leaf_core::Injectable` trait resolves a parameter/field type by trait dispatch (never by matching `"Ref"` in the tokens): it carries a const `Resolvable` (TypeId + cardinality + strictness, for the static wave-planner) and an async `inject(ctx)` (runtime). The impl-level `#[advisable]` macro lowers an `#[inject]` constructor's params via this trait and emits a provider that calls the constructor; the stereotype macro still emits the descriptor + a field-injection default, which the constructor provider overrides by `ContractId`.

**Tech Stack:** Rust (stable), `syn`/`quote` proc-macros (leaf-codegen/leaf-macros), `linkme` distributed slices, the existing `InjectionPlan`/`InjectionPoint`/`Provider`/`ResolveCtx` model in leaf-core.

**Reference spec:** `docs/superpowers/specs/2026-06-15-constructor-injection-design.md`

---

## File structure

- `crates/leaf-core/src/injectable.rs` — **NEW.** The `Injectable` trait, the `Resolvable` descriptor (type-derived subset of `InjectionPoint`), and impls for `Ref<T>`/`Lookup<T>`/`LazyRef<T>` (+ `SelfRef<T>` if needed). One responsibility: "how a parameter type obtains itself from the container."
- `crates/leaf-core/src/lib.rs` — export `Injectable`, `Resolvable`.
- `crates/leaf-codegen/src/descriptor.rs` — `emit_injection_points` builds each point from `<Ty as Injectable>::RESOLVABLE` + name; `emit_provider` resolves params via `<Ty as Injectable>::inject`; **delete** `produced_ty`'s `seg.ident == "Ref"`.
- `crates/leaf-codegen/src/stereotype.rs` — `fields_to_deps` carries the field's full type (not name-stripped); **delete** the duplicate `produced_ty`.
- `crates/leaf-codegen/src/config_impl.rs` — `#[advisable]` discovers the `#[inject]` constructor and lowers its params; `method_deps`/`config_method_input` route through `Injectable`.
- `crates/leaf-macros/src/lib.rs` — add `inject` to `CONCERN_ATTRS` + a `concern_marker_only` standalone `#[inject]`.
- `crates/leaf-boot/src/application.rs` — `collect_from_slices` merge precedence: constructor seed/plan overrides the struct field-default by `ContractId` (already merges by contract; verify "explicit/ctor wins").
- `examples/storefront/src/order/repository.rs`, `catalog/product_repository.rs` — migrate to `#[repository]` + `#[advisable] impl { #[inject] fn new() }`.

---

### Task 1: The `Injectable` trait + handle impls (leaf-core)

**Files:**
- Create: `crates/leaf-core/src/injectable.rs`
- Modify: `crates/leaf-core/src/lib.rs` (add `pub mod injectable;` + re-export)
- Test: in `injectable.rs` `#[cfg(test)] mod tests`

- [ ] **Step 1: Confirm the exact resolution API.** Read `crates/leaf-core/src/provider.rs` (`ResolveCtx`, `root()`) and `crates/leaf-core/src/injection.rs` around lines 1039–1230 (the `resolve(key, strictness, cardinality)` seam and `Ref`/`Lookup`/`LazyRef` `::new(key, container)` + the `ContainerRef` the ctx exposes). Note the exact method the provider uses today to resolve a `BeanKey` and to obtain a `ContainerRef` from a `&ResolveCtx`. The two unknowns to pin: (a) how a `&ResolveCtx` yields a `ContainerRef` (for the deferred handles), (b) the resolve call that yields a `Published`/`Ref<T>` for the eager handle.

- [ ] **Step 2: Write the failing trait + first impl test.**

```rust
// crates/leaf-core/src/injectable.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::Bean;
    use std::any::TypeId;

    struct Svc;
    impl Bean for Svc {}

    #[test]
    fn ref_resolvable_targets_the_inner_bean_type_single_required() {
        let r = <Ref<Svc> as Injectable>::RESOLVABLE;
        assert_eq!(r.produced, TypeId::of::<Svc>());
        assert_eq!(r.cardinality, Cardinality::Single);
        assert_eq!(r.strictness, Strictness::Strict);
    }

    #[test]
    fn lookup_resolvable_is_a_soft_single_dependency() {
        let r = <Lookup<Svc> as Injectable>::RESOLVABLE;
        assert_eq!(r.produced, TypeId::of::<Svc>());
        // Lookup is deferred/optional: the planner must NOT force Svc to exist.
        assert_eq!(r.strictness, Strictness::FullyTolerant);
    }
}
```

- [ ] **Step 3: Run it, watch it fail.** Run: `cargo test -p leaf-core injectable::tests` — Expected: FAIL (`Injectable`/`Resolvable` undefined).

- [ ] **Step 4: Define the trait + descriptor.**

```rust
//! `Injectable` — how a constructor parameter (or injected field) obtains itself from
//! the container. Trait dispatch, never type-name matching: aliases/re-exports of the
//! handle types are irrelevant. Each impl exposes a const `RESOLVABLE` (the static
//! dependency the wave-planner reads — TypeId + cardinality + strictness, known before
//! instantiation) and an async `inject` (runtime resolution).
use std::any::TypeId;
use crate::future::BoxFuture;
use crate::provider::ResolveCtx;
use crate::error::LeafError;
use crate::injection::{Cardinality, Strictness, Ref, Lookup, LazyRef};
use crate::handle::Bean;

/// The type-derived part of an injection point (the macro adds the param name +
/// qualifiers structurally). `const`-constructible so a plan is known at compile time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolvable {
    pub produced: TypeId,
    pub cardinality: Cardinality,
    pub strictness: Strictness,
}

pub trait Injectable: Sized + Send + Sync + 'static {
    const RESOLVABLE: Resolvable;
    fn inject<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Self, LeafError>>;
}

impl<T: Bean> Injectable for Ref<T> {
    const RESOLVABLE: Resolvable = Resolvable {
        produced: const { TypeId::of::<T>() },
        cardinality: Cardinality::Single,
        strictness: Strictness::Strict,
    };
    fn inject<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Self, LeafError>> {
        // Eager: resolve T (Strict, Single) and hand back the Ref handle.
        // Use the exact resolve seam confirmed in Step 1.
        Box::pin(async move { ctx.resolve_ref::<T>().await })
    }
}

impl<T: Bean> Injectable for Lookup<T> {
    const RESOLVABLE: Resolvable = Resolvable {
        produced: const { TypeId::of::<T>() },
        cardinality: Cardinality::Single,
        strictness: Strictness::FullyTolerant, // deferred/optional
    };
    fn inject<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Self, LeafError>> {
        // Deferred: build the handle from the ctx's ContainerRef + the by-type key.
        Box::pin(async move { Ok(Lookup::new(ctx.key_for::<T>(), ctx.container())) })
    }
}
// LazyRef<T> mirrors Lookup<T> (deferred, eager-single-on-first-use).
```

> Note: `resolve_ref::<T>()` / `key_for::<T>()` / `container()` are the names for whatever the Step-1 reading found on `ResolveCtx`/the resolver. If `ResolveCtx` doesn't expose them, add thin helpers there in this task (they're the seam the generated provider already needs).

- [ ] **Step 5: Add the module.** In `crates/leaf-core/src/lib.rs`: `pub mod injectable;` and re-export `pub use injectable::{Injectable, Resolvable};` beside the other injection re-exports.

- [ ] **Step 6: Run the tests, watch them pass.** Run: `cargo test -p leaf-core injectable::tests` — Expected: PASS.

- [ ] **Step 7: Add `inject` behavior tests** (resolve a registered `Ref<T>` through a test `ResolveCtx`; assert a `Lookup<T>` builds even when `T` is absent — the deferred guarantee). Mirror the resolver stubs at `crates/leaf-core/src/injection.rs` tests (~line 1943). Run + pass.

- [ ] **Step 8: Commit.**

```bash
git add crates/leaf-core/src/injectable.rs crates/leaf-core/src/lib.rs
git commit -m "leaf-core: Injectable trait — trait-based dependency resolution (no type names)"
```

---

### Task 2: `#[inject]` marker (leaf-macros)

**Files:**
- Modify: `crates/leaf-macros/src/lib.rs` (the `CONCERN_ATTRS` list + a standalone `#[inject]` proc-macro)
- Test: `crates/leaf-macros/tests/` (a trybuild/UI test that bare `#[inject]` errors with a hint)

- [ ] **Step 1: Write the failing UI test** that `#[inject] fn new() {}` *outside* an `#[advisable]` impl produces a `compile_error!` mentioning `#[advisable]`. Mirror the existing `#[transactional]`-outside-advisable UI fixture (find it under `crates/leaf-macros/tests/ui/`).

- [ ] **Step 2: Run it, watch it fail** (no `inject` macro yet). Run: `cargo test -p leaf-macros` (the trybuild harness) — Expected: FAIL.

- [ ] **Step 3: Add `inject` to `CONCERN_ATTRS`** (the strip-list `#[advisable]` consumes) and add `#[proc_macro_attribute] pub fn inject` routing to `concern_marker_only` (copy the `transactional` standalone at lib.rs ~774). Its error hint: "`#[inject]` marks the constructor of an `#[advisable]` impl."

- [ ] **Step 4: Run, watch it pass.** Run: `cargo test -p leaf-macros` — Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/leaf-macros/src/lib.rs crates/leaf-macros/tests/
git commit -m "leaf-macros: add #[inject] constructor marker (errors outside #[advisable])"
```

---

### Task 3: `#[advisable]` lowers the `#[inject]` constructor (leaf-codegen)

**Files:**
- Modify: `crates/leaf-codegen/src/config_impl.rs` (`emit_advisable_impl` / `emit_method_concerns`)
- Modify: `crates/leaf-codegen/src/descriptor.rs` (reuse `emit_injection_points`/`emit_provider`, now param-driven)
- Test: `crates/leaf-codegen/src/config_impl.rs` `#[cfg(test)]`

- [ ] **Step 1: Write the failing codegen test** asserting that for
  `#[advisable] impl OrderService { #[inject] fn new(catalog: Ref<CatalogService>) -> Self {…} }`
  the emitter produces (in the flat token string): a per-bean `InjectionPlan` whose single point is built from `< Ref < CatalogService > as :: leaf_core :: Injectable > :: RESOLVABLE` with `name = "catalog"`, and a provider whose `provide` awaits `< Ref < CatalogService > as :: leaf_core :: Injectable > :: inject` then calls `OrderService :: new (...)`, keyed by the `OrderService` `ContractId` into `SEED_PAIRINGS` + `INJECTION_PLAN_PAIRINGS`. Mirror the assertion style of the existing `concern.rs`/`config_impl.rs` token tests.

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p leaf-codegen config_impl::tests::` — Expected: FAIL.

- [ ] **Step 3: Implement.** In `emit_advisable_impl`: scan `item.items` for the lone `#[inject]`-marked `fn` returning `Self`/the self-type (the constructor). If >1, `EmitError`. Lower its params (not the struct's fields) to `Dependency { name: <param ident>, ty: <full param type> }` and feed them to a param-driven variant of `descriptor::emit` that:
  - builds each `InjectionPoint` via `::leaf_core::InjectionPoint { produced: <Ty as Injectable>::RESOLVABLE.produced, arity: …, name: "<param>", … }` (map `Resolvable.cardinality/strictness` onto `arity`/`PointKind`);
  - emits a provider whose `provide` does `let p0 = <P0 as ::leaf_core::Injectable>::inject(ctx).await?; … ; Ok(Published::shared_value(SelfTy::new(p0, …)))`;
  - submits the `ProviderSeed` + `InjectionPlan` pairing rows keyed by `ContractId::of(module::SelfTy)`.

- [ ] **Step 4: Run, watch it pass.** Run: `cargo test -p leaf-codegen config_impl::tests::` — Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/leaf-codegen/src/config_impl.rs crates/leaf-codegen/src/descriptor.rs
git commit -m "leaf-codegen: #[advisable] lowers the #[inject] constructor through Injectable"
```

---

### Task 4: Field-default through `Injectable` + delete `produced_ty` name-check

**Files:**
- Modify: `crates/leaf-codegen/src/descriptor.rs` (`emit_injection_points`, `produced_ty` → removed; `emit_provider`)
- Modify: `crates/leaf-codegen/src/stereotype.rs` (`fields_to_deps`, remove the duplicate `produced_ty`)
- Test: `crates/leaf-codegen/src/descriptor.rs`/`stereotype.rs` `#[cfg(test)]`

- [ ] **Step 1: Write the failing test** asserting that for `#[component] struct Foo { dep: Ref<Bar> }` (no `#[inject]` ctor), the field-default `InjectionPoint` is `produced = <Ref<Bar> as Injectable>::RESOLVABLE.produced` and the provider resolves via `<Ref<Bar> as Injectable>::inject` — i.e. the emitted tokens reference `Injectable`, NOT a name-stripped `Bar` TypeId.

- [ ] **Step 2: Run, watch it fail** (still name-stripping). Run: `cargo test -p leaf-codegen descriptor::tests:: stereotype::tests::` — Expected: FAIL.

- [ ] **Step 3: Implement.** Change `fields_to_deps` to carry the field's *full* type (drop the `produced_ty` call). In `emit_injection_points`/`emit_provider`, build points + resolution from `<FieldTy as Injectable>::{RESOLVABLE, inject}` exactly as Task 3. **Delete** `produced_ty` from both `descriptor.rs:461` and `stereotype.rs:477` and all callers.

- [ ] **Step 4: Run, watch it pass + no regressions.** Run: `cargo test -p leaf-codegen` then `cargo test --workspace` — Expected: PASS (existing all-`Ref` beans resolve identically through the trait). A state-holding `#[component]` with no `#[inject]` ctor now fails to compile/resolve loudly (its state field isn't `Injectable`) — assert that error in a UI/codegen test.

- [ ] **Step 5: Commit.**

```bash
git add crates/leaf-codegen/src/descriptor.rs crates/leaf-codegen/src/stereotype.rs
git commit -m "leaf-codegen: field injection through Injectable; delete produced_ty name-check"
```

---

### Task 5: Merge precedence — constructor overrides field-default (leaf-boot)

**Files:**
- Modify: `crates/leaf-boot/src/application.rs` (`collect_from_slices`, the seed + injection-plan folds, ~lines 571–633)
- Test: `crates/leaf-boot/tests/`

- [ ] **Step 1: Write the failing test** that when BOTH a struct field-default seed/plan AND an `#[inject]`-constructor seed/plan exist for one `ContractId`, the constructor's wins (the resolved bean is built via the constructor; assert via a bean with a state field that only the constructor can build).

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p leaf-boot <test_name>` — Expected: FAIL (ambiguous/duplicate or wrong winner).

- [ ] **Step 3: Implement.** Ensure the `#[inject]`-constructor pairing rows are tagged so `merge_by_contract` selects them over the struct field-default for the same `ContractId` (a precedence flag on the row, or emit the field-default only when no constructor row is linked — settle in this task). Keep unguarded behavior unchanged.

- [ ] **Step 4: Run, watch it pass + full gate.** Run: `cargo test --workspace` — Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/leaf-boot/src/application.rs crates/leaf-boot/tests/
git commit -m "leaf-boot: #[inject]-constructor wiring overrides the struct field-default by ContractId"
```

---

### Task 6: Route `#[bean]` methods + `#[runner]`/`#[auto_config]` through `Injectable`

**Files:**
- Modify: `crates/leaf-codegen/src/config_impl.rs` (`method_deps`, `config_method_input`)
- Modify: `crates/leaf-codegen/src/app.rs` (`runner_input`), `crates/leaf-codegen/src/conditional.rs` (`auto_config_input`)
- Test: the existing `#[bean]`/`#[runner]`/`#[auto_config]` codegen tests

- [ ] **Step 1: Write/extend the failing test** that a `#[bean] fn make(&self, dep: Ref<Db>) -> Pool` lowers `dep` via `<Ref<Db> as Injectable>::RESOLVABLE` (not name-stripping).

- [ ] **Step 2: Run, watch it fail.** Run: `cargo test -p leaf-codegen config_impl::tests::` — Expected: FAIL.

- [ ] **Step 3: Implement.** `method_deps` carries the full param type; the `#[bean]`/runner/auto_config provider emission uses `<Ty as Injectable>::{RESOLVABLE, inject}` (the same helper Task 3/4 built). No name-stripping remains in any injection lowering.

- [ ] **Step 4: Run, watch it pass + full gate.** Run: `cargo test --workspace` — Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add crates/leaf-codegen/src/config_impl.rs crates/leaf-codegen/src/app.rs crates/leaf-codegen/src/conditional.rs
git commit -m "leaf-codegen: route #[bean]/#[runner]/#[auto_config] injection through Injectable"
```

---

### Task 7: Migrate the storefront repositories (the proof) + backward-compat

**Files:**
- Modify: `examples/storefront/src/order/repository.rs`, `examples/storefront/src/catalog/product_repository.rs`
- Test: `examples/storefront/src/tests.rs` (unchanged assertions must stay green)

- [ ] **Step 1: Convert `OrderRepository`** from `register_component!(OrderRepository)` to:

```rust
#[repository]
pub struct OrderRepository { next_id: AtomicI64, saved: AtomicUsize }

#[advisable]
impl OrderRepository {
    #[inject]
    fn new() -> Self { OrderRepository { next_id: AtomicI64::new(1), saved: AtomicUsize::new(0) } }
    // … existing next_id/save/saved_count methods …
}
```

- [ ] **Step 2: Convert `ProductRepository`** the same way (`#[repository]` + `#[advisable] impl { #[inject] fn new() -> Self { … seeded products … } }`).

- [ ] **Step 3: Run the storefront proof.** Run: `cargo test -p storefront` and `cargo run -q -p storefront` — Expected: PASS + the demo still prints the order line (the repos resolve and carry their state).

- [ ] **Step 4: Full backward-compat gate.** Run: `cargo test --workspace` then `cargo clippy --workspace --all-targets -- -D warnings` — Expected: 0 failures, clippy clean (existing field-injection beans untouched).

- [ ] **Step 5: Commit.**

```bash
git add examples/storefront/src/order/repository.rs examples/storefront/src/catalog/product_repository.rs
git commit -m "storefront: repositories use #[repository] + #[inject] constructor (drop register_component!)"
```

---

### Task 8: `register_component!` alias + final sweep

**Files:**
- Modify: `crates/leaf-macros/src/lib.rs` / `crates/leaf-codegen/src/stereotype.rs` (document `register_component!` as the zero-`#[inject]`-param shorthand; keep it working)
- Modify: docs/NOTES mentioning the old field-injection model

- [ ] **Step 1: Confirm `register_component!` still works** (it is now equivalent to a stereotype with a zero-param `#[inject]` constructor) — add a doc comment saying so; leave a deprecation note if desired (do NOT remove it — other crates/tests use it).
- [ ] **Step 2: Grep for stale references** to the old "every field is injected" model in doc comments (`rg -n 'every field|fields_to_deps|produced_ty'`) and update them.
- [ ] **Step 3: Full gate.** Run: `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --workspace --no-deps` — Expected: all clean.
- [ ] **Step 4: Commit.**

```bash
git add -A
git commit -m "Document register_component! as the zero-param #[inject] shorthand; refresh injection docs"
```

---

## Out of scope (tracked follow-ups, not this plan)

- **Method/setter injection** (a `#[inject]` fn that is not the constructor).
- **`Vec`/`Result` name-detection cleanup** in `config.rs`/`validate.rs`/`concern.rs` (collection-shape binding + return-type unwrapping) — the same anti-pattern, different concern.

## Self-review

- **Spec coverage:** Injectable trait (T1) ✓; `#[inject]` marker (T2) ✓; `#[advisable]` constructor lowering (T3) ✓; field-default + `produced_ty` deletion (T4) ✓; merge precedence (T5) ✓; `#[bean]`/`#[runner]`/`#[auto_config]` (T6) ✓; register_component! subsumed (T7/T8) ✓; storefront proof + backward-compat (T7) ✓; deferred follow-ups listed ✓.
- **Placeholder scan:** the only deliberately-deferred specifics are the `ResolveCtx` seam names (`resolve_ref`/`key_for`/`container`), pinned by Task 1 Step 1's reading — flagged as "confirm + add thin helper if absent," not hand-waved.
- **Type consistency:** `Injectable`/`Resolvable`/`RESOLVABLE` used consistently across T1, T3, T4, T6; `InjectionPoint`/`InjectionPlan`/`ProviderSeed`/`ContractId` match leaf-core; `#[advisable]`/`#[inject]` names consistent T2→T7.
