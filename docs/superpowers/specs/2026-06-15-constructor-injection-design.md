# Constructor injection via a referenced constructor — design

Status: **approved (design), superseded the `#[inject]`-marker approach 2026-06-16**
Date: 2026-06-15 (revised 2026-06-16)

> **Revision note (2026-06-16).** The first cut of this design used an `#[inject]`
> marker on a constructor, lowered by the impl-level `#[advisable]` macro, with a
> runtime merge that let the constructor pairing override the struct field-default.
> It was implemented through Task 5 (commits `774dfce`..`231c831`) but hit a wall at
> the storefront migration: a stereotype on a *state-holding* struct still emitted a
> field-default provider (`R::new(all_fields)`) that cannot compile, because the
> struct macro cannot see the `#[inject]` constructor in the separate `impl` to
> suppress it. The fix below removes that whole class of problem by having the
> **stereotype macro reference the constructor by path** (an attribute it can read on
> the struct itself), resolving its parameters through a "magic constructor" trait
> and type inference — never parsing or introspecting the constructor. Feasibility
> was confirmed with a standalone spike (inference picks arities 0/1/2 from a bare
> `Type::new` value, no turbofish). **Tasks 1, 4, 6 (the `Injectable` trait,
> field-injection-through-`Injectable`, the `produced_ty` deletion, and the `#[bean]`
> migration) stand. Tasks 2, 3, 5 (the `#[inject]` marker, the `#[advisable]`
> constructor lowering, the runtime merge) are removed.**

## Problem

leaf's stereotype macros derive injection from **struct fields**: every field is
lowered to a dependency and the provider calls `Self::new(all_fields)`. Two original
consequences (and their current status):

1. **No internal state.** A bean cannot hold a non-bean field (`AtomicI64` counter,
   seeded `Vec`) — the field-default tries to *inject* it as a bean and fails to
   compile (the field is not `Injectable`, and the all-fields arity does not match a
   hand-written constructor). State-holding beans are forced onto
   `register_component!`, so a real repository cannot wear `#[repository]`. **Still
   open — this design closes it.**
