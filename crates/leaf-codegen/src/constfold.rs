//! The build-time `CondExpr` ConstFold folder + the deferred auto-config /
//! ordering plan (conditions-autoconfig phase3/05; the opt-in `cargo leaf
//! prepare` accelerator, discovery-codegen phase3/02).
//!
//! Two related build-time concerns, both pure + unit-testable:
//!
//! 1. **[`fold`] — the ConstFold pass.** A codegen-side mirror of the frozen
//!    [`leaf_core::CondExpr`] algebra ([`CondTree`]) that the `#[conditional(...)]`
//!    macro builds from parsed syntax. `fold` collapses constant sub-trees at
//!    BUILD time so the emitted const tree is minimal and the
//!    [`EarliestTier::ConstFold`](leaf_core::EarliestTier) leaves the design
//!    mandates actually arrive as [`CondExpr::Const`](leaf_core::CondExpr). The boolean-algebra
//!    rewrites are the standard short-circuits: `All` with any `false` child →
//!    `Const(false)`; `Any` with any `true` child → `Const(true)`;
//!    `Not(Const(b)) → Const(!b)`; an empty `All`/`Any` → its identity. Then
//!    [`emit`] lowers the folded tree to a const `::leaf_core::CondExpr`
//!    expression via ABSOLUTE paths.
//! 2. **[`AutoConfigPlan`] — the deferred ordering plan.** A const-emittable,
//!    deterministically-ordered plan over the auto-config set keyed on the stable
//!    [`leaf_core::ContractId`] — the artifact `cargo leaf prepare` would
//!    checkpoint so the runtime fold is skipped. Ordering is computed HERE from
//!    declared `before`/`after` edges + a tie-break on the stable contract id,
//!    NEVER from link order (which is unspecified). This is the codegen half; the
//!    runtime `run_autoconfig` driver lives in leaf-boot (a `// NOTE` boundary).

use std::collections::BTreeMap;

use proc_macro2::TokenStream;
use quote::quote;

/// A codegen-side mirror of the frozen [`leaf_core::CondExpr`] algebra: the tree
/// the `#[conditional(...)]` macro builds from parsed syntax, foldable + lowerable
/// to a const `::leaf_core::CondExpr` expression.
///
/// `Leaf` carries the rendered `ConditionId` expression (an opaque `u32` token
/// path the macro computed from the kind catalog) plus the rendered attr-slice
/// expression — both as already-built [`TokenStream`]s, because the codegen
/// folder reasons only about the BOOLEAN structure, never the leaf internals. The
/// folder never invents a leaf value; it only prunes/collapses around `Const`s.
#[derive(Clone, Debug)]
pub enum CondTree {
    /// A catalog-member leaf: the rendered `ConditionId` + the rendered attrs.
    /// Opaque to the folder (it is never `Const`-foldable).
    Leaf {
        /// The rendered `::leaf_core::ConditionId(..)` expression.
        id: TokenStream,
        /// The rendered `&[::leaf_core::Attr]` expression.
        attrs: TokenStream,
    },
    /// Conjunction.
    All(Vec<CondTree>),
    /// Disjunction.
    Any(Vec<CondTree>),
    /// Negation.
    Not(Box<CondTree>),
    /// A build-folded constant.
    Const(bool),
}

impl CondTree {
    /// `true` iff this node is a `Const`.
    #[must_use]
    pub fn is_const(&self) -> bool {
        matches!(self, CondTree::Const(_))
    }

    /// The constant value iff this node is a `Const`.
    #[must_use]
    pub fn as_const(&self) -> Option<bool> {
        match self {
            CondTree::Const(b) => Some(*b),
            _ => None,
        }
    }
}

