//! `leaf-core` — Ultra-stable kernel ABI (registry, handles, Provider, slices, injection, env/binding, exec-context, lifecycle, proxy, events, conditions, error-model). DAG root; no logic.
//!
//! Built kernel-first per the design corpus in `docs/design/` (phase3 subsystem
//! docs + phase2 `TOOLKIT.md`, reconciled by phase4 `SEAMS.md`).
//!
//! ## UNIT 1 — bedrock (error model + ownership handles)
//!
//! This is the FROZEN ABI every other crate and macro pins to. The bedrock laid
//! by this unit:
//!
//! - [`BoxFuture`] — the async-across-`dyn` boxing standard at every `dyn` seam.
//! - The ONE shared-bean currency: [`ErasedBean`], [`Ref<T>`], [`Published`],
//!   [`Bean`], and the [`downcast_ref`]/[`downcast_owned`] recognition helpers
//!   (ADR-01 ownership-model).
//! - The ONE diagnostic spine: [`LeafError`] causal chain carrying a typed
//!   [`ErrorKind`] (closed core + one open `Integration` arm), [`Cause`]
//!   narrative, [`Origin`], and [`Severity`]; the [`Diagnostic`] renderer and
//!   the [`FailureAnalyzer`] SPI (ADR-12 error-model).
//! - Stable cross-build identity: [`ContractId`] over the one [`contract_hash`]
//!   (FNV-1a 64-bit, phase4 SEAMS seam #2).
//!
//! ## UNIT 2 — identity + ordering
//!
//! - The keying currency: dense [`BeanId`] slot ids, interned [`BeanName`], the
//!   [`BeanKey`] lookup enum, and interned [`MarkerId`] qualifier identity (the
//!   latter minted through the same one [`contract_hash`], NEVER a `TypeId`).
//! - [`derive_default_name`] — Spring's `decapitalize` naming rule, including
//!   the acronym edge case (a leading two-uppercase run is preserved).
//! - The ONE ordering law (SEAMS C6): the pure [`cmp_order`] over [`OrderKey`]/
//!   [`OrderSource`] (lower value wins; `Interface < Annotation < Implicit`),
//!   and the correctness-grade composite [`cmp_chain`] over [`ChainKey`]
//!   ([`RoleTier`] → `cmp_order` → [`ContractId`]), plus the fixed `*_ORDER`
//!   const table the built-in advice/multicaster chains are pinned to.
//!
//! ## UNIT 3 — BeanDefinition metamodel + Provider seam
//!
//! - The ONE const bean-definition row: [`Descriptor`] (`contract`, `self_type`,
//!   `provides[]` [`TypeRow`] upcasts, `declared_name`/`aliases`, the
//!   [`ScopeDef`] triple, [`Role`], the flat const [`AnnotationMetadata`],
//!   template-merge `parent`, diagnostic-only `origin`). Flat, closed-schema,
//!   const — never an open boxed attribute map.
//! - Scope as DATA on three orthogonal axes (TOOLKIT / bean-lifecycle):
//!   [`Multiplicity`] × [`StoreSource`] × [`TeardownPolicy`], with the built-in
//!   [`ScopeDef::SINGLETON`]/[`PROTOTYPE`](ScopeDef::PROTOTYPE)/[`REQUEST`](ScopeDef::REQUEST)
//!   consts and the interned [`ScopeKind`] for ambient (request/session/custom)
//!   stores. Never a `Box<dyn Scope>`.
//! - The 2-axis [`CandidateRole`] (SEAMS C5): `{primacy: `[`Primacy`]`, fallback}`
//!   with the `NORMAL`/`PRIMARY`/`FALLBACK` const constructors — the ONE source
//!   of truth the Selector / probe / condition-report all read (a `@Fallback`
//!   can also be `@Primary`).
//! - The metamodel [`Role`] mapped totally onto the ordering [`RoleTier`] via the
//!   new [`RoleTier::of`](crate::RoleTier::of) bridge (SEAMS C6) — one role taxonomy.
//! - The ONE origin-agnostic creation seam: the [`Provider`] trait
//!   (`provide -> BoxFuture<Result<Published, LeafError>>`, boxed at the `dyn`
//!   boundary), the const [`ProviderSeed`] fn-pointer that BUILDS a `Provider`
//!   (never a live object — what keeps `Descriptor` const), the user-facing
//!   [`FactoryBean`] trait, and the minimal [`ResolveCtx`] placeholder the
//!   registry/engine units flesh out.
//!
//! Macros hard-code `::leaf_core` paths to these items, so the surface is
//! versioned conservatively. Adding an [`ErrorKind`] / [`CauseDetail`] variant
//! is a minor-but-careful change (both are `#[non_exhaustive]`); the open
//! `Integration { kind_id: ContractId }` arm lets integrations extend the
//! taxonomy BY DATA without a core bump.

