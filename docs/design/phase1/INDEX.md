# Phase 1 — Feature Design Catalog & Index

Per-feature, implementation-agnostic rust-native design explorations for leaf. 70 catalog features + the Bootstrap layer; **96 total designs** across 7 areas (incl. critic-surfaced features). Each offers 2-3 divergent realizations with pros/cons/mechanisms, T1 (cross-crate) & T2 (exec-context) notes, Rust clashes, interactions, and Phase-2 questions. Nothing here is decided — that is Phase 2/3.

## Area documents

- [`01-container-beans.md`](01-container-beans.md) — Container & Beans
- [`02-configuration-environment.md`](02-configuration-environment.md) — Configuration & Environment
- [`03-conditions-auto-configuration.md`](03-conditions-auto-configuration.md) — Conditions & Auto-configuration
- [`04-aop-interception.md`](04-aop-interception.md) — AOP & Interception
- [`05-events-lifecycle.md`](05-events-lifecycle.md) — Events & Lifecycle
- [`06-cross-cutting-abstractions.md`](06-cross-cutting-abstractions.md) — Cross-cutting Abstractions
- [`07-bootstrap.md`](07-bootstrap.md) — Bootstrap
- [`PHASE2-REGISTER.md`](PHASE2-REGISTER.md) — consolidated cross-cutting tensions seeding Phase 2

## Feature catalog (by area)

### Container & Beans
- **container-core** — Provides the central registry that turns configuration metadata into a wired object graph, with the two-layer split (bare engine vs. context-services façade) and auto-detection of post-processors that makes declarative features work.
- **bean-definition** — Represents each bean as editable metadata (class, scope, args, init/destroy, role, primary/fallback, parent template + merged definition) decoupled from instances, enabling parse-then-instantiate and definition rewriting. _(deps: container-core)_
- **bean-registry** — The full-featured registry/engine that stores definitions, manages the singleton cache, and performs candidate resolution; the workhorse every context composes (HAS-A) rather than subclasses. _(deps: container-core, bean-definition)_
- **container-hierarchy** — Lets a context delegate lookups upward with one-directional visibility (child sees parent, not vice versa; local shadows parent; injection is hierarchy-aware but listing is local-only), enabling layered isolation. _(deps: bean-registry)_
- **post-processor-spi** — Separates editing the blueprint (BFPP/BDRPP on definitions) from decorating the product (BPP on instances, incl. MergedBeanDefinitionPostProcessor) so the framework can dogfood its own features as ordered infrastructure beans. _(deps: bean-registry)_
- **bean-instantiation** — Creates and caches bean instances (eager non-lazy singletons, prototypes the container forgets), publishing them thread-safely under a creation lock with the three-level cache that backs early references. _(deps: bean-registry, scopes)_
- **autowiring-resolution** — Resolves a single-valued injection point by a layered, fail-fast policy (type, generics, qualifier, primary/fallback, name) that refuses to guess on ambiguity rather than silently picking a candidate. _(deps: bean-registry, candidate-resolver)_
- **candidate-resolver** — The pluggable resolver chain plus qualifier/generic-type matching that decides which beans *qualify* for an injection point (qualifiers, custom qualifier annotations, generics-as-implicit-qualifier, autowire/default-candidate flags), distinct from winner selection. _(deps: bean-definition, type-conversion)_
- **primary-fallback** — Resolves single-valued ambiguity by promoting one candidate (@Primary) or demoting library defaults (@Fallback) so a user bean transparently supersedes a shipped default across module boundaries. _(deps: autowiring-resolution)_
- **collection-injection** — Injects all matching beans as List/Set/array (ordered via @Order/@Priority/Ordered) or as a name-keyed Map, where ordering is a consumer concern strictly separate from startup order. _(deps: autowiring-resolution, ordering)_
- **injection-styles** — Supports constructor (preferred, mandatory/immutable), setter (optional), and field injection so beans stay POJOs whose collaborators are part of their identity and testable without the container. _(deps: autowiring-resolution)_
- **deferral-primitives** — Makes deferred, optional (0..1), multiple (0..N), lazy, or cycle-breaking access explicit in the type signature via ObjectProvider/ObjectFactory/jakarta Provider and lazy-resolution proxies, keeping the eager default intact. _(deps: autowiring-resolution, lazy-initialization)_
- **lazy-initialization** — Defers a singleton's creation from refresh to first access (per-bean @Lazy or a global flag with exclude filters), relocating rather than removing failure for genuinely expensive beans. _(deps: bean-instantiation)_
- **circular-references** — Tolerates setter/field singleton cycles via an early-singleton reference (with AOP early-proxy exposure) while failing fast on logically-unsatisfiable constructor cycles. _(deps: bean-instantiation, aop-proxying)_
- **factory-bean** — Lets a bean act as a programmatic, annotation-free factory whose product (not the factory) is what consumers resolve, with separate caching/lifecycle and the & dereference escape hatch for complex stateful construction. _(deps: bean-instantiation)_
- **scopes** — Defines instance multiplicity/lifetime (singleton, prototype, request/session/application/websocket, custom via the Scope SPI), the primary concurrency design lever (stateless→singleton, stateful→prototype). _(deps: bean-definition)_
- **bean-naming** — Derives deterministic default names (decapitalized class name, method name for @Bean), supports multiple aliases/namespaces, and turns name collisions into loud overriding/conflict errors rather than silent shadowing. _(deps: bean-definition)_
- **component-stereotypes** — Marks managed beans via @Component and a user-extensible, meta-annotated stereotype family (@Service/@Repository/@Controller) separating architectural semantics from container behavior. _(deps: annotation-model, bean-definition)_
- **component-scanning** — Discovers candidate beans by querying classpath metadata (read without loading), applying include/exclude filters, and registering definitions with name/scope/proxy resolution — convention-over-configuration discovery. _(deps: component-stereotypes, annotation-model)_
- **programmatic-registration** — Offers a functional, AOT-analyzable contract for imperative/conditional/looping bean registration with an instance-supplier model, replacing low-level BeanDefinition-centric escape hatches. _(deps: bean-registry, import-composition)_
- **background-bootstrap** — Marks slow independent singletons for concurrent init on a bootstrap executor (with explicit dependency modeling and lenient/thread-aware singleton locking) — a surgical tool for the real startup bottleneck. _(deps: bean-instantiation, task-execution)_

