# Cross-Crate Composition & Tooling in Rust — A Reference for leaf

> **Purpose.** Ground every later decision about cross-crate auto-discovery in how Rust and Cargo *actually* compile and link. leaf is a Spring-Boot-class IoC/DI framework (async-first, std) whose **core requirement** is Spring-like behaviour: components, beans, config, conditions, and auto-configurations authored in *arbitrary downstream and sibling crates* are auto-discovered and wired together — the analogue of classpath component scanning + `ServiceLoader` + auto-configuration. Rust has no global classpath and no whole-program runtime reflection, so this must be **synthesized from compile/link-time mechanisms**. This document lays out the real machinery, the failure modes, and the design space. It deliberately does **not** pick a winner.
>
> **Invariants this reference holds every option against:** (1) DX is priority #1; (2) low overhead; (3) stable-by-default (nightly only when markedly better); (4) macros stay thin (emit only hand-writable code); (5) avoid global locks; (6) reliability across platforms; (7) a workspace of dozens of crates (kernel + features + macros/codegen + optional pluggable integration crates).

---

## The global-classpath gap

### What the JVM gives Spring (and Rust does not)

The JVM hands a framework a flat, mutable, runtime-introspectable **classpath**. Every class is loaded lazily by string name; `ClassLoader`s enumerate JARs; `ServiceLoader` reads `META-INF/services` text files; and Spring's `ClassPathScanningCandidateComponentProvider` walks bytecode annotations at startup. "Discover all contributions of kind X across the whole program" is therefore a **runtime query against a global registry**.

Rust has none of this:

- **No global classpath.** There is no enumerable, program-wide list of "everything linked in."
- **No by-name runtime reflection / RTTI.** You cannot `Class.forName(...)` a type, nor enumerate a program's types at runtime.
- **Monomorphization erases generic identity** at compile time — a generic `Foo<T>` has no single runtime identity to register.
- **Crates are statically linked** as `rlib` archives into one binary at compile/link time (unless `-C prefer-dynamic`).

The decisive consequence: **"which crates exist and what do they contribute" must be answered at compile or link time, not at runtime** — and the **final binary crate is the only point in the system that sees the entire crate graph**. (Per the Rust Reference on linkage: an `rlib` is a static Rust library archive interpreted by rustc in later linkage; executables statically link rlibs by default; "a major goal of the compiler is to ensure that a library never appears more than once in any artifact.")

### The substitute design space

There is **no single Rust mechanism equal to the JVM classpath.** Instead there is a design space of partial substitutes, each with a different cost / reliability / portability profile:

1. **Link-time distributed slices (`linkme`).** Collect `const` static elements into a custom linker section; reconstruct an `&'static [T]` at runtime from linker-provided start/stop symbols. No life-before-main, zero runtime collection cost — but vulnerable to dead-code elimination (DCE) and `--gc-sections`, and to cross-crate reachability problems.
2. **Life-before-main registration (`inventory`).** Run global constructors before `main` to push elements onto a global registry; supports non-`const` (runtime-computed) values. More flexible, but adds pre-main safety/ordering hazards and weaker wasm/embedded support, and shares the DCE survival problem. **(Correction: current `inventory` is *not* built on the `ctor` crate — see the link-time section.)**
3. **`build.rs` codegen.** Generate Rust at build time into `OUT_DIR`, `include!`'d into the same crate. Fully static, no DCE/pre-main issues — but a build script can only see *its own* crate (no build-time classpath either).
4. **Explicit / manual registration (bevy `add_plugins`).** A human (or generated prelude) writes the wiring. Zero magic, maximum reliability and portability — but not "auto."
5. **Dynamic loading (`dlopen` / `libloading` / `abi_stable`).** True runtime plugin loading, but unstable native ABI (forcing C-ABI/`abi_stable` discipline) and you must still know what to load. Orthogonal to compile-time wiring.

**The deep truth for leaf:** both **rustc reachability** *and* the **system linker** want to *delete* registrations that nothing references. Any auto-discovery scheme must defeat two independent layers of elimination, and a registering crate that the binary never otherwise touches **can silently vanish, with no error**.