/// Const-fold a [`CondTree`] at BUILD time, collapsing constant sub-trees by the
/// standard boolean short-circuits so the emitted const tree is minimal.
///
/// Rewrites (applied bottom-up):
/// - `Not(Const(b))` → `Const(!b)`.
/// - `All([..])`: drop `Const(true)` children; any `Const(false)` child →
///   `Const(false)`; an empty result → `Const(true)` (the vacuous identity);
///   a single survivor unwraps.
/// - `Any([..])`: drop `Const(false)` children; any `Const(true)` child →
///   `Const(true)`; an empty result → `Const(false)` (the vacuous identity);
///   a single survivor unwraps.
/// - A `Leaf` is opaque (never folds — its verdict is only known at its tier).
///
/// This is the codegen realization of the `ConstFold` tier: a build-decidable
/// leaf the macro already lowered to `Const(b)` propagates through here so the
/// whole guard can collapse, matching [`CondExpr::tier`](leaf_core::CondExpr)
/// reading `ConstFold` for a folded constant.
#[must_use]
pub fn fold(tree: &CondTree) -> CondTree {
    match tree {
        CondTree::Leaf { id, attrs } => CondTree::Leaf {
            id: id.clone(),
            attrs: attrs.clone(),
        },
        CondTree::Const(b) => CondTree::Const(*b),
        CondTree::Not(inner) => {
            let folded = fold(inner);
            match folded.as_const() {
                Some(b) => CondTree::Const(!b),
                None => CondTree::Not(Box::new(folded)),
            }
        }
        CondTree::All(children) => {
            let mut survivors = Vec::new();
            for child in children {
                let folded = fold(child);
                match folded.as_const() {
                    Some(true) => {} // identity: drop a vacuously-true child
                    Some(false) => return CondTree::Const(false), // annihilator
                    None => survivors.push(folded),
                }
            }
            collapse(survivors, true, CondTree::All)
        }
        CondTree::Any(children) => {
            let mut survivors = Vec::new();
            for child in children {
                let folded = fold(child);
                match folded.as_const() {
                    Some(false) => {} // identity: drop a vacuously-false child
                    Some(true) => return CondTree::Const(true), // annihilator
                    None => survivors.push(folded),
                }
            }
            collapse(survivors, false, CondTree::Any)
        }
    }
}

/// Collapse a folded child list: an empty list → the `identity` const; a single
/// survivor unwraps to itself; otherwise rebuild the combinator.
fn collapse(
    mut survivors: Vec<CondTree>,
    identity: bool,
    build: fn(Vec<CondTree>) -> CondTree,
) -> CondTree {
    match survivors.len() {
        0 => CondTree::Const(identity),
        1 => survivors.pop().expect("len checked == 1"),
        _ => build(survivors),
    }
}

