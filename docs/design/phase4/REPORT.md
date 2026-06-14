# Verdict

**CONVERGES — WITH LOOP-BACKS.** The Phase-3 redesigns genuinely re-unify onto the fixed Phase-2 toolkit; **zero loop-backs reach Phase 2.** Every primitive the stress-tests exercised (`ErasedBean`/`Ref`/`Published`, the one `Provider`/`Engine::create` driver, the dense-`BeanId` registry + stable `ContractId`, the `App<Define→Resolve→Wired→Running>` machine + `seal()` + one `AssemblyReport`, `CondExpr` tiers, the `Interceptor`-chain + delegating-newtype proxy, `Cx` ambient + `TeardownLedger`, one `cmp_order`, one `LeafError`) held up under maximally-adversarial compound scenarios. The breaks are **all at the seams between sibling Phase-3 subsystems** — exactly the class Phase 4 exists to catch.

The eight cross-cutting seams are pinned; ~40 raw stress-test breaks dedupe to **7 substantive clusters** addressed by 7 reconciliation decisions and **6 Phase-3 loop-backs** (all additive — no fixed ABI decision is reopened, only authored or threaded). The design is **implementation-ready strictly modulo those reconciliations**, and the readiness is *uneven*: a long tail of leaf crates is buildable now, but the `leaf-core` ABI and the `leaf-boot` template are **blocked** until the reconciliations land — and since everything hard-codes `::leaf_core` paths, that block gates the critical path.

The dominant gap is real and load-bearing: `13-container-lifecycle.md` is still the literal `x`-stub on disk while six subsystems depend on a contract that exists only in the seam-1 JSON. Two **genuinely new holes** were discovered that the Phase-3 docs never modelled at all — in-flight request-scope drain at shutdown, and the false claim that the events multicaster reuses the proxy `Interceptor` trait verbatim. Both are fixable in place but require ABI/template edits before `leaf-core` can freeze.

**Not implementation-ready as-is.** Ready after the listed Phase-3 reconciliations land, which require no Phase-2 reopening.

## Seams pinned

1. **lifecycle-template** — ONE `Context::refresh().await` (fixed-order R0–R8) + `Context::shutdown().await` (CAS close-once, stop_all DESC, LIFO ledger drain) owned by leaf-boot, plus ONE `watch<RunState>` phase cell `{Created,Refreshing,Running,Stopping,Closing,Closed,Failed}` orthogonal to the two availability cells; cancel-cascade is structural (`Failed` vs `Closed`). Fills the empty container-lifecycle subsystem.
2. **contractid-contract** — `ContractId(u64) = FNV-1a 64-bit` of the canonical path `crate::module_path::ident` (declared-name EXCLUDED); ONE `contract_hash(&str)` entry point; freeze-time collision is a HARD `AssemblyError::ContractIdCollision{id,a,b}` (never salt); the path is a documented semver surface.
3. **fallback-arithmetic** — Replace `is_primary`/`is_fallback` bools with a 2-axis `CandidateRole{primacy: Primacy, fallback: bool}`; `Selector::resolve_one` runs FallbackDemote→PrimaryPromote→len-rule in that fixed order; `would_resolve_unique` runs the identical fold so back-off and wiring never disagree; collections structurally bypass the layer.
4. **roletier-order-table** — Define `order::RoleTier` + the `cmp_chain` comparator (RoleTier-first) and pin the concrete tier+`order:i32` table in proxy-substrate for every infra concern AND every multicaster interceptor; property-tested against the correctness invariants (translation innermost, cache-outside-tx, AsyncDispatch-outside-ErrorIsolation).
5. **validation-strictness** — ONE `startup_validation: StartupValidation{Strict,Lenient,Skip}` field on `BootstrapSettings`, bound from `leaf.main.startup-validation`, read once at `validate()` head and threaded down; covers wiring + strict-placeholder + strict @Value; `Skip` legal only with a hash-matching `cargo leaf prepare` plan; wiring soundness stays hard even under `Lenient`.
6. **runtime-interpreter-audit** — CONFIRMED no surviving feature needs a runtime-authored expression interpreter; closure-only backend stands (`ValueExpr<T>`/`CondExprFn`/`KeyExprFn` over one `EvalCx` shape); adds ONE mandatory build-time `leaf doctor` check: `#{}` used but no `ExpressionEvaluator` linked = loud BUILD error.
7. **perf-escapes** — ONE `PerfClass{AbiReserved,GatedCodegen,DefaultOn}` policy gated by `leaf.perf.*` Environment keys; v1 ships all three micro-opts OFF (BeanRef/arena + ErasedArgs/ErasedRet ABI reserved-but-inert pre-1.0; skip-Cx and mono-proxy deferred behind benchmarks); App<Wired> validation is `DefaultOn`.
8. **typerow-proxy-policy** — ALL-DECLARED dyn-safe service-trait rows in `Descriptor.provides[]`, ONE proxy newtype implementing all those views, per-MethodKey dispatch through one shared chain, collection dedup-by-`BeanId`; emission set == proxy impl-set byte-for-byte via one shared `declared_dyn_views` generator; `#[provides(...)]` is the only narrowing opt-out.

