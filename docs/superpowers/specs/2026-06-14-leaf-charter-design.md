# leaf — Project Charter & Design Methodology

> **Status:** Draft for review · **Date:** 2026-06-14 · **Owner:** Jan Brachthäuser
>
> This charter governs the design of **leaf**, a dependency-injection / IoC framework for Rust.
> It fixes scope, non-negotiable invariants, the multi-phase design methodology, and the
> identity/topology/DX north-star. It deliberately does **not** decide the framework's
> meta-architecture — that is the explicit job of Phase 2.

---

## 0. Vision

leaf targets **full feature parity with the non-ecosystem core of Spring Boot 4 + Spring
Framework 7**, re-expressed in idiomatic, **async-first** Rust. We replicate Spring's *intent* —
its developer experience, its composability, its opinionated-yet-overridable defaults — **not**
its JVM mechanisms. The work proceeds as a disciplined, multi-phase **design** effort; no
production implementation begins until the design has converged (Phase 4).

The conceptual foundation is the grounded reference `docs/research/spring-boot-4-di-design.md`.

---

## 1. Scope

**In scope (parity targets):**
- The IoC container & bean model; bean naming, scopes, lifecycle, collisions.
- Dependency injection & autowiring; conditions; profiles.
- The `Environment` / property model & externalized configuration; type-safe config binding.
- Auto-configuration (the opinionated, overridable-defaults layer).
- Events; the AOP / interception substrate.
- **All cross-cutting framework abstractions:** transaction management, caching, validation,
  scheduling/async, an expression layer, type conversion/formatting, i18n/messages, resource
  loading, retry/resilience.
- The bootstrap layer (the `SpringApplication`-equivalent entry point).

**Out of scope (but designed to be pluggable):** concrete ecosystem integrations — web
(MVC/reactive), data/JPA/ORM, security, messaging, and specific technology bindings. These
attach **on top** via auto-discovery; **the core never depends on them.**

---

## 2. Invariants (non-negotiable — every phase inherits these)

1. **DX is priority #1.** Every decision optimizes first for a rust-native, Spring-grade
   developer experience (declarative, low-ceremony, auto-discovered). **Low overhead is #2** —
   always weighed, but it loses to DX in a true conflict.
2. **Intent parity, not mechanism parity.** We realize each Spring feature's *intent* in
   idiomatic Rust, never its JVM mechanism for its own sake. Where a Spring feature exists only
   to route around a JVM limitation Rust lacks (erasure, no compile-time reflection, runtime
   classpath scanning), we may realize it completely differently — or prove it unnecessary.
3. **Async-first, with rigorous execution-context discipline.** The whole framework is
   async-first; no sync/async bimodality. Execution context is *explicit everywhere*: what runs
   where, what may block, what must never block, how cancellation and errors propagate, where
   work crosses threads. Threading is designed with intent and abuse-resistance, not emergent.
4. **Reactive on hot paths; polling only on cold paths.** Hot paths (resolution, event dispatch,
   request-scoped work, steady-state running) must be event-driven/notification-based (wakers,
   readiness, signals) — never busy-wait or interval-poll. Async polling loops (retry/backoff,
   reconnect, periodic refresh/health) are permitted **only on cold paths** (startup, background
   maintenance, explicit scheduled tasks).
5. **Concurrency & memory discipline.** No global locking where avoidable; minimize raw
   `Mutex`/`RwLock` and coarse shared-mutable state; lock *with intent* (fine-grained, justified,
   documented). Prefer stack over heap; avoid gratuitous `Arc`/boxing/heap churn. Good-Rust
   hygiene as a first-class constraint.
6. **Opinionated base, pluggable everything-else.** leaf forces only its own (intentionally
   opinionated) core. Async runtimes (tokio/smol), actor systems (kameo), and all ecosystem
   integrations attach on top via auto-discovery — never required. Runtime-agnosticism is a goal
   we relax only if it materially improves the core.
7. **std assumed.** No `no_std` contortions.
8. **Stable by default; nightly only when it markedly wins.** Nightly is used only for a marked,
   justified improvement, judged against `docs/research/rust-nightly-features-tradeoffs.md`.
   End-user predictability is paramount. *(Validated 2026-06-14: the core is fully buildable on
   stable Rust; no nightly feature is currently required.)*