**A warning worth heeding:** Jana Dönszelmann, author of the experimental nightly `#[global_registration]` feature, concluded after building it that truly *implicit, automatic* cross-crate global registration is something we likely should **not** want — on **predictability/surprise** grounds (a transitive dependency silently injecting routes/beans/config), with a passing note that it could even be done maliciously. *(Correction: the blog frames this as predictability/surprises, not as a "security footgun"; the author still endorses **explicit, opt-in** cross-crate sharing. The tracking issue `rust-lang/rust#125119` contains no such discussion — that conclusion is from the donsz.nl blog only.)* This is a direct argument for leaf to **scope** auto-discovery (à la Spring Boot's `@ComponentScan basePackages` + auto-configuration exclusion lists), not to make it truly global.

**Leaf implications.** Treat discovery as a **layered strategy**: a link-time/registration substrate for the common case **plus** an explicit escape hatch that always works on every target. Model contributions as concrete `const` descriptors and move *all* conditional/feature/profile logic into **runtime evaluation** over the collected descriptors inside the runtime — not into pre-main code and not into the linker.

---

## `build.rs` mechanics & cross-crate limits

### Mechanics

A Cargo build script is an ordinary **host-target executable** that Cargo compiles and runs **immediately before** its own crate is compiled (cwd = the build-script package root). Its inputs:

- **Its own package's files** (with `cargo::rerun-if-changed=PATH`).
- **`[build-dependencies]`.** *(Correction: a build script is a real executable linked against its build-dependencies, so it absolutely **can** use the Rust items, types, and source of crates declared under `[build-dependencies]` — e.g. `cc`, `bindgen`, `prost-build`, `syn`/`quote`. What it cannot see are crates under `[dependencies]`/`[dev-dependencies]`, which are "not built yet.")*
- **Cargo-provided env vars** — a larger set than often listed: `OUT_DIR`, `TARGET`, `HOST`, `OPT_LEVEL`, `PROFILE`, `DEBUG`, `NUM_JOBS`, `CARGO`, `CARGO_MANIFEST_DIR`, `CARGO_MANIFEST_PATH`, `CARGO_MANIFEST_LINKS`, `CARGO_CFG_*` (target config — read these, **not** `cfg!`/`#[cfg]`, which reflect the **host**), `CARGO_FEATURE_<NAME>` (its own activated features), `CARGO_PKG_*`, `RUSTC`/`RUSTDOC`/`RUSTC_WRAPPER`, `CARGO_ENCODED_RUSTFLAGS`, and `DEP_<LINKS>_<KEY>` from `links`-dependencies.

**Codegen pattern.** The sanctioned way to produce Rust is to write into `OUT_DIR` and splice with `include!(concat!(env!("OUT_DIR"), "/gen.rs"))`. The Book says scripts *should* not modify files outside `OUT_DIR` — *(Correction: this is a **SHOULD**, enforced only at packaging/publish time for registry immutability, not a hard **MUST** in local/workspace builds.)* `OUT_DIR` is intentionally **not cleaned** between builds, so codegen must be deterministic/idempotent.

**Compilation levers (the `cargo::` stdout directives).** The recognized directives include: `rustc-cfg`, `rustc-check-cfg` (Rust 1.80+), `rustc-env`, `rustc-link-lib`, `rustc-link-search`, the full `rustc-link-arg` family (`rustc-link-arg`, `-arg-bin`, `-arg-bins`, `-arg-tests`, `-arg-examples`, `-arg-benches`, **`-arg-cdylib`**, plus the legacy alias `rustc-cdylib-link-arg`), `rustc-flags` (only `-l`/`-L`), `metadata`, `warning`, `error` (Rust 1.84+), `rerun-if-changed`, `rerun-if-env-changed`. *(Corrections: the list is **not** exhaustively the ones often quoted — `rustc-cdylib-link-arg` exists; and the framing "only `cargo::`-prefixed" is imprecise — the **legacy single-colon `cargo:KEY=VALUE`** form is still accepted for MSRV < 1.77, and any unrecognized single-colon key is treated as metadata.)* Directive **order** can affect the order of args Cargo passes to rustc and thence to the linker — relevant when ordering link args.

**The `links` / `DEP_` channel.** `package.links` declares the crate links a native library and enables the only structured *build-script-to-build-script* data channel: a build script emits `cargo::metadata=KEY=VALUE`, and **immediate** dependents receive it as `DEP_<LINKS>_<KEY>`. Critical details:

- The prefix is the **value of the `links` key** (uppercased), *not* the package name. *(Correction.)*
- Metadata is passed **only to immediate dependents, not transitively.** *(Correction — a frequently-omitted, load-bearing restriction.)*
- The `links` value is **globally unique** graph-wide ("forbidden to have two packages link to the same native library"). *(Correction: the `-sys` convention is **not** the only sanctioned pattern — config-based "Overriding Build Scripts" via `[target.<triple>.<links-value>]` is also documented; and even `-sys` does not let two packages share a `links` value, it provides a single canonical owner.)*

### The hard wall: directionality

Information in Cargo flows **strictly downward** (dependency → dependent). The quote (Josh Triplett): *"a crate deeper in the dependency tree can't get information about a crate higher in the dependency tree, except what the higher crate passes down (such as features)."*

- A leaf **kernel** crate's `build.rs` **cannot** scan, enumerate, or learn the identity of the feature/auto-config crates that depend on it.
- *(Correction / nuance: "cannot react to dependents" is too strong. Because Cargo **unifies the union of features** across all dependents into one compiled copy, a build.rs's `CARGO_FEATURE_*` inputs **do** reflect — and can react to — features that dependents turned on, **anonymously**. The accurate statement: a build.rs cannot learn the **identity** of, or receive **targeted data** from, crates that depend on it; it can react to the unioned feature set those dependents demand.)*

### Why `build.rs` is not the discovery engine

*(Correction to the "trivial local shims" framing.)* The crates that actually perform cross-crate registration (`linkme`, `inventory`) **do not use `build.rs` for the registration codegen at all** — `linkme` emits `#[link_section]` statics via **proc-macro** and the **linker** aggregates them; `inventory` emits life-before-main constructors via **proc-macro**. So `build.rs` is essentially *not the vehicle* for cross-crate registration. What `build.rs` *can* do for leaf:

- **Per-crate codegen** that emits a crate's own local registration code (e.g. aggregating that crate's own known contributions, sqlx-style).
- **Emit linker/cfg directives** (`cargo::rustc-link-arg`, platform `cfg`) to protect link sections from `--gc-sections` on the **final binary**, and to gate platform-specific paths via `CARGO_CFG_TARGET_*`.
- A **deterministic `cargo metadata`-driven codegen path** at the **root/binary crate** that enumerates the integration crates the application opted into, and emits explicit registration/force-link references. This is the sqlx-style "build-time analysis → checked-in artifact → compile consumes it" pattern and is the **portable fallback** for wasm/embedded/staticlib.

**Leaf implications.** Do **not** design leaf around `cargo::metadata`/`DEP_` as an aggregation bus (one `links` value graph-wide; immediate-dependents-only). Anchor whole-graph awareness at the **final binary** (link time). Use `build.rs` only as a per-crate generator and as a place to emit anti-DCE linker args / gate platforms.

---

## Proc-macro mechanics & cross-crate limits

### Mechanics

A procedural macro is a compiler-plugin function that runs **during compilation of the invoking crate**, transforming only the `TokenStream` of that single invocation site. Three kinds: function-like `custom!(...)`, `#[derive(X)]` (may only *add* code), `#[attr]` (may *replace* the annotated item).

Hard properties from the Rust Reference:

- **Single-invocation scope.** A macro sees only the tokens of its own invocation. There is **no API** to enumerate other items, other crates, or a filesystem "classpath."
- **Unhygienic output.** Output is inserted "as if the output token stream was simply written inline." Therefore emitted code **must use fully-qualified absolute paths** (`::leaf_core::__registry::...`, `::core::option::Option`) and uniquely-prefixed generated idents.
- **`proc-macro = true` crate isolation.** Such a crate is built for the compiler's **host** target, "may not be used from the crate where they are defined," and can export **only** macros. So leaf's macro crate **cannot** also host the runtime registry/API; that must live in a separate normal kernel crate that both the emitted code and the runtime depend on.
- **No reliable global state across invocations.** rustc may reuse one proc-macro process per compilation, but invocation order/reuse is unspecified and unstable across rustc versions, incremental builds, and parallel codegen. **Never** accumulate a registry in a proc-macro `static`/`thread_local`.

### The consequence

A macro **alone cannot build a cross-crate registry.** It can only emit code at each call site. The cross-crate collection must be done by a **link-time mechanism** (`linkme` sections or `inventory` constructors) over the crates actually linked into the final binary. The macro's job is to emit **one hand-writable artifact** — e.g. a `#[distributed_slice(leaf_core::COMPONENTS)] static __LEAF_x: Descriptor = Descriptor { ... };` — and nothing more.

**Proc-macro hygiene gotchas for leaf:**
- The emitted registration static must have a unique, non-colliding symbol; reference the slice by an absolutely-resolvable path (`$crate`/fully-qualified), not assumed imports; and be both `#[used]` *and* reachable (a private item in an unreachable module recreates the DCE failure mode).
- Under resolver v1, proc-macro/build-dep features unify with the same crate used as a normal dependency, leaking build-time-only features into the runtime set — keep leaf's macro crates on resolver 2+ (see Cargo features section).

**Leaf implications.** Split crates strictly: **`leaf-macros`** (`proc-macro = true`, emits only thin code via absolute paths) + **`leaf-core`** (normal crate hosting the public registration API and the distributed-slice/inventory registries). Macros hold zero logic. Keep the registration ABI in `leaf-core` minimal and ultra-stable (a small `Descriptor` of fn pointers + metadata), since emitted code hard-codes paths to it across the whole workspace. **Hard-error** in the macros on generic targets and (for the `linkme` path) non-`const`-constructible descriptors, turning silent no-ops into clear diagnostics.

---

## Link-time registration — the workhorse (the deepest section)

`inventory` and `linkme` (both by David Tolnay) are the two production crates that synthesize "collect contributions from every linked crate into one iterable" — the Rust analogue of classpath scanning / `ServiceLoader`. They solve the same problem with **fundamentally different mechanisms**, and the difference dictates leaf's design.

### `linkme` — `#[distributed_slice]` (link-section collection, no life-before-main)

`#[distributed_slice] static FOO: [T] = [..]` declares a slice; each `#[distributed_slice(FOO)]` element is a separate `static` placed (via `#[link_section]` + `#[used]`) into a named section. The linker concatenates same-named input sections across **all** linked rlibs/objects and auto-defines boundary symbols (`__start_<section>`/`__stop_<section>` on ELF; `section$start`/`section$end` on Mach-O; `$a`/`$b`/`$c` bracketing on Windows). `linkme` materializes `&'static [T]` from `(start, (stop − start) / size_of::<T>())`.

- Docs verbatim: *"It does not involve life-before-main or any other runtime initialization on any platform. This is a zero-cost safe abstraction that operates entirely during compilation and linking."*
- **Element initializers must be `const` expressions.** The slice is fully built before `main` — good for synchronous wiring at startup.
- Per-platform section names (from `impl/src/linker.rs`): Linux/BSD `linkme_<ident>`; macOS `__DATA,__linkme<hash>,regular,no_dead_strip`; Windows `.linkme_<ident>$b`; illumos `set_linkme_<ident>`.
- Officially tested: Linux, macOS, Windows, FreeBSD, OpenBSD, illumos. **Not** general wasm/embedded.
- **Type-safety fix:** `linkme < 0.3.24` allowed coercion-based type confusion (RUSTSEC-2024-0407). **Pin `linkme >= 0.3.24`.**

### `inventory` — life-before-main constructors + lock-free linked list

`inventory::collect!(T)` defines a registry (must live in the type-defining crate, i.e. `leaf-core`); `inventory::submit! { v }` in **any** downstream/sibling crate emits a static function pointer placed in the platform constructor section that, at startup, atomically pushes a `&'static Node` onto `Registry { head: AtomicPtr<Node> }`. `inventory::iter::<T>()` walks it.

- **Mechanism (corrected):** *current* `inventory` (v0.3.24) **implements the constructor machinery itself** via `#[link_section]` + `#[used]` macros (`.init_array` on Linux/Android/BSD/Fuchsia; `__DATA,__mod_init_func` on macOS/iOS; `.CRT$XCU` on Windows; `__wasm_call_ctors` on wasm). It is **NOT built on the `ctor` crate** — it dropped that dependency in the ~0.2 rewrite (~2021); its only non-dev dep is `rustversion` (wasm-target-only). The stale "built on ctor" wording survives only in **typetag's** README. So: **typetag uses `inventory` (which today implements life-before-main itself) + `erased-serde`**, to register `Box<dyn Trait>`/`&dyn Trait` impls "across the dependency graph of the final program binary."
- Because a constructor **runs code**, the submitted value **need not be `const`** (key advantage over `linkme`).
- **Ordering is unspecified.** Docs verbatim: *"There is no guarantee about the order that plugins of the same type are visited by the iterator. They may be visited in any order."*
- Broader nominal platform support including wasm — but on wasm the embedder must actually **call** `__wasm_call_ctors`, and an `AtomicBool` guards against the linker invoking it more than once (which would make the list circular). On unsupported targets you silently *"find that no plugins have been registered."*

### The `#[used]` survival knob

- `#[used]` only forces the **compiler** to keep the symbol in the `.o`/`.rlib` — the Rust Reference is explicit: *"the linker is still free to remove such an item."*
- `#[used(compiler)]` → LLVM `llvm.compiler.used` (survives LTO; **not** `--gc-sections`). `#[used(linker)]` → LLVM `llvm.used` → `SHF_GNU_RETAIN` on ELF (survives `--gc-sections`); on COFF/Mach-O plain `#[used]` already prevents section-GC removal.
- **Default change:** `rust-lang/rust#140872` ("Make `#[used(linker)]` the default on ELF too") merged 2025-06-06, shipping in **Rust 1.89.0**, so on supporting ELF toolchains plain `#[used]` now resists `--gc-sections`. *(Caveat: `#[used(linker)]` is still platform-dependent — `rust-lang/rust#145362` shows it does **not** prevent section removal on `x86_64-pc-windows-gnu`.)*

### The dominant failure mode — DEAD-CODE ELIMINATION, at TWO distinct layers

This is the single biggest reliability risk for a Spring-style auto-discovery framework, and the survey's own framing needs **correction on which issue maps to which layer.**

**Layer A — rustc reachability / codegen-units (the historically dominant one).** rustc could drop an entire module's (or crate's) codegen — taking the registration with it — if nothing else in the program referenced it. This is `rust-lang/rust#47384` ("`no_mangle`/`used` static is only present in output when in reachable module").
- *(Correction:* `linkme#36` ("Distributed slice members in dependency crates are discarded") and `rust-lang/rust#67209` ("`#[link_section]` is only usable from the root crate") are **this layer, not the linker `--gc-sections` layer.** `linkme#36` was closed as a duplicate of `#31`, which dtolnay diagnosed as exactly `#47384`. In `#67209`, the reporter showed with `readelf` that `ld --gc-sections` *correctly retained* the section from both a raw `.o` and an `.a`; rustc simply was not emitting/exporting the symbol across the rlib boundary. So **all three issues evidence the rustc reachability layer.**)*
- **Status:** `#67209` was effectively fixed "by accident" in **rustc 1.62** (and `linkme 0.3.0`); it stays open only for a regression test. Workarounds in the era: `codegen-units = 1`, or referencing some symbol from the same module/crate.

