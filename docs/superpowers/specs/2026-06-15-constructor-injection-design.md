# Constructor injection via a trait — design

Status: **approved (design), pending spec review → implementation plan**
Date: 2026-06-15

## Problem

leaf's stereotype/`#[runner]`/`#[auto_config]` macros derive injection from **struct
fields**: `fields_to_deps` lowers *every* field to a dependency and `emit_provider`
calls `Ty::new(all_fields)`. Two consequences:

1. **No internal state.** A bean cannot hold a non-bean field (`AtomicI64` counter,
   seeded `Vec`) — the macro tries to *inject* it as a bean and fails. State-holding
   beans are forced onto `register_component!` (construct-via-`new()`, no injection),
   so a real repository can't wear `#[repository]`.
2. **Name-based handle detection.** `produced_ty` decides "this field is a `Ref<T>`
   handle, resolve `T`" by matching `seg.ident == "Ref"`. A proc-macro sees only
   tokens, and `Ref` is aliasable/re-exportable (`use leaf::Ref as R;`, `crate::Ref`,
   a glob), so name-matching is fragile. **Hard rule: leaf macros must never decide
   semantics from a type's textual name.**

`#[bean]` methods already avoid (1) — they inject via method *parameters*, decoupled
from the produced type's fields — but they still hit (2) (`method_deps` →
`produced_ty`). The fix generalizes the `#[bean]`-method model to all injection points
and replaces name-matching with trait dispatch.

## Design overview

Two pieces:

1. **An `Injectable` trait** carries, per parameter/field type, *how to resolve it*.
   Trait dispatch — never name matching — handles `Ref<T>`/`Lookup<T>`/… semantics, so
   aliases are irrelevant. It exposes both a **const dependency descriptor** (for the
   static wave-planner, so the dependency graph is known before instantiation) and an
   **async `inject(ctx)`** (runtime resolution).

2. **An `#[inject]` constructor**, processed by the existing impl-level `#[advisable]`
   macro. The constructor's *parameters* are the injection points; it is called on
   instantiation; its body initializes state fields with ordinary Rust. If a bean has
   no `#[inject]` constructor, the stereotype falls back to today's field injection
   (also routed through `Injectable`).

Net effect: `#[repository] struct OrderRepository { next_id: AtomicI64 }` +
`#[advisable] impl OrderRepository { #[inject] fn new() -> Self { … } }` works (the
constructor seeds state; zero injected params), subsuming `register_component!`. And a
mixed bean `#[service] struct OrderService { catalog: Ref<CatalogService>, hits:
AtomicU64 }` + `#[advisable] impl { #[inject] fn new(catalog: Ref<CatalogService>) ->
Self { Self { catalog, hits: AtomicU64::new(0) } } }` injects `catalog` and seeds
`hits`.

## Component: the `Injectable` trait (leaf-core)

```rust
/// A type obtainable from the container as a constructor parameter (or injected
/// field). Trait dispatch decides HOW each is resolved — never type-name matching —
/// so aliases/re-exports of the handle types are irrelevant.
pub trait Injectable: Sized + Send + Sync + 'static {
    /// The static dependency this parameter contributes to the wave-planner: the
    /// resolvable target (TypeId), cardinality, and strictness. A const so the
    /// dependency graph is known before any instantiation (cycle detection,
    /// whole-graph validation, wave ordering). Built with the const `TypeId::of`
    /// (stable 1.91, already used in the codebase).
    const DEPENDENCY: Resolvable;

    /// Obtain the value from the container at instantiation.
    fn inject<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Self, LeafError>>;
}
```

`Resolvable` is the type-derived subset of the existing `InjectionPoint` (produced
`TypeId` + `CollectionShape` cardinality + strictness/optionality); the macro combines
it with the *name* (and any `@Qualifier`) it reads structurally from the parameter.

Impls live in leaf-core for the **handle family only** (coherence forbids a blanket
`impl<T: Bean> Injectable for T` alongside the handle impls):