// The kernel is `unsafe`-free EXCEPT for the `linkme` distributed-slice
// substrate in `discovery`: `#[distributed_slice]` expands to a `#[used]`
// `#[link_section]` static, which trips the `unsafe_code` lint. That one module
// scopes its own `#[allow(unsafe_code)]` (the link-time-collection mechanism is
// load-bearing and has no safe equivalent on stable). Everywhere else, manual
// `unsafe` is a hard error — hence `deny`, not a blanket `forbid` (which cannot
// be locally relaxed for the slice macro).
#![deny(unsafe_code)]

pub mod advice;
pub mod bind;
pub mod bootstrap;
pub mod conditions;
pub mod convert;
pub mod cx;
pub mod definition;
pub mod discovery;
pub mod engine;
pub mod env;
pub mod error;
pub mod expr;
pub mod events;
pub mod exec;
pub mod future;
pub mod handle;
pub mod identity;
pub mod injection;
pub mod lifecycle;
pub mod lifecycle_engine;
pub mod metadata;
pub mod order;
pub mod placeholder;
pub mod provider;
pub mod proxy;
pub mod registry;
pub mod relaxed;

// ── curated re-exports: the flat bedrock surface macros and crates pin to ──

pub use future::BoxFuture;

pub use handle::{downcast_owned, downcast_ref, Bean, ErasedBean, Published, Ref};

pub use identity::{
    contract_hash, derive_default_name, BeanId, BeanKey, BeanName, ContractId, MarkerId,
};

pub use order::{
    cmp_chain, cmp_order, ChainKey, OrderKey, OrderSource, RoleTier, ASYNC_DISPATCH_ORDER,
    ASYNC_ORDER, CACHE_ORDER, CONCURRENCY_ORDER, CONTEXT_PROP_ORDER, DEFAULT_ORDER,
    ERROR_ISOLATION_ORDER, METRICS_ORDER, RETRY_ORDER, TRANSLATE_ORDER, TX_ORDER, VALIDATE_ORDER,
};

pub use error::{
    analyze_first, AnalysisCtx, CandidateInfo, Cause, CauseDetail, Diagnostic, ErrorKind,
    FailureAnalysis, FailureAnalyzer, InjectionEdge, LeafError, NarrowStep, Origin, RenderStyle,
    Severity,
};

pub use definition::{
    merge_descriptor, AnnotationMetadata, CandidateRole, Descriptor, Multiplicity, Primacy, Role,
    ScopeDef, ScopeKind, StoreSource, TeardownPolicy, TypeRow, UpcastFn,
};

pub use provider::{FactoryBean, Provider, ProviderSeed, ResolveCtx, ScopeStores};

// ── UNIT 4 — link-time discovery slices + the programmatic Registrar SPI ──
//
// The maximal-magic registration substrate (ADR-09 / discovery-codegen): the one
// `linkme` distributed-slice channel family, the `Registrar` SPI, the anti-DCE
// `SourceTag` anchor, the minimal const rows later units flesh out, and the one
// `collect_slice` read idiom. `linkme` is re-exported so emitted code references
// `::leaf_core::linkme`, never a bare `::linkme`.

