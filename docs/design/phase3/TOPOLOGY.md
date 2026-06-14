# leaf — Crate Topology (Phase 3)

Realizes charter section 6 verbatim (small stable kernel, macro/codegen separated from runtime, optional features as crates, deps strictly inward, no cycles) and is forced by the toolkit. NAMING: charter section 6 sketches leaf-framework, but the toolkit and all 16 crateHints uniformly use leaf-core for the kernel (every macro hard-codes leaf_core paths) and leaf-boot for bootstrap/assembly; the charter defers fine-grained topology to Phase 3 and calls section 6 principles-not-decisions, so I adopt leaf-core and leaf-boot. ONE-KERNEL INVARIANT: every crateHint places its ultra-stable ABI in leaf-core because codegen-boundary mandates absolute leaf_core paths and the single-copy-of-leaf-core invariant is load-bearing (two copies equals two AUTO_CONFIGS and COMPONENTS slices equals a silently split batch); leaf-core is deliberately broad-but-frozen ABI, not logic. LOGIC and DATA SPLIT (charter 2.10): leaf-macros (proc-macro true) emits only const rows plus forwarders; every heavy testable algorithm (annotation merge, the embedded-expression and pointcut grammars, build.rs force-link and ExpectedManifest emitters, the leaf metadata rollup) lives in normal unit-testable leaf-codegen so cargo-expand stays legible. ASSEMBLY and ABI SPLIT (ADR-05): leaf-core owns the inert Engine and Context types plus the single Engine create driver; leaf-boot owns every orchestration decision (App phase machine, seal, refresh template, run_autoconfig ladder, App Wired validation, App run). This is why the container-lifecycle crateHint was just x: it has no crate of its own; it IS the leaf-boot refresh and teardown template that COHERENCE fuses from the other subsystems. RUNTIME-AGNOSTICISM (charter 2.6): core names no runtime; leaf-tokio (default, force-linked by leaf-boot) and leaf-smol (mirror) provide ExecutionFacility, AmbientStore, timer-wheel, ShutdownTrigger; missing-executor is HARD-FAIL at refresh. OPTIONAL-FEATURES-AS-CRATES (charter 6): cross-cutting concerns (leaf-tx, leaf-cache, leaf-validation, leaf-resilience) and heavy-dep or concept-owning pieces (leaf-config, leaf-conditions, leaf-aop-expr, leaf-cron, leaf-i18n, leaf-serde, leaf-figlet) are separate crates, each dogfooding the shared Interceptor, AdvisorDescriptor, CondExpr, Holder vocabulary rather than minting bespoke machinery (charter 2.11), each depending on leaf-core only. SHARED MACHINERY: events dispatch reuses aop Interceptor plus cmp_order (no new comparator or around-advice); OnBean conditions delegate to injection-mechanics DefinitionProbe; declarative-advice and i18n read Cx and Holder from execution-context; transactional_event_listener is a thin macro in leaf-tx emitting the existing events row. NO CYCLES: leaf-macros to leaf-core; leaf-codegen, leaf-config, leaf-conditions, runtime, concern, and integration crates to leaf-core; leaf-boot to leaf-core plus leaf-conditions plus leaf-config plus leaf-codegen; leaf-starter-* to integration crates; leaf umbrella to everything optional. Integration and starter crates depend on leaf-core never the umbrella (hard Cargo cycle constraint); the umbrella is the unique DAG sink. WASM dropped; native runtime crates only.</parameter>
</invoke>


## Crates