- `impl<T: Bean> Injectable for Ref<T>` — `DEPENDENCY = single(TypeId::of::<T>(),
  Required)`; `inject` resolves `T` (Strict, Single) and wraps it in `Ref`.
- `impl<T: Bean> Injectable for Lookup<T>` — deferred/optional: `DEPENDENCY` marks `T`
  a *soft* dependency (the planner does not force `T` to exist); `inject` builds the
  `Lookup` handle (always `Ok` — resolution happens later via `get_if_available`).
- `impl<T: Bean> Injectable for LazyRef<T>` — deferred eager-single, resolved on first
  use; `DEPENDENCY` as a soft/required single per its existing semantics.
- `impl<T: Bean> Injectable for Inject<T>` — per `Inject`'s existing semantics
  (confirm against `leaf-core/src/injection.rs` during implementation).
- Collection forms (e.g. a multi-injection handle over `T`) follow the same pattern
  with `CollectionShape`.

A **bare-typed** parameter (`db: Database`) is therefore *not* `Injectable` — a clear
compile error steering to `Ref<Database>` (the established handle currency). This is
intentional: no bare-type injection, no name-based escape hatch.

## Component: the `#[inject]` constructor + `#[advisable]`

`#[inject]` is an inner **marker** (like `#[transactional]`/`#[cacheable]`), added to
`CONCERN_ATTRS`. A standalone `#[inject]` outside an `#[advisable]` impl is a
hard-error-with-hint (mirrors `concern_marker_only`). The impl-level `#[advisable]`
macro (kept under that name) processes it:

- Find the lone method marked `#[inject]` whose return is `Self`/the impl's self-type
  (the **constructor**). More than one `#[inject]` constructor is a Tier-0
  `compile_error!`.
- Lower each constructor **parameter** to an `InjectionPoint` = `<ParamTy as
  Injectable>::DEPENDENCY` (type-derived) + the parameter's binding name (structural).
  This is the per-bean `InjectionPlan`.
- Emit a provider whose `provide` awaits `<P_i as Injectable>::inject(ctx)` for each
  param in order, then calls `Self::new(p_1, …, p_n)`. The constructor body initializes
  state fields.
- Submit the provider's `ProviderSeed` + the `InjectionPlan` into `SEED_PAIRINGS` /
  `INJECTION_PLAN_PAIRINGS`, keyed by the impl self-type's `ContractId`.

Mechanics rationale (why not a method-only macro): a method-position attribute macro
(a) cannot see its enclosing `impl`'s self-type (sees `Self`, not `OrderService`) and
(b) cannot emit the module-scope `#[distributed_slice]` rows the wiring needs. The
impl-level macro sees both. This is exactly today's `#[advisable]`/`#[transactional]`
split.

Method/setter injection (a `#[inject]` fn that is *not* the constructor) is a
deliberate **future** extension on the same marker, out of scope here.

## Component: the stereotype macro + the merge