2. **Name-based handle detection** (`produced_ty`'s `seg.ident == "Ref"`). **Closed**
   — Tasks 1/4/6 routed all injection lowering through the `Injectable` trait and
   deleted the name-checks. (`rg 'seg.ident == "Ref"' crates/leaf-codegen/src` is
   clean of live code.)

A hard, standing rule frames the fix: **leaf macros must never decide semantics from
a type's textual name** — resolution is trait dispatch, not token matching.

## Design overview

Two construction paths, the first kept exactly as today, the second new:

1. **Field injection — the default, no constructor required.** A stereotype with no
   `constructor` argument lowers each field to an injection point via `<FieldTy as
   Injectable>::RESOLVABLE` and constructs the bean from its fields (today's behavior,
   already routed through `Injectable`). Field attributes (`@Qualifier`, names) are
   visible to the macro and continue to work. This is the path for ordinary beans
   whose every field is a dependency.

2. **A referenced constructor — opt-in, for state-holding or complex beans.** The
   stereotype carries `constructor = <path>` (e.g. `#[repository(constructor =
   OrderRepository::new)]`). The macro emits a provider that calls `construct_with(<path>,
   ctx)` and a plan that calls `ctor_deps(<path>)`; a per-arity `InjectableCtor` trait
   plus type inference resolve the constructor's *parameters* through `Injectable` and
   then call it. The constructor's body builds the struct, state and all. The presence
   of the argument tells the macro to **skip the field-default**, so a state-holding
   struct compiles.

Net effect: `#[repository] struct OrderRepository { next_id: AtomicI64 }` was
impossible; `#[repository(constructor = OrderRepository::new)]` + `fn new() -> Self {
… }` works (the constructor seeds state; zero injected params), subsuming
`register_component!`. A mixed bean `#[service(constructor = OrderService::new)] struct
OrderService { catalog: Ref<CatalogService>, hits: AtomicU64 }` + `fn new(catalog:
Ref<CatalogService>) -> Self { Self { catalog, hits: AtomicU64::new(0) } }` injects
`catalog` (by type, via `Injectable`) and seeds `hits`.

## Component: the `Injectable` trait (leaf-core) — unchanged, in place

Per-parameter/field resolution. Trait dispatch decides HOW each type is resolved — never
type-name matching. Exposes a const dependency descriptor (`RESOLVABLE`, for the static
wave-planner) and an async `inject(ctx)`. Impls live in leaf-core for the handle family
only (`Ref<T>`, `Lookup<T>`, `LazyRef<T>`, …). A bare-typed parameter is therefore not
`Injectable` — a clear compile error steering to `Ref<T>`. **This component is already
implemented (Task 1, commit `774dfce`) and is the primitive both construction paths build
on.**

## Component: the `InjectableCtor` trait + inference drivers (leaf-core) — new

A "magic constructor" trait, implemented once per arity, lets the macro reference a
constructor by path without ever seeing its parameter list:

```rust
/// Implemented for any `Fn(P1, …, Pn) -> T` whose every parameter is `Injectable`.
/// `Args` is the parameter tuple; the per-arity impls make `Args`/`T` inferable from a
/// bare `Type::new` value (proven by spike — no turbofish needed at the call site).
pub trait InjectableCtor<Args, T>: Sized {
    /// Resolve every parameter via `Injectable`, then call the constructor.
    fn construct<'a>(self, ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<T, LeafError>>;
    /// The static dependency plan (each parameter's `Injectable::RESOLVABLE`), for the
    /// wave-planner. Read from the fn value at assembly — before any instantiation.
    fn deps(&self) -> Vec<InjectionPoint>;
}

// arity 0:
impl<F, T> InjectableCtor<(), T> for F where F: Fn() -> T + Send + Sync + 'static, T: Send + 'static { … }
// arity 1:
impl<F, T, P1> InjectableCtor<(P1,), T> for F
where F: Fn(P1) -> T + Send + Sync + 'static, T: Send + 'static, P1: Injectable { … }
// … through a chosen max arity N (e.g. 12), generated by a small declarative macro.
```

Two free functions are the inference drivers the stereotype macro emits (they exist so the
macro never spells `Args`/`T`):

```rust
pub fn construct_with<'a, F, Args, T>(ctor: F, ctx: &'a ResolveCtx<'a>)
    -> BoxFuture<'a, Result<T, LeafError>> where F: InjectableCtor<Args, T> { ctor.construct(ctx) }
pub fn ctor_deps<F, Args, T>(ctor: F) -> Vec<InjectionPoint> where F: InjectableCtor<Args, T> { ctor.deps() }
```

The coherence concern is settled: each arity is a distinct `InjectableCtor<Args, _>`
instantiation (`()`, `(P1,)`, `(P1, P2)`, …), so the blanket impls do not overlap, and a
fn item has exactly one arity (the spike compiles all three at once). Send/Sync bounds keep
the produced `BoxFuture: Send`.

## Component: the stereotype macro + the `constructor` argument

The stereotype macros (`struct_input` for `#[component]`/`#[service]`/`#[repository]`/
`#[controller]`/`#[configuration]`) parse an optional `constructor = <path>` argument from
their own attribute:

- **Argument absent →** today's field-injection path (each field via `Injectable`,
  field attributes honored). Unchanged.
- **Argument present →** the macro emits a provider whose `provide` returns
  `construct_with(<path>, ctx)` and a plan thunk `ctor_deps(<path>)`, keyed by the bean's
  `ContractId`, and **does not emit the field-default**. `<path>` accepts either a full
  path (`OrderRepository::new`) or a bare method name (`new` → `Self::new` via the struct
  type the macro is attached to).

Because the argument lives on the struct's attribute, the macro reads it directly — there
is no struct-cannot-see-the-`impl` problem, and there is exactly **one** provider per bean
(no runtime merge). `register_component!` is preserved as a thin alias: it is equivalent to
a stereotype with `constructor = new` over a zero-parameter `new()`.

`#[runner]`/`#[auto_config]` keep field injection for now; adding `constructor = …` to them
is a trivial, deferred follow-up if a stateful runner/auto-config appears.

## Limitation (deliberate): by-type resolution for referenced constructors

Because the macro references the constructor rather than parsing it, it cannot see the
parameters' **names** or `@Qualifier`/`@Primary` attributes. A `constructor = <path>`
bean therefore resolves each parameter **by type** through `Injectable` (the planner gets
the `RESOLVABLE` descriptor; injection-point names are positional, e.g. `arg0`). Qualified
or by-name injection uses the **field-injection path**, where the struct macro parses the
fields and their attributes. State-holding beans do not need qualifiers, so this does not
block the current goal. A parsed-constructor form that recovers per-parameter qualifiers is
a possible future extension on the same `constructor =` surface.

