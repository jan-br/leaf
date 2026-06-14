# Expression, Messages & Resources  `[expr-i18n-resources]`

_The runtime expression evaluation engine (#{...} values, condition filtering, cache keys, bean refs), the hierarchy-aware MessageSource i18n bean, and the ResourceLoader / pattern-resolving resource abstraction used by scanning, config loading, and conditions._

This subsystem is the runtime "ask-a-question-get-a-value" layer of leaf: (1) the EXPRESSION engine behind every `#{...}` site — `@Value` value-production, `@ConditionalOnExpression`/`@EventListener(condition=)` boolean filtering, `@Cacheable(key=)` cache keys, and `@beanName`/`&factory` bean references; (2) the hierarchy-aware MessageSource i18n service; (3) the origin-agnostic ResourceLoader / ResourcePatternResolver consumed by component-scanning enumeration, config-data loading, conditions (`OnResource`), and message-bundle loading.

The re-unification thesis is that all three are the SAME shape: a sync-pure (expression/message-format) or cold-async (catalog/file IO) RESOLUTION over (a) a parsed const spec emitted by a thin macro, (b) an ambient `Cx` bundle (locale rides an Inherit `CxKey`; the registry rides bean-refs), and (c) the frozen `Registry`/`Engine`. They do NOT each mint their own interpreter, accessor registry, classpath scanner, or error type. Instead:
- Expressions are MONOMORPHIZED RUST CLOSURES (Phase-1 expression-language R1) lowered by leaf-codegen from a fixed `#{...}` subgrammar; the rare runtime-authored case (R2 interpreter) is dropped from v1 (WASM is gone — its only hard driver) and recovered only as an explicit hand-written `Fn(&EvalCx)`. There is no tree-walking interpreter on the event hot path.
- The MessageSource is an always-present `Context` service (container-shape fixes this as a structural field, not an Option) whose catalogs are `Descriptor`-shaped `Provider`s in the SAME `Registry`, discovered via the SAME `linkme` slice + `ExpectedManifest` anti-DCE self-check, hierarchy-walking via the SAME `Context::parent()` chain.
- The ResourceLoader's `classpath:` arm is a CLOSED, link-collected `ResourceEntry` table (R1) — the resource-shaped twin of component-scanning's discovery, sharing one substrate and one self-check; `file:`/URL arms are real cold-path async IO behind `BoxFuture`.

Every failure is one `LeafError`/`Diagnostic` node; every locale read is one `Holder<LocaleKey>` read; every catalog/resource discovery rides one `linkme`+`ExpectedManifest` pipeline; every bean-ref is one `Engine`/`BeanKey` lookup. The unification is maximal: a "value" produced by an expression, a "string" produced by the MessageSource, and a "byte bag" produced by a Resource are three typed views over the same context-resolution machinery.

## Shared machinery (how this rests on the toolkit)

The whole subsystem rests on the fixed toolkit primitives, sharing them rather than re-minting:

1. ONE ambient substrate `Cx` (async-context-model). The `EvalCx` an expression closure reads is a thin BORROWED view that pulls "the current locale" from the upstream `Holder<LocaleKey>` (`CxKey{NAME="locale", POLICY=Inherit}`), already declared in the execution-context subsystem — NOT a new thread-local, NOT a bespoke locale param. `MessageSource::message(code, args, None)` reads the same `LocaleKey` via `Cx::current()`. Locale-sensitive argument formatting and `${}`-in-location resolution read the same ambient. There is exactly one "current locale", surviving `.await` via the poll-re-installing `Scoped<F>`.

2. ONE creation/identity seam `Provider`/`Descriptor`/`BeanId`/`Engine` (ownership-model, registry-substrate, container-shape). A `MessageCatalogProvider` and a `classpath:` `ResourceEntry`-set are not bespoke registries: catalog beans are `Role::Infrastructure` `Provider`s with their own `Descriptor` rows that `Context::refresh()` auto-detects; the magic-named `messageSource` is just `BeanKey::ByName("messageSource")` resolution with an install-default branch (the same pattern as `applicationEventMulticaster`). Expression `@beanName`/`&factory` is `Engine::get_erased(BeanKey::ByName)` / the `& = ByName + Deref` flag already fixed in container-shape. The MessageSource and ResourceLoader are ALWAYS-PRESENT structural fields on `Context` (container-shape's hard guarantee), injectable via the `ResolvableDependency` terminal layer (injection-mechanics) — no `Aware` setter phase.

3. ONE codegen + anti-DCE pipeline (codegen-boundary, cross-crate-discovery). Thin macros emit const data only: `#[value("#{...}")]`/condition/cache-key attributes lower to a const closure + an `InjectionPoint`-shaped spec via `::leaf_core` paths; `register_catalog!`/`#[resource("path")]` emit one const `CatalogDescriptor`/`ResourceEntry` row into new sibling `linkme` slices `CATALOGS` and `RESOURCES` (joining `COMPONENTS`/`CONDITIONS`/...). The binary-crate `ExpectedManifest` + freeze-time self-check turns a DCE-dropped translation-only or resource-only crate into a loud `AntiDceError::SourceVanished` naming the crate — the SAME defense, not a per-feature one. The expression closure path has ZERO link-time exposure (it compiles where the attribute is written), so only catalogs/resources touch the slice substrate.

4. ONE condition algebra `CondExpr` (conditional-strategy). `OnExpression` and `OnResource` are already minted as Runtime-tier (PARSE sub-pass) `ConditionId` leaves. `OnExpression` evaluates a lowered boolean closure over the sealed `Env` + the `EvalCx`; `OnResource` calls `ResourceLoader::exists` over the frozen entry table. They emit/evaluate the identical `CondExpr::Leaf` — no parallel gating. A condition referencing `@bean` defers the whole guard to the REGISTER sub-pass (max-over-leaves tier inference).

5. ONE async discipline (async-context-model). Expression closures and message formatting are SYNC-PURE by contract (the macro hard-errors on an async body) so they sit directly on the event dispatch / cache-interceptor / refresh hot-and-cold paths with no boxing. The only async seams are catalog IO and `Resource::open`/`resolve_pattern`, which return `BoxFuture` at the `dyn` boundary (cold paths: refresh, first-miss, config load); `classpath:`/embedded reads are ready-futures. A monomorphized typed fast path (`engine.resource().classpath("x")`) is available at known-concrete sites (Phase-3 escape). No async Drop: a directory-watcher/file-handle catalog source's teardown is an awaited `TeardownLedger` entry drained by `Context::shutdown()`.

6. ONE error/diagnostic spine (error-model). `ExprError`, `NoSuchMessage`, and `ResourceError` are NOT new top-level enums — they are constructors of `LeafError` nodes (`ErrorKind::UnresolvedValue`/`Convert` for `@Value`, a new core `ErrorKind::NoSuchMessage`, `ErrorKind::ConfigIo` for resource IO; an `Integration{ContractId}` arm for a custom catalog/provider). A `@Value` strict failure and an `OnResource`/`OnExpression` evaluation outcome aggregate into the `App<Wired>` `AssemblyReport`; runtime evaluation (event-condition, cache-key, message render) produces a `LeafError` rendered by the one `Diagnostic`. `FailureAnalyzer`s ship for NoSuchMessage and resource-not-found.

7. ONE type-conversion neighbour (type-conversion, the sibling subsystem). Expression-to-typed-value and locale-sensitive message-argument formatting both delegate to the `FromConfigValue`/`ConvertCtx` machinery — the expression closure yields a typed value or a `ConfigValue` that conversion coerces (the "evaluate-then-convert" chain), never a bespoke formatter. This subsystem provides the `EvalCx` and the catalog; conversion owns the coercion.

## Features

### `expression-language`
DECISION: typed monomorphized closures (Phase-1 R1) as the SOLE backend; the interpreter (R2) and the hybrid (R3) are DROPPED for v1 because WASM/runtime-authored expressions — their only irreducible driver — are gone, and a tree-walker on the event hot path violates the reactive-hot invariant. Concrete leaf-core types:

```rust
// leaf-core::expr — the shared EvalCx SHAPE (the real unification primitive)
pub struct EvalCx<'a> {
    root:   Option<&'a (dyn std::any::Any + Send + Sync)>, // event/target; macro downcasts to the known concrete type
    args:   &'a [&'a (dyn std::any::Any + Send + Sync)],   // method args (cache keys)
    result: Option<&'a (dyn std::any::Any + Send + Sync)>, // #result for cache `unless`
    env:    &'a Env,                                       // ownership-model Arc<EnvCore> handle, for #{server.port}
    beans:  &'a dyn BeanResolver,                          // @name / &factory
    cx:     Option<&'a Cx>,                                // ambient: locale etc. via Holder<LocaleKey>
}
pub trait BeanResolver: Send + Sync {                     // backed by Engine; @name=ByName, &name=ByName+Deref flag
    fn bean(&self, name: &str) -> Result<ErasedBean, ResolveError>;
    fn factory(&self, name: &str) -> Result<ErasedBean, ResolveError>; // the `&` escape hatch (container-shape)
}
// Output is purpose-typed, NOT one erased Expression<T> (avoids the R3 erasing seam that hurts the hot path):
pub type ValueExpr<T> = fn(&EvalCx) -> Result<T, ExprError>;     // @Value (then type-conversion coerces)
pub type CondExprFn   = fn(&EvalCx) -> Result<bool, ExprError>; // @ConditionalOnExpression / @EventListener(condition)
pub type KeyExprFn    = fn(&EvalCx) -> Result<CacheKey, ExprError>; // @Cacheable(key)
```
The thin `#[value]`/`#[event_listener(condition=)]`/`#[cacheable(key=)]` macros lower the fixed `#{...}` subgrammar (literals, property/index access, comparison/logical/arithmetic, ternary/Elvis `?:`, `@bean`/`&factory`) to a hand-writable Rust `fn`/closure in leaf-codegen (heavy lowering in codegen, never the macro body — thin-macro invariant). Escape hatch: the user writes a plain `expr_fn(my_predicate)`. `ExprError` is a `LeafError` constructor (Tier-0 malformed-grammar compile error via the codegen-boundary `${...}`/`#{...}` doctrine; runtime bean-ref miss = Tier-3 `LeafError` with candidates-considered). The `@bean`/`&factory` sub-case is the only runtime-fallible arm; it routes through the existing `Engine`/`Selector` so its NoSuchBean/NoUniqueBean diagnostics are identical to autowiring.

**Resolved open questions:** Ship a real interpreter? NO — closure-only; the interpreter's sole hard driver (runtime-authored/WASM/config-loaded expression strings) is dropped with WASM; the escape hatch is an ordinary Rust closure, so no DSL and no tree-walker exist.; Backend equivalence / Expression<T> erasing seam? MOOT — single backend, purpose-typed evaluators (ValueExpr<T>/CondExprFn/KeyExprFn) sharing the EvalCx SHAPE, so the hot path is a direct monomorphized fn call with no Box<dyn Fn> seam.; Lenient-vs-strict, per consumer? Per-consumer policy on ConvertCtx/the call site: @Value defaults lenient-degrade-to-default (config foot-gun parity) but flippable to strict (Tier-2 UnresolvedValue in AssemblyReport); @ConditionalOnExpression and @EventListener(condition) are strict (an eval error is a hard LeafError, never silently false). Routed through the one error-model, not a global knob.; Fail-fast timing ladder: @Value/OnExpression at the App<Wired>/App<Resolve> passes (compile error for malformed grammar at Tier 0); event-condition per-dispatch (Tier 3); cache-key per-call (Tier 3); bean-ref irreducibly runtime via Engine — all the SAME LeafError shape regardless of when it fires.; BeanResolver cross-crate/DCE: NONE for the closure itself (compiles where written); @bean only resolves if that bean's crate is linked+registered, which is the registry's existing anti-DCE self-check, inherited not created.; @bean.method(...) capability surface: NOT SUPPORTED in the closure grammar (no host-method-call into beans from a string); method invocation is plain Rust in a hand-written escape closure — the injection/safety surface simply does not exist without an interpreter.; Ambient context into EvalCx: via the explicit `cx: Option<&Cx>` field reading Holder<LocaleKey> etc.; never a thread-local; the EvalCx is the single explicit-passing vehicle across all four consumers.; Unify behind ONE abstraction or purpose-typed? Purpose-typed evaluators sharing the EvalCx SHAPE — the unification is the context shape + accessor traits, not an erased common Expression type (resolves the invariant-11-vs-hot-path tension toward the hot path).

### `messages-i18n`
DECISION: Phase-1 R1 (runtime catalog-provider chain) as the assembly model, DOGFOODED through the container (R3's unification: catalogs are `Role::Infrastructure` `Provider`s auto-detected at refresh), with R2's typed-codegen front offered ONLY as an opt-in `messages!`-from-bundle convenience layered on top — not the default surface. Concrete leaf-core types on the always-present `Context.messages` field (container-shape):

```rust
// leaf-core: MessageSource is a context service (NOT on bare Engine), dyn-stored => BoxFuture at the seam
pub trait MessageSource: Send + Sync {
    fn message<'a>(&'a self, code: &'a str, args: &'a [Arg<'a>], locale: Option<&'a Locale>)
        -> BoxFuture<'a, Result<Arc<str>, LeafError /* NoSuchMessage */>>;
    fn message_or<'a>(&'a self, code: &'a str, args: &'a [Arg<'a>], default: &'a str, locale: Option<&'a Locale>)
        -> BoxFuture<'a, Arc<str>>;
    fn resolve<'a>(&'a self, r: &'a dyn MessageResolvable, locale: Option<&'a Locale>)
        -> BoxFuture<'a, Result<Arc<str>, LeafError>>;
}
pub trait MessageCatalogProvider: Send + Sync {           // origin-agnostic; a Role::Infrastructure bean
    fn lookup<'a>(&'a self, code: &'a str, locale: &'a Locale) -> BoxFuture<'a, Option<MessagePattern>>;
    fn name(&self) -> &str;
}
pub struct CatalogChain { providers: Box<[Arc<dyn MessageCatalogProvider>]>, parent: Option<Ref<Context>> }
```
Return type is `Arc<str>` (resolves the ownership openQuestion: cheap-clone, cacheable, Send+Sync, WASM-irrelevant now). Locale defaulting: `None` reads `Holder<LocaleKey>::get()` from `Cx::current()`. Hierarchy: on a local-chain miss, `CatalogChain.parent` delegates to `Context::parent()`'s MessageSource (one-directional, child shadows parent — the SAME `container-hierarchy` walk container-shape fixes; never a shared global registry). Magic name: `refresh()`'s fixed-order template resolves `BeanKey::ByName("messageSource")`, else installs a `DelegatingMessageSource` no-op (echoes code/default) — the SAME auto-detect-or-default pass used for the event multicaster. Catalog discovery: `register_catalog!{...}`/`#[catalog]` emits a const `CatalogDescriptor` into a `linkme` `CATALOGS` slice (or build.rs codegen from `.ftl`/`.properties` for the portable path); the `ExpectedManifest` self-check fails loudly if an expected bundle's crate is DCE'd. Argument formatting delegates to the type-conversion neighbour's locale-aware formatters (NOT a private MessageFormat). A startup validation pass over codes×active-locales (R3) is OPT-IN (warn/fail) and aggregates into the `AssemblyReport`. AFIT-dyn boxing is accepted on the dyn path (cold/error paths and per-render); a monomorphized typed-catalog fast path is the Phase-3 escape.

**Resolved open questions:** Diagnostic timing default: runtime NoSuchMessage (R1) is the default, with an OPT-IN startup codes×active-locales validation pass (R3) aggregating into AssemblyReport, and an additive typed `messages!`-codegen front (R2) for compile-checked codes where the bundle is build-time-known. Default surface is stringly + rich runtime NoSuchMessage; typed is opt-in additive.; Async-across-dyn boxing: ACCEPTED on the dyn MessageSource path (BoxFuture per resolution), consistent with async-context-model 5b; compiled-in catalog lookups return ready futures (no IO); a monomorphized typed-source fast path is the deferred Phase-3 escape. message resolution being potentially hot is mitigated by Arc<str> pattern caching + ready-futures, not by abandoning dyn.; Catalog discovery substrate: linkme CATALOGS slice (default, server) + build.rs codegen from bundle files (portable/embedded path), behind the uniform &[CatalogDescriptor] — the SAME substrate + ExpectedManifest self-check as components; a translation-only crate must force-link (binary-crate scan! list) and a DCE'd one is a loud AntiDceError. WASM dynamic source is dropped.; Resolved-message ownership: Arc<str> (cheap-clone, Send+Sync, cacheable across the read-mostly per-locale store); resolves the keystone scope/ownership question now that WASM-origin handles are out of scope.; Locale value model + formatting: leaf-owned `Locale` value type (BCP-47-ish tag); locale-sensitive number/date/plural formatting DELEGATES to the type-conversion neighbour (or a chosen i18n crate behind it), never a bespoke formatter — the clearest unification win.; Container-shape placement: an ALWAYS-PRESENT structural Context.messages field (container-shape fixes this), assembled by the fixed-order refresh template that auto-detects Role::Infrastructure catalog Providers — not an Option capability, not a separate meta-architecture. R3's dogfooding is realized purely as 'catalogs are ordinary Provider beans'.; Hot-reload/dynamic catalog: a directory-watching catalog Provider is allowed; reload is a reactive cache swap (no global lock, watch-driven), and its file-watcher/handle teardown rides Context::shutdown()'s TeardownLedger drain (no async Drop). WASM-module catalog source dropped.

### `resource-loading`
DECISION: Phase-1 R1 (async `Resource` trait + protocol-keyed `ResourceLoader`, `classpath:` as a closed link-collected embedded namespace) as the canonical dyn surface, with R3's monomorphized typed methods (`loader.classpath("x")`, `loader.file(p)`) as the Phase-3 zero-cost fast lane at known-concrete sites. R2's `ContentSource` fusion with property-sources is REJECTED as a subsystem boundary (kept as an internal note) to avoid the large-resource-vs-small-config impedance mismatch — but `ResourceId` carries `Origin` so provenance is uniform. Concrete leaf-core types on the always-present `Context.resources` field:

```rust
pub trait Resource: Send + Sync {                          // object-safe; dyn-stored => boxed open
    fn id(&self) -> &ResourceId;                           // typed Origin+Location, never a bare String
    fn exists(&self) -> Existence;                         // enum Existence { Known(bool), Unknown } — honest, no lying
    fn last_modified(&self) -> Option<std::time::SystemTime>;
    fn open<'a>(&'a self) -> BoxFuture<'a, Result<ResourceReader, LeafError /* ConfigIo */>>;
    fn read_to_bytes<'a>(&'a self) -> BoxFuture<'a, Result<bytes::Bytes, LeafError>>; // cold-path slurp convenience
}
pub trait ResourceLoader: Send + Sync {
    fn resolve(&self, loc: &Location) -> Result<Box<dyn Resource>, LeafError>;
}
pub trait ResourcePatternResolver: ResourceLoader {
    fn resolve_pattern<'a>(&'a self, pat: &'a Pattern) -> BoxFuture<'a, Result<Vec<Box<dyn Resource>>, LeafError>>;
}
pub trait ResourceProvider: Send + Sync { fn scheme(&self) -> Scheme; /* resolve + resolve_pattern */ }
pub struct ResourceEntry { pub logical_path: &'static str, pub bytes_fn: fn() -> &'static [u8] } // classpath: row
```
`Location`/`Pattern`/`Scheme` are PARSED value types (rich 'unknown scheme `claspath:`, did you mean `classpath:`?' diagnostics). Built-in providers: `FileResourceProvider` (real async fs / `spawn_blocking` via the runtime's `BlockingOffload`; glob = async dir walk), `UrlResourceProvider` (`resolve_pattern` => typed `PatternUnsupported(scheme)` — honest per-origin capability), and the keystone `ClasspathResourceProvider` over a CLOSED `RESOURCES` `linkme` slice of `ResourceEntry` consts (emitted by `#[resource("path")]` thin macro = an `include_bytes!`-backed static a user could hand-write, or build.rs codegen). `classpath:foo` is an O(1) table lookup; `classpath*:**/*.yaml` is a glob over the finite collected table — enumeration is POSSIBLE precisely because the table is link-closed, not a runtime jar scan. This IS the substrate component-scanning, auto-config `*.imports` discovery, and `OnResource` enumerate over (the unification openQuestion answered: ONE substrate). The `ExpectedManifest` self-check turns a DCE-dropped resource-only crate into a loud `AntiDceError`, mandatory. `${...}`-bearing locations are resolved against `Env` at resolve time (loader-side). Injected as `Arc<dyn ResourcePatternResolver>` via the `ResolvableDependency` terminal layer. `ResourceReader` is a LEAF-OWNED reader trait (`AsyncRead`-shaped but not pinned to tokio/futures, honoring runtime-agnosticism), with a `Bytes`-slurp default.

**Resolved open questions:** Separate vs shared registry: SHARED — classpath: resources, component-scanning, and auto-config discovery all enumerate over ONE link-collected table substrate (the RESOURCES/COMPONENTS linkme slices + one ExpectedManifest self-check); resolves the §2.11 unification call toward one registry, one self-check.; ContentSource fusion with property-sources (R2): REJECTED at the boundary (large-resource-stream vs small-parsed-config impedance mismatch), but ResourceId carries the shared Origin type so provenance/diagnostics are uniform — provenance unification without API fusion.; Async-read/streaming trait surface: a LEAF-OWNED ResourceReader reader trait (AsyncRead-shaped, runtime-agnostic per §2.6) with a Bytes-slurp `read_to_bytes` default for the common cold-path; not pinned to tokio or futures AsyncRead.; Always-boxed vs typed path: BOTH — always-boxed `dyn Resource`/BoxFuture-open is the canonical dyn surface (R1, simple, cold-path-acceptable alloc), with R3's monomorphized typed methods (loader.classpath/file -> concrete ClasspathResource/FileResource, ready/zero-box futures) as the Phase-3 known-concrete fast lane; pattern resolution stays dyn-only (globbing is inherently runtime).; Cross-crate substrate + self-check: linkme RESOURCES slice (default) / build.rs codegen (portable) behind uniform &[ResourceEntry]; the facade ExpectedManifest expected-vs-found resource-manifest self-check binds to it and fails loudly — the SAME anti-DCE defense as components, non-negotiable for classpath:.; Static-vs-dynamic duality: link-time embedded table provider + an optional directory-scanned FileResourceProvider both feed one resolve/resolve_pattern API (two providers, one contract); enabling a dynamic source is EXPLICIT/scoped (a configured provider), never implicit-global. WASM-module source dropped.; exists()/last_modified() honesty: a tri-state `Existence::{Known(bool), Unknown}` (no Spring-style lying sync bool that secretly does IO); getFile()/getUrl() are Option/typed-capability per provider, surfaced honestly by the type system, never a throwing always-present accessor.; Compile-time miss detection for #[resource("x")]: available for SAME-CRATE include_bytes!-backed statics (codegen-time error); cross-crate embedded resources fall back to the runtime ExpectedManifest self-check — the split is documented, not papered over.; ${...} placeholder location resolution: loader-side against the Env at resolve time; for the config-data chicken-and-egg, the loader depends only on runtime + embedded table + explicitly-passed dirs during environment-prep (it does not depend on resolved beans), matching the existing bring-up ordering constraint.

## Public API sketch

// ═══ leaf-core::expr — monomorphized closures over a shared EvalCx (no interpreter) ═══
pub struct EvalCx<'a> { /* root, args, result: &dyn Any+Send+Sync; env: &Env; beans: &dyn BeanResolver; cx: Option<&Cx> */ }
pub trait BeanResolver: Send + Sync {
    fn bean(&self, name: &str) -> Result<ErasedBean, ResolveError>;     // @name  (Engine::get_erased ByName)
    fn factory(&self, name: &str) -> Result<ErasedBean, ResolveError>;  // &name  (ByName + Deref flag)
}
pub type ValueExpr<T> = fn(&EvalCx) -> Result<T, ExprError>;            // @Value, then type-conversion coerces
pub type CondExprFn   = fn(&EvalCx) -> Result<bool, ExprError>;        // @ConditionalOnExpression / @EventListener(condition)
pub type KeyExprFn    = fn(&EvalCx) -> Result<CacheKey, ExprError>;    // @Cacheable(key)
pub struct ExprError(/* -> LeafError node */);
// macros (leaf-macros, thin): #[value("#{server.port ?: 8080}")], #[event_listener(condition="event.priority > 5")],
//   #[cacheable(key="args.0")] — lower the fixed #{...} subgrammar to a const fn in leaf-codegen; escape hatch = expr_fn(closure).

// ═══ leaf-core::msg — hierarchy-aware MessageSource (always-present Context service) ═══
pub struct Locale(/* BCP-47 tag */);
pub trait MessageSource: Send + Sync {
    fn message<'a>(&'a self, code:&'a str, args:&'a [Arg<'a>], locale:Option<&'a Locale>) -> BoxFuture<'a, Result<Arc<str>, LeafError>>;
    fn message_or<'a>(&'a self, code:&'a str, args:&'a [Arg<'a>], default:&'a str, locale:Option<&'a Locale>) -> BoxFuture<'a, Arc<str>>;
    fn resolve<'a>(&'a self, r:&'a dyn MessageResolvable, locale:Option<&'a Locale>) -> BoxFuture<'a, Result<Arc<str>, LeafError>>;
}
pub trait MessageCatalogProvider: Send + Sync {            // Role::Infrastructure bean; origin-agnostic
    fn lookup<'a>(&'a self, code:&'a str, locale:&'a Locale) -> BoxFuture<'a, Option<MessagePattern>>;
    fn name(&self) -> &str;
}
pub struct CatalogDescriptor { /* const row; into linkme CATALOGS slice */ }
#[linkme::distributed_slice] pub static CATALOGS: [CatalogDescriptor] = [..];
// macro: register_catalog!{ basename="messages", locales=[..] }  (or build.rs codegen from .ftl/.properties)
// locale None => Holder<LocaleKey>::get(); miss => Context::parent() MessageSource; unconfigured => DelegatingMessageSource no-op.

// ═══ leaf-core::resource — origin-agnostic ResourceLoader (always-present Context service) ═══
pub enum Existence { Known(bool), Unknown }
pub struct ResourceId { /* Origin + Location */ }
pub trait Resource: Send + Sync {
    fn id(&self) -> &ResourceId;
    fn exists(&self) -> Existence;
    fn last_modified(&self) -> Option<std::time::SystemTime>;
    fn open<'a>(&'a self)          -> BoxFuture<'a, Result<ResourceReader, LeafError>>;
    fn read_to_bytes<'a>(&'a self) -> BoxFuture<'a, Result<bytes::Bytes, LeafError>>;
}
pub trait ResourceLoader: Send + Sync { fn resolve(&self, loc:&Location) -> Result<Box<dyn Resource>, LeafError>; }
pub trait ResourcePatternResolver: ResourceLoader {
    fn resolve_pattern<'a>(&'a self, pat:&'a Pattern) -> BoxFuture<'a, Result<Vec<Box<dyn Resource>>, LeafError>>;
}
pub trait ResourceProvider: Send + Sync { fn scheme(&self) -> Scheme; /* resolve + resolve_pattern */ }  // file/url/classpath
pub struct ResourceEntry { pub logical_path:&'static str, pub bytes_fn: fn()->&'static [u8] }
#[linkme::distributed_slice] pub static RESOURCES: [ResourceEntry] = [..];   // classpath: closed table
pub trait ResourceReader: Send { /* leaf-owned AsyncRead-shaped; runtime-agnostic */ }
// macro: #[resource("config/app.yaml")] -> one const ResourceEntry (include_bytes!-backed, hand-writable).
// Phase-3 typed fast lane: impl ResourceLoader { fn classpath(&self,p:&str)->ClasspathResource; fn file(&self,p:&Path)->FileResource; }

// ═══ on the always-present Context (container-shape) ═══
// impl Context { fn messages(&self)->&dyn MessageSource;  fn resources(&self)->&dyn ResourcePatternResolver; }
// CondExpr leaves ON_EXPRESSION / ON_RESOURCE (conditional-strategy) evaluate CondExprFn / ResourceLoader::exists at the Runtime PARSE sub-pass.

## Cross-feature interactions
- **messages-i18n ↔ resource-loading** — The MessageSource's compiled-in/directory catalog providers are CONSUMERS of the ResourceLoader: a per-locale bundle is opened by `loader.resolve(classpath:messages_de.ftl)` / a directory FileResourceProvider. Both are always-present Context services (container-shape) on the same facade surface and share the one linkme+ExpectedManifest discovery substrate and one self-check; a missing bundle is the same AntiDceError class as a missing resource.
- **expression-language ↔ messages-i18n** — A `@Value("#{...}")` expression evaluates to a value that may be a message code, and the resolvable-object MessageResolvable form is how validation constraint violations (downstream) present codes+args+default to the MessageSource. Both share the EvalCx/Cx ambient (locale) and the one LeafError spine; an expression error and a NoSuchMessage are the same Diagnostic shape.
- **expression-language ↔ resource-loading (via conditions)** — `@ConditionalOnExpression` lowers to a CondExprFn boolean closure; `@ConditionalOnResource` calls ResourceLoader::exists. Both are Runtime-tier CondExpr::Leaf in the SAME conditional-strategy PARSE sub-pass over the sealed Env, recorded into the one ConditionReport — no parallel gating engine; a NoSuchBean enriches from these outcomes.
- **all three ↔ type-conversion (sibling subsystem)** — Expression-to-typed-value (evaluate-then-convert), locale-sensitive message-argument formatting, and Location→Resource conversion all DELEGATE to the type-conversion FromConfigValue/ConvertCtx machinery; this subsystem never grows a private formatter or coercion path. The closure backend yields a typed value / ConfigValue conversion consumes directly (no erased Value->T hop, which is exactly why the interpreter was dropped).
- **all three ↔ locale-context / context-propagation (execution-context subsystem)** — Every 'current locale' read is one `Holder<LocaleKey>` (CxKey POLICY=Inherit) read off the ambient Cx, surviving .await via Scoped per-poll re-install. messages-i18n is the canonical consumer; expression and resource ${}-resolution read the same bundle. A missing AmbientStore backing degrades to default-locale (WARN), not a crash — the same degraded-not-fatal posture as the upstream.
- **all three ↔ container-shape + injection-mechanics** — MessageSource and ResourceLoader are ALWAYS-PRESENT structural Context fields (bare Engine lacks them), magic-name-or-default auto-detected by the fixed-order refresh template, injected via the ResolvableDependency terminal layer (no Aware setter). Expression @bean/&factory routes through Engine::get_erased(BeanKey::ByName) and the &=ByName+Deref flag — the one creation/resolution driver, origin-blind.
- **all three ↔ error-model + codegen-boundary** — ExprError/NoSuchMessage/ResourceError are LeafError constructors (closed ErrorKind + a new NoSuchMessage variant + Integration{ContractId} for custom providers), aggregated into the App<Wired> AssemblyReport at Tier 2 and rendered by the one Diagnostic; malformed #{...}/${...} grammar and #[resource] missing same-crate statics are Tier-0 compile errors via the codegen-boundary span/doctrine machinery. FailureAnalyzers ship for NoSuchMessage and resource-not-found.

## Crate hints
- leaf-core: the ultra-stable ABI — EvalCx/BeanResolver/ValueExpr/CondExprFn/KeyExprFn/ExprError; MessageSource/MessageCatalogProvider/Locale/MessagePattern/MessageResolvable + the CATALOGS linkme slice + CatalogDescriptor; Resource/ResourceLoader/ResourcePatternResolver/ResourceProvider/ResourceReader/ResourceEntry/Location/Pattern/Scheme/Existence/ResourceId + the RESOURCES linkme slice. New ErrorKind::NoSuchMessage core variant. All emitted-against by macros via absolute ::leaf_core paths.
- leaf-codegen: the heavy #{...} subgrammar parser+lowerer (grammar -> Rust fn), the .ftl/.properties bundle parser for the build.rs catalog codegen path and the opt-in typed `messages!` front, and the #[resource]/register_catalog! include_bytes!/codegen emission. Normal testable code, never in the proc-macro body.
- leaf-macros (proc-macro=true): thin #[value]/#[event_listener(condition=)]/#[cacheable(key=)]/#[resource]/#[catalog]/register_catalog! emitting one const row + a hand-writable forwarder; Tier-0 malformed-grammar compile_error! with proc_macro::Span; no logic.
- leaf-boot: the refresh-template steps that auto-detect Role::Infrastructure catalog Providers, do the messageSource magic-name-or-DelegatingMessageSource-default install, assemble the CatalogChain/ResourceProvider scheme-map, run the ExpectedManifest expected-vs-found self-check for CATALOGS+RESOURCES, and the opt-in codes×active-locales startup validation pass aggregating into AssemblyReport.
- leaf-tokio / leaf-smol: the runtime-backed ResourceReader impls and the FileResourceProvider's async fs / BlockingOffload glob; the AmbientStore backing the Holder<LocaleKey> read. Core names no runtime.
- builds on the execution-context subsystem (Cx/Holder<LocaleKey>/Scoped/BoxFuture/ExecutionFacility/TeardownLedger) and the registry-core subsystem (Engine/BeanKey/Descriptor/Provider/Registry/ErasedBean/Ref) — do not redefine.
- relies on the sibling type-conversion subsystem (FromConfigValue/ConvertCtx) for evaluate-then-convert and locale-sensitive argument formatting — not owned here.

## Remaining risks (→ Phase 4)
- Dropping the interpreter is a real Spring-parity divergence: no `parse(str).getValue(ctx)` runtime-authored expression exists. Justified because WASM (the only irreducible driver) is gone and the hot-path/typed-failure cost is severe, but Phase 4 must confirm no surviving feature (e.g. an admin-supplied cache-key or config-loaded condition string) silently needs it; if one does, it returns as an explicit opt-in leaf-expr crate behind the SAME EvalCx shape, never on the event hot path.
- The exact #{...} subgrammar frozen by leaf-codegen (operators, accessors, Elvis precedence, root/args navigation over &dyn Any with macro-known concrete downcast) is a compile-time-frozen contract; extending it is a codegen change, not user-pluggable. Phase 4 must nail the grammar BNF and the downcast-failure diagnostic, and confirm fully-generic `#root` navigation (no macro-known type) is rejected at compile time rather than silently mis-lowered.
- AFIT-dyn boxing on the MessageSource resolution path is an accepted per-render alloc on a potentially hot (per-rendered-error / per-response) surface; mitigated by Arc<str> pattern caching + ready-futures for compiled-in catalogs, but the Phase-3 monomorphized typed-catalog fast lane is deferred and unmeasured — needs a benchmark before claiming the boxing is acceptable under high render volume.
- The classpath: anti-DCE self-check correctness for resources is load-bearing and configuration-dependent (debug-passes / release-+gc-sections-fails): a translation-only or resource-only crate not force-linked silently under-enumerates classpath*:. Phase 4 must verify the ExpectedManifest binds correctly across the linkme + build.rs-codegen substrates and that the CI final-binary-link matrix (release+LTO+--gc-sections) covers CATALOGS and RESOURCES, not just COMPONENTS.
- Locale value model + locale-sensitive formatting (plurals/gender/ordinals, ICU-grade) is delegated to type-conversion / a downstream i18n crate but not yet pinned; the leaf-owned Locale type's identity and how it composes with the chosen formatter is a cross-subsystem seam that Phase 4 co-decides with type-conversion (risk: a lowest-common-denominator Locale that neither parses nor formats well).
- resolve_pattern returning a typed PatternUnsupported(scheme) leaks origin into the ResourceLoader contract (URL can't glob, file/classpath can) — an honest but non-uniform surface that diverges from Spring's papered-over uniform ResourcePatternResolver. Phase 4 should confirm component-scanning and auto-config discovery (the primary pattern consumers) only ever target enumerable schemes (classpath/file), so the leak is never hit on a hot discovery path.
- ResourceReader as a leaf-owned reader trait (vs committing to tokio/futures AsyncRead) preserves runtime-agnosticism but risks an ecosystem-interop tax (users wanting a tokio AsyncRead must adapt); the exact trait shape and a blanket adapter are deferred and coupled to the still-open pluggable-runtime question in async-context-model.