9. **Fail with rich, complete, actionable context; as early as the richest diagnostic allows.**
   Errors carry the full causal chain — what was being assembled, what it needed, what was
   expected, what was actually found (incl. candidates considered), and where. Timing is the
   supporting goal: compile-time **where the compile-time error is also legible**, else a
   dedicated startup validation pass, else runtime. We prefer the failure mode with the most
   complete context; we do **not** force a check to compile-time if that only yields a cryptic
   error a deferred check could explain fully.
10. **Thin macros over a fully hand-writable API.** Macros carry as little logic as possible;
    they only *emit* what a user could have written by hand against public APIs. Nothing is
    hidden in or exclusive to a macro. Heavy logic lives in normal, testable crates; `cargo
    expand` stays legible; escape hatches exist at every level.
11. **Unification is the goal, not a nicety.** Features must share maximal common machinery; we
    deliberately resist bespoke per-feature solutions when a unified primitive can serve. This is
    the entire reason Phases 2–4 exist.

---

## 3. Deliberately deferred (these are Phase 2 outputs — NOT decided here)

Pre-deciding any of these would bias Phase 2's signal and defeat the methodology:

- The **metadata / codegen boundary** — proc-macro-emitted metadata vs `build.rs` generation vs
  runtime assembly.
- **Compile-time vs runtime resolution** — how much of the bean graph is frozen at build time
  (the AOT lineage) vs resolved at runtime.
- **Dependency-based auto-configuration** — whether it exists at all, and its ripple effects.
- The concrete **async execution & context-propagation model** (only the *requirement* for rigor
  is fixed, in §2.3/§2.4).
- The **unified error / diagnostics model** (the realization of §2.9).
- The **cross-crate composition & tooling model** (see threaded concern below).
- The **scope / lifetime / ownership model** — the deepest impedance mismatch (Spring's GC'd
  shared singletons vs Rust ownership; `Arc<T>` vs `Arc<dyn Trait>` vs borrows).
- The **type-erasure & registry substrate** — how `dyn` beans are stored/keyed/retrieved.
- The **conditional strategy** — `@Conditional` → `cfg` vs build-time codegen conditions vs
  runtime gates (likely a blend).
- The **fine-grained crate decomposition** (depends on all of the above).

---

## 4. The phase machine

Each phase is a **workflow** producing a **reviewable artifact**, gated by owner sign-off.
Nothing advances without it.

