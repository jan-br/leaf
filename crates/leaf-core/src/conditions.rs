//! The ONE typed gating algebra: [`CondExpr`] over a stable [`ConditionId`].
//!
//! Realizes conditions-autoconfig (phase3/05) — the toolkit's fixed
//! `conditional-strategy` ABI, made concrete. There is exactly ONE gating
//! mechanism in leaf:
//!
//! - [`CondExpr`] — the const, `&'static`-tree boolean algebra
//!   (`Leaf`/`All`/`Any`/`Not`/`Const`). The thin `#[conditional(...)]` macro
//!   emits exactly one const tree onto `Descriptor.meta`; `all`/`any`/`not` are
//!   first-class nodes (no `AnyOf`/`AllOf` `ConditionId`s), and `xor`/`count`
//!   are macro sugar lowered to these five — so the kernel enum stays FROZEN
//!   (adding a variant is a workspace-wide ABI break).
//! - The per-kind tier-map ABI: [`ConditionKind`] (`const ID`/`const TIER`/
//!   `const SUB`) over [`EarliestTier`] (`Cfg`/`ConstFold`/`Runtime` — `Runtime`
//!   is the MANDATORY floor) and [`SubPhase`] (`Parse`/`Register`). The phase is
//!   FRAMEWORK-INFERRED: [`CondExpr::tier`]/[`CondExpr::phase`] compute the
//!   `max` over leaves at const-eval time (an `OnBean` leaf nested anywhere
//!   auto-defers the whole guard to `Register`) — killing Spring's #1
//!   forgot-to-declare-`REGISTER` bug structurally.
//! - The runtime [`Condition`] SPI — synchronous, side-effect-free, no `.await`,
//!   no IO — matching over a borrowed read-only [`ConditionCtx`] snapshot, plus
//!   the [`ConditionId`]→`&dyn Condition` resolution row [`CondImplRow`] (a
//!   later unit lands the `CONDITIONS` linkme channel; the row type is here).
//! - The passive accounting sink: [`ReportSink`] / [`ConditionRecord`] /
//!   [`ConditionReport`] over the six-class [`ConditionReportClass`] verdict
//!   taxonomy, keyed by the stable [`ContractId`] (the silent-now/loud-later
//!   `NoSuchBean` enrichment join).
//! - Profiles as a PRESET, not a parallel engine: [`ON_PROFILE`] is one fixed
//!   `(Runtime, Parse)` row in the same tier-map; [`ProfileExpr`] (`!`/`&`/`|`)
//!   is a bounded micro-grammar; [`resolve_active`] is the pure activation
//!   algebra; [`matches`] is the pure evaluator (`And`=all, `Or`=any,
//!   `Not`=absence); [`accepts_profiles`] is the runtime-string escape hatch.
//! - The auto-config metamodel additions carried on `Descriptor.meta`:
//!   [`OrderHint`] (the three-pass batch sort data) and the closed
//!   [`ImportRef`]/[`ImportEdge`] composition currency. The `Descriptor` row
//!   itself is REUSED verbatim — there is no second `AutoConfigDescriptor` seed
//!   type (an auto-config IS a `Descriptor` flagged into the `AUTO_CONFIGS`
//!   channel at [`CandidateRole::FALLBACK`]).
//!
//! Scope note (this unit): the engines that DRIVE this ABI — `route_conditions`
//! / `run_autoconfig` / `order_batch` / the report finalize/fusion — live in
//! leaf-boot. The richer `ConditionCtx` fields (`DefinitionView`/`DefinitionProbe`/
//! `CapabilitySnapshot`) reference types owned by later units, so the borrowed
//! context is a minimal forward-compatible placeholder (`#[non_exhaustive]`,
//! carrying the always-available `&Env` + `&dyn ReportSink`). This unit pins the
//! const algebra, the tier inference, the profile grammar/algebra, and the
//! report shapes — all unit-testable in a bare crate.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use crate::definition::CandidateRole;
use crate::env::Env;
use crate::error::{Cause, ErrorKind, LeafError};
use crate::identity::ContractId;

// ─────────────────────────── tier-map kernel ABI ────────────────────────────

/// The earliest tier at which a condition leaf can be soundly decided
/// (conditional-strategy, FIXED). `Runtime` is the MANDATORY floor.
///
/// Ordered ascending = earliest-first so [`EarliestTier::max`] is a plain `max`:
/// a `Cfg` (compile-pruned `OnClass`) leaf is decided earliest; a `ConstFold`
/// (`Const(bool)`) leaf at build; a `Runtime` leaf (`OnProperty`/`OnProfile`/
/// `OnBean`) only against the sealed `Env`/definition set.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum EarliestTier {
    /// Compile-pruned + force-link-paired (`OnClass`/`OnCapability`) — earliest.
    Cfg = 0,
    /// Build-decidable, arrives as [`CondExpr::Const`] (`OnRustVersion`, …).
    ConstFold = 1,
    /// The mandatory floor: evaluated against the sealed `Env`/definition set.
    Runtime = 2,
}

impl EarliestTier {
    /// The later (more-deferred) of two tiers — the const-eval `max`.
    #[must_use]
    pub const fn max(self, other: EarliestTier) -> EarliestTier {
        if (self as u8) >= (other as u8) {
            self
        } else {
            other
        }
    }
}

/// The App&lt;Resolve&gt; sub-phase a runtime leaf must run in
/// (conditional-strategy). `Register` = the `OnBean` family, which must see the
/// growing definition set, so it is order-sensitive and runs LATER.
///
/// Ordered ascending = earliest-first so [`SubPhase::max`] is a plain `max`: an
/// `OnBean` leaf nested anywhere forces the WHOLE guard to `Register`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum SubPhase {
    /// No `OnBean`: evaluable in the Parse sub-pass over the sealed `Env`.
    Parse = 0,
    /// The `OnBean` family: must see the growing definition set (runs later).
    Register = 1,
}

impl SubPhase {
    /// The later (more-deferred) of two sub-phases — the const-eval `max`.
    #[must_use]
    pub const fn max(self, other: SubPhase) -> SubPhase {
        if (self as u8) >= (other as u8) {
            self
        } else {
            other
        }
    }
}

/// Stable identity of a condition KIND (an `OnProperty`/`OnBean`/… catalog
/// member), NOT a per-call-site id.
///
/// `ConditionId(u32)` is the dense in-binary key the `CONDITIONS` slice joins on
/// to find the kind's `&dyn Condition`. The condition-family catalog
/// (leaf-conditions) mints one stable id per member; the macro emits
/// `Leaf(ID, attrs)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConditionId(pub u32);

impl std::fmt::Debug for ConditionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConditionId({})", self.0)
    }
}

/// The fixed per-kind tier-map row: a condition KIND declares its stable id and
/// the earliest tier / sub-phase it can be decided at (the load-bearing
/// conditional-strategy artifact).
///
/// The map SHAPE is owned here (leaf-core); condition-family (leaf-conditions)
/// POPULATES rows by `impl ConditionKind for OnFoo`. Author phase declaration
/// exists NOWHERE — [`CondExpr::tier`]/[`phase`](CondExpr::phase) infer it.
pub trait ConditionKind {
    /// The stable cross-binary id this kind resolves under in `CONDITIONS`.
    const ID: ConditionId;
    /// The earliest tier at which this kind can be soundly decided.
    const TIER: EarliestTier;
    /// The App&lt;Resolve&gt; sub-phase this kind runs in.
    const SUB: SubPhase;
}

// ─────────────────────────── the attr carriage ──────────────────────────────

