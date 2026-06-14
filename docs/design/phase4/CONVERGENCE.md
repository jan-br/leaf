# Phase 4 — Convergence

**Verdict:** CONVERGES-WITH-LOOPBACKS. Zero phase2 loopbacks; all breaks reconcile onto the fixed toolkit. ~40 breaks dedupe to 7 substantive clusters; 6 phase3 loopbacks. Dominant: 13-container-lifecycle.md stub vs seam#1, six divergent consumers (~12 breaks). Two NEW holes: in-flight-request drain + per-request-ledger tx finalization; events false verbatim proxy-Interceptor reuse. Hold leaf-boot + leaf-core proxy/events/order ABI + advice crates; leaf-figlet/cron/serde/i18n/aop-expr/starter-redis ready now.</convergenceVerdict>
</invoke>


## Reconciliations

- **C1 container-lifecycle stub; validate-inside-vs-before-refresh + anti-DCE 4-placement.** → Author doc from seam#1; validate before refresh, R0 assert-only. _(affects: leaf-boot, leaf-core, six docs)_
- **C2(+C8) @Value coercion Tier-2 but resolve_one skips; config bind at R5; anti-DCE edge vs R0; lone @Fallback wins on vanish.** → Two validate-time sub-passes + anti-DCE edge-anchor + R0 reconcile; FallbackDemote after R0. _(affects: leaf-boot, leaf-core, binding-conversion, environment-config, discovery-codegen, conditions-autoconfig)_
- **C3 scoped+advised double-proxy; validate lacks ProxyPlan input.** → Two-site install; ProxyPlan a validate input + AdvisedConcreteInjection. _(affects: leaf-core, proxy, injection, bean-lifecycle)_
- **C4 prototype-into-singleton effectively-singleton; ScopeMismatch request-only.** → Widen ScopeMismatch to any shorter-scoped value injection; doctor-warn. _(affects: leaf-core, injection, bean-lifecycle)_
- **C5 events claims verbatim proxy-Interceptor reuse; type-false.** → Second per-listener DispatchInterceptor; Arc event; publish-time condition. _(affects: leaf-core, events, advice, proxy)_
- **C6 RoleTier named everywhere, no enum, cmp_order lacks param.** → Define order::RoleTier + cmp_chain; pin table in proxy-substrate. _(affects: leaf-core, proxy, advice, events, exec-context, injection)_
- **C7 NEW teardown lacks in-flight request drain; tx undrained; Refreshing-x-shutdown undefined.** → In-flight drain before stop_all + per-request ledger drain; scheduler disarm early; Refreshing-flag->B. _(affects: leaf-boot, leaf-core, leaf-tx, exec-context, advice, events)_

## Loop-backs needed

- **→ phase3**: C1 author container-lifecycle doc. — stub; 12 breaks
- **→ phase3**: C7 teardown drain redesign. — new design; needs request-scope registry seam
- **→ phase3**: C3 proxy two-site install. — double-proxy; no ProxyPlan input
- **→ phase3**: C5 events DispatchInterceptor. — type-impossibility; pre-1.0 ABI
- **→ phase3**: C4 widen ScopeMismatch. — silent-wrong-instance
- **→ phase3**: C6 define RoleTier+cmp_chain. — missing-artifact; pre-1.0 ABI

## Unresolved
- ErasedArgs/ErasedRet ABI pre-1.0.
- Cron DST.
- collection-owner test + OriginId artifact.
- Skip gating; group-sequences dropped v1.

## Per-crate readiness

- **leaf-core** — `blocked` — ABI
- **leaf-boot** — `blocked` — template+drain
- **leaf-tx** — `blocked` — C7+C5
- **leaf-macros, leaf-codegen, leaf-config, leaf-conditions** — `ready-after-reconciliation` — C2
- **leaf-tokio, leaf-smol, leaf-starter-web, leaf-redis, leaf** — `ready-after-reconciliation` — C7/C2
- **leaf-cache, leaf-validation, leaf-resilience** — `ready-after-reconciliation` — C6
- **leaf-aop-expr, leaf-cron, leaf-i18n, leaf-serde, leaf-figlet, leaf-starter-redis** — `ready` — none