pub use discovery::{
    collect_slice, linkme, origin_of, AdvisorRow, CatalogRow, ConditionRow, ConfigMetadataRow,
    EventListenerRow, Registrar, RegistrarCtx, ResourceRow, ScheduledRow, SourceTag, StereotypeRow,
    ADVISORS, AUTO_CONFIGS, CATALOGS, COMPONENTS, CONDITIONS, CONFIG_METADATA, EVENT_LISTENERS,
    FAILURE_ANALYZERS, REGISTRARS, RESOURCES, SCHEDULED, SOURCES, STEREOTYPES,
};

// ── UNIT 5 — the frozen registry, its builder, and the singleton store ──
//
// The two-epoch frozen-snapshot registry (registry-core `bean-registry` /
// `bean-naming` / extra-4): the append-only `RegistryBuilder`, the immutable
// dense-`BeanId` `Registry` (TypeId-primary index + insertion-ordered name
// overlay + alias/contract maps + the slot-indexed `OnceCell` singleton store),
// candidate-set queries, the loud name/contract/alias collision guards, and the
// canonical `NULL_BEAN` present-but-absent sentinel.

pub use registry::{is_null_bean, NullMarker, Registry, RegistryBuilder, NULL_BEAN};

// ── UNIT 6 — the injection resolution spine + deferral handles ──
//
// The single fixed-order traced layer-fold Selector (injection-resolution
// phase3/03): the const `InjectionPoint`/`InjectionPlan` rows, the `Cand`
// read-view + `CandidateSet`, the `Verdict`/`Resolved`/`Layer`/`LAYERS`/`Trace`
// fold machinery (with the SEAMS-C5 FallbackDemote→PrimaryPromote→len order
// inside `primary_promote`), the SOLE fail-fast len-rule in `Selector::resolve_one`,
// the `cmp_order`-based `collect_ordered` collection path, the
// `AdvisedConcreteInjection` COHERENCE rejection, and the honest-visible deferral
// family `Lookup`/`LazyRef`/`Inject`/`SelfRef` (resolve-on-demand over a `Weak`
// `Container` back-ref — single-phase construction, deferral-only cycle break).

pub use injection::{
    collect_ordered, layers, no_unique_bean_traced, reject_advised_concrete, resolved_to_result,
    resolved_to_result_traced, trace_to_steps, Arity, Cand, CandidateSet, Cardinality,
    CollectionShape, Container, ContainerRef, DescriptorFilter, Inject, InjectionPlan,
    InjectionPoint, Layer, LazyRef, Lookup, PointKind, QualifierReq, Resolve, ResolveFn, Resolved,
    Selector, SelfRef, Strictness, StreamOrder, Trace, Verdict, ViewUpcast, LAYERS,
};

// ── UNIT 7 — environment + property binding + conversion ──
//
// The value-shape half of leaf (environment-config phase3/06 +
// binding-conversion phase3/07): the ordered first-source-wins property stack
// and its read seam (`Env`/`PropertyResolver`/`PropertySource`), the canonical
// coercion trait (`FromConfigValue`/`ConvertCtx` + grammar newtypes), the one
// key-identity owner (`CanonicalName` + uniform-fold relaxed binding +
// env-var->canonical mapping), the escape-aware `${...}` placeholder engine
// (`PlaceholderSyntax` + the `${}`/`#{}` dispatch AST/`interpret`), the Binder
// tree-descent engine over the tri-state CPS adapter (`BindTarget`/`NodeSchema`/
// `BindResult`/`BindHandler`/`Binder`/`ConfigurationPropertySource`), the
// `random.*` computing source (`RandomValueSource`/`RandomSpec`), and the
// co-emitted config-metadata shape consuming `CONFIG_METADATA`. Pure ABI +
// pure functions; the `validate()` ORCHESTRATION (the C2 Tier-2 materialization)
// lives in leaf-boot.

pub use convert::{
    ConfigValue, ConvertCtx, DataSize, Duration, FromConfigValue, Leniency, Period, UnitHint,
};

pub use relaxed::{
    env_var_candidates, env_var_to_canonical, uniform_key, CanonicalName, NameSyntaxError, Segment,
    UniformName,
};

pub use placeholder::{
    has_expr, interpret, interpret_with, resolve_lenient, resolve_strict, PlaceholderSyntax,
    Segment as ValueSegment, DEFAULT_DEPTH_CAP,
};