## Where it applies

Field injection (default) and `constructor = <path>` (opt-in) cover the five stereotypes
via `struct_input`. `register_component!` becomes the `constructor = new` alias. `#[bean]`
methods already inject by parameter (parsed, qualifier-aware) through `Injectable` (Task 6)
and are unchanged. `emit_injection_points` and the field-default recipe stay for the
no-argument path.

## Runtime data flow

1. Link time: a no-`constructor` stereotype emits descriptor + field-default rows (fields
   via `Injectable`); a `constructor = <path>` stereotype emits descriptor + a single
   provider/plan pair built from `construct_with`/`ctor_deps`.
2. Assembly (`collect_from_slices`): one provider/plan per `ContractId` — no JOIN, no
   precedence. The plan (`ctor_deps(<path>)` or the field `RESOLVABLE`s) feeds the
   wave-planner (cycle detection, ordering, whole-graph validation) with no instantiation.
3. Instantiation: the provider awaits `construct_with(<path>, ctx)` — which awaits each
   parameter's `Injectable::inject` and calls the constructor — or the field-default's
   per-field `inject` + `Self::new(fields)`.

## Error handling

- A constructor parameter typed as a non-`Injectable` (a bare bean type, or a non-bean)
  → a clear trait-bound error steering to `Ref<T>`. (A `#[diagnostic::on_unimplemented]`
  on `Injectable`/`InjectableCtor` can sharpen the message.)
- A constructor whose arity exceeds the generated max → a trait-bound error (raise the max
  if it ever bites).
- A `constructor = <path>` naming a missing/mismatched function → an ordinary unresolved-path
  error at the generated call site.
- All resolution failures ride the existing single `LeafError` causal chain.

## Backward compatibility & migration

- **Existing beans are unchanged.** Today's `#[component] struct Foo { a: Ref<A> }` +
  `fn new(a)` stays on the field-injection path (no `constructor` argument, all-`Ref`
  fields resolved via `Injectable`).
- **`register_component!`** keeps working (the `constructor = new` alias).
- **The storefront repositories** migrate from `register_component!` to
  `#[repository(constructor = …)]` as the proof of the state-holding path.
- The committed `#[inject]` marker, `#[advisable]` constructor lowering, and runtime
  merge-precedence are **removed** (they are superseded by `constructor = <path>`).

## Testing strategy

- leaf-core: an `InjectableCtor` test mirroring the spike — `construct_with(Type::new, ctx)`
  builds for arities 0/1/2 from a bare fn value (no turbofish); `ctor_deps` returns the
  per-parameter `RESOLVABLE`s; a non-`Injectable` parameter fails to compile (trybuild or a
  documented compile-fail).
- leaf-codegen token tests: a `constructor = <path>` stereotype emits a `construct_with`/
  `ctor_deps` provider+plan and **no** field-default; a no-argument stereotype still emits
  the field-default; a bare `new` resolves to `Self::new`.
- End-to-end (leaf-boot + storefront): a stateful `#[repository(constructor = new)]`
  resolves and carries its state; a mixed `#[service(constructor = new)]` injects deps and
  seeds state; existing field-injection beans stay green; the wave-planner sees the
  `ctor_deps`-derived dependencies (cycle/validation unaffected).
- Removal is verified: the `#[inject]` attribute, `emit_wiring_only`, the `from_constructor`
  row flag, and the merge are gone; the full workspace gate stays green.

## Decisions made

- Surface: `constructor = <path>` on the stereotype attribute (full path or bare method →
  `Self::method`). No `#[inject]` marker, no `(inject)` flag, no `#[advisable]`-for-construction.
- Mechanism: a per-arity `InjectableCtor` "magic constructor" trait + `construct_with`/
  `ctor_deps` inference drivers — the macro references the constructor, never parses it.
- Field injection remains the default; `constructor = <path>` is opt-in.
- By-type resolution for referenced constructors (no per-parameter qualifiers) is an
  accepted limitation; qualified injection uses field injection.
- Tasks 1/4/6 stand; Tasks 2/3/5 are removed.
- Follow-ups (flagged, out of scope): `constructor = …` on `#[runner]`/`#[auto_config]`;
  a parsed-constructor form recovering per-parameter qualifiers; the `Vec`/`Result`
  `seg.ident` name-detection cleanup in config.rs/validate.rs/concern.rs.