/// One typed attribute on a condition leaf — the uniform stringly carriage the
/// `Condition` impl reads (typed-per-member validation happens at MACRO time;
/// the const data is uniform).
#[derive(Clone, Copy, Debug)]
pub enum Attr {
    /// A string-valued attribute (`having_value = "true"`).
    Str(&'static str, &'static str),
    /// A boolean-valued attribute (`match_if_missing = false`).
    Bool(&'static str, bool),
    /// An integer-valued attribute.
    Int(&'static str, i64),
    /// A `TypeId`-valued attribute (`on_bean(Foo)` carries `TypeId::of::<Foo>()`).
    Type(&'static str, std::any::TypeId),
}

impl Attr {
    /// The attribute's key.
    #[must_use]
    pub const fn key(&self) -> &'static str {
        match self {
            Attr::Str(k, _) | Attr::Bool(k, _) | Attr::Int(k, _) | Attr::Type(k, _) => k,
        }
    }
}

/// A const slice of [`Attr`] carried by a [`CondExpr::Leaf`].
pub type AttrSlice = &'static [Attr];

// ─────────────────────────── the CondExpr algebra ───────────────────────────

/// THE one condition algebra: a const `&'static`-tree boolean expression
/// (conditional-strategy). FROZEN at five variants for v1.
///
/// The thin `#[conditional(all(on_property("x"), any(on_bean(Foo), not(on_class("redis")))))]`
/// macro emits exactly one const `CondExpr` tree onto `Descriptor.meta` via
/// absolute `::leaf_core` paths — `cargo expand` shows one legible const. There
/// is no second gating mechanism and no `AnyNestedCondition` boilerplate
/// (boolean composition is first-class syntax).
#[derive(Clone, Copy, Debug)]
pub enum CondExpr {
    /// A catalog-member leaf: a [`ConditionId`] + its const [`AttrSlice`].
    Leaf(ConditionId, AttrSlice),
    /// Conjunction: matches iff EVERY child matches (vacuously `true`).
    All(&'static [CondExpr]),
    /// Disjunction: matches iff ANY child matches (vacuously `false`).
    Any(&'static [CondExpr]),
    /// Negation of its single child.
    Not(&'static CondExpr),
    /// A build-folded constant (a `ConstFold` leaf collapses to this).
    Const(bool),
}

impl CondExpr {
    /// The earliest tier at which this tree can be decided = `max` over leaves
    /// (const-eval). A `Const` is the `ConstFold` floor; an empty `All`/`Any`
    /// has no leaves, so it folds to the earliest tier (`Cfg`).
    ///
    /// Scope note: a `Leaf`'s per-kind tier is owned by the kind's
    /// [`ConditionKind`] impl (leaf-conditions). This const fn cannot look that
    /// up (a `ConditionId` is opaque data here), so a `Leaf` contributes the
    /// `Runtime` floor — the SOUND default (the mandatory floor), conservative
    /// by construction. leaf-conditions refines a known-`Cfg`/`ConstFold` leaf
    /// by lowering it to `Const`/pruning it before this is read.
    #[must_use]
    pub const fn tier(&self) -> EarliestTier {
        match self {
            CondExpr::Const(_) => EarliestTier::ConstFold,
            CondExpr::Leaf(_, _) => EarliestTier::Runtime,
            CondExpr::Not(inner) => inner.tier(),
            CondExpr::All(children) | CondExpr::Any(children) => {
                let mut acc = EarliestTier::Cfg;
                let mut i = 0;
                while i < children.len() {
                    acc = acc.max(children[i].tier());
                    i += 1;
                }
                acc
            }
        }
    }

    /// The App&lt;Resolve&gt; sub-phase this tree runs in = `max` over leaves
    /// (const-eval). A `Leaf` contributes `Parse` by default; an `OnBean`-family
    /// member is lowered to `Register` by leaf-conditions wrapping it
    /// (the macro reads the kind's `const SUB`), so a tree containing one defers
    /// wholly to `Register`.
    ///
    /// Scope note: like [`tier`](CondExpr::tier), a bare `Leaf`'s kind sub-phase
    /// is not introspectable from the opaque `ConditionId` here. The macro
    /// computes the per-leaf `SUB` from each member's `ConditionKind::SUB` at
    /// emit time; this const fn folds the structural `max`. A plain `Leaf`
    /// therefore reads as `Parse` (the common case); the register-deferral is
    /// driven by the macro choosing the tree shape, verified by leaf-conditions.
    #[must_use]
    pub const fn phase(&self) -> SubPhase {
        match self {
            CondExpr::Const(_) | CondExpr::Leaf(_, _) => SubPhase::Parse,
            CondExpr::Not(inner) => inner.phase(),
            CondExpr::All(children) | CondExpr::Any(children) => {
                let mut acc = SubPhase::Parse;
                let mut i = 0;
                while i < children.len() {
                    acc = acc.max(children[i].phase());
                    i += 1;
                }
                acc
            }
        }
    }

    /// `true` iff this tree is a build-folded constant (no runtime work).
    #[must_use]
    pub const fn is_const(&self) -> bool {
        matches!(self, CondExpr::Const(_))
    }
}

/// The vacuously-true unconditional guard (`All([])`) — the value a
/// `Descriptor` with no `#[conditional]` carries on `meta.guard`.
pub const UNCONDITIONAL: CondExpr = CondExpr::All(&[]);

/// Evaluate a [`CondExpr`] tree against a [`ConditionCtx`], returning ONE
/// [`ConditionOutcome`]. Pure, synchronous, side-effect-free except that leaf
/// matching delegates to the kind's [`Condition::matches`] (which may record a
/// reason into the borrowed [`ReportSink`]).
///
/// `All` short-circuits on the first non-match; `Any` short-circuits on the
/// first match; `Not` inverts. A `Const(b)` is `b` with an empty reason. A
/// `Leaf` resolves its `ConditionId` to a `&dyn Condition` through the supplied
/// resolver closure and delegates; an UNRESOLVED `ConditionId` is the loud
/// `ConditionError` (never a silent pass-all) — surfaced as `Err`.
///
/// # Errors
/// Returns a [`LeafError`] of [`ErrorKind::ConditionError`] iff a `Leaf`'s
/// [`ConditionId`] cannot be resolved to a registered [`Condition`] (the
/// anti-DCE "unresolved condition" guard). A condition that simply does not
/// match is NOT an error — it is a graceful `Ok(ConditionOutcome { matched:
/// false, .. })`.
pub fn evaluate(
    expr: &CondExpr,
    ctx: &ConditionCtx<'_>,
    resolve: &dyn Fn(ConditionId) -> Option<&'static dyn Condition>,
) -> Result<ConditionOutcome, LeafError> {
    match expr {
        CondExpr::Const(b) => Ok(ConditionOutcome::bare(*b)),
        CondExpr::Leaf(id, attrs) => match resolve(*id) {
            Some(cond) => Ok(cond.matches(ctx, attrs)),
            None => Err(unresolved_condition(*id)),
        },
        CondExpr::Not(inner) => {
            let inner_out = evaluate(inner, ctx, resolve)?;
            Ok(ConditionOutcome {
                matched: !inner_out.matched,
                reason: inner_out.reason,
            })
        }
        CondExpr::All(children) => {
            for child in children.iter() {
                let out = evaluate(child, ctx, resolve)?;
                if !out.matched {
                    // First failing child decides the conjunction (its reason).
                    return Ok(ConditionOutcome { matched: false, reason: out.reason });
                }
            }
            Ok(ConditionOutcome::bare(true))
        }
        CondExpr::Any(children) => {
            let mut last = ReasonMsg::EMPTY;
            for child in children.iter() {
                let out = evaluate(child, ctx, resolve)?;
                if out.matched {
                    return Ok(ConditionOutcome { matched: true, reason: out.reason });
                }
                last = out.reason;
            }
            // No child matched: report the last child's reason.
            Ok(ConditionOutcome { matched: false, reason: last })
        }
    }
}

/// Build the loud `ConditionError` for an unresolved [`ConditionId`] (the
/// anti-DCE "condition family crate not force-linked" guard).
#[must_use]
fn unresolved_condition(id: ConditionId) -> LeafError {
    LeafError::new(ErrorKind::ConditionError).caused_by(Cause::plain(
        "resolving condition",
        format!("no registered Condition for {id:?} (is its family crate force-linked?)"),
    ))
}

// ─────────────────────────── the runtime SPI ────────────────────────────────

/// A machine-parseable + human-renderable reason for a condition verdict
/// (condition-report). Each catalog member authors one so the report populates
/// for free (`OnProperty x.enabled: found "false", expected "true"`).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ReasonMsg {
    /// The kind that produced the verdict (`"OnProperty"`, `"OnMissingBean"`).
    pub kind: &'static str,
    /// What the kind expected, if expressible.
    pub expected: Option<String>,
    /// What the kind actually found, if expressible.
    pub found: Option<String>,
    /// The gate/key the verdict turned on (the property name, the bean type).
    pub gate: Option<&'static str>,
}

impl ReasonMsg {
    /// The empty reason (an `All`/`Const` carries no kind-specific narrative).
    pub const EMPTY: ReasonMsg = ReasonMsg { kind: "", expected: None, found: None, gate: None };

    /// A reason carrying only the producing kind.
    #[must_use]
    pub fn of(kind: &'static str) -> Self {
        ReasonMsg { kind, ..ReasonMsg::EMPTY }
    }
}

/// The verdict of one [`Condition::matches`] / [`evaluate`] call.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConditionOutcome {
    /// Whether the condition matched (registration proceeds iff `true`).
    pub matched: bool,
    /// The reason narrative (populated by the kind; empty for `All`/`Const`).
    pub reason: ReasonMsg,
}

impl ConditionOutcome {
    /// An outcome with no reason narrative.
    #[must_use]
    pub fn bare(matched: bool) -> Self {
        ConditionOutcome { matched, reason: ReasonMsg::EMPTY }
    }

    /// An outcome carrying a reason.
    #[must_use]
    pub fn new(matched: bool, reason: ReasonMsg) -> Self {
        ConditionOutcome { matched, reason }
    }
}

/// The result of a no-instantiation candidate probe over the definition view
/// (the `@ConditionalOnMissingBean` analogue). The `OnBean` family delegates its
/// verdict to this — the SAME primitive App&lt;Wired&gt; validation uses, so
/// there is one definition of "unambiguous".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolvability {
    /// No candidate for the queried type (`on_missing_bean` matches).
    None,
    /// Exactly one resolvable candidate after primary/fallback policy
    /// (`on_bean`/`on_single_candidate` match; carries its raw slot id as `u32`
    /// to stay independent of the registry `BeanId` until that unit is wired).
    Unique(u32),
    /// Multiple unresolved candidates (`on_bean` matches, `on_single_candidate`
    /// does not); carries the count.
    Ambiguous(u16),
}

impl Resolvability {
    /// `true` iff exactly one candidate resolves (`== Unique`).
    #[must_use]
    pub const fn is_unique(self) -> bool {
        matches!(self, Resolvability::Unique(_))
    }
}

/// The borrowed read-only snapshot a [`Condition`] matches against
/// (conditional-strategy). A cheap borrow — NO `Arc`-per-condition, NO global
/// lock.
///
/// Scope note (this unit): the rich fields (`defs: &DefinitionView`,
/// `probe: &DefinitionProbe`, `caps: &CapabilitySnapshot`) reference types owned
/// by later registry/injection/discovery units. This is the minimal
/// forward-compatible placeholder carrying the always-available sealed `&Env`
/// and the `&dyn ReportSink`; it is `#[non_exhaustive]` so adding those borrows
/// later is not a breaking change.
#[non_exhaustive]
pub struct ConditionCtx<'a> {
    /// The sealed environment snapshot (Parse-tier leaves read it).
    pub env: &'a Env,
    /// The passive accounting sink the one eval path writes through.
    pub report: &'a dyn ReportSink,
}

impl<'a> ConditionCtx<'a> {
    /// Build a context over a sealed `Env` and a report sink.
    #[must_use]
    pub fn new(env: &'a Env, report: &'a dyn ReportSink) -> Self {
        ConditionCtx { env, report }
    }
}

impl std::fmt::Debug for ConditionCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConditionCtx").finish_non_exhaustive()
    }
}