## What the stress-tests found

Nine end-to-end scenarios were traced source→macro→linkme→`App<Define→Resolve→Wired>`→seal→refresh→steady-state→teardown. Two scenarios composed cleanly except for one blocker each; the rest surfaced multiple seam defects. The material breaks dedupe to **seven clusters**, each with a reconciliation:

- **C1 — container-lifecycle stub + validate-placement fork + anti-DCE 4-placement** (the dominant cluster, ~12 raw breaks). `13-container-lifecycle.md` is an empty `x`-stub while six subsystems point at it; `App<Wired>::validate()` is placed three different ways across docs; the anti-DCE self-check is specified at four mutually-incompatible points (a Define-edge placement *cannot* distinguish a DCE-vanished crate from an all-conditioned-out one — a false-positive `SourceVanished` for the conditioned-out metrics crate). **Reconciliation:** author the doc from seam-1; validate runs *before* refresh; R0 is assert-only; split the self-check into a cheap Define-edge anchor-presence gate + a post-condition R0 row-count reconcile.
- **C2 (+C8) — @Value coercion tier vs config-bind locus vs FallbackDemote-after-anti-DCE.** `@Value` coercion is *labelled* Tier-2 but actually runs only inside `Provider::provide` at refresh R5 (Tier-3), so in a 3-failure run it is masked and only surfaces on a *second* startup — defeating the "one aggregated report" headline. Config-properties bind locus is stated as both Tier-2-aggregated AND R5-constructed (cannot both be true). A truly-vanished user-Normal crate misclassified silent lets a library `@Fallback` win the contract — the exact silent-correctness break FallbackDemote's precondition exists to prevent. **Reconciliation:** two validate-time sub-passes (placeholder + a throwaway @Value dry-run, gated by the seam-5 lever); config-properties is the one bean class `validate()` pre-constructs with R5 publishing the pre-bound `Arc`; anti-DCE edge-anchor carries the full expected `ContractId` set + a `CFG_GATED_COMPONENTS` const; FallbackDemote runs strictly after R0.
- **C3 — scoped + advised double-proxy; `validate()` lacks `ProxyPlan` input.** A request-scoped advised bean fires both the `after_init` proxy-swap and the scoped `ScopeTarget` install — never reconciled, yielding double-advise or raw-handoff. The Selector cannot enforce the fixed "advised-injected-by-concrete-type" rejection because `is advised` lives in the frozen `ProxyPlan`, which is not a declared `validate()` input. **Reconciliation:** two-site install rule; make `ProxyPlan` an explicit `validate()` input + add the `AdvisedConcreteInjection` terminal check.
- **C4 — prototype-into-singleton silently effectively-singleton.** A by-value owned move of a prototype into a singleton field defeats the fresh-per-call contract with no diagnostic; the `ScopeMismatch` guard is worded for REQUEST scope only. **Reconciliation:** widen `ScopeMismatch` to *any* shorter-scoped target injected by value, with a doctor-warn.
- **C5 — events falsely claims verbatim proxy-`Interceptor` reuse (NEW hole).** A per-listener fan-out (async-dispatch, error-isolation) is *type-impossible* as a single `Call→ErasedRet` around-advice hop. @Async on a transactional listener has two candidate spawn sites and only the commit-time one is correct; events and declarative-advice state *opposite* condition-evaluation timing. **Reconciliation:** a second per-listener `DispatchInterceptor` shape (sharing only the comparator); `Arc` event capture for deferral; publish-time condition evaluation pinned. Pre-1.0 ABI item.
- **C6 — `RoleTier` named everywhere but never defined; `cmp_order` lacks the param.** The comparator documented to sort RoleTier-first literally cannot see the key. **Reconciliation:** define `order::RoleTier` + `cmp_chain`; pin the table in proxy-substrate. Pre-1.0 ABI item.
- **C7 — teardown has no in-flight request drain (NEW hole).** The pinned (C) teardown drains the *container* ledger over a quiesced graph but never observes the *per-request* ledgers that live in the ambient `Cx`; an open `@Transactional` tx has no ledger to roll back on, falsifying "no async Drop"; singleton destroyers fire `close()` under a straggler's live `Arc` (memory-free ≠ logical-destroy); `shutdown()`-during-`refresh()` is undefined and reachable via Background beans. **Reconciliation:** add an explicit graceful-request-drain step (intake-stop → bounded grace → cooperative cancel → per-request-ledger drain) before `stop_all`; disarm scheduler early; route `Refreshing`-during-shutdown to the (B) cancel cascade.

