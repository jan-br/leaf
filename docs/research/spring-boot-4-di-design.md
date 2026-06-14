# The Design Philosophy of Spring Boot 4 / Spring Framework 7 Dependency Injection

> A grounded engineering reference to the **core IoC / dependency-injection layer** of Spring (deliberately excluding ecosystem integrations such as Web MVC/WebFlux, Data/JPA, Security, and Messaging).
>
> Anchored to **Spring Boot 4.x / Spring Framework 7.x** (Spring Framework 7.0 GA Nov 13 2025; Spring Boot 4.0 GA Nov 20 2025). Synthesized from 23 core deep-dives + 20 recursively-surfaced concepts (43 findings); every factual claim was adversarially verified against live docs/source and corrections were folded directly into the text. **542** sources cited (Appendix).
>
> Built as the conceptual foundation for designing a Rust-idiomatic DI framework (Task 2).


## Contents

- [Mental Model & Core Philosophy](#mental-model--core-philosophy)
- Foundations & the IoC Container
- Components, Stereotypes & Scanning
- Beans: Wiring, Naming & Lifecycle
- Configuration, Conditions & Auto-configuration
- Environment, Profiles & Properties
- Proxies, AOP & Events
- Concurrency, Context Propagation & Error Handling
- Spring Boot 4 / Framework 7: What Changed & Why
- Deeper & Hidden Concepts
- Appendix — Sources


---

## Mental Model & Core Philosophy

### The problem: assembly is a cross-cutting concern

Every non-trivial application is two things braided together: *business logic* (what your objects do) and *assembly* (how those objects find, create, and connect to their collaborators). Left to its own devices, assembly metastasizes into the business code — objects `new` up their dependencies or reach into a registry to look them up. The result is code that is hard-coupled to concrete implementations, opaque about what it actually depends on, and effectively untestable without standing up the whole world.

Inversion of Control (IoC) is the decision to *extract* assembly as a separate concern and hand it to infrastructure. Dependency Injection (DI) is the specific, disciplined form of IoC that Spring chose: rather than an object pulling its collaborators from a Service Locator (which only trades a `new` dependency for a coupling to the lookup API, and still hides dependencies behind runtime calls), an object simply *declares* what it needs — as constructor parameters, factory-method arguments, or setters — and something external supplies them.

The payoff is that **every dependency becomes visible in a plain Java signature.** A class with a three-argument constructor honestly advertises its three collaborators. You can instantiate it in a unit test with `new Service(mockA, mockB, mockC)` and no framework at all. This is the deepest principle in the entire system: DI is a *design discipline* that is valuable independently of any container. The container is just one mechanism for performing the wiring; it is deliberately optional infrastructure, not a hard runtime dependency baked into your types.

### Your code stays POJO; the container assembles

This yields the central mental model: **you write plain old Java objects, and the container assembles them into a running application.** Your classes do not extend a framework base class, do not implement a lookup interface, and ideally do not import a single framework type in their core logic. They express their needs and their behavior; the container reads metadata describing how the pieces fit and constructs the object graph.

Crucially, the container reasons over a *description* of every bean before any instance exists. Each bean is captured as a `BeanDefinition` — a mutable metamodel object recording its class, scope, laziness, constructor arguments, qualifiers, init/destroy methods, primary/fallback status, and so on. This decoupling of *description* from *instantiation* gives a clean two-phase model — first register and rewrite all definitions, then post-process and instantiate them — which is the structural reason the framework is so extensible (you can rewrite blueprints before any object is built) and so format-agnostic (XML, annotations, or programmatic registration all converge on the same metamodel).

### A layered progression: BeanFactory → ApplicationContext → Boot

Spring delivers this worldview as a deliberate progression of layers, each composing on the one beneath:

- **`BeanFactory` — the bare DI engine.** The root contract: hold definitions, perform DI, manage singleton/prototype lifecycle. It is minimal and embeddable, lazy by default, and makes *no assumptions* about annotations or configuration format. The real workhorse implementation, `DefaultListableBeanFactory`, is where autowiring, scoping, and singleton management actually live.

- **`ApplicationContext` — the enterprise superset.** Not a different container but a *façade* that composes a single `DefaultListableBeanFactory` and decorates it with the conveniences real applications want: automatic detection of `BeanFactoryPostProcessor`s and `BeanPostProcessor`s, application events, i18n (`MessageSource`), resource loading, and the `Environment`. The auto-detection of post-processors is the load-bearing addition: annotation processing, AOP proxying, `@Configuration` parsing, and placeholder resolution are *themselves* implemented as post-processors, so a declarative `@Configuration` only becomes working behavior because the context finds and runs them. This is precisely why those features silently do nothing in a plain `BeanFactory`, and why `ApplicationContext` is the default for all but the most constrained scenarios.

- **Spring Boot — opinionated auto-configuration.** Boot adds *judgment* on top of the context. `SpringApplication.run(App.class, args)` is a single opinionated entry point that replaces hand-wiring a context: it deduces the application type from the classpath, prepares the `Environment`, selects and creates the right context, runs initializers, refreshes, and invokes runners — firing a fixed, named event sequence at every phase. Auto-configuration then contributes hundreds of sensible default beans, each guarded so that it steps aside the moment you define your own.

Layered *contexts* also compose horizontally via parent/child hierarchies with strictly one-directional visibility: a child sees its ancestors' beans but never vice versa. Shared infrastructure is defined once "up"; specialized components live "down" and may locally override a parent bean — clean namespaces, no leakage, no sibling coupling.

### The recurring design themes

Across all of these layers, the same handful of values recur. They are worth naming explicitly, because once you internalize them, most of Spring's specific decisions become predictable rather than arbitrary.

**Convention over configuration.** The classpath *is* the configuration: adding `spring-boot-starter-web` makes the app a servlet app; the package of your `@SpringBootApplication` *is* your scan boundary; a bean's name *is* its decapitalized class name. Defaults are chosen so that the common case requires zero ceremony, while explicit escape hatches remain for the rest.

**Declarative metadata via annotations, treated as a composable vocabulary.** Annotations are not magic keywords; they are reusable, named bundles of configuration intent. The framework knows only a few base concepts (`@Component`, `@Configuration`, `@Scope`, `@Qualifier`...), and *everything else is composition on top*. `@Service`, `@RestController`, and your own `@MyApiController` are recognized automatically because they are meta-annotated with the primitives, with `@AliasFor` and the `MergedAnnotations` API providing the attribute-forwarding that Java's own annotations lack. Application teams and libraries can mint their own vocabulary and the container "just understands" it.

**Composable, overridable defaults that yield to user intent.** This is auto-configuration's soul. Defaults are not features you must opt out of; they are *conditional* and back away when you express your own preference. `@ConditionalOnMissingBean` guards framework beans, and auto-configuration is processed *after* all user configuration (via a `DeferredImportSelector`) precisely so that your beans win silently, with zero disabling flags. `@Fallback` even lets a library ship a default that any application bean transparently supersedes. The philosophy inverts the usual framework default: *opinionated, but never fighting the user.*

**Fail-fast on ambiguity and misconfiguration.** When the container cannot prove a unique intended target, it refuses to guess — `NoUniqueBeanDefinitionException` names the exact injection point and the candidate set, rather than silently picking first/newest/alphabetical and planting a correctness landmine far from its cause. The same instinct drives eager pre-instantiation of non-lazy singletons at startup (a context that started is a context that works), disabling silent bean-definition overriding in Boot, and rejecting unsatisfiable constructor cycles outright. The mental model it instills: *wiring must be unambiguous by construction; any ambiguity is a design decision the developer must make explicit.*

**The framework is built on its own public extension points.** Spring eats its own dog food. `@Configuration`/`@Bean` parsing, `@Autowired`, `@PostConstruct`, AOP, and `${...}` placeholder resolution are not privileged compiler tricks — they are ordinary `BeanFactoryPostProcessor`s and `BeanPostProcessor`s registered automatically. This proves the extension points are powerful enough for real work, keeps the core container small and annotation-agnostic, and means you extend Spring using the exact same mechanism Spring uses to implement itself. The two tiers are deliberately distinct: BFPPs *mutate the blueprint* before anything is built; BPPs *decorate the product* as it is built. Definitions are data you edit; instances are objects you decorate.

**A clean division of labor: the container owns assembly, the developer owns runtime concurrency.** The container guarantees *safe publication* of singletons — their post-construction state is visible across threads — but it explicitly does *not* make your bean logic thread-safe. Singletons are shared across all request threads precisely because most collaborators are stateless; statefulness is your signal to choose a narrower scope or guard it yourself. Spring solves the JMM problem it can solve generically and hands back the problem only you understand. The same line is drawn everywhere: the container assembles the graph; you decide what runs concurrently inside it, choosing scope as your primary concurrency design tool and talking to the `TaskExecutor` abstraction rather than to threads.

**The v4/v7 shift: move work from runtime to build time via AOT.** Historically, Spring's power came from runtime dynamism — reflective scanning, condition evaluation, proxy generation, all performed on every startup. Spring Framework 7 / Boot 4 reframe this: since that work is deterministic given a fixed classpath and `Environment`, it can be done *once*, at build time. The AOT engine refreshes the context *without instantiating beans*, freezes conditions and profiles, and emits plain, readable Java source — explicit `BeanDefinition` and instance-supplier code — plus typed `RuntimeHints` for reflection and resources. The result is a largely static, reflection-free bean graph compatible with GraalVM's closed world, with dramatically faster startup and lower memory. The mental model sharpens accordingly: *definitions are knowable at build time; instances are not.* This is why v7 introduces `BeanRegistrar` as the blessed, AOT-analyzable path for programmatic registration, why explicit injected cycles now fail under AOT (use `@Lazy`/`ObjectProvider` instead), and why the ecosystem-wide default leans toward lite-mode configuration and explicit suppliers over runtime interception. The dynamism that defined Spring for two decades is being traded, deliberately and per-artifact, for build-time determinism.

Taken together, these themes describe a single coherent stance: **keep application code as honest plain Java, let the container assemble it from declarative metadata, prefer convention but never lock-in, fail loudly the moment intent is ambiguous, build the framework out of the same seams it offers you, and increasingly resolve at build time what older versions resolved at every startup.** Everything that follows in this reference is an elaboration of that stance.


---

## Foundations & the IoC Container

Spring's foundational thesis is a single inversion: application objects should not acquire their own collaborators. Instead of an object creating or looking up the services it needs, it merely *declares* what it depends on, and an external authority — the container — supplies them. This section establishes that container, its two-layer interface design, the metadata model that drives it, the bootstrap pipeline that builds it, and the lifecycle template that brings it up and tears it down. The headline message for Spring Framework 7 / Spring Boot 4 is that this core is *deliberately stable*: the fundamentals below are unchanged from Framework 6, and the v7/Boot 4 deltas are additive (a first-class `BeanRegistrar`), default-tightening (consistent proxy defaulting), and housekeeping (JSpecify, jakarta-only).

### Inversion of Control: the principle and what it rejects

Inversion of Control (IoC) inverts the *direction of control over dependency acquisition*. In the traditional flow, an object `new`s-up or looks up its own collaborators — it is in control of assembly. Under IoC, the object only declares what it needs (through constructor arguments, factory-method arguments, or properties set after construction) and an external authority injects them.

Dependency Injection (DI) is the *specific implementation* of IoC that Spring uses. The mental model Spring wants to instill is: **the container is the assembler; your code stays a POJO.** Configuration metadata is parsed into bean descriptions, the container instantiates and wires the object graph, and your classes contain no Spring lookup code — dependencies arrive through plain constructors and setters.

Spring explicitly *rejects* the Service Locator alternative. A service locator is also a form of IoC, but it leaves a hidden runtime dependency on the locator API: dependencies are fetched behind opaque `locator.get(...)` calls rather than being visible in the signature. That defeats two things Spring prizes — *explicitness* (every dependency is visible in the constructor) and *testability* (you can instantiate the object with test doubles directly, with no framework present). With DI the container becomes optional infrastructure rather than a hard runtime dependency.

```java
// DI: collaborator is explicit, object is a plain POJO, trivially unit-testable
class OrderService {
    private final PaymentGateway gateway;
    OrderService(PaymentGateway gateway) { this.gateway = gateway; } // container injects
}
```

Spring favors **constructor injection for mandatory dependencies** precisely to keep objects valid POJOs: `final` fields, no partially-constructed state, and an object that is never observable in an invalid configuration.

#### Why hand control to a container at all

Centralizing assembly solves wiring, lifecycle, and cross-cutting concerns *once*, declaratively. The payoffs: loose coupling (a bean depends on an interface, not a concrete construction sequence), a single source of configuration truth, and — critically — a **stable seam** where the container can transparently apply AOP proxies, scoping, lazy initialization, and lifecycle callbacks without the business code knowing. The assembler owns the messy parts; your code never sees them.

### The two-layer container: `BeanFactory` vs `ApplicationContext`

Spring splits the container into two interface layers, and this split is a deliberate design decision rather than an accident of history.

`org.springframework.beans.factory.BeanFactory` is the **root interface** — the basic client view of the container. It is a central registry of bean definitions keyed by `String` name, returning shared (singleton) or independent (prototype) instances and running the full bean lifecycle (Aware callbacks, `BeanPostProcessor` hooks, `InitializingBean`/`afterPropertiesSet`, custom init; on shutdown `DisposableBean`/destroy). It is the "configuration framework and basic functionality" layer. Its sub-interfaces refine it: `ListableBeanFactory` (enumerate beans), `HierarchicalBeanFactory` (parent lookup), and `ConfigurableBeanFactory`/`AutowireCapableBeanFactory` (the configuration SPI).

The crucial property of a *plain* `BeanFactory` is that it is **"agnostic about special beans"**: it does **not** auto-detect post-processors. You must register `BeanPostProcessor`s and `BeanFactoryPostProcessor`s yourself.

`ApplicationContext` is a **sub-interface and complete superset** of `BeanFactory`. It additionally extends `ListableBeanFactory`, `HierarchicalBeanFactory`, `MessageSource`, `ApplicationEventPublisher`, `ResourceLoader`/`ResourcePatternResolver`, and `EnvironmentCapable`. The feature-table difference is sharp: bean instantiation/wiring is provided by both, but **automatic `BeanPostProcessor` registration, automatic `BeanFactoryPostProcessor` registration, integrated lifecycle management, `MessageSource` (i18n), and `ApplicationEvent` publication are `ApplicationContext`-only.**

The design rationale for the split: the layering lets the bare DI engine stay lightweight and embeddable, while the framework-oriented features compose on top. The minority that needs maximal control or minimal footprint can drop to the raw engine and opt into exactly the post-processors they want; everyone else gets the conveniences for free.

#### Why `ApplicationContext` is the default — and why features "silently fail" without it

The reference is explicit: **"use an `ApplicationContext` unless you have a good reason not to."** The reason is mechanical, not merely ergonomic. Annotation processing (autowiring, `@PostConstruct`) and AOP weaving are *themselves implemented as `BeanPostProcessor`s*, and `@Configuration`/`@Bean` parsing and `${...}` placeholder resolution are implemented as `BeanFactoryPostProcessor`s. Auto-detecting these by type is exactly what turns declarative configuration into working behavior with zero wiring. In a plain `BeanFactory` those post-processors are not registered, so the features they implement *silently do nothing*. That is the single most important consequence of the two-layer design.

A raw `DefaultListableBeanFactory` is therefore reserved for embedded/resource-constrained scenarios needing full manual control — there you must hand-wire `AutowiredAnnotationBeanPostProcessor`, `CommonAnnotationBeanPostProcessor`, `PropertySourcesPlaceholderConfigurer`, and so on.

**Hidden behavioral difference — eager vs lazy:** a bare `BeanFactory` creates beans only on first `getBean`, whereas `ApplicationContext.refresh()` **eagerly pre-instantiates all non-lazy singletons at startup**. This is a deliberate fail-fast choice: configuration errors surface at boot rather than under production traffic. `lazyInit`/`@Lazy` toggle this per bean.

### `DefaultListableBeanFactory`: the real engine behind the façade

A pervasive practitioner misconception is that "the container" *is* the `ApplicationContext`. The hidden truth: every `GenericApplicationContext` (and thus `AnnotationConfigApplicationContext` and the web contexts) **owns a single internal `DefaultListableBeanFactory`** that does the actual work — storing the `BeanDefinition` map, performing autowiring and candidate resolution, managing the singleton cache, and resolving dependencies. `DefaultListableBeanFactory` is the full-featured default implementation of the `ListableBeanFactory` and `BeanDefinitionRegistry` SPIs.

The `ApplicationContext` is essentially a **façade/orchestrator** that owns one of these and layers context services (events, i18n, resource loading) on top. The design intent is composition over a single proven registry: rather than reimplementing autowiring/scoping/singleton management per context type, Spring builds every context as a thin layer over the same engine.

Per-factory toggles such as `allowBeanDefinitionOverriding` and `allowCircularReferences` live on the factory and are applied by the context during refresh via `customizeBeanFactory()`.

#### `GenericApplicationContext`

`GenericApplicationContext` is a concrete, flexible `ApplicationContext` that holds one internal `DefaultListableBeanFactory` and implements `BeanDefinitionRegistry`. You can feed it any combination of `BeanDefinitionReader` sources (XML, Groovy, functional registration) and call `refresh()` once. It is the modern base for non-refreshable contexts and is the **recommended target for AOT processing**, because its bean definitions can be frozen and contributed at build time.

#### `AnnotationConfigApplicationContext`

A `GenericApplicationContext` subclass that pre-registers the standard annotation post-processors (`ConfigurationClassPostProcessor`, `AutowiredAnnotationBeanPostProcessor`, `CommonAnnotationBeanPostProcessor`, etc.) and accepts `@Configuration` classes and/or component-scan base packages directly in its constructor. It is the canonical container for Java-config / annotation-driven applications and is what Spring Boot's non-web `SpringApplication` ultimately drives.

```java
var ctx = new AnnotationConfigApplicationContext(AppConfig.class);
OrderService svc = ctx.getBean(OrderService.class);
```

### The `BeanDefinition` metamodel

A `BeanDefinition` is the container's internal description of how to create and configure **one** bean — *metadata, not the instance.* Representing every bean as a mutable metamodel object before any instance exists is a deliberate decoupling that yields a stable two-phase model: **parse/register all definitions, then post-process and instantiate.** This is precisely what lets `BeanFactoryPostProcessor`s rewrite definitions (resolve placeholders, set scopes, add new definitions) before any object is created, which in turn enables configuration-format independence (XML, Java config, Groovy, programmatic all collapse to the same `BeanDefinition`s).

A `BeanDefinition` carries:

- `beanClassName` (or just the class a factory method is invoked on)
- `scope` (singleton / prototype / request / session / custom)
- `lazyInit`
- `dependsOn` (force initialization order)
- `autowireCandidate` (eligible for type-based injection)
- `primary` (tie-breaker among matches) and `fallback`
- `factoryBeanName` + `factoryMethodName` (instance vs static factory methods)
- `constructorArgumentValues`
- `propertyValues` (setter injection)
- `initMethodName` / `destroyMethodName`
- `role` hint (`ROLE_APPLICATION` / `ROLE_SUPPORT` / `ROLE_INFRASTRUCTURE`)
- `description`
- `parentName` (definition inheritance) and the `abstract` flag

Qualifiers are carried on `AbstractBeanDefinition` for fine-grained autowire matching. `BeanDefinitionBuilder` is the programmatic builder.

#### Hidden concept: the *merged* definition

A child definition inherits from a parent via `parentName`. At instantiation time the container computes a **merged** `RootBeanDefinition` — and it is `getMergedBeanDefinition` that actually drives creation, *not* the raw registered definition. Practitioners who inspect only the registered definition can be surprised by inherited values that appear only after merging.

#### Hidden concept: `role` hints

Most developers never set `role`, but it lets tooling and the framework distinguish user beans (`ROLE_APPLICATION`) from internal plumbing (`ROLE_INFRASTRUCTURE`, e.g. the post-processors). It affects what shows up in diagnostics and certain selection logic.

#### Hidden concept: the `@Fallback` autowire candidate

A `@Fallback` bean is the inverse of `@Primary`: it is chosen **only when no non-fallback candidate matches**. This lets a library ship a default implementation that any user-defined bean silently overrides, without `@Primary` gymnastics. Note on provenance: the `@Fallback` selection flag and `BeanDefinitionBuilder.setFallback()` are **from Framework 6.2 (pre-v7), not new in v7** — `setPrimary` dates to 5.1.11. It is widely unknown but should not be attributed to the v7 release.

### Container hierarchies: parent/child contexts

Through `HierarchicalBeanFactory.getParentBeanFactory()`, a context can have a parent, and lookups **delegate upward**. The semantics are deliberately *one-directional*:

- A child can see and inject beans defined in the parent.
- The parent **cannot** see the child's beans.
- A child bean with the same name **shadows/overrides** the parent's.
- `MessageSource` and other lookups also walk the parent chain when not found locally.

**Hidden concept — `containsLocalBean`:** ordinary `getBean`/`containsBean` lookups delegate to ancestors, but `containsLocalBean()` deliberately ignores ancestors and checks only the local factory. The asymmetry *is* the point: it enforces layered isolation. The classic use is a root context holding services/data beans shared by several focused child contexts (e.g. the web layer), so common infrastructure is defined once while each layer keeps a private namespace. This asymmetry is also a common source of "why is the parent's bean being injected here?" confusion.

### Two extension tiers: `BeanFactoryPostProcessor` vs `BeanPostProcessor`

A foundational distinction underlies the entire container: Spring separates *changing the blueprint* from *changing the product*.

- **`BeanFactoryPostProcessor` (BFPP)** operates on **definitions**, before any instantiation. It can rewrite or add `BeanDefinition`s (placeholder resolution, `@Configuration` parsing, scope changes).
- **`BeanPostProcessor` (BPP)** operates on **instances**, wrapping/proxying already-instantiated beans (DI, AOP proxy creation, lifecycle annotation handling).

Conflating the two would force premature instantiation — the classic hazard where a BPP fails to apply to beans created by a BFPP-instantiated bean. The ordering between them in `refresh()` is the structural enforcement of this separation, and it is *why* AOP and annotation injection require an `ApplicationContext` (which auto-registers both tiers).

### The container-is-assembler mental model, end to end

Configuration metadata — Java `@Configuration`/`@Bean`, component-scanned annotations, XML, Groovy, or programmatic `BeanRegistrar` — is parsed into `BeanDefinition`s; the container then instantiates, configures, and assembles the object graph and injects collaborators. The deliberate design goal is that your classes contain no Spring lookup code, so the same objects are usable and unit-testable outside the container entirely.

### **Changed in v4/v7:** the additive and housekeeping deltas

The container core is largely stable in Framework 7 / Boot 4. The notable changes:

- **First-class programmatic registration via `BeanRegistrar`.** The new `BeanRegistrar` functional interface — `register(BeanRegistry registry, Environment env)` — is imported via `@Import` on a `@Configuration` class and typically guarded by `@Conditional`/`@ConditionalOnProperty`. `BeanRegistry.registerBean(...)` takes a customization spec (`prototype()`, `lazyInit()`, `description()`, `supplier(ctx -> ...)`, and `ctx.bean(Type.class)` to pull collaborators). Kotlin gets `BeanRegistrarDsl`. The design intent: imperative/conditional registration (loops, env-driven branching) is awkward in declarative `@Bean` methods and clunky via the raw `BeanDefinitionRegistry`, so `BeanRegistrar` is the recommended, **AOT/native-compatible** replacement for `BeanDefinitionRegistryPostProcessor` / `ImportBeanDefinitionRegistrar` for dynamic registration.

  ```java
  class MyRegistrar implements BeanRegistrar {
      public void register(BeanRegistry registry, Environment env) {
          if (env.containsProperty("feature.x")) {
              registry.registerBean(FeatureX.class, spec -> spec.lazyInit());
          }
      }
  }
  // @Import(MyRegistrar.class) on a @Configuration class
  ```

- **Consistent global proxy defaulting across all proxy processors.** As of Framework 7.0, the *global* proxy-type default (whatever it is in a given setup) is now **consistently applied to all proxy processors, including `@Async`/`@EnableAsync`** — previously some processors independently chose JDK proxies regardless of the global setting. Per-bean opt-out is available through the new `@Proxyable` annotation: `@Proxyable(INTERFACES)` forces JDK-interface proxying against a CGLIB/Spring-Boot-style default, and `@Proxyable(TARGET_CLASS)` forces class-based proxying against a JDK default. Note that CGLIB is **not** the universal default in the core Spring Framework — the official reference is explicit that "the core framework suggests interface-based proxies (JDK) by default," and it is Spring Boot that, depending on configuration properties, may enable class-based (CGLIB) proxies by default. The v7 change is about *consistency of the defaulting mechanism*, not a flip of the core default to CGLIB.

- **JSpecify nullability.** Nullability declarations migrated from JSR 305 to JSpecify annotations (`org.jspecify.annotations.@Nullable`), which allows nullness on generic types, arrays, and vararg elements. `MethodParameter#isOptional` now checks local (not inherited) annotations. This can require Kotlin code changes.

- **Jakarta-only; legacy removals.** Support for the legacy `javax.annotation` and `javax.inject` packages is removed entirely — you must use `jakarta.annotation` / `jakarta.inject` (`@PostConstruct`/`@PreDestroy`/`@Resource`/`@Inject`). The `spring-jcl` bridge module was removed in favor of using Apache Commons Logging directly, and `ListenableFuture` was removed in favor of `CompletableFuture`.

- **Resilience moved into core.** Spring Retry's mechanism now lives in `spring-core` (`org.springframework.core.retry`: `RetryTemplate`, `RetryPolicy`) with `@Retryable` and the new `@ConcurrencyLimit` in `spring-context` — illustrating new container-managed cross-cutting beans.

- **AOT/native syntax.** Runtime-hints syntax changed from regex to glob format, and registering a type hint is now sufficient for reflection (no need to enumerate member categories). GraalVM 25 with "exact reachability metadata" is the baseline. This matters because `GenericApplicationContext`/`BeanRegistrar` are the AOT-friendly registration paths.

- **Baselines.** JDK 17 minimum (JDK 25 recommended LTS), Jakarta Servlet 6.1, and JUnit 6 for the test module.

**Not changed in v4/v7 (verified):** the `BeanFactory` vs `ApplicationContext` split, the `BeanDefinition` metamodel surface, `DefaultListableBeanFactory` as the workhorse engine, and the parent/child hierarchy semantics are all unchanged from Framework 6.

> **Version note.** Spring Framework 7.0 reached GA on **November 13, 2025**, and Spring Boot 4.0.0 GA followed on **November 20, 2025** (not October 2025).

---

### The `SpringApplication` bootstrap pipeline

`SpringApplication` is Spring Boot's **single, opinionated entry point.** The philosophy is convention-over-configuration: one method, `SpringApplication.run(App.class, args)`, replaces hand-wiring an `ApplicationContext`, registering property sources, and calling `refresh()`. The rejected alternative — developers manually choosing/creating the context type and wiring listeners — is verbose and error-prone. The mental model: *tell Boot your primary config class; it assembles a production-grade container the right way.* The whole machine is driven by `spring.factories` SPIs, so the framework wires sensible defaults while leaving every step pluggable (Open/Closed applied to startup).

#### `run()` — the ordered sequence

In `SpringApplication.run(String...)`, the steps are, in order: create a `Startup` timer; enable the shutdown hook; `createBootstrapContext()`; `configureHeadlessProperty()`; `getRunListeners()`; `listeners.starting(bootstrapContext)`; build `DefaultApplicationArguments`; `prepareEnvironment()`; `printBanner()`; `createApplicationContext()`; `context.setApplicationStartup()`; `prepareContext()`; `refreshContext()`; `afterRefresh()` (a no-op hook); `startup.started()`; optional startup-info logging; `listeners.started()`; `callRunners()`; then in a second `try`, if `context.isRunning()`, `listeners.ready()`. Any throw routes to `handleRunFailure()`.

#### Classpath-driven application type deduction

The design principle is **"the classpath is the configuration."** Adding `spring-boot-starter-web` makes it a servlet app; adding webflux makes it reactive; neither makes it a plain application. This avoids a mandatory mode flag and lets dependency choice express intent, while `setWebApplicationType()` / `setApplicationContextFactory()` preserve explicit escape hatches.

**Changed in v4/v7:** the constructor now calls `WebApplicationType.deduce()` (`@since 4.0.1`), replacing the older `deduceFromClasspath()`. The new `deduce()` first loads `WebApplicationType.Deducer` SPI implementations via `SpringFactoriesLoader` — `WebMvcWebApplicationTypeDeducer` (in `spring-boot-webmvc`; needs `jakarta.servlet.Servlet` + `org.springframework.web.servlet.DispatcherServlet` + `ConfigurableWebApplicationContext` → `SERVLET`) and `WebFluxWebApplicationTypeDeducer` (in `spring-boot-webflux`; needs `reactor.core.publisher.Mono` + `org.springframework.web.reactive.DispatcherHandler` → `REACTIVE`). If no deducer matches, the core enum's fallback checks only `jakarta.servlet.Servlet` + `ConfigurableWebApplicationContext`, else `NONE`. The point of this change is **modularization**: core `spring-boot` no longer hard-codes WebFlux/WebMvc knowledge.

#### `ApplicationContext` selection

`createApplicationContext()` delegates to `ApplicationContextFactory.DEFAULT` (`DefaultApplicationContextFactory`), which iterates `ApplicationContextFactory` SPI candidates from `spring.factories`, passing the `WebApplicationType`; the first non-null result wins. `spring-boot-web-server` contributes `ServletWebServerApplicationContextFactory` and `ReactiveWebServerApplicationContextFactory`. If none apply, the default is `AnnotationConfigApplicationContext` — or a plain `GenericApplicationContext` when AOT generated artifacts are in use. The factory also supplies the matching `Environment` type.

#### Environment preparation order

`prepareEnvironment()` runs: `getOrCreateEnvironment()`; `configureEnvironment()` (adds a `CommandLinePropertySource`, activates profiles); `ConfigurationPropertySources.attach()`; fire `listeners.environmentPrepared()` — **this is where `EnvironmentPostProcessor`s run**, e.g. ConfigData/config-import loading; move `ApplicationInfoPropertySource` and `DefaultPropertiesPropertySource` to the end; `bindToSpringApplication()`; convert the environment type if it does not match the deduced type; re-attach `ConfigurationPropertySources`.

**Hidden concept — `bindToSpringApplication`:** during `prepareEnvironment`, `spring.main.*` properties (banner-mode, web-application-type, lazy-initialization, allow-bean-definition-overriding, keep-alive, register-shutdown-hook) are bound back **onto the live `SpringApplication` instance**, so externalized config can override programmatic setters that were called before `run()`.

**Hidden concept — property-source reordering:** Boot deliberately moves `DefaultPropertiesPropertySource` and `ApplicationInfoPropertySource` to the end (lowest precedence), and a `PropertySourceOrderingBeanFactoryPostProcessor` re-applies that ordering at refresh — a subtle but real determinant of property precedence.

#### `SpringApplicationRunListener` and the event sequence

`SpringApplicationRunListener` (loaded via `spring.factories` with a `(SpringApplication, String[])` constructor) defines callbacks: `starting`, `environmentPrepared`, `contextPrepared`, `contextLoaded`, `started`, `ready`, `failed` (the first two take a `ConfigurableBootstrapContext`). The sole default implementation, `EventPublishingRunListener`, translates these into events. The full ordered sequence:

```
ApplicationStartingEvent
→ ApplicationEnvironmentPreparedEvent
→ ApplicationContextInitializedEvent
→ ApplicationPreparedEvent
→ (ContextRefreshedEvent + WebServerInitializedEvent, during refresh)
→ ApplicationStartedEvent
→ AvailabilityChangeEvent(LivenessState.CORRECT)
→ ApplicationReadyEvent
→ AvailabilityChangeEvent(ReadinessState.ACCEPTING_TRAFFIC)
   (ApplicationFailedEvent on failure)
```

The design intent: startup is a **state machine with well-defined observation points**, giving extension authors precise, stable hooks ("run after the environment is ready but before the context exists") and giving Boot a uniform place to layer cross-cutting concerns (logging init, metrics, availability) without scattering callbacks.

**Hidden concept — early events use a standalone multicaster.** The first four events fire *before/around context creation*, through a standalone `SimpleApplicationEventMulticaster` (`initialMulticaster`). It is `EventPublishingRunListener.contextLoaded()` that iterates the `SpringApplication`'s `ApplicationListener`s, makes each `ApplicationContextAware`, adds them to the **live context's** multicaster, and *then* publishes `ApplicationPreparedEvent`. This is precisely why early listeners **cannot be `@Bean`s** — they must be registered via `SpringApplication.addListeners` or `spring.factories`.

**Hidden concept — availability is core, not Actuator.** `LivenessState.CORRECT` (with `ApplicationStartedEvent`) and `ReadinessState.ACCEPTING_TRAFFIC` (with `ApplicationReadyEvent`) are published by the core run listener itself; the probe/availability model is wired in core, not just by Actuator.

#### `ApplicationContextInitializer`

Loaded in the constructor via `getSpringFactoriesInstances(...)`, initializers are applied in `prepareContext()` via `applyInitializers(context)` — **after** `setEnvironment` and `postProcessApplicationContext`, but **before** bean definitions are loaded and **before** `refresh`. This is the hook for programmatic tweaks to the context/bean factory (register a `BeanFactoryPostProcessor`, set active profiles). `contextPrepared()` / `ApplicationContextInitializedEvent` fires immediately after.

#### `prepareContext()`

`setEnvironment`; `postProcessApplicationContext` (bean-name generator, resource loader, conversion service); add the AOT initializer if needed; configure `allowCircularReferences` / `allowBeanDefinitionOverriding` on the bean factory; `applyInitializers`; `listeners.contextPrepared`; `bootstrapContext.close(context)` (closing the bootstrap context, firing `BootstrapContextClosedEvent`); register the `springApplicationArguments` and `springBootBanner` singletons; add `LazyInitializationBeanFactoryPostProcessor` if `spring.main.lazy-initialization=true`; add the KeepAlive listener if configured; add `PropertySourceOrderingBeanFactoryPostProcessor`; load sources (unless AOT); `listeners.contextLoaded` (→ `ApplicationPreparedEvent`).

#### The bootstrap context

`DefaultBootstrapContext` is created first in `run()`; `BootstrapRegistryInitializer` SPI instances initialize it. It is a transient, pre-container registry (`ConfigurableBootstrapContext`/`BootstrapRegistry`) for sharing singletons or lazily-created objects across the bootstrap phase — e.g. an expensive client used by an `EnvironmentPostProcessor` — *before the real `ApplicationContext` exists*. Its registered singletons can be promoted into the context when `bootstrapContext.close(context)` runs. The design rationale: some work must happen before the real container exists and must be shareable/disposable, so it is given a deliberately minimal pre-container registry rather than being allowed to prematurely instantiate the real context.

**Changed in v4/v7:** the bootstrap classes (`BootstrapRegistry`, `BootstrapContext`, `ConfigurableBootstrapContext`, `DefaultBootstrapContext`, `BootstrapRegistryInitializer`) moved from `org.springframework.boot` to **`org.springframework.boot.bootstrap`**, and `EnvironmentPostProcessor` moved from `org.springframework.boot.env` to `org.springframework.boot`.

#### Banner

`printBanner(environment)` runs after environment prep, before context creation. The mode is controlled by `spring.main.banner-mode` (console/log/off) or `setBannerMode(...)`. `SpringApplicationBannerPrinter` resolves `banner.txt`/`spring.banner.location`, supports placeholders (`${application.version}`, `${spring-boot.version}`, `${application.title}`) and Ansi styling, and the printed banner is registered as the `springBootBanner` singleton.

#### Runners: `ApplicationRunner` / `CommandLineRunner`

`callRunners()` executes **after** `refreshContext()` and `listeners.started()`, but **before** `listeners.ready()` and the `ACCEPTING_TRAFFIC` availability change. This precise window — container fully initialized, but app not yet marked ready for traffic — is intentional: it lets startup data loading and migrations complete before readiness probes pass, integrating cleanly with Kubernetes-style readiness gating. `CommandLineRunner.run(String[])` receives raw args; `ApplicationRunner.run(ApplicationArguments)` receives parsed options/non-option args. A runner that throws wraps the exception in `IllegalStateException` and aborts startup.

Both `ApplicationRunner` and `CommandLineRunner` extend a common package-private `Runner` marker interface, and `callRunners()` collects all `Runner` beans and sorts them as a **single ordered stream** by `AnnotationAwareOrderComparator`, so the two kinds can be interleaved by `@Order`. *Note on provenance:* despite sometimes being described as a v4 change, this is **not** new in Boot 4 — the `Runner` marker was introduced in Boot 2.7, the single-ordered-stream behavior has been unchanged through 3.x and 4.x, and the two kinds have been interleaved in one sorted collection since at least Boot 2.6. There was no prior "separate groups" ordering regime to change away from.

#### `FailureAnalyzer` / `FailureAnalysis`

The philosophy: a startup failure should *teach*, not just dump a stack trace, because the common failure modes (port in use, missing bean, bad property binding) are predictable. `handleRunFailure()` calls `handleExitCode`, `listeners.failed`, then `reportFailure()` via the `SpringBootExceptionReporter` SPI. The default reporter, `FailureAnalyzers`, loads `FailureAnalyzer` beans from `spring.factories`, iterates them, and the first that returns a non-null `FailureAnalysis(description, action, cause)` wins; the analysis is rendered by a `FailureAnalysisReporter` (default `LoggingFailureAnalysisReporter`) into a clean diagnostic. `AbstractFailureAnalyzer<T>` matches a specific cause type. The context is then closed and the exception rethrown.

**Hidden concept — `AbandonedRunException`** short-circuits this: `handleRunFailure()` checks for it first and rethrows without analysis or close, used (e.g. by test slicing / `SpringApplicationHook`) to abort a run intentionally.

**Hidden concept — `SpringApplicationHook`:** `getRunListeners()` consults a thread-local hook that can inject an extra run listener for a single run. This is the mechanism behind test-context bootstrapping and `SpringApplication.withHook()`, letting tooling observe or abort runs without permanent registration.

**Hidden concept — `deduceMainApplicationClass` uses `StackWalker`** (with `RETAIN_CLASS_REFERENCE`) to find the `main` frame, used for banner/version/startup logging and AOT initializer naming.

**Hidden concept — the AOT path forks the pipeline:** when `AotDetector.useGeneratedArtifacts()` is true, `createDefaultApplicationContext()` returns a plain `GenericApplicationContext`, sources are **not** loaded at runtime, and a generated `<Main>__ApplicationContextInitializer` is injected first.

---

### `AbstractApplicationContext.refresh()`: the container lifecycle

`refresh()` is a **fixed-order template method** that builds the container in dependency layers. This is a deliberate design choice: making the order fixed (rather than a configurable pipeline) turns container bring-up into a predictable state machine where each step establishes an invariant the next step relies on. The central rule it enforces: **no application bean is instantiated until ALL definition-rewriting (BFPPs) and ALL per-bean interceptors (BPPs) are registered**, so every bean uniformly receives DI, proxying, and lifecycle callbacks. You extend the container by hooking a layer, not by reordering it.

#### The exact ordered sequence (v7.0)

Under a startup/shutdown lock, `refresh()` opens a `StartupStep "spring.context.refresh"` and runs, in strict order:

```
prepareRefresh()
obtainFreshBeanFactory()
prepareBeanFactory(bf)
try:
    postProcessBeanFactory(bf)
    [StartupStep "spring.context.beans.post-process"]
    invokeBeanFactoryPostProcessors(bf)
    registerBeanPostProcessors(bf)
    [end step]
    initMessageSource()
    initApplicationEventMulticaster()
    onRefresh()
    registerListeners()
    finishBeanFactoryInitialization(bf)
    finishRefresh()
```

On a `RuntimeException`/`Error`, the context logs, calls `destroyBeans()` and `cancelRefresh(ex)`, and rethrows; `finally` ends the startup step.

- **`prepareRefresh()`** — sets `startupDate`, flips `closed=false` / `active=true`, calls `initPropertySources()` (subclass hook), validates required properties, and **snapshots `earlyApplicationListeners`** so listener state can be reset on a later refresh/close.
- **`obtainFreshBeanFactory()`** — the subclass creates/refreshes the internal `ConfigurableListableBeanFactory` and loads definitions. `AbstractRefreshableApplicationContext` builds a fresh `DefaultListableBeanFactory` each refresh; `GenericApplicationContext` uses one fixed factory and forbids multiple refreshes. After this step, definitions exist but **no beans are instantiated.**
- **`prepareBeanFactory()`** — configures container-standard characteristics: context `ClassLoader`, a SpEL expression resolver, a `ResourceEditorRegistrar`, the `ApplicationContextAwareProcessor`, resolvable dependencies (`BeanFactory`, `ApplicationContext`, `ResourceLoader`, `ApplicationEventPublisher`), the `LoadTimeWeaverAwareProcessor` hook, and default environment beans (`environment`, `systemProperties`, `systemEnvironment`).
- **`postProcessBeanFactory()`** — empty base hook; web contexts override it to register scope-specific/web-specific `BeanPostProcessor`s and scopes. At this point definitions are loaded but no post-processors have run and **no beans instantiated.**
- **`invokeBeanFactoryPostProcessors()`** — instantiates and runs all BFPPs (the `BeanDefinitionRegistryPostProcessor` sub-type first), ordered `PriorityOrdered` > `Ordered` > rest. This is where `ConfigurationClassPostProcessor` parses `@Configuration`/`@Bean`/`@ComponentScan` and registers derived definitions, and `PropertySourcesPlaceholderConfigurer` resolves `${...}`.
- **`registerBeanPostProcessors()`** — instantiates and registers all BPPs (same ordering), e.g. `AutowiredAnnotationBeanPostProcessor`, `CommonAnnotationBeanPostProcessor`, and AOP's `AnnotationAwareAspectJAutoProxyCreator`. Must run before any application bean is created.
- **`initMessageSource()` / `initApplicationEventMulticaster()`** — locate the `messageSource` / `applicationEventMulticaster` bean or install a `DelegatingMessageSource` / `SimpleApplicationEventMulticaster` default, before listeners and singletons exist.
- **`onRefresh()`** — empty template hook for context-specific work, called before singleton instantiation. Boot's web contexts override it to **create the embedded `WebServer`** so the server object exists before the `SmartLifecycle` that starts it runs in `finishRefresh`.
- **`registerListeners()`** — registers static `ApplicationListener`s and listener beans by name (without instantiating them), then **multicasts any early events** that were buffered before the multicaster existed.
- **`finishBeanFactoryInitialization()`** — calls `prepareSingletonBootstrap()`, installs the `bootstrapExecutor` and a `ConversionService` if present, registers a default embedded value resolver if none exists, invokes any `BeanFactoryInitializer` beans early, **freezes configuration**, then calls `preInstantiateSingletons()` — which eagerly creates ALL non-lazy singletons and finally invokes `SmartInitializingSingleton.afterSingletonsInstantiated()` on all such beans once every singleton exists.
- **`finishRefresh()`** — `resetCommonCaches()`; `clearResourceCaches()`; `initLifecycleProcessor()`; `getLifecycleProcessor().onRefresh()` (auto-starts `SmartLifecycle` beans by ascending phase); then publishes `ContextRefreshedEvent`.

The design intent behind **eager singleton instantiation** is fail-fast: configuration errors surface at boot, the full object graph and all cross-cutting setup (proxies, validation) are built up front, and `refresh()` is explicitly **all-or-nothing** — on failure it destroys already-created singletons so a half-built context never escapes.

#### Hidden concepts in the refresh path

- **`SmartInitializingSingleton.afterSingletonsInstantiated()`** runs at the very end of `preInstantiateSingletons()`, once every non-lazy singleton exists, and crucially **outside the singleton creation lock** — the correct place for expensive post-init work that touches other beans, unlike `@PostConstruct` (which runs inside the lock).
- **`ApplicationContextAwareProcessor`:** `ApplicationContext`/`Environment`/etc. are injected by an *internal `BeanPostProcessor`*, not by the factory directly. This is why those `Aware` interfaces are "ignored for autowiring" and why `Aware` injection only works for beans the context creates — not for objects you `new` up yourself.
- **Early-event buffering:** events fired before the multicaster exists are queued in `earlyApplicationEvents` and flushed by `registerListeners()`, not lost.
- **`cancelRefresh` vs `doClose`:** a failed refresh calls `destroyBeans()` + `cancelRefresh()` (sets `active=false`, resets caches) but does **not** run the full `doClose()` path — no `ContextClosedEvent`, no `lifecycleProcessor.onClose()`. The context is left non-active rather than "closed."

#### Init-callback ordering and the singleton creation lock

For each bean: `Aware` setters (`BeanNameAware` … `ApplicationContextAware`) → `postProcessBeforeInitialization` → `@PostConstruct` → `InitializingBean.afterPropertiesSet()` → custom init-method → `postProcessAfterInitialization` (where AOP proxies are created). `@PostConstruct` and init methods run **inside the container's singleton creation lock**, and a bean is published to others only after `@PostConstruct` returns. Expensive post-init work should therefore use `SmartInitializingSingleton` or `ContextRefreshedEvent`, which run outside that lock after all singletons exist. Spring recommends `@PostConstruct`/`@PreDestroy` (the jakarta JSR-250 standard) over `InitializingBean`/`DisposableBean` precisely to decouple application code from Spring interfaces.

### `Lifecycle` / `SmartLifecycle`: the runtime lifecycle layer

Spring layers a phase-ordered, auto-started, gracefully-stoppable *runtime* lifecycle on top of the static singleton graph. The design rationale: bean **creation** (the DI graph) and runtime **activity** (servers listening, schedulers ticking, consumers polling) are different concerns with different ordering needs. Overloading init/destroy for "start serving traffic" would be wrong; phases give a deterministic, declarative start/stop choreography decoupled from construction order.

- `Lifecycle` = `start()`/`stop()`/`isRunning()`. Plain `Lifecycle` beans are **not auto-started** and are treated as phase 0.
- `SmartLifecycle` extends `Lifecycle` + `Phased` and adds `isAutoStartup()` (default true), `getPhase()` (default `DEFAULT_PHASE`), and `stop(Runnable callback)` for async graceful stop. The `LifecycleProcessor` calls **only** `stop(Runnable)` on `SmartLifecycle` beans, never the no-arg `stop()`.

#### Phase ordering semantics

**Lower phase starts first and stops last; higher phase starts last and stops first.** Plain `Lifecycle` = phase 0. `SmartLifecycle.DEFAULT_PHASE = Integer.MAX_VALUE`, so default `SmartLifecycle` beans start late and stop early — the safe default for components that depend on everything else already running (a network listener should start last and stop first, so it stops accepting work before its collaborators disappear). An explicit `depends-on` always overrides phase. Note the common misconception: `DEFAULT_PHASE` is **`Integer.MAX_VALUE`, not 0.**

#### Graceful shutdown via `stop(Runnable)`

`DefaultLifecycleProcessor` stops phases from highest to lowest; for each phase it invokes `stop(callback)` on every member (via a `LifecycleGroup` with a per-phase `CountDownLatch`) and blocks until all callbacks fire or a per-phase timeout elapses (`timeoutPerShutdownPhase`, **default 10 seconds as of 6.2** — some older reference text still says 30s; the javadoc says 10s). This lets a component (e.g. a web server) drain in-flight work before the next phase down is touched. The "ordered, and potentially concurrent, shutdown of all components having a common shutdown order value" semantics are the *original* behavior of `SmartLifecycle.stop(Runnable)` (present since Spring 3.0) — *not* a v7 change.

#### **Changed in v4/v7:** lifecycle deltas

- **`SmartLifecycle.isPauseable()` is new in Framework 7.0** (default true). Returning false makes a component opt out of pause/restart — it receives `stop()` only on actual close or explicit context-wide stop, not on a pause. Boot 4.0's `WebServerStartStopLifecycle` and `WebServerGracefulShutdownLifecycle` both override it to return false.
- **Context `pause()`/`restart()`** are part of the 7.0 `Lifecycle` surface on `AbstractApplicationContext`. Spring's test framework now pauses idle cached contexts and auto-restarts them on demand (`spring.test.context.cache.pause`).
- **Boot 4.0 web-server phases** are now constants on `WebServerApplicationContext`: `GRACEFUL_SHUTDOWN_PHASE = SmartLifecycle.DEFAULT_PHASE - 1024` and `START_STOP_LIFECYCLE_PHASE = DEFAULT_PHASE - 2048`. `WebServerGracefulShutdownLifecycle.SMART_LIFECYCLE_PHASE` is deprecated in 4.0.0 in favor of the former. Net effect: the web server starts first / stops last among the high-phase group, graceful drain runs just before the server stops, and all of this happens **after** ordinary application `SmartLifecycle` beans (at `MAX_VALUE`) have stopped — so app components quiesce, then requests drain, then the socket closes. Note the corollary to the misconception above: because `DEFAULT_PHASE` is `MAX_VALUE`, these phases are `MAX-1024`/`MAX-2048` (still high positive numbers), *not* near zero.
- **Project CRaC** checkpoint/restore integration (since 6.1, present in 7.0) builds directly on the `Lifecycle` phase machinery via `spring.context.checkpoint`/`spring.context.exit`.
- **Background bean bootstrap** (introduced 6.2, present in 7.0): `finishBeanFactoryInitialization()` installs a `bootstrapExecutor` bean, and `@Bean(bootstrap = Bean.Bootstrap.BACKGROUND)` initializes a bean on a background thread (completion forced by end of `refresh()` for non-lazy beans). **Without a `bootstrapExecutor` `Executor` bean, `BACKGROUND` markers are ignored.**

> **Provenance note.** The `ReentrantLock`-guarded `startupShutdownLock` (replacing the `synchronized startupShutdownMonitor`), the recorded `startupShutdownThread`, and `registerShutdownHook`'s `isStartupShutdownThreadStuck()` (which interrupts/abandons a thread stuck `WAITING`, e.g. from a user `System.exit` during startup, instead of deadlocking the hook) are all **real and present in v7 — but they were introduced in Framework 6.1 (Boot 3.2), not in v4/v7.** By Framework 7 this is inherited behavior.

### `ApplicationStartup` / `StartupStep` instrumentation

`ApplicationStartup.start(name)` opens a `StartupStep` with an `id` and `parentId` forming a tree; `tag(k, v)` attaches metadata; `end()` records duration. The design choice is **ship a no-op by default, opt into recording**: `ApplicationStartup.DEFAULT` records nothing (minimal overhead), while `BufferingApplicationStartup`/`FlightRecorderApplicationStartup` record to JFR for diagnosing slow boots. The `.`-namespaced, reusable step names give a stable, tool-friendly taxonomy of container phases. `refresh()` emits `spring.context.refresh` (root) and `spring.context.beans.post-process`; the bean factory emits `spring.beans.instantiate`, `spring.beans.smart-initialize`, and BFPP-invocation steps.

### `close()` / `doClose()` and destruction ordering

`doClose()` runs only on a successful CAS (active && `closed` false→true) and, in order: publishes `ContextClosedEvent`; calls `lifecycleProcessor.onClose()` (**stop all `Lifecycle` beans, reverse-phase, with graceful timeout — *before* destruction**); `destroyBeans()` → `beanFactory.destroySingletons()`; `closeBeanFactory()`; the `onClose()` subclass hook; `resetCommonCaches()`; reset listeners and null out the multicaster/`messageSource`.

The intent of **stopping `Lifecycle` beans before destroying singletons** is to quiesce runtime activity first (close listeners, stop schedulers, drain requests) so in-flight work never touches a bean that is about to be destroyed.

**Singleton destruction order** is the mirror of creation: singletons are destroyed in **reverse registration order while honoring dependencies** — a bean is destroyed before the beans it depends on, and dependents are destroyed before their dependencies. Per-bean destruction runs `@PreDestroy` methods, then `DisposableBean.destroy()`, then the configured/inferred destroy-method.

#### Hidden destruction concepts

- **Destroy-method inference:** `@Bean` methods and `AutoCloseable`/`Closeable` beans auto-register a public `close()`/`shutdown()` as the destroy method by default. A bean can be silently torn down by a `close()` you did not intend as a lifecycle hook.
- **Stop is not guaranteed before destroy on hot/cancelled refresh:** on a normal close, every `Lifecycle` bean gets `stop()` before destroy callbacks; but on a hot refresh or a cancelled refresh attempt, **only destroy methods run** — `Lifecycle.stop()` may be skipped. Drain logic placed only in `stop()` can be surprisingly bypassed.
- **`@Lazy` barely affects `SmartLifecycle` beans:** auto-startup forces them to initialize at refresh regardless, so marking them `@Lazy` rarely defers anything.
- **Per-phase shutdown timeout, not global:** `timeoutPerShutdownPhase` applies independently to each phase group, tunable via `setTimeoutForShutdownPhase`.


---

## Components, Stereotypes & Scanning

Component scanning and the stereotype family are where Spring's "convention over configuration" philosophy meets its annotation-composition machinery. The whole subsystem is built on a single, deliberately minimal idea — *a bean is anything meta-annotated with `@Component`* — and everything else (role-specific stereotypes, the candidate index, name/scope/proxy hooks, even Boot's `@SpringBootApplication`) is layered on top as composition rather than special-casing. This section dissects the mechanics and, more importantly, the design intent behind each layer.

### `@Component` and the Stereotype Family: Noun vs Adjective

The four core stereotypes — `@Component`, `@Controller`, `@Service`, `@Repository` — all live in `org.springframework.stereotype` (spring-context). `@Controller`, `@Service`, and `@Repository` are each themselves annotated with `@Component`:

```java
@Target(ElementType.TYPE)
@Retention(RetentionPolicy.RUNTIME)
@Documented
@Component                       // <-- the marker that makes scanning notice it
public @interface Service {
    @AliasFor(annotation = Component.class)
    String value() default "";   // sets the bean name
}
```

Because the component scanner's default include filter matches *any type meta-annotated with `@Component`* (resolved through `AnnotatedElementUtils` / the `MergedAnnotations` API), `@Service` and `@Controller` are at the container level **functionally identical to `@Component`**. They register no extra `BeanPostProcessor`, trigger no extra proxying, and change nothing about instantiation. The `value()` attribute is an `@AliasFor(Component.value())`, so the bean-name contract is uniform across all four.

**Design intent.** Spring deliberately separates *semantics* from *behavior*. A stereotype is documentation-as-code: `@Component` is the noun ("this is a managed bean"); the specialization is an adjective describing its architectural role ("...acting as a service-layer facade"). The reference docs are explicit that `@Service` "is clearly the better choice" for a service-layer bean — not because it does more, but because it communicates intent and is a clean pointcut target for AOP and tooling. The docs also reserve the right that stereotypes "may carry additional semantics in future releases," which is the real payoff: because the annotation is already in place, Spring can attach behavior later *without a breaking migration*. The rejected alternative — hard-coding `@Service`/`@Controller`/`@Repository` as distinct, special-cased mechanisms — would have coupled architecture documentation to container mechanics and closed the door to user-defined stereotypes. Instead, anyone can mint their own composed stereotype (`@OrderProcessor`, `@DomainService`) meta-annotated with `@Component`, and it "just works" — discovery, naming, and indexing all follow transitively.

#### Hidden concept: bean-name generation is uniform across stereotypes

`AnnotationBeanNameGenerator` derives the default name (decapitalized simple class name, e.g. `MovieFinderImpl` → `movieFinderImpl`) and honors the explicit `value()` identically for all four stereotypes, precisely *because* each `value()` is wired via `@AliasFor(Component.value())`. The stereotype you pick never changes naming.

**Changed in v4/v7:** The decapitalization logic itself moved from `Introspector.decapitalize` to `StringUtils.uncapitalizeAsProperty` — but note this landed in **Spring Framework 6.0** (Boot 3.0), *not* v7. By 7.0 it is long-standing behavior; do not attribute it to the v7 cycle.

### `@Controller`: The One Web-Flavored Stereotype in Core

`@Controller` is unusual: it ships in the core `org.springframework.stereotype` package (since 2.5), yet its only meaningful behavior is realized by the web modules. `RequestMappingHandlerMapping` in spring-webmvc / spring-webflux keys handler detection off `@Controller`; the core container still treats a `@Controller` bean as a plain `@Component`.

This is the cleanest illustration of the semantics/behavior split: the *marker* lives in core (so the container can scan it like any other component), while the *interpretation* lives in whichever module cares.

**Changed in v4/v7 (clarification, not a change):** The prompt-hypothesis that the core stereotypes "moved modules" in v7 is **not borne out** — `@Component`, `@Controller`, `@Service`, `@Repository` all remain in `org.springframework.stereotype` (spring-context) as of Spring Framework 7.0.x. The genuinely separate annotations are `@RestController` and `@ResponseBody`, which live in `org.springframework.web.bind.annotation` (spring-web), where `@RestController = @Controller + @ResponseBody`. That module boundary is **long-standing (since Spring 4.0), not new in v7**.

### `@Repository`: The One Stereotype With Real Mechanics

`@Repository` (since 2.0) is the exception that proves the rule: it is the only core stereotype that buys you runtime behavior. Its javadoc states a class so annotated "is eligible for Spring `DataAccessException` translation when used in conjunction with a `PersistenceExceptionTranslationPostProcessor`."

The mechanics:

- **`PersistenceExceptionTranslationPostProcessor`** is a `BeanPostProcessor` (extends `AbstractBeanFactoryAwareAdvisingPostProcessor`, since 2.0). It detects beans whose *class* carries `@Repository` and adds a `PersistenceExceptionTranslationAdvisor` to the bean's exposed proxy — either an existing AOP proxy or a newly generated one implementing the target's interfaces.
- The advisor delegates to **`PersistenceExceptionTranslator`** beans, which it autodetects from the context (from *any* scope, sorted by `Ordered`/`@Order`). These translators — `HibernateExceptionTranslator`, the JPA `EntityManagerFactory` (which itself implements the interface), the `SQLErrorCodeSQLExceptionTranslator` path — convert native exceptions (`HibernateException`, JPA `PersistenceException`, raw `SQLException`, `IllegalArgumentException`/`IllegalStateException`) into Spring's unchecked **`DataAccessException`** hierarchy (`DataIntegrityViolationException`, `OptimisticLockingFailureException`, `DuplicateKeyException`, ...).

**Design intent.** The persistence boundary is exactly where leaky, vendor-specific exceptions cross into business code. Spring's goal is a consistent, technology-agnostic exception hierarchy so the service layer can `catch (DuplicateKeyException e)` without importing Hibernate or JPA types. `@Repository` is the *natural pointcut*: it marks precisely the beans at that boundary, so the post-processor advises only them rather than every bean. The translation is woven via an **AOP advisor/proxy** rather than inheritance or a template — deliberately non-invasive, so the DAO stays a plain POJO using the native persistence API. This mirrors Spring's broader "declarative cross-cutting concern via proxy" model (the same machinery family as `@Transactional`), and it *composes*: the same proxy can carry both transaction advice and translation advice. The rejected alternatives — forcing DAOs to extend a Spring base class, or to translate exceptions by hand — would have made the DAO aware of the framework.

#### Spring Boot auto-registration

In plain Spring you must declare the post-processor yourself (`@Bean` or XML); the magic is opt-in. Spring Boot registers it for you via `PersistenceExceptionTranslationAutoConfiguration`, gated by `@ConditionalOnMissingBean` and a property (default on). So `@Repository` translation works out of the box in Boot.

**Changed in v4/v7:** In Boot 4 the controlling property was **renamed** from `spring.dao.exceptiontranslation.enabled` to **`spring.persistence.exceptiontranslation.enabled`** (still default `true`, `matchIfMissing`), and the auto-configuration **moved to a new `spring-boot-persistence` module**, package `org.springframework.boot.persistence.autoconfigure`. The old `spring.dao.*` property is no longer supported. The underlying `PersistenceExceptionTranslationPostProcessor` class and the `@Repository`→`DataAccessException` pipeline are otherwise unchanged from Boot 3.

#### Hidden concepts around exception translation

- **`@Repository` must be on the concrete class, not only an interface.** The post-processor advises beans whose *class* (or a meta-annotation thereof) carries `@Repository`. Annotating only a base interface a Spring Data repository extends is a classic "why isn't translation happening?" trap — though Spring Data registers its *own* translation independently of this post-processor, so the two paths should not be conflated.
- **It can create a proxy you didn't ask for.** A `@Repository` bean with no other advice still gets wrapped (JDK dynamic proxy, or CGLIB if `proxyTargetClass=true`/no interfaces). This can surprise code relying on the concrete type or interact with other proxying.
- **`repositoryAnnotationType` is pluggable.** You can point the post-processor at your own annotation (e.g. a non-Spring `@Dao`), decoupling translation from the Spring stereotype entirely.
- **Translators come from any scope and are ordered.** Not just singletons; sorted via `Ordered`/`@Order`. In JPA, the `EntityManagerFactory` *is* a translator, which is why translation works without you declaring an explicit translator bean.

### Meta-Annotations and Composed Annotations: The Engine Underneath

Everything above rests on Spring's treatment of annotations as a **composable, user-extensible vocabulary**. An annotation placed on another annotation's declaration is "meta-present"; this is transitive, so `@RestController` (which is `@Controller`, which is `@Component`) carries `@Component` at meta-distance 2. The framework hard-codes only a few base concepts (`@Component`, `@Configuration`, `@Scope`, `@Transactional`, `@RequestMapping`); everything else is composition.

#### `@AliasFor`: declarative attribute forwarding

Java annotations have no inheritance and no way to forward attributes. Rather than fork the language, Spring layers `@AliasFor` on top in two modes:

- **Explicit aliases within one annotation** — two attributes become interchangeable mirrors (`@RequestMapping` `value`/`path`). Constraints: same return type, both declare a default, and the **defaults must be identical** (not merely "both have a default"); `annotation()` must be omitted.
- **Attribute override into a meta-annotation** — when `annotation()` names a (meta-present) annotation, the attribute punches its value down into that meta-annotation's attribute. This is how `@SpringBootApplication.scanBasePackages` writes into `@ComponentScan.basePackages`.
- **Implicit (transitive) aliases** — if several attributes all override the *same* meta-annotation attribute (directly or through intermediate overrides), they form an implicit alias set; setting any one sets them all, and values flow down the whole chain.

#### The `MergedAnnotations` API and synthesized annotations

The hierarchy is read through `MergedAnnotations`, which flattens an element's annotation tree into distance-ordered `MergedAnnotation` views. `getDistance()` is 0 for directly-present, >0 for meta-distance; results are ordered by `getAggregateIndex()` then distance, so the *nearest/most specific* annotation wins (this is why a directly-present `@Scope` beats one inherited via a composed annotation). `SearchStrategy` controls hierarchy traversal: `DIRECT`, `INHERITED_ANNOTATIONS`, `SUPERCLASS`, `TYPE_HIERARCHY`.

When an annotation is read this way, Spring returns a **synthesized annotation** — a `java.lang.reflect.Proxy` implementing the annotation interface (plus the `SynthesizedAnnotation` marker) whose invocation handler enforces `@AliasFor` (reading `value()` returns the merged/aliased value). The crucial caveat, from the `@AliasFor` javadoc: *"the mere presence of `@AliasFor` on its own will not enforce alias semantics. For alias semantics to be enforced, annotations must be loaded via `MergedAnnotations`."* Plain `getAnnotation()` reflection ignores `@AliasFor` entirely.

**Design intent.** Centralizing enforcement in one merge algorithm avoids subtle divergence between code paths; the deliberate price is that you must read through Spring's API, not raw reflection. Misconfigured `@AliasFor` (missing reciprocal, mismatched default, wrong target) surfaces as an `AnnotationConfigurationException` **at merge time — runtime, not compile time** — a trap for authors who test composed annotations only with reflection.

#### `get`/`present` vs `search`/`find` semantics (hidden concept)

Two distinct algorithms. "Present" semantics (`MergedAnnotations.from` default, `getMergedAnnotation`) consider only the element plus its meta-annotations. "Find" semantics (`findMergedAnnotation`, `search(TYPE_HIERARCHY)`) additionally walk superclasses and interfaces. Choosing the wrong one is the classic "my annotation on the superclass isn't picked up" bug.

#### `AnnotationMetadata` / `AnnotatedTypeMetadata`: reading without loading

`AnnotatedTypeMetadata.getAnnotations()` returns a `MergedAnnotations`; `AnnotationMetadata` (since 2.5) extends it with class-level access "in a form that does not require that class to be loaded yet." Two implementations: `StandardAnnotationMetadata` (reflection, class already loaded) and the bytecode-reading `SimpleAnnotationMetadata` used by scanning. This is what `@Conditional` conditions and `ImportSelector`s receive, so a condition can decide *not* to load a class — essential for `@ConditionalOnClass` against optional dependencies.

**Changed in v4/v7:** Many older `AnnotationUtils` discovery/traversal methods (`getAnnotations`, `getRepeatableAnnotations`, `isAnnotationMetaPresent`, `findAnnotationDeclaringClass`, ...) are deprecated as "superseded by the `MergedAnnotations` API," while core `findAnnotation`/`getAnnotation`/`synthesizeAnnotation` remain. **Note:** these methods carry `@Deprecated(since = "5.2")` — they were deprecated when `MergedAnnotations` was introduced in **5.2**, not in v7. v7 merely continues that long-standing direction; the v7 release notes do not introduce these deprecations.

**Changed in v4/v7 (custom stereotype names):** A custom `@Component`-composed stereotype may expose its bean-name attribute under a name other than `value` (e.g. `name()`) via `@AliasFor(annotation = Component.class, attribute = "value")`, and `AnnotationBeanNameGenerator` honors it; convention-based stereotype names are deprecated. **Note:** this landed in **Spring Framework 6.1**, not 7.0 — it is a pre-v7 carryover, stable through v7.

### Classpath Scanning Mechanics

`@ComponentScan` configures, and `ClassPathScanningCandidateComponentProvider` performs, the actual discovery. The pipeline:

1. **Base package resolution.** `value`/`basePackages` accept Ant-style patterns and `${...}` placeholders; `basePackageClasses` is the type-safe, refactor-safe alternative (the *package* of each listed class is scanned). **If none are specified, the package of the annotated class is the default base package** — which is exactly why `@SpringBootApplication` (composing `@ComponentScan` with no base package) belongs at the root of your package tree: discovery radiates downward from it.
2. **Resource enumeration.** The base package is translated to `classpath*:` + package + `/` + `resourcePattern` (default `**/*.class`) and resolved via a `ResourcePatternResolver`.
3. **Metadata reading.** A `MetadataReader` is obtained per `.class` file, exposing `ClassMetadata` + `AnnotationMetadata` **read from bytecode, without loading the class**.
4. **Filtering.** `isCandidateComponent` = matches no exclude filter AND matches ≥1 include filter. A second gate requires the class to be *concrete and independent* (not an interface/abstract, not a non-static inner class) **unless** it declares `@Lookup` methods.
5. **Registration.** `ClassPathBeanDefinitionScanner` (a subclass) sets scope, generates the name, applies common annotation defaults (`@Lazy`/`@Primary`/`@DependsOn`/`@Role`/`@Description` via `AnnotationConfigUtils.processCommonDefinitionAnnotations`), optionally wraps in a scoped proxy, and registers a `BeanDefinitionHolder`.

#### Filters: exclude-then-include, with `@Component` as the sole default

`addExcludeFilter` prepends; `addIncludeFilter` appends. **Excludes are evaluated first and short-circuit.** Mental model: includes define the universe of interest, excludes carve out exceptions, exceptions win. With `useDefaultFilters=true`, the only registered include filter is an `AnnotationTypeFilter` for `@Component` (plus JSR-330 `@Named` / Jakarta `@ManagedBean` when present) — and because every stereotype is meta-annotated with `@Component`, all are detected transitively without enumeration. `FilterType` values: `ANNOTATION` (default), `ASSIGNABLE_TYPE`, `ASPECTJ`, `REGEX`, `CUSTOM` (the escape hatch with full `MetadataReader` access for metadata-driven decisions without loading classes).

**Design intent — read bytecode, don't load classes.** Premature class loading runs static initializers, forces transitive loading of referenced types (which may be absent at runtime), and permanently bloats the ClassLoader with classes that will be filtered out anyway. Reading just the constant pool, annotations, and hierarchy keeps scanning cheap and side-effect-free, and lets Spring inspect classes whose dependencies aren't even present. The mental model: *discovery is a metadata query over the classpath, not an execution step.*

**Changed in v4/v7 — the single most relevant scanning-internals change.** Bytecode metadata reading no longer requires Spring's bundled ASM fork on modern runtimes. Framework 7.0 introduces **`ClassFileMetadataReader`** (in spring-core), built on **Java 24's standard Class-File API (JEP 484)**, selected **automatically on Java 24+** and fully transparent to applications; the ASM fork (`org.springframework.asm`) is retained as a fallback for older runtimes and for CGLIB. The motivation: maintaining a forked ASM to track each new class-file version was perennial maintenance debt and a source of "unsupported class file version" failures; the JDK now owns that burden. (The Class-File path activates only on Java 24+; a Framework-7 deployment on Java 17–23 still uses the legacy reader.)

#### Naming, scope, and proxy hooks

`@ComponentScan` carries these as `Class<?>` references (not instances) because it is parsed from annotation metadata *before any beans exist*; the scanner instantiates them via a required no-arg constructor.

- **`nameGenerator`** — default is the `BeanNameGenerator.class` sentinel meaning "inherit the context's generator" (normally `AnnotationBeanNameGenerator`). `FullyQualifiedAnnotationBeanNameGenerator` uses the FQN to avoid cross-package collisions.
- **`scopeResolver`** — default `AnnotationScopeMetadataResolver`, reading `@Scope` **on the concrete class/factory method only — there is no bean-definition inheritance** of scope from a superclass or interface.
- **`scopedProxy`** — default `ScopedProxyMode.DEFAULT` (a sentinel meaning "defer to scanner default," effectively `NO`), distinct from explicitly choosing `NO`. Chooses `NO`/`INTERFACES` (JDK)/`TARGET_CLASS` (CGLIB) so a singleton can hold a stable handle to a shorter-lived request/session/prototype bean.

The sentinel-default pattern lets a single `@ComponentScan` opt into customization while otherwise inheriting the context-wide strategy — composition without forcing every annotation to repeat the global choice.

#### Lazy-init: eager singletons by default

`@ComponentScan.lazyInit` defaults to **`false`** — scanned singletons are eagerly instantiated at context refresh. This is **fail-fast philosophy**: wiring errors blow up deterministically at startup, not on the first request in production. Lazy init trades startup time for deferred, less predictable failure, so it is opt-in. For genuine on-demand dependencies the docs prefer `ObjectProvider<T>` over `@Lazy`, because it is explicit at the injection point rather than hiding a proxy.

#### Hidden scanning concepts

- **The second candidate gate (`@Lookup`).** An abstract `@Component` is normally skipped — *unless* it declares `@Lookup` methods, in which case Spring subclasses it to implement lookup-method injection. This surprises people expecting abstract components to be ignored unconditionally.
- **`@Scope` is not inherited.** Only the concrete bean class / `@Bean` method is introspected; `@Scope` on a superclass or interface is deliberately ignored.
- **JPMS requires `opens`/`exports`.** Under the module system, scanned packages must be exported and (for reflective injection into non-public members) opened, or discovery/injection fails.
- **`resourcePattern` is tunable.** The default `**/*.class` can be narrowed (or carefully widened) to limit which classpath resources are even considered before metadata filtering — a rarely-used lever for very large module trees.
- **`@Configuration` classes are scanned candidates.** Because `@Configuration` is meta-annotated `@Component`, configuration classes are discovered by scanning and CGLIB-enhanced (full mode). `@Bean` methods inside a plain `@Component` are *not* enhanced (lite mode), so inter-bean method calls there are plain Java calls, not container-routed.

### The Candidate Component Index, and Why AOT Superseded It

`@Indexed` (since 5.0) marks a type for the compile-time candidate index. **`@Component` is itself meta-annotated with `@Indexed`**, which is the actual — and frequently misunderstood — reason *all* stereotypes (and `@Configuration`, and any user-defined `@Component`-composed annotation) are indexable: the indexer follows `@Indexed` transitively through `@Component`, it does *not* special-case each stereotype.

```java
@Indexed                          // on the @Component declaration itself
public @interface Component { ... }
```

The mechanics: the `spring-context-indexer` annotation processor scans for `@Indexed`-annotated types at build time and emits `META-INF/spring.components`. At runtime, `CandidateComponentsIndexLoader` loads it and the provider queries `CandidateComponentsIndex` instead of enumerating the classpath — a startup optimization for large apps, auto-enabled merely by the file's presence.

**Two critical, lesser-known limitations** drove the design away from it:

- **It is silently ignored for non-`@Indexed` filters.** The index is consulted only when *all* include filters are `AnnotationTypeFilter`/`AssignableTypeFilter` on `@Indexed` targets. Add any `REGEX`, `ASPECTJ`, or custom include filter and the provider abandons the index entirely and falls back to full scanning — even though `spring.components` exists.
- **All-or-nothing failure mode.** Once *any* `META-INF/spring.components` is on the classpath, *every* jar must have been processed by the indexer; otherwise beans in unprocessed jars become invisible **with no error**. This footgun — not raw performance — is the headline reason for deprecation.

**Changed in v4/v7 (precise framing):** It is the **build-time processor `CandidateComponentsIndexer`** (in the `spring-context-indexer` module) that is `@Deprecated(since = "6.1", forRemoval = true)`, "in favor of the AOT engine and the forthcoming support for an AOT-generated Spring components index" (issue #30431, deprecated in **6.1**, carried into v7). The runtime classes are **not** deprecated and were in fact *extended* in 7.0: `@Component` still carries `@Indexed`, `CandidateComponentsIndexLoader` still consumes `META-INF/spring.components`, and 7.0 adds new `@since 7.0` programmatic APIs (`CandidateComponentsIndexLoader.addIndex`/`clearCache`; `CandidateComponentsIndex.registerScan`/`registerCandidateType`/no-arg constructor) precisely to support **AOT-generated** indexes rather than only the file-based path. So: file-based loading still works, the annotation-processor path is deprecated-for-removal, and the runtime API has been augmented for AOT.

**Design intent and the lesson.** The index solved "runtime scanning is slow for huge apps" by precomputing candidates at build time — the right instinct. But its on/off, whole-classpath-or-nothing contract made it brittle and operationally surprising. **AOT** generalizes the same insight holistically — doing expensive analysis once at build time across the *entire* context (bean definitions, proxies including the persistence-translation proxy, reflection hints, GraalVM native images) — which makes a scanning-only index redundant. The philosophy Spring is now teaching: build-time processing is the right home for startup-cost optimization, and it should be holistic, not a point fix.

### `@SpringBootApplication`: The Canonical Composed Annotation

Everything in this section converges in Boot's entry annotation, which is one annotation standing in for three stereotypes plus a curated façade over their attributes:

```java
@SpringBootConfiguration                 // = @Configuration + @Indexed
@EnableAutoConfiguration                 // = @AutoConfigurationPackage + @Import(AutoConfigurationImportSelector)
@ComponentScan(excludeFilters = {
    @Filter(type = CUSTOM, classes = TypeExcludeFilter.class),
    @Filter(type = CUSTOM, classes = AutoConfigurationExcludeFilter.class) })
@Inherited
public @interface SpringBootApplication {
    @AliasFor(annotation = Configuration.class, attribute = "proxyBeanMethods")
    boolean proxyBeanMethods() default true;
    @AliasFor(annotation = EnableAutoConfiguration.class) Class<?>[] exclude() default {};
    @AliasFor(annotation = ComponentScan.class, attribute = "basePackages")
    String[] scanBasePackages() default {};
    @AliasFor(annotation = ComponentScan.class, attribute = "nameGenerator")
    Class<? extends BeanNameGenerator> nameGenerator() default BeanNameGenerator.class;
    // ...
}
```

`@AliasFor` re-exports only the handful of attributes people actually tune (`proxyBeanMethods` → `@Configuration`; `exclude`/`excludeName` → `@EnableAutoConfiguration`; `scanBasePackages`/`scanBasePackageClasses`/`nameGenerator` → `@ComponentScan`). `@EnableAutoConfiguration` is `@AutoConfigurationPackage` + `@Import(AutoConfigurationImportSelector.class)`; the selector is a `DeferredImportSelector` (runs *after* user configuration), and candidates are located via `ImportCandidates` reading `META-INF/spring/org.springframework.boot.autoconfigure.AutoConfiguration.imports`. Auto-config classes are ordinary `@Configuration` beans, almost always `@Conditional`, so they "back away" as the user defines more.

**Design intent.** Beginners get one annotation; the composition keeps the three real concerns — configuration source, auto-config, scanning — independently meaningful and individually usable. The `DeferredImportSelector` + `.imports` file design makes auto-config a late, conditional, ordered step that yields to user beans: *opinionated but overridable.*

**Changed in v4/v7 (modularization, precisely):** Boot 4 split the formerly monolithic `spring-boot-autoconfigure` jar into many technology-specific modules and starters, with those modules' base packages now under `org.springframework.boot.<module>` (e.g. `org.springframework.boot.webmvc`). **However**, `@SpringBootApplication` and `@EnableAutoConfiguration` did **not** change package — both remain at `org.springframework.boot.autoconfigure.*`, residing in the single `core/spring-boot-autoconfigure` module (they are **not** split across `core/spring-boot`). What moved was the Gradle build directory of that module (into `core/`); the Java FQNs are stable from Boot 3.x.

### Cross-Cutting v4/v7 Baseline and Proxy Notes

- **Baselines.** Spring Framework 7.0 GA'd **2025-11-13**; Spring Boot 4.0.0 GA'd **2025-11-20** (built on Framework 7). Baseline is **JDK 17 minimum** (JDK 25 LTS recommended), **Jakarta EE 11** (Servlet 6.1, JPA 3.2, Bean Validation 3.1). Annotations in `javax.annotation`/`javax.inject` are no longer supported — use `jakarta.annotation`/`jakarta.inject` (so JSR-330 `@Named`/`@Inject` alongside stereotypes must be the Jakarta variants). Spring's JSR-305-flavored nullness annotations are deprecated in favor of **JSpecify** (a metadata/tooling change, not a behavioral change to scanning or translation).
- **Proxy defaulting.** **Changed in v4/v7:** the *global* proxy-type default — whatever it is in a given setup — is now **consistently applied to all proxy processors**, including `@Async`/`@EnableAsync`, which previously chose JDK proxies independently of the global setting. The new per-bean opt-out is `@Proxyable` (`@Proxyable(INTERFACES)` against a CGLIB default, `@Proxyable(TARGET_CLASS)` against a JDK default). Crucially, **CGLIB is *not* the default in the core framework** — the core framework still suggests JDK interface-based proxies by default; it is *Spring Boot* that, depending on configuration, enables class-based (CGLIB) proxies by default. The v7 change is about the *consistency of the defaulting mechanism*, not a flip of the core default to CGLIB.

### `@Role` / `BeanDefinition` Role Hints: Metadata, Not Enforcement

Orthogonal to stereotypes, `@Role` (in `org.springframework.context.annotation`) sets `BeanDefinition.getRole()`: `ROLE_APPLICATION` (0, default — a major user-defined bean), `ROLE_SUPPORT` (1 — a supporting part of a larger configuration), `ROLE_INFRASTRUCTURE` (2 — an internal background bean of no relevance to end users).

**Design intent.** This is purely informational classification consumed by tooling and diagnostics — it lets infrastructure beans be filtered out of overviews — and it changes no bean behavior. Spring keeps it **deliberately orthogonal** to stereotypes so the two concerns (*what kind of component* vs *how prominent in tooling*) don't entangle: documentation-as-metadata, with the container neither enforcing nor hiding anything functionally.

**Hidden concept:** `@Role` **does not cascade** from a `@Configuration` class to its `@Bean` methods. Placing `@Role(ROLE_INFRASTRUCTURE)` on a config class marks only the config bean itself; each `@Bean` method needs its own `@Role`. A frequent misunderstanding when trying to hide framework plumbing from tooling.


---

## Beans: Wiring, Naming & Lifecycle

A Spring `ApplicationContext` is, at its core, a machine that turns *configuration metadata* (bean definitions) into a wired graph of *bean instances* and then drives those instances through a deterministic lifecycle. Three questions sit at the heart of that machine: **how is a bean identified** (naming), **how does the container decide which bean satisfies a given dependency** (wiring), and **what happens to a bean from instantiation to destruction** (lifecycle). The unifying philosophy across all three is *fail-fast determinism over silent guessing*: the container would rather refuse to start, loudly, at boot than make a plausible-but-wrong choice that surfaces as a baffling runtime bug far from its cause.

### Bean names: the decapitalized-class-name default

When a component is autodetected by classpath scanning with no explicit name, Spring's `AnnotationBeanNameGenerator` derives the canonical bean name from the **uncapitalized simple (non-qualified) class name**: `MovieFinderImpl` becomes `movieFinderImpl`, and `mypackage.MyJdbcDao` becomes `myJdbcDao`. An explicit value in a stereotype annotation (`@Service("myMovieLister")`, `@Component("x")`, `@Repository`, `@Controller`) or via `@jakarta.inject.Named` overrides that default; for `@Bean` methods the method name is the default unless `name`/`value` is supplied.

```java
@Service                       // bean name -> "movieFinderImpl"
public class MovieFinderImpl implements MovieFinder { }

@Service("lister")             // bean name -> "lister"
public class DefaultMovieLister { }
```

**Design intent.** This is convention-over-configuration applied to identity. In pure type-driven autowiring the name is irrelevant plumbing — most beans are matched by type and never referenced by name at all — so forcing developers to christen every bean would be pure noise. A *deterministic, predictable* default (matching the standard Java instance-field convention `accountManager`, `userDao`) keeps a usable name available for the rare by-name case while costing nothing in the common case. The mental model: a bean's name is a fallback handle, not its essence.

#### Hidden concept: the acronym edge case

There is one subtle rule that trips up by-name lookups and `@Qualifier` references. If the **first two characters of the simple class name are both uppercase**, casing is preserved instead of lowercasing the first letter. So `URLFooServiceImpl` becomes `URLFooServiceImpl` — *not* `uRLFooServiceImpl`. Developers who assume "first letter is always lowercased" and then write `@Qualifier("uRLFooServiceImpl")` get a silent no-match. The rule exists so that acronym-prefixed names read naturally rather than being mangled.

**A Framework 6.0 change worth knowing.** Bean-name generation uses `StringUtils.uncapitalizeAsProperty` (backed by `PropertyDescriptorUtils.determineBasicProperties`) rather than the legacy `java.beans.Introspector.decapitalize`. The *observable* rule is identical — lowercase the first character unless the first two are uppercase — but the new path avoids the `java.beans.Introspector` machinery, which streamlines startup and eases GraalVM native images (where `Introspector` otherwise needs substitutions). This landed in **Spring Framework 6.0** (Spring Boot 3.0, issue #29320), so by Boot 4 it is inherited, established behavior rather than a v7-specific change.

### Identity, aliases, and per-subsystem namespaces

A bean has **one or more identifiers**, unique within the container; the first is the *canonical name* and the rest are *aliases*. In XML the `id` attribute is exactly one id, the `name` attribute accepts comma/semicolon/whitespace-separated aliases, and `<alias name="fromName" alias="toName"/>` registers an alias for a bean possibly defined elsewhere. `@Bean(name = {"a", "b", "c"})` likewise supports multiple names where the first is canonical.

```xml
<bean id="myApp-dataSource" class="..."/>
<alias name="myApp-dataSource" alias="subsystemA-dataSource"/>
<alias name="myApp-dataSource" alias="subsystemB-dataSource"/>
```

**Design intent.** Aliasing decouples a bean's stable identity from the many context-specific names different subsystems want to call it by. One shared `dataSource` can be referred to as `subsystemA-dataSource` and `subsystemB-dataSource`, giving each module its own naming namespace over a single instance. Large modular configurations can then evolve their local names independently without duplicating the underlying bean — identity is the stable anchor, names are negotiable surface.

### Name collisions: overriding vs. conflict

There are two distinct collision scenarios, handled differently:

- **Bean definition overriding** — registering a *second* definition under an existing name. Raw Spring's `DefaultListableBeanFactory` allows this by default (`allowBeanDefinitionOverriding = true`), and the later definition silently wins.
- **`ConflictingBeanDefinitionException`** — thrown during classpath scanning when a *generated* name clashes with an existing **non-compatible** definition (a genuinely different class/source).

#### Hidden concept: `isCompatible` tolerates re-scans

The scanner does *not* throw on every duplicate name. An identical re-detection of the *same* class is considered *compatible* and tolerated — which is precisely why overlapping `@ComponentScan` packages so often fail to error even though the same class is discovered twice. Only a same-name-but-genuinely-different-definition clash is treated as a real conflict.

#### Cross-package clashes: `FullyQualifiedAnnotationBeanNameGenerator`

When two scanned components in different packages share a simple class name (`com.foo.MyService` and `com.bar.MyService`), their derived default names collide. The documented fix is to configure `FullyQualifiedAnnotationBeanNameGenerator` (via `@ComponentScan(nameGenerator = ...)` or `<context:component-scan name-generator=.../>`), which defaults bean names to the fully-qualified class name. (The generator must have a public no-arg constructor.)

**Design intent.** Short names are the ergonomic default; fully-qualified names are ugly and leak package structure into wiring. Auto-resolving collisions by package was explicitly *requested and deliberately rejected* as a default (SPR-14665 / issue #19229). Spring keeps the pleasant default and makes you *opt in* to FQN names only when you actually hit a clash, rather than penalizing every project for a rare problem.

**Changed in v4/v7 (carried over, still the default).** Spring Boot keeps `allowBeanDefinitionOverriding = false` (`spring.main.allow-bean-definition-overriding` default `false`) — the default since Boot 2.1 and still false in Boot 4.x. A same-name registration throws `BeanDefinitionOverrideException`, and `BeanDefinitionOverrideFailureAnalyzer` advises either renaming a bean or enabling the property. Raw Spring Framework still defaults to *allowing* overriding.

**Design intent of the Boot flip.** Silent override is a footgun, and Boot auto-registers hundreds of beans the developer never sees. A stray same-named user bean shadowing an auto-config bean (or two auto-configs colliding) would quietly replace one with the other, producing invisible-cause runtime behavior. Boot flips the default to `false` so the collision becomes a *loud startup failure with a remediation hint*. Raw Framework keeps `true` because hand-written configs are smaller and intentional, and for backward compatibility.

### The autowiring resolution algorithm

For a **single-valued** injection point, Spring resolves candidates through a layered policy, each layer more explicit than the last:

1. Match by **type**.
2. If multiple, narrow by **generic type** (the type parameter acts as an implicit qualifier).
3. Apply **`@Qualifier`** / custom qualifier metadata.
4. Apply **`@Primary`** (promote one) or **`@Fallback`** (demote the rest).
5. Fall back to matching the **bean name against the field/parameter name**.
6. If still more than one candidate, throw **`NoUniqueBeanDefinitionException`**.

Internally (in `DefaultListableBeanFactory.doResolveDependency`, v7.0.5) this is a six-step pipeline: (1) a pre-resolved `@Autowired` shortcut, (2) a `@Value`/expression suggested value, (3) a fast declared-name/qualifier-name shortcut, (4) multiple-bean handling (stream/array/collection/map) and candidate finding, (5) `determineAutowireCandidate` if more than one candidate, (6) validation of the single result. The selection step `determineAutowireCandidate` orders its tie-breakers as: primary candidate → bean-name-vs-dependency-name match → qualifier-suggested-name match → highest `@Priority` → unique default-candidate → directly-registered `resolvableDependency`.

**Design intent — fail fast on ambiguity.** This is the central philosophy of the whole DI engine. When the container cannot *prove* a unique intended target, any guess (pick first, pick newest, pick alphabetical) would be a silent correctness landmine. Spring instead refuses to start and reports the exact ambiguous injection point and the candidate set. The mental model it instills: **wiring must be unambiguous by construction**, and any ambiguity is a *design decision the developer must make explicit* — via `@Primary`, `@Fallback`, `@Qualifier`, generics, or naming — never something the framework should paper over. (If *zero* candidates match, it is instead `NoSuchBeanDefinitionException`, unless `required = false`, `Optional<T>`, or `@Nullable` makes the point optional.)

#### Hidden concept: the bean name is an implicit qualifier

Step 5 above means the field/parameter *identifier* is matched against bean names as the final tie-break before failure. Consequently, **renaming a field can silently change which bean is injected** — a subtle coupling between source identifiers and wiring that surprises developers who think names are cosmetic.

#### Hidden concept: the fast standard-bean-lookup shortcut and its guards

Practitioners often assume name matching happens only as a *late* tie-breaker. In fact v7's `doResolveDependency` also tries an **early** name/qualifier-to-bean-name shortcut (step 3) *before* the expensive `findAutowireCandidates`, but only when `descriptor.usesStandardBeanLookup()` and *all* guards pass: type match, `isAutowireCandidate`, **not** a fallback, **no** primary conflict (`hasPrimaryConflict`), and **not** a self-reference.

#### Hidden concept: `resolvableDependencies`

Infrastructure objects like `ApplicationContext`, `BeanFactory`, `ResourceLoader`, and `ApplicationEventPublisher` are injectable even though they are *not* normal bean definitions. They live in a `resolvableDependencies` map and are matched at the lowest-precedence step of `determineAutowireCandidate`.

### `@Primary` and its inverse `@Fallback`

`@Primary` marks one bean of a type as the preferred candidate for single-valued injection when several match. Internally, `determinePrimaryCandidate` does a two-pass selection: first it looks for a unique `@Primary` bean (two local primaries throw `NoUniqueBeanDefinitionException`, "more than one primary bean found"; a local primary beats a parent-context primary). Only if there is no primary does it run a second pass for the unique **non-fallback** candidate.

**`@Fallback`** is the inverse companion: rather than *promoting* one bean, it *demotes* beans so they are used only if no regular (non-fallback) candidate exists. If exactly one regular bean remains after excluding fallbacks, it effectively becomes primary by type — with *zero* annotations on the regular bean.

```java
@Bean @Fallback DataSource defaultDataSource() { ... }   // library-supplied default
@Bean             DataSource appDataSource()    { ... }   // wins automatically, no @Primary needed
```

**Design intent — two opposite ergonomics for one problem.** `@Primary` fits "I have one obvious default and a few specials" (promote one). `@Fallback` fits "I have a real implementation *plus* library-supplied defaults" (demote the defaults). The killer use case, easy to miss, is **composition across module boundaries**: a starter/library can ship a `@Fallback` bean that the application's own plain bean *transparently supersedes* without the application having to annotate anything `@Primary`. This makes graceful default-overriding composable between modules.

**Changed in v4/v7 (carried over from 6.2).** `@Fallback` was introduced in **Spring Framework 6.2** and is fully present in 7.x. Developers migrating from a Boot 3.0–3.1 / Framework 6.0–6.1 baseline will encounter it as new, but it is not a 7-specific addition. The `NoUniqueBeanDefinitionException` fail-fast contract, `@Primary`, `@Qualifier`, generic-type qualifiers, and Map/collection injection all behave as in 6.2 — no breaking semantic change in v7.

> **Crucial caveat: `@Primary`/`@Fallback` never collapse collections.** They affect *single-valued* injection only. A `List<T>` or `Map<String, T>` still receives *every* matching candidate regardless of which is primary or fallback.

### Qualifiers, generics, and the implicit-qualifier philosophy

`@Qualifier("main")` at an injection point narrows type matches to a bean carrying that qualifier value (and a bean's name acts as an implicit qualifier too). You can define **custom qualifier annotations** meta-annotated with `@Qualifier`, optionally with attributes (e.g. `@MovieQualifier(genre=..., format=...)`), where a candidate must match *all* attribute values; qualifier metadata can be placed at the type level on a scanned component. Custom annotations *not* meta-annotated with `@Qualifier` can still be registered via `CustomAutowireConfigurer`.

The **generic type parameter acts as an implicit qualifier**: `Store<String>` injects the `stringStore` bean and `Store<Integer>` the `integerStore` bean, with no `@Qualifier` needed. This extends to collections — `List<Store<Integer>>` receives only `Store<Integer>` beans, excluding `Store<String>`.

**Design intent.** Generics already encode the developer's intent (`Store<Integer>` vs `Store<String>`); requiring a redundant `@Qualifier` would duplicate information the type system already carries. Reusing the type as the discriminator keeps wiring *type-safe and refactor-safe* — rename or re-parameterize the type and the wiring follows automatically.

### Collection, array, and map injection

Injecting an array, `List`, or `Set` of a type pulls in **all** matching beans, ordered by `@Order`, the standard `jakarta.annotation.Priority`, or the `Ordered` interface; absent those, by bean-definition registration order. A `Map<String, T>` can also be autowired: **keys are bean names, values are the beans of type `T`** (only `String` keys are supported), with the same ordering semantics applied to values.

```java
@Autowired List<Filter> filters;             // all Filter beans, @Order-sorted
@Autowired Map<String, Filter> filtersByName; // name -> bean, for free
```

#### Hidden concept: `@Order` vs `@Priority` scope

`@Order` is allowed at class level *and* on `@Bean` methods (per-definition). `@Priority` (a *type-level* standard annotation) **cannot annotate `@Bean` methods** — so for factory methods you must express the same intent with `@Order` plus `@Primary`/`@Fallback`. Many developers wrongly assume the two are interchangeable everywhere.

**Design intent.** `jakarta.annotation.Priority` is a standard annotation that legally targets types only; allowing it on methods would be a non-standard extension. Spring keeps standards-compliance and steers you to its own `@Order` (which *does* target methods), maintaining one consistent ordering vocabulary.

#### Hidden concept: injection-point ordering is NOT startup ordering

`@Order`/`@Priority`/`Ordered` influence the order elements appear in an *injected collection* — a consumer concern. They do **not** influence *singleton startup/instantiation order*, which is governed solely by actual dependencies and `@DependsOn`. (`@Priority`, uniquely, *also* breaks ties for single-valued selection; `@Order`/`Ordered` do not.) Conflating the two would let a cosmetic hint accidentally reorder initialization and mask real dependency bugs — so Spring keeps them strictly separate.

### Injection styles: constructor, setter, field

Spring supports constructor, setter, and field injection, but the team **officially advocates constructor injection for required dependencies**.

- **Constructor injection.** Dependencies are passed as constructor arguments and assigned to `private final` fields. A class with a single constructor needs *no* `@Autowired` (implicit autowiring). Only **one** constructor may declare `@Autowired(required=true)`; if multiple constructors carry `@Autowired` they must *all* be `required=false`, and Spring picks the greediest satisfiable one.
- **Setter / method injection.** The container calls setters (or any multi-arg `@Autowired` method) after no-arg construction. A non-required method is *not called at all* if any argument is unsatisfiable; a non-required field is left at its default.
- **Field injection.** `@Autowired` on a field, set via reflection after instantiation.

```java
@Service
public class OrderService {
    private final PaymentGateway gateway;   // required, immutable
    public OrderService(PaymentGateway gateway) {   // no @Autowired needed
        this.gateway = gateway;
    }
}
```

**Design intent — dependencies are part of identity, not bolted-on state.** Constructor injection encodes the class invariant *in the type system*: an object cannot exist without its mandatory collaborators, so it is always handed to clients fully initialized and non-null, eliminating defensive null checks; `final` fields give immutability and thread-safety. The deepest "why": because dependencies enter through a plain Java constructor, the class is decoupled from Spring entirely — a unit test just calls `new OrderService(mockGateway)` with *no container and no reflection*. DI as a *design discipline* (POJOs with explicit collaborators) is valuable independent of the IoC container; the container is merely one wiring mechanism.

Setter injection is reserved for *optional* dependencies with sensible defaults ("constructors for mandatory, setters for optional"). Field injection is *discouraged*: fields cannot be `final` (no immutability), the object can exist in an invalid null state, and — the real cost — a field-injected class has **no constructor or setter through which a unit test can supply mocks**, pushing developers toward slow `@SpringBootTest` integration tests or reflection. A large constructor is deliberately left *painful* rather than papered over with field injection, so that the pain signals a Single-Responsibility violation and prompts refactoring instead of hiding ten `@Autowired` fields.

### Circular dependencies and the early-singleton reference

Spring **tolerates** circular dependencies among *setter/field-injected singletons* but **fails** on *constructor* cycles. In `AbstractAutowireCapableBeanFactory.doCreateBean`, if `earlySingletonExposure` holds (`mbd.isSingleton() && allowCircularReferences && isSingletonCurrentlyInCreation(beanName)`), then *before* `populateBean` the container calls `addSingletonFactory(beanName, () -> getEarlyBeanReference(...))`, exposing a raw (pre-initialization) reference so a collaborator created mid-cycle can wire to it. A constructor-only cycle cannot use this trick — the object does not yet exist — and fails fast with `BeanCurrentlyInCreationException`.

This relies on a three-tier singleton cache: `addSingletonFactory` places an `ObjectFactory` in `singletonFactories`; a later `getSingleton(name, false)` promotes it into `earlySingletonObjects`. `getEarlyBeanReference` runs `SmartInstantiationAwareBeanPostProcessor`s so that an early AOP proxy is created for the partially-built bean if needed.

#### Hidden concept: `allowRawInjectionDespiteWrapping`

If a singleton in a cycle gets AOP-proxied *after* another bean already injected its *raw* early reference, the collaborator would be holding a non-proxied object. Spring detects this mismatch and throws `BeanCurrentlyInCreationException` ("injected … in its raw version … but has eventually been wrapped") unless `allowRawInjectionDespiteWrapping` is set. The fix hint is usually to avoid over-eager type matching (`getBeanNamesForType` with `allowEagerInit=false`).

**Design intent.** A constructor cycle is *logically unsatisfiable* — neither object can be built first — so failing at startup is correct. Setter/field cycles *are* satisfiable via early exposure, so Spring permits them, but the early-singleton machinery is a *pragmatic escape hatch, not an endorsement*: a cycle still signals tangled design, and the recommendation remains to remove it.

**Changed in v4/v7.** **AOT-optimized contexts (Boot 4 / native images) fail to start on explicit circular references** — the runtime early-singleton trick is unavailable under AOT. To break cycles in AOT builds you must use `@Lazy` injection points or `ObjectProvider`. At the *regular* runtime, setter/field cycles still resolve as before.

### Deferral primitives: `ObjectProvider`, `Provider`, `@Lazy`

Not every dependency should be resolved eagerly at wiring time. Spring offers indirection objects that make deferral *explicit in the type signature*:

- **`ObjectFactory<T>`** — `getObject()`.
- **`ObjectProvider<T>`** — adds `getIfAvailable()`, `getIfUnique()`, `stream()`, `orderedStream()`; the recommended tool for sophisticated optional (0..1), multiple (0..N), lazy, or cycle-breaking access.
- **`jakarta.inject.Provider<T>`** — the JSR-330 equivalent, `get()`.
- **`@Lazy`** on an injection point — injects a *lazy-resolution proxy* that resolves the real bean on first use, useful to break a cycle or defer an expensive bean. The docs themselves call this approach "rather limited" and prefer `ObjectProvider` for nuanced cases.

**Design intent.** The container is *eager by default* (so wiring errors surface at boot), but shorter scopes, expensive beans, optional/multiple dependencies, and cycles all need opt-in laziness. Encoding that laziness in the *type* (`ObjectProvider<Foo>` rather than `Foo`) keeps the eager model intact while making the deferral visible and honest rather than hidden.

### `@Value` injection

`@Value` injects externalized values: `${property}` placeholders, `#{SpEL}` expressions, and `${prop:default}` defaults. It is processed by a `BeanPostProcessor` using a `ConversionService` (`String` → `int`, comma-separated → `String[]`). In `doResolveDependency` this is step 2 (`getSuggestedValue`).

#### Hidden concept: the lenient-by-default resolver

The default embedded value resolver is **lenient**: an unresolved `${catalog.name}` is injected as the *literal string* `"catalog.name"` rather than failing — silently producing wrong configuration. Registering a `PropertySourcesPlaceholderConfigurer` makes resolution *strict* (fail on unresolved placeholders). This is a classic, easy-to-miss source of bugs.

### Self-injection

`@Autowired` *considers* self references — a bean injecting itself — but only as a **last-resort fallback**: self references never count as normal candidates, are never primary, and always sort to lowest precedence. The intended use is invoking another method on the *same* instance through its transactional/AOP proxy (a plain `this.method()` call bypasses the proxy). Alternatives are `@Resource` (obtains a proxy back by unique name) or factoring the methods into a separate delegate bean.

**Design intent.** Self injection is almost always a smell; making it lowest-precedence ensures real collaborators always win, so the feature solves the proxy-invocation problem without polluting normal candidate selection.

### Bean scopes

The default scope is **singleton**: one shared instance *per container per bean definition* (not the GoF per-classloader singleton), eagerly pre-instantiated at context refresh and published thread-safely under a singleton creation lock. **Prototype** scope yields a fresh, fully-initialized instance on every `getBean()`/injection-point resolution, after which the container *forgets* it. **Web scopes** are request, session, application, and websocket. **Custom scopes** plug in via the `Scope` SPI.

**Design intent — singleton as default, eager instantiation.** Most enterprise collaborators (services, repositories, controllers) are stateless and infrastructure-like; sharing one instance minimizes object churn and GC pressure and matches the mental model that *wiring describes a fixed application topology*, not a per-request object graph. Eager pre-instantiation is fail-fast: missing dependencies, bad config, and environment problems surface at startup, not at the first user request in production. The cost is paid once, deterministically; laziness is opt-in (`@Lazy` / default-lazy-init) for genuinely expensive beans. The default nudges developers toward stateless design — "stateless → singleton, stateful → prototype."

#### Hidden concept: the container does NOT destroy prototypes

Initialization callbacks *do* run on prototypes, but configured **destruction callbacks (e.g. `@PreDestroy`, `DisposableBean`) are never invoked** for prototype beans. The container hands off the instance and forgets it; the *client* owns cleanup of expensive resources. Tracking every prototype for destruction would require retaining references — a memory leak that contradicts the "hand off and forget" contract. The mental model: **a prototype is factory output you now own.**

#### Hidden concept: the singleton-holds-prototype gotcha

Injecting a prototype into a singleton resolves the prototype **exactly once**, at the singleton's instantiation time, so the singleton keeps the *same* prototype instance forever — defeating prototype semantics entirely. Fixes: **method injection** (`@Lookup` / abstract lookup method) to fetch a fresh instance per call, an `ObjectProvider`/`Provider<T>`, or a scoped proxy. This is the canonical "scope mismatch" problem.

#### Hidden concept: application scope ≠ ApplicationContext singleton

`@ApplicationScope` is one instance *per `ServletContext`*, exposed as a `ServletContext` attribute, so it is shared across multiple Spring `ApplicationContext`s in the same web app — semantically different from a plain singleton, which is per-container.

#### Web scopes and request binding

Web scopes require a web-aware context. Outside a `DispatcherServlet` you must bind the request to the servicing thread via `RequestContextListener` or `RequestContextFilter` (the dispatcher, listener, and filter all do the same job). Convenience annotations `@RequestScope`, `@SessionScope`, `@ApplicationScope` are each meta-annotated with `@Scope` and default `proxyMode` to `TARGET_CLASS`.

#### The `Scope` SPI

A custom scope implements `org.springframework.beans.factory.config.Scope`:

```java
public interface Scope {
    Object get(String name, ObjectFactory<?> objectFactory);
    Object remove(String name);
    void registerDestructionCallback(String name, Runnable callback);
    Object resolveContextualObject(String key);
    String getConversationId();
}
```

Register it via `ConfigurableBeanFactory.registerScope(String, Scope)` or declaratively with `CustomScopeConfigurer`. You **cannot override the built-in `singleton` and `prototype` scopes.** `SimpleThreadScope` ships as an example — but note it is thread-bound and **does not itself invoke destruction callbacks** on thread teardown, a documented and frequently-surprising caveat. The often-overlooked `resolveContextualObject(String key)` exposes scope-contextual objects (e.g. the current request/session) to custom-scope authors.

### Scoped proxies

A longer-lived bean (a singleton) cannot hold a *direct* reference to a shorter-lived bean (a session-scoped one), because that reference would go stale. Instead the container injects a **proxy** of the same public type that, on each method call, fetches the live instance from the active scope. Configured via `@Scope(proxyMode=...)` / `<aop:scoped-proxy/>`, with `ScopedProxyMode` values: `TARGET_CLASS` (CGLIB class proxy — the default for `@RequestScope`/`@SessionScope`/etc.), `INTERFACES` (JDK dynamic proxy), `NO`, `DEFAULT`.

**Design intent.** DI wires a graph *once*; a singleton cannot be re-injected per request. Rather than leak scope-lookup logic into business code (calling `getBean` each time), Spring interposes a transparent proxy so the long-lived bean codes against a *stable reference* while the proxy resolves the live short-lived target per invocation — preserving IoC and keeping business code scope-agnostic.

#### Hidden concept: CGLIB proxies skip private and final methods

A CGLIB scoped/AOP proxy cannot intercept `private` or `final` methods, so a call to such a method does **not** delegate to the real scoped target and silently bypasses scope resolution.

**Changed in v4/v7 — proxy-default consistency (precisely framed).** What genuinely changed in Spring Framework 7.0 is that *whatever the global proxy-type default is in a given setup* is now applied **consistently across all proxy processors**, including `@Async`/`@EnableAsync` (previously some processors independently chose JDK proxies regardless of the global setting). This is a consistency change, not a flip of the core default to CGLIB: CGLIB is **not** the core-framework default — the official reference docs state the core framework suggests *interface-based (JDK) proxies* by default, and it is *Spring Boot* that, depending on configuration properties, enables class-based (CGLIB) proxies by default. The new `@Proxyable` annotation lets individual beans opt out: `@Proxyable(INTERFACES)` to force JDK-interface proxying against a CGLIB/Boot default, or `@Proxyable(TARGET_CLASS)` against the regular JDK default; a `ProxyConfig` bean can set defaults.

The scope model itself (singleton/prototype/request/session/application/websocket), the `Scope` SPI, and `ScopedProxyMode` values are **unchanged** from Framework 6 to 7.

### Lifecycle callbacks and their ordering

For a single bean, **initialization** callbacks fire in this order:

1. `@PostConstruct` (JSR-250, now `jakarta.annotation`)
2. `InitializingBean.afterPropertiesSet()`
3. custom init-method / `@Bean(initMethod=...)`

**Destruction** is the mirror image:

1. `@PreDestroy`
2. `DisposableBean.destroy()`
3. custom destroy-method / `@Bean(destroyMethod=...)`

**Design intent.** Spring recommends the JSR-250 annotations (`@PostConstruct`/`@PreDestroy`) over the `InitializingBean`/`DisposableBean` interfaces precisely because annotations keep POJOs *decoupled from Spring-specific types* — the same class is portable across Jakarta-EE/CDI containers and unit-testable without Spring. The interface callbacks remain for cases needing programmatic control but are considered legacy style.

#### Hidden concept: `@PostConstruct` runs under the singleton creation lock

A bean is considered fully initialized and *publishable* only after `@PostConstruct` returns, and it runs *inside* the container's singleton creation lock. Blocking or doing cross-bean async work there can deadlock — which is precisely *why* the separate `SmartInitializingSingleton` / `ContextRefreshedEvent` hooks exist (see below).

#### Hidden concept: same-named method dedup

If the *same method name* serves multiple lifecycle mechanisms (an `init()` that is both `@PostConstruct` and the configured init-method), it is invoked **once**, not multiple times.

#### Hidden concept: `@Bean(destroyMethod)` auto-inference and shutdown hooks

By default, `@Bean` auto-detects a public `close()` or `shutdown()` method and calls it as the destroy method; set `destroyMethod=""` to disable this inference (relevant when `close()` has unrelated semantics). And in a *standalone* application, destruction callbacks fire **only** if you call `ctx.registerShutdownHook()` or `ctx.close()` — many developers wrongly assume JVM exit handles it.

### `Aware` callbacks

`Aware` interfaces inject container infrastructure into a bean. `BeanNameAware.setBeanName` fires *after* property population but *before* init callbacks. The fuller order is: `BeanNameAware` → `BeanClassLoaderAware` → `BeanFactoryAware` (these driven directly by the bean factory) → the context-level group (`EnvironmentAware`, `EmbeddedValueResolverAware`, `ResourceLoaderAware`, `ApplicationEventPublisherAware`, `MessageSourceAware`, `ApplicationContextAware`) → `BeanPostProcessor.postProcessBeforeInitialization` → `@PostConstruct` → `afterPropertiesSet` → init-method.

#### Hidden concept: the context-level `Aware` group fires together

The context-level `Aware` callbacks are *not* invoked one-by-one by the factory; they are all driven by a single `BeanPostProcessor`, the `ApplicationContextAwareProcessor` — which is why they fire as a *group*, before `@PostConstruct`.

**Design intent.** `Aware` interfaces tie code to the Spring API and *invert* IoC (the bean reaches into the container instead of being given what it needs). Spring keeps them for *infrastructure* beans that genuinely need container access and steers application beans toward constructor injection (or autowiring `ApplicationContext` when truly necessary).

### `SmartInitializingSingleton` and `Lifecycle`

`SmartInitializingSingleton.afterSingletonsInstantiated()` is a single callback invoked **once at the end of singleton pre-instantiation**, after *all* eager singletons exist and are initialized — the right place for expensive post-init work (async DB prep, building cross-bean indexes) that must run *outside* the per-bean creation lock and against a *complete* graph. (`ApplicationListener<ContextRefreshedEvent>` / `@EventListener(ContextRefreshedEvent.class)` is an alternative.)

Distinct from init/destroy, `Lifecycle{start/stop/isRunning}` drives the *running state* of components after the context is up. `SmartLifecycle` adds `isAutoStartup()`, `getPhase()`, and `stop(Runnable)`. The lowest phase starts first and stops last (`Integer.MIN_VALUE` = first to start, last to stop; default phase 0); `DefaultLifecycleProcessor` has a default per-phase shutdown timeout.

**Design intent.** `@PostConstruct` runs under the singleton lock against a *partially-built* graph; heavy or cross-bean work there risks deadlocks and ordering bugs. Providing a distinct "all singletons ready" hook plus a separate running-state lifecycle yields a clean phased model: **build graph → finalize → run.**

> **Correction on a `SmartLifecycle` claim.** The "ordered, and potentially concurrent, shutdown of all components having a common shutdown order value" semantics of `SmartLifecycle.stop(Runnable)` are *not* new in v4/v7 — that Javadoc wording has existed since Spring 3.0. (`stop(Runnable)` is declared on `SmartLifecycle`/`Lifecycle`, not on `DefaultLifecycleProcessor`; the processor merely invokes it, achieving within-phase concurrency via a per-phase `CountDownLatch` when beans complete asynchronously.) The genuinely recent lifecycle additions were in 6.2 (custom per-phase shutdown timeouts) and 6.2.6 (concurrent *startup* of specific phases) — not v7.

### Container extension SPIs: the dogfooding philosophy

Spring exposes two distinct extension SPIs and implements nearly every core feature as an infrastructure bean built on them.

- **`BeanFactoryPostProcessor` (BFPP)** mutates bean **definitions** (the blueprints) after they are loaded but *before any non-BFPP bean is instantiated*. It must **not** instantiate beans (doing so triggers premature instantiation and bypasses other BFPPs).
- **`BeanDefinitionRegistryPostProcessor` (BDRPP)** is a BFPP sub-interface that can additionally **add/remove** definitions via `postProcessBeanDefinitionRegistry`, which runs strictly *before* all `postProcessBeanFactory` callbacks.
- **`BeanPostProcessor` (BPP)** wraps/modifies bean **instances** with `postProcessBeforeInitialization` (before `@PostConstruct`/`afterPropertiesSet`/init-method) and `postProcessAfterInitialization` (after them). Either may return a *different* object — which is exactly how AOP returns a proxy in place of the target.

Spring's own features are built on these public hooks:

| Feature | Implemented by | SPI |
|---|---|---|
| `@Configuration`/`@Bean`/`@Import`/`@ComponentScan` | `ConfigurationClassPostProcessor` (PriorityOrdered, HIGHEST_PRECEDENCE) | BDRPP |
| `@Autowired`/`@Value`/`@Inject` | `AutowiredAnnotationBeanPostProcessor` | BPP |
| `@PostConstruct`/`@PreDestroy`/`@Resource` | `CommonAnnotationBeanPostProcessor` (PriorityOrdered) | BPP |
| `${...}` placeholders | `PropertySourcesPlaceholderConfigurer` | BFPP |
| AOP | `AbstractAutoProxyCreator` subclasses | (Smart)InstantiationAware BPP |

**Design intent — split the two phases, then eat your own dog food.** The two SPIs map to the two phases of the IoC lifecycle: there is a *blueprint* (`BeanDefinition`) you edit, and there is a *constructed object* you decorate. Conflating them invites the anti-pattern of instantiating beans during the definition phase. Building the framework's own features (`@Configuration`, `@Autowired`, `@Value`, `@PostConstruct`, AOP, placeholders) as ordinary BFPP/BPP beans — auto-registered by `AnnotationConfigUtils.registerAnnotationConfigProcessors` / `<context:annotation-config/>` — proves the extension points are powerful enough for real work, keeps the core container tiny and annotation-agnostic, and lets users add or replace features through the *exact same* mechanism. The rejected alternative — hard-coding annotation handling inside the container — would make it monolithic and unextensible.

`ConfigurationClassPostProcessor` is a BDRPP at HIGHEST_PRECEDENCE specifically because `@Bean` definitions in `@Configuration` classes must be *registered* (hence BDRPP, not plain BFPP) before any other BFPP runs — so that, for example, a placeholder configurer can resolve `${...}` inside an `@Bean`-defined datasource. It also CGLIB-enhances full `@Configuration` classes so inter-`@Bean`-method calls return the singleton.

#### Hidden concept: full vs. lite `@Configuration`

A full `@Configuration` class (`proxyBeanMethods=true`) is CGLIB-subclassed so a call from one `@Bean` method to another returns the *managed singleton*. Setting `proxyBeanMethods=false` (lite mode) skips the subclass for faster startup but *loses* inter-bean-method singleton semantics — a subtle correctness/performance trade-off.

#### Hidden concept: the three-phase ordering algorithm

Both `invokeBeanFactoryPostProcessors` and `registerBeanPostProcessors` (in `PostProcessorRegistrationDelegate`) partition processors into three sorted phases: (1) `PriorityOrdered`, (2) `Ordered`, (3) unordered. For BDRPPs, *all* `postProcessBeanDefinitionRegistry` callbacks (across all tiers, re-scanned to catch newly-registered BDRPPs) complete before any `postProcessBeanFactory` runs.

**Design intent.** `PriorityOrdered` is the *bootstrap* layer that must be honored before anything else can even be sorted/instantiated; `Ordered` is normal relative ordering among bootstrapped processors; unordered is best-effort last. This staged contract lets framework-critical processors (config parsing, autowiring) guarantee precedence without a user accidentally reordering them via plain `@Order`.

#### Hidden concept: `MergedBeanDefinitionPostProcessor`s re-register LAST

During `registerBeanPostProcessors`, processors implementing `MergedBeanDefinitionPostProcessor` (e.g. `AutowiredAnnotationBeanPostProcessor`, `CommonAnnotationBeanPostProcessor`) are pulled into an `internalPostProcessors` list and re-registered *after* all others, so injection-metadata processing applies at a well-defined late point.

#### Hidden concept: BPPs are not auto-proxied

BPPs *and the beans they directly reference* are instantiated during a special early startup phase — before the AOP auto-proxy creator (itself a BPP) can act on them. Consequently, "**neither `BeanPostProcessor` instances nor the beans they directly reference are eligible for auto-proxying**," and Spring logs "Bean … is not eligible for getting processed by all BeanPostProcessors." This is a real source of "why isn't my `@Transactional` working?" bugs when a business bean is injected into a BPP. The recommended mitigation: declare BFPP/BPP `@Bean` methods as **`static`** and keep them dependency-light, so producing the processor does not prematurely initialize its surrounding `@Configuration` class.

#### Hidden concept: `getEarlyBeanReference` for AOP + cycles

`SmartInstantiationAwareBeanPostProcessor.getEarlyBeanReference` lets the auto-proxy creator publish the *proxy* before full initialization, so circular collaborators inject the proxy rather than the raw target. The default implementation returns the bean as-is; AOP overrides it. The mental model: **the proxy identity is decided as early as the dependency graph demands.** Spring registers *different* auto-proxy creators depending on enabled features — `InfrastructureAdvisorAutoProxyCreator` for `@EnableTransactionManagement`/`@EnableCaching`, `AnnotationAwareAspectJAutoProxyCreator` for `@EnableAspectJAutoProxy` — sharing `AbstractAutoProxyCreator`, with registration hardened so the most capable creator wins.

**Design intent of eager processor instantiation.** A processor cannot process a bean created *before* the processor exists, so the container front-loads the entire post-processor population. The accepted cost — infrastructure beans and their direct dependencies cannot be fully post-processed/auto-proxied — is *explicitly documented and warned about* rather than hidden, teaching developers to keep BPPs lightweight.

### Cross-cutting v4/v7 changes

Several changes ripple across naming, wiring, and lifecycle:

- **`javax.*` removed.** `@Resource`, `@PostConstruct`, `@PreDestroy`, `@Inject`, `@Named`, and `jakarta.inject.Provider` must now come from the **`jakarta.*`** packages; Boot 3/Framework 6 still tolerated the legacy `javax` variants via compatibility, and Framework 7 dropped that fallback. Framework 7 has a Jakarta EE 11 baseline and a **JDK 17 baseline** (JDK 25 the recommended LTS runtime).
- **JSpecify nullness.** Nullness annotations migrated to `org.jspecify.annotations.*`; Spring's JSR-305-semantics annotations are deprecated. `@Nullable` at injection points should use the JSpecify type, and nullness now also covers generic-type/array/vararg elements. Pragmatic `@Nullable` checks at injection points now examine only *local* annotations rather than inherited ones, affecting `@Autowired` optionality resolution.
- **`BeanRegistrar` SPI.** A new functional, lambda-style, AOT-analyzable contract for programmatic/dynamic bean registration — an alternative to writing a hand-rolled BDRPP (which is opaque to AOT). It addresses the constraint that a single `@Bean` method should register exactly one bean of its most concrete return type.
- **AOT participation.** `ConfigurationClassPostProcessor` also implements `BeanRegistrationAotProcessor` and `BeanFactoryInitializationAotProcessor`; in AOT/native builds, config-class parsing and bean-definition contribution happen at *build time*, with reflection-heavy runtime post-processing replaced by generated code.
- **Bean Overrides on non-singletons.** `@MockitoBean`, `@MockitoSpyBean`, and `@TestBean` can now be applied to *prototype* and *custom-scoped* beans (previously singleton-only).
- **Release timing (corrected).** Spring Framework 7.0 reached GA on **2025-11-13**, and **Spring Boot 4.0 GA'd on 2025-11-20** (not October 2025, as sometimes stated). The fail-fast `NoUniqueBeanDefinitionException` contract, `@Primary`/`@Fallback`, qualifiers, generic-type qualifiers, Map/collection injection, and `@Order`/`@Priority` ordering all carry forward unchanged from 6.2.

The through-line: across naming, wiring, scoping, and lifecycle, Spring consistently chooses an *ergonomic default* (decapitalized names, singleton scope, type-based autowiring, eager instantiation) backed by a *loud, explicit failure* when the default cannot be applied unambiguously — and it builds even its own annotation processing on the same public extension points it offers you, so the container stays small and the philosophy stays uniform.


---

## Configuration, Conditions & Auto-configuration

This section dissects the three concentric rings of Spring's configuration model. At the center sits the Framework primitive: the `@Configuration` class and its `@Bean` methods, and the family of `@Import` mechanisms that compose them. Around that sits `@Conditional` — the predicate SPI that decides *whether* a definition is even considered. And wrapping both is Spring Boot's auto-configuration: a disciplined, opinionated application of conditional configuration that "backs away" the moment you assert your own intent. The throughline is a single design value: **convention you can always override, never lock-in** — achieved not by magic flags but by composing small, honest primitives.

### `@Configuration`: full mode and the CGLIB illusion

The default `@Configuration` runs in **full mode** (`proxyBeanMethods=true`). At startup, `ConfigurationClassPostProcessor` marks the class as a "full" configuration class and CGLIB-subclasses it. The generated subclass overrides each `@Bean` method so that it first consults the bean factory for an already-created (singleton/scoped) instance and only invokes your actual method body on a true cache miss.

```java
@Configuration // proxyBeanMethods=true by default
public class AppConfig {
    @Bean BeanTwo beanTwo() { return new BeanTwo(); }
    @Bean BeanOne beanOne() { return new BeanOne(beanTwo()); } // returns the MANAGED beanTwo singleton
}
```

The **design intent** here is to let Java configuration *read like plain Java* while still upholding the container's singleton contract. The mental model Spring wants to instill is: "a `@Bean` method is the single definition of that bean, and calling it always yields the managed instance." Without interception, `new BeanOne(beanTwo())` would silently create a *second, unmanaged* `beanTwo` — a subtle, surprising bug. CGLIB makes the intuitive syntax also be the *correct* semantics. The rejected alternative — forcing everyone to wire via method parameters — was judged too verbose for the common case and a poor on-ramp from the XML world Java config replaced.

This power has a cost imposed by subclassing: a full-mode `@Configuration` class **may not be `final`**, and its `@Bean` methods **may not be `private` or `final`** (a subclass cannot override what it cannot see or extend, so interception would silently break). CGLIB also requires the class to be instantiable, because the generated subclass invokes a superclass constructor.

### Lite mode: `@Bean` as a plain factory method

Set `proxyBeanMethods=false` ("lite mode") — or declare `@Bean` methods inside a plain `@Component` — and *no* CGLIB subclass is generated. `@Bean` methods become, per the reference docs, "a general-purpose factory method mechanism without special runtime processing." A direct call from one `@Bean` method to another is now an ordinary Java call: it runs the method body and returns a **new instance every time**, bypassing the container cache.

> **Hidden subtlety:** the reference docs phrase this more strongly than "calls create new instances." Lite-mode `@Bean` methods are **"not meant to declare inter-bean dependencies" at all**. You should not even *think* in terms of method-to-method references; collaborators must be received as autowired *method parameters* (or read from the containing component's fields). A common misconception is that `proxyBeanMethods=false` only "sometimes" breaks calls — in fact you should never make them.

```java
@Configuration(proxyBeanMethods = false)
public class AppConfig {
    @Bean BeanTwo beanTwo() { return new BeanTwo(); }
    // Wire via PARAMETERS — Spring injects the managed beanTwo:
    @Bean BeanOne beanOne(BeanTwo beanTwo) { return new BeanOne(beanTwo); }
}
```

The **design philosophy** of offering lite mode (rather than removing CGLIB entirely) is to let authors *declare their intent*. Interception has real cost — subclass generation, larger footprint, GraalVM-native hostility, and the `final`/`private` restrictions — that is pure waste when a config never makes intra-class `@Bean` calls. Rather than a global on/off switch, Spring exposes a per-class opt-out so that an author who writes `proxyBeanMethods=false` is asserting "this class wires only via parameters; don't make me pay for proxying." The teaching goal is to make developers *conscious* that inter-bean **method calls** are a privileged, expensive feature, while **parameter injection** is the universally correct default. The generalized best practice: use `proxyBeanMethods=false` unless you specifically rely on inter-bean method-call interception.

### `@Bean` method semantics

A `@Bean` method's name defaults to the method name; the `name`/`value` attribute overrides it and accepts a `String[]` for aliases, e.g. `@Bean({"dataSource", "a-ds", "b-ds"})`. Method parameters are autowired (constructor-injection-equivalent), the canonical wiring style in lite mode. `initMethod`/`destroyMethod` name lifecycle callbacks; `@PostConstruct`/`@PreDestroy`, `InitializingBean`/`DisposableBean`, the `*Aware` callbacks, and `Lifecycle` all work. Declare the *most concrete* return type so the container can predict the bean's type accurately for matching. `@Scope` (and `@SessionScope`, etc.) set scope with an optional scoped-proxy `proxyMode`.

> **Hidden concept — default destroy-method INFERENCE.** `@Bean` automatically infers a public `close()` or `shutdown()` method as the destroy callback. This silently closes pools and clients on shutdown — convenient, but a trap for JNDI or container-managed `DataSource`s that Spring does *not* own. For those you **must** set `destroyMethod=""` to suppress the inference, or you risk shutting down a resource you should never touch. Widely missed.

> **Hidden concept — `static` `@Bean` methods for post-processors.** `BeanFactoryPostProcessor` and `BeanPostProcessor` beans must be declared with **`static`** `@Bean` methods so they can be instantiated very early without forcing premature initialization of the enclosing `@Configuration` class. Non-static declarations produce ordering / early-init warnings.

> **Hidden concept — field injection forces early init.** `@Autowired` on a `@Configuration` field (or constructor) makes that configuration class initialize *early*; accessing locally-defined `@Bean` methods from `@PostConstruct` can then create circular references. Parameter injection on `@Bean` methods sidesteps this entirely.

### `@Bean` vs `@Component`, and what `@Configuration` actually adds

`@Component` (with `@ComponentScan`) means "Spring instantiates and wires *this class* as a bean." `@Bean` means "this method is a factory that *I, the developer*, call to produce a bean" — the tool for third-party/legacy types you cannot annotate, or where construction needs imperative logic. The two are different ownership models, not interchangeable styles.

> **Hidden concept — `@Configuration` *is* a `@Component`.** `@Configuration` is meta-annotated with `@Component`, so it is itself component-scannable and a managed bean. The *only* thing it adds over `@Component` is opting its `@Bean` methods into full-mode CGLIB interception (when `proxyBeanMethods=true`). Consequently, `@Bean` methods inside a plain `@Component` are *never* CGLIB-enhanced — they always run in lite mode.

### The `@Import` family: composing configuration at four altitudes

`@Import` is how configuration composes. It accepts four kinds of target, deliberately layered so that a contributor can plug in at *exactly* the altitude its problem requires:

1. **`@Configuration`/`@Component` classes** — registers their bean definitions directly.
2. **`ImportSelector`** — `selectImports(AnnotationMetadata)` returns class **names** to import, evaluated **immediately**. Decision point: "which config classes, decided from metadata, early."
3. **`DeferredImportSelector`** — the same, but deferred until *all* `@Configuration` classes are processed, with a `Group` for cross-selector merging. Decision point: "which config classes, decided *after* seeing the user's whole configuration, and merged/ordered across contributors." (Added in Spring Framework 4.0.)
4. **`ImportBeanDefinitionRegistrar`** — `registerBeanDefinitions(AnnotationMetadata, BeanDefinitionRegistry, BeanNameGenerator)` for fully programmatic, imperative `BeanDefinition` registration. Decision point: "I need to mint `BeanDefinition`s by hand, below the `@Bean`-method level." (The `BeanNameGenerator` overload was added in Spring 5.2.)

The **design rationale** is that each tier targets a different *decision point* in the container's lifecycle, and conflating them would force every contributor into the lowest, most opaque mechanism. The split keeps the imperative escape hatch available without making it the default.

> **Hidden concept — Aware callbacks vs single-constructor injection on selectors/registrars.** A selector or registrar may obtain `Environment`/`BeanFactory`/`ClassLoader`/`ResourceLoader` either by implementing the matching `*Aware` interface *or* by declaring a single constructor with those parameter types. Crucially, the `*Aware` callbacks fire **before** `registerBeanDefinitions()`.

> **Hidden concept — registrars cannot register a `BeanDefinitionRegistryPostProcessor`.** This is a lifecycle constraint baked into the javadoc: registrars run *during* configuration-class processing, which is *after* the BDRPP phase, so registering a BDRPP from a registrar is too late and is unsupported.

**Changed in v4/v7:** As of Framework 7.0, `@Import` declared on **interfaces implemented by a `@Configuration` class** is now honored. Previously `@Import` was processed only on the class itself and its superclass hierarchy.

### `DeferredImportSelector.Group`: coordinated, global merging

A `Group` implements `process(AnnotationMetadata, DeferredImportSelector)` to collect candidate imports across multiple selectors, then `selectImports()` returns the *merged, ordered* list of `Group.Entry` results. Selectors sharing a `getImportGroup()` class are processed together; ordering among deferred selectors uses `@Order`/`Ordered`.

This is precisely the engine behind Boot's `AutoConfigurationImportSelector`: it lets de-duplication, ordering (`@AutoConfigureBefore`/`After`/`Order`), and filtering apply **globally across every contributor** in one coordinated pass, rather than per-import. The design intent is that auto-configuration is a *single ordered batch* assembled from dozens of jars, and the `Group` is what makes that batch coherent.

> **Hidden concept — deferral exists for `@Conditional` correctness, not performance.** The whole point of deferring `DeferredImportSelector` is to ensure `@Conditional`/`@ConditionalOnMissingBean` on imported configs are evaluated *after* all user `@Configuration` is registered, so auto-config can reliably back off in favor of user-defined beans. It is a *correctness* mechanism that happens to look like a scheduling tweak.

### `ImportAware`: the backbone of `@Enable*` annotations

A class registered via `@Import` can implement `ImportAware` to receive `setImportMetadata(AnnotationMetadata)` — the annotation metadata of the `@Configuration` class that *imported* it. This is the mechanism behind every `@Enable*` annotation: an imported configuration reads attributes declared on the importer (e.g. the proxy/mode attributes on `@EnableScheduling` or `@EnableTransactionManagement`) and configures itself accordingly.

The **design philosophy** is that an `@Enable` annotation is "just sugar over `@Import` + metadata introspection." Composable feature toggles need the imported config to read the importer's attributes, and `ImportAware` keeps that flow declarative — avoiding passing configuration through globals or thread-locals. The mental model: there is no special "enable" machinery; it is the ordinary import + metadata pipeline.

### `BeanRegistrar` — programmatic registration done right

**Changed in v4/v7:** Spring Framework 7.0 introduces `BeanRegistrar`, a functional interface and the headline DI-container addition over Framework 6:

```java
public interface BeanRegistrar {
    void register(BeanRegistry registry, Environment env);
}
```

It is imported via `@Import(MyRegistrar.class)` on a `@Configuration` class, or registered directly through `GenericApplicationContext.register(BeanRegistrar...)`. `BeanRegistry.registerBean` exposes a fluent spec — `.prototype()`, `.lazyInit()`, `.description(...)`, and `.supplier(context -> ...)` — and the `Environment` parameter enables conditional registration:

```java
class MyRegistrar implements BeanRegistrar {
    public void register(BeanRegistry registry, Environment env) {
        registry.registerBean("foo", Foo.class);
        if (env.matchesProfiles("baz")) {
            registry.registerBean("bar", Bar.class,
                spec -> spec.lazyInit().supplier(ctx -> new Bar(ctx.bean(Foo.class))));
        }
    }
}
```

Kotlin uses `BeanRegistrarDsl` instead. A `BeanRegistrar` may also implement `ImportAware` to introspect the importing class's metadata.

The **design intent**, named directly in the release notes: `@Bean` methods must return one concrete type and shouldn't register several beans, which "gets in the way" of dynamic, looping, or multiple registration. The older escape hatches — `ImportBeanDefinitionRegistrar` and `BeanDefinitionRegistryPostProcessor` — are low-level, `BeanDefinition`-centric, and AOT-unfriendly. `BeanRegistrar` offers a concise functional API with an *instance-supplier* model that the AOT engine understands, giving programmatic flexibility **without** sacrificing native-image support — closing the gap between declarative `@Bean` and imperative registration.

> **Hidden concept — the instance supplier is the AOT-friendly part.** The `.supplier(context -> ...)` form with `context.bean(Type.class)` is exactly what lets the AOT engine generate code *without reflection*, which is precisely why `BeanRegistrar` is positioned as the modern replacement for `BeanDefinitionRegistryPostProcessor` in native images.

### A note on the v7 proxy-defaulting change

**Changed in v4/v7:** Framework 7.0 makes the *global proxy-type default* (whatever it is in a given setup) **consistently apply to all proxy processors** — including `@Async`/`@EnableAsync`, `@Transactional`, `@Cacheable`, `@Retryable`, etc. Previously some processors independently chose JDK proxies regardless of the global setting. A new per-bean `@Proxyable` annotation overrides the default: `@Proxyable(INTERFACES)` forces JDK interface proxying against a CGLIB default, `@Proxyable(TARGET_CLASS)` forces class-based proxying against a JDK default.

Note the careful framing: this is *not* a flip of the core framework default to CGLIB. The core Spring Framework still suggests **interface-based (JDK) proxies by default**; it is *Spring Boot* that, depending on configuration properties, has long enabled class-based (CGLIB) proxies by default. What changed in 7.0 is the *consistency* of how the global default propagates to every processor — not the core default itself. This proxy story is also entirely separate from `@Configuration`'s own `proxyBeanMethods` CGLIB mechanism, which remains independent.

**Changed in v4/v7:** Framework 7.0 also documents a custom `ConfigurationBeanNameGenerator`, including `FullyQualifiedConfigurationBeanNameGenerator`, to derive fully-qualified bean names for `@Configuration`/`@Bean` definitions that lack explicit names — useful to avoid name clashes across modules.

> **Continuity, not change:** `proxyBeanMethods` has existed since Framework 5.2, and the full-vs-lite distinction plus the `@Import` selector/registrar tiers are long-standing. They are behaviorally unchanged in v7 except as flagged above.

---

### `@Conditional` and the `Condition` SPI

`@Conditional(Class<? extends Condition>[])` is a meta-annotation placed on `@Component`/`@Configuration` types or `@Bean` methods (or composed into other annotations). Each listed `Condition` implements one method:

```java
public interface Condition {
    boolean matches(ConditionContext context, AnnotatedTypeMetadata metadata);
}
```

**ALL** conditions on an element must return `true` (AND semantics) for that element to be considered for registration. The SPI receives `AnnotatedTypeMetadata`, so a condition can read the attributes of the very annotation that triggered it — which is how one reusable `Condition` class can serve many parameterized annotations. Indeed, `@Profile` is itself implemented purely as `@Conditional(ProfileCondition.class)`, where `ProfileCondition` reads `Profile.value` from the metadata.

The **design rationale** is maximal minimalism: any registration decision reduces to a *pure predicate* over container state plus the triggering annotation's attributes. Passing metadata lets the entire `@ConditionalOnX` family be thin annotations delegating to a handful of `Condition` classes, avoiding a combinatorial explosion of bespoke conditions. The rejected alternative — imperative `if`/`registerBeanDefinition` code inside an `ImportBeanDefinitionRegistrar` or `BeanFactoryPostProcessor` — works but is opaque and non-declarative; `@Conditional` keeps the decision *next to the declaration*.

### `ConditionContext`: introspective, read-mostly container state

`ConditionContext` deliberately exposes the IoC container's introspective state, read-mostly, to the decision:

- `getRegistry()` → `BeanDefinitionRegistry` (throws `IllegalStateException` if none is available).
- `getBeanFactory()` → `@Nullable ConfigurableListableBeanFactory` (null if unavailable or not downcastable — bean-introspecting conditions **must** null-check).
- `getEnvironment()` → `Environment` (properties + active profiles; basis for `OnProperty`/`OnExpression`/`OnCloudPlatform`).
- `getResourceLoader()` → `ResourceLoader` (basis for `OnResource`).
- `getClassLoader()` → `@Nullable ClassLoader` (basis for `OnClass`).

The **mental model** is "a condition *observes* the world and *votes*" — never "a condition *mutates* the container." Read-mostly access keeps conditions side-effect-free and re-orderable. The honestly-`@Nullable` `BeanFactory`/`ClassLoader` signal that conditions can run in contexts (early parsing, AOT) where those aren't available, forcing authors to handle absence rather than NPE at runtime.

### `ConfigurationCondition`: the two-phase model

`ConfigurationCondition` extends `Condition` with `getConfigurationPhase()`, returning one of:

- **`PARSE_CONFIGURATION`** — evaluated *as* a `@Configuration` class is being parsed; a non-match means the class is never added.
- **`REGISTER_BEAN`** — evaluated when adding a regular (non-`@Configuration`) bean; a non-match does *not* prevent `@Configuration` classes from being added, and by the time it runs, **all `@Configuration` classes have been parsed**.

This two-phase split is the linchpin that makes bean-presence conditions correct. `@ConditionalOnBean`/`OnMissingBean`/`OnSingleCandidate` are inherently order-sensitive — they answer "has this bean been defined *so far*?" Running them during `PARSE_CONFIGURATION` would give wrong answers because not all definitions exist yet, so they are `REGISTER_BEAN`-phase conditions. Meanwhile, class-presence (`OnClass`) can be decided *immediately* at parse time to prune whole configuration trees cheaply. Collapsing both into one phase would either make `OnBean` unreliable (too early) or make `OnClass` needlessly late. The phase enum encodes this distinction as a first-class, author-declared contract.

> **Hidden concept — the single most common author mistake.** A bare `Condition` (not implementing `ConfigurationCondition`) has *no declared phase* and the parser treats it conservatively. Authors of bean-introspecting conditions **must** implement `ConfigurationCondition` and return `REGISTER_BEAN`, or they risk evaluating before sibling definitions exist.

> **Hidden concept — `@Conditional` does not always cascade to nested classes.** A `@Conditional` on an enclosing `@Configuration` applies to a nested `@Configuration` only when the nested class is reached via the parser's recursion or `@Import`. If the nested class is discovered *independently* (via `@ComponentScan` or direct registration), it is evaluated using only its *own* `@Conditional` annotations — a subtle source of "why did my nested config load anyway?" bugs.

### The Spring Boot `@ConditionalOnX` family

Boot builds a rich family on this SPI — all extending `SpringBootCondition` and producing `ConditionOutcome` objects:

- **`@ConditionalOnClass`/`OnMissingClass`** — evaluated via the `@Nullable` `ClassLoader` and, crucially, parsed from annotation metadata using **ASM**, so naming a class that is *absent* at runtime does not throw `NoClassDefFoundError` during condition evaluation. In a meta-annotation you must use the `name` (`String`) attribute rather than `value` (`Class`).
- **`@ConditionalOnBean`/`OnMissingBean`/`OnSingleCandidate`** — on `@Bean` methods the target type defaults to the method's return type (hence "declare the most specific return type"). All accept a `SearchStrategy` (`CURRENT`/`ANCESTORS`/`ALL`) to scope the bean-factory-hierarchy search.
- **`@ConditionalOnProperty`** — `prefix`+`name` form the key; matches when present and not `false` by default; `havingValue` pins an exact (case-insensitive) value; `matchIfMissing` (default `false`) controls absent-property behavior; multiple `name`s must *all* pass.
- **`@ConditionalOnExpression`** (SpEL; referencing a bean forces very early init), **`@ConditionalOnResource`**, **`@ConditionalOnWebApplication(SERVLET/REACTIVE)`/`OnNotWebApplication`**, **`@ConditionalOnWarDeployment`/`OnNotWarDeployment`**, **`@ConditionalOnCloudPlatform`**, **`@ConditionalOnJava(Range)`**.

> **Hidden concept — `OnClass` ASM safety has a sharp edge on `@Bean` methods.** Although the *condition* reads ASM metadata without loading the absent class, the JVM loads a `@Bean` method's parameter and return types *before* the method-level condition runs. So a class condition guarding a single `@Bean` method must be **hoisted onto a separate nested `@Configuration(proxyBeanMethods=false)` class** to isolate the unsafe type reference.

> **Hidden concept — `@ConditionalOnSingleCandidate` means "autowire would succeed."** It does *not* mean "exactly one bean." It matches when by-type injection would resolve unambiguously — so a `@Primary` among several candidates, or `autowireCandidate`/`defaultCandidate` filtering, can make it match even when the raw count exceeds one.

> **Hidden concept — class-level `OnBean` still creates the `@Configuration` object.** A class-level `@ConditionalOnBean`/`OnMissingBean` does not prevent the `@Configuration` class from being instantiated/parsed; it only prevents it being registered as a bean. Side effects in such a class are not necessarily suppressed.

> **Hidden concept — boolean logic needs nested-condition helpers.** Stacked `@Conditional` annotations are *pure AND*. To express OR/AND/NOR you must subclass `AnyNestedCondition`, `AllNestedConditions`, or `NoneNestedConditions` and declare member conditions on inner methods — choosing the `ConfigurationPhase` in the constructor.

> **Hidden concept — lesser-known annotations.** Beyond the famous ones: `@ConditionalOnThreading` (platform vs virtual threads), `@ConditionalOnCheckpointRestore` (Project CRaC), `@ConditionalOnJndi`, `@ConditionalOnMissingFilterBean`, and the container annotations `@ConditionalOnProperties`/`@ConditionalOnBooleanProperties`.

**Changed in v4/v7:** `@ConditionalOnBooleanProperty` (with its container `@ConditionalOnBooleanProperties`) is a boolean-specific complement to `@ConditionalOnProperty` — it requires the property to be *present and equal to `true`* by default (introduced in the 3.x line, standard in 4.x). Some annotations were also **renamed for consistency**, e.g. `@ConditionalOnEnabledTracing` → `@ConditionalOnEnabledTracingExport` (paired with the property `management.tracing.enabled` → `management.tracing.export.enabled`).

### `ConditionEvaluationReport`: making "magic" debuggable

Every Boot `ConditionOutcome` carries a human-readable `ConditionMessage` ("did not find property", "found different value in property", "`@ConditionalOnMissingBean` found beans of type X"). These are recorded into a `ConditionEvaluationReport` with `positiveMatches`, `negativeMatches`, `exclusions`, and `unconditionalClasses`, dumped at startup with `--debug` and exposed as structured JSON via the actuator `/actuator/conditions` endpoint.

The **philosophy** is that auto-configuration is "magic" that must remain *accountable*: the framework owes the developer a clear, machine-readable account of every match/no-match with a reason. This converts an opaque "why is my bean missing?" into a lookup, preserving trust in heavy convention-over-configuration. A useful corollary for authors: writing custom conditions with good `ConditionMessage` strings makes them debuggable *for free*, since those strings *are* the report output.

### The core philosophy: declarative, composable BACKOFF

The unifying design value is **opinionated defaults that never fight the user**. By guarding every auto-configured bean with `@ConditionalOnMissingBean` and ordering auto-config strictly *after* user config, the user's bean silently wins with zero explicit disabling. This *inverts* the typical framework default that requires opt-out flags. The rejected alternatives — profiles/flags to toggle features, or last-wins bean overriding (disabled by default since Boot 2.1) — are either verbose or surprising; conditional backoff is declarative and self-documenting.

This is also why Boot scopes `@ConditionalOnBean`/`OnMissingBean` to **auto-configuration classes only**: because they are order-sensitive, using them on ordinary user `@Configuration` gives undefined results (you cannot know what's been processed yet). Auto-config is guaranteed to load *last*, so it is the one place where "what has been defined so far" is a stable, meaningful question. The API is *teaching*: reason about ordering only where ordering is guaranteed.

---

### `@EnableAutoConfiguration` and `@SpringBootApplication` wiring

`@EnableAutoConfiguration` is meta-annotated with `@AutoConfigurationPackage` and `@Import(AutoConfigurationImportSelector.class)`. It declares `exclude()`/`excludeName()` and the constant `ENABLED_OVERRIDE_PROPERTY = "spring.boot.enableautoconfiguration"` — a global kill-switch. Its javadoc states the whole creed: auto-configuration "tries to be as intelligent as possible and will back-away as you define more of your own configuration ... always applied after user-defined beans have been registered."

`@SpringBootApplication` is a composed annotation equal to `@SpringBootConfiguration` + `@EnableAutoConfiguration` + `@ComponentScan`. Crucially, its `@ComponentScan` declares two `excludeFilters`: `TypeExcludeFilter` and `AutoConfigurationExcludeFilter`.

> **Hidden concept — `AutoConfigurationExcludeFilter`.** This is *why* `@SpringBootApplication` won't double-register an auto-config that happens to sit in a scanned package. It matches classes that are `@Configuration` **and** (`@AutoConfiguration`-annotated or listed in the `.imports` set) and excludes them from component scanning — so they enter the context *only* through the import selector, exactly once.

> **Hidden concept — `@AutoConfigurationPackage`.** `@EnableAutoConfiguration` silently registers the annotated class's package as the "default" base package. JPA `@Entity` scanning and Spring Data repository scanning default to *this* package — which is *why* your main class should sit in a root package. It is distinct from `@ComponentScan`'s `basePackages`.

### `@AutoConfiguration`: a lite configuration by mandate

`@AutoConfiguration` is meta-annotated with `@Configuration(proxyBeanMethods = false)` + `@AutoConfigureBefore` + `@AutoConfigureAfter`. Its javadoc: "Auto-configuration classes are regular `@Configuration` with the exception that `proxyBeanMethods` is always `false`."

The **design rationale** for hardwiring lite mode is decisive. Boot has hundreds of auto-config classes, and CGLIB-subclassing every one at startup is "potentially quite an expensive operation" (issue #9068, Phil Webb). Auto-config classes already wire everything via `@Bean` method *parameters* and never call `@Bean` methods directly, so full-mode interception buys them nothing. Hardcoding `proxyBeanMethods=false` yields faster startup, smaller footprint, better GraalVM-native compatibility, and removes the `final`/`private` constraints. The deeper intent: *library/framework code must be fast and native-friendly by default and should model exemplary style.* Boot removed the foot-gun entirely — auto-config authors *literally cannot* rely on method-call interception, which enforces the parameter-injection discipline ecosystem-wide.

The `@AutoConfiguration` annotation also carries `before`/`beforeName`/`after`/`afterName` attributes that `@AliasFor` the corresponding `@AutoConfigureBefore`/`@AutoConfigureAfter` values — so ordering can be expressed *inline*.

> **Hidden concept — you rarely need the standalone ordering annotations.** Practitioners often don't realize that the single `@AutoConfiguration` annotation already carries all four ordering attributes; the separate `@AutoConfigureBefore`/`@AutoConfigureAfter` are seldom necessary now.

### The `AutoConfiguration.imports` file

The candidate file is exactly `META-INF/spring/org.springframework.boot.autoconfigure.AutoConfiguration.imports` (the resource name is the fully-qualified marker-annotation name + `.imports`). It holds one fully-qualified class name per line; `#` starts a comment; nested classes use `$` (`com.example.Outer$NestedAutoConfiguration`). It is read by `ImportCandidates.load(annotation, classLoader)`, which scans **all** jars on the classpath via `classLoader.getResources(...)`, so every library contributes its own file and duplicates are de-duped.

This file **replaced the `spring.factories` `EnableAutoConfiguration` key** (deprecated in Boot 2.7, removed in 3.0 — *not* a v4 change). The **design rationale**: `spring.factories` was a generic multi-key properties file shared by everything, slow to parse and opaque in overloading one key for auto-config. A single-purpose, one-class-per-line file is faster to read, easier to tool and index, and clearly separates "candidate auto-configs" from other factory types. Component scanning was rejected because auto-configs live in libraries and must *not* be discovered transitively or duplicated — hence `AutoConfigurationExcludeFilter`.

> **Hidden concept — `SpringFactoriesLoader` is still in play.** Only the candidate *list* moved to `.imports`. `AutoConfigurationImportFilter` and `AutoConfigurationImportListener` implementations are *still* loaded via `SpringFactoriesLoader`.

### `AutoConfigurationImportSelector`: the deferred engine

`AutoConfigurationImportSelector` implements `DeferredImportSelector` (plus `BeanClassLoaderAware`/`ResourceLoaderAware`/`BeanFactoryAware`/`EnvironmentAware`/`Ordered`, with `ORDER = LOWEST_PRECEDENCE - 1`). "Deferred" means its `@Import` is *not* processed during the normal pass over `@Configuration` classes — it runs *after* all regular configuration is parsed, which is precisely what guarantees auto-config sees the final set of user beans. The pipeline in `getAutoConfigurationEntry()`:

```
getCandidateConfigurations (ImportCandidates)
  -> removeDuplicates
  -> getExclusions
  -> checkExcludedClasses
  -> remove exclusions
  -> ConfigurationClassFilter.filter
  -> fire import events
```

Its `getImportGroup()` returns `AutoConfigurationGroup`. The group's `process()` is called once per importing selector to collect `AutoConfigurationEntry` objects; `selectImports()` then unions all configurations, subtracts all exclusions, and runs `AutoConfigurationSorter.getInPriorityOrder` over the *whole* set before emitting ordered `Entry(metadata, className)` pairs. Grouping makes ordering and de-duplication global rather than per-import, registering all auto-config as a single ordered batch.

> **Hidden concept — the `ENABLED_OVERRIDE_PROPERTY` kill-switch.** `spring.boot.enableautoconfiguration=false` disables *all* auto-configuration at once (checked in `isEnabled`), but only for the stock selector — subclasses always return enabled. Rarely used outside diagnostics.

### Two-phase condition filtering (the fast path)

`AutoConfigurationImportFilter` implementations — chiefly `OnClassCondition`, `OnBeanCondition`, `OnWebApplicationCondition` — pre-filter candidates using `AutoConfigurationMetadata`, generated at *build time* by an annotation processor into `META-INF/spring-autoconfigure-metadata.properties`. This evaluates `@ConditionalOnClass` and friends via ASM bytecode metadata *without loading* the candidate classes, dropping non-matching auto-configs cheaply before the expensive `@Configuration` parse.

The **design rationale**: there are hundreds of candidates but a given app needs few. Evaluating `@ConditionalOnClass` by actually loading classes would trigger massive classloading and `NoClassDefFoundError`s for absent optional deps. Build-time metadata + ASM discards most candidates without loading them — keeping startup fast and robust against missing classes. This is exactly why `@ConditionalOnClass` can safely name a class that isn't on the classpath.

> **Hidden concept — build-time `AutoConfigurationMetadata`.** This generated `.properties` file lets the `OnClassCondition` filter and `AutoConfigurationSorter` read conditions/order/before/after *without* ASM-reading each class at runtime — a major startup optimization most developers never see.

The recommended authoring pattern combines both phases: nest a `@Configuration(proxyBeanMethods=false)` guarded by `@ConditionalOnClass(X.class)`, and put `@Bean @ConditionalOnMissingBean` inside. Class conditions guard the nested class (safe — the return type is only referenced inside), bean conditions guard the methods.

### Ordering: `AutoConfigurationSorter`

`getInPriorityOrder` runs three passes: (1) sort **alphabetically** by class name (a stable, deterministic base); (2) stable-sort by **`@AutoConfigureOrder`** (`DEFAULT_ORDER = 0`, same semantics as `@Order` but a dedicated annotation); (3) **topological sort** honoring `@AutoConfigureBefore`/`@AutoConfigureAfter`, with explicit cycle detection ("AutoConfigure cycle detected between X and Y"). It reads order/before/after from build-time metadata when available, else from ASM annotation metadata.

> **Hidden concept — ordering affects *definition* order, not *creation* order.** `@AutoConfigureAfter` only guarantees that bean *definitions* are added in that order (so conditions evaluate correctly). The actual instantiation order is still driven by dependencies and `@DependsOn`.

### Exclusions

`getExclusions()` merges three sources: the `exclude=` and `excludeName=` attributes (from `@SpringBootApplication`/`@EnableAutoConfiguration`) plus the `spring.autoconfigure.exclude` environment property (bound via `Binder`, with relaxed binding and list support). `checkExcludedClasses` throws `IllegalStateException` if you try to exclude a class that is on the classpath but is *not* actually an auto-configuration class — a typo guard.

### `@ImportAutoConfiguration` and test slices

`@ImportAutoConfiguration` is `@Import(ImportAutoConfigurationImportSelector.class)`. It applies the *same ordering rules* as `@EnableAutoConfiguration` but restricts the set to an explicitly listed `classes()`/`value()` rather than consulting the full `ImportCandidates` list. When `classes` is empty, it reads a per-annotation file `META-INF/spring/<fully-qualified-annotated-class-name>.imports`, where entries may be prefixed `optional:` to be skipped if absent. This is the engine behind test slices (`@WebMvcTest`, `@DataJpaTest`, etc.), which need a curated subset, not the whole app's auto-config.

### The contract: names are public API, contents are not

A deliberate boundary: a class *name* and its *ordering relationships* are the contract you may rely on (to exclude or order); nested classes, `@Bean` methods, and fields are internal, so Boot stays free to refactor innards without breaking users.

**Changed in v4/v7:** Boot 4.0 (GA **November 20, 2025**, atop Spring Framework 7.0 GA November 13, 2025) **shattered the monolithic `spring-boot-autoconfigure` jar** — which had grown from 182 KiB (Boot 1.0) to ~2 MiB (3.5) — into ~47 per-technology modules. Most technology auto-config packages *relocated* from `org.springframework.boot.autoconfigure.*` to module roots like `org.springframework.boot.webmvc` / `org.springframework.boot.jdbc.autoconfigure` (e.g. `DataSourceAutoConfiguration` moved to `org.springframework.boot.jdbc.autoconfigure`, `WebMvcAutoConfiguration` to `org.springframework.boot.webmvc.autoconfigure`). The **design rationale** (per the "Modularizing Spring Boot" blog): make module boundaries *contracts rather than soft conventions*, shrink runtime/native footprint, sharpen IDE auto-complete, and ease security review.

Important nuances on this split:
- The **condition annotations and infrastructure remain** in `org.springframework.boot.autoconfigure.condition` (the core `spring-boot-autoconfigure` module persists); *only the technology auto-configs relocated*.
- The annotation FQNs `@SpringBootApplication` and `@EnableAutoConfiguration` are **unchanged** — both still live in `org.springframework.boot.autoconfigure` (in the single `core/spring-boot-autoconfigure` module), exactly as in Boot 3.x. They did not change package at all.

**Changed in v4/v7:** A new **`spring-boot-autoconfigure-classic`** module (and "classic" starter POMs) bundles the modular auto-config modules *without their reactive transitive dependencies*, as a deliberate migration aid.

**Changed in v4/v7:** **Public members (other than constants) were removed from auto-configuration classes**, and members of configurations imported by auto-configs were made package-private — enforcing via Java access control what was previously only documented: you observe auto-config *effects* (via the conditions report), you do not subclass them.

**Changed in v4/v7:** `AutoConfigurationImportSelector` gained an **`AutoConfigurationReplacements`** mechanism (loaded per marker annotation, applied in `getExclusions` and in the sorter's before/after mapping, with an `AutoConfiguration.replacements` resource for renamed classes) so the v4 package relocations don't break existing `exclude=` declarations or before/after references.

**Changed in v4/v7:** The Boot 4 / Framework 7 baseline is **Java 17** (full support through Java 25), **Spring Framework 7.x**, **Jakarta EE 11 / Servlet 6.1**, **Kotlin 2.2+**; native image requires GraalVM 25+. Note that `@Bean(bootstrap = Bootstrap.BACKGROUND)` background initialization is a 6.2+ feature carried into 7.x, *not* new in 7.0.

> **Continuity, not change:** `DeferredImportSelector` and its `Group`/`selectImports` contract are stable from Framework 6; Boot 4 relies on the same grouping/deferral semantics. The whole mechanism — defer, filter cheaply via ASM, sort, register as lite configs with `@ConditionalOnMissingBean` — is intact. What changed is packaging and access-control enforcement, not the engine.


---

## Environment, Profiles & Properties

Spring's `Environment` abstraction is the single answer to one question: *what is true about where this artifact is running?* It models exactly two facets of the runtime — **profiles** (which named groups of bean definitions are active) and **properties** (key/value resolution through an ordered source stack). Everything in this section is an elaboration of those two facets and the philosophy that binds them: *build the artifact once, configure it per environment*. The same jar or container image should assemble a different object graph and tune it with different values across dev, CI, staging, and prod, with zero rebuilds. Profiles answer "which beans exist"; properties answer "how those beans are tuned"; `@ConfigurationProperties` projects properties onto strongly-typed objects.

**Changed in v4/v7 (baseline context, not a semantic change):** Spring Boot 4.0 GA'd on November 20, 2025, on top of Spring Framework 7.0 (GA November 13, 2025). The baseline rose to Java 17 (Java 21/25 recommended), Jakarta EE 11, and Servlet 6.1; all `javax.*` is gone, so JNDI/servlet-backed property sources now operate over `jakarta.*` APIs. The monolithic `spring-boot-autoconfigure` jar was split into focused per-technology modules with their own starters — this does not change property-source mechanics but changes *which* `@ConfigurationProperties` are on the classpath. `EnvironmentPostProcessor` moved from `org.springframework.boot.env` to `org.springframework.boot`, and `BootstrapRegistry` moved from `org.springframework.boot` to `org.springframework.boot.bootstrap` (deprecated old forms linger for upgrade ease — custom EPPs registered in `META-INF/spring.factories` must update their imports). Crucially, the profile and property *mechanisms themselves* are materially identical to 6.x/3.x; the redesign people remember (Config Data) landed back in Boot 2.4.

---

### The Environment: profiles + properties in one abstraction

`org.springframework.core.env.Environment` extends `PropertyResolver` and adds `getActiveProfiles()`, `getDefaultProfiles()`, and `acceptsProfiles(Profiles)`. The container owns exactly one `ConfigurableEnvironment`, and it is reachable everywhere (`EnvironmentAware`, or simply `@Autowired Environment`).

**Design intent — why unify the two.** Profiles and properties both answer "what is true about where I'm running," so Spring deliberately models them in one place. The payoff is conceptual: the *same* mechanism drives both — `spring.profiles.active` is itself just a property — and placeholder resolution can feed profile decisions. Developers get one mental anchor (the `Environment`) for all deployment-context reasoning rather than two parallel systems.

#### Read vs. mutate: PropertyResolver vs. ConfigurableEnvironment

The API is split along a least-privilege seam.

- **`PropertyResolver`** is the *read* interface: `getProperty(key)`, `getProperty(key, defaultValue)`, `getProperty(key, Class<T>)`, `getRequiredProperty`, `containsProperty`, `resolvePlaceholders` / `resolveRequiredPlaceholders`.
- **`ConfigurableEnvironment`** is the *mutate* interface: `setActiveProfiles`, `addActiveProfile`, `setDefaultProfiles`, `getPropertySources()` (returns `MutablePropertySources`), `getSystemProperties` / `getSystemEnvironment`, and `merge(parent)`.

`PropertySourcesPropertyResolver` is the concrete resolver that walks the source list.

**Design intent.** Application code should *consume* configuration, never reshape the source ordering; infrastructure code (boot startup, `EnvironmentPostProcessor`) needs to mutate it. By handing beans a `PropertyResolver`/`Environment` view while reserving `ConfigurableEnvironment` for the bootstrap layer, Spring prevents business code from accidentally reordering precedence — the override semantics stay an infrastructure concern.

---

### PropertySource and the "first source wins" rule

A `PropertySource<T>` is a named wrapper over any key/value backing object — a `Properties`, a `Map`, `System.getenv()`, a servlet context. The name is its identity, used by `addBefore`/`addAfter`/`replace`/`remove` and by parent-environment merging. Concrete subtypes include `MapPropertySource`, `PropertiesPropertySource`, `SystemEnvironmentPropertySource` (which performs relaxed `UPPER_SNAKE` matching for env vars), `CommandLinePropertySource`, and the enumerable variant `EnumerablePropertySource`.

The `Environment` holds these in an ordered `MutablePropertySources`. Resolution **iterates in order and returns the first source that contains the key** — earlier entries have higher precedence. The mutation API is explicit about ordering: `addFirst()` (highest), `addLast()` (lowest), `addBefore(name, src)`, `addAfter(name, src)`, `replace`, `remove`.

```java
ConfigurableEnvironment env = ctx.getEnvironment();
env.getPropertySources().addFirst(
    new MapPropertySource("override", Map.of("app.mode", "maintenance")));
// app.mode now resolves to "maintenance" regardless of lower sources
```

`AbstractEnvironment.customizePropertySources(MutablePropertySources)` is the protected hook subclasses override to seed defaults. Plain `StandardEnvironment` seeds two: `systemProperties` (`System.getProperties()`) **first**, then `systemEnvironment` (`System.getenv()`) — so JVM `-D` system properties win over OS env vars by default. `StandardServletEnvironment` prepends `servletConfig`, `servletContext`, and `jndiProperties` ahead of those.

**Design intent — determinism over merging.** Spring deliberately *never blends* values across sources. A property has exactly one effective value with exactly one traceable origin. The mental model it instills: configuration is a **stack of layers**, and you reason about overrides by asking "which layer is on top." The rejected alternative — deep-merging key by key across sources — would create ambiguous provenance and surprising partial overrides. The framework docs are explicit that a preceding source's value *entirely replaces* a later one; there is no key-level deep merge.

#### Hidden concept: `spring.getenv.ignore` and the parent/child merge rule

`IGNORE_GETENV_PROPERTY_NAME` (`spring.getenv.ignore=true`) is a rarely-used escape hatch that suppresses `System.getenv()` access entirely (and its security-manager warnings), effectively removing the system-environment source.

When an `ApplicationContext` has a parent, `AbstractEnvironment.merge(parent)` keeps the **child's** instance for any identically-named `PropertySource` and discards the parent's, plus de-duplicates profile names — so the child overrides and common sources (system props/env) aren't searched twice. **Changed in v4/v7:** the Framework docs now *emphasize* this child-wins merge semantics for hierarchical contexts and tests, though the behavior itself is not new.

---

### Profiles: conditionally-registered bean groups

A profile is a named logical group of beans registered only when active. The key insight to internalize:

#### `@Profile` *is* a `@Conditional`

`@Profile` is meta-annotated `@Conditional(ProfileCondition.class)` and has been since Spring 3.1. It is **not** a special-cased container feature — it is ordinary condition evaluation. `ProfileCondition` reads the `@Profile` value array and calls `Environment.acceptsProfiles(Profiles.of(...))`.

**Design intent.** Profiles (3.1) actually *predate* the general `@Conditional` SPI (4.0). When the generic mechanism arrived, Spring retrofitted `@Profile` onto it rather than maintaining two parallel engines. The intent is conceptual economy: you learn one extensibility model (`Condition`/`ConditionContext`), and profiles are simply its most ergonomically-named preset. Custom `@Conditional` logic and `@Profile` then compose uniformly during bean-definition filtering.

**Where `@Profile` can sit.** Type-level on `@Component`/`@Configuration`, method-level on individual `@Bean` methods, or as a meta-annotation for custom stereotypes (e.g. `@Production`). On a `@Bean` method it gates that one bean; on a `@Configuration` class it gates all contained `@Bean` methods. For `@ConfigurationProperties`: if registered via `@EnableConfigurationProperties`, `@Profile` must sit on the `@Configuration` class; if component-scanned, it can sit on the `@ConfigurationProperties` class itself.

The deeper philosophy is **environment-varying wiring**: the same artifact assembles a *structurally different* object graph (embedded HSQL `DataSource` in dev, JNDI lookup in prod) without recompilation. Profiles express the 12-factor "build once, configure per environment" principle structurally, not just through key/value config.

#### Profile expression grammar

A `@Profile` value is either a simple name (`"prod"`) or an expression with operators `!` (NOT), `&` (AND), `|` (OR).

**Critical rule:** `&` and `|` may **not** be mixed without parentheses. `"a & b | c"` is invalid and must be written `"(a & b) | c"` or `"a & (b | c)"`.

```java
@Profile("production & (us-east | eu-central)")  // valid
@Profile("production & us-east | eu-central")     // INVALID — throws
```

**Design intent — fail fast on ambiguity.** Rather than baking in operator-precedence rules that every developer would have to memorize (and that differ across languages), Spring refuses ambiguous expressions outright. It trades a little verbosity for zero ambiguity: the expression reads exactly as it evaluates. This reflects a recurring Spring API value — refuse to guess intent.

#### Hidden concept: the array form is OR, and negation matches absence

`@Profile({"p1", "p2"})` is an **implicit OR** — it registers if `p1` OR `p2` is active. Practitioners routinely expect AND; to require both, use a single expression string `@Profile("p1 & p2")`.

Negation matches *absence*: `@Profile("!prod")` is true whenever `prod` is simply not in the active set — including the all-default case. There is no "unknown" state; absent equals not-active for negation purposes.

#### The `Profiles` value object and the Environment API

`Profiles.of(String...)` (since 5.1) builds a `Profiles` predicate that matches if **any** of the supplied expressions matches — easy to misread as requiring all to match. `Profiles.matches(Predicate<String> isProfileActive)` evaluates the expression tree against active-profile membership.

On the `Environment`:
- `matchesProfiles(String...)` (since 5.3.28) is the modern entry point — a shortcut for `acceptsProfiles(Profiles.of(...))`.
- `acceptsProfiles(Profiles)` (since 5.1).
- `acceptsProfiles(String...)` is `@Deprecated(since="5.1")` in favor of the two above, because the `String...` overload ambiguously conflated "list of simple names" with "profile expressions." Splitting into `acceptsProfiles(Profiles)` and `matchesProfiles(String...)` makes the expression-vs-name distinction explicit at the type level.

**Changed in v4/v7:** `Environment.acceptsProfiles(String...)` remains `@Deprecated(since="5.1")` in Framework 7.0 and has **not** been removed — even though 7.0, a major version, removed many other long-deprecated APIs. This reflects Spring's long-deprecation-window compatibility ethos: keep a widely-used method rather than churn it. (The Boot 4 reference docs nudge users toward the `Profiles`/expression API; prefer `matchesProfiles(String...)` or `acceptsProfiles(Profiles)`.)

> Note: the relevant constants live on `AbstractEnvironment`: `ACTIVE_PROFILES_PROPERTY_NAME` = `"spring.profiles.active"`, `DEFAULT_PROFILES_PROPERTY_NAME` = `"spring.profiles.default"`, `RESERVED_DEFAULT_PROFILE_NAME` = `"default"`.

#### The reserved `default` profile

If no profile is explicitly active, a fallback profile named literally `"default"` is activated, and `@Profile("default")` beans register. The name is configurable via `ConfigurableEnvironment.setDefaultProfiles(...)` or the `spring.profiles.default` property.

**Override semantics — and a notorious gotcha.** The moment *any* explicit active profile exists, the default set is ignored *entirely*. It is a fallback, not an always-on baseline. The hidden trap: activating even one unrelated profile (e.g. `metrics`) silently disables every `@Profile("default")` bean — a frequent source of "my default `DataSource` vanished" surprises.

**Design intent.** The fallback avoids the awkward zero-profiles-active state where *nothing* gets wired, guaranteeing a baseline. Making it a fallback rather than always-on prevents accidental *double*-wiring of default + environment beans. **Changed in v4/v7 (confirmed unchanged):** `spring.profiles.default` still defaults to `"default"` in Boot 4.x; setting it to e.g. `none` remains the documented way to effectively disable default-profile wiring.

#### Activation: programmatic and declarative

**Framework (programmatic):** `setActiveProfiles(String...)` / `addActiveProfile(String)` / `setDefaultProfiles(String...)`, typically before refresh:

```java
ctx.getEnvironment().setActiveProfiles("development");
ctx.refresh();
```

**Boot adds** `SpringApplication.setAdditionalProfiles(...)`, which *augments* rather than replaces, applied before the environment is fully prepared.

**Declarative (Boot):** `spring.profiles.active` (comma-separated) in `application.properties`/yaml, on the command line as `--spring.profiles.active=dev,hsqldb`, or as `-Dspring.profiles.active`. The `SPRING_PROFILES_ACTIVE` environment variable is the relaxed-binding equivalent. Standard `PropertySource` ordering applies — a command-line switch can replace what a file declared.

#### Three distinct levers: active, include, group

These encode three *different authorial intents*, deliberately kept as separate knobs rather than overloading one:

- **`spring.profiles.active`** = the externally chosen environment (replaceable by ops).
- **`spring.profiles.include`** = profiles the app *insists on* regardless of environment (cross-cutting, e.g. `common`). Included profiles are added **before** the explicitly-active ones, and — hidden concept — processing is **per-property-source, not list-merged**: normal collection-merge/last-wins relaxed-binding rules do *not* apply, so you cannot rely on a higher source "extending" a lower source's include list the way you can for ordinary list properties. **Restriction (Boot 2.4+):** `include` may not appear in a profile-specific document.
- **`spring.profiles.group`** = a logical alias that fans out:

```properties
spring.profiles.group.production=proddb,prodmq
# activating "production" also activates proddb and prodmq
```

This lets ops toggle one coarse, human-meaningful profile while developers keep fine-grained ones. Separating the three keeps each property's semantics predictable.

#### XML `<beans profile="...">`

The original pre-annotation form. A nested `<beans profile="dev">` block registers its beans only when `dev` is active, accepting the same `!`/`&`/`|` grammar (negation as `profile="!production"`). **Still supported in Framework 7.**

#### Hidden concept: conditions evaluate *before* instantiation

Because `@Profile` is a `@Conditional`, a profile-excluded `@Bean` method's return type never even becomes a bean definition. It is invisible to autowiring, to `@Autowired(required=...)`, and to `BeanFactoryPostProcessor`s — not merely lazy or uninstantiated. Relatedly, class-level `@Profile` short-circuits all nested `@Bean` methods: a method-level `@Profile` cannot *re-enable* a method if the enclosing `@Configuration` was itself filtered out by a conflicting class-level condition.

#### Profile name validation

**Boot** validates profile names by default (`spring.profiles.validate=true`): allowed characters are letters, numbers, and `- _ . + @`, and a name must start and end with a letter or number. Set `spring.profiles.validate=false` to relax. This is a Boot-layer guard against parsing ambiguities — e.g. a literal name colliding with the expression operators. **Changed in v4/v7 (confirmed unchanged):** still the Boot default in 4.x exactly as in 3.x.

---

### Boot's externalized configuration precedence

On top of Framework's first-wins iteration, Boot fixes a long, opinionated precedence order. Listed **lowest → highest** (later overrides earlier — the mirror image of Framework's first-wins iteration):

1. `SpringApplication.setDefaultProperties(Map)`
2. `@PropertySource` on `@Configuration`
3. Config Data files (`application.properties`/yaml and profile variants)
4. `RandomValuePropertySource` (`random.*`)
5. OS environment variables
6. Java system properties
7. JNDI attributes from `java:comp/env`
8. `ServletContext` init params
9. `ServletConfig` init params
10. `SPRING_APPLICATION_JSON` / `spring.application.json`
11. Command-line arguments (`--key=value`)
12. `properties` attribute on `@SpringBootTest`/slice annotations
13. `@DynamicPropertySource`
14. `@TestPropertySource`
15. Devtools global settings in `$HOME/.config/spring-boot`

**Design intent — specificity and intentionality rise to the top.** The order encodes a deployment philosophy: things known *later / closer to runtime* override things known *earlier / at build time*. Code defaults are weakest; packaged files override defaults; external files override packaged; machine config (env vars, system props) overrides files; explicit invocation (command-line, SAJ) overrides everything operational; tests override everything. One immutable artifact thus behaves correctly across all environments — the core 12-factor goal. The specific choice to put **OS env vars *below* Java system properties and command-line args** follows the same axis: env vars are ambient and broad (set once per host/container), `-D` is per-JVM-launch, `--args` are per-invocation and most explicit, so an operator's one-off override always wins over inherited ambient config.

**Changed in v4/v7:** the 15-step ordering, first-wins iteration, `SPRING_APPLICATION_JSON` behavior, origin tracking, and relaxed binding all carried over from Boot 3.x essentially intact — no precedence changes.

#### Hidden concept: `@PropertySource` is too late, and null never overrides

`@PropertySource` sources are added during context *refresh*, so they sit just above `setDefaultProperties` at the **bottom** of precedence — processed after logging and `SpringApplication` bootstrapping. Setting `logging.level.*` or `spring.main.*` via `@PropertySource` silently has no effect; use `application.properties` or an `EnvironmentPostProcessor`. (`@PropertySource` also cannot load YAML or multi-document files; it *is* `@Repeatable` and supports `${...}` placeholders in its location.)

`PropertySourcesPropertyResolver` treats a **null value as "property absent."** A `null` in `SPRING_APPLICATION_JSON` (or any source) cannot mask a non-null value in a lower-precedence source — absence is the only fall-through mechanism. **Design intent:** allowing null to blank out a lower layer would make "unset" and "set to null" indistinguishable and cause accidental erasure; Spring keeps override semantics monotonic so `null` never overrides.

---

### The Config Data API

Pre-2.4 Boot used a separate "bootstrap" context plus `EnvironmentPostProcessor`s with confusing profile-specific ordering. The Config Data engine replaced that with a deterministic, single-pass, *declarative* model.

#### File discovery and ordering

Beyond `application.{properties,yaml}`, Boot loads `application-{profile}.{properties,yaml}` for each active profile from the same locations. Within the Config Data layer the order (low → high) is:

1. `application.*` packaged in the jar
2. profile-specific `application-{profile}.*` in the jar
3. `application.*` outside the jar
4. profile-specific `application-{profile}.*` outside the jar

Default search locations: classpath root, classpath `/config`, current dir, `./config/`, and `./config/*/` subdirectories. **Hidden concept:** profile-specific *always* overrides non-specific, and with multiple active profiles a **last-wins strategy applies at the location-GROUP level** — with `spring.profiles.active=prod,live`, `application-live` overrides `application-prod`, resolved per config-location group during the deterministic import traversal, *not* by textual file order on disk. If both `.properties` and `.yaml` exist in the same location, **`.properties` wins**.

#### name / location / additional-location

- `spring.config.name` changes the base filename (default `application`).
- `spring.config.location` **replaces** the default search locations.
- `spring.config.additional-location` **adds** locations while keeping defaults.

**Hidden concept:** all three are consumed *extremely early*, before files are located — so they only work as an env var, system property, or command-line arg; placing them inside `application.properties` is silently ignored. Use the `optional:` prefix to tolerate missing locations (otherwise `ConfigDataLocationNotFoundException`), or `spring.config.on-not-found=ignore` globally. A `*` wildcard is allowed in the last segment for external dirs only, sorted alphabetically.

#### spring.config.import

Declared inside a config file, `spring.config.import` pulls in further documents, inserted immediately **below** (higher precedence than) the importing document.

```yaml
spring:
  config:
    import: "optional:configtree:/etc/config/,extra/extra.properties"
```

**Hidden concepts:** an import is **idempotent** — imported once no matter how many times declared, keeping its position from first discovery. Locations are either **fixed** (start with `/` or a URL prefix like `file:`/`classpath:`) or **import-relative** (resolved relative to the declaring file); the `optional:` prefix is stripped before that determination, and chained imports compound relatively. Bracketed attributes give hints: `[encoding=utf-8]`, `[extension=.yaml]`, extensionless forms like `myconfig[.yaml]`. Default file charset is ISO-8859-1.

#### env: and configtree: prefixes — your platform's config *is* a property source

- `spring.config.import=env:MY_CONFIG` parses a (possibly multiline) environment variable as a properties/yaml document.
- `spring.config.import=configtree:/etc/config/` treats a directory tree as config: each file's path becomes the key, its contents the value — purpose-built for Kubernetes ConfigMap/Secret volume mounts and Docker secrets (`/run/secrets/`). Dotted filenames map directly (`myapp.username` → property `myapp.username`); `configtree:/etc/config/*/` aggregates multiple trees alphabetically.

**Design intent.** Cloud-native platforms expose config as mounted files or large env vars. Rather than forcing glue code, Boot natively maps a directory-of-files or a multiline env var into the property namespace. The mental model: *your platform's native config mechanism is already a property source — no adapter required.*

#### Multi-document files and `spring.config.activate.on-profile`

A single `application.yaml` can hold ordered logical documents split by `---` (or `#---` / `!---` in `.properties`), processed in order with later overriding earlier. `spring.config.activate.on-profile` (a profile expression) and `spring.config.activate.on-cloud-platform` conditionally enable a document — the Boot-level analogue of `@Profile` for property documents.

```yaml
spring:
  config:
    activate:
      on-profile: "prod & !legacy"
my.datasource.url: "jdbc:postgresql://prod-db/app"
```

**Hard rule (Boot 2.4+):** a document carrying `spring.config.activate.on-profile` may **not** itself set `spring.profiles.active` / `.include` / `.group` / `.default` — doing so raises `InvalidConfigDataPropertyException` / `InactiveConfigDataAccessException`. This replaced the legacy pre-2.4 `spring.profiles` document key.

**Design intent — decide profiles up front, then filter.** Pre-2.4, a profile-specific document could activate *further* profiles, producing order-dependent, hard-to-reason-about cascades during loading. The Config Data redesign made loading a deterministic single-pass traversal; banning self-activation removes circular/late-activation paradoxes. The mental model Spring wants: *profiles are decided up front, then documents are filtered — not mutually recursive.*

#### Hidden concept: two profile layers that share one active set

`spring.config.activate.on-profile` gates which **property documents** load (a Config Data concern); `@Profile` gates which **bean definitions** register (a Framework concern). They share the active-profile set but run at *different phases*: properties are resolved first, then beans are filtered. Conflating them is a common source of confusion.

#### RandomValuePropertySource and SPRING_APPLICATION_JSON

`RandomValuePropertySource` responds only to `random.*` keys, consulted via placeholders (not bound directly), sitting just above the Config Data files:

```properties
my.secret=${random.value}
my.number=${random.int}
my.id=${random.uuid}
my.bounded=${random.int(10)}
my.ranged=${random.int[1024,65536]}   # OPEN value(,max) CLOSE; max exclusive
```

`SPRING_APPLICATION_JSON` (also via system property, command-line arg, or JNDI) flattens a JSON blob into the Environment — `{"my":{"name":"test"}}` → `my.name=test` — sitting just below command-line args. As above, JSON `null` is treated as missing and cannot mask lower sources.

---

### Origin tracking

Boot wraps loaded values in `OriginTrackedValue` (value + `Origin`) inside `OriginTrackedMapPropertySource`, and sources implement `OriginLookup<String>` to answer "where did this key come from." `TextResourceOrigin` records file + line/column; `PropertySourceOrigin` records the owning source. The package is `org.springframework.boot.origin`.

**Design intent.** In a 15-layer precedence stack, "*why* is this value X?" is the hardest operational question. Origin tracking turns the Environment from an opaque map into a self-documenting structure, powering precise error messages ("Property X was set in `application.yaml` line 12"), the `/actuator/env` endpoint, and binding/validation diagnostics. The philosophy: *configuration should be debuggable, and provenance is a first-class concern.* **Changed in v4/v7 (confirmed unchanged):** the `org.springframework.boot.origin` package and its mechanics carried over from 3.x intact.

---

### Relaxed binding

`@ConfigurationProperties` binding tolerates naming variance: kebab-case (`context-path`), camelCase (`contextPath`), snake_case (`context_path`), and `UPPER_SNAKE` env vars (`CONTEXT_PATH`) all bind to one canonical property. The canonical form is lowercase **kebab-case**.

**The internal model (the real reason it works).** A property name is a `ConfigurationPropertyName` with three comparison *Forms*: `ORIGINAL` (as written), `DASHED` (lowercase alphanumeric + dashes), and `UNIFORM` (lowercase alphanumeric only — `foo-bar`, `foo_bar`, `fooBar` all → `foobar`). Matching happens on the `UNIFORM` form, which is exactly why `first-name`, `firstName`, `first_name`, and `FIRSTNAME` collide onto the same target.

**Environment-variable mapping rule (subtle).** To turn a canonical name into an env var: replace dots with underscores, **REMOVE dashes** (not convert to underscore), and uppercase. So `spring.main.log-startup-info` → `SPRING_MAIN_LOGSTARTUPINFO`. `SystemEnvironmentPropertyMapper` produces up to **four candidate names** (current and legacy formats, each upper and lower case), so a single env var can match both `foo.bar-baz` and `foo_bar_baz`. This is deliberately designed around OS restrictions (env names limited to letters/digits/underscore).

**Lists and maps from env vars.** Indexed list elements use a numeric index wrapped in underscores: `MY_SERVERS_0`, `MY_SERVERS_1` for `List<String> servers`. When binding env vars to a `Map`, Boot lowercases the env-var **name** (not the value) before binding — `MY_PROPS_VALUES_KEY=Value` yields `{"key" = "Value"}` (key lowercased, value case preserved). To preserve an original key with dashes/dots/uppercase in files, wrap it in brackets: `my.map.[key-with-dashes]=value`.

**Hidden concept — placeholders are asymmetric.** Always reference the canonical kebab form in `${...}`. `${demo.item-price}` works with relaxed lookup across `.properties` and env, but `${demo.itemPrice}` will **not** pick up `demo.item-price`. Placeholder relaxed-binding only fully works from the canonical form.

`SpringApplication.setEnvironmentPrefix("input")` namespaces **only** system environment variables (`remote.timeout` → `INPUT_REMOTE_TIMEOUT`); it deliberately does *not* prefix system properties or file-based properties.

**Design intent.** Different transports impose different naming rules (env vars can't have dots; properties prefer dots; code prefers camelCase). Rather than forcing users to know which form each source needs, Boot binds them all to one canonical name. You name a property once and set it however your platform allows — decoupling the key's *identity* from each source's syntactic constraints.

**Changed in v4/v7 (confirmed unchanged):** relaxed-binding rules carried over from 3.x intact.

---

### @ConfigurationProperties: type-safe binding

`@ConfigurationProperties` binds a tree of external config onto a strongly-typed POJO or record rooted at a kebab-case `prefix`, replacing scattered, stringly-typed `@Value` injection with a cohesive, validated, documented configuration object that fails fast at startup.

```java
@ConfigurationProperties("my.main-project.person")
public class PersonProperties {
    private String firstName;       // binds my.main-project.person.first-name
    // getters/setters...
}
```

`value()` is an alias for `prefix()`; the prefix **must** be canonical kebab-case. Two leniency flags: `ignoreUnknownFields` defaults `true` (a stray key is silently ignored), `ignoreInvalidFields` defaults `false` (a coercion failure throws).

**Design intent — config is data, not code.** Unlike `@Value`, `@ConfigurationProperties` does **not** evaluate SpEL, because these values are externalized config, not container expressions. This is a deliberate philosophical split: `@Value` is a container-expression mechanism (SpEL-capable, per-field, eager, good for isolated scalars); `@ConfigurationProperties` is an externalized-config mechanism (no SpEL, whole-object, relaxed + validated + documented). Keeping config *inert* instills the mental model that config is data bound to a type, not code evaluated in a bean. Guidance: prefer `@ConfigurationProperties` for any group/hierarchy of related settings; reserve `@Value` for isolated values or genuine SpEL needs.

#### Two binding modes

**JavaBean (setter) binding** is the default — needs a no-arg constructor plus getters/setters. Setters may be omitted for pre-initialized mutable collections/maps (mutated in place) and for pre-initialized nested POJOs exposed via a getter; but to have the binder *create* a nested instance on demand, a setter is required.

```java
@ConfigurationProperties("my.app")
public record AppProperties(
    String name,
    @DefaultValue("USER") List<String> roles,
    @DefaultValue Security security) {}   // empty @DefaultValue forces a non-null nested instance
```

**Constructor / value-object binding** is for immutable config. It is **implicit** when the class/record has a single parameterized constructor that is neither private nor `@Autowired` (records get this for free); `@ConstructorBinding` is only *required* to disambiguate multiple constructors. `@DefaultValue` supplies parameter defaults, and — hidden concept — an *empty* `@DefaultValue` on a nested-object parameter forces creation of a non-null nested instance even when nothing binds (a common NPE source otherwise). Requires `-parameters` compilation (automatic under the Boot plugins).

**Changed in v4/v7:** Binding to **public fields is removed**. Classes must use private fields with getters/setters (JavaBean) or constructor binding; previously-bound public fields silently stop binding. (The minimum Java version is **17** — some third-party blogs erroneously claim 21.)

#### Three enabling routes — and why constructor binding is restricted

1. `@EnableConfigurationProperties(MyProps.class)` on a `@Configuration` class — explicit, ideal for conditional/auto-config and third-party classes.
2. `@ConfigurationPropertiesScan({packages})` — scan-like auto-discovery.
3. Plain `@Component` stereotype — works but **only** supports JavaBean/setter binding.

**Crucial rule:** constructor binding works **only** via routes (1)/(2). It cannot be combined with beans created by `@Component`, `@Bean` methods, or `@Import`. Auto-registered beans are named `<prefix>-<fully.qualified.ClassName>`.

**Design intent.** `@Component`/`@Bean`/`@Import` instantiate the bean *themselves* (calling a constructor for DI), leaving no opportunity for the Binder to supply constructor args from config. Restricting value-object binding to `@EnableConfigurationProperties`/`@ConfigurationPropertiesScan` preserves a clean seam where Boot owns instantiation and can inject bound values — preventing a confusing class of "why are my `final` fields null" bugs. Relatedly, Spring discourages injecting *other beans* into `@ConfigurationProperties` classes: config objects are meant to be pure projections of the Environment, bound very early and easily tested/serialized.

#### The Binder API

The engine is `org.springframework.boot.context.properties.bind.Binder` over `ConfigurationPropertySource`s, binding a `Bindable<T>` target:

```java
BindResult<MyProps> result =
    Binder.get(environment).bind("app", Bindable.of(MyProps.class));
MyProps props = result.orElse(MyProps.DEFAULT);
// bindOrCreate(...) creates a default instance if nothing bound
```

`BindResult<T>` is a monad (`isBound`/`get`/`orElse`/`map`). `Bindable` carries type info plus metadata (`of`, `ofInstance`, `listOf`, `setOf`, `mapOf`; builder methods `withAnnotations`, `withExistingValue`, `withSuppliedValue`, `withBindMethod`, `withBindRestrictions`). The `BindMethod` enum is `JAVA_BEAN` vs `VALUE_OBJECT`; **an existing or supplied value forces `JAVA_BEAN`**. A `BindHandler` hook (with `onStart`/`onSuccess`/`onFailure`/`onFinish`) customizes/validates the process — it is the same machinery `@ConfigurationProperties` uses internally (e.g. `ValidationBindHandler`), exposed for programmatic binding.

#### Collections, conversion, and validation

Lists/sets/arrays bind via `[index]` in files, YAML list syntax, or comma-separated scalars; maps bind child keys directly under the prefix; nested objects descend one prefix segment per level. **Hidden concept:** collections/maps are **replaced, not deep-merged** across sources — the entire collection comes from the single highest-precedence source defining it (you cannot append from a lower source). Scalars follow normal Environment precedence.

Conversion is driven by `org.springframework.boot.convert`: `Duration` accepts ISO-8601 or suffixes `ns,us,ms,s,m,h,d` (`@DurationUnit`); `Period` accepts `y,m,w,d` (`@PeriodUnit`); `DataSize` accepts `B,KB,MB,GB,TB` (`@DataSizeUnit`); enums bind case-insensitively. **Hidden concept:** custom converters register as beans qualified `@ConfigurationPropertiesBinding`, and such `@Bean` methods should be declared `static` to avoid "not eligible for all BeanPostProcessors" warnings — binding happens very early.

Validation uses `@Validated` on the class plus JSR-303 constraints (`@NotNull`, `@Min`, `@Email`) with a provider like Hibernate Validator; invalid config then **fails fast at startup** with a `BindValidationException` rather than a deferred NPE. `@Valid` cascades into nested objects; a bean named `configurationPropertiesValidator` supplies custom logic.

**Design intent.** Fail-fast surfaces misconfiguration as an immediate, descriptive startup failure rather than subtle production misbehavior, and reuses standard JSR-303 so config constraints look like every other validated bean.

#### Configuration metadata generation

Adding `spring-boot-configuration-processor` (Gradle `annotationProcessor` / Maven `annotationProcessorPaths`) emits `META-INF/spring-configuration-metadata.json` at compile time — groups, properties (name/type/description-from-Javadoc/defaultValue/deprecation), and hints — powering IDE autocompletion and docs. Hand-author or override via `META-INF/additional-spring-configuration-metadata.json` (which also supports an `ignored.properties` section); collection/map defaults can't be auto-detected and are typically documented there.

**Design intent.** Compile-time generation makes configuration a discoverable, self-documenting API surface — addressing "what properties does this app even accept?" — with no runtime cost or reflection.

**Changed in v4/v7:** a new `@ConfigurationPropertiesSource` annotation marks reusable types (often in a separate module/jar) so the processor generates full metadata for them even when the consuming class can't see their source; output goes to a **new per-type location** `META-INF/spring/configuration-metadata/<fqn>.json`. Additionally, `PropertyMapper` (used when wiring bound properties onto third-party objects) **no longer invokes adapter/predicate methods for null source values by default**: `alwaysApplyingWhenNonNull()` was removed, replaced by `always()` to opt into mapping nulls. Numerous property *key* renames accompany the module realignment (e.g. `spring.data.mongodb.*` → `spring.mongodb.*`, `spring.session.redis` → `spring.session.data.redis`, `management.tracing.enabled` → `management.tracing.export.enabled`); the `spring-boot-properties-migrator` module reads the metadata JSON to detect these during upgrade.


---

## Proxies, AOP & Events

Spring's proxying machinery and its application-event bus are two faces of the same conviction: that collaboration between objects should be *added around* plain Java objects by the container, not baked into the objects themselves. Proxies attach cross-cutting behavior to the seam between beans; events let one bean notify others without ever holding a reference to them. Both are deliberately non-invasive, both live inside the IoC lifecycle, and both pay a specific, principled price for that non-invasiveness.

### Proxy-based AOP: the core decision

Spring AOP is *proxy-based*. Cross-cutting advice — `@Transactional`, `@Async`, `@Cacheable`, `@Validated`, `@Retryable`, and any declared `@Aspect` — is applied by wrapping a target bean in a proxy that intercepts external method calls and routes them through an interceptor chain before (and after) delegating to the real object.

The rejected alternative is full AspectJ bytecode weaving, which is more powerful but couples your build to the AspectJ compiler or to a load-time-weaving JVM agent. Spring chose proxies precisely because they need *no special compiler, no build-time weaving step, and no JVM agent* — a proxy is an ordinary runtime object created by an ordinary `BeanPostProcessor`. This keeps AOP entirely inside the IoC container and the normal Java toolchain, lowering adoption cost to near zero.

The mental model this instills is the load-bearing idea of the whole section: **advice is something the container adds around your bean when it hands it to collaborators — it is not something compiled into your class.** Every consequence below, including the famous self-invocation limitation, follows directly from that single design choice.

### Two proxy mechanisms: JDK dynamic proxies vs CGLIB

There are exactly two implementations.

- **JDK dynamic proxies** are built into the JDK. They use `java.lang.reflect.Proxy` plus an `InvocationHandler` and can only expose *interface-typed* methods. Spring chooses a JDK proxy when the target implements at least one interface and class-based proxying is not forced.
- **CGLIB** generates a runtime *subclass* of the target and overrides its methods to insert interception. Spring uses CGLIB when the target implements no interfaces, or when proxying is forced to the class. CGLIB is repackaged into `spring-core` under `org.springframework.cglib` (so there is no separate CGLIB dependency).

The intent behind historically preferring JDK interface proxies was to enforce "program to interfaces": the proxy's exposed surface stays intentional, and callers depend on contracts, not concretions. The cost is real, though — a bean with no interface cannot be JDK-proxied, and you can never cast a JDK proxy back to the concrete class.

```java
public class OrderService { /* no interface */ }   // -> must be CGLIB
public class OrderServiceImpl implements OrderService // -> JDK proxy by default
```

#### `proxy-target-class` / `proxyTargetClass`

This boolean knob forces subclass (CGLIB) proxying instead of interface (JDK) proxying. It surfaces in several places:

- the `proxyTargetClass` attribute on `@EnableAspectJAutoProxy`, `@EnableTransactionManagement`, `@EnableCaching`, `@EnableAsync`, etc.;
- `proxy-target-class` in XML `<aop:config>` / `<aop:aspectj-autoproxy>`;
- the `spring.aop.proxy-target-class` property in Boot.

One subtlety: even with `proxyTargetClass=true`, if the bean's resolved target type is *itself* an interface, a JDK proxy is still produced — you cannot subclass an interface.

#### **Changed in v4/v7:** consistent global default applied to all proxy processors

Spring Framework 7.0 makes the *global default proxy type* consistently apply to **all** proxy processors, including `@EnableAsync` and friends. The release-note wording is:

> "As of 7.0, global proxy type defaulting to CGLIB - like in Spring Boot - is consistently applied to all proxy processors (including @Async and co)."

The precise nature of this change matters and is easy to overstate. Previously, individual processors such as `@Async` and `@Retryable` could independently choose JDK proxies regardless of the global setting, producing confusing inconsistency across `@Transactional` vs `@Async` vs Boot. v7 unifies the *defaulting mechanism* so that whatever the context-wide default is, every proxy processor honors it.

It is **not** the case that CGLIB has simply become the universal default in plain Spring Framework. Per the reference docs, the core framework still *suggests interface-based (JDK) proxies by default*; it is Spring Boot that — depending on configuration properties — enables class-based (CGLIB) proxies by default. So in a Boot application you will typically see CGLIB everywhere; in plain Spring Framework the suggested default remains JDK interface proxies. What changed is consistency of application across processors, not a blanket flip of the core default.

#### **Changed in v4/v7:** `@Proxyable` for per-bean opt-out

Spring Framework 7.0 introduces `@Proxyable` for per-bean control, placed on a `@Bean` method or a `@Component` class:

```java
@Proxyable(INTERFACES)            // force JDK interface proxy against a CGLIB default
@Proxyable(TARGET_CLASS)          // force CGLIB against a JDK default
@Proxyable(interfaces = MyService.class)  // restrict the JDK proxy to a chosen interface subset
```

The `interfaces` attribute is a genuinely new, finer-grained encapsulation control: it narrows the exposed type set of a JDK proxy so callers cannot downcast to interfaces you did not intend to expose — a sharper tool than the old all-or-nothing interface proxying.

#### **Changed in v4/v7:** context-wide `ProxyConfig` bean

Also new in 7.0: a `ProxyConfig` bean registered under `AutoProxyUtils.DEFAULT_PROXY_CONFIG_BEAN_NAME` sets application-context-wide default proxy settings. This gives a single programmatic place to control proxy defaults instead of scattering `proxyTargetClass` attributes across every `@Enable*` annotation.

(Note on continuity, not change: the `@EnableAspectJAutoProxy` javadoc still documents `proxyTargetClass` with attribute default `false` and `exposeProxy` default `false`. The per-annotation attribute defaults are unchanged; what v7 changed is the *effective global/context default* applied when nothing is specified, and the consistency with which it is applied.)

### Proxying is a `BeanPostProcessor` woven into the DI lifecycle

The mechanism that turns a raw bean into a proxy is `AbstractAutoProxyCreator`, which implements `SmartInstantiationAwareBeanPostProcessor` and `BeanFactoryAware`. In `postProcessAfterInitialization` it asks subclasses whether the freshly-initialized bean is eligible (has matching Advisors/advice); if so it builds a `ProxyFactory`, configures it from the effective `ProxyConfig`, copies in the matched interceptor chain, and **returns the proxy in place of the raw bean**.

This last point is the crux of why proxies are transparent to DI: because the post-processor returns a *different object* than it received, the container injects that proxy into every dependent. **DI never sees the raw target.** There is no separate proxy-registration step; proxying composes with all other post-processors automatically.

The philosophy here is that AOP is *just another lifecycle concern the container manages*, not a parallel system. Folding it into the standard bean lifecycle is also what lets it cooperate with circular-reference resolution (see hidden concepts below).

#### Two families of auto-proxy creator (and only one wins)

Most developers assume there is a single "AOP proxy creator." There are several, and **only one runs per context** — the most capable one wins, and the single proxy it produces carries the *union* of all matching advice.

- **`InfrastructureAdvisorAutoProxyCreator`** only considers `Advisor` beans marked `@Role(ROLE_INFRASTRUCTURE)`. This is how `@EnableTransactionManagement` (which registers `BeanFactoryTransactionAttributeSourceAdvisor` in `ProxyTransactionManagementConfiguration`), `@EnableCaching`, `@EnableAsync`, and `@Validated` keep their advice isolated from application aspects.
- **`AnnotationAwareAspectJAutoProxyCreator`** (registered by `@EnableAspectJAutoProxy` / Boot's `AopAutoConfiguration`) does everything the infrastructure variant does *and* discovers user `@Aspect` beans, turning their `@Pointcut`/`@Before`/`@Around` advice into Advisors. `@EnableAspectJAutoProxy` "upgrades" the creator to this AspectJ-aware one. It has been available since Spring 3.1, has attributes `proxyTargetClass` (default `false`) and `exposeProxy` (default `false`, since 4.3.1), and requires `aspectjweaver` on the classpath.

The isolation provided by `@Role(ROLE_INFRASTRUCTURE)` is deliberate: it keeps transaction/cache/async advice from being entangled with — or accidentally matched by — application-level aspects.

#### How inert annotations become proxies

`@Transactional`, `@Async`, `@Cacheable`, `@Validated`, and `@Retryable` are, by themselves, *inert metadata*. The corresponding `@Enable*` annotation (often via an `ImportSelector`, e.g. `TransactionManagementConfigurationSelector`) does the real work: it registers the auto-proxy-creator `BeanPostProcessor` plus the feature's `Advisor` and its interceptor — `TransactionInterceptor`, `AsyncAnnotationAdvisor`, `CacheInterceptor`, `MethodValidationInterceptor`, or Spring Retry's `RetryOperationsInterceptor`. The interceptor is what actually opens a transaction, dispatches to a `TaskExecutor`, consults the cache, validates arguments, or retries — all *around* the real method call. The annotation is the marker; the `@Enable*` selector is the wiring; the interceptor is the behavior.

### The self-invocation limitation

This is the defining tradeoff of proxy-based AOP. The caller holds a reference to the proxy, so external calls are intercepted. But once control is *inside* the target, a `this.bar()` call goes straight to the raw object's `bar()` and never re-enters the proxy — so `bar()`'s advice (its `@Transactional`, `@Cacheable`, etc.) **silently does not run.**

```java
@Service
class Billing {
    public void process() {
        charge();          // self-call -> proxy bypassed -> @Transactional ignored
    }
    @Transactional
    public void charge() { ... }
}
```

This is not a bug; it is the honest boundary of the wrapper model. Making `this.method()` also be intercepted would require rewriting the target's own bytecode — which is exactly what proxies were chosen to avoid. The intended mental model: **advice attaches to the seam between beans (the DI-injected reference), not to internal control flow.** AspectJ weaving has no such limitation because advice is woven into bytecode, not added by an external wrapper.

The sanctioned workarounds, in order of preference:

1. Refactor to avoid the self-call (move `charge()` to a separate collaborator bean).
2. Inject a self-reference (the proxy) and call `self.charge()`.
3. As a last resort, `AopContext.currentProxy()` with `exposeProxy=true`.

The docs steer toward redesign or self-injection over `AopContext` deliberately — because relying on `AopContext` couples your business code to Spring AOP, defeating the non-invasive goal that motivated proxies in the first place.

### `@Configuration` CGLIB enhancement (full vs lite mode) — *not* advice

This is a separate use of CGLIB that is frequently conflated with advice proxying, and the conflation leads to wrong reasoning about why a config class is a CGLIB subclass.

A `@Configuration` class with `proxyBeanMethods=true` (**full mode**) is itself CGLIB-enhanced so that *inter-bean method calls* — one `@Bean` method calling another — are intercepted and return the shared singleton rather than a fresh instance:

```java
@Configuration  // full mode (proxyBeanMethods=true by default)
class AppConfig {
    @Bean A a() { return new A(b()); }   // b() returns the singleton, not a new B
    @Bean B b() { return new B(); }
}
```

`proxyBeanMethods=false` (**lite mode**, available since Spring 5.2) skips the subclass and the interception cost; dependencies must instead be passed as `@Bean` method parameters. The intent: full mode makes a config class read like ordinary Java where calling `b()` "obviously" returns the singleton, preserving developers' intuition. But that interception costs a CGLIB subclass per config class — wasteful when there are no inter-bean references, and unfriendly to AOT/native. That is why Boot's own auto-configurations pervasively use lite mode, making the cost explicit and opt-out for faster startup. Critically, this enhancement is *orthogonal to* `@Transactional`/aspect proxying.

### Scoped proxies

Injecting a shorter-lived bean (request/session/prototype) into a singleton needs a proxy to *defer resolution to call time* — otherwise the singleton would capture one instance forever and silently violate the shorter scope.

- `@Scope(proxyMode = ScopedProxyMode.TARGET_CLASS)` creates a CGLIB scoped proxy.
- `@Scope(proxyMode = ScopedProxyMode.INTERFACES)` creates a JDK scoped proxy over the bean's interfaces.

The proxy is injected once into the singleton but routes each method call to the *currently-active* scoped instance, keeping scope boundaries correct without the singleton ever knowing.

### Hidden and lesser-known proxy concepts

#### `getEarlyBeanReference` — AOP meets circular references

`AbstractAutoProxyCreator` implements `SmartInstantiationAwareBeanPostProcessor.getEarlyBeanReference` so it can expose the proxy *early* into a circular reference. If a singleton in a cycle gets wrapped, the early reference handed to collaborators must be the proxy — otherwise some collaborators would hold the raw bean and bypass advice. Rather than silently inject a raw instance, Spring detects this situation and, in many cases, errors out. This is the lifecycle integration paying off: proxying participates in cycle resolution instead of fighting it.

#### One proxy, many concerns, ordering matters

A bean that is `@Transactional` *and* `@Cacheable` *and* targeted by custom aspects gets a **single** proxy whose interceptor chain contains all matching advice, ordered by Advisor order (`@Order` / `Ordered` / `@Priority`). Because there is no per-feature proxy, misordering is invisible — e.g. placing cache *outside* vs *inside* the transaction is a common, subtle bug that you cannot see by inspecting object identity. Ordering only matters and only applies within one proxy's chain.

#### `final`/`private`/package-private cannot be advised under CGLIB

Because Boot (and, with v7's consistency change, more setups generally) lean on CGLIB, more code is silently subject to CGLIB's constraints: a `final` class cannot be proxied at all, and `final`, `private`, or cross-package package-private methods cannot be advised — the advice silently does not fire. Kotlin classes are `final` by default, making this a frequent surprise. Practitioners on JPMS/native should also expect CGLIB constraints, with modules sometimes requiring `--add-opens` for `java.lang`.

#### Objenesis constructor bypass

CGLIB proxy instances are created via **Objenesis**, which bypasses the constructor. So a proxied bean's constructor is invoked once (on the target), not a second time during proxy creation — older CGLIB behavior could invoke the constructor twice. Relying on constructor invocation count, or doing real side-effecting work in constructors, interacts badly with this. (Spring Framework 6.0 had already moved CGLIB to a fully repackaged, ASM-based form generating classes in the same package as the target and using Objenesis; v7 builds on that.)

#### `exposeProxy` ThreadLocal cost

`AopContext.currentProxy()` only works if `exposeProxy=true`, which makes *every* advised call store the proxy in a `ThreadLocal`. It is off by default both for the small overhead and — more importantly — because relying on it couples business code to Spring AOP, defeating the non-invasive goal.

---

### The application event system: decoupled in-process pub/sub

Spring's event system is an in-process, observer-pattern pub/sub layer built into `ApplicationContext` — deliberately *not* `BeanFactory`. A plain `BeanFactory` has no events. The split is intentional and instructive: DI/IoC (the `BeanFactory`) answers *"who do I depend on?"*, while eventing answers *"who should react when something happens?"* **without the publisher knowing the reactors.** Keeping events in `ApplicationContext` signals that they are an application-level collaboration mechanism layered on top of the container, complementing — never replacing — constructor wiring.

The philosophical payoff is open/closed at the wiring level. DI couples a consumer to a producer by type at wiring time (A holds a B). Events invert this: the producer emits a *fact* and is oblivious to consumers; new reactors are added by adding beans, never by editing the producer. This is what keeps high-fan-out, cross-cutting reactions — audit, cache eviction, notifications — from turning the emitter into a hub of dependencies.

#### **Changed in v4/v7:** essentially nothing

The core event API — `ApplicationEvent`, `ApplicationListener`, `@EventListener`, `ApplicationEventMulticaster`/`SimpleApplicationEventMulticaster`, `PayloadApplicationEvent`, `@TransactionalEventListener`, and the context lifecycle events — is **unchanged** in Spring Framework 7.0 / Boot 4.0 versus Framework 6 / Boot 3. The 7.0 release notes do not mention the event system at all; treat it as a stable, mature API. The only surrounding shifts are the JDK 17+ / Jakarta EE 11 baseline and Boot's move of *auto-configuration* off `spring.factories` — but note that the `ApplicationListener` key in `spring.factories` **still works** for early listeners (more below).

### `ApplicationEvent` / `ApplicationListener` — the classic model

The classic form: extend `ApplicationEvent` (which carries a `source` Object and a timestamp) and implement `ApplicationListener<MyEvent>`, whose single `onApplicationEvent(MyEvent)` is invoked. `ApplicationListener` is a `@FunctionalInterface` extending `java.util.EventListener` — the canonical Observer pattern.

### `@EventListener` — the preferred POJO model

Annotate any public method on a managed bean; the single method parameter type selects the event:

```java
@EventListener
void on(OrderPlaced e) { ... }

@EventListener(classes = {A.class, B.class}, condition = "#root.event.priority > 5")
void on() { ... }   // common supertype or no parameter
```

Processing is performed by `EventListenerMethodProcessor` (a `SmartInitializingSingleton`), which uses `DefaultEventListenerFactory` to wrap each method in an `ApplicationListenerMethodAdapter`. Introduced in 4.2.

The design intent for preferring annotated methods over implementing `ApplicationListener`: implementing the interface forces one-event-per-class and ties the class to the framework. Annotated methods let one bean host *many* listeners, target generic instantiations, filter via SpEL, order via `@Order`, and chain via return values — an annotation-driven, POJO-friendly model consistent with `@Transactional`/`@Scheduled`.

#### `condition` SpEL filtering

The `condition` attribute is a SpEL string evaluated *per event*; the method runs only if it yields `true` (or `"true"`/`"on"`/`"yes"`/`"1"`). The root context exposes `#root.event` / `event`, `#root.args` / `args`, and each argument by name (`#myEvent`) or index (`#a0`/`#p0`). This filters at dispatch time without polluting listener bodies with `if`-guards.

#### Return-value-as-new-event chaining

A non-void `@EventListener` return value is re-published as a new event; a returned `Collection` or array publishes each element separately; `null`/`void` publishes nothing. This enables small declarative event pipelines (`BlockedListEvent -> ListUpdateEvent`) without injecting a publisher. It is explicitly **not supported on `@Async` listeners** — there the result would be produced on another thread after the publisher already returned, so silently re-publishing it would have surprising timing/ordering. On async listeners you must inject `ApplicationEventPublisher` and publish manually.

#### Payload events (`PayloadApplicationEvent`)

Since 4.2, `publishEvent(Object)` accepts *any* object; if it is not an `ApplicationEvent`, Spring wraps it in `PayloadApplicationEvent<T>` whose `getPayload()` returns it. Listeners just declare the payload type:

```java
@EventListener void on(MyDto e) { ... }   // Spring unwraps the payload
```

The intent: forcing every event to extend `ApplicationEvent` leaked framework types into the domain model. Auto-wrapping POJOs lets domain events be plain records, reducing coupling and making events reusable outside Spring — the same philosophy as dropping the mandatory interface for `@EventListener`.

#### Generic event type resolution

`@EventListener void on(EntityCreatedEvent<Person> e)` matches only that generic instantiation. Because of type erasure this works only when the concrete event resolves the type variable: either via a concrete subclass (`PersonCreatedEvent extends EntityCreatedEvent<Person>`), or by the event implementing `ResolvableTypeProvider#getResolvableType()` to advertise its full `ResolvableType` at runtime. Without that, `EntityCreatedEvent<Person>` and `EntityCreatedEvent<Order>` are indistinguishable, defeating type-safe targeting. `ResolvableTypeProvider` is how the API preserves the type-safety promise it makes.

### The multicaster: one named extension seam

The context delegates `publishEvent` to a single multicaster bean that **must be named exactly `applicationEventMulticaster`** (the constant `AbstractApplicationContext.APPLICATION_EVENT_MULTICASTER_BEAN_NAME`). If absent, the context auto-creates a `SimpleApplicationEventMulticaster`. It resolves matching listeners (caching by event type) and invokes them.

The design rationale is "sensible global default, targeted local override": rather than scatter executor/error config across every listener, Spring funnels *all* dispatch through one replaceable bean. Override it to customize threading and error handling globally:

```java
@Bean
ApplicationEventMulticaster applicationEventMulticaster() {  // magic name required
    var m = new SimpleApplicationEventMulticaster();
    m.setTaskExecutor(...);     // async dispatch for ALL events
    m.setErrorHandler(...);     // isolate listeners from each other
    return m;
}
```

### Synchronous-by-default dispatch

By default, events run on the *caller's thread*, sequentially; `publishEvent` blocks until every listener returns, and an uncaught listener exception propagates back to the publisher (unless an `ErrorHandler` is set). This is deliberate: it preserves the caller's transaction, security context, and exception semantics, so an event behaves like an ordinary method call you didn't have to wire. Async is a deliberate *opt-in* because it silently changes transaction boundaries, exception propagation, and context propagation — Spring refuses to make that the default. (Boot docs warn that listeners must not run lengthy tasks, since they block startup — use `ApplicationRunner`/`CommandLineRunner` for that.)

#### `@Order` ordering

For synchronous dispatch, listeners for a given event are invoked in `@Order` priority order (lower value = earlier; also via `Ordered`/`PriorityOrdered`/`@Priority`). Ordering is only meaningful *synchronously* and *within one event type*; it imposes no ordering across concurrent async listeners.

#### Async events

Two async paths exist:

1. **Global** — set a `TaskExecutor` on the `SimpleApplicationEventMulticaster` so *all* events dispatch on the executor.
2. **Per-listener** — annotate the `@EventListener` method `@Async` (requires `@EnableAsync`) so only that listener runs off-thread.

The async caveats are all consequences of leaving the caller's thread: return-value chaining is disabled; checked exceptions are wrapped in `UndeclaredThrowableException`; uncaught exceptions go to `AsyncUncaughtExceptionHandler` rather than the caller; and `ThreadLocal`/MDC/Micrometer Observation context are **not** propagated unless you add a `ContextPropagatingTaskDecorator` to the executor.

### Built-in lifecycle events

The framework publishes:

- `ContextRefreshedEvent` — after `refresh()`, all singletons instantiated/initialized (can fire multiple times for refreshable contexts);
- `ContextStartedEvent` / `ContextStoppedEvent` — on explicit `start()`/`stop()` of `Lifecycle` beans;
- `ContextClosedEvent` — on `close()` or shutdown hook (the context is end-of-life and cannot be restarted).

Web adds `RequestHandledEvent` / `ServletRequestHandledEvent` per completed request.

### Boot's `SpringApplicationEvents`

Boot fires, in order:

`ApplicationStartingEvent` → `ApplicationEnvironmentPreparedEvent` → `ApplicationContextInitializedEvent` → `ApplicationPreparedEvent` → (`WebServerInitializedEvent`) → `ContextRefreshedEvent` → `ApplicationStartedEvent` → `AvailabilityChangeEvent(LivenessState.CORRECT)` → `ApplicationReadyEvent` (after all runners) → `AvailabilityChangeEvent(ReadinessState.ACCEPTING_TRAFFIC)`; and `ApplicationFailedEvent` on startup error.

The events that fire *before the context exists* (`ApplicationStartingEvent`, `ApplicationEnvironmentPreparedEvent`) cannot use `@Bean`/`@EventListener` — there is no context to host the bean yet. Register them via `SpringApplication.addListeners(...)`, `SpringApplicationBuilder.listeners(...)`, or the `ApplicationListener` key in `META-INF/spring.factories`. **Changed in v4/v7:** Boot 4 stops loading *auto-configuration* from `spring.factories` (it now uses `META-INF/spring/...AutoConfiguration.imports`), but the `ApplicationListener` key in `spring.factories` **still works** — `SpringFactoriesLoader` continues to read non-autoconfig keys.

### `@TransactionalEventListener` — extensibility, illustrated

A `@EventListener` meta-annotated specialization (in `spring-tx`) that defers invocation to a `TransactionPhase`: `BEFORE_COMMIT`, `AFTER_COMMIT` (the default), `AFTER_ROLLBACK`, or `AFTER_COMPLETION`. It binds to the current transaction's synchronization; if no transaction is active the listener is **skipped** unless `fallbackExecution=true`. Since 6.1 it also supports `ReactiveTransactionManager`, carrying the transaction context inside the event via `TransactionalEventPublisher` (because Reactor uses `Context`, not `ThreadLocal`). It is a clean illustration of how the listener-adapter layer is extensible: a new phase-aware listener is built entirely on top of `@EventListener`.

#### `defaultExecution` vs `fallbackExecution`

`@EventListener#defaultExecution` (added in 6.2, default `true`, present in 7.0) exists primarily so composed annotations can flip it. `@TransactionalEventListener` maps it to `fallbackExecution` with default `false`. This coupling is *why* a `@TransactionalEventListener` silently does nothing when there is no active transaction — understanding it turns a baffling no-op into expected behavior.

### Hidden and lesser-known event concepts

- **Lazy beans silently lose `@EventListener` registration.** If a bean carrying `@EventListener` methods is `@Lazy`, the context honors the laziness and never registers its listener methods — the classic "my listener never fires" bug. The reference docs explicitly warn against this.
- **The multicaster bean name is a magic string.** Name it anything other than `applicationEventMulticaster` and it is ignored, falling back to the default synchronous one — the same magic-name pattern as `messageSource` and `taskScheduler`.
- **Context hierarchy double-delivery.** An event published in a child context is *also* delivered to listeners in ancestor contexts (Boot parent/child hierarchies). A listener registered in multiple contexts can fire multiple times for one logical event; compare `event.getApplicationContext()` to your injected context to disambiguate.
- **Exceptions abort the whole sync chain unless an `ErrorHandler` is set.** With default sync dispatch and no `ErrorHandler`, the first listener that throws stops the remaining listeners and propagates to the publisher. `SimpleApplicationEventMulticaster.setErrorHandler` decouples listeners from each other.
- **Checked exceptions become `UndeclaredThrowableException`.** A listener may declare checked exceptions, but since the publisher contract only handles runtime exceptions, checked ones are wrapped.
- **`ContextRefreshedEvent` can fire more than once.** For refreshable contexts, a `refresh()` re-publishes it; one-time-init listeners should guard themselves or prefer Boot's `ApplicationStartedEvent`/`ApplicationReadyEvent`.
- **An `ApplicationReadyEvent` listener failure can fail startup.** In `SpringApplication.run`, an exception thrown from an `ApplicationReadyEvent` listener is caught and routed through `handleRunFailure` — i.e. it can fail the application start, not merely log.
- **`AvailabilityChangeEvent` rides the same bus.** Boot's liveness/readiness probe state (`LivenessState.CORRECT`, `ReadinessState.ACCEPTING_TRAFFIC`) is delivered as an ordinary `AvailabilityChangeEvent` on the same multicaster — actuator health is wired through the core event system, a tidy bit of dogfooding.
- **Context propagation needs `ContextPropagatingTaskDecorator`.** Async listeners lose `ThreadLocal`/MDC/Micrometer Observation context unless the multicaster's `TaskExecutor` is decorated with `ContextPropagatingTaskDecorator` (in `spring-core`).

### The unifying philosophy

Both halves of this section express the same principle from different angles. Proxies let the container *add behavior around* a bean at the DI seam, so cross-cutting concerns never invade the bean's source — at the honest cost that only calls crossing that seam are intercepted. Events let a bean *announce facts* with no knowledge of who listens, so reactions can be added without editing the emitter — at the honest cost that the default in-process, synchronous dispatch carries the caller's transaction and thread. In both cases Spring chose the non-invasive, runtime, container-managed option, named its limitation plainly rather than papering over it, and steered developers toward designs that work *with* the wrapper/observer boundary instead of fighting it.


---

## Concurrency, Context Propagation & Error Handling

Spring's design across these three concerns shares a single organizing idea: **the container is an assembler, not a runtime concurrency manager, and not a recovery engine.** It solves the problems it can solve generically and *correctly* (safe publication of singletons, fail-fast wiring validation, vendor-neutral exception models), and it delegates the problems only the developer can solve (mutable runtime state, business recovery) back to the developer—while handing them well-shaped tools (`TaskExecutor`, the context-propagation SPI, `FailureAnalyzer`) so the delegation is ergonomic rather than abandoning. This section traces that philosophy through the threading model, ambient-context propagation, and the DI error/exception model.

---

### The container's concurrency contract: safe publication, NOT runtime thread-safety

The single most important—and most misunderstood—guarantee. The Spring reference (now formally documented under "Thread Safety and Visibility," **changed in v4/v7** in that this contract is *explicitly* written into the bean-factory-nature chapter) states the container "publishes created singleton instances in a thread-safe manner, guarding access through a singleton lock and guaranteeing visibility in other threads."

What this *does* mean: configuration state set only during initialization (constructor/setter injection, `@PostConstruct`) acquires **final-like visibility** to every thread that later observes the bean, courtesy of the Java Memory Model semantics of the singleton lock. What it does *not* mean: that your bean's *methods* are thread-safe at runtime. A shared singleton is invoked concurrently by every request thread; if a method mutates shared state, that is a data race the container will not prevent.

```java
@Service
class PricingService {
    private final TaxTable taxTable;          // injected once; safe-published, no volatile needed
    private volatile int lastComputedTotal;    // MUTATED after init → MUST be volatile or lock-guarded

    PricingService(TaxTable t) { this.taxTable = t; }
}
```

**Design intent.** Making every singleton method `synchronized` would annihilate the entire performance rationale for sharing one instance across all request threads, and would impose needless contention on the overwhelmingly common case—stateless service/controller/repository objects that need no locking at all. Crucially, the container *cannot* know your invariants, so a "thread-safe-by-default" container is both impossible and pessimal. Spring therefore solves the hard, *generic* JMM problem it actually can solve (safe publication / visibility of post-construction config) and points developers at standard Java tools (`volatile`, locks, concurrent collections) for the problem only they understand. The mental model: **the container guarantees you safely *see* a fully-built bean; it guarantees nothing about what happens when many threads *call* it.**

#### Hidden concept: config-only fields don't need `volatile`; only post-init mutation does

A pervasive misunderstanding in both directions. Setter-injected configuration mutated *solely* during the init phase gets final-like visibility via safe publication—**no `volatile` required.** The `volatile`/lock requirement applies *only* to fields changed *after* the bean is published. Practitioners routinely over-synchronize immutable config (wasteful) while under-protecting genuine runtime state (incorrect).

---

### Scope as the primary concurrency design tool

The reference's guidance is blunt: "Use the prototype scope for all stateful beans and the singleton scope for stateless beans." This is not a performance tip—it is the *concurrency* design lever. Because a singleton is shared across all threads, statefulness in a singleton is the number-one source of subtle data races. Pushing conversational/mutable state into `prototype` (or web `request`/`session`) scope gives each user of that state its own instance, sidestepping synchronization entirely.

**Design intent.** The scope choice instills a durable mental model: *if it has mutable per-interaction state, it should not be a singleton.* Rather than ask developers to reason about locks, Spring reframes the problem as a *scoping* decision—a choice the container can mechanically honor.

---

### The singleton creation lock and the `@PostConstruct` deadlock trap

Singleton creation runs under the container's singleton creation lock, and—**hidden concept**—"`@PostConstruct` and initialization methods in general are executed within the container's singleton creation lock." A bean is published as fully initialized only after `@PostConstruct` returns. The consequence is a real hazard: doing external bean access, blocking I/O, or *spawning threads that call back into the container* from an init method risks an **initialization deadlock**.

The sanctioned escape hatches run *outside* the lock, after all singletons are ready:

```java
@Component
class Warmup implements SmartInitializingSingleton {
    @Override public void afterSingletonsInstantiated() { /* heavy work here, lock-free */ }
}
// or:  @EventListener(ContextRefreshedEvent.class)  void onReady() { ... }
```

**Design intent.** Holding the lock through `@PostConstruct` *guarantees* no thread can ever observe a half-initialized bean—correctness of publication. Rather than weaken that guarantee, Spring *constrains the contract*: init methods are for **validating configuration state**, not real work. It then provides explicit lock-free phases (`SmartInitializingSingleton.afterSingletonsInstantiated()`, `ContextRefreshedEvent`) for expensive post-init work. The mental model: **initialization = wiring validation; real work starts after the context is up.**

#### Hidden concept: `FactoryBean.getObject()` shares the same lock

A singleton `FactoryBean` is "processed within the general singleton lock as well," so a `FactoryBean` doing heavy work or re-entering the container can deadlock in exactly the same way an init method can. The same discipline applies.

---

### Singleton locking internals: from strict global lock to lenient/thread-aware locking

Historically `DefaultSingletonBeanRegistry` took a *global* lock, creating singletons strictly one-at-a-time. Under concurrent init (a background thread spawned in an init method, a `BeanPostProcessor` creating interdependent beans in opposite order) this produced deadlocks—the long-standing pain points tracked in issues #23501, #25667, and #34349, centered on the `getSingleton(beanName, allowEarlyReference=false)` path and lock granularity.

**Changed in v4/v7:** Spring Framework 6.2 (carried into 7.x) replaced the strict global lock with a **mix of strict + lenient locking**, so independent beans can be created concurrently (the enabler for background bootstrapping) while interdependent creation stays protected. A `STRICT_LOCKING_PROPERTY_NAME` system property restores 6.1-style strict locking for edge cases. **Framework 7 refines this with thread-aware locking**: `prepareSingletonBootstrap` marks the main bootstrap thread, background threads get lenient locking, and `isCurrentThreadAllowedToHoldSingletonLock()` arbitrates.

#### Hidden concept: a top-level `BeanCurrentlyInCreationException` can be *benign*

Under lenient locking, a `BeanCurrentlyInCreationException` thrown during mainline pre-instantiation can be **safely ignored**—another thread already picked up that bean and will finish it. This flips the long-held intuition that this exception always signals a fatal circular dependency. (Note: a genuine *constructor* cycle is still fatal—see the error-handling section below.)

**Design intent.** "Pay a little determinism to buy parallelism and far fewer deadlocks." The strict global lock was the safe-but-serial baseline; 6.2/7 deliberately trade a slice of startup determinism (the occasional benign exception, restorable via the escape hatch) for concurrent creation and a dramatically reduced deadlock surface.

---

### Opt-in background / parallel bean initialization

**Changed in v4/v7:** `@Bean(bootstrap = Bean.Bootstrap.BACKGROUND)` (introduced in Framework 6.2, carried into 7.x) marks a singleton for concurrent initialization on a separate thread during context startup.

```java
@Configuration
class Config {
    @Bean(bootstrap = Bean.Bootstrap.BACKGROUND)
    SlowClient slowClient() { return new SlowClient(); }   // inits on a bootstrap thread
}
```

It requires an `Executor` bean registered as the factory's bootstrap executor (`DefaultListableBeanFactory.setBootstrapExecutor`)—"otherwise, the background markers will be ignored at runtime." The mechanics carry several subtleties:

- **Hidden concept: non-lazy dependents block automatically.** A non-`@Lazy` injection point into a background bean *blocks* the dependent's creation until the background bean is ready—so you don't get parallelism for free. Real parallelism requires `@Lazy` or `ObjectProvider` on the dependent.
- **Hidden concept: `@DependsOn` forces main-thread init first.** `@DependsOn` targets are initialized on the **main bootstrap thread** *before* the background bean's init is triggered, so a `@DependsOn` dependency is never itself parallelized by the dependent's `BACKGROUND` marker.
- All non-`@Lazy` background inits are forced to complete by the end of context startup.

**The JPA precedent.** `LocalContainerEntityManagerFactoryBean`/`LocalSessionFactoryBean` long exposed a `bootstrapExecutor`: JPA/Hibernate provider init runs in parallel, an `EntityManagerFactory` *proxy* is injectable immediately, and first real access (`createEntityManager`) blocks until bootstrap completes. As of 6.2 this is enforced before context-refresh completion so DB-infrastructure availability is predictable. `@Bean(bootstrap)` is the generalization of this proven pattern.

**Design intent.** Parallelizing *all* bean creation by default would reintroduce exactly the deadlock/ordering hazards Spring spent years taming, and would make startup nondeterministic. Instead Spring demands you (a) explicitly mark the slow, independent beans, (b) explicitly provide the executor (so you own the thread budget), and (c) explicitly model the dependency edges. A surgical tool for the real bottleneck (one slow `EntityManagerFactory`/connection pool), not global concurrency semantics imposed on every bean.

---

### The `TaskExecutor` abstraction: IoC applied to threads

`org.springframework.core.task.TaskExecutor` (extends `java.util.concurrent.Executor`) is Spring's unifying SPI for "run this `Runnable` somehow." `AsyncTaskExecutor` adds `submit`/`Future` and start-timeout semantics. The two canonical implementations embody a deliberate contrast:

- **`ThreadPoolTaskExecutor`** — true pooling (`corePoolSize`/`maxPoolSize`/`queueCapacity`).
- **`SimpleAsyncTaskExecutor`** — *no* pooling (a new thread per task), optional concurrency limit, optional virtual threads, graceful shutdown.

**Design intent.** Direct `new Thread()`/`ExecutorService` usage hides resource limits, can't be centrally tuned or monitored, and can't participate in context lifecycle/shutdown. By injecting a `TaskExecutor`, the same call site can be backed by a bounded pool, a virtual-thread executor, or a context-propagating decorator—*chosen by configuration, not code.* This is the IoC principle applied to concurrency: code declares "run this asynchronously," the container decides *how.* It is precisely why swapping to virtual threads becomes a one-property change with zero code edits.

---

### `@EnableAsync` / `@Async`: proxied asynchrony and its gotchas

`@Async` runs a method on an `AsyncTaskExecutor` via an AOP proxy—an `AsyncExecutionInterceptor` (an AOP-Alliance `MethodInterceptor`) hands the invocation to the executor. Return type must be `void`, `Future`, or `CompletableFuture`. The well-known traps:

1. **Self-invocation bypasses the proxy**, so `@Async` is silently ignored when a bean calls its own `@Async` method internally.
2. **`void`-returning `@Async` cannot propagate exceptions to the caller**—register an `AsyncUncaughtExceptionHandler` via `AsyncConfigurer`.
3. **Executor selection order:** by-qualifier `@Async("beanName")`, else `AsyncConfigurer.getAsyncExecutor()`, else a context `Executor`.

**Proxy default (v7).** Framework 7.0 introduces a `@Proxyable` annotation (e.g. `@Proxyable(INTERFACES)` / `@Proxyable(TARGET_CLASS)`) and makes the *global* proxy-type default apply consistently to all proxy processors—including `@EnableAsync`, which previously could choose JDK proxies independently of the global setting. CGLIB is **not** the universal default in the *core* framework: per the AOP reference, the core framework suggests interface-based (JDK) proxies by default, while *Spring Boot*—depending on configuration—may enable class-based (CGLIB) proxies. So the v7 framing is: the global default (whatever it is in a given setup) is now applied uniformly to every proxy processor, and `@Proxyable` lets you opt out per bean.

---

### `@EnableScheduling` / `@Scheduled`

`@EnableScheduling` activates a `ScheduledAnnotationBeanPostProcessor` that registers `@Scheduled` methods (`cron`/`fixedRate`/`fixedDelay`) against a `TaskScheduler`. You can enable scheduling *without* `@EnableAsync` and customize via `SchedulingConfigurer`.

#### Hidden concept: the default scheduler is single-threaded

Without an explicit `TaskScheduler` (or virtual threads), `@Scheduled` tasks share **one** thread (`ThreadPoolTaskScheduler`, pool size 1). A single long-running scheduled task therefore *serializes and delays all the others*—a frequent production surprise.

---

### Virtual-thread support: one switch, an unchanged contract

`spring.threads.virtual.enabled=true` (requires Java 21+) is the single switch. Boot's `@ConditionalOnThreading(Threading.VIRTUAL)` gates virtual-thread wiring—the `VIRTUAL` enum checks *both* the property *and* Java ≥ 21. When enabled:

- the auto-configured `AsyncTaskExecutor` becomes a `SimpleAsyncTaskExecutor` on virtual threads;
- the scheduler becomes a `SimpleAsyncTaskScheduler` (one scheduler thread firing a fresh virtual thread per execution; pooling properties ignored; fixed-delay runs on the single scheduler thread).

`SimpleAsyncTaskExecutorBuilder`/`SimpleAsyncTaskSchedulerBuilder` auto-select virtual threads when enabled.

**Design intent.** Virtual-thread adoption should be a *deployment/runtime decision, not a code rewrite.* Because business code already talks to abstractions (`TaskExecutor`, `TaskScheduler`) rather than threads, Boot can rewire the concrete implementations behind one flag. Critically, **virtual threads do not change the thread-safety contract**: per-thread (`ThreadLocal`/transaction/security) semantics are preserved, so the same code is correct on either substrate. This reinforces that the threading substrate is infrastructure, not an application concern.

---

### Boot's auto-configured executors and bean naming

Absent a user `Executor` bean, Boot auto-configures an `AsyncTaskExecutor`: a virtual-thread `SimpleAsyncTaskExecutor` if enabled, else a `ThreadPoolTaskExecutor` (8 core threads, grow/shrink). The naming carries semantics:

- The **`applicationTaskExecutor`** bean backs MVC async, WebFlux blocking, GraphQL `Callable`, WebSocket channels, the JPA bootstrap executor, *and* `ApplicationContext` background initialization.
- A **`taskExecutor`**-named bean is used for `@EnableAsync` when no `@Primary` `Executor`/`AsyncConfigurer` exists.

#### Hidden concept: `spring.task.execution.mode=force` overrides custom `@Async` executors

Setting `force` (present since Boot 3.2) routes **all** integrations—*including `@Async`*—through the auto-configured executor regardless of your custom `Executor` beans; **only an `AsyncConfigurer` overrides it.** Conversely, registering an `Executor` with `defaultCandidate=false` lets the auto-config ignore it. Defining your own `Executor` bean does *not* automatically reroute `@Async` under force mode—a real gotcha.

---

### Framework 7 resilience: `@ConcurrencyLimit` and `@Retryable`

**Changed in v4/v7:** Framework 7.0 adds core resilience annotations enabled via `@EnableResilientMethods`. `@ConcurrencyLimit(n)` caps concurrent invocations—backed by `ConcurrencyThrottleInterceptor` for synchronous calls and a `SimpleAsyncTaskExecutor` concurrency limit for async tasks. It is explicitly positioned as useful *with virtual threads*, which otherwise have no pool ceiling.

#### Hidden concept: `@ConcurrencyLimit(1)` as a declarative instance lock

Setting the limit to 1 effectively serializes access to the bean—a declarative alternative to `synchronized` for protecting a non-thread-safe target, again especially relevant under virtual threads, which provide no implicit concurrency ceiling.

#### Hidden concept: immediate-destroy-without-stop

`Lifecycle` beans must tolerate a destroy callback *without* a preceding stop (cancelled bootstrap, stop timeout). Any runtime-mutable `Lifecycle` state (e.g. a `runnable` field) must be `volatile` and robust to out-of-order shutdown.

---

## Context Propagation: the holder/strategy pattern and the Micrometer bridge

The threading model above exposes a second problem: Spring's classic "ambient context" (current request, locale, transaction) lives in `ThreadLocal`, which works only while a request stays on one thread—and silently breaks at every thread hop the `TaskExecutor` abstraction introduces.

### The holder/strategy pattern over `ThreadLocal`

A "Holder" is a class of only static methods delegating to static `ThreadLocal` fields, giving global static read/write access to a value that is physically per-thread. `RequestContextHolder`, `LocaleContextHolder`, and (with a different shape) `TransactionSynchronizationManager` all follow this. The static facade is the *API contract*; the `ThreadLocal` is the *hidden storage strategy*.

#### Hidden concept: `NamedThreadLocal` / `NamedInheritableThreadLocal`

Spring never uses raw `java.lang.ThreadLocal` in these holders—it subclasses with a human-readable name *purely* so a leaked thread-local is identifiable in heap dumps. A debugging-quality decision most practitioners never notice.

**The dual-field selection pattern.** `RequestContextHolder` declares *two* fields:

```java
ThreadLocal<RequestAttributes> requestAttributesHolder =
    new NamedThreadLocal<>("Request attributes");
ThreadLocal<RequestAttributes> inheritableRequestAttributesHolder =
    new NamedInheritableThreadLocal<>("Request context");
```

`setRequestAttributes(attrs, inheritable)` writes one and clears the other based on the boolean; `getRequestAttributes()` reads the plain one first, then falls back to the inheritable one. `LocaleContextHolder` uses the identical dual-field pattern. `InheritableThreadLocal` copies the parent's value into a child thread *at thread construction time*—but it is **off by default** (`inheritable=false`) precisely because copy-on-create is dangerous with pools.

**Design intent.** A request/transaction/locale is "a property of the current execution, not of every method signature." Threading a `RequestContext` or `Connection` through every call would pollute APIs and couple unrelated layers; `ThreadLocal` exploits the invariant that classic servlet/JDBC work is pinned to one thread, making "the current X" a free ambient lookup. The rejected alternatives: explicit parameter passing (correct but viral and intrusive) and a single global static (wrong—not per-request).

### `TransactionSynchronizationManager`: thread-bound, *never* inheritable

Its six fields (`resources` map, `synchronizations` set, `currentTransactionName`, `currentTransactionReadOnly`, `currentTransactionIsolationLevel`, `actualTransactionActive`) are **all plain `NamedThreadLocal`**—never `InheritableThreadLocal`. `bindResource`/`getResource`/`unbindResource` maintain a thread-scoped `Object→Object` map so the same JDBC `Connection`/`EntityManager` is reused throughout a transaction.

**Design intent.** Correctness over convenience: silently sharing a transactional `Connection` with a child thread would shatter transaction isolation and connection-pool accounting. A transaction must live and die on one thread, so inheritance is made *impossible* here, not merely discouraged.

### The `context-propagation` SPI: generalizing the holder pattern

Before this, every concern (tracing, MDC, security, locale, tenant) reinvented its own "copy my `ThreadLocal` to the other thread" code, and none composed. The `io.micrometer:context-propagation` library extracts four abstractions:

- **`ThreadLocalAccessor<V>`** — the contract for one `ThreadLocal`: `Object key()`, `V getValue()`, `void setValue(V)`, no-arg `setValue()` (clear), `restore(V prev)`, no-arg `restore()`; `reset()` is deprecated in favor of the entry/exit pair.
- **`ContextAccessor`** — the Map-like dual contract (e.g. for Reactor `Context`).
- **`ContextRegistry`** — the global singleton registry (`getInstance().registerThreadLocalAccessor(...)`), which also discovers accessors via `java.util.ServiceLoader`.
- **`ContextSnapshot`** — the immutable holder of captured values keyed by `key()`.

**The capture/restore lifecycle.** `ContextSnapshot.captureAll()` iterates every registered accessor, calls `getValue()`, and stores non-null values. On another thread, `snapshot.setThreadLocals()` returns an `AutoCloseable` `Scope`: on open it calls `setValue(captured)` (or no-arg `setValue()` to clear); on `close()` it calls `restore(previous)`. The try-with-resources `Scope` is what guarantees the borrowed thread is left pristine.

#### Hidden concept: `getValue()` is also the source of the "previous value"

During `setThreadLocals()`, the accessor's *current* `getValue()` on the target thread is what gets saved as the `previousValue` for `restore()` on close. The same method serves both "capture for export" and "snapshot-before-overwrite-for-restore"—subtle but central to leak-free borrowing.

**Why restore, not clear.** On a *borrowed/pooled* thread you cannot assume it was empty. Restoring the *previous* value (rather than blindly clearing) prevents cross-task contamination; the no-arg variants handle the "there was no value" case. This is why two-phase entry/exit replaced the old `reset()`.

**Design intent.** A single `ContextRegistry` of accessors lets one `captureAll()`/`setThreadLocals()` move *all* registered contexts atomically across any boundary—and the same snapshot can target a `ThreadLocal`, a Reactor `Context`, or a coroutine. The SPI was deliberately extracted into *Micrometer*, not Spring, so Reactor, Spring, and other ecosystems share one neutral contract. The stated philosophy: "imperative code should interact with `ThreadLocal` as usual, Reactor code with `Context` as usual"—adapt only at the seams, never force a rewrite.

#### Hidden concept: `ServiceLoader` auto-discovery

`ReactorContextAccessor` and `ObservationThreadLocalAccessor` are registered globally via `META-INF` `ServiceLoader` entries, so merely having the jars on the classpath makes propagation "just work"—which can baffle anyone debugging *why* context moves. (Also note `ContextSnapshotFactory`, a builder allowing a non-singleton registry, predicate-filtered capture, and control over clearing missing thread-locals—the configurable entry point most examples omit.)

### Reactor bridge and `ObservationThreadLocalAccessor`

Reactor ships `ReactorContextAccessor` (a `ContextAccessor`, `ServiceLoader`-loaded), treating the immutable Reactor `Context` as the source of truth. Default mode restores `ThreadLocal`s only in `handle()`/`tap()`; `Hooks.enableAutomaticContextPropagation()` makes **all** operators (even `map`) restore-and-clear at thread boundaries—but **hidden concept:** it only affects *new* subscriptions, so enabling it late silently does nothing for already-subscribed flows. (Boot 3.2+ replaced the manual hook with `spring.reactor.context-propagation=auto`.)

`ObservationThreadLocalAccessor` (KEY `"micrometer.observation"`) is **scope-based, not value-based**: `setValue(obs)` calls `obs.openScope()` (pushing onto a scope stack), `restore()` closes the current scope. **Hidden concept: `NullObservation` masking**—the no-arg `setValue()` opens a `NullObservation` scope rather than clearing, deliberately *masking* a leaked parent observation so a borrowed thread doesn't misattribute work to an unrelated trace. Mismatched open/close here is the classic source of "scope leak" warnings.

### `ContextPropagatingTaskDecorator`: wiring the SPI into Spring's executors

`ContextPropagatingTaskDecorator` (`org.springframework.core.task.support`, since Framework 6.1) is a `TaskDecorator` that, at submission time on the caller thread, does `ContextSnapshot.captureAll()` and wraps the `Runnable` to open `setThreadLocals()` before running and close the `Scope` after.

**Changed in v4/v7:** Boot 4 exposes `spring.task.execution.propagate-context` (default **false**); when true it auto-registers a `ContextPropagatingTaskDecorator` on the auto-configured `AsyncTaskExecutor`. Framework 7.0 also lets you declare **multiple `TaskDecorator` beans**, which Spring composes into a chain automatically (previously a single decorator), so the context decorator can coexist with others without manual composition.

**Design intent (opt-in by default).** Propagating context implicitly into every async task risks "data bleed" (a stale tenant/security context flowing where it shouldn't) and costs CPU per task. Spring keeps async threads *isolated by default* and asks you to consciously enable propagation—trading magic for predictability.

### v4/v7 propagation deltas and the virtual-thread future

- **Changed in v4/v7:** Framework 7.0 (GA 13 Nov 2025) adds **`PropagationContextElement`** (a kotlinx `ThreadContextElement`) for **Kotlin coroutines**, capturing/restoring Micrometer-registered contexts (and Reactor `Context` when `kotlinx-coroutines-reactor` is present) when a coroutine resumes on a thread—closing the prior gap where tracing context flowed through blocking and reactive code but *not* across coroutine suspension. Requires `io.micrometer:context-propagation` on the classpath.
- **Changed in v4/v7:** Null-safety migrated to **JSpecify** annotations (e.g. `@Nullable ThreadLocalAccessor.getValue` now carries JSpecify semantics).

#### Hidden concept: `ThreadLocal` hazards at virtual-thread scale

`InheritableThreadLocal` copies O(threads × variables) at thread creation—a heap-explosion vector with millions of virtual threads, and it silently becomes `null` in threads forked via `StructuredTaskScope.fork()`. Separately, the common idiom of per-thread caching of expensive objects (`SimpleDateFormat`, buffers) *silently stops optimizing* on virtual threads: because they aren't reused, the "cache" is created and discarded per task—no error, just lost optimization and GC pressure. Both are concrete reasons Spring keeps inheritance off, the ecosystem favors explicit capture/restore, and Java 21+ `ScopedValue` (immutable, bounded, leakage-free) is the structurally-safe successor the ecosystem is moving toward.

---

## Error Handling: the BeansException model and fail-fast-at-refresh

### Unchecked-by-design: the `BeansException` hierarchy

`BeansException extends NestedRuntimeException extends RuntimeException`. The javadoc is explicit: "this is a runtime (unchecked) exception. Beans exceptions are usually fatal; there is no reason for them to be checked." Direct subclasses include `BeanNotOfRequiredTypeException`, `FatalBeanException`, `NoSuchBeanDefinitionException`, `PropertyAccessException`, and `PropertyBatchUpdateException`. `NestedRuntimeException` supplies `getRootCause()`/`getMostSpecificCause()`, so the deepest real cause is always reachable through wrapping.

**Design intent.** Rod Johnson's original argument (Expert One-on-One J2EE): checked exceptions for *fatal* conditions are pure noise—the immediate caller cannot meaningfully recover from "no bean of type Foo," so forcing `try`/`catch` or `throws` clauses only adds boilerplate. Unchecked keeps business code clean and lets a *centralized* handler decide policy. The same value drives the parallel `DataAccessException` hierarchy below.

### The wrapping/nesting chain

Failures nest deliberately. `BeanCreationException` (a `FatalBeanException`) is the catch-all envelope for any failure while instantiating/populating a bean; it carries `beanName` + `resourceDescription`. A leaf bean's `BeanCreationException` is re-wrapped as the dependent's `UnsatisfiedDependencyException` (also a `BeanCreationException` subclass, capturing the `InjectionPoint`—which constructor arg/field/setter—and the underlying `BeansException`). `UnsatisfiedDependencyException` typically wraps a `NoSuchBeanDefinitionException` (zero candidates) or `NoUniqueBeanDefinitionException` (>1 candidate, where `NoUniqueBeanDefinitionException` is itself a subclass of `NoSuchBeanDefinitionException`).

**Design intent.** The leaf cause ("no bean of type Foo") is meaningless without the *path* that needed it. Nesting preserves both the WHERE (which bean/injection point) and the WHY (root cause), making the *legibility of failure* a first-class feature.

#### Hidden concept: chase `getMostSpecificCause()`, not the outermost message

Because every exception nests its cause, the useful error is often several wraps deep. `NestedRuntimeException.getMostSpecificCause()` jumps straight to the innermost non-Spring cause—invaluable when reading a twelve-frame `BeanCreationException` chain. Practitioners routinely fixate on the wrong (outermost) message.

#### Hidden concept: two distinct lifecycle phases

`BeanDefinitionStoreException` is thrown during the *definition-loading/parsing* phase (malformed config, unparseable `@Configuration`, factory-method resolution)—"your configuration is structurally invalid." `BeanCreationException` is thrown during the *instantiation* phase. A definition can be perfectly valid yet fail to instantiate; knowing which you got tells you whether your CONFIG or your CODE/wiring is broken.

### Circular references

`BeanCurrentlyInCreationException` signals a cycle detected mid-creation. **Pure constructor-injection cycles are unbreakable**—detected at load time and thrown. **Hidden concept:** setter/field-injection cycles *can* be broken via the early-singleton-reference cache (a half-initialized A is exposed to B), which is the mechanical reason setter cycles historically "worked" and constructor cycles never could—and why the docs recommend constructor injection. **Spring Boot disables breaking cycles by default since 2.6**, so even setter cycles fail-fast unless `spring.main.allow-circular-references=true`. (Note the distinction from the *benign* lenient-locking `BeanCurrentlyInCreationException` discussed earlier—a genuine constructor cycle is fatal; the lenient-locking variant is not.)

### Fail-fast-at-refresh

`ApplicationContext` implementations pre-instantiate singleton beans by default during `refresh()` (via `DefaultListableBeanFactory.preInstantiateSingletons()`), so missing/ambiguous/cyclic wiring throws *at startup, not on the first request.* The docs state the trade-off plainly: "At the cost of some upfront time and memory" you "discover configuration issues when the `ApplicationContext` is created, not later." `@Lazy` opts a bean out, deferring its potential failure to first use.

**Design intent.** A misconfiguration found at 3 a.m. on the first production request is vastly costlier than one found at deploy/boot. Paying upfront converts latent, request-time landmines into deterministic startup failures that CI/CD and health checks can gate on. The mental model: **a context that started successfully is fully wired.** The same fail-fast philosophy explains Boot disabling bean-definition *overriding* by default since 2.1 (`spring.main.allow-bean-definition-overriding=false`)—a duplicate bean name should be surfaced, not silently shadow another. `BeanDefinitionOverrideException` (a `BeanDefinitionStoreException` subclass) enforces this.

### Spring Boot's diagnostics layer: `FailureAnalyzer` → `FailureAnalysis`

`FailureAnalyzer` intercepts a startup `Throwable` and, if it recognizes it, returns a `FailureAnalysis(description, action, cause)`—turning a stack trace into a structured "Description: … / Action: …" report. `AbstractFailureAnalyzer<T extends Throwable>` matches a specific exception type; built-ins cover port-in-use, `NoSuchBeanDefinition`, `BeanCurrentlyInCreation`, `BeanDefinitionOverride`, etc.

```properties
# META-INF/spring.factories
org.springframework.boot.diagnostics.FailureAnalyzer=com.example.MyFailureAnalyzer
```

#### Hidden concept: `FailureAnalyzer`s run BEFORE the context is usable

They are **not beans** and cannot be `@Autowired`—they are instantiated reflectively during the failure-handling path, which is exactly why container state must arrive via **constructor arguments** (declare `BeanFactory`/`Environment` as constructor params; the old `BeanFactoryAware`/`EnvironmentAware` path was deprecated in Boot 2.7). **Hidden concept:** returning `null` is a chain-of-responsibility "pass"—the next analyzer gets a chance, and only the first non-null `FailureAnalysis` is reported.

**Changed in v4/v7:** `FailureAnalyzer` is **still** registered in `META-INF/spring.factories` under `org.springframework.boot.diagnostics.FailureAnalyzer` in Boot 4—a commonly misremembered point. Only the `EnableAutoConfiguration` key migrated to `META-INF/spring/...AutoConfiguration.imports` (back in Boot 3); diagnostics registration did *not* move.

**Design intent.** Separation of concerns: the framework throws *precise, machine-meaningful* exceptions; Boot owns the *human-facing presentation and remediation advice.* This keeps Spring Framework UI-agnostic while letting Boot ship an opinionated, actionable startup report. The "Action:" line embodies Boot's philosophy of telling developers what to *do*, not just what broke.

### DI-time exception translation

`PersistenceExceptionTranslationPostProcessor` is a `BeanPostProcessor` that, at container init, auto-proxies every `@Repository` bean; the proxy's advisor catches native persistence exceptions and runs registered `PersistenceExceptionTranslator`s to convert them into Spring's `DataAccessException` hierarchy. This is a *DI-time* concern (weaving happens during refresh) even though translation fires at runtime. `DataAccessException extends NestedRuntimeException` (unchecked); its javadoc explains the goal—let user code "react to an optimistic locking failure without knowing that JDBC is being used." Direct subclasses: `NonTransientDataAccessException`, `TransientDataAccessException`, `RecoverableDataAccessException`. Originals are wrapped, never lost.

#### Hidden concept: it's the proxy that translates

Translation is opt-in by `@Repository`, but the **proxy** does the work. If a repository is `final`, or a method is invoked self-internally (bypassing the proxy), translation silently does *not* happen—native exceptions leak where developers expected a `DataAccessException`. The classic AOP-proxy gotcha.

**Changed in v4/v7:** The `PersistenceExceptionTranslationPostProcessor` class and the `@Repository`→`DataAccessException` pipeline are unchanged. However, in Boot 4 the auto-configuration moved to a new `spring-boot-persistence` module (package `org.springframework.boot.persistence.autoconfigure`), and the enabling property was **renamed** from `spring.dao.exceptiontranslation.enabled` to **`spring.persistence.exceptiontranslation.enabled`** (default `true`); the old property is no longer supported.

### Unsatisfied `@Conditional` = graceful no-bean, NOT an error

When a `@Conditional` (or Boot's `@ConditionalOnProperty`/`@ConditionalOnMissingBean`/`@ConditionalOnClass`) evaluates false, the bean definition is simply *never registered*—no exception. `matchIfMissing` controls behavior when a property is absent. Visibility comes via the `ConditionEvaluationReport` (DEBUG log or `/actuator/conditions`), not thrown errors.

#### Hidden concept: condition-not-met and wiring-broken are distinct, time-separated failures

A false condition produces a *silent* no-bean at definition time. The pain appears *later*, as a `NoSuchBeanDefinitionException`, when something needed that bean. Debugging requires the `ConditionEvaluationReport` to see that the bean *backed off* rather than *broke*.

**Design intent.** Auto-configuration must compose dozens of *optional* modules where "absent" is normal, not exceptional. If a missing condition threw, every optional integration would be a startup landmine. Making it a non-event (with a discoverable report) is precisely what lets defaults back off to user beans and lets the same jar run with or without a given dependency—the engine of Boot's "back off gracefully" model.

### Other v4/v7 error-model notes

The `BeansException`/`DataAccessException` hierarchies, fail-fast-at-refresh, and `FailureAnalyzer` mechanics are essentially **unchanged** in Framework 7.x / Boot 4.x—mature, stable machinery. The peripheral deltas:

- **Changed in v4/v7:** Null-safety migrated from Spring's own `@Nullable` (JSR-305 semantics) to **JSpecify** annotations across the codebase; this touches API signatures (e.g. `@Nullable resourceDescription` on `UnsatisfiedDependencyException` constructors) but not runtime exception behavior.
- **Changed in v4/v7 (Framework 7):** Support for `javax.annotation.@Resource`, `javax.annotation.@PostConstruct`, and `javax.inject.@Inject` was **removed**—only the `jakarta.*` equivalents are supported. Relevant because `@PostConstruct`/`@Inject` failures flow through the same `BeanCreationException` path; the annotations merely changed package.
- **Changed in v4/v7 (Framework 7):** A new `BeanRegistrar` contract for programmatic bean registration, and Bean Overrides now support prototype/custom-scoped beans (previously singleton-only)—both touching the definition-registration phase guarded by `BeanDefinitionStoreException`/`BeanDefinitionOverrideException`.
- **Baseline (hedged):** Spring Boot 4 runs on Spring Framework 7 with a Java 17 minimum (Java 25 LTS recommended), requiring Java 21+ for virtual threads. Note that Boot 4.0 reached GA on **20 November 2025** (Framework 7.0 a week earlier, 13 November 2025)—not October 2025, as is sometimes stated.


---

## Spring Boot 4 / Framework 7: What Changed & Why

Spring Framework 7.0 reached GA on **2025-11-13** and Spring Boot 4.0 on **2025-11-20**. Treat these two as a single coordinated release train: Framework 7 supplies the IoC/DI core, and Boot 4 layers its conventions, auto-configuration, and module structure on top. This section is the map of what actually moved in the dependency-injection surface — and, more importantly, *why* each move was made and which mental model it is trying to install. A recurring theme below is that several changes widely attributed to v7 actually shipped in 6.2 or earlier; the genuinely new DI-relevant work is narrower and more deliberate than the blog-post folklore suggests.

### Platform baselines: JDK 17, Jakarta EE 11

**Changed in v4/v7:** The Jakarta EE baseline rises to **Jakarta EE 11** — Servlet 6.1, JPA 3.2, Bean Validation 3.1. The Java baseline, however, *stays* at **JDK 17 minimum**, with **JDK 25 (the current LTS) recommended** for production and Java 21+ encouraged to exploit virtual threads and the Class-File API. The system-requirements docs note compatibility up to Java 26.

The widely repeated "Boot 4 requires Java 21" claim from secondary blogs is **incorrect** — the Boot 4 migration guide and Framework 7 release notes both state Java 17 or later.

> Design intent: this is a deliberate decoupling of two upgrade pressures. The framework team did not want to force enterprises off JDK 17 *at the same time* as imposing the Jakarta EE 11 / Jackson 3 / module-restructuring churn. By holding the JDK floor at 17 while recommending 25, they keep alignment with the broad Java ecosystem (where many shops are still on 17) and preserve a smooth upgrade path, while still optimizing for and nudging toward the modern JVM. The mental model: *pick your JDK on your own schedule; the Jakarta and module changes are the real cost of this upgrade.*

### The javax → jakarta finalization (a DI-surface break)

**Changed in v4/v7:** The javax→jakarta migration begun in Framework 6 / Boot 3 is now *finalized*. Annotations in `javax.annotation` (`@Resource`, `@PostConstruct`, `@PreDestroy`) and `javax.inject` (`@Inject`, `@Named`) are **no longer supported**. You must use the `jakarta.*` equivalents:

```java
import jakarta.annotation.PostConstruct;   // not javax.annotation
import jakarta.inject.Inject;              // not javax.inject
import jakarta.inject.Named;
```

> Design intent: completing the namespace migration removes the dual-namespace maintenance burden and commits fully to the Jakarta governance model. Developers get **one canonical namespace** for DI and lifecycle annotations rather than a confusing dual-support window.

**Hidden gotcha — the JSR-330 silent break:** This directly touches the DI surface because `@Resource`, `@PostConstruct`, and especially `@Inject`/`@Named` are common *injection and lifecycle* annotations. The trap is libraries (and older internal code) that wired things with the **JSR-330** `javax.inject.@Inject`/`@Named` rather than Spring's own `@Autowired`/`@Component`. Those annotations now **silently stop working** — they are not Spring's, so there is no Spring-specific deprecation breadcrumb pointing at them. Grep your dependency graph for `javax.inject` and `javax.annotation` imports before upgrading.

### Null-safety: from Spring's own annotations to JSpecify

**Changed in v4/v7:** The framework's null-safety contract migrated off Spring's own `@Nullable` / `@NonNull` / `@NonNullApi` / `@NonNullFields` (JSR-305-style, declaration-level) to **JSpecify** (`org.jspecify.annotations.*`). Spring's own annotations are now **deprecated as of 7.0**.

The two mechanical differences that matter:

1. **JSpecify `@Nullable`/`@NonNull` are `TYPE_USE` annotations**, so placement is *part of the type* and changes meaning, not just position.
2. **`@NullMarked` (in `package-info.java`) sets a package-level non-null default**, after which only the exceptional nullable cases need annotating.

```java
// package-info.java
@NullMarked
package com.example.service;
import org.jspecify.annotations.NullMarked;
```

```java
// type-use placement: the @Nullable now sits on the type, after the modifier
private @Nullable String name;

// these are now genuinely different types:
Object @Nullable [] a;     // the array itself may be null
@Nullable Object[] b;      // the array is non-null; its elements may be null

// and generics are finally expressible:
List<@Nullable String> values;
```

> Design intent: Spring's annotations were, per the team, created "when JSpecify did not exist" and were "the best option at that time," but carried JSR-305 baggage. JSpecify provides a properly specified standard, a single canonical dependency with no split-package issues, better tooling (NullAway), and — the decisive win — **type-use granularity** so nullability of generics, array-elements-vs-the-array, and varargs is expressible. The mental model deliberately inverts: **`@NullMarked` once per package, then non-null is the default and you annotate only the rare `@Nullable`** — because in real code non-null usage vastly outnumbers nullable.

**Hidden concept — `@NullMarked` is the actual change, not the rename.** Practitioners who migrate by mechanical find/replace of the annotation often forget to add `@NullMarked` at the package-info level, leaving the contract *unenforced* — the annotations are present but nothing flips the default.

**Hidden concept — placement silently changes the contract.** Because JSpecify is `TYPE_USE`, a naive replacement of Spring's `@Nullable Object[]` with JSpecify `@Nullable` can quietly swap "array of nullable elements" for "nullable array" (or vice-versa) for arrays and varargs. This is a semantic change disguised as a textual one.

**Hidden concept — Kotlin signatures can change under you.** JSpecify annotations are auto-translated into Kotlin null-safety (the JSR-305 ones were not always honored). Upgrading can therefore turn a previously *platform-type* Kotlin signature into a hard nullable or non-null one, producing **compile errors in downstream Kotlin code even when you changed nothing on your side**. The payoff is the point: ending the long-standing JSR-305 ↔ Kotlin interop friction.

### BeanRegistrar / BeanRegistry: first-class programmatic registration

**Changed in v4/v7:** A new `@FunctionalInterface`, `org.springframework.beans.factory.BeanRegistrar`, with a single method:

```java
void register(BeanRegistry registry, Environment env);
```

It is contributed to the context via `@Import(MyBeanRegistrar.class)` on a `@Configuration` class (and is gateable with `@Conditional`). `BeanRegistry` exposes `registerBean(name, type)` and `registerBean(type, spec -> ...)` with a fluent `BeanSpec` (`prototype()`/`singleton()`, `lazyInit()`, `description(...)`, `supplier(context -> ...)`), and `context.bean(Type.class)` to resolve dependencies *inside* a supplier:

```java
class MyRegistrar implements BeanRegistrar {
    public void register(BeanRegistry registry, Environment env) {
        registry.registerBean("bar", Bar.class, spec -> spec
            .prototype()
            .lazyInit()
            .description("a programmatically registered bar")
            .supplier(context -> new Bar(context.bean(Foo.class))));

        if (env.matchesProfiles("baz")) {              // arbitrary control flow
            for (String n : someRuntimeList()) {
                registry.registerBean(n, Widget.class);
            }
        }
    }
}
```

A Kotlin `BeanRegistrarDsl` provides reified `registerBean<Foo>()` and `profile("baz") { ... }` blocks.

> Design intent: this is positioned explicitly as **the bridge between the convenience of `@Bean` and the power of programmatic registration**. The pre-existing options were each awkward in a different way. `BeanDefinitionRegistryPostProcessor` and `ImportBeanDefinitionRegistrar` operate at the low-level `BeanDefinition` layer; `@Bean` methods cannot cleanly loop or branch over runtime data. `BeanRegistrar` reads like ordinary imperative code (`if`/`for`), resolves dependencies lazily via `context.bean(...)`, respects profiles via `env.matchesProfiles(...)`, and — crucially — is **explicitly designed to be AOT / native-image analyzable, including its instance suppliers**. It is the sanctioned, AOT-friendly replacement for the older escape hatches, aligned with Spring's AOT / GraalVM / Leyden direction.

**Hidden concept — suppliers do not autowire; you pull.** Inside a `BeanRegistrar` supplier you do *not* get constructor autowiring. You explicitly retrieve collaborators with `context.bean(Type.class)`. This is a different style from `@Bean`-method parameter injection, and it is intentional: explicit pulls keep the wiring statically analyzable for AOT, which arbitrary reflective autowiring inside a lambda would not be.

### @Bean precise return types (AOT correctness)

**Changed in v4/v7 (emphasis, not new API):** Best practice is now strongly stated: declare the **most precise / concrete return type** on `@Bean` factory methods.

```java
@Bean
MyServiceImpl myService() {       // prefer this
    return new MyServiceImpl();
}

@Bean
MyService myService() {           // avoid: interface return type
    return new MyServiceImpl();
}
```

> Design intent: the AOT engine inspects the *declared* bean type to detect `@Autowired` members and lifecycle callbacks. Because the engine refreshes the context **without instantiating beans**, it cannot inspect a runtime instance to recover the concrete class — it only sees what you declared. An interface return type can therefore cause `@Autowired`-injection or destroy-method post-processing to be **silently skipped** in a native image. This is guidance and behavioral emphasis rather than a new annotation, but it is load-bearing.

**Hidden concept — this is a silent, runtime-only native failure.** Returning an interface produces no compile error and behaves fine on the regular JVM; it manifests only as a missing injection or un-invoked lifecycle method *in the native image*. That asymmetry is exactly why the guidance is now stressed so loudly.

### Boot 4 module restructuring: `spring-boot-<technology>`

**Changed in v4:** Boot modules are renamed to `spring-boot-<technology>` with packages `org.springframework.boot.<technology>`; every technology gets a dedicated `spring-boot-starter-<technology>`, and test modules become `spring-boot-<technology>-test`. Two further structural moves:

- `EnvironmentPostProcessor` moved from `org.springframework.boot.env` to `org.springframework.boot`.
- **Public members (except constants) were removed from auto-configuration classes.**

A practical consequence: features that previously activated off the mere presence of a third-party jar (e.g. Flyway, Liquibase) **may now require an explicit starter**.

> Note on the application annotations: the broad modularization is real, but `@SpringBootApplication` and `@EnableAutoConfiguration` did **not** change package — they remain at `org.springframework.boot.autoconfigure.*`, as they were in Boot 3.x. What moved was the *build directory* of the autoconfigure module and the splitting of the formerly monolithic autoconfigure jar into many technology-specific modules. `spring-boot-autoconfigure` is also no longer intended as a direct public dependency, and a `spring-boot-autoconfigure-classic` compatibility module was added.

> Design intent: the restructuring makes each integration an **explicit, independently versioned, separately startable unit**. Removing public members from auto-configuration classes reinforces a principle the team has long wanted to enforce: **auto-configuration is implementation detail, not public API**. That gives the team freedom to evolve auto-config internals and pushes users toward stable contracts and *explicit* starters rather than relying on transitive-jar "magic."

**Hidden concept — code that reached into auto-config beans breaks by design.** Any code that treated an auto-configuration class's public members as API will break in Boot 4. This is intentional: it is the enforcement mechanism for the "auto-config is not API" principle, not an accident.

### Removed and deprecated infrastructure

**Changed in v4/v7:** Several pieces of supporting infrastructure that touch configuration and wiring are gone or deprecated:

- `spring-jcl` **removed** in favor of Apache Commons Logging 1.3.x.
- `ListenableFuture` **removed** — use `CompletableFuture`.
- **OkHttp3, Theme support, and Undertow removed** (Undertow is Servlet 6.1-incompatible).
- **Jackson 2.x deprecated** in favor of Jackson 3.x.

> Migration model: in Boot 4, **classes/methods/properties that were deprecated across the Boot 3.x line are removed** — so you must clear deprecation warnings *before* upgrading. The recommended staged path is **2.7 → 3.5 → 4.0**: upgrade to the latest 3.5.x, resolve every deprecation warning, then move to 4.0/7.0.

> A precision caveat for the *Framework 7* core specifically: Framework 7 does **not** hard-remove *every* deprecated API. Per the framework's own tracking, 7.0 aims to remove APIs marked deprecated-*for-removal* as of 6.2.0; plain deprecations and anything deprecated after 6.2.0 are expected to survive at least through the 7.0.x line. The blanket "every deprecation is removed" framing is accurate for Boot 4's property/class surface but overstated for Framework 7 core DI.

### Myth-busting: what did *not* change in v4/v7

A large fraction of "new in v7" claims are misattributions. The findings flag these explicitly:

- **Bean-definition overriding is *not* a v4 change.** It has defaulted to **disabled since Boot 2.1** and remains off by default in Boot 4. Re-enable with `spring.main.allow-bean-definition-overriding=true`. This is unchanged, not new.
- **`@Fallback` and `@Bean(bootstrap=BACKGROUND)` are from Framework 6.2**, not 7.0 (`@Fallback` and `BeanDefinitionBuilder.setFallback()` carry `@since 6.2`).
- **The `Runner` marker interface** that both `ApplicationRunner` and `CommandLineRunner` extend, and the single-ordered-stream `callRunners()` that interleaves both kinds by `@Order`, are **not new in v4/v7**. The marker interface arrived in Boot 2.7, and the two runner kinds have been interleaved in one sorted collection since at least Boot 2.6 — there was never a recent "ordered within separate groups" regime to change from.
- **The `ReentrantLock`-based `startupShutdownLock`** (with `startupShutdownThread` and the shutdown hook's `isStartupShutdownThreadStuck()` interrupt/abandon-on-`WAITING` logic) is **not a v7 change** — it landed in **Framework 6.1** (Boot 3.2), replacing the older `synchronized` `startupShutdownMonitor`. In v7 it is inherited, unchanged behavior.
- **`SmartLifecycle.stop(Runnable)`'s "ordered, and potentially concurrent, shutdown of all components having a common shutdown order value"** is original behavior **since Spring 3.0**, not new. The recent lifecycle additions were custom per-shutdown-phase timeouts (6.2) and concurrent *startup* of specific phases (6.2.6) — not concurrent shutdown, and not in 7.0. Note also that `stop(Runnable)` is declared on `SmartLifecycle`/`Lifecycle`; `DefaultLifecycleProcessor` merely invokes it (via a per-phase `CountDownLatch`).
- **The `AnnotationUtils` traversal methods** (`getAnnotations`, `getRepeatableAnnotations`, `isAnnotationMetaPresent`, `findAnnotationDeclaringClass`, etc.) being deprecated "superseded by the `MergedAnnotations` API" is **not a v7 change** — they carry `@Deprecated(since = "5.2")`. `findAnnotation`/`getAnnotation`/`synthesizeAnnotation` remain. v7 merely continues a long-standing deprecation.
- **`Introspector.decapitalize` → `StringUtils.uncapitalizeAsProperty`** for bean names landed in **Framework 6.0** (Boot 3.0), not v7.

### Adjacent v7 changes worth knowing (with one correction)

A few items sit just outside core DI but are commonly conflated with it:

**Proxying — `@Proxyable` and consistent global defaulting.** **Changed in v7:** the *global* proxy-type default (whatever it is in a given setup) is now **consistently applied to all proxy processors**, including `@Async`/`@EnableAsync` — previously some processors independently chose JDK proxies regardless of the global setting. You can opt out per bean with the **new `@Proxyable` annotation**: `@Proxyable(INTERFACES)` to force JDK-interface proxying against a CGLIB default, or `@Proxyable(TARGET_CLASS)` against the regular JDK default.

> Correction to a common overstatement: this does **not** mean "CGLIB is now the global default everywhere." Per the reference docs, the **core framework still suggests interface-based (JDK) proxies by default**; it is **Spring Boot** that — depending on configuration properties — enables class-based (CGLIB) proxies by default. v7 changed the *consistency of the defaulting mechanism*, not the core default.

**Persistence exception translation — renamed property.** **Changed in v4:** the post-processor toggle was renamed. The old `spring.dao.exceptiontranslation.enabled` is no longer supported; use **`spring.persistence.exceptiontranslation.enabled`** (default `true`), and the auto-configuration moved to the new `spring-boot-persistence` module (`org.springframework.boot.persistence.autoconfigure`). The underlying `PersistenceExceptionTranslationPostProcessor` and the repository-to-`DataAccessException` pipeline are unchanged.

**Component index — still works, but the build-time processor is on its way out.** `@Component` is still meta-annotated with `@Indexed`, and `CandidateComponentsIndexLoader` still consumes `META-INF/spring.components` at runtime. But the **build-time annotation processor** `CandidateComponentsIndexer` (the `spring-context-indexer` module) is `@Deprecated(since = "6.1", forRemoval = true)` "in favor of the AOT engine and a forthcoming AOT-generated components index." In 7.0 the runtime index classes were actually *extended* with new `@since 7.0` programmatic APIs (e.g. `registerScan`, `registerCandidateType`, `CandidateComponentsIndexLoader.addIndex`) precisely to support AOT-populated indexes. So: file-based loading still functions, but the annotation-processor path is deprecated-for-removal and AOT is the modern replacement for build-time discovery.

**Other headline v7 features adjacent to DI** (surfaced in release notes, not detailed here): `@Retryable`/`@ConcurrencyLimit` and `RetryTemplate` in `org.springframework.core.retry`; first-class API Versioning in MVC/WebFlux; declarative HTTP Service Client auto-configuration in Boot 4; `CompositeTaskDecorator` when multiple `TaskDecorator` beans exist; and the deprecation of JUnit 4 support in the TestContext framework.

### The through-line

Read together, the v4/v7 DI changes point one direction: **shifting container work from runtime to build time, and making the bean graph statically knowable.** JSpecify gives the type system a precise nullability contract; `BeanRegistrar` gives programmatic registration an AOT-analyzable form; the `@Bean` precise-return-type guidance exists because the AOT engine reads declarations rather than instances; the module restructuring makes each integration an explicit, analyzable unit and demotes auto-config from API to implementation detail. The mental model Spring is installing is consistent across all of them: **declare precisely and explicitly, because the framework increasingly resolves your intentions at build time, where it can no longer ask a running instance what you meant.**


---

I'll write this section based strictly on the verified research findings, applying the adversarial corrections.

## Deeper & Hidden Concepts

This section descends below the declarative `@Autowired`/`@Bean` surface into the engine that actually realizes a Spring context: the container object model, the per-bean lifecycle, the candidate-resolution and ordering machinery, the indirection contracts (`FactoryBean`, `ObjectProvider`), and the build-time AOT pipeline. The recurring design tension throughout Spring Framework 7 / Spring Boot 4 is **runtime dynamism versus build-time determinism** — almost every "hidden" mechanism here is either a lever for one or a bridge between the two.

### The container object model: engine vs. façade, and the three data strata

The single most clarifying mental model is that **`DefaultListableBeanFactory` is the real IoC engine, and `ApplicationContext` is a façade that *composes* (HAS-A) exactly one of them** — it does not subclass it. `GenericApplicationContext.getBeanFactory()` is declared `public final ConfigurableListableBeanFactory getBeanFactory()` and returns "the single internal BeanFactory held by this context… never null." `DefaultListableBeanFactory` is described in its own javadoc as "a full-fledged bean factory based on bean definition metadata, extensible through post-processors," implementing `ConfigurableListableBeanFactory`, `BeanDefinitionRegistry`, `AutowireCapableBeanFactory`, `SingletonBeanRegistry`, and more.

**Design intent.** This is separation of concerns made structural. The bean-definition table plus the wiring/instantiation algorithm is a self-contained, reusable, format-agnostic engine; context-level concerns (events, i18n, resource loading, lifecycle, environment) are orthogonal decoration. Composition lets the *same* engine back annotation, XML, functional, and web context flavours, and lets advanced users reach through `getBeanFactory()` for full control. The rejected alternative — one monolithic context class — would have entangled metadata management with application services and prevented standalone reuse of the factory.

A deliberate consequence: a **plain `DefaultListableBeanFactory` is "dumb" about special beans**. It does *not* auto-detect `BeanPostProcessor` or `BeanFactoryPostProcessor` instances — you must wire them by hand (`factory.addBeanPostProcessor(...)`, or manually invoke `cfg.postProcessBeanFactory(factory)`). The `ApplicationContext` is precisely the layer that adds, *by convention*, automatic post-processor registration, lifecycle management, `MessageSource` access, and `ApplicationEvent` publication. The engine is mechanism; the context is policy.

The data model has **three distinct strata, each with its own extension point**:

1. **`BeanDefinition`** — the editable recipe (class, scope, constructor args, property values, init/destroy methods, `depends-on`, attributes). This is what a **`BeanFactoryPostProcessor`** mutates, before any instantiation.
2. **Merged `RootBeanDefinition`** — the read-only, parent-flattened view the engine actually instantiates from, obtained via `getMergedBeanDefinition(String)`. This is what a **`MergedBeanDefinitionPostProcessor`** sees.
3. **The bean instance** — the realized object, which a **`BeanPostProcessor`** wraps/replaces (e.g. AOP proxying).

The three extension points map 1:1 onto the three strata, and *that layering is the contract* — it is the load-bearing reason the post-processor model composes so predictably.

#### Hidden: the merged definition is a recomputed *copy*, not your registered object

`getBeanDefinition()` returns the original `GenericBeanDefinition`/`ChildBeanDefinition` you registered; the engine instantiates from a *separate* `RootBeanDefinition` cached in a `ConcurrentHashMap` (`mergedBeanDefinitions`). `getMergedLocalBeanDefinition` returns the cached copy **only if it is non-null and not `stale`**; otherwise it recomputes by recursively merging the parent template via `overrideFrom(bd)` so child values win. Mutating the registered definition after merge has no effect unless the merged cache is invalidated.

That invalidation is itself subtle: `clearMergedBeanDefinition(name)` sets `stale=true` *rather than removing the entry* — a lock-light strategy that lets concurrent readers still get a soon-recomputed value. And `markBeanAsCreated()` freezes a bean's merged definition the first time it is touched, which is the implicit "no more `BeanFactoryPostProcessor` edits" boundary.

`MergedBeanDefinitionPostProcessor.postProcessMergedBeanDefinition` is the **third, most-overlooked hook**: this is where `AutowiredAnnotationBeanPostProcessor`, `CommonAnnotationBeanPostProcessor`, `InitDestroyAnnotationBeanPostProcessor`, and the scheduling/JMS processors *discover and cache injection metadata* — against the merged definition, before population. That is why `@Autowired`/`@PostConstruct`/`@Resource` detection runs even before a bean is populated.

#### Hidden: abstract parent templates and the `abstract=true` footgun

A child definition always takes `depends-on`, autowire mode, dependency-check, singleton, and lazy-init **from the child**, while inheriting scope, constructor args, property values, and method overrides from the parent. An `abstract="true"` parent is a pure template, never instantiated. The classic startup surprise: a parent that specifies a class but forgets `abstract=true` is a perfectly valid concrete singleton and *will be eagerly pre-instantiated* — with whatever side effects that entails.

#### Bean overriding

`registerBeanDefinition(name, bd)` keys definitions by name; `setAllowBeanDefinitionOverriding(boolean)` controls whether re-registering a name replaces the prior definition. Core framework historically allows overriding (logged at INFO); Spring Boot defaults `spring.main.allow-bean-definition-overriding` to `false` (since 2.1, still the default in Boot 4), and the reference docs now state "Bean overriding will be deprecated in a future release." A quiet exception: a `@Bean` method silently overrides a component-scanned class of the same component name when return types match.

> **Changed in v4/v7:** A known v7 issue (#36648) initially caused beans registered via `BeanRegistrar` or in a `GenericApplicationContext` initializer to not honor the default `allow-bean-definition-overriding` setting. Also note `getBeanFactory()` and the hierarchy/nullness APIs are now JSpecify-annotated.

### The `preInstantiateSingletons` refresh loop

`finishBeanFactoryInitialization()` calls `DefaultListableBeanFactory.preInstantiateSingletons()`, which runs **two passes over a defensive copy** of `beanDefinitionNames` (the copy lets an init-method register new definitions mid-bootstrap without `ConcurrentModificationException`):

- **Phase 1:** for each name, resolve the merged `RootBeanDefinition`; if `!isAbstract() && isSingleton()`, call `preInstantiateSingleton`. Inside, `isLazyInit()` gates eager creation — a lazy singleton is registered but not built.
- **Phase 2 (separate loop):** for each singleton instance that is a `SmartInitializingSingleton`, fire `afterSingletonsInstantiated()`.

**Design intent — fail-fast by default.** Eagerly pre-instantiating all non-lazy singletons front-loads wiring, validation, and proxy creation so a misconfigured app *refuses to start* rather than failing on first request in production. Lazy is the opt-out precisely to preserve "a context that started is a context that works."

The two passes are **not** an accident. `SmartInitializingSingleton.afterSingletonsInstantiated()` (since 4.1) is guaranteed to run only after *every* eager singleton exists, so a bean can safely call `getBeansOfType()` to aggregate collaborators without triggering accidental early instantiation of half-built peers. The mental model it instills: do *per-bean* setup in `@PostConstruct`; do *registry-wide aggregation* in `afterSingletonsInstantiated`. It is **not** fired for lazy-on-demand singletons or any non-singleton scope.

> **Changed in v4/v7 (inherited from 6.2):** Phase 1 now collects `CompletableFuture`s for `@Bean(bootstrap = BACKGROUND)` beans and `CompletableFuture.allOf(...).join()`s them before refresh completes. Background init runs `instantiateSingletonInBackgroundThread` on a `bootstrapExecutor` (Spring Boot 3.5+/4 auto-configures a `bootstrapExecutor` delegating to `applicationTaskExecutor`). Each `afterSingletonsInstantiated` call is wrapped in an `ApplicationStartup` step `spring.beans.smart-initialize`.

### Full lifecycle callback ordering

Inside `AbstractAutowireCapableBeanFactory.initializeBean`, the exact sequence is:

```
invokeAwareMethods
  -> applyBeanPostProcessorsBeforeInitialization   (skipped if mbd.isSynthetic())
  -> invokeInitMethods
  -> applyBeanPostProcessorsAfterInitialization
```

`invokeAwareMethods` handles **exactly three** "factory-tier" interfaces, in order: `BeanNameAware` → `BeanClassLoaderAware` (only if a classloader is set) → `BeanFactoryAware`. These are knowable by *any* bean factory with zero `ApplicationContext` infrastructure, so they are hardwired.

#### Hidden: the seven *context*-level Aware callbacks fire *during* before-init

The popular summary "Aware, then BPP-before-init" is slightly wrong. `ApplicationContextAwareProcessor` is **itself a `BeanPostProcessor`**, registered first in `prepareBeanFactory`, whose `postProcessBeforeInitialization` invokes `EnvironmentAware` → `EmbeddedValueResolverAware` → `ResourceLoaderAware` → `ApplicationEventPublisherAware` → `MessageSourceAware` → `ApplicationStartupAware` → `ApplicationContextAware`. So the context awares are physically a *sub-step* of the first before-init pass.

**Design intent.** This dogfoods the public extension point: the framework injects its own context callbacks the same way it asks third parties to extend beans (a `BeanPostProcessor`). It keeps the minimal `spring-beans` kernel decoupled from the `spring-context` layer — the rejected alternative (branching inside `initializeBean` per context interface) would couple the two modules. `prepareBeanFactory` correspondingly calls `ignoreDependencyInterface(...)` for the seven awares (so a `setApplicationContext` setter is *not* an autowire injection point) while `registerResolvableDependency(...)` still makes `ApplicationContext`/`BeanFactory`/etc. injectable *by type* — a clean split between "callback injection" and "dependency injection" of the same objects.

#### Hidden: `@PostConstruct` is not handled by `invokeInitMethods`

`@PostConstruct` is invoked by `CommonAnnotationBeanPostProcessor` during the *before-init* phase, which is why the documented order is `@PostConstruct` → `InitializingBean.afterPropertiesSet` → custom `init-method`. `invokeInitMethods` runs `afterPropertiesSet()` then loops the custom init-method names, **deduplicating** a custom method named `afterPropertiesSet` when the bean is an `InitializingBean` (so a same-named method runs once). Annotation-discovered init methods are recorded as "externally managed" to avoid re-running them as custom init-methods.

#### Hidden: init runs *under* the singleton lock; heavy work belongs after

The reference is explicit: `@PostConstruct` and init methods execute **within the container's singleton creation lock**, and a bean is published only after they return. This guarantees visibility of fields set during init across threads (effectively-final semantics) but invites lock-ordering deadlocks if init code accesses other beans. The sanctioned escape hatch for cross-bean or I/O-heavy work is `SmartInitializingSingleton.afterSingletonsInstantiated()` (or a `ContextRefreshedEvent` listener), which the docs guarantee run **after all regular singleton init and outside any singleton lock**.

### Singleton creation locking: strict vs. lenient

> **Changed in v4/v7 (introduced in 6.2, carried into 7.0):** The container moved from a single global `synchronized` mutex on the singletons map to `DefaultSingletonBeanRegistry.singletonLock` — a `ReentrantLock` — plus a separate `lenientCreationLock`/`Condition`. (It is *not* a `StampedLock`; the "tryLock-with-fallback" pattern is the StampedLock-*style* spirit, not the class.) `getSingletonMutex()` is deprecated since 6.2 and now returns a throwaway `new Object()`, meaning external code that synchronized on it silently loses coordination.

The pivotal hook is `protected @Nullable Boolean isCurrentThreadAllowedToHoldSingletonLock()` (since 6.2): `null` = traditional forced full lock; `true` = may lock but accepts lenient fallback if `tryLock()` fails; `false` = forced lenient. `DefaultListableBeanFactory` overrides it during the pre-instantiation phase: the **main** bootstrap thread is strict, **background** init threads (`@Bean(bootstrap=BACKGROUND)`) are always lenient, and unmanaged threads are *inferred* from a thread-name-prefix comparison against the captured `mainThreadPrefix`.

`getSingleton(beanName, ObjectFactory)` computes `acquireLock = !Boolean.FALSE.equals(lockFlag)` and `locked = acquireLock && singletonLock.tryLock()`. On contention, a `true`-flagged thread *creates the bean leniently outside the lock* (tracked in `singletonsInLenientCreation`, with a hand-rolled deadlock detector via `lenientWaitingThreads` and `checkDependentWaitingThreads`) rather than blocking; a `null`-flagged thread blocks on `lock()`.

**Design intent.** The old single global lock held during user code (factory callbacks, `@PostConstruct`) could deadlock when that code spawned a re-entrant thread or two threads cross-checked types (issues #23501, #30887). Simply removing the lock caused races. The middle path — keep a lock for safe publication, but allow non-blocking `tryLock` with controlled lenient fallback — lets a contending thread build the bean itself rather than wait on a lock held by user code, enabling parallel background startup. The trade-off is explicit: lenient mode can surface a `BeanCurrentlyInCreationException` the main thread now *tolerates and skips* (issue #34349, 6.2.3), trusting another thread to finish.

> **Changed in v4/v7:** `spring.locking.strict=true` (`STRICT_LOCKING_PROPERTY_NAME`, since 6.2.6, read once via the tri-state `SpringProperties.checkFlag`) restores pre-6.2 full locking. The default in 7.0 remains *inferred* — it was **not** flipped to strict-by-default.

### Circular-reference resolution: the three-level singleton cache

`DefaultSingletonBeanRegistry` holds three `ConcurrentHashMap`s with strict promotion ordering: `singletonObjects` (256, fully initialized) → `earlySingletonObjects` (16, half-built instances) → `singletonFactories` (16, `ObjectFactory` callbacks). A name advances factory → early → final. The lock-free fast read path does `singletonObjects.get(name)` with **no lock at all**, only `tryLock`ing to build an early reference.

In `doCreateBean`, after instantiation but before population:

```java
boolean earlySingletonExposure =
    mbd.isSingleton() && this.allowCircularReferences && isSingletonCurrentlyInCreation(beanName);
if (earlySingletonExposure)
    addSingletonFactory(beanName, () -> getEarlyBeanReference(beanName, mbd, bean));
```

**Why register a *factory*, not the raw instance** — this is the high-design-intent choice. The middle `ObjectFactory` level defers computing the early reference so that AOP/post-processors (`SmartInstantiationAwareBeanPostProcessor.getEarlyBeanReference`, e.g. `AbstractAutoProxyCreator`) can produce the *eventual proxy* lazily and exactly once, recording it in `earlyProxyReferences` so `postProcessAfterInitialization` won't double-wrap. If no cycle materializes, the factory is never called and no premature proxy is built. The early reference and the final singleton thereby become the *same* object whenever possible.

**Constructor cycles are deliberately unresolvable.** A constructor argument must exist before `this` references a live object, so there is no half-built instance to expose; the early-exposure factory is only registered *after* instantiation. Spring fails fast with `BeanCurrentlyInCreationException` rather than papering over a design smell — reinforcing "prefer constructor injection, because it makes illegal cycles impossible to express."

#### Hidden: `BeanCurrentlyInCreationException` has two distinct origins

Same exception type, completely different diagnosis: (1) **genuinely unresolvable cycle** — re-entrant creation fails `singletonsCurrentlyInCreation.add(beanName)` (the classic constructor case); (2) **resolved-but-inconsistent** — after init, `getSingleton(beanName, false)` finds the final `exposedObject` differs from the raw bean (it got wrapped), `allowRawInjectionDespiteWrapping` is `false`, and the bean has real dependents, so it throws because those dependents would hold a stale, *un-proxied* reference (bypassing `@Transactional`/security advice). The message points at "over-eager type matching" — type probes via `getBeanNamesForType(..., allowEagerInit=true)` can wrap the bean at the wrong moment; `removeSingletonIfCreatedForTypeCheckOnly` exists to evict such probe instances.

> **Changed in v4/v7:** An AOT-optimized context (the default for native/optimized Boot 4 builds) has **no early-exposure machinery** — beans come from generated instance suppliers — so even setter/field cycles that *work* on the JVM **fail at startup** under AOT. The reference prescribes `@Lazy` or `ObjectProvider` to break them. The core framework still defaults `allowCircularReferences=true`; Spring Boot defaults `spring.main.allow-circular-references=false` (since 2.6, unchanged in Boot 4).

### `FactoryBean` and `SmartFactoryBean` indirection

A bean implementing `FactoryBean<T>` is registered under its name, but `getBean(name)` resolves to `factory.getObject()` (the product, type `T`); the factory itself is reachable only via the `&` dereference prefix (`BeanFactory.FACTORY_BEAN_PREFIX`). `transformedBeanName` strips `&` (and resolves aliases) so factory and product share **one canonical definition name**. The `&`-branch in `getObjectForBeanInstance` throws `BeanIsNotAFactoryException` if you dereference a non-factory.

**Design intent.** The mental model: a bean name denotes a *usable collaborator*, regardless of how it was built. The factory is a construction detail consumers should never couple to — so dereferencing is deliberately made awkward (a sigil) to discourage it. `FactoryBean` is a *programmatic, annotation-free* contract precisely because `getObjectType()`/`getObject()` "may arrive early in the bootstrap process, even ahead of any post-processor setup," so it cannot rely on annotation-driven injection. This is why framework/library authors use it for complex stateful construction (50+ ship in Spring: `ProxyFactoryBean`, `JndiObjectFactoryBean`, …), while ordinary instantiation logic belongs in `@Bean` methods that run inside a fully-initialized container.

The singleton product lives in a **separate `factoryBeanObjectCache`** (`ConcurrentHashMap`, cap 16), distinct from `singletonObjects` which holds the factory. So one canonical name maps to two cached objects. Caching happens only when `factory.isSingleton() && containsSingleton(beanName)`, guarded by a **double-checked** read/create/re-read (the second read catches an `alreadyThere` value populated by a re-entrant `getBean` during `getObject()`, preserving singleton identity).

#### Hidden: two separate locks, and the orphaned product lifecycle

`getObject()` rides the **registry's** `singletonLock` *and* an additional defensive `synchronized(factory)` monitor — two distinct guards. The verbatim rationale: defend against "non-thread-safe `FactoryBean.getObject()` implementations, potentially to be called from a background thread while the main thread currently calls the same `getObject()` method within the singleton lock." The container manages **only the factory's** lifecycle; the *product* is orphaned by design — a `Closeable`/pooled `DataSource` product's `close()` is **not** called automatically, so the `FactoryBean` must implement `DisposableBean` and delegate. A `null` product is wrapped in an internal `NullBean` sentinel (or, if returned while in creation, raises `BeanCurrentlyInCreationException`).

`getObjectType()` exists so the container can do **type matching without calling `getObject()`**; returning `null` makes the factory *invisible to autowiring* (graceful degradation, not an error). `OBJECT_TYPE_ATTRIBUTE` (since 5.2) and, for AOT, `RootBeanDefinition.setTargetType(ResolvableType...)` let the product type be declared statically when it can't be deduced. Determining "is this a `FactoryBean`?" forces early bean-`Class` resolution via `predictBeanType`, memoized onto the merged definition.

During `preInstantiateSingletons`, `instantiateSingleton` always creates the factory (`getBean("&" + name)`) but creates the **product eagerly only if it is a `SmartFactoryBean` with `isEagerInit()==true`** — plain factory products stay lazy. `SmartFactoryBean.isPrototype()` is *not* the inverse of `isSingleton()`: a scoped product can be `isSingleton()==false` yet `isPrototype()==false`.

> **Changed in v4/v7:** `SmartFactoryBean` gains `boolean supportsType(Class<?>)` and `<S> S getObject(Class<S>)` (both `@since 7.0`), enabling *type-aware* injection where one factory exposes multiple product types (e.g. Hibernate `LocalSessionFactoryBean` exposing transactional `Session` and `StatelessSession`, mirroring JPA 3.2 `EntityManager` injection). `isTypeMatch` now consults `supportsType(...)` *before* `getObjectType()`. Crucially, `SmartFactoryBean` singletons are now **excluded from both `factoryBeanObjectCache` and the defensive `synchronized(factory)`** — the contract shifts thread-safety onto the implementer in exchange for multi-type support.

### `ObjectProvider` / `ObjectFactory` / `jakarta.inject.Provider`

`ObjectProvider<T>` (since 4.3) `extends ObjectFactory<T>, Iterable<T>` — and deliberately **does *not* extend `java.util.function.Supplier`**. It is the consumer-side lookup API for *deferred, optional, lenient, ordered, proxy-free* access. The three-tier strictness ladder is the heart of the design:

- `getObject()` — strict; never null; throws `NoSuchBeanDefinitionException` (absent) or `NoUniqueBeanDefinitionException` (ambiguous).
- `getIfAvailable()` — tolerates **absence** (returns null) but **still throws on ambiguity**.
- `getIfUnique()` — tolerates **both** (null when absent *or* ambiguous).

**Design intent.** Optionality and not-unique tolerance live in the *method choice* (the API surface), not in annotation attributes resolved once at startup. The mental model: "a dependency lookup is an operation with explicit failure modes," chosen at the moment of use. Each provider is a *live handle* — every call re-resolves against current factory state, holds no instance — which gives the same lifecycle safety as a scoped proxy but with an *honest, debuggable call site* instead of hidden bytecode. Not extending `Supplier` keeps it from being mistaken for an in-memory cached value (`getObject()` can throw and can re-resolve).

#### Hidden: the most-missed semantics

- **`getIfAvailable()` throws on ambiguity; only `getIfUnique()` swallows it.** "Available" is optionality with respect to *absence only*.
- **`stream()` is unordered (registration order); only `orderedStream()` applies the factory's order comparator** (`Ordered`/`@Order`/`@Priority`). The *default interface* `orderedStream()` applies only a plain `OrderComparator`; it honors annotations only because `DefaultListableBeanFactory`'s implementation uses the annotation-aware comparator.
- **Injecting `List<T>` resolves eagerly; `stream()` is lazy** and can be filtered by class (`stream(Predicate)`) *before* any bean is created.
- **`@Lazy` proxy vs `ObjectProvider`:** the docs call the `@Lazy` lazy-resolution proxy "rather limited" — it is *always* injected even when no bean exists, so a missing target surfaces only as an exception on first invocation. `ObjectProvider.getIfAvailable()` is the honest choice for optional + lazy.

Uniqueness honors the `@Primary`, `@Fallback`, and `default-candidate` flags. `jakarta.inject.Provider<T>.get()` is the JSR-330 analogue for the basic lazy case (lacking the optional/unique/stream richness).

> **Changed in v4/v7:** Signatures are now JSpecify-annotated (`default @Nullable T getIfAvailable()`); a new `getBeanProvider(ParameterizedTypeReference<T>)` lands `@since 7.0`; and `javax.inject` is dropped — `jakarta.inject.Provider` is the only standardized provider. (The `stream(Predicate)`/`orderedStream(Predicate)`/`UNFILTERED` overloads and all-default-methods refactor are **6.2.x**, *not* v7 — a common misattribution.)

### `@Lazy` and `spring.main.lazy-initialization`

`@Lazy` defaults `value()` to `true` and applies to `@Component`, `@Bean` methods, `@Configuration` classes (cascading), and injection points. It defers a singleton's creation from refresh time to first access — **relocating, not removing, failure**.

#### Hidden traps

- **Direct injection silently defeats `@Lazy`.** If any eager singleton injects a `@Lazy` bean *directly by type*, the container must build it at startup. Laziness only holds when the *reference* is deferred (lazy proxy at the injection point, `ObjectProvider`, or no eager dependents).
- **`@Lazy(false)` is an override tool**, used to opt a specific bean back into eager init under a lazy `@Configuration` or the global flag.
- **Lazy reduces startup work, not total heap** — the JVM must still be sized for all beans.
- **Singleton-cached vs re-resolved proxy:** a `@Lazy` proxy caches on first access for singletons but *re-resolves every access* for other scopes.

The Boot global flag registers a `LazyInitializationBeanFactoryPostProcessor` that sets `lazy-init` on definitions not explicitly set, **excluding** `ROLE_INFRASTRUCTURE` beans, `SmartInitializingSingleton` beans, and anything matched by a `LazyInitializationExcludeFilter` (the SPI seam for DSLs that register beans dynamically). A lazy `SmartLifecycle` bean with `isAutoStartup()==true` is still started (SPR-7014, declined).

> **Changed in v4/v7:** `@Lazy` is escalated from "nice pattern" to **required remedy**: AOT-optimized contexts fail fast on explicit injected cycles, and `@Lazy`/`ObjectProvider` are the documented fix. It is also the **mandatory glue for `@Bean(bootstrap=BACKGROUND)`** — without `@Lazy`/`ObjectProvider` on dependent injection points, the bootstrap thread blocks waiting for the background bean, defeating the concurrency (and is silently inert without a `bootstrapExecutor`).

### The autowire candidate-resolution engine

Resolution is **two phases**: candidate *filtering* via a pluggable `AutowireCandidateResolver` chain, then winner *selection* in the bean factory. The resolver chain is a decorator-by-inheritance stack, each level a superset:

```
SimpleAutowireCandidateResolver          (autowire-candidate flag only)
  <- GenericTypeAwareAutowireCandidateResolver   (generics via ResolvableType)
  <- QualifierAnnotationAutowireCandidateResolver (@Qualifier / JSR-330 / @Value)
  <- ContextAnnotationAutowireCandidateResolver   (@Lazy proxy + SpEL)
```

**Design intent.** Filtering rules are pluggable and composable; the tie-break *policy* is a fixed container contract, so a custom resolver can change *what qualifies* without re-encoding selection order. A resolver answers yes/no/**abstain** per candidate; the container alone picks the winner.

#### Hidden: `SimpleAutowireCandidateResolver` is the *bare* default

A hand-built `DefaultListableBeanFactory` uses `SimpleAutowireCandidateResolver` — **no generics, no qualifiers, no `@Value`, no `@Lazy`**. The rich behavior everyone assumes is "Spring" is installed by `AnnotationConfigUtils`/`AnnotationConfigApplicationContext` upgrading the resolver to `ContextAnnotationAutowireCandidateResolver`. Tests using a raw factory silently lose qualifier/generic resolution.

#### Hidden: three-valued `checkQualifiers`

`checkQualifiers` returns `@Nullable Boolean`: `TRUE` (qualifier found and matched), `FALSE` (found but mismatched — active veto), `null` (none found — abstain, fall through to type/primary/name). The `null`≠`false` distinction is essential: without it, "no qualifier" would be indistinguishable from "qualifier mismatched," breaking fall-through. It encodes the philosophy that qualifiers *narrow within* type matches rather than being the sole gate.

A qualifier value **falls back to bean name/alias**: with no matching `<qualifier>`/`@Qualifier` on the definition, `@Qualifier("main")` matches a bean literally named `main` (via `bdHolder.matchesName`, which covers aliases). This is deliberately a *last-resort* convenience — Spring refuses to make `@Qualifier("x")` a synonym for `getBean("x")`, preserving type-driven-first DI. **Generics are implicit qualifiers**: `Store<Integer>` will not match a `Store<String>` bean (`dependencyType.isAssignableFrom(targetType)` via `ResolvableType`).

The selection order in `determineAutowireCandidate`: **`@Primary` (and unique non-`@Fallback`) → `@Priority` → unique default-candidate → injection-point-name vs bean-name match.**

- `autowireCandidate=false` removes a bean from by-type autowiring **entirely** (still reachable by name).
- `defaultCandidate=false` is weaker: excluded from *plain-type* autowiring but still injectable **when explicitly qualified** — and it is the lever that lets `ObjectProvider.getIfUnique()` resolve cleanly when every other same-type bean is marked non-default.
- `@Primary`/`@Fallback` affect **only single injection points** — arrays, `Collection`s, `Map`s, and `ObjectProvider` streams take **all** type-matching beans regardless.

**`@Fallback` (6.2, *not* v7) as the inverse of `@Primary`:** rather than forcing the library author to bless a winner, a default bean marks *itself* fallback so any user-supplied bean of the same type wins with zero extra config — the auto-configuration override story made declarative. `@Priority` is class-level only (`jakarta.annotation.Priority`, no `METHOD` target), so it cannot go on `@Bean` methods; model it with `@Order` + `@Primary`/`@Fallback` instead.

The resolver is **cloned per factory** (`cloneIfNecessary`): it is `BeanFactoryAware` and caches its owning factory, so `copyConfigurationFrom` clones rather than shares it (the stateless `SimpleAutowireCandidateResolver` returns a shared `INSTANCE`). `CustomAutowireConfigurer` (a `BeanFactoryPostProcessor`) reaches into the live resolver via `addQualifierType` to register annotations not meta-annotated with `@Qualifier`.

> **Changed in v4/v7:** Migration to JSpecify nullness; `MethodParameter#isOptional` now checks only *local* annotations (affecting required/optional inference at injection points); `javax.inject`/`javax.annotation` removed — only `jakarta.inject.Qualifier` is recognized. **`@Fallback` and the `defaultCandidate` flag are 6.2 features, not new in 7.**

### Ordering infrastructure

A single shared mechanism in two layers: `OrderComparator` (core; knows only the `Ordered`/`PriorityOrdered` interfaces) and `AnnotationAwareOrderComparator` (adds `@Order` and `jakarta.annotation.Priority`). The factory's `dependencyComparator` is the annotation-aware one.

Rules: lower value = higher precedence (`HIGHEST_PRECEDENCE = Integer.MIN_VALUE`, `@Order` defaults to `LOWEST_PRECEDENCE`); a `PriorityOrdered` instance sorts ahead of any plain `Ordered` **before** numeric values are even compared; the `Ordered` **interface beats the annotation** (runtime-computed order is the more specific signal).

#### Hidden: the array/List-vs-Map asymmetry

`resolveMultipleBeans` sorts **array** results (`Arrays.sort`) and **`List`/`Collection`** results (`list.sort`) via the factory comparator — but `resolveMultipleBeanMap` does **no sort**, so an injected `Map<String, T>` follows registration order and `@Order`/`@Priority`/`Ordered` have **no effect** on it. This is the single most-missed ordering gotcha.

**`@Order` (sort all) is orthogonal to `@Priority` (select one unique winner).** `@Order` does **not** influence singleton startup/instantiation order — that is governed by real dependency edges and `@DependsOn`. The intent: instantiation order is a correctness concern; injection/collection order is a presentation concern, and a cosmetic annotation must not silently break init invariants. `@Order` on a `@Bean` method works via `FactoryAwareOrderSourceProvider`, which inspects order sources — the `ORDER_ATTRIBUTE`, the factory `Method`, the target type — rather than the produced instance (which often carries no annotation), and unwraps `DecoratingProxy`.

> **Changed in v4/v7 (internal only):** `OrderComparator` gained a public `getOrder(Object, OrderSourceProvider)` `@since 7.0`; the public ordering contract is otherwise unchanged.

### `BeanRegistrar` — first-class programmatic registration

> **Changed in v4/v7:** `BeanRegistrar` is **entirely new in Framework 7.0**: a `@FunctionalInterface` (`org.springframework.beans.factory`, by Sebastien Deleuze) with the single method `void register(BeanRegistry registry, Environment env)`.

```java
@Configuration
@Import(MyBeanRegistrar.class)
class AppConfig {}

class MyBeanRegistrar implements BeanRegistrar {
    public void register(BeanRegistry registry, Environment env) {
        registry.registerBean("bar", Bar.class, spec ->
            spec.prototype().lazyInit().supplier(ctx -> new Bar(ctx.bean(Foo.class))));
    }
}
```

`BeanRegistry` offers `register(BeanRegistrar)`, `registerAlias`, and eight `registerBean` overloads ({named/unnamed} × {`Class` | `ParameterizedTypeReference`} × {with/without `Consumer<Spec<T>>`}). The fluent `Spec` maps 1:1 onto `RootBeanDefinition` setters: `backgroundInit()`, `description()`, `fallback()`, `infrastructure()` (`ROLE_INFRASTRUCTURE`), `lazyInit()`, `notAutowirable()` (`setAutowireCandidate(false)`), `order(int)`, `primary()`, `prototype()`, `scope(String)` (added 7.0.4), and `supplier(...)`. (There is **no** `targetType()` on `Spec`; the `ParameterizedTypeReference` overloads set the target type automatically.)

**Design intent.** The mental model is "imperative bean registration with full host-language control flow" — `if`/`for`/`switch` and `env.matchesProfiles(...)` decide what to register, moving dynamic decisions to a place Spring executes *once, at definition time*, rather than scattering them across awkwardly-composing `@Conditional`s. Passing `Environment` (not the full context) keeps the contract narrow. Wiring via `@Import` (not `@Bean`) is essential: it slots into `ConfigurationClassPostProcessor` parsing so the definitions exist before instantiation **and** are build-time-analyzable; a `@Bean`-produced registrar would run too late. `Spec` is a curated, discoverable subset of the sprawling `AbstractBeanDefinition` surface — "describe the bean's intent" rather than hand-assemble a `RootBeanDefinition`. It is positioned as the modern successor to `BeanDefinitionRegistryPostProcessor` and `ImportBeanDefinitionRegistrar`, which are verbose and AOT-hostile.

#### Why instance suppliers stay native-friendly

Inside a `supplier`, dependencies are resolved **explicitly** through `context.bean(...)` / `context.beanProvider(...)` (delegating to `getBean`/`getBeanProvider`) — there is no reflective autowiring into the supplier-built instance (a behavioral/testing difference practitioners miss). `BeanRegistrarBeanDefinition.getPreferredConstructors()` returns `null` when a supplier is set, making the lambda the sole instantiation path. Explicit lookup removes the reflective autowiring metadata that is exactly what is expensive/fragile under GraalVM — and the supplier lambda is concrete code the AOT engine keeps. The accepted trade-off is verbosity in exchange for AOT determinism.

#### Hidden seams

- **`aotProcessingIgnoreRegistration=true`** — set by `BeanRegistrarBeanDefinition` to tell the AOT engine to *ignore the registrar's own registration path* (avoiding the classic `BeanDefinitionRegistryPostProcessor` double-registration at native runtime) while still contributing concrete definitions. Under AOT, `ConfigurationClassPostProcessor`'s `BeanRegistrarAotContribution` generates code that simply **re-invokes `registrar.register(...)`** at startup through a fresh `BeanRegistryAdapter` — sidestepping "code generation is not supported for instance supplier callbacks." `ImportAware` metadata is reattached via `MetadataReaderFactory`.
- **The `customizers` `MultiValueMap<String, BeanDefinitionCustomizer>`** on `BeanRegistryAdapter` — null at normal runtime; chiefly an AOT-internal channel so re-running `register()` can re-apply init/destroy wiring a naive rerun would lose. An outer framework (Boot) can also post-customize specific registrar beans by name without the registrar knowing.
- **`infrastructure()` vs `notAutowirable()`** — `infrastructure()` only hints tooling that a bean is plumbing (it does *not* prevent injection); `notAutowirable()` is what actually removes it from by-type autowiring while keeping it retrievable by name.
- **Build-time determinism requirement** — because the registrar runs at AOT build time, profile/property decisions are *frozen* at build time; a registrar reading runtime-only state won't behave dynamically in a native image.

Kotlin gets a dedicated `BeanRegistrarDsl` (`registerBean<Foo>()`, `profile("x") { ... }`, type-inferring `bean()`), recommended over implementing `BeanRegistrar` directly.

> **IDE tooling gap:** as of early Boot 4 adoption, IntelliJ does not yet recognize beans registered via `BeanRegistrar` (spring-tools #1498), producing false "no bean found" warnings despite correct runtime behavior.

### The AOT per-bean code-generation pipeline

`ApplicationContextAotGenerator.processAheadOfTime` drives a **definition-only refresh** (`refreshForAotProcessing(RuntimeHints)`): it runs `BeanFactoryPostProcessor`s exactly as normal (config parsing, `@Import`, scanning, `@Conditional` evaluation) so the factory ends up with the full merged definitions — **but creates no instances**. Because nothing is instantiated, ordinary `BeanPostProcessor`s don't run; only `MergedBeanDefinitionPostProcessor` (to extract init/destroy onto the merged def) and `SmartInstantiationAwareBeanPostProcessor` (to predict types and *materialize proxies* needed at runtime) are invoked.

**Design intent.** Rather than re-implement Spring's rich configuration logic in a static analyzer, the engine **reuses the real refresh** up to the instantiation boundary — guaranteeing generated definitions match runtime behavior. It then emits *readable Java source* (JavaPoet) — `@Generated`-tagged `<Class>__BeanDefinitions` classes plus an `ApplicationContextInitializer` — so the normal toolchain compiles it. The mental model offered to developers: "AOT just writes the boring wiring code you would have written by hand."

`BeanRegistrationsAotProcessor` walks each `RegisteredBean`, lets per-bean `BeanRegistrationAotProcessor`s contribute code fragments, and wires each bean to a **`BeanInstanceSupplier`** — the universal translation target — built via `forConstructor(...)` / `forFactoryMethod(...)` `.withGenerator(...)`. This replaces reflective member injection with direct constructor/factory calls; the arg-resolving generator form quietly adds an `ExecutableMode.INTROSPECT` reflection hint so parameter annotations remain readable in native images. Because there is no instance to probe, AOT must pick a deterministic constructor: `RootBeanDefinition.getPreferredConstructors()` honors `PREFERRED_CONSTRUCTORS_ATTRIBUTE` (since 6.1), with `@Autowired` as the recommended marker.

**Fail-fast on dynamism.** A definition carrying an opaque `setInstanceSupplier(lambda)` cannot be statically analyzed — `BeanDefinitionMethodGenerator` throws "Default code generation is not supported for bean definitions declaring an instance supplier callback." Same for `registerSingleton()` (instance already exists, no recipe). The philosophy is determinism-over-dynamism: dynamism must be made explicit (a declarative factory) or re-run at runtime via a `BeanRegistrar` (the `aotProcessingIgnoreRegistration` route).

> **Changed in v4/v7:** Metadata reading adopts the JDK Class-File API (JEP 484) via a new `ClassFileMetadataReader` on Java 24+, reducing reliance on shaded ASM. `BeanRegistrar` integration (above) is the notable new AOT-friendly registration path. The core pipeline (`ApplicationContextAotGenerator`, `refreshForAotProcessing`, `BeanInstanceSupplier` since 6.0) is otherwise the matured 6.x design.

### Hierarchical parent–child containers

A child sees its own beans plus all ancestor beans; a parent **never** sees descendants. `HierarchicalBeanFactory` exposes `getParentBeanFactory()` and `containsLocalBean(String)` (ignores ancestors); `setParentBeanFactory` lives on `ConfigurableBeanFactory` (read/write interface segregation). `getBean(name)` delegates upward only on a local miss, and **local definitions shadow same-named parent definitions** ("lowest factory wins" — even for by-type lookups).

#### Hidden: listing is local-only, but injection is hierarchy-aware

`getBeanNamesForType`/`getBeansOfType` are **deliberately LOCAL-ONLY** — they do not traverse ancestors. To span the hierarchy you must opt in via `BeanFactoryUtils.beanNamesForTypeIncludingAncestors`/`beansOfTypeIncludingAncestors` (which collapse overridden names to one entry). **Yet `@Autowired`/`resolveDependency()` *does* traverse ancestors automatically**, injecting the lowest-level match. This asymmetry — *injection* is hierarchy-aware, *listing* is not — is the most common practitioner surprise.

**Design intent.** Unidirectional visibility mirrors classloader-parent delegation: stable shared infrastructure lives "up," specialized/volatile components "down," so many children layer on one parent without leaking into each other or the parent (the classic Spring MVC root + DispatcherServlet arrangement). By-type *listing* is a per-container introspection op whose result must be deterministic and cheap; silently merging ancestors would make collection injection non-obvious and risk double-counting. So flattening is something you *ask for*. A direct corollary of no-downward-visibility: **post-processors are scoped per-container** — a parent's `BeanPostProcessor` physically cannot process child beans.

> **Changed in v4/v7:** No functional change to the hierarchical mechanism; nullness APIs are JSpecify-annotated, and bean-override testing support (`@MockitoBean`/`@TestBean`) now extends to non-singleton beans, interacting with the per-context override model in `@ContextHierarchy` tests.

### `@Configuration` full mode vs. lite mode

In **full mode** (`@Configuration`, default `proxyBeanMethods=true`), Spring generates a runtime CGLIB subclass that **intercepts inter-`@Bean` method calls**, so `clientDao()` called inside `clientService1()` returns the shared singleton rather than a fresh object. In **lite mode** (`@Bean` on a non-`@Configuration` class, or `proxyBeanMethods=false`), no subclass is generated; `@Bean` methods are plain factory methods and an inter-bean call is ordinary Java that creates a new instance every time — "behaviorally equivalent to removing the `@Configuration` stereotype."

**Design intent.** Full mode instills the model that *a `@Bean` method call is a bean lookup expressed as Java, not a constructor* — letting the intuitive, refactor-safe, IDE-navigable call syntax mean "give me the managed bean" while preserving scope and AOP. Lite mode is the first-class opt-out for the common case of independent factory methods, dropping the heavyweight, native-hostile CGLIB subclass (which also forbids `final`/`private` classes and methods).

#### Hidden traps

- **`@Component`-with-`@Bean` is lite mode** — inter-bean calls there silently create duplicates. Only the literal `@Configuration` stereotype triggers full-mode interception.
- **Lite mode still applies scope and init/destroy** to the container-created bean; only inter-bean *call interception* is lost. The correct lite pattern passes dependencies as method parameters (`@Bean ServiceB b(ServiceA a)`), not via nested method calls.
- **BFPP/BPP-returning `@Bean` methods should be `static`** — a non-static one forces premature instantiation of its declaring config class before post-processors are registered, producing the "not eligible for getting processed by all BeanPostProcessors" warning.

**Why lite/`proxyBeanMethods=false` is AOT-preferred:** runtime CGLIB enhancement is unavailable in GraalVM native images and adds startup/footprint cost. With nothing to proxy, AOT maps cleanly onto factory-method `BeanInstanceSupplier`s (`getBean(Config.class).dataSource()`), making inter-bean references explicit `getBean` lookups.

> **Changed in v4/v7:** Spring Boot 4 emits an explicit native-image failure — "CGLIB runtime enhancement not supported on native image. Make sure to enable Spring AOT processing…" — directing you to `proxyBeanMethods=false`. `@Configuration`'s `enforceUniqueMethods` element is **deprecated as of 7.0**. Spring Framework 7 also makes the global proxy-type default **consistently apply to all proxy processors** (including `@Async`/`@EnableAsync`), with the new `@Proxyable` annotation for per-bean opt-out (`@Proxyable(INTERFACES)` against a CGLIB default, `@Proxyable(TARGET_CLASS)` against the JDK default). Note, however, that **CGLIB is *not* the core-framework default** — the core framework still suggests interface-based (JDK) proxies; class-based defaulting remains a Spring Boot behavior driven by configuration. The v7 change is consistency of the *defaulting mechanism*, not a universal flip to CGLIB.

### `DefaultLifecycleProcessor` and the `SmartLifecycle` phase model

The phased engine starts beans **lowest-phase-first** and stops them **highest-phase-first** (symmetric reversal). `SmartLifecycle.DEFAULT_PHASE = Integer.MAX_VALUE`, so auto-started infrastructure starts last and stops first; plain `Lifecycle` beans are phase 0 and are **not** auto-started at refresh (only `SmartLifecycle` is). For `SmartLifecycle`, the processor calls only `stop(Runnable)` — never the no-arg `stop()` — enabling asynchronous, bounded shutdown.

**Design intent — per-phase, not global, timeout.** Each phase is a dependency tier that must fully quiesce before the tier below stops (drain HTTP before stopping its thread pool). A single global budget could be exhausted by an early phase, forcing unsafe teardown later; a per-phase budget guarantees each tier its own grace window (worst-case total ≈ N × timeout — the accepted price of correctness). Stopping high-phase-first encodes the dependency direction with a single number: B (higher phase, depends on A) starts after A and stops before A.

> **Changed in v4/v7:** Spring Framework 7 adds a genuine **pause/restart** model distinct from start/stop — `LifecycleProcessor.onPause()`/`onRestart()`, `ConfigurableApplicationContext.pause()`/`restart()`, and `SmartLifecycle.isPauseable()` (default `true`). `pause()` stops all beans *except* those with `isPauseable()==false`. Spring Boot 4 places its web-server lifecycles just below `DEFAULT_PHASE`: graceful shutdown at `DEFAULT_PHASE - 1024`, connector start/stop at `DEFAULT_PHASE - 2048` — so app `SmartLifecycle` beans (at `DEFAULT_PHASE`) stop first, giving them a drain window while the server is still serving, and the 1024-unit gaps are deliberate insertion points for custom ordering. Boot's web-server lifecycles override `isPauseable()` to `false` (pausing a context must not tear down the HTTP connector).

#### Hidden lifecycle rules

- **`stop()` is *not* guaranteed before `destroy()`.** On regular shutdown, `Lifecycle` beans receive `stop()` before destruction; but on **hot refresh or aborted/stopped refresh attempts, only destroy methods are called**. Never put graceful-drain logic solely in `@PreDestroy` expecting `stop()` to have run.
- **`lazy-init` has essentially no effect on auto-start `SmartLifecycle` beans** — the processor instantiates them at refresh regardless.
- The per-phase shutdown timeout default is **10s as of Framework 6.2** (was 30s); the reference prose still says "30 seconds" in places while its own example and javadoc say 10000ms — treat 10s as authoritative. Boot exposes it as `spring.lifecycle.timeout-per-shutdown-phase`, which is the **per-phase** value, not a total budget.


---

## Appendix — Sources

_542 unique sources consulted across all research agents._

- @Autowired (ordering, Map, @Order vs @Priority, optional, self references) :: Spring Framework — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired.html
- @Bean annotation (Java config, init/destroy & scoped proxy) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/java/bean-annotation.html
- @ConfigurationProperties (Spring Boot 4.0 API javadoc) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/api/java/org/springframework/boot/context/properties/ConfigurationProperties.html
- @ConfigurationPropertiesBinding (Spring Boot 4.0 API javadoc) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/api/java/org/springframework/boot/context/properties/ConfigurationPropertiesBinding.html
- @ConfigurationPropertiesSource (Spring Boot 4.0.0 API javadoc) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/context/properties/ConfigurationPropertiesSource.html
- @ConstructorBinding (Spring Boot 4.0 API javadoc) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/api/java/org/springframework/boot/context/properties/bind/ConstructorBinding.html
- @EventListener (Spring Framework 7.0 Javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/event/EventListener.html
- @Lazy not working in Native mode · Issue #30985 · spring-projects/spring-framework — https://github.com/spring-projects/spring-framework/issues/30985
- @Order (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/annotation/Order.html
- A Guide to Fallback Beans in Spring Framework (Baeldung) — https://www.baeldung.com/spring-fallback-beans
- AOT Best Practices — break circular dependencies with @Lazy or ObjectProvider :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/aot.html
- AOT instance supplier code generation error (Spring Boot issue #34371) — https://github.com/spring-projects/spring-boot/issues/34371
- AbstractApplicationContext (Spring Framework 7.0.7/7.0.8 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/support/AbstractApplicationContext.html
- AbstractApplicationContext.java (spring-framework main, prepareBeanFactory) — https://github.com/spring-projects/spring-framework/blob/main/spring-context/src/main/java/org/springframework/context/support/AbstractApplicationContext.java
- AbstractApplicationContext.java source (spring-framework v7.0.0 tag) — https://github.com/spring-projects/spring-framework/blob/v7.0.0/spring-context/src/main/java/org/springframework/context/support/AbstractApplicationContext.java
- AbstractAutoProxyCreator (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/aop/framework/autoproxy/AbstractAutoProxyCreator.html
- AbstractAutoProxyCreator (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/aop/framework/autoproxy/AbstractAutoProxyCreator.html
- AbstractAutowireCapableBeanFactory.java (spring-framework main, initializeBean/invokeAwareMethods/invokeInitMethods) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/AbstractAutowireCapableBeanFactory.java
- AbstractAutowireCapableBeanFactory.java (v7.0.5) — early-singleton-reference / circular dependency mechanism (doCreateBean, addSingletonFactory, getEarlyBeanReference) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/support/AbstractAutowireCapableBeanFactory.java
- AbstractAutowireCapableBeanFactory.java source (spring-framework main) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/AbstractAutowireCapableBeanFactory.java
- AbstractBeanDefinition.java / RootBeanDefinition.java — Spring Framework v7.0.5 (PREFERRED_CONSTRUCTORS_ATTRIBUTE, getPreferredConstructors, setTargetType) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/support/RootBeanDefinition.java
- AbstractBeanFactory Javadoc merged definition cache methods — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/AbstractBeanFactory.html
- AbstractBeanFactory.java (spring-framework 7.0.x source) — isTypeMatch supportsType+getTypeForFactoryBean, isFactoryBean(String)/(name,mbd) predictBeanType, getObjectForBeanInstance, getTypeForFactoryBean(name,mbd,allowInit) — https://github.com/spring-projects/spring-framework/blob/7.0.x/spring-beans/src/main/java/org/springframework/beans/factory/support/AbstractBeanFactory.java
- AbstractBeanFactory.java (spring-framework main source) — transformedBeanName, getObjectForBeanInstance, isFactoryDereference — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/AbstractBeanFactory.java
- AbstractBeanFactory.java getMergedLocalBeanDefinition getMergedBeanDefinition clearMergedBeanDefinition markBeanAsCreated — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/AbstractBeanFactory.java
- AbstractBeanFactory.java source (spring-framework main) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/AbstractBeanFactory.java
- AbstractEnvironment (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/env/AbstractEnvironment.html
- AbstractEnvironment source — customizePropertySources ordering (spring-framework GitHub) — https://github.com/spring-projects/spring-framework/blob/main/spring-core/src/main/java/org/springframework/core/env/AbstractEnvironment.java
- AbstractRefreshableApplicationContext.customizeBeanFactory (allowBeanDefinitionOverriding/allowCircularReferences) Javadoc — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/support/AbstractRefreshableApplicationContext.html
- Add support for indexing functional bean registrations via BeanRegistrar (spring-tools Issue #1498) — https://github.com/spring-projects/spring-tools/issues/1498
- Additional Capabilities of the ApplicationContext (events, MessageSource, hierarchies) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/context-introduction.html
- Ahead of Time (AOT) Support for Tests :: Spring Framework — https://docs.spring.io/spring-framework/reference/testing/testcontext-framework/aot.html
- Ahead of Time Optimizations (Spring Framework 7.0.5 reference, aot.adoc) — proxyBeanMethods=false example, generated BeanInstanceSupplier — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/aot.adoc
- Ahead of Time Optimizations :: Spring Data JPA 4.0 — https://docs.spring.io/spring-data/jpa/reference/4.0-SNAPSHOT/jpa/aot.html
- Ahead of Time Optimizations :: Spring Framework (reference docs) — https://docs.spring.io/spring-framework/reference/core/aot.html
- Ahead of Time Optimizations :: Spring Framework (reference, current/7.x) — https://docs.spring.io/spring-framework/reference/core/aot.html
- Ahead of Time Optimizations :: Spring Framework Reference (7.x) — https://docs.spring.io/spring-framework/reference/core/aot.html
- Ahead of Time Optimizations :: Spring Framework Reference (AOT, circular deps, @Lazy/ObjectProvider) — https://docs.spring.io/spring-framework/reference/core/aot.html
- Ahead of Time Optimizations :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/aot.html
- Ahead-of-Time Processing :: Spring Boot Gradle Plugin (ProcessAot / process-aot) — https://docs.spring.io/spring-boot/gradle-plugin/aot.html
- Ahead-of-Time Processing :: Spring Boot Maven Plugin — https://docs.spring.io/spring-boot/maven-plugin/aot.html
- AliasFor (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/annotation/AliasFor.html
- AnnotatedElementUtils (Spring Framework API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/annotation/AnnotatedElementUtils.html
- Annotation-based Autowiring (@Autowired, optional dependencies, @Order/@Priority) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired.html
- Annotation-based Container Configuration :: Spring Framework (reference) — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config.html
- AnnotationAwareOrderComparator (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/annotation/AnnotationAwareOrderComparator.html
- AnnotationAwareOrderComparator.java source (spring-framework main / 7.x) — findOrder, getPriority, DecoratingProxy — https://github.com/spring-projects/spring-framework/blob/main/spring-core/src/main/java/org/springframework/core/annotation/AnnotationAwareOrderComparator.java
- AnnotationBeanNameGenerator (Spring Framework 7.0.x API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/AnnotationBeanNameGenerator.html
- AnnotationConfigUtils source (Spring Framework main) — https://github.com/spring-projects/spring-framework/blob/main/spring-context/src/main/java/org/springframework/context/annotation/AnnotationConfigUtils.java
- AnnotationMetadata (Spring Framework API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/type/AnnotationMetadata.html
- AnnotationUtils (Spring Framework 7.0.x API javadoc) — synthesizeAnnotation, deprecations — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/annotation/AnnotationUtils.html
- AopAutoConfiguration (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/autoconfigure/aop/AopAutoConfiguration.html
- Application Events (Testing) — @RecordApplicationEvents / ApplicationEvents — https://docs.spring.io/spring-framework/reference/testing/testcontext-framework/application-events.html
- ApplicationContext.getParent (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/ApplicationContext.html
- ApplicationContextAwareProcessor.java (spring-framework main, invokeAwareInterfaces order) — https://github.com/spring-projects/spring-framework/blob/main/spring-context/src/main/java/org/springframework/context/support/ApplicationContextAwareProcessor.java
- ApplicationStartup (Spring Framework 7.0.x API javadoc) — DEFAULT no-op, start(name) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/metrics/ApplicationStartup.html
- AsyncExecutionInterceptor (Spring Framework 7.0 API) — @Async proxying mechanism — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/aop/interceptor/AsyncExecutionInterceptor.html
- Auto-configuration (Spring Boot reference, using/auto-configuration) — https://docs.spring.io/spring-boot/reference/using/auto-configuration.html
- Auto-configure a bootstrapExecutor bean spring-boot issue 39791 — https://github.com/spring-projects/spring-boot/issues/39791
- Auto-resolve bean name conflicts for scanned classes [SPR-14665] (spring-framework issue #19229) — https://github.com/spring-projects/spring-framework/issues/19229
- AutoConfiguration.java (Spring Boot v4.1.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot-autoconfigure/src/main/java/org/springframework/boot/autoconfigure/AutoConfiguration.java
- AutoConfigurationImportSelector.java (Spring Boot v4.1.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot-autoconfigure/src/main/java/org/springframework/boot/autoconfigure/AutoConfigurationImportSelector.java
- AutoConfigurationSorter.java (Spring Boot v4.1.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot-autoconfigure/src/main/java/org/springframework/boot/autoconfigure/AutoConfigurationSorter.java
- AutowireCandidateResolver (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/AutowireCandidateResolver.html
- Autowiring Collaborators (default-autowire-candidates patterns) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-autowire.html
- Basic Concepts: @Bean and @Configuration (Spring Framework 7.0.5 reference, basic-concepts.adoc) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/java/basic-concepts.adoc
- Basic Concepts: @Bean and @Configuration :: Spring Framework Reference (full vs lite mode) — https://docs.spring.io/spring-framework/reference/core/beans/java/basic-concepts.html
- Bean (Spring Framework 7.0 API) - autowireCandidate/defaultCandidate — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Bean.html
- Bean (Spring Framework API javadoc) — @Bean Lite Mode, static BFPP/BPP @Bean methods — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Bean.html
- Bean Definition Inheritance (parent/child, merged definitions, abstract) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/child-bean-definitions.html
- Bean Definition Inheritance child-bean-definitions Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/child-bean-definitions.html
- Bean Overview (BeanDefinition as metadata; overriding) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/definition.html
- Bean References in SpEL — '&' factory bean dereference (Spring Framework reference) — https://docs.spring.io/spring-framework/reference/core/expressions/language-ref/bean-references.html
- Bean Scopes :: Spring Framework Reference (current/7.x) — https://docs.spring.io/spring-framework/reference/core/beans/factory-scopes.html
- Bean Scopes — Scoped Beans as Dependencies / Scoped Proxies vs ObjectFactory/Provider :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/factory-scopes.html
- Bean.Bootstrap (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Bean.Bootstrap.html
- Bean.Bootstrap (Spring Framework 7.0.7 API javadoc, BACKGROUND) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Bean.Bootstrap.html
- BeanCurrentlyInCreationException (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanCurrentlyInCreationException.html
- BeanDefinition (Spring Framework current API Javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/config/BeanDefinition.html
- BeanDefinitionBuilder (setScope/setLazyInit/setPrimary/setFallback) Javadoc — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/BeanDefinitionBuilder.html
- BeanDefinitionMethodGenerator.java — Spring Framework source (instance-supplier IllegalArgumentException) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/aot/BeanDefinitionMethodGenerator.java
- BeanDefinitionMethodGeneratorFactory.java — Spring Framework source (exclusion + IGNORE_REGISTRATION_ATTRIBUTE checks) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/aot/BeanDefinitionMethodGeneratorFactory.java
- BeanDefinitionOverrideException (Spring Framework 7.0.5 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/BeanDefinitionOverrideException.html
- BeanDefinitionOverrideFailureAnalyzer (Spring Boot source) — https://github.com/spring-projects/spring-boot/blob/main/spring-boot/core/spring-boot/src/main/java/org/springframework/boot/diagnostics/analyzer/BeanDefinitionOverrideFailureAnalyzer.java
- BeanDefinitionOverrideFailureAnalyzer source (spring-boot main) — https://github.com/spring-projects/spring-boot/blob/main/spring-boot/core/spring-boot/src/main/java/org/springframework/boot/diagnostics/analyzer/BeanDefinitionOverrideFailureAnalyzer.java
- BeanFactory (Spring Framework 7.0 API javadoc) — hierarchical lookup and override — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanFactory.html
- BeanFactory (Spring Framework current API Javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanFactory.html
- BeanFactory.getBeanProvider(...) Spring Framework 7.0.x API javadoc — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanFactory.html
- BeanFactoryPostProcessor (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/config/BeanFactoryPostProcessor.html
- BeanFactoryUtils (Spring Framework 7.0 API javadoc) — *IncludingAncestors methods and shadowing note — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanFactoryUtils.html
- BeanInstanceSupplier (Spring Framework 7.0.2 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/aot/BeanInstanceSupplier.html
- BeanInstanceSupplier.java — Spring Framework v7.0.5 source — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/aot/BeanInstanceSupplier.java
- BeanPostProcessor (Spring Framework 7.0.7 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/config/BeanPostProcessor.html
- BeanPostProcessor (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/config/BeanPostProcessor.html
- BeanRegistrar (Spring Framework 7.0 API Javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanRegistrar.html
- BeanRegistrar (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanRegistrar.html
- BeanRegistrar (Spring Framework 7.0.x API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanRegistrar.html
- BeanRegistrar (Spring Framework 7.x API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanRegistrar.html
- BeanRegistrar (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanRegistrar.html
- BeanRegistrar.java source (v7.0.5) — https://raw.githubusercontent.com/spring-projects/spring-framework/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/BeanRegistrar.java
- BeanRegistrationAotProcessor (Spring Framework API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/aot/BeanRegistrationAotProcessor.html
- BeanRegistrationAotProcessor.java — Spring Framework v7.0.5 source — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/aot/BeanRegistrationAotProcessor.java
- BeanRegistrationCodeFragments (Spring Framework API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/aot/BeanRegistrationCodeFragments.html
- BeanRegistrationExcludeFilter (Spring Framework API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/aot/BeanRegistrationExcludeFilter.html
- BeanRegistry (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanRegistry.html
- BeanRegistry (Spring Framework 7.0.8 API) — https://docs.spring.io/spring-framework/docs/7.0.x/javadoc-api/org/springframework/beans/factory/BeanRegistry.html
- BeanRegistry (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanRegistry.html
- BeanRegistry.Spec (Spring Framework API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/BeanRegistry.Spec.html
- BeanRegistry.java source (spring-framework v7.0.5) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/BeanRegistry.java
- BeanRegistryAdapter.java source (spring-framework v7.0.5) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/support/BeanRegistryAdapter.java
- BeanRegistryAdapter.java — Spring Framework source (aotProcessingIgnoreRegistration set by BeanRegistrarBeanDefinition) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/BeanRegistryAdapter.java
- BeansException (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/BeansException.html
- Bindable (Spring Boot 4.0 API javadoc) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/api/java/org/springframework/boot/context/properties/bind/Bindable.html
- Binder / BindResult (Spring Boot 4.0 API javadoc) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/api/java/org/springframework/boot/context/properties/bind/BindResult.html
- CGLIB proxies are not used at runtime on @Configuration classes in AOT mode (Issue #29107) — https://github.com/spring-projects/spring-framework/issues/29107
- CGLIB runtime enhancement not supported on native image with Spring Boot 4 (Issue #49350) — https://github.com/spring-projects/spring-boot/issues/49350
- CandidateComponentsIndex (Spring Framework API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/index/CandidateComponentsIndex.html
- ClassPathScanningCandidateComponentProvider (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/ClassPathScanningCandidateComponentProvider.html
- Classpath Scanning / @SessionScope proxyMode :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html
- Classpath Scanning and Managed Components :: Spring Framework (7.0.5 docs source) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/classpath-scanning.adoc
- Classpath Scanning and Managed Components :: Spring Framework (reference) — https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html
- Classpath Scanning and Managed Components :: Spring Framework Reference (7.0.8) — https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html
- CommonAnnotationBeanPostProcessor (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/CommonAnnotationBeanPostProcessor.html
- Comparing Spring AOP and AspectJ (Baeldung) — https://www.baeldung.com/spring-aop-vs-aspectj
- Component.java source (spring-context, main) — shows @Indexed meta-annotation — https://github.com/spring-projects/spring-framework/blob/main/spring-context/src/main/java/org/springframework/stereotype/Component.java
- ComponentScan (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/ComponentScan.html
- Composing Configuration Classes — Background Initialization (@Bean bootstrap=BACKGROUND, bootstrapExecutor) — https://docs.spring.io/spring-framework/reference/core/beans/java/composing-configuration-classes.html
- Composing Java-based Configurations (@Import) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/java/composing-configuration-classes.html
- Composing Java-based Configurations (@Lazy, @DependsOn, @Bean bootstrap=BACKGROUND) :: Spring Framework — https://docs.spring.io/spring-framework/reference/core/beans/java/composing-configuration-classes.html
- Composing Java-based Configurations / @Import (Spring Framework 7.0.x reference) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/java/composing-configuration-classes.adoc
- Condition (Spring Framework current javadoc) — matches(ConditionContext, AnnotatedTypeMetadata) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Condition.html
- ConditionContext (Spring Framework current javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/ConditionContext.html
- ConditionalOnBooleanProperty (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/4.0/api/java/org/springframework/boot/autoconfigure/condition/ConditionalOnBooleanProperty.html
- ConditionalOnMissingBean (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/autoconfigure/condition/ConditionalOnMissingBean.html
- ConditionalOnProperty (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/autoconfigure/condition/ConditionalOnProperty.html
- ConditionalOnSingleCandidate (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/4.0/api/java/org/springframework/boot/autoconfigure/condition/ConditionalOnSingleCandidate.html
- Conditions Evaluation Report (conditions) actuator endpoint :: Spring Boot — https://docs.spring.io/spring-boot/api/rest/actuator/conditions.html
- ConfigurableApplicationContext (Spring Framework 7.0.x API javadoc) — pause()/restart() — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/ConfigurableApplicationContext.html
- ConfigurableApplicationContext.setParent (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/ConfigurableApplicationContext.html
- ConfigurableBeanFactory.getMergedBeanDefinition (Spring Framework 7.0.x API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/config/ConfigurableBeanFactory.html
- ConfigurableBeanFactory.getMergedBeanDefinition Javadoc — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/config/ConfigurableBeanFactory.html
- ConfigurableEnvironment (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/env/ConfigurableEnvironment.html
- ConfigurableListableBeanFactory (Spring Framework 7.0.8 API) — preInstantiateSingletons, prepareSingletonBootstrap (since 6.2.12) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/config/ConfigurableListableBeanFactory.html
- Configuration (Spring Framework 7.0 API javadoc) — proxyBeanMethods — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Configuration.html
- Configuration (Spring Framework 7.0 API javadoc) — proxyBeanMethods, lite mode, enforceUniqueMethods — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Configuration.html
- Configuration Metadata: Annotation Processor (Spring Boot 4.0 specification) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/specification/configuration-metadata/annotation-processor.html
- ConfigurationClassPostProcessor (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/ConfigurationClassPostProcessor.html
- ConfigurationClassPostProcessor.java source (BeanRegistrarAotContribution) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-context/src/main/java/org/springframework/context/annotation/ConfigurationClassPostProcessor.java
- ConfigurationCondition (Spring Framework current javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/ConfigurationCondition.html
- ConfigurationCondition.ConfigurationPhase (Spring Framework current javadoc) — PARSE_CONFIGURATION vs REGISTER_BEAN — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/ConfigurationCondition.ConfigurationPhase.html
- ConfigurationPropertyName source (Form enum), spring-boot main — https://github.com/spring-projects/spring-boot/blob/main/spring-boot/core/spring-boot/src/main/java/org/springframework/boot/context/properties/source/ConfigurationPropertyName.java
- Container Extension Points :: Spring Framework (reference) — https://docs.spring.io/spring-framework/reference/core/beans/factory-extension.html
- Container Overview (configuration metadata, BeanDefinition properties) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/basics.html
- Context Configuration with Environment Profiles :: Spring Framework TestContext Reference — https://docs.spring.io/spring-framework/reference/testing/testcontext-framework/ctx-management/env-profiles.html
- Context Hierarchies — @ContextHierarchy parent-child semantics (Spring Framework testing reference) — https://docs.spring.io/spring-framework/reference/testing/testcontext-framework/ctx-management/hierarchies.html
- Context Propagation with Project Reactor 1 — The Basics (spring.io blog) — https://spring.io/blog/2023/03/28/context-propagation-with-project-reactor-1-the-basics/
- ContextAnnotationAutowireCandidateResolver (Spring Framework API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/ContextAnnotationAutowireCandidateResolver.html
- ContextPropagatingTaskDecorator (Spring Framework API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/task/support/ContextPropagatingTaskDecorator.html
- Controller (Spring Framework 7.0.7 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/stereotype/Controller.html
- Core Container reference: FactoryBean / factory-extension — https://docs.spring.io/spring-framework/reference/core/beans/factory-extension.html
- Create a Custom FailureAnalyzer with Spring Boot (Baeldung) — https://www.baeldung.com/spring-boot-failure-analyzer
- Creating Your Own Auto-configuration (Spring Boot reference, developing-auto-configuration) — https://docs.spring.io/spring-boot/reference/features/developing-auto-configuration.html
- Creating Your Own Auto-configuration :: Spring Boot Reference (@AutoConfiguration, AutoConfiguration.imports, lite mode) — https://docs.spring.io/spring-boot/reference/features/developing-auto-configuration.html
- CustomAutowireConfigurer (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/annotation/CustomAutowireConfigurer.html
- CustomAutowireConfigurer (Spring Framework API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/annotation/CustomAutowireConfigurer.html
- Customizing the Nature of a Bean (Spring Framework Reference, current) — https://docs.spring.io/spring-framework/reference/core/beans/factory-nature.html
- Customizing the Nature of a Bean (lifecycle callbacks & ordering) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/factory-nature.html
- Customizing the Nature of a Bean / Container Extension Points (BeanFactoryPostProcessor vs BeanPostProcessor) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/factory-extension.html
- Customizing the Nature of a Bean / Container Extension Points — per-container BeanPostProcessor & BeanFactoryPostProcessor scoping (Spring Framework reference) — https://docs.spring.io/spring-framework/reference/core/beans/factory-extension.html
- Customizing the Nature of a Bean / FactoryBean — Spring Framework reference (core.beans.factory-extension / factory-nature) — https://docs.spring.io/spring-framework/reference/core/beans/factory-extension.html
- Customizing the Nature of a Bean — Lifecycle, SmartLifecycle, LifecycleProcessor, init/destroy callbacks (Spring Framework reference) — https://docs.spring.io/spring-framework/reference/core/beans/factory-nature.html
- Data Access - DAO Support (Spring Framework Reference) — https://docs.spring.io/spring-framework/reference/data-access/dao.html
- DataAccessException (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/dao/DataAccessException.html
- DefaultApplicationContextFactory.java source (v4.1.0) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot/src/main/java/org/springframework/boot/DefaultApplicationContextFactory.java
- DefaultLifecycleProcessor (Spring Framework 7.0.x API javadoc) — timeoutPerShutdownPhase, per-phase timeouts, concurrent startup, CRaC — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/support/DefaultLifecycleProcessor.html
- DefaultLifecycleProcessor (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/support/DefaultLifecycleProcessor.html
- DefaultLifecycleProcessor.java source (spring-framework main) — https://github.com/spring-projects/spring-framework/blob/main/spring-context/src/main/java/org/springframework/context/support/DefaultLifecycleProcessor.java
- DefaultListableBeanFactory (Spring Framework 7 API) — STRICT_LOCKING_PROPERTY_NAME, setBootstrapExecutor, prepareSingletonBootstrap, isCurrentThreadAllowedToHoldSingletonLock — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/DefaultListableBeanFactory.html
- DefaultListableBeanFactory (Spring Framework 7.0 API javadoc) — resolveDependency / autowiring engine — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/DefaultListableBeanFactory.html
- DefaultListableBeanFactory (Spring Framework 7.0.x API javadoc) — preInstantiateSingletons, STRICT_LOCKING_PROPERTY_NAME, prepareSingletonBootstrap — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/DefaultListableBeanFactory.html
- DefaultListableBeanFactory (Spring Framework 7.0.x API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/DefaultListableBeanFactory.html
- DefaultListableBeanFactory (Spring Framework API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/DefaultListableBeanFactory.html
- DefaultListableBeanFactory.java (spring-framework 7.0.x source) — preInstantiateSingletons, instantiateSingleton, isEagerInit gate, isCurrentThreadAllowedToHoldSingletonLock override, strictLocking, resolveBean requiredType — https://github.com/spring-projects/spring-framework/blob/7.0.x/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultListableBeanFactory.java
- DefaultListableBeanFactory.java (spring-framework main source) — preInstantiateSingleton, instantiateSingleton (SmartFactoryBean isEagerInit), isCurrentThreadAllowedToHoldSingletonLock override — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultListableBeanFactory.java
- DefaultListableBeanFactory.java (spring-framework main, preInstantiateSingletons two-loop + smart-initialize step) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultListableBeanFactory.java
- DefaultListableBeanFactory.java (v7.0.0 source) — STRICT_LOCKING_PROPERTY_NAME and isCurrentThreadAllowedToHoldSingletonLock override — https://github.com/spring-projects/spring-framework/blob/v7.0.0/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultListableBeanFactory.java
- DefaultListableBeanFactory.java (v7.0.5) — doResolveDependency, determineAutowireCandidate, determinePrimaryCandidate, determineHighestPriorityCandidate, isFallback — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultListableBeanFactory.java
- DefaultListableBeanFactory.java preInstantiateSingletons instantiateSingleton spring-framework main — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultListableBeanFactory.java
- DefaultListableBeanFactory.java source (setAutowireCandidateResolver / copyConfigurationFrom / determineAutowireCandidate) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultListableBeanFactory.java
- DefaultListableBeanFactory.java source (spring-framework main / 7.x) — resolveMultipleBeans (array/List sorted, Map not), adaptOrderComparator, FactoryAwareOrderSourceProvider, getPriority, determineDefaultCandidate — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultListableBeanFactory.java
- DefaultSingletonBeanRegistry (Spring Framework 7.0.x API javadoc) — isCurrentThreadAllowedToHoldSingletonLock, FactoryBean-prefix-unaware lookups — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/DefaultSingletonBeanRegistry.html
- DefaultSingletonBeanRegistry (Spring Framework 7.0.x API) — isCurrentThreadAllowedToHoldSingletonLock javadoc — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/DefaultSingletonBeanRegistry.html
- DefaultSingletonBeanRegistry (Spring Framework 7.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/DefaultSingletonBeanRegistry.html
- DefaultSingletonBeanRegistry source (main) — singleton creation locking / getSingleton — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultSingletonBeanRegistry.java
- DefaultSingletonBeanRegistry.java (spring-framework 7.0.x source) — singletonLock ReentrantLock, lenientCreationLock, isCurrentThreadAllowedToHoldSingletonLock @since 6.2, getSingletonMutex — https://github.com/spring-projects/spring-framework/blob/7.0.x/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultSingletonBeanRegistry.java
- DefaultSingletonBeanRegistry.java (v7.0.0 source) — spring-projects/spring-framework — https://github.com/spring-projects/spring-framework/blob/v7.0.0/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultSingletonBeanRegistry.java
- DefaultSingletonBeanRegistry.java source (spring-framework main) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultSingletonBeanRegistry.java
- DeferredImportSelector (Spring Framework 7.0 API javadoc) — Group, getImportGroup — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/DeferredImportSelector.html
- Dependencies and Configuration in Detail / Circular Dependencies (Spring Framework Reference) — https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-collaborators.html
- Deprecate spring-context-indexer (Issue #30431) — https://github.com/spring-projects/spring-framework/issues/30431
- DurationUnit / DataSizeUnit / PeriodUnit (org.springframework.boot.convert package summary) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/api/java/org/springframework/boot/convert/package-summary.html
- EnableAspectJAutoProxy (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/EnableAspectJAutoProxy.html
- EnableAutoConfiguration (Spring Boot 4.1 API javadoc) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/autoconfigure/EnableAutoConfiguration.html
- EnableAutoConfiguration (Spring Boot 4.x API javadoc) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/autoconfigure/EnableAutoConfiguration.html
- EnableAutoConfiguration.java (Spring Boot v4.0.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.0.0/core/spring-boot-autoconfigure/src/main/java/org/springframework/boot/autoconfigure/EnableAutoConfiguration.java
- EnableAutoConfiguration.java (Spring Boot v4.1.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot-autoconfigure/src/main/java/org/springframework/boot/autoconfigure/EnableAutoConfiguration.java
- Environment (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/env/Environment.html
- Environment (Spring Framework 7.0.7 API) javadoc — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/env/Environment.html
- Environment Abstraction / Bean Definition Profiles :: Spring Framework Reference (core/beans/environment) — https://docs.spring.io/spring-framework/reference/core/beans/environment.html
- Environment Abstraction :: Spring Framework (7.x reference) — https://docs.spring.io/spring-framework/reference/core/beans/environment.html
- EventListener.java source (spring-framework main) — defaultExecution attribute — https://github.com/spring-projects/spring-framework/blob/main/spring-context/src/main/java/org/springframework/context/event/EventListener.java
- EventPublishingRunListener.java source (v4.1.0) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot/src/main/java/org/springframework/boot/context/event/EventPublishingRunListener.java
- External Application Properties / Profile Specific Files :: Spring Boot Reference — https://docs.spring.io/spring-boot/reference/features/external-config.html
- Externalized Configuration :: Spring Boot (4.x reference) — https://docs.spring.io/spring-boot/reference/features/external-config.html
- FactoryBean (Spring Framework 7.0.x API javadoc) — getObjectType null/autowiring note, isSingleton caching note, OBJECT_TYPE_ATTRIBUTE, programmatic-contract note — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/FactoryBean.html
- FactoryBean (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/FactoryBean.html
- FactoryBean 6.2.x source (for v6 baseline comparison: old getObjectFromFactoryBean signature, SmartFactoryBean without supportsType) — https://github.com/spring-projects/spring-framework/blob/6.2.x/spring-beans/src/main/java/org/springframework/beans/factory/support/FactoryBeanRegistrySupport.java
- FactoryBeanRegistrySupport.java (spring-framework 7.0.x source) — factoryBeanObjectCache, getObjectFromFactoryBean double-checked locking, synchronized(factory), NullBean, postProcessObjectFromSingletonFactoryBean — https://github.com/spring-projects/spring-framework/blob/7.0.x/spring-beans/src/main/java/org/springframework/beans/factory/support/FactoryBeanRegistrySupport.java
- FactoryBeanRegistrySupport.java (spring-framework main source) — getObjectFromFactoryBean / doGetObjectFromFactoryBean locking & caching — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/FactoryBeanRegistrySupport.java
- FailureAnalyzers.java source (v4.1.0) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot/src/main/java/org/springframework/boot/diagnostics/FailureAnalyzers.java
- Fallback (Spring Framework 6.2.0 API javadoc) — https://docs.spring.io/spring-framework/docs/6.2.0/javadoc-api/org/springframework/context/annotation/Fallback.html
- Fallback (Spring Framework 6.2.0 API) — https://docs.spring.io/spring-framework/docs/6.2.0/javadoc-api/org/springframework/context/annotation/Fallback.html
- Fallback (Spring Framework 7.0.6 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Fallback.html
- Fine-tuning Annotation-based Autowiring with @Primary or @Fallback (reference docs) — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired-primary.html
- Fine-tuning Annotation-based Autowiring with @Primary or @Fallback :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired-primary.html
- Fine-tuning Annotation-based Autowiring with @Primary or @Fallback :: Spring Framework — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired-primary.html
- Fine-tuning Annotation-based Autowiring with Qualifiers (reference docs) — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired-qualifiers.html
- Fine-tuning Annotation-based Autowiring with Qualifiers :: Spring Framework (7.0.5 docs source) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/annotation-config/autowired-qualifiers.adoc
- First-class support for BeanRegistrar registration on GenericApplicationContext (Issue #34574) — https://github.com/spring-projects/spring-framework/issues/34574
- From Spring Framework 6.2 to 7.0 (roadmap blog) — https://spring.io/blog/2024/10/01/from-spring-framework-6-2-to-7-0/
- From Spring Framework 6.2 to 7.0 (spring.io blog) — https://spring.io/blog/2024/10/01/from-spring-framework-6-2-to-7-0/
- FullyQualifiedAnnotationBeanNameGenerator (Spring Framework 7.0.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/FullyQualifiedAnnotationBeanNameGenerator.html
- GenericApplicationContext (Spring Framework 7.0 API javadoc) — register(BeanRegistrar...) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/support/GenericApplicationContext.html
- GenericApplicationContext (Spring Framework current API Javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/support/GenericApplicationContext.html
- GenericApplicationContext.getBeanFactory() (Spring Framework 7.0.x API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/support/GenericApplicationContext.html
- GenericTypeAwareAutowireCandidateResolver (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/support/GenericTypeAwareAutowireCandidateResolver.html
- GenericTypeAwareAutowireCandidateResolver.java source (spring-framework main) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/support/GenericTypeAwareAutowireCandidateResolver.java
- GitHub Issue #23501 — Synchronization during singleton creation may result in deadlock — https://github.com/spring-projects/spring-framework/issues/23501
- GitHub Issue #25667 — Avoid full singleton lock for DefaultSingletonBeanRegistry.getSingleton(beanName, false) — https://github.com/spring-projects/spring-framework/issues/25667
- GitHub Issue #34349 — Continue with pre-instantiation when current bean is in creation already (lenient locking) — https://github.com/spring-projects/spring-framework/issues/34349
- Graceful Shutdown (Spring Boot 4.0 reference) — https://github.com/spring-projects/spring-boot/blob/v4.0.5/documentation/spring-boot-docs/src/docs/antora/modules/reference/pages/web/graceful-shutdown.adoc
- HierarchicalBeanFactory (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/HierarchicalBeanFactory.html
- HierarchicalBeanFactory (getParentBeanFactory / containsLocalBean) Javadoc — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/HierarchicalBeanFactory.html
- ImportAutoConfiguration.java (Spring Boot v4.1.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot-autoconfigure/src/main/java/org/springframework/boot/autoconfigure/ImportAutoConfiguration.java
- ImportBeanDefinitionRegistrar (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/ImportBeanDefinitionRegistrar.html
- ImportCandidates.java (Spring Boot v4.1.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot/src/main/java/org/springframework/boot/context/annotation/ImportCandidates.java
- Improve BeanFactory/ObjectProvider to select the only one default candidate among non-default candidates (issue #34432) — https://github.com/spring-projects/spring-framework/issues/34432
- Indexed (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/stereotype/Indexed.html
- Infrastructure Advisors and Spring ApplicationContext lifecycle (Dave Syer gist) — https://gist.github.com/dsyer/ebeb25d5afbdd9242cd5
- InfrastructureAdvisorAutoProxyCreator (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/aop/framework/autoproxy/InfrastructureAdvisorAutoProxyCreator.html
- Introduce Environment.matchesProfiles() for profile expressions (Issue #30206) — https://github.com/spring-projects/spring-framework/issues/30206
- Introducing GraalVM Native Images :: Spring Boot — https://docs.spring.io/spring-boot/reference/packaging/native-image/introducing-graalvm-native-images.html
- Introduction to Proxies :: Spring Framework (reference) — https://docs.spring.io/spring-framework/reference/core/aop/introduction-proxies.html
- Introduction to the Spring IoC Container and Beans :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/introduction.html
- Issue #23501 — Synchronization during singleton creation may result in deadlock — https://github.com/spring-projects/spring-framework/issues/23501
- Issue #25667 — Avoid full singleton lock for DefaultSingletonBeanRegistry.getSingleton(beanName, false) — https://github.com/spring-projects/spring-framework/issues/25667
- Issue #25667: Avoid full singleton lock for getSingleton(beanName, false) — https://github.com/spring-projects/spring-framework/issues/25667
- Issue #30887 — DefaultListableBeanFactory#getSingletonFactoryBeanForTypeCheck deadlock — https://github.com/spring-projects/spring-framework/issues/30887
- Issue #33972 — BeanCurrentlyInCreationException when multiple threads create a FactoryBean (6.2.0 regression, fixed 6.2.1) — https://github.com/spring-projects/spring-framework/issues/33972
- Issue #34349 — Continue with pre-instantiation when current bean is in creation already (fixed 6.2.3) — https://github.com/spring-projects/spring-framework/issues/34349
- Issue #34349 — Continue with pre-instantiation when current bean is in creation already — https://github.com/spring-projects/spring-framework/issues/34349
- Issue #35545 — Thread race during bean instantiations starting with 6.2 due to lenient locks — https://github.com/spring-projects/spring-framework/issues/35545
- Issue #36648: Beans created with BeanRegistrar or GenericApplicationContext do not honor default allow-bean-definition-overriding — https://github.com/spring-projects/spring-framework/issues/36648
- Lazy (@Lazy at injection points → lazy-resolution proxy; recommends ObjectProvider) Spring Framework 7.0.x API — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Lazy.html
- Lazy (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Lazy.html
- Lazy Initialization :: Spring Boot 4 Reference (SpringApplication) — https://docs.spring.io/spring-boot/reference/features/spring-application.html
- Lazy SmartLifecycle non autoStarting bean being initialized [SPR-7014] · Issue #11679 — https://github.com/spring-projects/spring-framework/issues/11679
- Lazy-initialized Beans :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-lazy-init.html
- Lazy-initialized Beans factory-lazy-init Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-lazy-init.html
- LazyInitializationBeanFactoryPostProcessor (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/LazyInitializationBeanFactoryPostProcessor.html
- LazyInitializationExcludeFilter (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/LazyInitializationExcludeFilter.html
- Let Spring Boot consistently switch to CGLIB proxies for any proxy processor — spring-framework issue #35286 — https://github.com/spring-projects/spring-framework/issues/35286
- Lifecycle (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/Lifecycle.html
- LifecycleProcessor (Spring Framework 7.0.x API javadoc) — onPause/onRestart — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/LifecycleProcessor.html
- ListableBeanFactory (Spring Framework 7.0 API javadoc) — getBeanNamesForType hierarchy caveat — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/ListableBeanFactory.html
- LocaleContextHolder (Spring Framework 7.0.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/i18n/LocaleContextHolder.html
- LocaleContextHolder source (spring-projects/spring-framework, main) — https://github.com/spring-projects/spring-framework/blob/main/spring-context/src/main/java/org/springframework/context/i18n/LocaleContextHolder.java
- MergedAnnotations (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/annotation/MergedAnnotations.html
- MetadataReader (org.springframework.core.type.classreading) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/type/classreading/MetadataReader.html
- Micrometer Context Propagation — Purpose — https://docs.micrometer.io/context-propagation/reference/purpose.html
- Modularizing Spring Boot (Spring Blog, 28 Oct 2025) — https://spring.io/blog/2025/10/28/modularizing-spring-boot/
- Modularizing Spring Boot (spring.io blog, 2025-10-28) — https://spring.io/blog/2025/10/28/modularizing-spring-boot/
- Move away from spring.factories for auto-configuration imports — spring-boot issue #29698 — https://github.com/spring-projects/spring-boot/issues/29698
- MutablePropertySources source (spring-framework GitHub) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-core/src/main/java/org/springframework/core/env/MutablePropertySources.java
- Naming Beans / Aliasing a Bean :: Spring Framework — https://docs.spring.io/spring-framework/reference/core/beans/definition.html
- NoUniqueBeanDefinitionException (Spring Framework API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/NoUniqueBeanDefinitionException.html
- NoUniqueBeanDefinitionException (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/index-files/index-14.html
- Null-safe applications with Spring Boot 4 (spring.io blog) — JSpecify nullness at injection points — https://spring.io/blog/2025/11/12/null-safe-applications-with-spring-boot-4/
- Null-safe applications with Spring Boot 4 (spring.io blog) — https://spring.io/blog/2025/11/12/null-safe-applications-with-spring-boot-4/
- Null-safety (Spring Framework 7.0 Reference) — https://docs.spring.io/spring-framework/reference/7.0-SNAPSHOT/core/null-safety.html
- ORM - Exception Translation / PersistenceExceptionTranslationPostProcessor (Spring Framework Reference) — https://docs.spring.io/spring-framework/reference/data-access/orm/general.html
- ObjectProvider (Spring Framework 7.0 API javadoc) — stream()/orderedStream()/iterator()/UNFILTERED — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/ObjectProvider.html
- ObjectProvider (Spring Framework 7.0 API) - uniqueness algorithm — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/ObjectProvider.html
- ObjectProvider (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/ObjectProvider.html
- ObjectProvider (Spring Framework current/7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/ObjectProvider.html
- ObjectProvider.java source (spring-framework main) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/ObjectProvider.java
- ObjectProvider.java source (spring-framework v7.0.1) — https://raw.githubusercontent.com/spring-projects/spring-framework/v7.0.1/spring-beans/src/main/java/org/springframework/beans/factory/ObjectProvider.java
- ObservationThreadLocalAccessor source (micrometer-metrics/micrometer) — https://github.com/micrometer-metrics/micrometer/blob/main/micrometer-observation/src/main/java/io/micrometer/observation/contextpropagation/ObservationThreadLocalAccessor.java
- OnPropertyCondition source (spring-boot-autoconfigure) — ConditionOutcome/ConditionMessage — https://github.com/spring-projects/spring-boot/blob/main/spring-boot/core/spring-boot-autoconfigure/src/main/java/org/springframework/boot/autoconfigure/condition/OnPropertyCondition.java
- OrderComparator (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/OrderComparator.html
- OrderComparator.java source (spring-framework main / 7.x) — doCompare, withSourceProvider, getOrder @since 7.0 — https://github.com/spring-projects/spring-framework/blob/main/spring-core/src/main/java/org/springframework/core/OrderComparator.java
- Ordered (Spring Framework 7.0 API javadoc) — HIGHEST/LOWEST_PRECEDENCE — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/Ordered.html
- OriginLookup (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/origin/OriginLookup.html
- OriginTrackedMapPropertySource (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/env/OriginTrackedMapPropertySource.html
- Perform basic property determination without java.beans.Introspector (spring-framework issue #29320) — https://github.com/spring-projects/spring-framework/issues/29320
- PersistenceExceptionTranslationAutoConfiguration (Spring Boot API) — https://docs.spring.vmware.com/spring-boot/docs/3.2.14.1/api/org/springframework/boot/autoconfigure/dao/PersistenceExceptionTranslationAutoConfiguration.html
- PersistenceExceptionTranslationPostProcessor (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/dao/annotation/PersistenceExceptionTranslationPostProcessor.html
- PostProcessorRegistrationDelegate source (Spring Framework v7.0.5) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-context/src/main/java/org/springframework/context/support/PostProcessorRegistrationDelegate.java
- Profile (Spring Framework 7.0.x API) javadoc — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Profile.html
- Profiles (Spring Framework 7.0.x API) javadoc — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/env/Profiles.html
- Profiles :: Spring Boot 4.x Reference (features/profiles) — https://docs.spring.io/spring-boot/reference/features/profiles.html
- Profiles retained during AOT processing are not configured in a native image (spring-boot issue #48408) — https://github.com/spring-projects/spring-boot/issues/48408
- Programmatic Bean Registration (BeanRegistrar / BeanRegistry / BeanRegistrarDsl) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/java/programmatic-bean-registration.html
- Programmatic Bean Registration (BeanRegistrar) :: Spring Framework 7 Reference — https://docs.spring.io/spring-framework/reference/core/beans/java/programmatic-bean-registration.html
- Programmatic Bean Registration (BeanRegistrar) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/java/programmatic-bean-registration.html
- Programmatic Bean Registration (BeanRegistrar) :: Spring Framework — https://docs.spring.io/spring-framework/reference/core/beans/java/programmatic-bean-registration.html
- Programmatic Bean Registration :: Spring Framework Reference (7.x) — https://docs.spring.io/spring-framework/reference/core/beans/java/programmatic-bean-registration.html
- Programmatic Bean Registration :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/java/programmatic-bean-registration.html
- Programmatic Bean Registration Mechanism With BeanRegistrar in Spring (Baeldung) — https://www.baeldung.com/spring-beanregistrar-registration
- Programmatic Bean Registration Mechanism With BeanRegistrar in Spring | Baeldung — https://www.baeldung.com/spring-beanregistrar-registration
- Programmatic Bean Registration with BeanRegistrar (Baeldung) — https://www.baeldung.com/spring-beanregistrar-registration
- Programmatic Bean Registration with BeanRegistrar - Spring 7 Cookbook (Hantsy) — https://hantsy.github.io/spring7-sandbox/bean-reg/
- Programmatic Bean Registration with BeanRegistrar — Spring 7 Cookbook (Hantsy) — https://hantsy.github.io/spring7-sandbox/bean-reg/
- PropagationContextElement (Spring Framework 7.0 kdoc) — https://docs.spring.io/spring-framework/docs/7.0.0-RC1/kdoc-api/spring-core/org.springframework.core/-propagation-context-element/index.html
- ProviderCreatingFactoryBean (Spring Framework 7.0.x API — exposes jakarta.inject.Provider) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/config/ProviderCreatingFactoryBean.html
- ProxyConfig (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/aop/framework/ProxyConfig.html
- ProxyFactoryBean.java (spring-framework 7.0.x source) — plain FactoryBean, own singletonInstance cache, getObjectType early proxy class prediction — https://github.com/spring-projects/spring-framework/blob/7.0.x/spring-aop/src/main/java/org/springframework/aop/framework/ProxyFactoryBean.java
- Proxying Mechanisms :: Spring Framework (reference) — https://docs.spring.io/spring-framework/reference/core/aop/proxying.html
- QualifierAnnotationAutowireCandidateResolver (Spring Framework 7.0 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/annotation/QualifierAnnotationAutowireCandidateResolver.html
- QualifierAnnotationAutowireCandidateResolver (Spring Framework API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/annotation/QualifierAnnotationAutowireCandidateResolver.html
- QualifierAnnotationAutowireCandidateResolver.java source (spring-framework main) — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/annotation/QualifierAnnotationAutowireCandidateResolver.java
- Reactor Core Reference — Context-Propagation Support — https://projectreactor.io/docs/core/release/reference/advanced-contextPropagation.html
- Repository (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/stereotype/Repository.html
- RequestContextHolder source (spring-projects/spring-framework, main) — https://github.com/spring-projects/spring-framework/blob/main/spring-web/src/main/java/org/springframework/web/context/request/RequestContextHolder.java
- RestController (Spring Framework 7.0.7 API javadoc) — package org.springframework.web.bind.annotation — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/web/bind/annotation/RestController.html
- Role (Spring Framework 7.0.5 API javadoc) — ROLE_APPLICATION/SUPPORT/INFRASTRUCTURE — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Role.html
- Scope (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/Scope.html
- Scoped Values vs ThreadLocal in Java 25 — Safer Context Propagation — https://www.springjavalab.com/2025/12/scoped-values-vs-threadlocal-java-25.html
- ScopedProxyMode (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/annotation/ScopedProxyMode.html
- Service (Spring Framework current API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/stereotype/Service.html
- SmartFactoryBean (Spring Framework 7.0.8 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/SmartFactoryBean.html
- SmartFactoryBean (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/SmartFactoryBean.html
- SmartFactoryBean.java (spring-framework main source) — isPrototype, isEagerInit, supportsType @since 7.0, getObject(Class) @since 7.0 — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/SmartFactoryBean.java
- SmartInitializingSingleton (Spring Framework 7.0.8 API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/beans/factory/SmartInitializingSingleton.html
- SmartInitializingSingleton source — https://github.com/spring-projects/spring-framework/blob/main/spring-beans/src/main/java/org/springframework/beans/factory/SmartInitializingSingleton.java
- SmartInstantiationAwareBeanPostProcessor.getEarlyBeanReference javadoc (7.0.x) — https://docs.spring.io/spring-framework/docs/7.0.x/javadoc-api/org/springframework/beans/factory/config/SmartInstantiationAwareBeanPostProcessor.html
- SmartLifecycle (Spring Framework 7.0.x API javadoc) — DEFAULT_PHASE=Integer.MAX_VALUE, isPauseable() since 7.0, stop(Runnable) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/SmartLifecycle.html
- SmartLifecycle (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/SmartLifecycle.html
- SmartLifecycle.java source (spring-framework main) — https://github.com/spring-projects/spring-framework/blob/main/spring-context/src/main/java/org/springframework/context/SmartLifecycle.java
- Spring AOT :: Spring Framework Reference (@Configuration(proxyBeanMethods=false) for AOT) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/aot.adoc
- Spring Annotation Programming Model (wiki) — https://github.com/spring-projects/spring-framework/wiki/Spring-Annotation-Programming-Model
- Spring Boot 'Default code generation is not supported for bean definitions declaring an instance supplier callback' (issue #38185) — https://github.com/spring-projects/spring-boot/issues/38185
- Spring Boot 2.6 Release Notes (circular references disabled by default) — https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-2.6-Release-Notes
- Spring Boot 2.6 Release Notes: circular references prohibited by default — https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-2.6-Release-Notes
- Spring Boot 4 & Spring Framework 7 – What's New (Baeldung) — https://www.baeldung.com/spring-boot-4-spring-framework-7
- Spring Boot 4 source — WebServerApplicationContext.java (phase constants) — https://github.com/spring-projects/spring-boot/blob/main/module/spring-boot-web-server/src/main/java/org/springframework/boot/web/server/context/WebServerApplicationContext.java
- Spring Boot 4 source — WebServerGracefulShutdownLifecycle.java — https://github.com/spring-projects/spring-boot/blob/main/module/spring-boot-web-server/src/main/java/org/springframework/boot/web/server/context/WebServerGracefulShutdownLifecycle.java
- Spring Boot 4 source — WebServerStartStopLifecycle.java (servlet) — https://github.com/spring-projects/spring-boot/blob/main/module/spring-boot-web-server/src/main/java/org/springframework/boot/web/server/servlet/context/WebServerStartStopLifecycle.java
- Spring Boot 4's Bean Registrar: A Cleaner Way to Register Beans Programmatically (Dan Vega) — https://www.danvega.dev/blog/programmatic-bean-registration
- Spring Boot 4's Bean Registrar: A Cleaner Way to Register Beans Programmatically | Dan Vega — https://www.danvega.dev/blog/programmatic-bean-registration
- Spring Boot 4.0 AOT how-to (build-time profiles) — https://github.com/spring-projects/spring-boot/blob/v4.0.0/documentation/spring-boot-docs/src/docs/antora/modules/how-to/pages/aot.adoc
- Spring Boot 4.0 AbstractApplicationContextRunner.withAllowCircularReferences (defaults false) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/api/java/org/springframework/boot/test/context/runner/AbstractApplicationContextRunner.html
- Spring Boot 4.0 Migration Guide (GitHub wiki) — https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-4.0-Migration-Guide
- Spring Boot 4.0 Migration Guide (wiki) — https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-4.0-Migration-Guide
- Spring Boot 4.0 Reference — Developing Auto-configuration (Condition Annotations) — https://docs.spring.io/spring-boot/4.0/reference/features/developing-auto-configuration.html
- Spring Boot 4.0 Release Notes (GitHub wiki) — Framework 7 baseline, auto-config API changes — https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-4.0-Release-Notes
- Spring Boot 4.0 Release Notes (GitHub wiki) — https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-4.0-Release-Notes
- Spring Boot 4.0 Release Notes (wiki) — https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-4.0-Release-Notes
- Spring Boot 4.0 SpringApplication API (setAllowCircularReferences defaults false, since 2.6) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/api/java/org/springframework/boot/SpringApplication.html
- Spring Boot 4.0.0 Release Notes (wiki) — https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-4.0.0-Release-Notes
- Spring Boot 4.0.0 available now (2025-11-20, JDK 17 baseline) — https://spring.io/blog/2025/11/20/spring-boot-4-0-0-available-now/
- Spring Boot 4.0.0 available now (Spring blog) — https://spring.io/blog/2025/11/20/spring-boot-4-0-0-available-now/
- Spring Boot 4.0.0 available now (blog, 2025-11-20) — https://spring.io/blog/2025/11/20/spring-boot-4-0-0-available-now/
- Spring Boot 4.0.0 available now (spring.io blog, 2025-11-20) — https://spring.io/blog/2025/11/20/spring-boot-4-0-0-available-now/
- Spring Boot 4.0.0 available now — https://spring.io/blog/2025/11/20/spring-boot-4-0-0-available-now/
- Spring Boot 4.0.0 profiles.adoc source (v4.0.0 tag) — https://github.com/spring-projects/spring-boot/blob/v4.0.0/documentation/spring-boot-docs/src/docs/antora/modules/reference/pages/features/profiles.adoc
- Spring Boot How-To: Application customization (context type, initializers, EnvironmentPostProcessor) — https://docs.spring.io/spring-boot/how-to/application.html
- Spring Boot How-to: Create Your Own FailureAnalyzer (Boot 4.1 docs) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/documentation/spring-boot-docs/src/docs/antora/modules/how-to/pages/application.adoc
- Spring Boot Reference — Application Events and Listeners (spring-application) — https://docs.spring.io/spring-boot/reference/features/spring-application.html
- Spring Boot Reference — Graceful Shutdown (spring.lifecycle.timeout-per-shutdown-phase, server.shutdown) — https://docs.spring.io/spring-boot/reference/web/graceful-shutdown.html
- Spring Boot Reference — Task Execution and Scheduling (applicationTaskExecutor, taskExecutor, spring.threads.virtual.enabled, spring.task.execution.mode, builders) — https://docs.spring.io/spring-boot/reference/features/task-execution-and-scheduling.html
- Spring Boot Reference — Task Execution and Scheduling — https://docs.spring.io/spring-boot/reference/features/task-execution-and-scheduling.html
- Spring Boot Reference: SpringApplication - Startup Failure (Boot 4.1 docs) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/documentation/spring-boot-docs/src/docs/antora/modules/reference/pages/features/spring-application.adoc
- Spring Boot Reference: SpringApplication features (events, runners, banner, failure analysis) — https://docs.spring.io/spring-boot/reference/features/spring-application.html
- Spring Boot System Requirements — https://docs.spring.io/spring-boot/system-requirements.html
- Spring Boot issue #31714 — Change phases of WebServer start-stop and graceful shutdown lifecycles — https://github.com/spring-projects/spring-boot/issues/31714
- Spring Boot: Developing Auto-configuration (AutoConfiguration.imports vs spring.factories, Boot 4.1 docs) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/documentation/spring-boot-docs/src/docs/antora/modules/reference/pages/features/developing-auto-configuration.adoc
- Spring Data Ahead-of-Time Repositories (spring.io blog) — https://spring.io/blog/2025/05/22/spring-data-ahead-of-time-repositories/
- Spring Framework 6.2 Release Notes (@Fallback, defaultCandidate, ObjectProvider stream/predicate additions) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-6.2-Release-Notes
- Spring Framework 6.2 Release Notes (revised autowiring algorithm, @Fallback) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-6.2-Release-Notes
- Spring Framework 6.2 Release Notes (singleton locking revision, background bootstrap, spring.locking.strict) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-6.2-Release-Notes
- Spring Framework 6.2 Release Notes (strict/lenient locking, bootstrap=BACKGROUND) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-6.2-Release-Notes
- Spring Framework 6.2 Release Notes background bean init strict lenient locking — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-6.2-Release-Notes
- Spring Framework 6.2 Release Notes — background bean initialization, mix of strict/lenient locking, spring.locking.strict — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-6.2-Release-Notes
- Spring Framework 6.2 Release Notes — strict vs lenient locking, spring.locking.strict (6.2.6), background initialization — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-6.2-Release-Notes
- Spring Framework 6.2 Release Notes: strict/lenient singleton locking, spring.locking.strict — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-6.2-Release-Notes
- Spring Framework 6.2 — Fallback Annotation (Eric Anicet, Medium) — https://boottechnologies-ci.medium.com/spring-framework-6-2-fallback-annotation-051e046ce182
- Spring Framework 6.2.0-M1 all the little things bootstrap BACKGROUND — https://spring.io/blog/2024/04/11/spring-framework-6-2-0-m1-all-the-little-things/
- Spring Framework 6.2.0-M1: all the little things (spring.io blog) — https://spring.io/blog/2024/04/11/spring-framework-6-2-0-m1-all-the-little-things/
- Spring Framework 7.0 General Availability (2025-11-13) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (Java 17 baseline / Java 25 / Jakarta EE 11 / JSpecify) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (Java 17 baseline / Java 25, Jakarta EE 11) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (Spring blog) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (blog, 2025-11-13) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (spring.io blog) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (spring.io blog, 13 Nov 2025) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (spring.io blog, 2025-11-13) — Java 17 baseline, Java 25 support, JSpecify — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (spring.io blog, 2025-11-13) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (spring.io blog, Nov 13 2025) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability (spring.io blog, Nov 2025) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 General Availability — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 Generally Available (GA Nov 13, 2025; Java 17/25, Jakarta EE 11, JSpecify null safety) — https://spring.io/blog/2025/11/13/spring-framework-7-0-general-availability/
- Spring Framework 7.0 Reference — Conditionally Include @Configuration Classes or @Bean Methods (composing-configuration-classes) — https://docs.spring.io/spring-framework/reference/core/beans/java/composing-configuration-classes.html
- Spring Framework 7.0 Release Notes (BeanRegistrar, @Import on interfaces, CGLIB proxy defaulting, @Proxyable) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (BeanRegistrar, ClassFileMetadataReader / JEP 484 Class-File API) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (GitHub wiki) — JSpecify, javax removal, DI changes — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (GitHub wiki) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (GitHub wiki: CGLIB default, @Proxyable, JSpecify, javax removal, BeanRegistrar) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (JDK 17 / Jakarta EE 11 baseline, javax removal, JSpecify, @Proxyable) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (JSpecify nullness, Jakarta EE 11, javax.inject removal, Java baselines) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (JSpecify, BeanRegistrar, no lifecycle ordering changes) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (Jakarta EE 11 baseline, javax.inject removal, JDK 17 minimum) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (programmatic bean registration, JSpecify null-safety, proxy defaulting) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (wiki) — JSpecify nullness, BeanRegistrar, JDK baseline, Boot 4 foundation — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (wiki) — JSpecify nullness, Jakarta EE 11 / JDK 17 baseline — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (wiki) — LocalSessionFactoryBean Session/StatelessSession injection — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes (wiki) — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes — consistent CGLIB proxy defaulting, @Proxyable, programmatic bean registration — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 Release Notes — https://github.com/spring-projects/spring-framework/wiki/Spring-Framework-7.0-Release-Notes
- Spring Framework 7.0 core/aot reference: circular dependencies fail AOT, use @Lazy / ObjectProvider — https://github.com/spring-projects/spring-framework/blob/v7.0.7/framework-docs/modules/ROOT/pages/core/aot.adoc
- Spring Framework 7.0.0-M3 Available Now (BeanRegistrar introduction) — https://spring.io/blog/2025/03/13/spring-framework-7-0-0-M3-available-now/
- Spring Framework 7.0.0-M4 Available Now (Class-File API / ClassFileMetadataReader) — https://spring.io/blog/2025/04/17/spring-framework-7-0-0-M4-available-now/
- Spring Framework 7.0.5 core/aot.adoc source (framework-docs) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/aot.adoc
- Spring Framework Observability Reference — async event multicaster + ContextPropagatingTaskDecorator — https://docs.spring.io/spring-framework/reference/integration/observability.html
- Spring Framework Reference — @Component and Further Stereotype Annotations (classpath scanning) — https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html
- Spring Framework Reference — @Value annotations (placeholders, SpEL, defaults, BeanPostProcessor/ConversionService) — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/value-annotations.html
- Spring Framework Reference — AOT Best Practices: Avoid Circular Dependencies (use @Lazy / ObjectProvider) (v7.0.5 source: aot.adoc) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/aot.adoc
- Spring Framework Reference — Annotation-based Container Configuration / Stereotype Annotations — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config.html
- Spring Framework Reference — Autowiring Collaborators (modes, limitations, autowire-candidate) — https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-autowire.html
- Spring Framework Reference — Bean Factory Nature (Thread Safety and Visibility) — https://docs.spring.io/spring-framework/reference/core/beans/factory-nature.html
- Spring Framework Reference — Bean Scopes (singleton vs prototype, stateless/stateful) — https://docs.spring.io/spring-framework/reference/core/beans/factory-scopes.html
- Spring Framework Reference — Bean Scopes: Alternatives to Scoped Proxies (factory-scopes) — https://docs.spring.io/spring-framework/reference/core/beans/factory-scopes.html
- Spring Framework Reference — Classpath Scanning (@Lazy on injection points -> lazy-resolution proxy) (v7.0.5 source: classpath-scanning.adoc) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/classpath-scanning.adoc
- Spring Framework Reference — Classpath Scanning: Meta-annotations and Composed Annotations — https://docs.spring.io/spring-framework/reference/core/beans/classpath-scanning.html
- Spring Framework Reference — Composing Java-based Configuration (Background Bean Initialization / Concurrent Startup) — https://docs.spring.io/spring-framework/reference/core/beans/java/composing-configuration-classes.html
- Spring Framework Reference — Core / Factory Nature: Startup and Shutdown Callbacks — https://docs.spring.io/spring-framework/reference/core/beans/factory-nature.html
- Spring Framework Reference — DAO Support / Consistent Exception Hierarchy — https://docs.spring.io/spring-framework/reference/data-access/dao.html
- Spring Framework Reference — Dependencies and Configuration / Constructor vs Setter Injection (v7.0.5 source: factory-collaborators.adoc) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/dependencies/factory-collaborators.adoc
- Spring Framework Reference — Dependency Injection (constructor vs setter, circular dependencies) — https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-collaborators.html
- Spring Framework Reference — Fine-tuning with @Primary or @Fallback (v7.0.5 source: autowired-primary.adoc) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/annotation-config/autowired-primary.adoc
- Spring Framework Reference — JPA (LocalContainerEntityManagerFactoryBean background bootstrapping) — https://docs.spring.io/spring-framework/reference/data-access/orm/jpa.html
- Spring Framework Reference — JSR-330 Standard Annotations (@Inject, jakarta.inject.Provider) (v7.0.5 source: standard-annotations.adoc) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/standard-annotations.adoc
- Spring Framework Reference — Kotlin Coroutines (context propagation) — https://docs.spring.io/spring-framework/reference/languages/kotlin/coroutines.html
- Spring Framework Reference — ORM, Exception Translation (PersistenceExceptionTranslationPostProcessor / @Repository) — https://docs.spring.io/spring-framework/reference/data-access/orm/general.html
- Spring Framework Reference — Observability Support — https://docs.spring.io/spring-framework/reference/integration/observability.html
- Spring Framework Reference — Qualifier-based autowiring (v7.0.5 source: autowired-qualifiers.adoc) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/annotation-config/autowired-qualifiers.adoc
- Spring Framework Reference — Resilience (@ConcurrencyLimit, @Retryable, @EnableResilientMethods) — https://docs.spring.io/spring-framework/reference/core/resilience.html
- Spring Framework Reference — Scoped Beans / Alternatives to Scoped Proxies (ObjectFactory, ObjectProvider, Provider) (v7.0.5 source: factory-scopes.adoc) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/factory-scopes.adoc
- Spring Framework Reference — Standard and Custom Events (context-introduction) — https://docs.spring.io/spring-framework/reference/core/beans/context-introduction.html
- Spring Framework Reference — Task Execution and Scheduling (TaskExecutor, @EnableAsync/@Async, @EnableScheduling/@Scheduled, virtual threads) — https://docs.spring.io/spring-framework/reference/integration/scheduling.html
- Spring Framework Reference — Using @Autowired (single-constructor, required, Optional, @Nullable, collections, ordering, self-injection) — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired.html
- Spring Framework Reference — Using JSR-330 Standard Annotations (jakarta.inject.Provider) — https://docs.spring.io/spring-framework/reference/core/beans/standard-annotations.html
- Spring Framework Reference: Ahead of Time Optimizations (@Bean precise type) — https://docs.spring.io/spring-framework/reference/core/aot.html
- Spring Framework Reference: Ahead of Time Optimizations (Avoid Circular Dependencies) — https://docs.spring.io/spring-framework/reference/core/aot.html
- Spring Framework Reference: Autowiring with @Primary and @Fallback — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/annotation-config/autowired-primary.adoc
- Spring Framework Reference: Composing Java-based Configurations (@Bean bootstrap=BACKGROUND) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/java/composing-configuration-classes.adoc
- Spring Framework Reference: Dependencies and circular dependencies — https://docs.spring.io/spring-framework/reference/core/beans/dependencies/factory-collaborators.html
- Spring Framework Reference: Null-Safety (core/null-safety.adoc, v7.0.5) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/null-safety.adoc
- Spring Framework Reference: Programmatic Bean Registration — https://docs.spring.io/spring-framework/reference/core/beans/java/programmatic-bean-registration.html
- Spring Framework issue #29709: False positive of circular dependency in Spring AOT (BeanInstanceSupplier lacks early reference) — https://github.com/spring-projects/spring-framework/issues/29709
- Spring Framework issue #34349: continue pre-instantiation when current bean is in creation (lenient locking behavior) — https://github.com/spring-projects/spring-framework/issues/34349
- Spring Framework reference (7.0) — Background Initialization and concurrent startup (composing-configuration-classes) — https://github.com/spring-projects/spring-framework/blob/v7.0.0/framework-docs/modules/ROOT/pages/core/beans/java/composing-configuration-classes.adoc
- Spring Framework reference (7.0) — Bean lifecycle: init callbacks run under the singleton creation lock; SmartInitializingSingleton/ContextRefreshedEvent run outside it — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/factory-nature.adoc
- Spring Framework v7.0.5 AbstractAutowireCapableBeanFactory.java (allowCircularReferences, getEarlyBeanReference, doCreateBean exposure & wrapping check) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/support/AbstractAutowireCapableBeanFactory.java
- Spring Framework v7.0.5 DefaultSingletonBeanRegistry.java (three-level cache, getSingleton, locking) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/spring-beans/src/main/java/org/springframework/beans/factory/support/DefaultSingletonBeanRegistry.java
- Spring Framework v7.0.5 reference: Dependency Resolution Process / Circular dependencies — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/dependencies/factory-collaborators.adoc
- Spring Profiles - Baeldung — https://www.baeldung.com/spring-profiles
- Spring-specific index file for component candidate classes [SPR-11890] (issue #16509) — https://github.com/spring-projects/spring-framework/issues/16509
- SpringApplication (Spring Boot 4.x API javadoc) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/SpringApplication.html
- SpringApplication.java source (v4.1.0) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot/src/main/java/org/springframework/boot/SpringApplication.java
- SpringApplication.java source — ApplicationReadyEvent listener failure handling — https://github.com/spring-projects/spring-boot/blob/main/spring-boot/core/spring-boot/src/main/java/org/springframework/boot/SpringApplication.java
- SpringApplicationRunListener.java source (v4.1.0) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot/src/main/java/org/springframework/boot/SpringApplicationRunListener.java
- SpringBootApplication.java (Spring Boot v4.0.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.0.0/core/spring-boot-autoconfigure/src/main/java/org/springframework/boot/autoconfigure/SpringBootApplication.java
- SpringBootApplication.java (Spring Boot v4.1.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot-autoconfigure/src/main/java/org/springframework/boot/autoconfigure/SpringBootApplication.java
- SpringBootConfiguration.java (Spring Boot v4.0.0 source) — https://github.com/spring-projects/spring-boot/blob/v4.0.0/core/spring-boot/src/main/java/org/springframework/boot/SpringBootConfiguration.java
- SpringProperties (Spring Framework API) — checkFlag tri-state (null when unset) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/SpringProperties.html
- Standard Annotations: jakarta.inject.Provider / ObjectProvider for lazy access :: Spring Framework — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/standard-annotations.adoc
- StartupStep (Spring Framework 7.0.x API javadoc) — getName/getId/getParentId/tag/end — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/metrics/StartupStep.html
- StringUtils (Spring Framework 7.0.8 API) - uncapitalizeAsProperty — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/util/StringUtils.html
- Support constructor injection for FailureAnalyzers (issue #29811) — https://github.com/spring-projects/spring-boot/issues/29811
- Support final classes annotated with @Configuration(proxyBeanMethods=false) (Issue #22869) — https://github.com/spring-projects/spring-framework/issues/22869
- System Requirements (Spring Boot) — https://docs.spring.io/spring-boot/system-requirements.html
- System Requirements :: Spring Boot — https://docs.spring.io/spring-boot/system-requirements.html
- SystemEnvironmentPropertyMapper source, spring-boot main — https://github.com/spring-projects/spring-boot/blob/main/spring-boot/core/spring-boot/src/main/java/org/springframework/boot/context/properties/source/SystemEnvironmentPropertyMapper.java
- TaskExecutionProperties (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/autoconfigure/task/TaskExecutionProperties.html
- TestExecutionListener configuration — sorted via AnnotationAwareOrderComparator (Spring reference) — https://docs.spring.io/spring-framework/reference/testing/testcontext-framework/tel-config.html
- The BeanDefinitionOverrideException in Spring Boot (Baeldung) — https://www.baeldung.com/spring-boot-bean-definition-override-exception
- The BeanFactory API :: Spring Framework (reference) — https://docs.spring.io/spring-framework/reference/core/beans/beanfactory.html
- The BeanFactory API :: Spring Framework Reference (current/7.x) — https://docs.spring.io/spring-framework/reference/core/beans/beanfactory.html
- Thread race during bean instantiations starting with 6.2 due to lenient locks issue 35545 — https://github.com/spring-projects/spring-framework/issues/35545
- ThreadLocalAccessor javadoc — https://javadoc.io/doc/io.micrometer/context-propagation/latest/io/micrometer/context/ThreadLocalAccessor.html
- ThreadLocalAccessor source (io.micrometer.context) — https://raw.githubusercontent.com/micrometer-metrics/context-propagation/main/context-propagation/src/main/java/io/micrometer/context/ThreadLocalAccessor.java
- Transaction-bound Events — @TransactionalEventListener (Spring Framework Reference) — https://docs.spring.io/spring-framework/reference/data-access/transaction/event.html
- TransactionSynchronizationManager source (spring-projects/spring-framework, main) — https://github.com/spring-projects/spring-framework/blob/main/spring-tx/src/main/java/org/springframework/transaction/support/TransactionSynchronizationManager.java
- TransactionalEventListener.java source (spring-framework main) — fallbackExecution / phases — https://github.com/spring-projects/spring-framework/blob/main/spring-tx/src/main/java/org/springframework/transaction/event/TransactionalEventListener.java
- Type-safe Configuration Properties (Spring Boot 4.0 reference, external-config) — https://docs.spring.io/spring-boot/4.0-SNAPSHOT/reference/features/external-config.html
- Use @Configuration(proxyBeanMethods=false) wherever possible (spring-boot issue 9068) — https://github.com/spring-projects/spring-boot/issues/9068
- Use @Configuration(proxyBeanMethods=false) wherever possible — spring-boot issue #9068 (rationale, CGLIB cost) — https://github.com/spring-projects/spring-boot/issues/9068
- Using @Autowired (Spring Framework reference) — resolution behavior — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired.html
- Using @Autowired — ordering arrays/collections/maps, @Order vs @Priority, startup order caveat (Spring Framework reference) — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired.html
- Using @Qualifier (Custom qualifiers, CustomAutowireConfigurer, autowire-candidate) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/annotation-config/autowired-qualifiers.html
- Using Generics as Autowiring Qualifiers :: Spring Framework (7.0.5 docs source) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/annotation-config/generics-as-qualifiers.adoc
- Using JSR-330 Standard Annotations (jakarta.inject.Provider vs ObjectFactory/ObjectProvider) :: Spring Framework Reference — https://docs.spring.io/spring-framework/reference/core/beans/standard-annotations.html
- Using the @Bean annotation :: Spring Framework Reference (name/initMethod/destroyMethod inference/scope/parameters) — https://docs.spring.io/spring-framework/reference/core/beans/java/bean-annotation.html
- Using the @Configuration annotation (Spring Framework reference, configuration-annotation.adoc) — https://docs.spring.io/spring-framework/reference/core/beans/java/configuration-annotation.html
- Using the @Configuration annotation :: Spring Framework Reference (full mode, CGLIB, proxyBeanMethods, restrictions) — https://docs.spring.io/spring-framework/reference/core/beans/java/configuration-annotation.html
- Using the auto-proxy facility :: Spring Framework (reference, v7.0.5) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/aop-api/autoproxy.adoc
- VirtualThreadTaskExecutor (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/task/VirtualThreadTaskExecutor.html
- VirtualThreadTaskExecutor (Spring Framework 7.0.x API javadoc) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/core/task/VirtualThreadTaskExecutor.html
- WebApplicationType (Spring Boot 4.x API javadoc) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/WebApplicationType.html
- WebApplicationType.java source (v4.1.0) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/core/spring-boot/src/main/java/org/springframework/boot/WebApplicationType.java
- WebFluxWebApplicationTypeDeducer.java source (v4.1.0) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/module/spring-boot-webflux/src/main/java/org/springframework/boot/webflux/WebFluxWebApplicationTypeDeducer.java
- WebMvcWebApplicationTypeDeducer.java source (v4.1.0) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/module/spring-boot-webmvc/src/main/java/org/springframework/boot/webmvc/WebMvcWebApplicationTypeDeducer.java
- WebServerApplicationContext.java (spring-boot main) — GRACEFUL_SHUTDOWN_PHASE / START_STOP_LIFECYCLE_PHASE constants, since 4.0.0 — https://github.com/spring-projects/spring-boot/blob/main/module/spring-boot-web-server/src/main/java/org/springframework/boot/web/server/context/WebServerApplicationContext.java
- WebServerGracefulShutdownLifecycle.java (spring-boot main) — SMART_LIFECYCLE_PHASE deprecated 4.0.0, isPauseable()==false — https://github.com/spring-projects/spring-boot/blob/main/module/spring-boot-web-server/src/main/java/org/springframework/boot/web/server/context/WebServerGracefulShutdownLifecycle.java
- are @Order/Ordered and @Priority separate concepts? (spring-framework issue #31545) — https://github.com/spring-projects/spring-framework/issues/31545
- context-propagation README (micrometer-metrics/context-propagation) — https://github.com/micrometer-metrics/context-propagation/blob/main/README.md
- core/aot.adoc — Spring Framework v7.0.5 source (AOT engine overview, refresh for AOT, bean registration contributions, bean definition generation) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/aot.adoc
- external-config.adoc source (Spring Boot v4.1.0, GitHub) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/documentation/spring-boot-docs/src/docs/antora/modules/reference/pages/features/external-config.adoc
- external-config.adoc source (spring-boot v4.1.0 GitHub) — https://github.com/spring-projects/spring-boot/blob/v4.1.0/documentation/spring-boot-docs/src/docs/antora/modules/reference/pages/features/external-config.adoc
- org.springframework.boot.autoconfigure.condition package (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/4.0/api/java/org/springframework/boot/autoconfigure/condition/package-summary.html
- org.springframework.boot.bootstrap package (Spring Boot 4.x API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/bootstrap/package-summary.html
- org.springframework.boot.origin package (Spring Boot 4.0 API) — https://docs.spring.io/spring-boot/api/java/org/springframework/boot/origin/package-summary.html
- org.springframework.cglib package (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/cglib/package-summary.html
- org.springframework.context.index (Spring Framework 7.0.5 API) — CandidateComponentsIndex / indexer — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/index/package-summary.html
- org.springframework.context.index package (Spring Framework 7.0 API) — https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/context/index/package-summary.html
- spring-framework v7.0.5 — aot.adoc (Avoid Circular Dependencies via @Lazy / ObjectProvider) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/aot.adoc
- spring-framework v7.0.5 — autowired.adoc (collection ordering, @Order/@Priority, optional dependencies) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/annotation-config/autowired.adoc
- spring-framework v7.0.5 — classpath-scanning.adoc (@Lazy proxy 'rather limited', recommends ObjectProvider) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/aot.adoc
- spring-framework v7.0.5 — factory-scopes.adoc (Alternatives to Scoped Proxies) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/factory-scopes.adoc
- spring-framework v7.0.5 — standard-annotations.adoc (Provider / @Inject, ObjectProvider alternative) — https://github.com/spring-projects/spring-framework/blob/v7.0.5/framework-docs/modules/ROOT/pages/core/beans/standard-annotations.adoc
- spring.main.allow-bean-definition-overriding default false (Spring Boot additional configuration metadata) — https://github.com/spring-projects/spring-boot/blob/main/spring-boot/core/spring-boot/src/main/resources/META-INF/additional-spring-configuration-metadata.json
