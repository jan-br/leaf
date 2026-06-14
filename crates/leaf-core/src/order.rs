//! `leaf_core::order` — THE single ordering law: [`cmp_order`] (pure) and the
//! RoleTier-first correctness composite [`cmp_chain`] (SEAMS C6).
//!
//! There is exactly ONE ordering comparator family in leaf, so no subsystem
//! defines its own and the five+ ordering call sites can never drift:
//!
//! - [`cmp_order`] is the PURE comparator over an [`OrderKey`] (`{value, source}`).
//!   Lower `value` wins; at equal value an [`OrderSource::Interface`] source
//!   beats [`OrderSource::Annotation`] which beats [`OrderSource::Implicit`]
//!   (interface-declared ordering is more specific than an annotation, which is
//!   more specific than a defaulted implicit order). It is consumed by
//!   `PriorityRank`, collection/map injection ordering, `Lookup::ordered_stream`,
//!   and runtime-lifecycle integer-`Phase` ordering — none of which are
//!   RoleTier-graded, so `cmp_order` STAYS pure (SEAMS C6 keeps it unchanged).
//!
//! - [`cmp_chain`] is the correctness-grade composite over a [`ChainKey`]
//!   (`{tier, order, id}`): [`RoleTier`] first (ascending = outermost-first;
//!   `Infrastructure=0` wraps application advice), THEN [`cmp_order`], THEN the
//!   stable [`ContractId`] FNV-1a tie-break (SEAMS seam #2). It is the ONE
//!   comparator the advisor chain (`ProxyPlan::freeze`), the multicaster
//!   pipeline, and refresh R2/R3/R6 Infrastructure ordering all call.
//!
//! The fixed `*_ORDER` `i32` const table is the single source of truth for the
//! built-in advice and multicaster chains (each per-concern `@Order` value is
//! set from these consts); the property tests below pin the load-bearing
//! invariants (`CACHE_ORDER < TX_ORDER`, `TRANSLATE_ORDER` is the innermost
//! infrastructure advisor, `ASYNC_DISPATCH_ORDER < ERROR_ISOLATION_ORDER`).

use std::cmp::Ordering;

use crate::identity::ContractId;

/// The default integer order when no explicit `@Order`/`@Priority` is declared
/// (Spring's `Ordered.LOWEST_PRECEDENCE` intent is modeled via `Implicit`
/// source rather than a magic int, so the default value is a plain `0`).
pub const DEFAULT_ORDER: i32 = 0;

/// Provenance of an [`OrderKey`] — the equal-`value` tie-break dimension.
///
/// More specific sources win at equal numeric value: an `Ordered`-trait
/// (interface) declaration is more specific than an `#[order]`/`#[priority]`
/// annotation, which is more specific than a defaulted implicit order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum OrderSource {
    /// Declared by implementing the ordering interface (`Ordered`) — most specific.
    Interface,
    /// Declared by an `#[order]` / `#[priority]` annotation.
    Annotation,
    /// No explicit order; a defaulted/implicit value — least specific.
    Implicit,
}

impl OrderSource {
    /// The tie-break rank (lower = wins at equal numeric value).
    ///
    /// `Interface(0) < Annotation(1) < Implicit(2)`.
    #[must_use]
    const fn rank(self) -> u8 {
        match self {
            OrderSource::Interface => 0,
            OrderSource::Annotation => 1,
            OrderSource::Implicit => 2,
        }
    }
}

/// The orderable key for one bean / advisor / lifecycle participant.
///
/// `value` is the integer precedence (lower wins). `source` is the equal-value
/// tie-break provenance (see [`OrderSource`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct OrderKey {
    /// Integer precedence; LOWER wins (Spring's ascending `@Order`).
    pub value: i32,
    /// Provenance, breaking ties at equal `value`.
    pub source: OrderSource,
}

impl OrderKey {
    /// An order key with the [`DEFAULT_ORDER`] value and an implicit source.
    #[must_use]
    pub const fn implicit() -> Self {
        OrderKey { value: DEFAULT_ORDER, source: OrderSource::Implicit }
    }
}

impl Default for OrderKey {
    fn default() -> Self {
        OrderKey::implicit()
    }
}

/// THE single pure comparator: lower `value` wins; at equal value a more
/// specific [`OrderSource`] wins (`Interface < Annotation < Implicit`).
///
/// Pure and total over [`OrderKey`] — RoleTier-blind by design (SEAMS C6 keeps
/// it unchanged for the non-tiered sites: `PriorityRank`, collection/map
/// ordering, `Lookup::ordered_stream`, runtime-lifecycle phases).
#[must_use]
pub fn cmp_order(a: &OrderKey, b: &OrderKey) -> Ordering {
    a.value
        .cmp(&b.value)
        .then_with(|| a.source.rank().cmp(&b.source.rank()))
}