/// THE runtime condition SPI (the mandatory floor): synchronous,
/// side-effect-free, no `.await`, no IO.
///
/// A catalog member (leaf-conditions) implements this once and registers one
/// [`CondImplRow`] into the `CONDITIONS` slice. `matches` reads the borrowed
/// [`ConditionCtx`] (the sealed `Env`, later the definition probe) and the const
/// [`AttrSlice`], returning a verdict + a kind-authored [`ReasonMsg`].
pub trait Condition: Send + Sync {
    /// Evaluate this condition against the context and its const attributes.
    fn matches(&self, ctx: &ConditionCtx<'_>, attrs: &AttrSlice) -> ConditionOutcome;
}

/// One `ConditionId -> &dyn Condition` resolution row carried by the
/// `CONDITIONS` linkme slice (anti-DCE self-checked: an unresolved id is a
/// Tier-1 assembly error via [`evaluate`], never a silent pass-all).
///
/// `Debug` is hand-written (a `&dyn Condition` is not itself `Debug`).
#[derive(Clone, Copy)]
pub struct CondImplRow {
    /// The kind id this row resolves.
    pub id: ConditionId,
    /// The kind's singleton condition implementation.
    pub imp: &'static dyn Condition,
}

impl std::fmt::Debug for CondImplRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CondImplRow").field("id", &self.id).finish_non_exhaustive()
    }
}

// ─────────────────────────── condition report ───────────────────────────────

/// One leaf's recorded verdict inside a [`ConditionRecord`] (condition-report).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LeafOutcome {
    /// The leaf's kind id.
    pub id: ConditionId,
    /// Whether the leaf matched.
    pub matched: bool,
    /// The kind-authored reason.
    pub reason: ReasonMsg,
}

/// The six FROZEN cross-tier verdict classes (conditional-strategy
/// `ConditionReportClass`). All producers/consumers agree on these.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ConditionReportClass {
    /// Registered: the guard matched.
    Positive,
    /// Backed off: the guard did not match (carries the reason). NOT an error.
    Negative(ReasonMsg),
    /// Excluded by `exclude=`/`leaf.autoconfigure.exclude` (mints no slot).
    Exclusion(ContractId),
    /// No `#[conditional]` guard at all (the unconditional element).
    Unconditional,
    /// Compiled out: its capability feature is off (from the `CapabilitySnapshot`).
    CompiledOutByCfg(crate::discovery::SourceTag),
    /// Build-folded false: a `ConstFold` leaf decided `false` at build.
    BuildFoldedFalse(ConditionId),
}

/// One accounting row the eval path writes (condition-report). Keyed by the
/// stable [`ContractId`] (the silent-now/loud-later `NoSuchBean` enrichment
/// join); `self_type` is the in-process fast secondary key.
#[derive(Clone, Debug)]
pub struct ConditionRecord {
    /// The gated element's stable cross-build id.
    pub element: ContractId,
    /// The element's `TypeId` (fast secondary key), if known.
    pub self_type: Option<std::any::TypeId>,
    /// The verdict class.
    pub class: ConditionReportClass,
    /// Per-leaf verdicts (the tree breakdown).
    pub leaves: Box<[LeafOutcome]>,
}