**Layer B — system-linker `--gc-sections` / encapsulation-symbol GC (the genuinely linker-side one).** ELF `__start_/__stop_` encapsulation symbols were designed for *metadata*, so linkers differ: GNU `ld` defaults to conservative `-z nostart-stop-gc` (keeps element sections if the slice symbol is referenced — ideal for `linkme`); **LLD 13+ defaults to eager `-z start-stop-gc`**, which drops element sections because only the boundary symbols reference them — **emptying `linkme` slices.** This is `linkme#49`, fixed by making `#[used(linker)]`/`SHF_GNU_RETAIN` the ELF default in Rust 1.89.
- *(Correction: `-z nostart-stop-gc` fixes **this** (`#49`) case — **not** `#67209`/`#36`, whose MWE already passed `--gc-sections` and failed in rustc.)*
- **Same OS, different linker → different result.** Code that works under GNU `ld` can silently break under `ld.lld` on the same machine. **Linker choice is a correctness variable.**
- **Worst case: `staticlib` into C/C++** (Chromium/Android). The foreign build's `--gc-sections`/`-dead_strip` strips everything; even `-C link-dead-code` did not fix it for Chromium — only `used_linker`/`SHF_GNU_RETAIN` did.
- **macOS** has a *separate* tail: `linkme#61` (distributed slice empty from an external crate on macOS **without LTO**).

