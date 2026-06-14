# WASM Dependency-Modules in leaf — Where It Wins, Where It's a Trap

> **STATUS — DROPPED / not pursued (2026-06-14).** WASM was floated to ease autodiscovery / the "classpath" problem, but that is solved **natively** by link-time registration (`linkme`/`inventory`), so WASM was never actually needed for its original purpose. Its only *distinct* value (runtime-loaded, sandboxed, hot-reload, multi-language modules) is not a current goal. This document is retained as a record of the analysis — **not part of the active design.** leaf runs as pure native Rust. The one thing carried forward is the WASM-independent *origin-agnostic provider* discipline (which also serves test-doubles/mocks).
>
> An optional layer, not a foundation. This explores whether `leaf-wasm` — WASM isolation at *dependency/module* granularity — earns its keep. Short version: it's a real, narrow win for a specific class of problem, and a trap if you reach for it everywhere.
>
> **Correction (2026-06-14):** the async-status passages below were revised after checking primary sources. WASI 0.3 (the async ABI) is **ratified & stable** and shipping in Wasmtime — async-across-the-boundary is no longer "unsettled." The residual caveat is guest-side toolchain maturity, not the spec.

## The reframe that matters

The instinct when someone says "WASM in a DI container" is to imagine each bean compiled to WASM and the container wiring sandboxes together. That instinct is wrong, and it's worth saying why up front: a bean is the wrong unit. Beans are fine-grained, they hold references to each other, they call each other on hot paths, and they share state through the container. Putting a sandbox boundary between two beans means every cross-bean call becomes a serialize-copy-deserialize trip across a memory wall. You'd pay marshalling cost on your most frequent operations to isolate things that were never a security or fault concern. That is the per-bean trap, and it makes WASM look bad for reasons that have nothing to do with WASM.

The granularity that actually makes sense is the **dependency / module** — the same unit you already publish, version, and trust (or distrust) as a whole: a crate-shaped thing, a plugin, a third-party extension. At that boundary the calls are coarse (you invoke a *capability*, not a getter), the trust decision is natural ("do I trust this module's author?"), the version is a property of the whole unit, and the marshalling cost is amortized over real work rather than paid per field access. Everything good about WASM here — capability sandboxing, fault isolation, language independence, dynamic loading — lives at exactly this granularity. So the entire design hinges on the boundary being coarse and the unit being a whole dependency. Get that right and the rest of the analysis falls out cleanly.

## What WASM actually gives us today

A grounded reality check for a designer new to the ecosystem. The relevant world is the **WebAssembly Component Model** plus **WASI 0.2**, not raw core WASM modules. The distinction matters: core modules only speak in integers and floats and a shared linear-memory blob; the Component Model adds a real interface type system (records, variants, lists, strings, results, and crucially *resources* — opaque handles to host- or guest-owned objects) described in **WIT** (WASM Interface Types). This is what makes "present a module as something Rust can call with structs and traits" feasible at all.