/// The passive accounting SINK the one eval path writes through
/// (condition-report) — NOT an evaluator and NOT a reason-author.
///
/// The default sink is a cold-path append (no `Arc`, no lock); at `seal()` it
/// freezes into an `Arc<ConditionReport>`. `Send + Sync` because the borrowed
/// `&dyn ReportSink` rides the `ConditionCtx` through the cold assembly pass.
pub trait ReportSink: Send + Sync {
    /// Record one element's condition verdict.
    fn record(&self, rec: ConditionRecord);
}

/// A no-op sink (the bare-engine default / tests that ignore the report).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopReportSink;

impl ReportSink for NoopReportSink {
    fn record(&self, _rec: ConditionRecord) {}
}

/// The frozen condition report (condition-report), keyed by [`ContractId`] for
/// O(1) `NoSuchBean`-enrichment lookup.
///
/// Built once at `seal()` from the accumulated [`ConditionRecord`]s; read
/// lock-free at steady state. The `render_startup`/`serialize` delivery readers
/// and the report finalize/fusion (runtime rows + `CapabilitySnapshot` +
/// anti-DCE result) live in leaf-boot; this is the frozen shape + the O(1)
/// lookup the kernel owns.
#[derive(Clone, Debug, Default)]
pub struct ConditionReport {
    records: Vec<ConditionRecord>,
    index: HashMap<ContractId, usize>,
}

impl ConditionReport {
    /// An empty report.
    #[must_use]
    pub fn new() -> Self {
        ConditionReport::default()
    }

    /// Freeze a batch of accumulated records into the keyed report. A later
    /// record for the same element overwrites (last-write-wins on the index).
    #[must_use]
    pub fn from_records(records: Vec<ConditionRecord>) -> Self {
        let mut index = HashMap::with_capacity(records.len());
        for (i, rec) in records.iter().enumerate() {
            index.insert(rec.element, i);
        }
        ConditionReport { records, index }
    }

    /// O(1) lookup by stable element id (the `NoSuchBean` enrichment join).
    #[must_use]
    pub fn lookup(&self, c: ContractId) -> Option<&ConditionRecord> {
        self.index.get(&c).map(|&i| &self.records[i])
    }

    /// All records (insertion order).
    #[must_use]
    pub fn records(&self) -> &[ConditionRecord] {
        &self.records
    }

    /// Number of recorded elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the report is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

// ─────────────────────── auto-config metamodel additions ────────────────────

/// The three-pass batch-sort data carried on `Descriptor.meta` for an
/// auto-config (auto-config-ordering). Definition order ONLY — structurally
/// unreadable by the App&lt;Running&gt; instantiation scheduler.
///
/// `order` feeds the `cmp_order` pass (a dedicated auto-config `OrderSource`,
/// distinct from collection-injection's `@Order`); `before`/`after` (typed) and
/// `before_name`/`after_name` (string, resolved against the candidate name
/// index) feed the topological pass with cycle detection.
#[derive(Clone, Copy, Debug)]
pub struct OrderHint {
    /// The `i32` priority (lower = earlier); `DEFAULT_ORDER` when unset.
    pub order: i32,
    /// Auto-configs that must register BEFORE this one (typed edges).
    pub before: &'static [ContractId],
    /// Auto-configs that must register AFTER this one (typed edges).
    pub after: &'static [ContractId],
    /// `before` edges named by string (resolved against the candidate index).
    pub before_name: &'static [&'static str],
    /// `after` edges named by string (resolved against the candidate index).
    pub after_name: &'static [&'static str],
}

impl OrderHint {
    /// The default hint: `DEFAULT_ORDER`, no edges.
    pub const DEFAULT: OrderHint = OrderHint {
        order: crate::order::DEFAULT_ORDER,
        before: &[],
        after: &[],
        before_name: &[],
        after_name: &[],
    };
}

impl Default for OrderHint {
    fn default() -> Self {
        OrderHint::DEFAULT
    }
}

/// The CLOSED import-composition currency (import-composition): a reference to
/// an importable element, by stable id or by registered marker — NEVER a
/// classloader/runtime-string resolution.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ImportRef {
    /// Import by stable cross-build [`ContractId`].
    ByContract(ContractId),
    /// Import by a registered [`MarkerId`].
    Marker(crate::identity::MarkerId),
}

/// One `#[import(..)]` edge carried on `Descriptor.meta` (import-composition):
/// the importer references real types, so the importer path-references the
/// importee — solving Layer-0 DCE for that edge for free.
#[derive(Clone, Copy, Debug)]
pub struct ImportEdge {
    /// The importing element's stable id.
    pub from: ContractId,
    /// The imported elements (de-duped by id at lift; diamond-safe).
    pub to: &'static [ContractId],
}

/// Whether a [`Descriptor`](crate::Descriptor) participates as an auto-config
/// candidate (registers at [`CandidateRole::FALLBACK`]) — the structural flag
/// distinguishing the `AUTO_CONFIGS` channel from `COMPONENTS` without a second
/// seed type. This is a convenience over the existing [`CandidateRole`]: an
/// auto-config row carries `FALLBACK`; a plain component carries `NORMAL`.
#[must_use]
pub const fn auto_config_role() -> CandidateRole {
    CandidateRole::FALLBACK
}

// ───────────────────────────── profiles ─────────────────────────────────────

/// The fixed `ON_PROFILE` condition id — one row in the per-kind tier-map at
/// `(TIER = Runtime, SUB = Parse)`. Profiles are a PRESET, not a parallel
/// engine: `#[profile("prod & (eu | us)")]` lowers to `Leaf(ON_PROFILE, attrs)`.
///
/// Minted through the one [`contract_hash`](crate::contract_hash) over a stable
/// FQN so it is reproducible across builds; truncated to the `ConditionId(u32)`
/// dense space.
pub const ON_PROFILE: ConditionId = ConditionId(crate::identity::contract_hash("leaf::condition::OnProfile") as u32);

