# Phase 2 — Architecture Toolkit Index

12 resolved ADRs + toolkit overview + coherence review. Keystone (ownership model) decided via 3-proposal judge panel. WASM dropped (native-only).

## Read order
- [`TOOLKIT.md`](TOOLKIT.md) — the unified architecture in one place (start here)
- [`COHERENCE.md`](COHERENCE.md) — contradiction check & verdict

- [`ADR-01-ownership-model.md`](ADR-01-ownership-model.md) — Scope / Lifetime / Ownership Model — the uniform erased shared handle + scope-keyed publication
- [`ADR-02-registry-substrate.md`](ADR-02-registry-substrate.md) — Type-Erasure & Registry Substrate (origin-agnostic)
- [`ADR-03-codegen-boundary.md`](ADR-03-codegen-boundary.md) — Metadata / Codegen Boundary
- [`ADR-04-compile-runtime-split.md`](ADR-04-compile-runtime-split.md) — Compile-vs-Runtime Resolution Split (AOT lineage)
- [`ADR-05-container-shape.md`](ADR-05-container-shape.md) — Container Shape & the Provider Unification
- [`ADR-06-injection-mechanics.md`](ADR-06-injection-mechanics.md) — Injection / Resolution Mechanics & Injection-Point Shapes
- [`ADR-07-async-context-model.md`](ADR-07-async-context-model.md) — Async Execution & Context-Propagation Model
- [`ADR-08-proxy-substrate.md`](ADR-08-proxy-substrate.md) — Proxy / Interception Substrate (no CGLIB)
- [`ADR-09-cross-crate-discovery.md`](ADR-09-cross-crate-discovery.md) — Cross-Crate Discovery: Maximal-Magic + Anti-DCE + Static/Dynamic-WASM Duality
- [`ADR-10-conditional-strategy.md`](ADR-10-conditional-strategy.md) — Conditional Strategy + Profiles + Auto-Config Back-off
- [`ADR-11-autoconfig-starters-bom.md`](ADR-11-autoconfig-starters-bom.md) — Auto-Configuration Model + Starters + BOM + Distribution
- [`ADR-12-error-model.md`](ADR-12-error-model.md) — Unified Error / Diagnostics Model