### `leaf-core` — Ultra-stable kernel ABI crate (normal lib). The charter leaf-framework IoC role, renamed leaf-core because every macro hard-codes leaf_core paths. No runtime, no orchestration logic; ABI surface only. DAG root.
- **contains:** registry-core: ErasedBean, Ref, Published, Provider, BeanId, ContractId, BeanKey, Descriptor, TypeRow, Role, ScopeDef, Registry, RegistryBuilder, Engine, Context, FactoryBean, derive_default_name, ResolveError, AssemblyError; discovery ABI: MarkerId, contract_hash, COMPONENTS/AUTO_CONFIGS/CONDITIONS/SOURCES/STEREOTYPES/REGISTRARS linkme slices, ProviderSeed, BeanSeed, Registrar; injection: Selector, Verdict, Trace, InjectionPoint, Arity, QualifierReq, OrderKey, OrderSource, cmp_order, Lookup, LazyRef, Inject, SelfRef; environment+binding: Env, PropertyResolver, PropertySource, FromConfigValue, ConvertCtx, Binder, BindTarget, PlaceholderSyntax, ConfigGroup, CONFIG_METADATA slice, RandomValueSource; execution-context: BoxFuture, Spawner, BlockingOffload, ConcurrencyGate, ExecutionFacility, SchedulerCore, ScheduledMethodDescriptor, SCHEDULED slice, Cx, CxKey, Propagation, AmbientStore, Holder, Lifecycle, Shutdown, watch_run_state; bean-lifecycle: LifecyclePlan, AwareFlags, InstanceStore, ScopeKind, Destroyer, InitializingBean, DisposableBean, Engine::create driver, OnceCell singleton guard, publish_pipeline, prototype Owned lane; proxy: Interceptor, Call, Next, AdviceChain, TargetSource, ErasedProxy, MethodTable, AdvisorDescriptor, Pointcut, ProxyPlan, CreatorPolicy; events: EventPublisher, ApplicationListener, EVENT_LISTENERS slice, ListenerDescriptor with defer, events dispatch reusing aop Interceptor and cmp_order, availability over watch RunState; expr-i18n-resources: ValueExpr, ExprError, MessageSource, CATALOGS slice, ResourceProvider, ResourceReader, RESOURCES slice; conditions: CondExpr, ConditionId, ConditionKind, tier-map, Condition SPI, CandidateRole (Normal/Primary/Fallback), OrderHint, ProfileExpr, ConditionReport, AutoConfigDescriptor row; declarative-advice ABI: TransactionManager, Cache, CacheManager, Validate traits, TxResourceKey, TxSyncRegistry, RetryPolicy, BackoffPolicy, ADVISORS slice; bootstrap ABI: ApplicationArguments, AppType, run-participant slices (deducers/initializers/listeners/seeders/exit-contributors), ShutdownTrigger seam; error-model: LeafError chain, Diagnostic, FailureAnalyzer, FAILURE_ANALYZERS slice, closed ErrorKind plus open Integration arm
- **deps:** indexmap; smallvec; once_cell; futures; linkme

### `leaf-macros` — Single thin proc-macro crate (proc-macro true). Exports only macros; no logic. Each macro emits one hand-writable const row via absolute leaf_core paths. Heavy logic delegated to leaf-codegen.
- **contains:** component/service/repository/controller/configuration/bean, stereotype, register_component (generic hard-errors); value/property_source/config_properties/derive BindTarget/converter; scheduled/holder/async; LifecyclePlan const; conditional/profile/auto_config/import (ProfileExpr parse, self-call lint); advisable/aspect/advice/pointcut (newtype plus AdvisorDescriptor; register_proxy Concrete escape); event_listener/transactional_event_listener; cacheable/resource/catalog; main/failure_analyzer/runner
- **deps:** leaf-codegen; syn; quote; proc-macro2

### `leaf-codegen` — Normal (non-proc-macro) library with all heavy unit-testable codegen logic the thin macros and build.rs call. Testable without compilation/link/runtime.
- **contains:** annotation merge/alias/distance, AliasFor validator, build.rs force-link plus ExpectedManifest emitter, opt-in cargo leaf prepare plan; ConstFold folding plus deferred autoconfig/ordering plan; embedded-expression and message-bundle parsers for catalog codegen; leaf metadata rollup over CONFIG_METADATA plus check; the cargo leaf subcommand
- **deps:** leaf-core; serde_json; toml