/// The framework-vs-application ordering tier (SEAMS C6).
///
/// ASCENDING = OUTERMOST-first: `Infrastructure = 0` is the outermost advisor
/// (framework concerns wrap application advice), `Application = 2` is innermost.
/// `#[repr(u8)]` with the explicit discriminants so the derived `Ord` IS the
/// outermost-first order and matches the wire/diagnostic rendering.
///
/// There is exactly one role taxonomy in the kernel: the metamodel-side
/// [`Role`](crate::Role) maps totally onto this ordering-side tier via
/// [`RoleTier::of`] (SEAMS C6), so the advisor-chain sort reads role provenance
/// without forking a second enum.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum RoleTier {
    /// Framework infrastructure — outermost (wraps everything else).
    Infrastructure = 0,
    /// Support concerns — middle tier.
    Support = 1,
    /// Application beans — innermost.
    Application = 2,
}

impl RoleTier {
    /// The total map from the metamodel [`Role`](crate::Role) to its ordering
    /// tier (SEAMS C6) — the single bridge between the two role surfaces.
    ///
    /// `const` so the const advisor/chain tables can grade by tier at compile
    /// time. A closed match over the three `Role` variants keeps it total by
    /// construction.
    #[must_use]
    pub const fn of(role: crate::definition::Role) -> RoleTier {
        match role {
            crate::definition::Role::Infrastructure => RoleTier::Infrastructure,
            crate::definition::Role::Support => RoleTier::Support,
            crate::definition::Role::Application => RoleTier::Application,
        }
    }
}

/// The correctness-grade composite key: [`RoleTier`] first, then an
/// [`OrderKey`], then the stable [`ContractId`] tie-break (SEAMS C6 / seam #2).
///
/// `AdvisorRef::chain_key()` (proxy-substrate) builds this over its existing
/// `{id, order, role}` fields; `ProxyPlan::freeze`'s documented
/// `(RoleTier, cmp_order, ContractId)` sort IS [`cmp_chain`] over this key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ChainKey {
    /// Primary key: the framework-vs-application tier (outermost-first).
    pub tier: RoleTier,
    /// Secondary key: the pure [`OrderKey`].
    pub order: OrderKey,
    /// Final stable tie-break: the cross-build [`ContractId`].
    pub id: ContractId,
}

/// THE one correctness-grade composite comparator (SEAMS C6).
///
/// `tier` (outermost-first) THEN [`cmp_order`] THEN the [`ContractId`] FNV-1a
/// tie-break (seam #2) — a total, deterministic, cross-build-stable order. Used
/// at the advisor-chain sort (`ProxyPlan::freeze`), the multicaster pipeline
/// sort, and refresh R2/R3/R6 Infrastructure ordering.
#[must_use]
pub fn cmp_chain(a: &ChainKey, b: &ChainKey) -> Ordering {
    a.tier
        .cmp(&b.tier)
        .then_with(|| cmp_order(&a.order, &b.order))
        .then_with(|| a.id.0.cmp(&b.id.0))
}

// ── the fixed `*_ORDER` i32 table (SEAMS C6, single source of truth) ─────────
//
// Advice chain (Infrastructure tier), canonical order
// VALIDATE → RETRY → @ASYNC → CACHE → TX → CONCURRENCY → TRANSLATE:

/// Bean-validation advice order (outermost user-facing concern).
pub const VALIDATE_ORDER: i32 = 100;
/// Retry/resilience advice order.
pub const RETRY_ORDER: i32 = 200;
/// `@Async` dispatch advice order.
pub const ASYNC_ORDER: i32 = 300;
/// Cache advice order — INTENTIONALLY `< TX_ORDER` so a cache hit avoids
/// opening a transaction (the cache-outside-tx correctness invariant).
pub const CACHE_ORDER: i32 = 400;
/// Transaction advice order.
pub const TX_ORDER: i32 = 500;
/// Structured-concurrency advice order.
pub const CONCURRENCY_ORDER: i32 = 550;
/// Exception-translation advice order — the INNERMOST infrastructure advisor.
pub const TRANSLATE_ORDER: i32 = 600;

// Multicaster pipeline (event dispatch interceptors),
// ASYNC_DISPATCH → ERROR_ISOLATION → CONTEXT_PROP → METRICS:

/// Async-dispatch multicaster order (outermost) — `< ERROR_ISOLATION_ORDER`.
pub const ASYNC_DISPATCH_ORDER: i32 = 100;
/// Error-isolation multicaster order.
pub const ERROR_ISOLATION_ORDER: i32 = 200;
/// Context-propagation multicaster order.
pub const CONTEXT_PROP_ORDER: i32 = 300;
/// Metrics multicaster order (innermost).
pub const METRICS_ORDER: i32 = 400;

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: i32, source: OrderSource) -> OrderKey {
        OrderKey { value, source }
    }

    // ── cmp_order ────────────────────────────────────────────────────────────

    #[test]
    fn cmp_order_lower_value_wins() {
        let lo = key(10, OrderSource::Implicit);
        let hi = key(20, OrderSource::Implicit);
        assert_eq!(cmp_order(&lo, &hi), Ordering::Less);
        assert_eq!(cmp_order(&hi, &lo), Ordering::Greater);
        // Negative orders sort before positive (ascending integer precedence).
        assert_eq!(
            cmp_order(&key(-1, OrderSource::Implicit), &key(0, OrderSource::Implicit)),
            Ordering::Less
        );
    }

    #[test]
    fn cmp_order_interface_beats_annotation_beats_implicit_at_equal_value() {
        let iface = key(0, OrderSource::Interface);
        let anno = key(0, OrderSource::Annotation);
        let implicit = key(0, OrderSource::Implicit);
        assert_eq!(cmp_order(&iface, &anno), Ordering::Less);
        assert_eq!(cmp_order(&anno, &implicit), Ordering::Less);
        assert_eq!(cmp_order(&iface, &implicit), Ordering::Less);
        // Symmetry.
        assert_eq!(cmp_order(&anno, &iface), Ordering::Greater);
    }

    #[test]
    fn cmp_order_value_dominates_source() {
        // A lower value with the weakest source STILL beats a higher value with
        // the strongest source: value is primary, source only breaks ties.
        let strong_but_high = key(5, OrderSource::Interface);
        let weak_but_low = key(1, OrderSource::Implicit);
        assert_eq!(cmp_order(&weak_but_low, &strong_but_high), Ordering::Less);
    }

    #[test]
    fn cmp_order_equal_keys_are_equal() {
        let a = key(7, OrderSource::Annotation);
        let b = key(7, OrderSource::Annotation);
        assert_eq!(cmp_order(&a, &b), Ordering::Equal);
    }

    #[test]
    fn cmp_order_sorts_a_slice_deterministically() {
        let mut v = vec![
            key(2, OrderSource::Implicit),
            key(1, OrderSource::Implicit),
            key(1, OrderSource::Interface),
            key(1, OrderSource::Annotation),
        ];
        v.sort_by(cmp_order);
        assert_eq!(
            v,
            vec![
                key(1, OrderSource::Interface),
                key(1, OrderSource::Annotation),
                key(1, OrderSource::Implicit),
                key(2, OrderSource::Implicit),
            ]
        );
    }

    // ── RoleTier ─────────────────────────────────────────────────────────────

    #[test]
    fn role_tier_is_outermost_first_ascending() {
        assert!(RoleTier::Infrastructure < RoleTier::Support);
        assert!(RoleTier::Support < RoleTier::Application);
        // Explicit discriminants are the wire/diagnostic contract.
        assert_eq!(RoleTier::Infrastructure as u8, 0);
        assert_eq!(RoleTier::Support as u8, 1);
        assert_eq!(RoleTier::Application as u8, 2);
    }

    #[test]
    fn role_tier_of_is_the_total_map_from_role() {
        use crate::definition::Role;
        // The single bridge between the metamodel Role and the ordering tier
        // (SEAMS C6): Infrastructure is outermost, Application innermost.
        assert_eq!(RoleTier::of(Role::Infrastructure), RoleTier::Infrastructure);
        assert_eq!(RoleTier::of(Role::Support), RoleTier::Support);
        assert_eq!(RoleTier::of(Role::Application), RoleTier::Application);
        // const-evaluable (const advisor/chain tables grade by tier at compile time).
        const T: RoleTier = RoleTier::of(Role::Infrastructure);
        assert_eq!(T, RoleTier::Infrastructure);
        // Ordering is preserved: a default (Application) bean sorts innermost.
        assert!(RoleTier::of(Role::Infrastructure) < RoleTier::of(Role::Application));
    }

    // ── cmp_chain ────────────────────────────────────────────────────────────

    fn chain(tier: RoleTier, value: i32, source: OrderSource, id: &str) -> ChainKey {
        ChainKey { tier, order: key(value, source), id: ContractId::of(id) }
    }

    #[test]
    fn cmp_chain_tier_dominates_everything() {
        // An Infrastructure key with a HUGE order still beats an Application key
        // with the smallest order — tier is the primary key.
        let infra = chain(RoleTier::Infrastructure, 9999, OrderSource::Implicit, "a");
        let app = chain(RoleTier::Application, -9999, OrderSource::Interface, "z");
        assert_eq!(cmp_chain(&infra, &app), Ordering::Less);
    }

    #[test]
    fn cmp_chain_falls_back_to_cmp_order_within_a_tier() {
        let lo = chain(RoleTier::Support, 1, OrderSource::Implicit, "a");
        let hi = chain(RoleTier::Support, 2, OrderSource::Implicit, "a");
        assert_eq!(cmp_chain(&lo, &hi), Ordering::Less);
        // Equal value within a tier -> source tie-break (Interface < Annotation).
        let iface = chain(RoleTier::Support, 1, OrderSource::Interface, "a");
        let anno = chain(RoleTier::Support, 1, OrderSource::Annotation, "a");
        assert_eq!(cmp_chain(&iface, &anno), Ordering::Less);
    }

    #[test]
    fn cmp_chain_final_tiebreak_is_stable_contract_id() {
        // Same tier AND same OrderKey -> the ContractId FNV-1a value decides,
        // deterministically and reproducibly (seam #2).
        let a = chain(RoleTier::Application, 0, OrderSource::Implicit, "crate::Aaa");
        let b = chain(RoleTier::Application, 0, OrderSource::Implicit, "crate::Bbb");
        let expected = ContractId::of("crate::Aaa").0.cmp(&ContractId::of("crate::Bbb").0);
        assert_eq!(cmp_chain(&a, &b), expected);
        assert_ne!(cmp_chain(&a, &b), Ordering::Equal, "distinct ids must not tie");
        // Identical keys tie.
        assert_eq!(cmp_chain(&a, &a), Ordering::Equal);
    }

    #[test]
    fn cmp_chain_sorts_a_mixed_chain_outermost_first() {
        let mut v = vec![
            chain(RoleTier::Application, 0, OrderSource::Implicit, "app"),
            chain(RoleTier::Infrastructure, TX_ORDER, OrderSource::Annotation, "tx"),
            chain(RoleTier::Infrastructure, CACHE_ORDER, OrderSource::Annotation, "cache"),
            chain(RoleTier::Support, 0, OrderSource::Implicit, "support"),
        ];
        v.sort_by(cmp_chain);
        let ids: Vec<RoleTier> = v.iter().map(|c| c.tier).collect();
        assert_eq!(
            ids,
            vec![
                RoleTier::Infrastructure, // cache (CACHE_ORDER < TX_ORDER)
                RoleTier::Infrastructure, // tx
                RoleTier::Support,
                RoleTier::Application,
            ]
        );
        // Within Infrastructure, cache (400) sorts before tx (500).
        assert_eq!(v[0].order.value, CACHE_ORDER);
        assert_eq!(v[1].order.value, TX_ORDER);
    }

    // ── the fixed *_ORDER table (SEAMS C6 property pins) ──────────────────────

    #[test]
    fn cache_order_is_strictly_less_than_tx_order() {
        // A cache hit must be able to short-circuit BEFORE a tx is opened.
        assert!(CACHE_ORDER < TX_ORDER, "cache must wrap (sit outside) tx");
    }

    #[test]
    fn translate_order_is_the_innermost_infrastructure_advisor() {
        let infra = [
            VALIDATE_ORDER,
            RETRY_ORDER,
            ASYNC_ORDER,
            CACHE_ORDER,
            TX_ORDER,
            CONCURRENCY_ORDER,
            TRANSLATE_ORDER,
        ];
        let max = infra.iter().copied().max().unwrap();
        assert_eq!(TRANSLATE_ORDER, max, "translate must be innermost (max order)");
    }

    #[test]
    fn canonical_advice_chain_is_validate_retry_async_cache_tx_concurrency_translate() {
        let mut v = vec![
            ("translate", TRANSLATE_ORDER),
            ("tx", TX_ORDER),
            ("cache", CACHE_ORDER),
            ("async", ASYNC_ORDER),
            ("retry", RETRY_ORDER),
            ("validate", VALIDATE_ORDER),
            ("concurrency", CONCURRENCY_ORDER),
        ];
        v.sort_by_key(|(_, o)| *o);
        let names: Vec<&str> = v.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec!["validate", "retry", "async", "cache", "tx", "concurrency", "translate"]
        );
    }

    #[test]
    fn async_dispatch_order_is_strictly_less_than_error_isolation_order() {
        assert!(ASYNC_DISPATCH_ORDER < ERROR_ISOLATION_ORDER);
    }

    #[test]
    fn canonical_multicaster_pipeline_order() {
        let mut v = vec![
            ("metrics", METRICS_ORDER),
            ("context_prop", CONTEXT_PROP_ORDER),
            ("error_isolation", ERROR_ISOLATION_ORDER),
            ("async_dispatch", ASYNC_DISPATCH_ORDER),
        ];
        v.sort_by_key(|(_, o)| *o);
        let names: Vec<&str> = v.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec!["async_dispatch", "error_isolation", "context_prop", "metrics"]
        );
    }
}