**Layer 0 (more fundamental than either) — the crate must actually be *linked*.** Cargo/rustc only link an rlib into the final binary if **something path-references it**. A dependency declared in `Cargo.toml` but never `use`d may be omitted **entirely**, so its sections/constructors never exist. dtolnay confirms in `inventory#7` that `submit!` *"doesn't work unless code from the downstream crate is actually used,"* with the canonical fix `use somecrate as _;`. **This is intended Rust behaviour, not a bug — there is no true classpath.** A "registration-only" feature crate the binary never otherwise references contributes **nothing**.

**Other hazards.**
- **Silent failure is the worst trait.** A dropped registration yields an **empty iterable**, never a compile or link error — and it can pass in debug but fail in release, or vary by linker/platform. *(Non-deterministic DCE has even been observed: `rust-lang/rust#150462`.)* leaf **must** add a self-check (expected-count assertion, or a build-time manifest cross-check) or users will ship binaries silently missing components.
- **Generics cannot be registered.** Both mechanisms need a concrete, monomorphized `'static` value with a fixed symbol/address. A generic `Component<T>` has no instantiation for the linker/constructor to point at, and rustc won't monomorphize an unused generic. typetag has the same limitation. You must register **each concrete instantiation explicitly**.
- **Ordering is unspecified** for both (link/section order for `linkme`; constructor order for `inventory`). The Global Registration pre-RFC even proposes deliberate randomization (`-Z layout-seed`). Never rely on registration order.
- **Life-before-main is hostile to async and safety.** `inventory` constructors run before `main`, on an unknown thread, with std possibly not fully initialized; they must not panic, cannot run async, and have unspecified order. Register **descriptors/factories, not live objects**; defer all real init/IO/async to the runtime.

### Performance

`linkme` = **zero runtime cost** (contiguous static region, O(1) access, built at link time). `inventory` pays a life-before-main cost: each `submit!` runs a constructor doing one lock-free atomic push at process start — negligible for thousands, but non-zero and pre-`main`.

### Future direction

An internals pre-RFC + `rust-lang/rust#125119` propose a compiler-native `#[global_registration]` that builds the collection at compile time (like `#[test]`), exposing an opaque `IntoIterator` (no `len()`/order), working on **all** platforms incl. no_std, avoiding life-before-main. dtolnay endorses the `linkme`/distributed-slice *shape* as the right API. **Status: unstable, cross-crate semantics unresolved — do not block on it.** Design leaf's descriptor format so it *could* migrate later.

**Leaf implications.**
- **Default to `linkme`, not `inventory`, for the core registry:** zero-cost, no pre-main hazards (compatible with async-first init), thin/data-only macros, no global locks. Reserve `inventory` for cases needing a **runtime-computed (non-`const`)** registered value or trait-object self-registration (typetag pattern).
- Register **thin `const` descriptors**, not components: e.g. `&'static ComponentDescriptor { type_id, name, fn make(&Container) -> BoxFuture<Box<dyn Any>>, dependencies: &[TypeId], conditions, priority }`. The fn pointer is `const`-constructible, erases generics to a uniform signature, and lets the container do async construction/wiring at runtime from a flat descriptor table.
- **Solve Layer 0 deliberately:** require the application/binary to depend on and **reference** each contributing crate (a generated or hand-written `enable!(crate_a, crate_b)` prelude that emits `use crate_x as _;`), forcing the archive member in.
- **Toolchain guardrails:** require Rust **≥ 1.89** for the ELF `#[used(linker)]` default, or inject `-Clink-arg=-Wl,-z,nostart-stop-gc` for ELF in a recommended `.cargo/config.toml`; document the LLD-vs-GNU-ld split; pin `linkme >= 0.3.24`.
- **Do all ordering at runtime** via a topological sort over descriptor dependencies/priority — never link/section/constructor order. This naturally supports `@DependsOn`, conditional beans, and auto-config ordering.
- **Scope platform claims honestly:** solid on Linux/macOS/Windows/BSD server targets; treat wasm and bare-metal as a separate, caveated path (explicit registration or build-time codegen). Keep the runtime API identical across paths so the substrate is swappable.

---

## Cargo features, resolver & conditional assembly

Cargo features are the most natural Rust analogue of Spring's "the classpath decides the wiring": enabling a feature / adding an optional dependency is how a user opts a contribution into the build graph — the analogue of adding a Spring Boot starter. But features interact with link-time discovery in two dangerous ways.

### Feature unification (the propagation channel **and** the silent-activation trap)

*(Corrected, resolver-aware framing.)* Cargo resolves a dependency with the **union** of all features any consumer enables *for a given shared copy* — but this is **not** unconditionally "anywhere in the graph for one whole shared build":

- **Resolver v1** (pre-2021 editions / no `resolver` key) unifies host/target and build/normal deps together — closest to "anywhere."
- **Resolver v2** (default for edition 2021) deliberately **avoids** unifying in three cases: (1) target/platform-specific deps for a target not being built; (2) features on **build-dependencies/proc-macros** vs the same crate as a normal dependency; (3) **dev-dependencies** vs normal builds unless that dev target is being built. Because of (1)/(2), the *same crate can be compiled multiple times with different feature sets* — so there is not always a single "whole shared build."
- **Resolver v3** (edition 2024, Rust 1.84+) is **MSRV-aware version selection** (`incompatible-rust-version = fallback`); it does **not** change feature-unification semantics vs v2.
- **The resolver version is read only from the top-level package or `[workspace]` table** and ignored on members — a virtual workspace that forgets `[workspace] resolver = "3"` silently falls back to v1 and over-unifies. **Set it explicitly.**

The hazard remains real where unification *does* apply: a sibling/test/example/benchmark enabling a leaf feature turns that component path **on** for a binary that never asked for it. Diagnose with `cargo tree -e features`. Nightly `-Z feature-unification` exposes `selected` (default), `workspace` (uniform Spring-like assembly across all members), and `package` (isolate members from sibling leakage) — document these as escape hatches but do **not** depend on them for correctness.

### The additive-features rule