### `leaf-config` — Optional config-data engine feature crate (normal lib). Heavy loaders kept off the kernel. Folds into leaf-core only if tiny.
- **contains:** environment-config: ConfigDataLoader, plan/apply engine, PrecedenceRung, JSON/YAML/configtree/env loaders, PlaceholderEngine
- **deps:** leaf-core; serde_json; a YAML crate; indexmap; once_cell

### `leaf-conditions` — Feature crate (normal lib): the concrete condition-family catalog; keeps dozens of conditions out of the frozen kernel.
- **contains:** OnProperty/OnBean/OnMissingBean/OnSingleCandidate/OnProfile/OnExpression/OnResource/OnRustVersion (OnBean delegates to DefinitionProbe), resolve_active plus ProfileExpr evaluator
- **deps:** leaf-core

### `leaf-aop-expr` — Optional feature crate (normal lib): compile-time pointcut-expression parser lowering strings to const Pointcut predicates. Opt-in.
- **contains:** pointcut-expression parser/lowerer producing leaf-core const Pointcut predicates
- **deps:** leaf-core

### `leaf-boot` — Assembly engine plus run pipeline crate (normal lib). Charter bootstrap/auto-config layer atop leaf-core Engine/Context. Hosts all orchestration. Force-links leaf-tokio by default.
- **contains:** App Define-Resolve-Wired-Running machine, seal, Context refresh template, run_fixpoint, run_autoconfig (exclude then back-off then default ladder; Fallback; kill-switch); seal_environment (5f fence), argv parser, PropertySource step, binding plus validation over sealed Env, RandomValueSource placement; App Wired validate folding DependsOn into WiringPlan waves, wave executor, smart-init loop, TeardownLedger drains; order_batch (3-pass sort plus cycle detect), ExclusionSet merge, slice test mode, condition-report finalize, ExpectedManifest self-checks; messageSource/CatalogChain/ResourceProvider install; advisor auto-config; App run plus FailureAnalysis plus exit-code coordinator; leaf-doctor; the fused container-lifecycle template
- **deps:** leaf-core; leaf-conditions; leaf-config; leaf-codegen

### `leaf-tokio` — Default runtime integration crate (normal lib). Force-linked by leaf-boot. Provides ExecutionFacility plus AmbientStore plus timer-wheel plus ShutdownTrigger. Depends on leaf-core only; never the umbrella.
- **contains:** TokioAmbient (task_local), TokioExecutionFacility (spawn/spawn_blocking/Semaphore), reactive timer-wheel for SchedulerCore plus retry, primary applicationTaskExecutor Infrastructure bean; AmbientStore backing context-scope InstanceStores (missing-executor HARD-FAIL at refresh); async ResourceReader/FileResourceProvider; AsyncDispatchInterceptor plus availability watch-cell; tokio signal ShutdownTrigger
- **deps:** leaf-core; tokio; futures

### `leaf-smol` — Alternative runtime integration crate (normal lib): mirror of leaf-tokio over smol, proving the runtime-agnostic seam. Depends on leaf-core only.
- **contains:** SmolAmbient plus SmolExecutionFacility, timer-wheel, primary executor bean; smol-backed mirrors of leaf-tokio lifecycle/events/resource/bootstrap impls
- **deps:** leaf-core; smol; futures

### `leaf-cron` — Small helper crate (normal lib): the 6/7-field calendar engine plus next-fire plus missed-fire policy, parsed at the startup validation pass. Separate so the scheduler does not pull a calendar parser into core.
- **contains:** cron calendar engine plus next-fire plus missed-fire policy consumed by SchedulerCore
- **deps:** leaf-core