The stereotype macro (`struct_input` for `#[component]`/`#[service]`/`#[repository]`/
`#[controller]`/`#[configuration]`, plus `runner_input`, `auto_config_input`) keeps
emitting the **descriptor** (identity, role, slice). It ALSO emits a **default
field-injection** provider/plan (today's behavior, now routed through `Injectable`):
each field's `InjectionPoint` = `<FieldTy as Injectable>::DEPENDENCY` + field name, and
the provider calls `Self::new(all_fields)`.

The `#[advisable]`-emitted constructor provider/plan and the struct's field-default are
JOINed by `ContractId` in `Application::collect_from_slices` / the assembly pass. **The
constructor pairing wins** when present (an explicit `merge_by_contract` precedence:
`#[inject]`-constructor rows override the struct field-default rows). A bean with state
fields therefore *must* supply an `#[inject]` constructor — the field-default would try
to inject a state field and fail with a loud `NoSuchBean`, which is correct.

## Where it applies

Every struct/constructor-injection surface routes through `Injectable`:
`struct_input` (5 stereotypes), `register_input`/`register_input_with`
(`register_component!`), `runner_input` (`#[runner]`), `auto_config_input`
(`#[auto_config]` struct form), `config_method_input`/`method_deps` (`#[bean]`
methods), and `emit_injection_points`. `register_component!` is preserved as a thin,
possibly-deprecated alias — it is now equivalent to a stereotype with a zero-param
`#[inject]` constructor.

## Cleanup (the no-type-names rule)

- Remove the name-based handle detection: `produced_ty`'s `seg.ident == "Ref"` in BOTH
  `descriptor.rs:461` and `stereotype.rs:477`. Resolution is the trait's job; the macro
  passes the parameter/field type to `<Ty as Injectable>` verbatim.
- **Follow-up (flagged, not in this spec's scope):** the same anti-pattern lives in
  `config.rs`/`validate.rs` (`seg.ident == "Vec"` — collection-shape detection for
  binding/validation) and `concern.rs` (`seg.ident == "Result"` — return-type
  unwrapping). These are binding/return concerns, not injection; they want the same
  trait-based treatment in a later pass. (`.ident.to_string()` uses that read a type's
  *name as the bean name* are legitimate and stay.)

## Runtime data flow

1. Link time: `#[component]` struct → descriptor + field-default rows; `#[advisable]` +
   `#[inject]` → constructor provider/plan rows. Both into the linkme slices.
2. Assembly (`collect_from_slices`): JOIN by `ContractId`; constructor rows override
   field-default rows. The `InjectionPlan` (from `Injectable::DEPENDENCY`) feeds the
   wave-planner (cycle detection, ordering, whole-graph validation) — no instantiation.
3. Instantiation: the provider awaits `<P_i as Injectable>::inject(ctx)` per param,
   then `Self::new(resolved…)`; the constructor body seeds state.

## Error handling

- A constructor parameter typed as a non-`Injectable` (a bare bean type, or a non-bean)
  → a clear trait-bound error at the user's `#[inject]` site, steering to `Ref<T>`.
- A field-default bean with a state field → `NoSuchBean` at resolution (correct; use an
  `#[inject]` constructor).
- More than one `#[inject]` constructor in an impl, or `#[inject]` outside `#[advisable]`
  → Tier-0 `compile_error!` with a hint.
- All resolution failures ride the existing single `LeafError` causal chain.

## Backward compatibility & migration

- **Existing beans are unchanged.** Today's `#[component] struct Foo { a: Ref<A> }` +
  `fn new(a)` has all-`Ref` fields → the field-default path resolves them via
  `Injectable` (same `TypeId`, same `Self::new(a)` call). No source change, no behavior
  change.
- **`register_component!`** keeps working (subsumed; thin alias).
- **The storefront repositories** migrate from `register_component!` to real
  `#[repository]` + `#[advisable] impl { #[inject] fn new() … }` as the proof.
- The `Vec`/`Result` name-detection cleanup is a separate, later pass.

## Testing strategy

- leaf-core unit tests for each `Injectable` impl: `DEPENDENCY` (TypeId/cardinality/
  strictness) and `inject` (resolves/wraps; `Lookup`/`LazyRef` are always-constructible
  deferred handles).
- leaf-codegen token tests: `#[advisable]` emits the constructor provider/plan from the
  `#[inject]` params (not the struct fields); the stereotype still emits the descriptor
  + field-default; the merge precedence; the error paths (two ctors, marker outside
  `#[advisable]`).
- End-to-end (leaf-boot tests + the storefront): a stateful `#[repository]` resolves and
  carries its state; a mixed `#[service]` injects deps + seeds state; existing
  field-injection beans stay green (backward-compat); the wave-planner sees the
  `Injectable`-derived dependencies (cycle/validation unaffected).

## Decisions made

- Attribute: `#[inject]` (JSR-330 flavor) — not `#[autowired]`.
- Impl macro: keep the name `#[advisable]` (it now covers construction + advice).
- Mechanism: trait dispatch (`Injectable`) with a const dependency descriptor — no
  type-name matching anywhere in injection.
- Scope this round: constructor injection across all struct-injection points + the
  `produced_ty` cleanup. Method/setter injection and the `Vec`/`Result` name-detection
  cleanups are explicit follow-ups.