pub use env::{
    Env, EnvBuilder, EnvCore, MapPropertySource, NoSuchSource, PropertyResolver, PropertySource,
    PropertyValue, RandomSpec, RandomValueSource, ResolvedValue, SealedStack, SourceCaps, SourceName,
};

pub use bind::{
    bind_error, BindCtx, BindCursor, BindHandler, BindMethod, BindResult, BindTarget, Binder,
    ConfigurationPropertySource, ConfigurationPropertyState, ConversionService, Converter, Field,
    NodeSchema, NoopBindHandler, StackCps,
};

pub use metadata::{
    collect_config_metadata, find_by_prefix, group_to_row, CodeSpan, ConfigGroup, Deprecation,
    Hint, Property,
};

// ── UNIT 8 — execution facility + ambient Cx + RunState [exec-context] ──
//
// The runtime-agnostic async spine (execution-context phase3/10 +
// container-lifecycle phase3/13, ADR-07). NO runtime is named here.
//
// - lifecycle: the ONE phase-axis `RunState` machine (`can_transition_to` law),
//   the std-based `watch<RunState>` cell (`watch_run_state`/`run_state_channel`/
//   `WatchSender`/`WatchReceiver` — reactive, charter §2.4, NO tokio, no global
//   lock), and the `Lifecycle`/`Shutdown` dyn seams (boxed futures, no async
//   Drop).

pub use lifecycle::{
    run_state_channel, run_state_sender, watch_channel, watch_run_state, Changed, Lifecycle,
    RunState, RunStateReceiver, RunStateSender, Shutdown, WaitFor, WatchReceiver, WatchSender,
};

// - cx: the ONE ambient bundle (`Cx`/`CxKey`/`Propagation`), the `AmbientStore`
//   storage seam (+ the built-in `ThreadLocalAmbientStore` fallback +
//   `install_ambient_store`), the per-poll-re-installing `Scoped` combinator
//   (`CxFutureExt::scoped`), the demoted `CxBridge`/`CxDecorator` seams, and the
//   typed `Holder<K>` accessor. NO runtime named here.

pub use cx::{
    ambient_store, install_ambient_store, AmbientStore, BridgeGuard, CxBridge, CxDecorator,
    CxFutureExt, CxKey, Holder, Propagation, Scoped, ThreadLocalAmbientStore,
};
// `Cx` is re-exported separately so its rustdoc anchor is unambiguous.
pub use cx::Cx;

// - exec: the capability-split execution ABI (`Spawner`/`BlockingOffload`/
//   `ConcurrencyGate` composed by the `ExecutionFacility` supertrait), the
//   runtime-agnostic `SpawnHandle`/`BlockingHandle`/`Permit` seams (+ their
//   `JoinSeam`/`PermitSeam` runtime backings, `DropPolicy`, `JoinError`), the
//   `SpawnableWork` doctrine bound, the `AsyncUncaughtFailureHandler` sink; and
//   the scheduling capability over the SAME Spawner: the sync `Trigger` SPI +
//   `TriggerContext` + built-in `FixedRateTrigger`/`FixedDelayTrigger`,
//   `OverlapPolicy`, the const `TriggerSpec`/`MethodKey`/`ScheduledMethodDescriptor`
//   (bridged to the frozen `SCHEDULED` slice via `to_row`/`collect_scheduled`),
//   and the `SchedulerCore` registration/quiesce seam. NO runtime named here.

pub use exec::{
    collect_scheduled, AsyncUncaughtFailureHandler, BlockingHandle, BlockingOffload,
    ConcurrencyGate, DropPolicy, ExecutionFacility, FixedDelayTrigger, FixedRateTrigger, JoinError,
    JoinSeam, MethodKey, OverlapPolicy, Permit, PermitSeam, ScheduledMethodDescriptor,
    SchedulerCore, SpawnHandle, SpawnableWork, Spawner, Trigger, TriggerContext, TriggerSpec,
};