### `leaf-tx` — Cross-cutting concern feature crate (normal lib; thin macros from leaf-macros). Ships Infrastructure AdvisorDescriptors. Concrete managers live in integration crates as ordinary beans. Force-linked by enable_transaction_management.
- **contains:** transactional lowering, TransactionInterceptor over TxResourceKey plus TxSyncRegistry, propagation, rollback-rule matching, tx AdvisorDescriptor; transactional_event_listener thin macro emitting the events row with defer
- **deps:** leaf-core; futures; smallvec

### `leaf-cache` — Cross-cutting concern feature crate (normal lib): caching advice plus in-memory default. Backends are separate integration crates contributing Arc dyn CacheManager beans.
- **contains:** cacheable/cache_put/cache_evict, CacheInterceptor, typed MethodKey-CacheKey store plus single-flight map, in-memory default Cache
- **deps:** leaf-core; futures; smallvec

### `leaf-validation` — Cross-cutting concern feature crate (normal lib): bean/method validation. No central registry; impls travel with types.
- **contains:** derive Validate, built-in constraint fns, recursive ValidationContext plus cycle guard, MethodValidationInterceptor, binder-side adapter reusing the shared ErrorSink
- **deps:** leaf-core

### `leaf-resilience` — Cross-cutting concern feature crate (normal lib): retry plus concurrency limiting. enable_resilient_methods with the mandatory two-advisor self-check.
- **contains:** RetryTemplate/RetryPolicy/BackoffPolicy, RetryInterceptor plus ConcurrencyLimitInterceptor over leaf-core ConcurrencyGate
- **deps:** leaf-core; leaf-cron

### `leaf-i18n` — Optional concept-owning integration crate (normal lib): declares the LocaleKey holder (Propagation Inherit) via holder, declare-once-enforced at freeze. Owns the i18n concept without putting it in core.
- **contains:** LocaleKey (Inherit) via holder; locale-sensitive message/catalog concerns on the core CATALOGS/MessageSource ABI
- **deps:** leaf-core

### `leaf-serde` — Optional feature-gated bridge crate (normal lib): serde-bridge converter plus ConfigDeserializer alternate, behind a cargo feature so FromConfigValue stays canonical.
- **contains:** optional serde-bridge converter plus ConfigDeserializer alternate
- **deps:** leaf-core; serde

### `leaf-figlet` — Small optional crate (normal lib): banner template default plus ANSI auto/always/never plus NO_COLOR/tty detection. Self-contained.
- **contains:** banner rendering default plus ANSI/NO_COLOR/tty detection
- **deps:** —

### `leaf-starter-redis` — CAPABILITY starter (aggregator crate; Cargo.toml only). Depends on one integration crate (leaf-redis) plus peers (leaf-tokio). Named once; pulled by a dep-hidden umbrella feature. Never depends on the umbrella.
- **contains:** single-capability aggregator; its presence auto-adds leaf-redis plus leaf-tokio to the umbrella force-link set plus ExpectedManifest
- **deps:** leaf-redis; leaf-tokio

### `leaf-starter-web` — STACK starter (aggregator crate; Cargo.toml only): spring-boot-starter-web analogue. Curated additive bundle. Backend choice is runtime/profile, never an XOR feature. Never depends on the umbrella.
- **contains:** curated multi-crate stack aggregator (router plus tokio plus json plus validation) auto-added to force-link plus ExpectedManifest when its umbrella feature is on
- **deps:** leaf-router; leaf-tokio; leaf-json; leaf-validation

### `leaf` — The umbrella/facade crate (normal lib): the BOM coordination point and single dependency a downstream app names. Re-exports core plus macros plus enabled integrations behind dep-hidden capability features; its main/build.rs owns starter force-link plus ExpectedManifest. Its own version transitively pins the aligned set. Only crate depending on starters; nothing depends on it (DAG sink).
- **contains:** dep-hidden capability features (redis, web), prelude re-exports, force-link shim plus const ExpectedManifest, CreatorPolicy capability-lattice at main; the binary-crate codegen seam (main plus build.rs) feeding the ExpectedManifest self-check across all linkme channels
- **deps:** leaf-core; leaf-macros; leaf-boot; leaf-tokio; leaf-starter-redis; leaf-starter-web; other leaf-starter-*