### Configuration & Environment
- **value-injection** — Injects externalized values into beans via ${property} placeholders, #{SpEL} expressions, and defaults, with type conversion at the injection point. _(deps: environment, property-resolution, type-conversion, expression-language)_
- **profiles** — Registers named groups of beans only when active (@Profile is just a @Conditional), with expression grammar and active/include/group levers, expressing build-once-configure-per-environment structurally. _(deps: conditions, environment)_
- **environment** — Unifies the two facets of where the artifact runs — active profiles and a property-source stack — behind one read/mutate-segregated abstraction reachable everywhere.
- **property-sources** — Holds named key/value backing sources in an ordered, first-source-wins stack with explicit ordering mutation, deliberately never blending values so every property has one traceable origin. _(deps: environment)_
- **property-resolution** — Resolves keys and ${...} placeholders (with defaults, recursive resolution, strict-vs-lenient modes) over the source stack — the read path the whole config model depends on. _(deps: property-sources)_
- **config-data** — Loads application/profile config files with a deterministic single-pass model (locations, spring.config.import, env:/configtree: prefixes, multi-document activation) and fixes Boot's full externalized-config precedence order. _(deps: property-sources, profiles, origin-tracking)_
- **origin-tracking** — Wraps loaded values with their provenance (file/line, owning source) so a deep precedence stack stays self-documenting for error messages and diagnostics. _(deps: property-sources)_
- **relaxed-binding** — Maps one canonical kebab-case property to many source-specific forms (camelCase, snake_case, UPPER_SNAKE env vars, list/map index conventions) so a key's identity is decoupled from each transport's syntax. _(deps: property-resolution)_
- **config-properties** — Binds a property tree onto a strongly-typed, validated, documented POJO/record (JavaBean or constructor binding) rooted at a prefix, replacing scattered @Value with a cohesive fail-fast config object. _(deps: relaxed-binding, binder, type-conversion, validation)_
- **binder** — The programmatic engine that binds ConfigurationPropertySources onto a Bindable target with bind-method selection, handlers, and a result monad — the machinery @ConfigurationProperties uses internally. _(deps: relaxed-binding, property-resolution)_
- **config-metadata** — Emits compile-time metadata (groups, properties, descriptions, hints) describing accepted configuration, making config a discoverable self-documenting API surface for tooling. _(deps: config-properties)_

### Conditions & Auto-configuration
- **configuration-classes** — Lets Java code declare bean definitions via @Bean factory methods, with full mode (interception so inter-bean calls return the managed singleton) vs lite mode (plain factory methods, parameter-injected, faster/AOT-friendly). _(deps: post-processor-spi, bean-definition)_
- **import-composition** — Composes configuration at four altitudes (config classes, ImportSelector, DeferredImportSelector with Group merging, ImportBeanDefinitionRegistrar) plus ImportAware as the backbone of @Enable* annotations. _(deps: configuration-classes)_
- **conditions** — Reduces any registration decision to a pure predicate over container state plus the triggering annotation's metadata (AND semantics, two-phase parse-vs-register model), so registration is declarative and composable. _(deps: annotation-model, environment)_
- **condition-family** — Provides the rich parameterized condition set (OnClass/OnMissingClass, OnBean/OnMissingBean/OnSingleCandidate, OnProperty, OnExpression, OnResource, OnWebApplication, etc.) and nested-condition boolean helpers built on the Condition SPI. _(deps: conditions)_
- **condition-report** — Records every match/no-match with a human-readable reason (positive/negative/exclusions) so heavy convention-over-configuration stays accountable and debuggable. _(deps: condition-family)_
- **auto-configuration** — Applies opinionated, conditional, ordered default configuration loaded from an imports file via a deferred selector that runs after user config and backs off (@ConditionalOnMissingBean) — opinionated but always overridable. _(deps: import-composition, condition-family, auto-config-ordering)_
- **auto-config-ordering** — Deterministically orders the whole auto-config batch (alphabetical, @AutoConfigureOrder, topological before/after with cycle detection) and merges exclusion sources, treating class names as public API. _(deps: import-composition, ordering)_