/// The const profile-expression micro-grammar (profiles): `!` / `&` / `|`,
/// array = OR, negation = absence. The `#[profile("...")]` macro PARSES the
/// string at MACRO time into a const `ProfileExpr`; mixed `&`/`|` without parens
/// is a `compile_error!` (the fail-fast-on-ambiguity intent as a Tier-0 error).
#[derive(Clone, Copy, Debug)]
pub enum ProfileExpr {
    /// A bare profile name; matches iff active.
    Name(&'static str),
    /// Negation; `Not(Name)` matches iff the name is ABSENT.
    Not(&'static ProfileExpr),
    /// Conjunction (`&`); matches iff every child matches (vacuously `true`).
    And(&'static [ProfileExpr]),
    /// Disjunction (`|`, and the array form); matches iff any child matches.
    Or(&'static [ProfileExpr]),
}

/// Why a profile became active (profiles `ActiveProfiles` provenance).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ActivationReason {
    /// Listed in the `active` lever.
    Active,
    /// Contributed by the `include` lever.
    Include,
    /// Fanned out from a group expansion.
    Group(Arc<str>),
    /// The default fallback (`{default}` when nothing else activated).
    Default,
}

/// The canonical sealed set of active profiles + ordered provenance (profiles).
/// `Send + Sync`, lock-free read; produced once by [`resolve_active`] inside
/// `seal_environment`.
#[derive(Clone, Debug, Default)]
pub struct ActiveProfiles {
    set: HashSet<Arc<str>>,
    ordered_provenance: Box<[(Arc<str>, ActivationReason)]>,
}

impl ActiveProfiles {
    /// `true` iff `name` is in the active set.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.set.contains(name)
    }

    /// The active set (unordered).
    #[must_use]
    pub fn set(&self) -> &HashSet<Arc<str>> {
        &self.set
    }

    /// The ordered `(profile, reason)` provenance.
    #[must_use]
    pub fn provenance(&self) -> &[(Arc<str>, ActivationReason)] {
        &self.ordered_provenance
    }

    /// Whether the active set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

/// The activation levers harvested by environment-config from the property
/// stack (TRANSPORT) — profiles owns only the [`resolve_active`] ALGEBRA over
/// them. `include` arrives already per-source-resolved (the buried
/// per-property-source rule lives upstream).
#[derive(Clone, Debug, Default)]
pub struct ProfileLevers {
    /// The explicitly-activated profiles (`leaf.profiles.active`).
    pub active: Vec<Arc<str>>,
    /// The included profiles (`leaf.profiles.include`), prepended before active.
    pub include: Vec<Arc<str>>,
    /// Group fan-out aliases (a group name → its member profiles), applied
    /// transitively with cycle detection.
    pub groups: HashMap<Arc<str>, Vec<Arc<str>>>,
    /// The default profile name (`{default}` fallback when nothing activated).
    pub default: Arc<str>,
}

/// A profile-activation algebra error (profiles).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ProfileError {
    /// A group fan-out cycle (carries the path of group names).
    GroupCycle {
        /// The cyclic group-name path.
        path: Vec<Arc<str>>,
    },
    /// A profile name failed validation (gated on `validate = true`).
    InvalidName {
        /// The offending name.
        name: Arc<str>,
    },
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::GroupCycle { path } => {
                write!(f, "profile group cycle: ")?;
                for (i, p) in path.iter().enumerate() {
                    if i > 0 {
                        write!(f, " -> ")?;
                    }
                    write!(f, "{p}")?;
                }
                Ok(())
            }
            ProfileError::InvalidName { name } => write!(f, "invalid profile name `{name}`"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl From<ProfileError> for LeafError {
    fn from(e: ProfileError) -> LeafError {
        LeafError::new(ErrorKind::ProfileError)
            .caused_by(Cause::plain("resolving active profiles", e.to_string()))
    }
}

/// A profile-string PARSE error (the runtime [`accepts_profiles`] escape hatch).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProfileParseError {
    /// The human-readable parse diagnostic.
    pub message: String,
}

impl std::fmt::Display for ProfileParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "profile parse error: {}", self.message)
    }
}

impl std::error::Error for ProfileParseError {}

/// The PURE activation algebra (profiles): every buried rule as a named branch.
/// Runs once inside `seal_environment`.
///
/// Rules, in order: empty `active` ∧ empty `include` → `{default}`; any explicit
/// `active` ∨ `include` drops `default` entirely (`default_suppressed_by_explicit`);
/// `include` is prepended before `active`; group fan-out is applied TRANSITIVELY
/// with cycle detection; name validation is gated on `validate`.
///
/// # Errors
/// Returns [`ProfileError::GroupCycle`] on a transitive group cycle, or
/// [`ProfileError::InvalidName`] when `validate` is set and a name is illegal.
pub fn resolve_active(levers: ProfileLevers, validate: bool) -> Result<ActiveProfiles, ProfileError> {
    let mut set: HashSet<Arc<str>> = HashSet::new();
    let mut provenance: Vec<(Arc<str>, ActivationReason)> = Vec::new();

    let has_explicit = !levers.active.is_empty() || !levers.include.is_empty();

    // Seed the worklist: include first (prepended before active), then active.
    let mut work: Vec<(Arc<str>, ActivationReason)> = Vec::new();
    for p in &levers.include {
        work.push((p.clone(), ActivationReason::Include));
    }
    for p in &levers.active {
        work.push((p.clone(), ActivationReason::Active));
    }

    if !has_explicit {
        // Nothing explicit: the default profile activates.
        work.push((levers.default.clone(), ActivationReason::Default));
    }

    // Transitive group fan-out with cycle detection. We expand each activated
    // name; if it names a group, its members are enqueued under Group(name).
    // The `in_progress` stack detects a cycle along the current expansion path.
    let mut visited_groups: HashSet<Arc<str>> = HashSet::new();
    let mut i = 0;
    while i < work.len() {
        let (name, reason) = work[i].clone();
        i += 1;

        if validate && !is_valid_profile_name(&name) {
            return Err(ProfileError::InvalidName { name });
        }

        if set.insert(name.clone()) {
            provenance.push((name.clone(), reason));
        }

        // Group fan-out (transitive).
        if let Some(members) = levers.groups.get(&name) {
            // Cycle detection: a depth-first expansion path that re-enters a
            // group already on the active expansion path is a cycle.
            expand_group(
                &name,
                members,
                &levers.groups,
                &mut work,
                &mut visited_groups,
                &mut Vec::new(),
            )?;
        }
    }

    Ok(ActiveProfiles {
        set,
        ordered_provenance: provenance.into_boxed_slice(),
    })
}

/// Recursively enqueue a group's members, detecting cycles along the path.
fn expand_group(
    group: &Arc<str>,
    members: &[Arc<str>],
    groups: &HashMap<Arc<str>, Vec<Arc<str>>>,
    work: &mut Vec<(Arc<str>, ActivationReason)>,
    visited: &mut HashSet<Arc<str>>,
    path: &mut Vec<Arc<str>>,
) -> Result<(), ProfileError> {
    if path.iter().any(|g| g == group) {
        let mut cycle = path.clone();
        cycle.push(group.clone());
        return Err(ProfileError::GroupCycle { path: cycle });
    }
    path.push(group.clone());
    for m in members {
        work.push((m.clone(), ActivationReason::Group(group.clone())));
        if let Some(sub) = groups.get(m) {
            if visited.insert(m.clone()) {
                expand_group(m, sub, groups, work, visited, path)?;
            } else {
                // Already expanded elsewhere — but still check it is not on the
                // current path (a back-edge), which `path` membership catches.
                if path.iter().any(|g| g == m) {
                    let mut cycle = path.clone();
                    cycle.push(m.clone());
                    return Err(ProfileError::GroupCycle { path: cycle });
                }
            }
        }
    }
    path.pop();
    Ok(())
}

/// Whether a profile name is syntactically valid (no whitespace, non-empty, no
/// grammar operators). The validation gate is intentionally permissive — the
/// macro-time grammar parse is the strict check.
fn is_valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(|c| c.is_whitespace() || matches!(c, '!' | '&' | '|' | '(' | ')'))
}

/// The PURE profile evaluator (profiles): `And` = all, `Or` = any, `Not` =
/// absence. A synchronous lock-free read over the sealed [`ActiveProfiles`].
#[must_use]
pub fn matches(expr: &ProfileExpr, active: &ActiveProfiles) -> bool {
    match expr {
        ProfileExpr::Name(n) => active.contains(n),
        ProfileExpr::Not(inner) => !matches(inner, active),
        ProfileExpr::And(children) => children.iter().all(|c| matches(c, active)),
        ProfileExpr::Or(children) => children.iter().any(|c| matches(c, active)),
    }
}

/// The runtime-string escape hatch (profiles): parse a profile expression over
/// the 3-operator algebra (`!`/`&`/`|`, parens) and evaluate it against the
/// current process active set.
///
/// This is the ONE sanctioned runtime-string profile parse (SEAMS: a bounded
/// micro-grammar, NOT an interpreter). It is `accepts_profiles` over an
/// explicitly-supplied active set so the kernel stays free of an ambient global.
///
/// # Errors
/// Returns [`ProfileParseError`] on a syntactically malformed expression
/// (mismatched parens, mixed `&`/`|` without parens, empty operand).
pub fn accepts_profiles(s: &str, active: &ActiveProfiles) -> Result<bool, ProfileParseError> {
    let expr = parse_profile_expr(s)?;
    Ok(eval_owned(&expr, active))
}