### `leaf-redis (representative integration crate)` — Representative ecosystem integration crate (normal lib): the pattern every binding follows (leaf-router, leaf-json, leaf-sqlx-tx, cache backends, leaf-servlet/leaf-reactive analogues). Ships auto_config rows plus Infrastructure Providers/Advisors. Depends on leaf-core only (plus a runtime/3rd-party lib); never the umbrella; contributes data plus Providers, never an Engine impl or kernel strategy.
- **contains:** RedisAutoConfig (auto_config) emitting AUTO_CONFIGS rows with CandidateRole Fallback plus CondExpr guards; web-flavor crates: one AppTypeDeducer plus capability marker plus FlavorSeeder plus ContextFactory; backend crates: Arc dyn CacheManager / TransactionManager beans
- **deps:** leaf-core; leaf-tokio; the underlying 3rd-party library

## Starters & BOM

STARTERS are leaf-starter-* aggregator crates (Spring-Boot-starter analogue) in two shapes: a CAPABILITY starter (leaf-starter-redis: one integration crate plus peers) and a STACK starter (leaf-starter-web equals router plus tokio plus json plus validation). A starter is almost code-free (pure Cargo.toml), named once, composed with the umbrella via a dep-hidden feature (on leaf: redis maps to dep:leaf-starter-redis). TWO-GATE ACTIVATION, forced by T1 because a cargo feature gates COMPILATION not LINKAGE so a starter must do both: an integration auto-config PARTICIPATES (compiled, force-linked, self-checked, in the candidate batch) iff in the force-link set equals enabled capability feature UNION explicit scan list, never an arbitrary transitive dep; it WIRES iff additionally its runtime CondExpr guard matches AND not excluded AND, as a Fallback candidate, loses to no user bean. Enabling the umbrella redis feature pulls leaf-starter-redis (which pulls leaf-redis plus leaf-tokio) AND the umbrella leaf-main/build.rs force-link shim adds each pulled integration crate to the participating set (use leaf_redis as underscore) and to the const ExpectedManifest. An enabled-but-empty starter is a loud self-check failure; a DCE-vanished starter is AntiDceError SourceVanished naming the crate. THE AUTO-CONFIG ENGINE lives in leaf-boot as run_autoconfig over a SECOND linkme channel leaf_core AUTO_CONFIGS, one cold sync pass in App Resolve after user defs, ladder exclude(ContractId) then user-bean back-off(OnMissingBean) then auto-config default, registering survivors incrementally so each DefinitionProbe sees the growing set; auto-config beans register CandidateRole Fallback so a user bean transparently supersedes. Kill-switch leaf.enable-autoconfiguration equals false. Test slices reuse the SAME engine over an explicit subset via the slice macro. BOM is deliberately DUAL because Cargo has no native BOM. INTERNAL: workspace.dependencies at the leaf workspace root pins one version of leaf-core, leaf-macros, linkme, and every integration crate, all members using key.workspace equals true, enforcing the load-bearing single-copy-of-leaf-core invariant (two copies equals two AUTO_CONFIGS slices equals a silently split batch); resolver equals 3 set explicitly so dev/build/target features do not leak into the production candidate set. DOWNSTREAM: the version-pinned leaf umbrella IS the BOM surrogate; a user writes one dependency leaf with version 1.4 and features redis and web, and because leaf 1.4 pins exact internal versions of every starter/integration crate it re-exports, picking the umbrella version transitively pins the aligned set. Integration and starter crates depend on leaf-core never the umbrella. A leaf-bom doc-table (not an importable constraint crate, since Cargo has no dependencyManagement import-scope) ships the mutually-tested version tuple for non-umbrella users, the honest escape hatch per charter 2.10. The umbrella is the blessed path.