// ── UNIT 9 — bean lifecycle + Engine::create driver + Context façade ──
//
// The value-phase of leaf (bean-lifecycle phase3/04 + registry-core container-core
// + container-lifecycle phase3/13). Two halves:
//
// - lifecycle_engine: the const lifecycle metamodel the thin macro emits beside
//   the `Descriptor` (`LifecyclePlan`/`LifecycleStep`/`LifecyclePhase`/`LifecycleFn`/
//   `StepId`/`AwareFlags`/`Bootstrap`), the typed escape-hatch traits
//   (`InitializingBean`/`DisposableBean`/`Closeable`/`AwareReady`/
//   `AfterSingletonsReady`), the `run_init`/`run_destroy` forward/reverse runners,
//   the ONE teardown path (`Destroyer`/`TeardownLedger` LIFO drain — no async
//   Drop), the per-scope `InstanceStore` seam (bare `ErasedBean`, reached via Cx),
//   the `CallbackError` → `LeafError` bridge, and the `ShareableBean`
//   concurrency-contract doctrine diagnostic.

pub use lifecycle_engine::{
    run_destroy, run_init, AfterSingletonsReady, AwareFlags, AwareReady, Bootstrap, CallbackError,
    Closeable, Destroyer, DisposableBean, InitializingBean, InstanceStore, LifecycleFn,
    LifecyclePhase, LifecyclePlan, LifecycleStep, ShareableBean, StepId, TeardownLedger,
    TeardownOutcome,
};

// - engine: the bare inert `Engine` (DefaultListableBeanFactory analogue) owning
//   the frozen `Registry` + singleton store + `Selector` + the ONE concrete
//   `Engine::create` driver (construct → init → publish through the
//   Provider/Published seam, single-phase); the OnceCell singleton publication
//   guard (at-most-once, lock-free ready read); the prototype Owned-move lane
//   (container retains nothing). The `Context` façade HAS-A one Engine + the one
//   `TeardownLedger`, delegating the BeanFactory surface. `EnginePolicy`, and the
//   aggregated `AssemblyError`/`AssemblyReport`.

pub use engine::{
    AssemblyError, AssemblyReport, Context, Engine, EnginePolicy,
};

// ── UNIT 10/11 — proxy / interception substrate + event dispatch [proxy-events] ──
//
// The ONE wrap primitive (ADR-08 proxy-substrate, phase3/08) — the RUNTIME side of
// the compile-time-generated transparent-newtype mechanism — plus the SEPARATE
// event-dispatch shape (per C5).
//
// - proxy: the CALL-ROUTING primitive (`Interceptor` wrapping one `Call` → `ErasedRet`,
//   the REPLAYABLE+SKIPPABLE `Next`, the `AdviceChain`), the `TargetSource` seam
//   unifying singleton (`FixedTarget`) / scoped re-resolution (`ScopeTarget`) /
//   advised-prototype (`OwnedTarget`), the dynamic `ErasedProxy`/`MethodTable`
//   fallback, the typed-combinator `Pointcut` model (`within`/`annotated_marker`/
//   `returns` + `And`/`Or`/`Not`), the flat const `AdvisorDescriptor` (collected via
//   `ADVISORS`), and the frozen `BeanId`-keyed `ProxyPlan` (`freeze` sorts by
//   `cmp_chain`; `advisors_for` is the O(1) `after_init` lookup) gated by the
//   binary-root `CreatorPolicy`. Errors flow into the one `LeafError` via `AdviceError`.

pub use proxy::{
    annotated_marker, returns, within, AdviceChain, AdviceError, AdvisorDescriptor, AdvisorRef,
    And, Annotated, Anything, BeanJoinPoints, Call, CreatorPolicy, ErasedArgs, ErasedProxy,
    ErasedRet, FixedTarget, Interceptor, JoinPointMeta, MakeInterceptor, MethodEntry,
    MethodJoinPoint, MethodTable, Next, Not, Or, OwnedTarget, Pointcut, ProxyPlan, ResolveError,
    Returns, ScopeTarget, Tail, TargetSource, Within,
};