The seams the docs were *designed* for held cleanly: FallbackDemote (`{Normal_A, Normal_B, Fallback_C}` → loud `NoUniqueBean`; user beats starter-Fallback), the anti-DCE 4-class distinction (genuine `SourceVanished` loud vs conditioned-out `Negative` silent), ContractId derivation, the LazyRef cycle-break, and collection-injection-over-supertype dedup all walked end-to-end without defect.

## Loop-backs

**Six loop-backs, all to Phase 3. Zero to Phase 2** — every break reconciles onto the fixed toolkit; no toolkit primitive needs to change.

1. **C1 — author `13-container-lifecycle.md`** from seam-1 (stub; ~12 breaks; root of the validate-placement and anti-DCE-timing forks).
2. **C7 — teardown drain redesign** (genuinely new design; needs a request-scope-registry seam co-owned by bean-lifecycle + leaf-tx + execution-context).
3. **C3 — proxy two-site install** (double-proxy; `ProxyPlan` must become a `validate()` input).
4. **C5 — events `DispatchInterceptor`** (type-impossibility; touches pre-1.0 ABI).
5. **C4 — widen `ScopeMismatch`** (silent-wrong-instance class).
6. **C6 — define `RoleTier` + `cmp_chain`** (missing artifact the comparator already references; pre-1.0 ABI).

The remaining stress-test findings (RunState-vs-availability wording contradictions, the `enum leaf.main.startup-validation` vs `bool leaf.validation.strict` shape inconsistency *within the seam JSON itself*, under-specified cross-crate `NoUniqueBean` enrichment, the "init HOLDS NO GUARD" mis-wording, the strict-vs-lenient @Value default contradiction across docs 06/07) are **friction-grade, fixable in place** during the same Phase-3 doc edits.

## Implementation readiness by crate

Grouped kernel → framework → concerns → integrations → boot/starters/umbrella; **build order is top-to-bottom (kernel-first, dependency-respecting)**, matching the acyclic inward topology.