- **Component model + WIT.** WIT defines the contract; `wit-bindgen` generates guest-side Rust bindings, and **Wasmtime**'s `bindgen!` macro generates host-side bindings. Resources let a module hand back a handle that the host treats as an opaque object with methods — this is the mechanism that lets a coarse `Arc<dyn Trait>` proxy work (more below). As of early 2026 the component model and WASI 0.2 are stable and shipping in Wasmtime; this part is no longer research-grade.
- **Resources are the good news and the catch.** They give you object identity across the boundary, but every method call still crosses the boundary, and every non-trivial argument/return value is *copied* (lowered/lifted) between linear memories. There is no shared-pointer aliasing across the wall, by design.
- **Performance, order of magnitude.** Wasmtime's compiled (Cranelift) execution of guest *compute* is typically within a small multiple of native — call it roughly 1.1x–2x for arithmetic-heavy code, sometimes better with optimization. That is *not* the number that matters for a DI boundary. The number that matters is **call + marshalling overhead**: a cross-boundary call is on the order of tens of nanoseconds of dispatch overhead *plus* copy cost proportional to argument size. For a coarse call doing real work, negligible. For a hot getter called millions of times, it's the whole cost. This single asymmetry is the entire green/trap divide.
- **Async and WASI 0.3.** *(Updated June 2026.)* This moved fast: **WASI 0.3.0 is ratified and stable**, making async native to the Component Model — `stream<T>`, `future<T>`, and `async` are now first-class at the Canonical ABI, so async flows *through* the WIT boundary by design (not a serialize-and-block workaround). Runtime support is shipping: Wasmtime 45 runs the release candidate and **Wasmtime 46 enables Component Model Async by default**. The remaining caveat is narrower than "unsettled": **guest-side toolchain maturity** — the per-language authoring story (Rust `wit-bindgen`, Go, jco, …) is still rolling out — plus production track record. So for an async-first host like leaf, native cross-boundary async is *designable now*; treat the residual risk as toolchain maturity, not spec instability. (A blocking-bridge fallback remains for the transition.)
- **Build DX.** Honest assessment: rougher than a normal crate. You target `wasm32-wasip2` (or `wasm32-wasip1` + adapter), run `wit-bindgen`, possibly `cargo-component` or `wac` to compose, and debug with weaker tooling (stack traces, profiling, and step-debugging across the boundary are all worse than native). It's tractable and improving fast, but a module author today feels the difference. Thin macros (see §4) can hide a lot of this, but not all.

Net: the *capability* primitives (component model, resources, WIT, capability-based WASI) **and the async ABI (WASI 0.3)** are ready and stable. The remaining soft spots are *guest-side toolchain maturity* for authoring (especially async) components and the *developer experience* — both improving fast.

## The map: green / situational / trap

| Lens / scenario | Verdict | Why |
|---|---|---|
| Untrusted / third-party modules (capability-sandboxed) | **GREEN** | Deny-by-default sandbox + explicit capability grants is exactly WASM's core competency. No native equivalent. |
| Fault isolation (a module panic/OOM/loop must not take down the host) | **GREEN** | A trapping component is contained; the host catches the trap, the rest of the app survives. Native crates can't offer this. |
| Dynamic, directory-scanned discovery (drop-in modules, no recompile) | **GREEN** | This is the genuinely *new* capability — a real runtime classpath (see §6). |
| Multi-language modules (a leaf app consuming a Go/C++/AssemblyScript module) | **GREEN** | WIT is the lingua franca; the host doesn't care what compiled the component. |
| Dependency-version isolation (module A wants `foo 1.x`, module B wants `foo 2.x`) | **GREEN** | Each component carries its own statically-linked world; no shared-crate version unification, no "one global `foo`". |
| Coarse-grained services with real work per call (a codec, a policy engine, a rules evaluator) | **SITUATIONAL** | Fine if calls are coarse and trust/fault/version is a real concern; otherwise a native crate is simpler. |
| Occasionally-reloaded config/strategy plugins | **SITUATIONAL** | Worth it if hot-reload or untrusted authorship matters; overkill otherwise. |
| Hot-path beans (called per-request, per-row, per-frame) | **TRAP** | Marshalling + boundary cost dominates; you pay isolation tax with no isolation benefit. |
| Fine-grained shared mutable state across the boundary | **TRAP** | No shared-pointer aliasing across linear memories; you'd serialize state constantly. |
| Rich async-across-the-boundary | **SITUATIONAL** | WASI 0.3 async is ratified/stable and shipping (Wasmtime 46 default); designable now. Residual caveat is guest-toolchain maturity, not the spec. |
| Tightly-coupled internal beans of the same app | **TRAP** | No trust boundary to enforce; pure overhead and worse DX. |

**Reading the table.** The GREEN rows share one trait: there is a *real* boundary in the problem domain (trust, fault, language, version, or load-time identity) that native Rust linking genuinely cannot express. WASM isn't winning on performance there — it's winning because it's the *only* tool that draws that line. The TRAP rows share the opposite trait: there is no real boundary, so the WASM wall is pure cost — marshalling on hot paths, serialization of shared state, and worse debugging. The SITUATIONAL rows are where you must ask: "is the boundary real, or am I sandboxing my own trusted code?" If it's your own trusted code on a hot path, it's a trap wearing a green hat.