*(Corrected, precise framing.)* Cargo **recommends** (does **not** enforce) that features be **additive**: *"enabling a feature should not disable functionality, and it should usually be safe to enable any combination of features."* Note **"should"** and **"usually."** Non-additive features compile fine; they break only when consumers demand conflicting states. Because unification ORs features on and they **cannot be un-set downstream**, a feature that *removes* or *swaps* behaviour (`no-default-runtime`, `single-threaded`) is a footgun. Phrase opt-outs as opt-ins (`std`, not `no_std`).

**Mutually exclusive features are officially unsupported** and will be unioned-on-together. Model a *choice* as additive capability + **runtime selection** (Spring `@Profile`/conditional-on-property), or split into separate crates. If truly unavoidable, gate with `#[cfg(all(feature="a", feature="b"))] compile_error!(...)` to fail loudly.

### Capability-named umbrella features

- An `optional = true` dependency implicitly defines a same-named feature; use the **`dep:` prefix** (Rust 1.60+) in `[features]` to suppress it and expose a capability name instead: `tokio = ["dep:leaf-tokio"]` hides the integration-crate name. (Forgetting `dep:` leaks the raw crate name and lets users bypass leaf's umbrella + force-link shim.)
- **Weak features** `"pkg?/feat"` (Rust 1.60+) enable a feature on an integration crate **only if it is already present** — how leaf threads cross-cutting capabilities (`serde`, `tracing`, `metrics`) without force-pulling dormant crates. Plain `"pkg/feat"` would *also* activate the optional crate.

### `cfg(feature)` gates COMPILATION, not LINK retention

This is the crux for leaf. `#[cfg(feature="x")]` decides whether the registration *item exists in the compiled rlib*. It does **not** protect it from the linker, nor force the crate to be linked. So "enable the `leaf-redis` feature" can correctly compile the registrations and still produce an **empty registry** unless leaf forces the crate onto the link graph by reference. *(Correction to over-attributing this to `--gc-sections`: the historically dominant cause was rustc reachability (`#47384`), mostly fixed in 1.62; `--gc-sections` dropping an already-included section is real but secondary/platform-dependent, most acute on macOS-without-LTO and in `staticlib` output.)*

**Leaf implications.**
- Model features as **additive capabilities** only (`tokio`, `metrics`, `redis`), each a `dep:`-hidden umbrella over an integration crate; never as on/off behaviour switches or mutually exclusive backends — make backend choice a runtime/profile decision.
- A feature must do **two things**: (a) gate compilation of the integration crate's registrations, **and** (b) cause leaf's generated app-root to emit a **force-link reference** to that crate. A feature alone is necessary but **not sufficient**.
- Set `[workspace] resolver = "3"` explicitly so proc-macro/codegen, dev, and target features don't leak into the production component set.
- Implement Spring-style `@Conditional` gating at **runtime** over collected descriptors — link-time collection answers "what *could* contribute"; runtime conditions answer "what *does* wire up." Condition evaluation must be deterministic given the **final resolved feature set of the binary**, and documented to differ between `-p` and `--workspace`.
- For generics: a feature that "enables" a generic component still needs a **concrete registration shim per instantiation** — features won't conjure the monomorphization.

---

## How real Rust frameworks do cross-crate extensibility — ecosystem lessons

Real frameworks split into two camps.

**Explicit-composition camp (no auto-discovery, by design).**
- **bevy:** `App::add_plugins(p)` where a plugin is any `fn(&mut App)`; plugins run in **registration order**, carry config, bundle via `PluginGroup`. Maximum reliability/portability, zero linker magic. Notably, **bevy deliberately rejected `linkme`/`inventory`** for core registration because they "only work on some platforms" and add build jank.
- **axum/tower:** explicit, value-level, type-checked composition (`ServiceBuilder::layer`, `Router::route/merge/nest/layer`).
- **tracing:** role separation — `set_global_default` is called **once, by the application binary**; **libraries must not** call it (they only emit events / use scoped `with_default`). A clean "who owns the container" pattern.
- **DI crates — shaku / dill / teloc:** all **explicit, no scanning.** `shaku` `module!` lists components with compile-time `HasComponent` checks; `dill` `Catalog::builder().add::<Impl>().bind::<dyn Trait,_>().build()` with **runtime** graph `validate()` (detects dangling/ambiguous/cyclic deps); `teloc` `ServiceProvider::add_singleton/add_transient`, type-checked. In all three, multi-crate is just "call `.add()` more times" in the composing crate.

**Link-time-magic camp (apparent auto-discovery, gated/caveated).**
- **typetag, pyo3, leptos, cucumber** use `inventory`/`linkme` but **every one gates it behind feature flags or documents platform exclusions** because it is fundamentally unreliable. `serde_flexitos` exists *specifically* because typetag's `inventory` "does not work on every platform (for example, WASM)."

**The build-time-codegen camp (the deterministic middle path).**
- **sqlx:** `query!` checks SQL at compile time against a live DB; `cargo sqlx prepare` serializes metadata into a checked-in `.sqlx` cache (works offline via `SQLX_OFFLINE`); `cargo sqlx prepare --check` fails CI when stale. The template for leaf: an **out-of-band build step produces explicit, checked-in artifacts** the compile then consumes — no link magic. *(sqlx does not discover foreign crates; it analyzes known inputs.)*

**Leaf implications.**
- Pure *passive* auto-discovery (the literal classpath-scan analogue) is **not reliably achievable on stable Rust across all platforms today.** Do not promise it as the only mechanism.
- The reliable, stable, cross-platform path is **build-time codegen** (sqlx model): a leaf build step walks the resolved dependency graph (`cargo metadata` / a manifest convention) and emits thin, hand-writable explicit registration + force-link references into the binary crate — sidestepping Layer 0, `#67209`, and `--gc-sections` entirely.
- Adopt tracing's **role separation**: contribution crates only *declare* components (no global side effects, no global locks); the single application/binary crate **owns** assembly.
- Adopt dill's **runtime graph validation** (dangling/ambiguous/cyclic beans) as a complement to compile-time wiring, with good diagnostics.
- Provide a **checked-in, `--check`-able manifest** (sqlx-style) so builds are reproducible and CI catches silently-lost contributions.

---

## Workspace, dependency graph & topology constraints

Two non-negotiable Cargo/Rust facts drive the topology:

1. **No cycles among normal dependencies** between packages (Cargo errors "cyclic package dependency"); only `[dev-dependencies]` cycles are tolerated (and they don't help runtime wiring, and complicate publishing). → Forces a **strict layered DAG.**
2. **An unreferenced dependency rlib is not linked** (Layer 0 above). → The kernel **cannot** force its plugins to link; only the final binary (or a generated aggregator) can.

### The realistic topology

```
leaf-core      (kernel: traits, Descriptor/Condition types, the
                distributed_slice / collect! registries; NO in-workspace deps)
   ▲
   │  (features & integration crates depend UP onto the kernel, never the reverse)
leaf-tokio, leaf-redis, leaf-axum, ...  (each registers into leaf-core's slices)
   ▲
leaf-macros    (proc-macro = true; isolated; emits only ::leaf-core/::leaf paths)
   ▲
leaf           (umbrella/facade: pub use of core + macros + enabled integrations;
                one dependency for users; stable ::leaf::… path surface)
   ▲
application / binary crate  (enables features; hosts the generated force-link
                             references; is the ONLY place the full graph is visible)
```

- **Integration/feature crates must depend on `leaf-core`, never the umbrella `leaf`** (depending on the umbrella risks a cycle and a hard error).
- The **umbrella facade** gives users one dependency and lets internal crate boundaries stay `#[doc(hidden)]`/unstable (good for semver). It must also ensure enabled integration crates are actually **linked** (`pub use leaf_axum;` or a `leaf::scan!{ leaf_axum, ... }` macro emitting `use leaf_axum as _;`).
- **Workspace inheritance** (`[workspace.package]`, `[workspace.dependencies]` with `key.workspace = true`, single root `Cargo.lock` and `target/`) gives **version unification** — `leaf-core`, `linkme`, `inventory` resolve to one copy each, preventing the "two copies of a type registry" hazard. Pin `linkme`/`inventory` here.
- Splitting into dozens of crates improves **compile parallelism and incremental rebuilds** (the crate is the unit of parallel codegen), at the cost of semver surface — which the umbrella facade contains.

**Leaf implications.** The force-linking responsibility lives in the **top-level app or a generated aggregator**, never the kernel. Ship a `leaf doctor`/startup self-check that lists discovered registrations and warns when an expected integration produced **zero** contributions (the classic symptom of an unreferenced/GC'd crate). Provide a `register_component!(ConcreteType)` macro for generic components.

---

## Viable architectures for leaf cross-crate auto-discovery

Four candidate architectures. **No winner is chosen here** — that is a later phase. Each is scored against leaf's invariants: **DX (#1)**, **low overhead**, **stable-by-default**, **thin macros**, **reliability across platforms**.

### Architecture A — Pure link-time distributed slices (`linkme`)

**Shape.** `leaf-macros` expands `#[component]`/`#[bean]`/`#[configuration]` to a single `#[distributed_slice(leaf_core::COMPONENTS)] static __LEAF_x: Descriptor = ...;`. The runtime reads `&'static [Descriptor]` in `main`, runs conditions, topo-sorts, and wires asynchronously.

| Invariant | Assessment |
|---|---|
| DX | Excellent for the *authoring* side (annotate and forget) — **provided** Layer 0 is solved; otherwise silent empty registries are terrible DX. |
| Overhead | Best possible — zero runtime collection cost, no pre-main code. |
| Stable-by-default | Yes (stable `linkme`). |
| Thin macros | Yes — emits one hand-writable `const` static. |
| Platform reliability | Solid on Linux/macOS/Windows/BSD/illumos **with Rust ≥ 1.89 or `-z nostart-stop-gc`**; **fails on wasm/embedded**; fragile under LLD pre-1.89 and `staticlib`-into-C. |

**Tradeoff.** Lowest overhead and cleanest macros, but **does not by itself solve Layer 0** (unreferenced crates) and is the most platform-fragile. Must be paired with an anti-DCE/force-link strategy (→ Architecture B). `const`-only and no generics.

### Architecture B — Macro-emitted registration + `linkme` **with an explicit anti-DCE / force-link strategy**

**Shape.** Architecture A **plus** a generated app-root: `#[leaf::main]` (or a `leaf::scan!{ leaf_tokio, leaf_redis }` macro / `cargo metadata`-driven `build.rs`) emits `use leaf_tokio as _;` for every opted-in integration crate, guaranteeing each is on the link graph; recommended `.cargo/config.toml` linker args for ELF; a startup self-check comparing discovered vs expected crate markers.

| Invariant | Assessment |
|---|---|
| DX | Strong — users list their integration crates **once** (far less than hand-registering every bean), and the self-check turns silent failures into loud ones. The "list crates once" step is the honest analogue of Spring Boot's `@ComponentScan basePackages` / starter list. |
| Overhead | Same as A (zero collection cost); the force-link references are free. |
| Stable-by-default | Yes. |
| Thin macros | Yes — registration macro stays thin; the `scan!`/`main` macro emits only `use … as _;` and a descriptor walk, all hand-writable. |
| Platform reliability | Best of the link-time options on server targets; still weak on wasm/embedded (degrade to D there). |

**Tradeoff.** This is the realistic "Spring-like but honest" sweet spot for server targets: auto-discovery *within* each linked crate, explicit *opt-in* of which crates participate. Aligns with the predictability concern from the `#[global_registration]` author. Cost: the user must enumerate participating crates (one line each) and Layer-0 discipline is non-optional.

### Architecture C — Build-time codegen (sqlx model, `cargo metadata`-driven)

**Shape.** A `build.rs` (or `cargo leaf prepare` CLI) in the binary crate reads the resolved dependency graph and a manifest convention, then emits explicit `register_all(ctx) { leaf_tokio::register(ctx); leaf_redis::register(ctx); ... }` into `OUT_DIR`, `include!`'d into the binary. Optionally a checked-in, `--check`-able manifest.

| Invariant | Assessment |
|---|---|
| DX | Good but with a build step; deterministic and debuggable (the generated file is inspectable). Best DX story for *reproducibility* and CI. |
| Overhead | Zero runtime collection cost (explicit calls); no pre-main code. |
| Stable-by-default | Yes — pure stable codegen, no linker tricks. |
| Thin macros | N/A for collection (no link magic); per-crate registration fns are hand-writable. |
| Platform reliability | **Best — fully platform-independent**, including wasm/embedded/`staticlib`. Avoids both DCE layers and non-deterministic DCE entirely. |

**Tradeoff.** The most reliable and portable, and the recommended fallback for hostile targets. Costs a build step and "less magic"; still cannot scan crates the build doesn't know about (it enumerates **declared** integration crates via metadata, not a true classpath). Requires deterministic/idempotent codegen and `OUT_DIR` hygiene.

### Architecture D — Explicit with ergonomic helpers (bevy-style)

**Shape.** `leaf::App::new().with(leaf_tokio::plugin()).with(leaf_redis::plugin()).build()` — every contribution explicitly added in the binary; macros only reduce boilerplate within a plugin.

| Invariant | Assessment |
|---|---|
| DX | Lowest "magic," highest predictability; more typing for large apps, but bevy shows it scales acceptably with `PluginGroup`-style bundling. |
| Overhead | Zero. |
| Stable-by-default | Yes. |
| Thin macros | Trivially. |
| Platform reliability | **Maximum — works on every target, every linker.** The only thing guaranteed everywhere. |

**Tradeoff.** Sacrifices "auto" for total reliability. **This is the escape hatch leaf must always offer**, and the canonical path on wasm/embedded. Its existence lets the runtime API stay identical across substrates.

### Cross-cutting recommendation (not a winner-pick)

The architectures are **composable, not exclusive.** The decision to defer is: *which substrate is the default on server targets* (A+B vs C), with **D as the universal fallback** and **C as the wasm/embedded path**. All four can share an identical runtime API (a `&[Descriptor]` plus runtime conditions/topo-sort), making the substrate swappable per target. The two genuinely separable questions for the next phase:
1. Default server substrate: **B (`linkme` + force-link)** for zero overhead, or **C (build-time codegen)** for determinism/portability?
2. How is "which crates participate" expressed — a `scan!`/`with` list (explicit, predictable) vs a metadata-driven sweep (more automatic, less predictable)?

---

## Hard constraints & non-negotiable gotchas

Every leaf designer must keep these in mind:

- **DCE is the #1 failure mode, and it is SILENT.** A dropped registration yields an empty iterable — never a compile/link error. It can pass in debug, fail in release; vary by linker (GNU `ld` vs LLD), platform, and `codegen-units`. Build a self-check (expected-count / manifest cross-check) and **test the actual final binary, not lib tests** (lib tests hide the dropping).
- **Three elimination layers, in order of fundamentality:** (0) the crate must be **path-referenced** to be linked at all (`inventory#7`; intended behaviour, not a bug); (A) **rustc reachability/codegen-units** can drop registrations in unreferenced modules/crates (`#47384`; `#67209`/`linkme#36` are *this* layer, mostly fixed in rustc 1.62); (B) **system-linker `--gc-sections`/`start-stop-gc`** can drop sections (`linkme#49`; ELF default fixed in Rust 1.89 via `#[used(linker)]`/`SHF_GNU_RETAIN`; `-z nostart-stop-gc` is the pre-1.89/escape-hatch lever).
- **`#[used]` alone does not defeat the linker.** *"The linker is still free to remove such an item."* Need `#[used(linker)]`/`SHF_GNU_RETAIN` (ELF default from 1.89) — and even that is platform-dependent (`#145362`: ineffective on `x86_64-pc-windows-gnu`).
- **Linker choice is a correctness variable.** Same OS, GNU `ld` vs `ld.lld`, can give different results pre-1.89.
- **Generics cannot be registered.** Only concrete, monomorphized `'static` values get a symbol. Macros must **hard-error** on generic targets; provide `register_component!(ConcreteType)` per instantiation.
- **No dependency cycles** (normal deps). Contributions flow features → kernel; the kernel cannot reference its plugins. Force-linking lives in the binary/aggregator.
- **Features must be additive** (by convention, not enforcement) and **"usually" safe in any combination**; unification ORs them on and they cannot be un-set downstream. No mutually exclusive features — use runtime/profile selection. Set `[workspace] resolver = "3"` explicitly.
- **`cfg(feature)` gates compilation, not link retention or linkage.** Enabling a feature compiles registrations but does not force the crate to be linked, nor keep its sections.
- **Proc-macro isolation:** a `proc-macro = true` crate exports only macros and is built for the host; emitted code must use **absolute paths**; never accumulate state across invocations. Keep `leaf-core` (the API) separate from `leaf-macros`.
- **Life-before-main (`inventory`) is hostile to async/safety:** runs before `main` on an unknown thread, no panic, no async, unspecified order, std possibly not ready. Register descriptors/factories, never live objects.
- **Ordering is unspecified** for both `linkme` and `inventory` (and may be deliberately randomized). Encode priority/ordering/conditions as **data** on each descriptor; resolve at runtime via a topological sort.
- **wasm & embedded are weak spots.** No native linker sections on wasm; `inventory` needs `__wasm_call_ctors` actually invoked; `linkme` is not tested there. Plan an explicit/codegen fallback (Architectures C/D) for these targets.
- **`build.rs` cannot scan siblings/downstream.** Info flows downward only; the kernel's `build.rs` can never enumerate its dependents. `cargo::metadata`/`DEP_` is `links`-gated (globally unique value), immediate-dependents-only, and not an aggregation bus.
- **`inventory` is not built on `ctor`** (current versions implement constructors themselves). typetag = `inventory` + `erased-serde`. Don't repeat the stale "inventory+ctor" wording as fact.
- **Pin `linkme >= 0.3.24`** (RUSTSEC-2024-0407 type-confusion).
- **Validate the substrate in CI across the matrix that bites:** release builds (LTO + `--gc-sections`), the **final-binary link** (not lib tests), workspace vs single-package builds (feature unification), each target OS, and each linker — because the failures are silent and configuration-dependent.
- **Do not block on `#[global_registration]`** (unstable, cross-crate semantics unresolved); design the descriptor format so it *could* migrate later, but ship on stable.
- **Scope auto-discovery; do not make it truly global.** Mirror Spring Boot's `@ComponentScan basePackages` + auto-config exclusions, addressing both the predictability concern and feature-unification non-determinism. The `#[global_registration]` author's own conclusion: prefer **explicit, opt-in** cross-crate participation.

---

## Sources

**Rust/Cargo reference & linkage**
- The Rust Reference — Linkage: https://doc.rust-lang.org/reference/linkage.html
- The Rust Reference — Application Binary Interface (`#[used]`, `#[link_section]`, `#[no_mangle]`): https://doc.rust-lang.org/reference/abi.html
- The Rust Reference — Procedural Macros: https://doc.rust-lang.org/reference/procedural-macros.html
- The Rust Reference — Extern crates (`extern crate foo as _;` link side-effect): https://doc.rust-lang.org/reference/items/extern-crates.html
- The Cargo Book — Build Scripts: https://doc.rust-lang.org/cargo/reference/build-scripts.html
- The Cargo Book — Build Script Examples: https://doc.rust-lang.org/cargo/reference/build-script-examples.html
- The Cargo Book — Environment Variables: https://doc.rust-lang.org/cargo/reference/environment-variables.html
- The Cargo Book — Features (unification, additivity, mutually-exclusive): https://doc.rust-lang.org/cargo/reference/features.html
- The Cargo Book — Dependency Resolution (resolver versions): https://doc.rust-lang.org/cargo/reference/resolver.html
- The Cargo Book — Unstable (`feature-unification` modes): https://doc.rust-lang.org/cargo/reference/unstable.html
- The Cargo Book — Workspaces: https://doc.rust-lang.org/cargo/reference/workspaces.html
- The Cargo Book — Specifying Dependencies (`workspace = true`): https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html
- The Cargo Book — Manifest (`package.links`): https://doc.rust-lang.org/cargo/reference/manifest.html
- Rust Edition Guide — Cargo resolver v3 (2024): https://doc.rust-lang.org/edition-guide/rust-2024/cargo-resolver.html
- Rust Edition Guide — Default resolver v2 (2021): https://doc.rust-lang.org/edition-guide/rust-2021/default-cargo-resolver.html
- Announcing Rust 1.89.0: https://blog.rust-lang.org/2025/08/07/Rust-1.89.0/
- rustc-dev-guide — Libraries and metadata: https://rustc-dev-guide.rust-lang.org/backend/libs-and-metadata.html

**Link-time registration crates**
- `linkme` — README/docs: https://github.com/dtolnay/linkme | https://docs.rs/linkme/latest/linkme/
- `linkme::DistributedSlice` API: https://docs.rs/linkme/latest/linkme/struct.DistributedSlice.html
- `linkme` per-platform sections (`impl/src/linker.rs`): https://github.com/dtolnay/linkme/blob/master/impl/src/linker.rs
- `inventory` — README/docs: https://github.com/dtolnay/inventory | https://docs.rs/inventory/latest/inventory/
- `inventory` Cargo.toml / lib.rs (no `ctor` dep): https://github.com/dtolnay/inventory/blob/master/Cargo.toml | https://github.com/dtolnay/inventory/blob/master/src/lib.rs
- `ctor` — docs (life-before-main hazards): https://docs.rs/ctor/latest/ctor/
- `typetag` — docs (inventory + erased-serde): https://github.com/dtolnay/typetag | https://docs.rs/typetag/latest/typetag/
- `serde_flexitos` (explicit registry; inventory not on WASM): https://docs.rs/serde_flexitos
- RUSTSEC-2024-0407 (linkme type-confusion, fixed 0.3.24): https://rustsec.org/advisories/RUSTSEC-2024-0407

**Issues & RFCs (DCE, `#[used]`, global registration)**
- rust-lang/rust#47384 (used static only present in reachable module): https://github.com/rust-lang/rust/issues/47384 (key comment: https://github.com/rust-lang/rust/issues/47384#issuecomment-1032974536)
- rust-lang/rust#67209 (`#[link_section]` only usable from root crate; fixed ~1.62): https://github.com/rust-lang/rust/issues/67209
- rust-lang/rust#150462 (non-deterministic DCE): https://github.com/rust-lang/rust/issues/150462
- rust-lang/rust#145362 (`#[used(linker)]` ineffective on windows-gnu): https://github.com/rust-lang/rust/issues/145362
- rust-lang/rust#137426 (link object files that use `#[used]`): https://github.com/rust-lang/rust/pull/137426
- rust-lang/rust#140872 (`#[used(linker)]` default on ELF; Rust 1.89): https://github.com/rust-lang/rust/pull/140872
- rust-lang/rust#125119 (tracking issue, `#[global_registration]`): https://github.com/rust-lang/rust/issues/125119
- linkme#31 / #36 (dependency-crate members discarded; dup; fixed): https://github.com/dtolnay/linkme/issues/31 | https://github.com/dtolnay/linkme/issues/36
- linkme#49 (encapsulation symbols under `--gc-sections`; `-z nostart-stop-gc`): https://github.com/dtolnay/linkme/issues/49
- linkme#61 (macOS distributed slice empty without LTO): https://github.com/dtolnay/linkme/issues/61
- inventory#7 (submit! needs downstream crate referenced; `use crate as _;`): https://github.com/dtolnay/inventory/issues/7
- RFC 2386 — `#[used]` (used(compiler) vs used(linker), SHF_GNU_RETAIN): https://rust-lang.github.io/rfcs/2386-used.html
- RFC 2957 — Cargo feature resolver v2: https://rust-lang.github.io/rfcs/2957-cargo-features2.html
- RFC 3692 — feature-unification: https://rust-lang.github.io/rfcs/3692-feature-unification.html
- Tracking issue — workspace feature-unification (cargo#14774): https://github.com/rust-lang/cargo/issues/14774
- Global Registration pre-RFC (internals): https://internals.rust-lang.org/t/global-registration-a-kind-of-pre-rfc/20813
- Global Registration (Jana Dönszelmann) — predictability/surprise conclusion: https://donsz.nl/blog/global-registration/
- "Can build.rs get activated features of the top package?" (Josh Triplett): https://internals.rust-lang.org/t/can-build-rs-get-activated-features-of-the-top-package/13515

**Linker internals**
- MaskRay — Linker garbage collection (`--gc-sections`, archive semantics, `SHF_GNU_RETAIN`): https://maskray.me/blog/2021-02-28-linker-garbage-collection
- MaskRay — Metadata sections, COMDAT, `SHF_LINK_ORDER`/start-stop-gc: https://maskray.me/blog/2021-01-31-metadata-sections-comdat-and-shf-link-order
- LLD — `start-stop-gc` (LLD 13+ eager default): https://lld.llvm.org/ELF/start-stop-gc
- "There Is Life Before Main in Rust" (grack): https://grack.com/blog/2026/06/11/life-before-main/

**Ecosystem frameworks & dynamic loading**
- bevy `Plugin` / `add_plugins`: https://docs.rs/bevy/latest/bevy/app/trait.Plugin.html | https://docs.rs/bevy_app/latest/bevy_app/trait.Plugin.html
- Bevy Cheat Book — Plugins: https://bevy-cheatbook.github.io/programming/plugins.html
- axum `Router`: https://docs.rs/axum/latest/axum/struct.Router.html
- tracing `set_global_default`: https://docs.rs/tracing/latest/tracing/subscriber/fn.set_global_default.html
- shaku: https://docs.rs/shaku/latest/shaku/
- dill-rs (runtime `validate()`): https://github.com/kamu-data/dill-rs
- teloc: https://github.com/p0lunin/teloc
- sqlx (compile-time checked queries; offline cache): https://github.com/launchbadge/sqlx | https://github.com/launchbadge/sqlx/blob/main/sqlx-cli/README.md | https://docs.rs/sqlx/latest/sqlx/macro.query.html
- abi_stable: https://crates.io/crates/abi_stable | https://docs.rs/abi_stable/latest/abi_stable/
- libloading `Library` (`close()`/Drop unload, may be a no-op): https://docs.rs/libloading/latest/libloading/struct.Library.html
- "Plugins in Rust: Dynamic Loading" (NullDeref): https://nullderef.com/blog/plugin-dynload/
- wasm-bindgen#1216 (static constructors on wasm): https://github.com/rustwasm/wasm-bindgen/issues/1216

**Tooling**
- `cargo_metadata` (programmatic graph enumeration): https://docs.rs/cargo_metadata/latest/cargo_metadata/
- Luca Palmieri — "Going beyond build.rs: cargo-px": https://lpalmieri.com/posts/cargo-px/
- "Cargo Workspace and the Feature Unification Pitfall" (nickb.dev): https://nickb.dev/blog/cargo-workspace-and-the-feature-unification-pitfall/