/// A runtime (owned) profile expression — the parse target of
/// [`accepts_profiles`] (the const [`ProfileExpr`] is the macro-time target).
#[derive(Clone, PartialEq, Eq, Debug)]
enum OwnedProfileExpr {
    Name(String),
    Not(Box<OwnedProfileExpr>),
    And(Vec<OwnedProfileExpr>),
    Or(Vec<OwnedProfileExpr>),
}

fn eval_owned(expr: &OwnedProfileExpr, active: &ActiveProfiles) -> bool {
    match expr {
        OwnedProfileExpr::Name(n) => active.contains(n),
        OwnedProfileExpr::Not(inner) => !eval_owned(inner, active),
        OwnedProfileExpr::And(children) => children.iter().all(|c| eval_owned(c, active)),
        OwnedProfileExpr::Or(children) => children.iter().any(|c| eval_owned(c, active)),
    }
}

/// Parse a profile expression string into an [`OwnedProfileExpr`].
///
/// Grammar (the frozen 3-operator algebra): names are `[A-Za-z0-9_.-]+`; `!`
/// prefix-negates; `&` is conjunction; `|` is disjunction; parens group. Mixing
/// `&` and `|` at the SAME level without parens is rejected (the
/// fail-fast-on-ambiguity rule, mirrored from the macro-time `compile_error!`).
fn parse_profile_expr(s: &str) -> Result<OwnedProfileExpr, ProfileParseError> {
    let tokens = tokenize_profile(s)?;
    let mut pos = 0;
    let expr = parse_expr(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(ProfileParseError {
            message: format!("unexpected trailing tokens at position {pos}"),
        });
    }
    Ok(expr)
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum PTok {
    Name(String),
    Not,
    And,
    Or,
    LParen,
    RParen,
}

fn tokenize_profile(s: &str) -> Result<Vec<PTok>, ProfileParseError> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '!' => {
                toks.push(PTok::Not);
                chars.next();
            }
            '&' => {
                toks.push(PTok::And);
                chars.next();
            }
            '|' => {
                toks.push(PTok::Or);
                chars.next();
            }
            '(' => {
                toks.push(PTok::LParen);
                chars.next();
            }
            ')' => {
                toks.push(PTok::RParen);
                chars.next();
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' => {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' {
                        name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(PTok::Name(name));
            }
            other => {
                return Err(ProfileParseError {
                    message: format!("illegal character `{other}`"),
                });
            }
        }
    }
    Ok(toks)
}

/// Parse one expression: a sequence of primaries joined by a SINGLE operator
/// kind (mixing `&`/`|` at one level without parens is rejected).
fn parse_expr(tokens: &[PTok], pos: &mut usize) -> Result<OwnedProfileExpr, ProfileParseError> {
    let first = parse_primary(tokens, pos)?;
    // Peek the operator (if any).
    let op = match tokens.get(*pos) {
        Some(PTok::And) => Some(PTok::And),
        Some(PTok::Or) => Some(PTok::Or),
        _ => None,
    };
    let Some(op) = op else {
        return Ok(first);
    };

    let mut operands = vec![first];
    while let Some(tok) = tokens.get(*pos) {
        match tok {
            PTok::And | PTok::Or if *tok == op => {
                *pos += 1;
                operands.push(parse_primary(tokens, pos)?);
            }
            PTok::And | PTok::Or => {
                // A different operator at the same level without parens.
                return Err(ProfileParseError {
                    message: "mixed `&` and `|` without parentheses".to_string(),
                });
            }
            _ => break,
        }
    }
    Ok(match op {
        PTok::And => OwnedProfileExpr::And(operands),
        _ => OwnedProfileExpr::Or(operands),
    })
}