## A concrete shape for an optional leaf-wasm layer

**Granularity.** One WASM component = one *module* = a coarse, independently-published, independently-trusted unit that exposes one or a few **service interfaces**. Never a single bean; think "this component provides the `PaymentRiskScorer` capability."

**The host↔module contract.** Define service interfaces as **coarse WIT worlds** — methods that do meaningful work, returning `result<_, error>` (so a guest-side failure surfaces as a Rust `Result`, and a trap is caught by the host as a fault). On the host side, a thin generated **host-proxy** wraps the Wasmtime-instantiated component and implements the corresponding native trait, so the rest of the container only ever sees an ordinary `Arc<dyn Trait>`:

```
WIT world  ──wit-bindgen──▶  guest impl (the module author's code)
   │
   └──wasmtime bindgen!──▶  host bindings ──▶  generated host-proxy ──▶ Arc<dyn PaymentRiskScorer>
```

The container does not know or care that the object behind the `Arc<dyn Trait>` is a WASM component. It's just another provider. This is the load-bearing design move: **WASM-ness stops at the proxy.** The proxy owns the `Store`, holds the component instance (using a component *resource* handle if the module returns a stateful object), and translates each trait-method call into a lifted/lowered WIT call. Because the boundary is coarse, the per-call lowering cost is amortized.

**Discovery.** Bounded and explicit. The layer scans a single, configured location (e.g. `./modules/*.wasm` or a path from config) at startup — no network, no implicit search path, no recursive walking of the filesystem. Each discovered component is inspected for the WIT worlds it exports; those become candidate providers registered into the container alongside native beans. Discovery is *additive and observable*: the app can log exactly which modules were found and what they provide. (Static native providers remain the default; directory scanning is the opt-in dynamic path — see §6.)

**Capability grants.** Deny-by-default, the WASM way. A module gets *nothing* — no filesystem, no clock, no network, no env — unless the host explicitly grants it via the WASI capability model (`wasmtime-wasi` preopened dirs, configured `WasiCtx`) and via host-exported WIT interfaces. Grants are declared per-module in leaf config, so the operator decides "this risk-scorer may read `./rules`, nothing else." This is precisely the GREEN-row value, made concrete.

**Container / lifecycle / async integration.** The proxy participates in the normal lifecycle: instantiation maps to component instantiation (cold-start cost is real — pool or pre-instantiate hot modules), and shutdown drops the `Store`. For async: with WASI 0.3 (ratified; default in Wasmtime 46) the host drives **native cross-boundary async** on its existing executor, so async calls flow through the WIT boundary rather than being block-bridged. A blocking-bridge (component call on a dedicated pool) stays as a fallback for guest modules whose toolchain doesn't yet emit 0.3 async. The thing to track is guest-toolchain maturity, not the spec.

**Thin authoring macros.** To make writing a module feel close to a normal crate, provide a `#[leaf::module]` proc-macro on the guest side that (a) emits the WIT-derived trait impl boilerplate from an annotated Rust trait, (b) wires `wit-bindgen`'s generated glue, and (c) registers exports. The goal: the author writes something that looks like a normal `impl PaymentRiskScorer for MyScorer`, plus a target triple and a build step, and the macro hides the binding ceremony. This narrows — but does not erase — the DX gap from §2.

## Explicit non-goals

- **Not the core substrate.** leaf's container, wiring, and abstractions stay native Rust. `leaf-wasm` is a *peripheral, optional* layer that plugs into the existing container. If it ever becomes mandatory or foundational, the design has failed.
- **Not per-bean isolation.** The boundary is the *module*. Anyone reaching for "let me sandbox these two beans from each other" is in the trap and should be redirected.
- **Not a way to run leaf's own abstractions in WASM.** The container, the wiring macros, leaf-the-framework do not get compiled to WASM. WASM is for *guest modules that leaf consumes*, not for leaf itself.
- **Not a hot-path mechanism.** If something is called at high frequency or shares fine-grained mutable state, it is a native bean. Full stop.
- **Not a polyglot fantasy where everything is a component.** Multi-language is a GREEN *capability* for specific modules, not a default architecture.

## Verdict on "build our own classpath"