/// Lower a [`CondTree`] to a const `::leaf_core::CondExpr` expression via ABSOLUTE
/// paths (the macro drops it onto `Descriptor.meta`'s guard).
///
/// `&'static`-shaped: `All`/`Any` children lower to a `&[..]` slice literal and
/// `Not`'s child to a `&` reference, matching the frozen
/// `CondExpr::{All,Any}(&'static [CondExpr])` / `Not(&'static CondExpr)` shapes.
/// Callers typically [`fold`] first so the emitted tree is minimal.
#[must_use]
pub fn emit(tree: &CondTree) -> TokenStream {
    match tree {
        CondTree::Const(b) => quote! { ::leaf_core::CondExpr::Const(#b) },
        CondTree::Leaf { id, attrs } => quote! { ::leaf_core::CondExpr::Leaf(#id, #attrs) },
        CondTree::Not(inner) => {
            let child = emit(inner);
            quote! { ::leaf_core::CondExpr::Not(&#child) }
        }
        CondTree::All(children) => {
            let rows = children.iter().map(emit);
            quote! { ::leaf_core::CondExpr::All(&[ #(#rows),* ]) }
        }
        CondTree::Any(children) => {
            let rows = children.iter().map(emit);
            quote! { ::leaf_core::CondExpr::Any(&[ #(#rows),* ]) }
        }
    }
}

/// Fold then emit — the one entry point the `#[conditional(...)]` macro calls.
#[must_use]
pub fn fold_and_emit(tree: &CondTree) -> TokenStream {
    emit(&fold(tree))
}

// ───────────────────── the deferred auto-config ordering plan ────────────────

/// One auto-config entry in the deferred ordering plan: a stable identity string
/// (the `crate::Module::Ident` path that mints the `ContractId`) + its declared
/// ordering edges.
///
/// `before`/`after` are the `@AutoConfigureBefore`/`@AutoConfigureAfter` edges
/// (also identity strings). The plan resolves a deterministic total order from
/// these PLUS a stable tie-break on the identity string — NEVER link order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoConfigEntry {
    /// The author-stable identity path (`crate::module::Ident`) → `ContractId`.
    pub contract_path: String,
    /// Identities this entry must be ordered BEFORE.
    pub before: Vec<String>,
    /// Identities this entry must be ordered AFTER.
    pub after: Vec<String>,
}

impl AutoConfigEntry {
    /// A bare entry with no ordering edges.
    #[must_use]
    pub fn new(contract_path: impl Into<String>) -> Self {
        AutoConfigEntry {
            contract_path: contract_path.into(),
            before: Vec::new(),
            after: Vec::new(),
        }
    }

    /// Builder: declare a `before` edge.
    #[must_use]
    pub fn before(mut self, other: impl Into<String>) -> Self {
        self.before.push(other.into());
        self
    }

    /// Builder: declare an `after` edge.
    #[must_use]
    pub fn after(mut self, other: impl Into<String>) -> Self {
        self.after.push(other.into());
        self
    }
}

/// A cyclic-edge diagnostic from [`plan_order`] — the build-time `RegistrationStorm`
/// analogue for an unsatisfiable auto-config ordering (a Tier-1 hard error the
/// macro/prepare step turns loud, never a silent arbitrary order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderCycle {
    /// The identities still entangled in the cycle (deterministically sorted).
    pub members: Vec<String>,
}

impl std::fmt::Display for OrderCycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "auto-config ordering has a cycle among: {}",
            self.members.join(", ")
        )
    }
}

impl std::error::Error for OrderCycle {}

/// The deferred, deterministically-ordered auto-config plan (`cargo leaf prepare`
/// accelerator). The runtime `run_autoconfig` fold in leaf-boot would otherwise
/// recompute this every boot; checkpointing it here makes boot deterministic and
/// skips the fold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoConfigPlan {
    /// The resolved total order (identity paths), earliest-first.
    pub order: Vec<String>,
}

impl AutoConfigPlan {
    /// Lower the resolved order to a const `&[::leaf_core::ContractId]` expression
    /// via ABSOLUTE paths — the checkpoint the assembly pass reads instead of
    /// re-folding.
    #[must_use]
    pub fn emit(&self) -> TokenStream {
        let rows = self
            .order
            .iter()
            .map(|p| quote! { ::leaf_core::ContractId::of(#p) });
        quote! {
            #[doc(hidden)]
            pub const __LEAF_AUTOCONFIG_PLAN: &[::leaf_core::ContractId] = &[ #(#rows),* ];
        }
    }
}

/// Resolve a deterministic total order over the auto-config entries from their
/// `before`/`after` edges, tie-broken on the stable identity path (NEVER link
/// order). A Kahn topological sort with a sorted ready-queue for determinism.
///
/// # Errors
/// Returns an [`OrderCycle`] when the `before`/`after` edges are unsatisfiable
/// (a cycle) — a loud build-time diagnostic, never a silent arbitrary order.
pub fn plan_order(entries: &[AutoConfigEntry]) -> Result<AutoConfigPlan, OrderCycle> {
    // Normalize every edge to a forward "X before Y" relation. `after = [Z]`
    // on X means "Z before X". Only edges among KNOWN entries constrain the
    // order; an edge to an absent identity is a soft hint (ignored) so a plan
    // over a subset still resolves.
    let known: std::collections::BTreeSet<&str> =
        entries.iter().map(|e| e.contract_path.as_str()).collect();

    // adjacency: predecessors[node] = set of nodes that must come before it.
    let mut preds: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
    for e in entries {
        preds.entry(e.contract_path.as_str()).or_default();
    }
    for e in entries {
        let node = e.contract_path.as_str();
        for b in &e.before {
            // node before b  ⇒ b has predecessor node
            if known.contains(b.as_str()) {
                preds.get_mut(b.as_str()).expect("known").insert(node);
            }
        }
        for a in &e.after {
            // node after a   ⇒ node has predecessor a
            if known.contains(a.as_str()) {
                preds.get_mut(node).expect("known").insert(a.as_str());
            }
        }
    }

    // Kahn with a sorted ready-set for deterministic tie-breaking.
    let mut order: Vec<String> = Vec::new();
    let mut remaining = preds;
    while !remaining.is_empty() {
        // Ready = nodes with no remaining predecessors, picked in sorted order.
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, p)| p.is_empty())
            .map(|(n, _)| *n)
            .collect();
        let Some(&next) = ready.first() else {
            // No ready node but entries remain ⇒ a cycle.
            let mut members: Vec<String> =
                remaining.keys().map(|s| (*s).to_string()).collect();
            members.sort();
            return Err(OrderCycle { members });
        };
        order.push(next.to_string());
        remaining.remove(next);
        for p in remaining.values_mut() {
            p.remove(next);
        }
    }
    Ok(AutoConfigPlan { order })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render a `TokenStream` to a whitespace-collapsed string.
    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    /// A sample opaque leaf (the folder treats its internals as a black box).
    fn leaf(n: u32) -> CondTree {
        CondTree::Leaf {
            id: quote! { ::leaf_core::ConditionId(#n) },
            attrs: quote! { &[] },
        }
    }

    // ── ConstFold: the boolean-algebra rewrites ────────────────────────────────

    #[test]
    fn fold_not_of_a_const_inverts() {
        assert!(matches!(
            fold(&CondTree::Not(Box::new(CondTree::Const(true)))),
            CondTree::Const(false)
        ));
        assert!(matches!(
            fold(&CondTree::Not(Box::new(CondTree::Const(false)))),
            CondTree::Const(true)
        ));
    }

    #[test]
    fn fold_all_with_a_false_child_collapses_to_false() {
        // `All([leaf, false])` is unsatisfiable at build → Const(false).
        let tree = CondTree::All(vec![leaf(1), CondTree::Const(false)]);
        assert_eq!(fold(&tree).as_const(), Some(false));
    }

    #[test]
    fn fold_all_drops_true_children_and_unwraps_a_single_survivor() {
        // `All([true, leaf])` → just `leaf` (the true is the identity).
        let tree = CondTree::All(vec![CondTree::Const(true), leaf(7)]);
        let folded = fold(&tree);
        match folded {
            CondTree::Leaf { id, .. } => assert!(flat(&id).contains("ConditionId(7")),
            other => panic!("expected the lone leaf to unwrap, got {other:?}"),
        }
    }

    #[test]
    fn fold_empty_all_is_vacuously_true() {
        assert_eq!(fold(&CondTree::All(vec![])).as_const(), Some(true));
    }

    #[test]
    fn fold_any_with_a_true_child_collapses_to_true() {
        let tree = CondTree::Any(vec![leaf(1), CondTree::Const(true)]);
        assert_eq!(fold(&tree).as_const(), Some(true));
    }

    #[test]
    fn fold_any_drops_false_children_and_unwraps_a_single_survivor() {
        let tree = CondTree::Any(vec![CondTree::Const(false), leaf(3)]);
        match fold(&tree) {
            CondTree::Leaf { id, .. } => assert!(flat(&id).contains("ConditionId(3")),
            other => panic!("expected the lone leaf to unwrap, got {other:?}"),
        }
    }

    #[test]
    fn fold_empty_any_is_vacuously_false() {
        assert_eq!(fold(&CondTree::Any(vec![])).as_const(), Some(false));
    }

    #[test]
    fn fold_is_recursive_and_propagates_through_nesting() {
        // Not(Any([false, All([true, true])]))
        //   Any inner: All([true,true]) → true ; Any([false, true]) → true
        //   Not(true) → false
        let tree = CondTree::Not(Box::new(CondTree::Any(vec![
            CondTree::Const(false),
            CondTree::All(vec![CondTree::Const(true), CondTree::Const(true)]),
        ])));
        assert_eq!(fold(&tree).as_const(), Some(false));
    }

    #[test]
    fn fold_leaves_a_genuine_leaf_unfolded() {
        // A bare opaque leaf is never const-folded (its verdict is tier-deferred).
        assert!(matches!(fold(&leaf(1)), CondTree::Leaf { .. }));
    }

    // ── lowering to const ::leaf_core::CondExpr ────────────────────────────────

    #[test]
    fn emit_lowers_const_through_absolute_paths() {
        let s = flat(&emit(&CondTree::Const(true)));
        assert_eq!(s, "::leaf_core::CondExpr::Const(true)");
    }

    #[test]
    fn emit_lowers_all_to_a_static_slice_and_parses() {
        let tree = CondTree::All(vec![leaf(1), leaf(2)]);
        let ts = emit(&tree);
        syn::parse2::<syn::Expr>(ts.clone()).expect("emitted CondExpr is a valid expression");
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::CondExpr::All(&["), "got: {s}");
        assert!(s.contains("::leaf_core::CondExpr::Leaf(::leaf_core::ConditionId(1"), "got: {s}");
    }

    #[test]
    fn emit_lowers_not_to_a_reference() {
        let tree = CondTree::Not(Box::new(leaf(5)));
        let s = flat(&emit(&tree));
        assert!(s.contains("::leaf_core::CondExpr::Not(&::leaf_core::CondExpr::Leaf"), "got: {s}");
    }

    #[test]
    fn fold_and_emit_collapses_then_lowers_a_constant_tree() {
        // The full pipeline: a build-decidable tree folds to one Const literal.
        let tree = CondTree::All(vec![
            CondTree::Const(true),
            CondTree::Not(Box::new(CondTree::Const(false))),
        ]);
        let s = flat(&fold_and_emit(&tree));
        assert_eq!(s, "::leaf_core::CondExpr::Const(true)");
    }

    // ── the deferred auto-config ordering plan ─────────────────────────────────

    #[test]
    fn plan_order_resolves_before_after_edges() {
        // A before B; C after B  ⇒  A, B, C.
        let entries = vec![
            AutoConfigEntry::new("crate::A").before("crate::B"),
            AutoConfigEntry::new("crate::B"),
            AutoConfigEntry::new("crate::C").after("crate::B"),
        ];
        let plan = plan_order(&entries).expect("acyclic edges resolve");
        let a = plan.order.iter().position(|p| p == "crate::A").unwrap();
        let b = plan.order.iter().position(|p| p == "crate::B").unwrap();
        let c = plan.order.iter().position(|p| p == "crate::C").unwrap();
        assert!(a < b && b < c, "order: {:?}", plan.order);
    }

    #[test]
    fn plan_order_is_deterministic_tie_broken_on_identity() {
        // No edges at all ⇒ sorted by the stable identity path (NEVER link order).
        let entries = vec![
            AutoConfigEntry::new("crate::Zeta"),
            AutoConfigEntry::new("crate::Alpha"),
            AutoConfigEntry::new("crate::Mu"),
        ];
        let plan = plan_order(&entries).expect("resolves");
        assert_eq!(plan.order, vec!["crate::Alpha", "crate::Mu", "crate::Zeta"]);
    }

    #[test]
    fn plan_order_detects_a_cycle_loudly() {
        // A before B, B before A ⇒ a loud OrderCycle (never a silent order).
        let entries = vec![
            AutoConfigEntry::new("crate::A").before("crate::B"),
            AutoConfigEntry::new("crate::B").before("crate::A"),
        ];
        let err = plan_order(&entries).expect_err("a cycle must be loud");
        assert_eq!(err.members, vec!["crate::A", "crate::B"]);
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[test]
    fn plan_order_ignores_edges_to_absent_identities() {
        // An edge to a non-participating auto-config is a soft hint (a subset
        // plan still resolves), not a hard failure.
        let entries = vec![AutoConfigEntry::new("crate::A").after("crate::NotHere")];
        let plan = plan_order(&entries).expect("absent edge is ignored");
        assert_eq!(plan.order, vec!["crate::A"]);
    }

    #[test]
    fn auto_config_plan_emits_a_const_contract_id_checkpoint() {
        let plan = AutoConfigPlan {
            order: vec!["crate::A".into(), "crate::B".into()],
        };
        let ts = plan.emit();
        syn::parse2::<syn::File>(ts.clone()).expect("the plan checkpoint is valid Rust items");
        let s = flat(&ts);
        assert!(s.contains("const__LEAF_AUTOCONFIG_PLAN:&[::leaf_core::ContractId]"), "got: {s}");
        assert!(s.contains(r#"::leaf_core::ContractId::of("crate::A")"#), "got: {s}");
        assert!(s.contains(r#"::leaf_core::ContractId::of("crate::B")"#), "got: {s}");
    }
}