### AOP & Interception
- **scoped-proxies** — Lets a longer-lived bean hold a stable proxy that resolves the live shorter-lived (request/session/prototype) target per call, preserving IoC without leaking scope-lookup logic into business code. _(deps: scopes, aop-proxying)_
- **aop-proxying** — Wraps a target bean in a proxy (as a BeanPostProcessor returning a different object) so cross-cutting advice attaches at the DI seam without invading source code — the substrate for all declarative interception. _(deps: post-processor-spi)_
- **auto-proxy-creator** — A single per-context proxy creator that collects all matching advisors into one ordered interceptor chain per bean (one proxy, many concerns), with infrastructure-vs-aspect isolation via role hints. _(deps: aop-proxying, ordering)_
- **aspect-model** — Lets users declare reusable cross-cutting behavior (pointcuts matching join points, before/after/around advice, @Aspect beans) turned into advisors woven by the auto-proxy creator. _(deps: auto-proxy-creator)_
- **async-execution** — Runs a method on a TaskExecutor via an AOP proxy with executor selection and async exception handling — declarative asynchrony with the self-invocation/void-return caveats. _(deps: auto-proxy-creator, task-execution)_

### Events & Lifecycle
- **refresh-lifecycle** — A fixed-order template that builds the container in dependency layers (load definitions, run all BFPPs, register all BPPs, init message source/multicaster, eager-instantiate singletons), making bring-up a predictable, all-or-nothing state machine. _(deps: post-processor-spi, bean-instantiation)_
- **lifecycle-callbacks** — Runs ordered initialization (@PostConstruct, afterPropertiesSet, init-method) and mirrored destruction (@PreDestroy, destroy, destroy-method with close/shutdown inference) per bean, with prototypes never destroyed. _(deps: bean-instantiation)_
- **aware-callbacks** — Injects container infrastructure (bean name, factory, environment, context, resource loader, event publisher) into infrastructure beans that genuinely need container access, fired as a group before init. _(deps: bean-instantiation, container-core)_
- **smart-initializing** — Provides a single lock-free callback after every eager singleton exists, the correct place for expensive cross-bean post-init work that would deadlock in @PostConstruct. _(deps: bean-instantiation, refresh-lifecycle)_
- **runtime-lifecycle** — Layers a phase-ordered, auto-started, gracefully-stoppable running-state machine (start/stop with phases, async stop callback, graceful drain timeouts) on top of the static singleton graph. _(deps: refresh-lifecycle, ordering)_
- **context-close** — Tears down the context by stopping Lifecycle beans first (quiesce runtime activity), then destroying singletons in dependency-honoring reverse order via shutdown hook or explicit close. _(deps: runtime-lifecycle, lifecycle-callbacks)_
- **startup-instrumentation** — Records a tree of named, timed container startup steps (no-op by default, opt-in recording) to diagnose slow boots with a stable, tool-friendly taxonomy. _(deps: refresh-lifecycle)_
- **events** — An in-process observer pub/sub on ApplicationContext where a publisher emits a fact with no knowledge of reactors (classic ApplicationListener + preferred @EventListener POJO model, payload events, generics), giving open/closed wiring. _(deps: container-core, expression-language)_
- **event-multicaster** — Routes all publishing through one replaceable multicaster with synchronous-by-default ordered dispatch (preserving caller's transaction/context) and opt-in async, error handling, and return-value-as-new-event chaining. _(deps: events, ordering, task-execution)_
- **lifecycle-events** — Publishes context lifecycle facts (refreshed, started, stopped, closed) and availability state (liveness/readiness) on the same bus so app code and probes can react to container milestones. _(deps: events, refresh-lifecycle)_
- **transactional-events** — Defers @EventListener invocation to a transaction phase (before/after commit/rollback) via the extensible listener-adapter layer, illustrating how the event model is built upon. _(deps: events, transaction-management)_

### Cross-cutting Abstractions
- **annotation-model** — Treats annotations as a composable, user-extensible vocabulary with attribute forwarding (@AliasFor), distance-ordered merging, and metadata readable without loading the class — the engine under stereotypes, conditions, and @Enable*.
- **exception-translation** — Weaves an advisor onto @Repository beans that converts vendor-specific persistence exceptions into a consistent technology-agnostic DataAccessException hierarchy, keeping DAOs framework-unaware. _(deps: aop-proxying, error-model)_
- **task-execution** — Applies IoC to threads — code declares 'run this somehow' and the container chooses pool/virtual-thread/decorated backing by configuration, enabling central tuning, monitoring, and lifecycle participation. _(deps: container-core)_
- **scheduling** — Registers @Scheduled methods (cron/fixedRate/fixedDelay) against a TaskScheduler abstraction, with virtual-thread support, for declarative periodic work. _(deps: post-processor-spi, task-execution)_
- **transaction-management** — Applies declarative transaction demarcation via an interceptor woven by the auto-proxy creator, binding resources to the execution context — the canonical declarative cross-cutting concern. _(deps: auto-proxy-creator, context-propagation)_
- **caching** — Applies declarative caching via an interceptor that consults/populates a cache around method calls, ordered within the shared proxy chain alongside transactions. _(deps: auto-proxy-creator, expression-language)_
- **validation** — Applies declarative method-argument/return and bean validation (JSR-303 constraints) via an interceptor, failing fast with descriptive errors and reused by config-properties binding. _(deps: auto-proxy-creator)_
- **retry-resilience** — Applies declarative resilience — retry with policy/backoff and concurrency limiting — via interceptors (and a RetryTemplate primitive), positioned for the unbounded-concurrency virtual-thread world. _(deps: auto-proxy-creator, task-execution)_
- **expression-language** — Provides a runtime expression layer for #{...} values, condition filtering, cache keys, and bean references — the evaluation engine many declarative features delegate to. _(deps: type-conversion)_
- **type-conversion** — A central ConversionService/formatter registry that coerces strings to typed values (numbers, durations/sizes, enums, collections) used by @Value, binding, and autowiring.
- **messages-i18n** — Resolves localized, parameterized messages through a hierarchy-aware MessageSource bean (magic-named), giving the context first-class internationalization. _(deps: container-core, locale-context)_
- **resource-loading** — Abstracts access to classpath/file/URL resources through a ResourceLoader / pattern-resolving abstraction the context exposes, used by scanning, config loading, and conditions. _(deps: container-core)_
- **context-propagation** — Generalizes the holder/strategy pattern over ambient context (request/locale/transaction/tracing) into a capture/restore SPI so context survives thread hops introduced by async execution. _(deps: task-execution)_
- **locale-context** — Exposes 'the current X' (locale, request attributes, transaction sync) as ambient per-execution lookups via named thread-bound holders, the inputs context propagation must move across boundaries.
- **concurrency-contract** — Guarantees safe publication/visibility of fully-built singletons (not runtime method thread-safety), framing scope choice as the primary concurrency design tool and delegating mutable-state safety to the developer. _(deps: bean-instantiation, scopes)_

### Bootstrap
- **spring-application-run** — leaf's single opinionated bootstrap entry point — the SpringApplication.run analogue — that replaces hand-wiring a container. Its intent (pe…
- **application-type-deduction** — leaf's rust-native realization of Spring Boot's WebApplicationType.deduce() (spring-boot-4-di-design.md §246-254, "Classpath-driven applicat…
- **application-arguments** — leaf's realization of Spring Boot's `DefaultApplicationArguments` + `CommandLinePropertySource` (spring-boot-4-di-design.md §run() line 244 …
- **context-initializers** — leaf's realization of Spring's ApplicationContextInitializer hook — the ordered, discoverable set of programmatic customizations applied to …
- **run-listeners-events** — This feature is the SpringApplicationRunListener analogue plus the fixed, named bootstrap event sequence — NARROWER than (and sitting inside…
- **runners** — The readiness-gating callback window. Per spring-boot-4-di-design.md §305-309 ("Runners: ApplicationRunner / CommandLineRunner") and the run…
- **failure-analysis** — leaf's realization of the boot-layer "a startup failure should TEACH, not dump a stack trace" surface — the FailureAnalyzer to FailureAnalys…
- **banner** — leaf's configurable startup banner — the SpringApplicationBannerPrinter/printBanner analogue (spring-boot-4-di-design.md §"Banner", lines 30…
- **exit-code-shutdown** — leaf's APPLICATION-LEVEL exit-code + shutdown-coordination layer — the SpringApplication.exit() / ExitCodeGenerator / ExitCodeExceptionMappe…
- **bootstrap-context** — leaf's TRANSIENT, PRE-CONTAINER registry — the ConfigurableBootstrapContext / BootstrapRegistry analogue (Spring intent: spring-boot-4-di-de…