The owner's intuition is correct, and it splits cleanly in two:

- **The static case already has a classpath-equivalent.** leaf's native link-time provider registry *is* the classpath for everything compiled into the binary — known at build time, resolved by the container, zero runtime discovery cost. You do not need WASM for this, and you should not build a second mechanism for it. This is the common case and it's already solved.
- **The dynamic case is where WASM adds something genuinely new.** Native Rust cannot load a provider that wasn't compiled into the binary. WASM directory-scanning gives leaf a *real additional dynamic classpath*: drop a `.wasm` into the configured location, and a new provider appears in the container — no recompile, sandboxed, fault-isolated, version-isolated, possibly written in another language. That is not a reimplementation of the static registry; it is the **loadable-module classpath** that the static registry structurally can't be.

So: don't build "your own classpath" as a replacement for native linking. Build the *dynamic* classpath that native linking can't provide, and let it sit beside the static one. That, and only that, is `leaf-wasm`'s sweet spot.

## Open questions for Phase 2

- **Cold-start and pooling.** What's the measured instantiation cost per component, and do we need an instance pool / pre-instantiation for modules on warm paths?
- **Versioning the contract.** How do we evolve a WIT world without breaking already-deployed modules? Do we adopt semver-on-WIT conventions and refuse incompatible components at discovery?
- **Async seam.** With WASI 0.3 async ratified and default in Wasmtime 46, how do we drive native cross-boundary async from leaf's executor, and which guest toolchains are mature enough to author 0.3-async components yet? Where do we still need the blocking-bridge fallback during the transition?
- **Capability UX.** What does the per-module grant declaration look like in leaf config, and how do we make "this module asked for the network" visible/auditable to the operator?
- **Resource lifetime.** When a module returns a stateful resource handle behind an `Arc<dyn Trait>`, who owns teardown, and how does it interact with container shutdown ordering?
- **Failure semantics.** When a module traps mid-call, what does the host-proxy surface to callers — a typed `Err`, a panic, a circuit-breaker? Is a trapped module quarantined or reloaded?
- **Trust establishment.** Signing / provenance for third-party modules before we grant capabilities — in scope for Phase 2 or later?
- **Benchmark the divide.** Pick one representative GREEN scenario and one representative TRAP scenario and *measure* the marshalling cost, so the green/trap table is backed by leaf's own numbers rather than ecosystem estimates.

## Sources

- **WebAssembly Component Model** — design and MVP (resources, worlds, composition): the `WebAssembly/component-model` specification.
- **WIT (WASM Interface Types)** — the IDL for component interfaces; record/variant/list/result/resource types.
- **Wasmtime** — host runtime; `bindgen!` for host bindings, component instantiation, `Store`, Cranelift compilation; the source for the "within a small multiple of native compute" claim and instantiation-cost characterization (Bytecode Alliance docs and Wasmtime book).
- **wit-bindgen** — guest-side Rust binding generation.
- **cargo-component / wac** — building and composing components (build DX).
- **WASI 0.2** — stable capability-based system interface (preopens, `WasiCtx`); deny-by-default capabilities.
- **WASI 0.3 / async component model** — **ratified & stable (June 2026)**; async native to the Component Model (`stream<T>`/`future<T>`/`async` at the Canonical ABI), shipping in Wasmtime (45 = RC, 46 = enabled by default). Sources: [Bytecode Alliance — WASI 0.3 Launched](https://bytecodealliance.org/articles/WASI-0.3), [WASI v0.3.0 release](https://github.com/WebAssembly/WASI/releases/tag/v0.3.0), [wasi.dev/roadmap](https://wasi.dev/roadmap). Residual caveat: guest-toolchain maturity.
- **wasm32-wasip2 target** — the Rust target underpinning component authoring.

> Maturity caveat restated for the designers: the capability primitives (component model, resources, WIT, capability-based WASI 0.2) **and the async ABI (WASI 0.3 — ratified & stable; default in Wasmtime 46)** are all stable enough to design against today. The remaining moving ground is **guest-side toolchain maturity** (per-language authoring, especially async) and **end-to-end developer experience** — keep those as seams, but the async *spec/ABI* is no longer a risk.