// - events: the in-process observer bus + the dispatch multicaster (C5 — a
//   SEPARATE shape from advice). The publish/subscribe model (`ApplicationListener`,
//   the `ListenerDescriptor` ABL + `ErasedAdapterFn`, `ListenerOutcome`/`ErasedEvent`
//   return-as-event chaining, `ListenerEntry`/`ListenerSeq` + the `cmp_order` merge),
//   the STANDALONE `DispatchInterceptor` + `ListenerNext` fan-out (sorted by
//   `cmp_chain`; sharing ONLY that comparator + the RoleTier grade with AOP), the
//   `Multicaster`/`PipelineMulticaster` seam over the inline-await `CoreDispatch`
//   sink with the `DispatchErrorMode` per-dispatch policy, the built-in lifecycle
//   facts (`Refreshed`/`Started`/`Stopped`/`Closed`/`StartupFailed`/
//   `AvailabilityChanged`), and the availability STATE over the watch primitive
//   (`AvailabilityHandle` + `LivenessState`/`ReadinessState`).

pub use events::{
    sort_listener_entries, ApplicationListener, AvailabilityChanged, AvailabilityHandle,
    AvailabilityKind, AvailabilityState, CloseReason, Closed, ContainerId, CoreDispatch,
    DispatchErrorMode, DispatchInterceptor, DispatchOutcome, ErasedAdapterFn, ErasedEvent,
    ListenerDescriptor, ListenerEntry, ListenerNext, ListenerOutcome, ListenerSeq, LivenessState,
    Multicaster, PipelineMulticaster, ReadinessState, Refreshed, Started, StartupFailed, Stopped,
    SupportsFn,
};

// ── UNIT 11 — conditions algebra + expr/i18n/advice/bootstrap ABI ──
//
// The ONE gating + composition spine (conditions-autoconfig phase3/05) plus the
// remaining trait/value ABI (definitions, not engines) for expr-i18n-resources
// (phase3/11), declarative-advice (phase3/09), and bootstrap-diagnostics
// (phase3/14). Engines (route_conditions/run_autoconfig/order_batch, the live
// MessageSource/TransactionManager/Cache impls, deduce/run pipeline) live in
// leaf-conditions/leaf-boot/leaf-i18n/leaf-tx/leaf-cache/etc.; leaf-core freezes
// only the ABI macros and crates hard-code `::leaf_core` paths to.

// - conditions: the const `CondExpr` algebra over `ConditionId` with the
//   per-kind tier-map (`EarliestTier`/`SubPhase`/`ConditionKind`; Runtime is the
//   mandatory floor; `tier`/`phase` infer the earliest-sound placement = max
//   over leaves at const-eval), the runtime `Condition` SPI + `ConditionCtx` +
//   `ConditionOutcome`/`ReasonMsg` + `CondImplRow`, the `Resolvability` OnBean
//   probe verdict, the passive `ReportSink`/`ConditionRecord`/`ConditionReport`
//   over the six-class `ConditionReportClass`, the auto-config metamodel
//   additions (`OrderHint`/`ImportRef`/`ImportEdge`/`auto_config_role`), and
//   profiles as a preset (`ON_PROFILE`/`ProfileExpr`/`ActiveProfiles`/
//   `ProfileLevers` + `resolve_active`/`matches`/`accepts_profiles`).

pub use conditions::{
    accepts_profiles, auto_config_role, evaluate, matches as profile_matches, resolve_active, Attr,
    AttrSlice, CondExpr, CondImplRow, Condition, ConditionCtx, ConditionId, ConditionKind,
    ConditionOutcome, ConditionRecord, ConditionReport, ConditionReportClass, EarliestTier,
    ActiveProfiles, ActivationReason, ImportEdge, ImportRef, LeafOutcome, NoopReportSink, OrderHint,
    ProfileError, ProfileExpr, ProfileLevers, ProfileParseError, ReasonMsg, ReportSink,
    Resolvability, SubPhase, ON_PROFILE, UNCONDITIONAL,
};

// - expr: the closure-only expression backend (the shared `EvalCx` shape +
//   `BeanResolver` lookup seam; `ValueExpr`/`CondExprFn`/`KeyExprFn` purpose-typed
//   const fn pointers; `ExprError`), the hierarchy-aware i18n facade
//   (`MessageSource`/`MessageCatalogProvider`/`Locale`/`Arg`/`MessagePattern`/
//   `MessageResolvable`/`CatalogDescriptor`), and the origin-agnostic resource
//   loading ABI (`Resource`/`ResourceLoader`/`ResourcePatternResolver`/
//   `ResourceProvider`/`ResourceReader`/`ResourceEntry`/`Location`/`Pattern`/
//   `Scheme`/`Existence`/`ResourceId`). Pure ABI — live engines elsewhere.