fn parse_primary(tokens: &[PTok], pos: &mut usize) -> Result<OwnedProfileExpr, ProfileParseError> {
    match tokens.get(*pos) {
        Some(PTok::Not) => {
            *pos += 1;
            let inner = parse_primary(tokens, pos)?;
            Ok(OwnedProfileExpr::Not(Box::new(inner)))
        }
        Some(PTok::LParen) => {
            *pos += 1;
            let inner = parse_expr(tokens, pos)?;
            match tokens.get(*pos) {
                Some(PTok::RParen) => {
                    *pos += 1;
                    Ok(inner)
                }
                _ => Err(ProfileParseError {
                    message: "missing closing parenthesis".to_string(),
                }),
            }
        }
        Some(PTok::Name(n)) => {
            *pos += 1;
            Ok(OwnedProfileExpr::Name(n.clone()))
        }
        Some(other) => Err(ProfileParseError {
            message: format!("expected a name, `!`, or `(`, got {other:?}"),
        }),
        None => Err(ProfileParseError {
            message: "unexpected end of expression".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{Env, EnvBuilder};

    // ── helpers ─────────────────────────────────────────────────────────────

    fn empty_env() -> Env {
        EnvBuilder::new().seal_env()
    }

    // A condition that matches iff the given const flag is set; records its kind.
    struct FlagCondition {
        result: bool,
        kind: &'static str,
    }
    impl Condition for FlagCondition {
        fn matches(&self, _ctx: &ConditionCtx<'_>, _attrs: &AttrSlice) -> ConditionOutcome {
            ConditionOutcome::new(self.result, ReasonMsg::of(self.kind))
        }
    }

    static COND_TRUE: FlagCondition = FlagCondition { result: true, kind: "True" };
    static COND_FALSE: FlagCondition = FlagCondition { result: false, kind: "False" };

    const ID_TRUE: ConditionId = ConditionId(1);
    const ID_FALSE: ConditionId = ConditionId(2);

    fn resolver(id: ConditionId) -> Option<&'static dyn Condition> {
        match id {
            ID_TRUE => Some(&COND_TRUE),
            ID_FALSE => Some(&COND_FALSE),
            _ => None,
        }
    }

    fn eval(expr: &CondExpr) -> Result<bool, LeafError> {
        let env = empty_env();
        let sink = NoopReportSink;
        let ctx = ConditionCtx::new(&env, &sink);
        evaluate(expr, &ctx, &resolver).map(|o| o.matched)
    }

    const LEAF_TRUE: CondExpr = CondExpr::Leaf(ID_TRUE, &[]);
    const LEAF_FALSE: CondExpr = CondExpr::Leaf(ID_FALSE, &[]);

    // ── CondExpr evaluation: All / Any / Not over a stub ctx ─────────────────

    #[test]
    fn leaf_delegates_to_the_resolved_condition() {
        assert!(eval(&LEAF_TRUE).unwrap());
        assert!(!eval(&LEAF_FALSE).unwrap());
    }

    #[test]
    fn not_inverts_its_child() {
        const NOT_TRUE: CondExpr = CondExpr::Not(&LEAF_TRUE);
        const NOT_FALSE: CondExpr = CondExpr::Not(&LEAF_FALSE);
        assert!(!eval(&NOT_TRUE).unwrap());
        assert!(eval(&NOT_FALSE).unwrap());
    }

    #[test]
    fn all_is_conjunction_and_vacuously_true() {
        const ALL_TT: CondExpr = CondExpr::All(&[LEAF_TRUE, LEAF_TRUE]);
        const ALL_TF: CondExpr = CondExpr::All(&[LEAF_TRUE, LEAF_FALSE]);
        const ALL_EMPTY: CondExpr = CondExpr::All(&[]);
        assert!(eval(&ALL_TT).unwrap());
        assert!(!eval(&ALL_TF).unwrap());
        assert!(eval(&ALL_EMPTY).unwrap(), "empty All is vacuously true");
    }

    #[test]
    fn any_is_disjunction_and_vacuously_false() {
        const ANY_FT: CondExpr = CondExpr::Any(&[LEAF_FALSE, LEAF_TRUE]);
        const ANY_FF: CondExpr = CondExpr::Any(&[LEAF_FALSE, LEAF_FALSE]);
        const ANY_EMPTY: CondExpr = CondExpr::Any(&[]);
        assert!(eval(&ANY_FT).unwrap());
        assert!(!eval(&ANY_FF).unwrap());
        assert!(!eval(&ANY_EMPTY).unwrap(), "empty Any is vacuously false");
    }

    #[test]
    fn const_leaf_needs_no_resolver() {
        assert!(eval(&CondExpr::Const(true)).unwrap());
        assert!(!eval(&CondExpr::Const(false)).unwrap());
    }

    #[test]
    fn nested_composition_evaluates_correctly() {
        // all(true, any(false, not(false)))  =>  true && (false || !false) => true
        const TREE: CondExpr =
            CondExpr::All(&[LEAF_TRUE, CondExpr::Any(&[LEAF_FALSE, CondExpr::Not(&LEAF_FALSE)])]);
        assert!(eval(&TREE).unwrap());
    }

    #[test]
    fn unresolved_condition_id_is_a_loud_error_not_a_silent_pass() {
        const UNKNOWN: CondExpr = CondExpr::Leaf(ConditionId(999), &[]);
        let err = eval(&UNKNOWN).expect_err("unresolved id must be loud");
        assert_eq!(err.kind, ErrorKind::ConditionError);
    }

    #[test]
    fn all_propagates_the_failing_child_reason() {
        const ALL_TF: CondExpr = CondExpr::All(&[LEAF_TRUE, LEAF_FALSE]);
        let env = empty_env();
        let sink = NoopReportSink;
        let ctx = ConditionCtx::new(&env, &sink);
        let out = evaluate(&ALL_TF, &ctx, &resolver).unwrap();
        assert!(!out.matched);
        assert_eq!(out.reason.kind, "False");
    }

    // ── tier / phase classification (max over leaves, const-eval) ────────────

    #[test]
    fn earliest_tier_max_picks_the_later_tier() {
        assert_eq!(EarliestTier::Cfg.max(EarliestTier::Runtime), EarliestTier::Runtime);
        assert_eq!(EarliestTier::ConstFold.max(EarliestTier::Cfg), EarliestTier::ConstFold);
        assert_eq!(EarliestTier::Runtime.max(EarliestTier::Runtime), EarliestTier::Runtime);
    }

    #[test]
    fn const_expr_tier_is_constfold() {
        const T: EarliestTier = CondExpr::Const(true).tier();
        assert_eq!(T, EarliestTier::ConstFold);
    }

    #[test]
    fn leaf_tier_is_the_runtime_floor() {
        const T: EarliestTier = LEAF_TRUE.tier();
        assert_eq!(T, EarliestTier::Runtime, "a leaf is the mandatory runtime floor by default");
    }

    #[test]
    fn empty_all_folds_to_cfg_tier() {
        const T: EarliestTier = CondExpr::All(&[]).tier();
        assert_eq!(T, EarliestTier::Cfg, "no leaves => earliest tier");
    }

    #[test]
    fn tree_tier_is_max_over_leaves() {
        // all(Const, Leaf) => max(ConstFold, Runtime) = Runtime
        const TREE: CondExpr = CondExpr::All(&[CondExpr::Const(true), LEAF_TRUE]);
        const T: EarliestTier = TREE.tier();
        assert_eq!(T, EarliestTier::Runtime);
        // all(Const, Const) => ConstFold
        const TREE2: CondExpr = CondExpr::All(&[CondExpr::Const(true), CondExpr::Const(false)]);
        const T2: EarliestTier = TREE2.tier();
        assert_eq!(T2, EarliestTier::ConstFold);
    }

    #[test]
    fn sub_phase_max_picks_register() {
        assert_eq!(SubPhase::Parse.max(SubPhase::Register), SubPhase::Register);
        assert_eq!(SubPhase::Parse.max(SubPhase::Parse), SubPhase::Parse);
    }

    #[test]
    fn const_evaluable_tier_and_phase() {
        // The whole point: tier()/phase() are const fns usable in const context.
        const TREE: CondExpr = CondExpr::Not(&CondExpr::All(&[CondExpr::Const(true), LEAF_TRUE]));
        const TIER: EarliestTier = TREE.tier();
        const PHASE: SubPhase = TREE.phase();
        assert_eq!(TIER, EarliestTier::Runtime);
        assert_eq!(PHASE, SubPhase::Parse);
    }

    #[test]
    fn unconditional_is_vacuously_true() {
        assert!(eval(&UNCONDITIONAL).unwrap());
        assert!(UNCONDITIONAL.tier() <= EarliestTier::Runtime);
    }

    // ── ConditionKind tier-map row ───────────────────────────────────────────

    #[test]
    fn condition_kind_declares_its_const_tier_map_row() {
        struct OnBean;
        impl ConditionKind for OnBean {
            const ID: ConditionId = ConditionId(42);
            const TIER: EarliestTier = EarliestTier::Runtime;
            const SUB: SubPhase = SubPhase::Register;
        }
        assert_eq!(<OnBean as ConditionKind>::ID, ConditionId(42));
        assert_eq!(<OnBean as ConditionKind>::TIER, EarliestTier::Runtime);
        assert_eq!(<OnBean as ConditionKind>::SUB, SubPhase::Register);
    }

    // ── Resolvability (OnBean delegation contract) ───────────────────────────

    #[test]
    fn resolvability_unique_predicate() {
        assert!(Resolvability::Unique(3).is_unique());
        assert!(!Resolvability::None.is_unique());
        assert!(!Resolvability::Ambiguous(2).is_unique());
    }

    // ── ConditionReport keyed lookup ─────────────────────────────────────────

    #[test]
    fn condition_report_is_keyed_by_contract_id() {
        let a = ContractId::of("crate::A");
        let b = ContractId::of("crate::B");
        let report = ConditionReport::from_records(vec![
            ConditionRecord {
                element: a,
                self_type: None,
                class: ConditionReportClass::Positive,
                leaves: Box::new([]),
            },
            ConditionRecord {
                element: b,
                self_type: None,
                class: ConditionReportClass::Negative(ReasonMsg::of("OnProperty")),
                leaves: Box::new([]),
            },
        ]);
        assert_eq!(report.len(), 2);
        assert!(matches!(report.lookup(a).unwrap().class, ConditionReportClass::Positive));
        assert!(matches!(
            report.lookup(b).unwrap().class,
            ConditionReportClass::Negative(_)
        ));
        assert!(report.lookup(ContractId::of("crate::Missing")).is_none());
    }

    #[test]
    fn report_sink_records_through_the_seam() {
        use std::sync::Mutex;
        struct CountingSink(Mutex<Vec<ContractId>>);
        impl ReportSink for CountingSink {
            fn record(&self, rec: ConditionRecord) {
                self.0.lock().unwrap().push(rec.element);
            }
        }
        let sink = CountingSink(Mutex::new(Vec::new()));
        sink.record(ConditionRecord {
            element: ContractId::of("crate::X"),
            self_type: None,
            class: ConditionReportClass::Unconditional,
            leaves: Box::new([]),
        });
        assert_eq!(sink.0.lock().unwrap().len(), 1);
    }

    // ── auto-config metamodel additions ──────────────────────────────────────

    #[test]
    fn auto_config_role_is_fallback() {
        assert_eq!(auto_config_role(), CandidateRole::FALLBACK);
        assert!(auto_config_role().is_fallback());
    }

    #[test]
    fn order_hint_default_is_zero_and_edgeless() {
        const H: OrderHint = OrderHint::DEFAULT;
        assert_eq!(H.order, crate::order::DEFAULT_ORDER);
        assert!(H.before.is_empty() && H.after.is_empty());
        assert!(H.before_name.is_empty() && H.after_name.is_empty());
    }

    #[test]
    fn import_ref_is_a_closed_set() {
        let by_c = ImportRef::ByContract(ContractId::of("crate::Cfg"));
        let by_m = ImportRef::Marker(crate::identity::MarkerId::of("leaf::Marker"));
        assert_ne!(by_c, by_m);
        const EDGE: ImportEdge = ImportEdge {
            from: ContractId::of("crate::App"),
            to: &[ContractId::of("crate::DbConfig")],
        };
        assert_eq!(EDGE.to.len(), 1);
    }

    // ── ProfileExpr parse + eval ─────────────────────────────────────────────

    fn active(names: &[&str]) -> ActiveProfiles {
        let levers = ProfileLevers {
            active: names.iter().map(|s| Arc::from(*s)).collect(),
            include: vec![],
            groups: HashMap::new(),
            default: Arc::from("default"),
        };
        resolve_active(levers, false).unwrap()
    }

    #[test]
    fn profile_expr_name_matches_active_membership() {
        const PROD: ProfileExpr = ProfileExpr::Name("prod");
        assert!(matches(&PROD, &active(&["prod"])));
        assert!(!matches(&PROD, &active(&["dev"])));
    }

    #[test]
    fn profile_expr_not_matches_absence() {
        const NOT_PROD: ProfileExpr = ProfileExpr::Not(&ProfileExpr::Name("prod"));
        // !prod registers under default (prod absent).
        assert!(matches(&NOT_PROD, &active(&["dev"])));
        assert!(!matches(&NOT_PROD, &active(&["prod"])));
    }

    #[test]
    fn profile_expr_and_or_compose() {
        // prod & (eu | us)
        const EXPR: ProfileExpr = ProfileExpr::And(&[
            ProfileExpr::Name("prod"),
            ProfileExpr::Or(&[ProfileExpr::Name("eu"), ProfileExpr::Name("us")]),
        ]);
        assert!(matches(&EXPR, &active(&["prod", "eu"])));
        assert!(matches(&EXPR, &active(&["prod", "us"])));
        assert!(!matches(&EXPR, &active(&["prod"])), "needs a region");
        assert!(!matches(&EXPR, &active(&["eu"])), "needs prod");
    }

    #[test]
    fn empty_and_is_true_empty_or_is_false() {
        const AND_EMPTY: ProfileExpr = ProfileExpr::And(&[]);
        const OR_EMPTY: ProfileExpr = ProfileExpr::Or(&[]);
        assert!(matches(&AND_EMPTY, &active(&[])));
        assert!(!matches(&OR_EMPTY, &active(&[])));
    }

    // ── accepts_profiles (runtime-string escape hatch) ───────────────────────

    #[test]
    fn accepts_profiles_parses_and_evaluates() {
        let a = active(&["prod", "eu"]);
        assert!(accepts_profiles("prod", &a).unwrap());
        assert!(accepts_profiles("prod & (eu | us)", &a).unwrap());
        assert!(accepts_profiles("!dev", &a).unwrap());
        assert!(!accepts_profiles("dev", &a).unwrap());
        assert!(!accepts_profiles("prod & us", &a).unwrap());
    }

    #[test]
    fn accepts_profiles_rejects_mixed_operators_without_parens() {
        let a = active(&["prod"]);
        let err = accepts_profiles("a & b | c", &a).expect_err("mixed without parens is ambiguous");
        assert!(err.message.contains("mixed"), "got: {}", err.message);
    }

    #[test]
    fn accepts_profiles_rejects_unbalanced_parens() {
        let a = active(&[]);
        assert!(accepts_profiles("(a & b", &a).is_err());
        assert!(accepts_profiles("a)", &a).is_err());
        assert!(accepts_profiles("", &a).is_err());
    }

    #[test]
    fn accepts_profiles_parenthesized_mix_is_ok() {
        let a = active(&["a", "c"]);
        // (a & b) | c  => (a&b)=false, but | c => true
        assert!(accepts_profiles("(a & b) | c", &a).unwrap());
    }

    // ── resolve_active activation algebra ────────────────────────────────────

    #[test]
    fn resolve_active_empty_falls_back_to_default() {
        let levers = ProfileLevers {
            active: vec![],
            include: vec![],
            groups: HashMap::new(),
            default: Arc::from("default"),
        };
        let ap = resolve_active(levers, false).unwrap();
        assert!(ap.contains("default"));
        assert_eq!(ap.set().len(), 1);
        assert_eq!(ap.provenance()[0].1, ActivationReason::Default);
    }

    #[test]
    fn resolve_active_explicit_suppresses_default() {
        let levers = ProfileLevers {
            active: vec![Arc::from("prod")],
            include: vec![],
            groups: HashMap::new(),
            default: Arc::from("default"),
        };
        let ap = resolve_active(levers, false).unwrap();
        assert!(ap.contains("prod"));
        assert!(!ap.contains("default"), "explicit active drops default");
    }

    #[test]
    fn resolve_active_include_is_prepended_before_active() {
        let levers = ProfileLevers {
            active: vec![Arc::from("prod")],
            include: vec![Arc::from("base")],
            groups: HashMap::new(),
            default: Arc::from("default"),
        };
        let ap = resolve_active(levers, false).unwrap();
        assert!(ap.contains("base") && ap.contains("prod"));
        // include comes first in provenance ordering.
        assert_eq!(ap.provenance()[0].0.as_ref(), "base");
        assert_eq!(ap.provenance()[0].1, ActivationReason::Include);
    }

    #[test]
    fn resolve_active_expands_groups_transitively() {
        let mut groups = HashMap::new();
        groups.insert(Arc::<str>::from("prod"), vec![Arc::<str>::from("db"), Arc::<str>::from("cache")]);
        groups.insert(Arc::<str>::from("db"), vec![Arc::<str>::from("postgres")]);
        let levers = ProfileLevers {
            active: vec![Arc::from("prod")],
            include: vec![],
            groups,
            default: Arc::from("default"),
        };
        let ap = resolve_active(levers, false).unwrap();
        assert!(ap.contains("prod"));
        assert!(ap.contains("db"));
        assert!(ap.contains("cache"));
        assert!(ap.contains("postgres"), "transitive group fan-out");
    }

    #[test]
    fn resolve_active_detects_a_group_cycle() {
        let mut groups = HashMap::new();
        groups.insert(Arc::<str>::from("a"), vec![Arc::<str>::from("b")]);
        groups.insert(Arc::<str>::from("b"), vec![Arc::<str>::from("a")]);
        let levers = ProfileLevers {
            active: vec![Arc::from("a")],
            include: vec![],
            groups,
            default: Arc::from("default"),
        };
        let err = resolve_active(levers, false).expect_err("a -> b -> a is a cycle");
        assert!(matches!(err, ProfileError::GroupCycle { .. }));
    }

    #[test]
    fn resolve_active_validates_names_when_gated() {
        let levers = ProfileLevers {
            active: vec![Arc::from("bad name")],
            include: vec![],
            groups: HashMap::new(),
            default: Arc::from("default"),
        };
        let err = resolve_active(levers, true).expect_err("whitespace name is invalid");
        assert!(matches!(err, ProfileError::InvalidName { .. }));
    }

    #[test]
    fn profile_error_converts_into_leaf_error() {
        let e: LeafError = ProfileError::GroupCycle { path: vec![Arc::from("a")] }.into();
        assert_eq!(e.kind, ErrorKind::ProfileError);
    }

    #[test]
    fn on_profile_is_a_stable_reproducible_id() {
        // Same FQN => same id across builds (the contract_hash invariant).
        assert_eq!(ON_PROFILE.0, crate::identity::contract_hash("leaf::condition::OnProfile") as u32);
    }
}