**Two concerns are threaded through *every* phase** (each phase must explicitly answer them):
- **(T1) Cross-crate / tooling:** how does this behave across crate boundaries and through
  Cargo / `build.rs` / proc-macros / the linker? (Rust has no global classpath, no runtime
  reflection; build.rs can't see sibling crates; proc-macros see only their own invocation.)
- **(T2) Async / execution context:** which execution context does this run in, what may it
  block, how do cancellation and errors propagate?

| Phase | Purpose | Output |
|---|---|---|
| **0 — Toolkit prep** | Standing grounded references. Spring DI research ✅, nightly-features tradeoffs ✅, cross-crate composition & tooling ⏳ (running). More only if Phase 1 reveals a need. | `docs/research/` |
| **1 — Per-feature design (breadth)** | Seeded from the Spring research concept inventory (43 findings → a deduped feature catalog). For each feature: its intent, candidate rust-native realization(s), honest clashes/tensions, and how it *wants* to interact with neighbours. **Implementation-agnostic** — no meta-architecture picks. | `docs/design/phase1/` |
| **2 — Meta-concepts (the toolkit)** | Mine the whole Phase 1 catalog; discover and **resolve** the deferred cross-cutting decisions (§3) plus any others that emerge. Open-ended. | ADRs + toolkit spec, `docs/design/phase2/` |
| **3 — Re-unify (depth)** | Redesign every Phase 1 feature on top of the Phase 2 toolkit; plan cross-feature interactions explicitly; converge on maximal shared machinery. Produces the now-decidable crate topology. | consolidated design + topology, `docs/design/phase3/` |
| **4 — Integration & convergence (iterate to fit)** | Stress-test the Phase 3 redesigns *together* end-to-end. Where they don't compose cleanly, **loop back** — to Phase 3 (re-unify offenders) or Phase 2 (a meta-concept was wrong) — and re-converge. **Explicitly a loop, not a one-shot.** | convergence report + finalized design, `docs/design/phase4/` |

The Phase 4 output is what eventually feeds the per-crate / per-feature `writing-plans` →
implementation cycle.

---

## 5. Candidate Phase 2 meta-concepts (a seed list, not a ceiling)

Phase 2 will actively hunt for more. Foreseen so far:
- The **scope/lifetime/ownership model** (the keystone decision; drives DX *and* overhead more
  than any other).
- The **metadata/codegen boundary** and **compile-vs-runtime split**.
- The **cross-crate composition & tooling model** (T1) — how contributions in arbitrary
  downstream/sibling crates are discovered and assembled.
- The **async execution & context-propagation model** (T2) — incl. the rust-native answer to
  ThreadLocal holders across `.await`, and structured concurrency / cancellation.
- The **unified error / diagnostics model** (§2.9).
- The **type-erasure & registry substrate**.
- The **conditional strategy** (`cfg` vs codegen vs runtime).
- **Dependency-based auto-configuration**: yes/no and its consequences.

---

## 6. Identity, topology principles & DX north-star

**Identity.** The framework is **leaf**, mirroring Spring's split:
- **`leaf-framework`** — IoC container, bean model, lifecycle, config/environment, interception
  substrate, events, and the cross-cutting abstractions (tx/cache/validation/scheduling/
  expressions/conversion/i18n/resources/retry).
- **`leaf-boot`** — the opinionated bootstrap + auto-configuration layer on top.
- **`leaf-macros` / `leaf-codegen`** — proc-macros and `build.rs` support, isolated from runtime
  crates.
- **integration crates** (`leaf-tokio`, `leaf-kameo`, …) — optional, pluggable, auto-discovered;
  the core never depends on them.

**Topology principles** (the *fine-grained* crate graph is a Phase 3 output, since it depends on
Phase 2; but these principles hold regardless):
- A small, stable **kernel** that rarely changes; features layered above it.
- **Macro/codegen crates stay separate** from runtime crates (and proc-macro crates can export
  only macros).
- **Optional features become separate crates** (not just cargo features) where that sharpens
  boundaries and compile-time optionality — Spring-module-like; dozens of crates expected.
- Dependencies point **inward**; the core never depends on an integration crate; **no dependency
  cycles** (a hard Cargo constraint that shapes layering).

**DX north-star.** The felt experience is Spring-grade: declare intent, leaf wires it — minimal
ceremony, auto-discovery, sensible defaults — but via rust-native means (attribute macros
emitting *metadata*, build/link-time assembly, typed APIs), with **explicit escape hatches at
every level** (always a programmatic/manual fallback, per §2.10). The macro-vs-codegen-vs-runtime
split is Phase 2; the *feel* is fixed here.

---

## 7. Grounding inputs (handed to every phase's agents)

- `docs/research/spring-boot-4-di-design.md` — Spring Boot 4 / Framework 7 core-DI design
  philosophy (the parity target's *intent*).
- `docs/research/rust-nightly-features-tradeoffs.md` — which Rust capabilities are available on
  stable vs nightly, with recommendations (the *capability* envelope).
- `docs/research/rust-cross-crate-composition.md` — *(pending)* how Rust/Cargo cross-crate
  composition, `build.rs`, proc-macros, and link-time registration actually work (the *tooling*
  reality for auto-discovery).

---

## 8. Operating rules & gates

- **Phase 1 is implementation-agnostic** so Phase 2's signal stays unbiased.
- **Every phase agent is handed the grounding inputs** (§7) so the effort stays anchored to real
  Spring intent and real Rust capability.
- **Owner gate between phases:** each phase artifact is reviewed and signed off before the next
  begins.
- **Iteration is expected:** Phase 4 loops; it may reopen Phase 3 or Phase 2. Convergence — not a
  fixed phase count — is the exit condition.