pub use expr::{
    Arg, BeanResolver, CacheKeyValue, CatalogDescriptor, CondExprFn, EvalCx, Existence, ExprError,
    ExpressionEvaluator, KeyExprFn, Locale, Location, MessageCatalogProvider, MessagePattern,
    MessageResolvable, MessageSource, Pattern, Resource, ResourceEntry, ResourceId, ResourceLoader,
    ResourcePatternResolver, ResourceProvider, ResourceReader, Scheme, ValueExpr,
};

// - advice: the SHARED declarative-advice shapes only (the wrap primitive is
//   consumed from proxy unchanged). Transactions (`TransactionManager`/
//   `TxAttribute`/`TxPropagation`/`Isolation`/`TxState`/`TxResourceKey`/`TxPhase`/
//   `TxSyncRegistry`/`TxDeferral`), caching (`CacheManager`/`Cache`/`CacheKey`/
//   `StoredValue`/`CacheOpMeta`), validation (`Validate`/`ValidationContext`/
//   `Violation`), retry (`RetryTemplate`/`RetryPolicy`/`BackoffPolicy`), and
//   exception translation (`DataAccessExceptionTranslator`/`DataAccessKind`
//   riding `ErrorKind::Integration`). The chain-order `*_ORDER` consts live in
//   `order`; the `ADVISORS`/`SCHEDULED` slices in `discovery`.

pub use advice::{
    AsyncMeta, BackoffPolicy, Cache, CacheKey, CacheManager, CacheOpMeta, DataAccessExceptionTranslator,
    DataAccessKind, ErrorMatch, FixedBackoff, Isolation, RetryPolicy, RetryTemplate, StoredValue,
    TransactionManager, TxAttribute, TxDeferral, TxDefinition, TxOutcome, TxPhase, TxPropagation,
    TxResourceKey, TxState, TxSyncCallback, TxSyncRegistry, Validate, ValidationContext, Violation,
};

// - bootstrap: the run-pipeline ABI leaf-boot is built on. `ApplicationArguments`
//   (the ONE argv owner; the `--opt[=v]`/non-option split is pure + here), the
//   open `AppType` (`NONE`/`SERVLET`/`REACTIVE` vocabulary; deduction is
//   leaf-boot's) + the shared `CapabilitySet`, the run-participant linkme
//   channels (`APP_TYPE_DEDUCERS`/`CONTEXT_INITIALIZERS`/`EARLY_LISTENERS`/
//   `FLAVOR_SEEDERS`/`EXIT_CODE_CONTRIBUTORS`) + their descriptor rows + traits
//   (`ContextInitializer`/`EarlyListener`/`Runner`/`FlavorSeeder`/
//   `ExitCodeContributor`/`DeducerDescriptor`), the `ShutdownTrigger` seam, the
//   unified `StartupValidation { Strict, Lenient, Skip }` lever, and the frozen
//   `BootstrapSettings`/`ShutdownSettings`/`Deadline`/`BannerMode`/`RunMilestone`
//   self-binding records.

pub use bootstrap::{
    AppType, ApplicationArguments, BannerMode, BootstrapSettings, CapabilitySet,
    ContextInitializer, ContributorDescriptor, Deadline, DeducerDescriptor, EarlyListener,
    EarlyListenerDescriptor, ExitCodeContributor, ExitCodeEvent, FlavorSeeder,
    FlavorSeederDescriptor, InitializerDescriptor, PreRefreshCtx, RunMilestone, Runner,
    ShutdownSettings, ShutdownTrigger, StartupValidation, APP_TYPE_DEDUCERS, CONTEXT_INITIALIZERS,
    EARLY_LISTENERS, EXIT_CODE_CONTRIBUTORS, FLAVOR_SEEDERS,
};