| # | Layer | Crate(s) | Readiness | Blocking work |
|---|---|---|---|---|
| 1 | **Kernel** | `leaf-core` | **BLOCKED** | ABI freeze gated on C5 (`DispatchInterceptor`), C6 (`RoleTier`/`cmp_chain`), C2 (`CFG_GATED_COMPONENTS`/anchor const), perf-escape reserved-but-inert shapes (`BeanRef`/arena, `ErasedArgs`/`ErasedRet`), seam-1 `RunState` variants |
| 2 | **Codegen** | `leaf-macros`, `leaf-codegen` | ready-after-reconciliation | C2 (canonical-path builder + dry-run hooks); `declared_dyn_views` shared generator (seam-8) |
| 2 | **Framework/conditions/config** | `leaf-conditions`, `leaf-config` | ready-after-reconciliation | C2 (validate-time sub-passes + anti-DCE edge-anchor) |
| 3 | **Boot** | `leaf-boot` | **BLOCKED** | C1 (author the refresh/teardown template) + C7 (in-flight drain redesign) — the highest-leverage gap |
| 3 | **Runtime** | `leaf-tokio`, `leaf-smol` | ready-after-reconciliation | C7 (ShutdownTrigger + request-drain plumbing), C2 |
| 4 | **Cross-cutting concerns** | `leaf-tx` | **BLOCKED** | C7 (tx finalization on the per-request ledger) + C5 (transactional_event_listener spawn-site) |
| 4 | | `leaf-cache`, `leaf-validation`, `leaf-resilience` | ready-after-reconciliation | C6 (consume the pinned `RoleTier` table) |
| 5 | **Integrations** | `leaf-redis` (+ representative pattern), `leaf` umbrella | ready-after-reconciliation | C7/C2 (consume the templates) |
| 5 | | `leaf-starter-web` | ready-after-reconciliation | C7 (web intake-stop) |
| 6 | **Ready now (no reconciliation)** | `leaf-figlet`, `leaf-cron`, `leaf-serde`, `leaf-i18n`, `leaf-aop-expr`, `leaf-starter-redis` | **ready** | none — self-contained or pure data/grammar; writing-plans can start immediately |

**Recommended sequencing:** write `leaf-core` last-among-kernel (it must absorb all six reconciliations first), but begin the **ready-now leaf crates in parallel today** since they only depend on a `leaf-core` ABI surface the reconciliations *extend* rather than *break*. Critical path = C1 + C7 + C6 + C5 → `leaf-core` freeze → `leaf-boot` → everything else.

## What's next

Hand off to per-crate **writing-plans** in this order:

1. **Immediately:** the six **ready** crates (`leaf-figlet`, `leaf-cron`, `leaf-serde`, `leaf-i18n`, `leaf-aop-expr`, `leaf-starter-redis`) — no reconciliation dependency.
2. **First Phase-3 loop-back batch (unblocks the kernel):** author `13-container-lifecycle.md` (C1) and the teardown-drain redesign (C7) together — they share the request-scope-ledger seam; then C5/C6/C3/C4 as ABI edits. Land all six before drafting the `leaf-core` writing-plan.
3. **Then:** `leaf-core` → `leaf-macros`/`leaf-codegen`/`leaf-conditions`/`leaf-config` → `leaf-boot` → runtime/concern/integration crates → umbrella.

**Residual risk (be honest):**
- The two NEW holes (C7 in-flight drain, C5 events interceptor shape) are *redesigns*, not edits — they could surface secondary seams when authored, especially where request-scope teardown, tx rollback, and structured-concurrency cancellation interleave. C7 in particular touches background-bootstrap + structured concurrency + scope teardown + the async fence simultaneously (COHERENCE flagged this exact intersection as the hardest item).
- Pre-1.0 ABI items still genuinely unresolved per the convergence summary: the `ErasedArgs`/`ErasedRet` pack/unpack ABI shape must be frozen before `leaf-core` 1.0 but is not yet pinned in detail; cron DST handling; the collection-owner test + `OriginId` artifact; `Skip`-gating details; and group-sequences dropped for v1. None block v1 *correctness*, but the `ErasedArgs`/`ErasedRet` freeze is on the kernel critical path.
- The seam decisions themselves still carry one internal inconsistency (the strictness-lever key/shape stated two ways) that must be resolved during the C-cluster doc edits, or it will propagate into `leaf-config` + `leaf-boot` as a divergent config key.
