//! The single resolution spine: [`InjectionPoint`], the fixed-order traced
//! [`Selector`] fold, and the honest-visible deferral handles
//! ([`Lookup`]/[`LazyRef`]/[`Inject`]/[`SelfRef`]).
//!
//! Realizes injection-resolution (phase3/03) and the SEAMS C5 (2-axis
//! [`CandidateRole`]) / C6 (one [`cmp_order`]) pins. Nine Phase-1 features
//! collapse onto exactly four pieces of shared machinery, all owned here:
//!
//! 1. ONE selection primitive — [`Selector::resolve_one`], a generic fold over
//!    the fixed [`LAYERS`] array with a SINGLE terminal fail-fast rule
//!    (`len 0 -> None`, `len 1 -> One`, `len >1 -> Ambiguous`). Each layer is a
//!    pure `fn(&InjectionPoint, &[&Cand]) -> Verdict`; the driver short-circuits
//!    on `Unique`/narrow-to-one and applies the 0/1/>1 rule in EXACTLY ONE place.
//!    The `Trace` is recorded ONLY on the `>1` path (the warm `len<=1` path stays
//!    allocation-free and branch-only).
//! 2. ONE candidate SET, keyed for FILTERING only — [`CandidateSet`] of [`Cand`]
//!    read-views over the frozen registry; it produces the set, it NEVER selects.
//! 3. ONE deferral driver — [`resolve`](Resolve) over the renamed consumer family
//!    [`Lookup`]/[`LazyRef`]/[`Inject`], with [`SelfRef`] for self-injection. The
//!    handle holds the ownership-model `Resolve = Arc<dyn Fn(..) -> BoxFuture>`
//!    closure over a [`Weak`](std::sync::Weak) container back-ref (no `Arc` cycle).
//!    These are the HONEST VISIBLE handles (the docs' own preference): advice is
//!    transparent `dyn Svc`, but the deferral family is a visible-in-the-type
//!    handle — no CGLIB-style transparent `@Lazy`.
//! 4. ONE comparator — [`cmp_order`](crate::cmp_order) for [`PriorityRank`] and
//!    collection/map ordering. No site mints its own.
//!
//! ## Single-phase construction; deferral-only cycle break
//!
//! Construction is single-phase (factory params ARE the injection points), so
//! there is never a half-built instance: a constructor-param cycle is logically
//! fatal at the validation pass and the ONLY break is an explicit deferral edge
//! ([`PointKind::Deferral`]/[`PointKind::SelfRef`] remove the construction-time
//! edge). No two-phase `populate`, no early-exposure cache.
//!
//! ## The fixed layer order (SEAMS C5)
//!
//! `[generic_narrow, qualifier_narrow, primary_promote, name_match,
//! qualifier_name, priority_rank, default_candidate, resolvable_dep]`. The
//! `primary_promote` layer is a FIXED three-step fold — **FallbackDemote FIRST**,
//! then PrimaryPromote, then the len-rule — so a soft `@Fallback` ALWAYS loses to
//! a user `Normal` of the same contract (the SEAMS C5 ordering pin; this corrects
//! the original two-pass that let a `@Fallback @Primary` win over a plain user
//! bean). [`Resolved::None`]/[`Resolved::Ambiguous`] are non-coercible, so
//! "refuse to guess" is a TYPE.
//!
//! ## `AdvisedConcreteInjection` (the COHERENCE seam)
//!
//! An advised bean MUST be injected through a declared service-trait view; a
//! [`Selector`] concrete-`TypeId` match against an advised bean is REJECTED as
//! [`ErrorKind::AdvisedConcreteInjection`](crate::ErrorKind::AdvisedConcreteInjection)
//! (never a silent un-advised raw-target handoff). [`reject_advised_concrete`]
//! is the kernel check the `App<Wired>` pass calls.

use std::any::TypeId;
use std::sync::{Arc, Weak};

use smallvec::SmallVec;

use crate::definition::CandidateRole;
use crate::error::{Cause, ErrorKind, LeafError};
use crate::future::BoxFuture;
use crate::handle::{downcast_ref, Published, Ref};
use crate::identity::{BeanId, BeanKey, MarkerId};
use crate::order::{cmp_order, OrderKey};

// ─────────────────────────── InjectionPoint ─────────────────────────────────

/// The shape (cardinality) of an injection point — how MANY beans it consumes.
///
/// `Single`/`Optional` route through [`Selector::resolve_one`] (the arbitrating
/// fold); `Collection`/`Map` BYPASS selection entirely (collect-all + order, the
/// `Cardinality::Multiple` path). `primary_promote`/`FallbackDemote` and the
/// other narrowing layers run ONLY for `Single`/`Optional` — arity structurally
/// decides whether the layer runs at all (collection-injection non-interaction).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Arity {
    /// Exactly one mandatory bean (`Ref<T>`); zero is `NoSuchBean`, many is
    /// `NoUniqueBean`.
    Single,
    /// At most one bean (`Option<T>`); zero is `Ok(None)`, many is still
    /// `NoUniqueBean` (absence is tolerated, ambiguity is NOT).
    Optional,
    /// All qualifying beans as an ordered collection (`Vec`/`HashSet`/`Array`);
    /// the empty set is an empty collection, never `NoSuchBean`.
    Collection,
    /// All qualifying beans keyed by bean name (`Map<String, T>`), in the
    /// registration-ordered name overlay.
    Map,
}

impl Arity {
    /// `true` iff this arity BYPASSES [`Selector::resolve_one`] (the
    /// collect-all + `cmp_order` `Cardinality::Multiple` path).
    #[must_use]
    pub const fn is_multiple(self) -> bool {
        matches!(self, Arity::Collection | Arity::Map)
    }

    /// `true` iff absence (an empty candidate set) is a tolerated, non-error
    /// outcome for this arity (`Optional`/`Collection`/`Map`, never `Single`).
    #[must_use]
    pub const fn tolerates_absence(self) -> bool {
        !matches!(self, Arity::Single)
    }
}

/// What KIND of injection point this is — whether it participates in the
/// construction-time dependency graph.
///
/// `Bean`/`Value` are real construction-time edges; `Deferral`/`SelfRef` REMOVE
/// the construction-time edge (they resolve post-build through the deferral
/// handle's `Weak` back-ref), which is the ONLY cycle break.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PointKind {
    /// A normal collaborator bean (constructor/field/setter param).
    Bean,
    /// A `@Value`/placeholder-resolved scalar (read from `Env`, not the registry).
    Value,
    /// A deferral handle ([`Lookup`]/[`LazyRef`]/[`Inject`]) — removes the
    /// construction-time edge; resolves post-build (also the cycle break and the
    /// `@Lookup`/lookup-method mechanism).
    Deferral,
    /// A self-injection handle ([`SelfRef`]) — emits NO by-type candidate edge;
    /// resolves to the bean's OWN published (advised, if advised) handle.
    SelfRef,
}

impl PointKind {
    /// `true` iff a point of this kind contributes a construction-time edge to
    /// the dependency graph (`Bean`/`Value`); `Deferral`/`SelfRef` do NOT, which
    /// is what makes them the cycle break.
    #[must_use]
    pub const fn is_construction_edge(self) -> bool {
        matches!(self, PointKind::Bean | PointKind::Value)
    }
}

/// A qualifier requirement on an injection point (candidate-resolver hybrid).
///
/// The tension the design demands: a typed zero-sized `#[qualifier]` marker
/// (interned to a [`MarkerId`] — the compile-safe single-marker key, NOT a
/// `TypeId`, matching the frozen `AnnotationMetadata.qualifiers: &[MarkerId]`
/// ABI) AND/OR an open string (the bean-name-as-implicit-qualifier / config-
/// valued / name-fallback path). Both may be present (match-both); both `None`
/// is a vacuous requirement that matches anything.
///
/// Design note: the phase-3 API sketch wrote `marker: Option<TypeId>`, but the
/// reconciled identity unit (SEAMS seam #2) fixed qualifier identity to the
/// cross-build [`MarkerId`] over `contract_hash`, NEVER a `TypeId`; this row
/// uses `MarkerId` to stay coherent with the frozen `Descriptor` ABI.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct QualifierReq {
    /// The required typed-marker identity, if any (interned `MarkerId`).
    pub marker: Option<MarkerId>,
    /// The required open-string qualifier (bean name / config-valued), if any.
    pub name: Option<&'static str>,
}

impl QualifierReq {
    /// A requirement on a single typed marker.
    #[must_use]
    pub const fn marker(marker: MarkerId) -> Self {
        QualifierReq { marker: Some(marker), name: None }
    }

    /// A requirement on a single open-string qualifier (bean name).
    #[must_use]
    pub const fn named(name: &'static str) -> Self {
        QualifierReq { marker: None, name: Some(name) }
    }

    /// `true` iff this requirement constrains nothing (matches any candidate).
    #[must_use]
    pub const fn is_vacuous(self) -> bool {
        self.marker.is_none() && self.name.is_none()
    }
}

/// The element shape of a collection/map injection point.
///
/// `Some(..)` on an [`InjectionPoint`] means [`Arity`] is multiple
/// (`Cardinality::Multiple`); the materialized container is keyed/deduped by
/// `BeanId` (the dense join key), so a multi-view bean is exactly one element.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CollectionShape {
    /// `Vec<T>`, `cmp_order`-ordered.
    Vec,
    /// `HashSet<T>`, `cmp_order`-ordered into a stable iteration order.
    HashSet,
    /// `[T; N]` — a length mismatch (`candidates != n`) is a rich error.
    Array(usize),
    /// `Map<String, T>` keyed on bean names (insertion/registration order).
    Map,
}

/// THE const injection-point row — one per constructor/field/setter param,
/// macro-emitted into a per-bean [`InjectionPlan`].
///
/// A flat, `Copy`, const record (mirrors [`Descriptor`](crate::Descriptor)'s
/// closed-schema discipline). `produced` is the resolved `dyn Svc` view or
/// concrete `TypeId`; `generics` narrows free generics-as-qualifier; `qualifiers`
/// carries the typed-marker + string requirements; `name` is the declared
/// param/field ident (the implicit string qualifier); `arity`/`kind` drive the
/// fold vs collect-all routing and the construction-edge classification.
#[derive(Clone, Copy, Debug)]
pub struct InjectionPoint {
    /// The produced `dyn Svc` view or concrete `TypeId` this point resolves.
    pub produced: TypeId,
    /// Generic-argument `TypeId`s (free generics-as-qualifier narrowing).
    pub generics: &'static [TypeId],
    /// The typed-marker + open-string qualifier requirements (match-all).
    pub qualifiers: &'static [QualifierReq],
    /// The declared param/field ident — the implicit string (name) qualifier.
    pub name: &'static str,
    /// How many beans this point consumes (and the fold-vs-collect-all routing).
    pub arity: Arity,
    /// Whether this point is a construction edge or a deferral/self edge.
    pub kind: PointKind,
    /// `Some` iff multi-valued; the element/container shape (bypasses selection).
    pub collection: Option<CollectionShape>,
}

impl InjectionPoint {
    /// A mandatory single-bean point of `produced` type (the common case).
    #[must_use]
    pub const fn single(produced: TypeId, name: &'static str) -> Self {
        InjectionPoint {
            produced,
            generics: &[],
            qualifiers: &[],
            name,
            arity: Arity::Single,
            kind: PointKind::Bean,
            collection: None,
        }
    }

    /// An optional single-bean point (`Option<T>`): absence is tolerated.
    #[must_use]
    pub const fn optional(produced: TypeId, name: &'static str) -> Self {
        InjectionPoint {
            produced,
            generics: &[],
            qualifiers: &[],
            name,
            arity: Arity::Optional,
            kind: PointKind::Bean,
            collection: None,
        }
    }

    /// A collection point of element type `produced` with the given shape.
    #[must_use]
    pub const fn collection(produced: TypeId, name: &'static str, shape: CollectionShape) -> Self {
        let arity = match shape {
            CollectionShape::Map => Arity::Map,
            _ => Arity::Collection,
        };
        InjectionPoint {
            produced,
            generics: &[],
            qualifiers: &[],
            name,
            arity,
            kind: PointKind::Bean,
            collection: Some(shape),
        }
    }

    /// `true` iff this point removes the construction-time edge (a `Deferral` or
    /// `SelfRef` kind) — i.e. it is a valid cycle break.
    #[must_use]
    pub const fn is_deferred(self) -> bool {
        !self.kind.is_construction_edge()
    }
}

/// The per-bean const injection plan — the ordered injection points the
/// single-phase factory consumes. Macro-emitted via `::leaf_core` paths.
#[derive(Clone, Copy, Debug)]
pub struct InjectionPlan {
    /// The ordered injection points (constructor/field/setter params).
    pub points: &'static [InjectionPoint],
}

impl InjectionPlan {
    /// An empty plan (a bean with no injected collaborators).
    pub const EMPTY: InjectionPlan = InjectionPlan { points: &[] };

    /// The construction-time edges of this plan (the `Bean`/`Value` points that
    /// feed the dependency-graph cycle classification — deferral/self removed).
    pub fn construction_edges(&self) -> impl Iterator<Item = &InjectionPoint> {
        self.points
            .iter()
            .filter(|p| p.kind.is_construction_edge())
    }
}

impl Default for InjectionPlan {
    fn default() -> Self {
        InjectionPlan::EMPTY
    }
}

// ─────────────────────────── Cand (read-view) ───────────────────────────────

/// A candidate read-view the [`Selector`] layers fold over.
///
/// A cheap `Copy` projection of one frozen `MergedDefinition` row carrying ONLY
/// what the layers read: the dense `BeanId`, the 2-axis [`CandidateRole`] (SEAMS
/// C5 — the SINGLE source of truth, never separate `is_primary`/`is_fallback`
/// bools), the [`OrderKey`] for `priority_rank`/collection ordering, the
/// canonical `name`, the `hierarchy_depth` (`0` = local; local beats parent),
/// the `generics`/`marker` keys for narrowing, the `default_candidate` weak flag,
/// whether the match was against the bean's CONCRETE `self_type` (vs a `dyn`
/// view), and whether the bean is advised (for the `AdvisedConcreteInjection`
/// COHERENCE rejection).
#[derive(Clone, Copy, Debug)]
pub struct Cand<'a> {
    /// The dense registry slot id (the join key for dedup + construction).
    pub id: BeanId,
    /// The 2-axis selection role (SEAMS C5) — primacy × fallback.
    pub role: CandidateRole,
    /// The orderable key (`priority_rank` / collection ordering).
    pub order: OrderKey,
    /// The canonical bean name (implicit string qualifier / `name_match`).
    pub name: &'a str,
    /// Parent-merge depth (`0` = local); a local primary beats a parent primary.
    pub hierarchy_depth: u16,
    /// The candidate's generic-argument `TypeId`s (elementwise narrowing).
    pub generics: &'a [TypeId],
    /// The candidate's typed-qualifier markers (set-membership narrowing).
    pub markers: &'a [MarkerId],
    /// Whether the candidate participates in plain by-type autowiring; `false`
    /// means weak (`default_candidate`) — injectable only when explicitly named.
    pub autowire_candidate: bool,
    /// `true` iff the injection point matched this bean's CONCRETE `self_type`
    /// (as opposed to a declared `dyn Svc` view) — drives the advised-concrete
    /// rejection.
    pub concrete_match: bool,
    /// `true` iff this bean is advised (wrapped by an advisor chain), so a
    /// concrete-`TypeId` injection of it is the `AdvisedConcreteInjection` error.
    pub advised: bool,
}

impl<'a> Cand<'a> {
    /// A plain candidate with default flags (the common test/builder shape):
    /// normal role, implicit order, not advised, matched by `dyn` view.
    #[must_use]
    pub fn new(id: BeanId, name: &'a str) -> Self {
        Cand {
            id,
            role: CandidateRole::NORMAL,
            order: OrderKey::implicit(),
            name,
            hierarchy_depth: 0,
            generics: &[],
            markers: &[],
            autowire_candidate: true,
            concrete_match: false,
            advised: false,
        }
    }

    /// `true` iff this candidate carries `marker` in its typed-qualifier set.
    #[must_use]
    pub fn has_marker(&self, marker: MarkerId) -> bool {
        self.markers.contains(&marker)
    }
}

/// The candidate set fed to [`Selector::resolve_one`] — produced by the
/// candidate-resolver (filtering), NEVER selecting. Backed by the dense `BeanId`
/// space; the working set is a [`SmallVec`] so the warm single-candidate case is
/// inline.
#[derive(Clone, Debug, Default)]
pub struct CandidateSet<'a> {
    cands: SmallVec<[Cand<'a>; 4]>,
}

impl<'a> CandidateSet<'a> {
    /// An empty candidate set.
    #[must_use]
    pub fn new() -> Self {
        CandidateSet { cands: SmallVec::new() }
    }

    /// Build a candidate set from an iterator of [`Cand`] read-views.
    pub fn from_iter_cands(iter: impl IntoIterator<Item = Cand<'a>>) -> Self {
        CandidateSet { cands: iter.into_iter().collect() }
    }

    /// Push one candidate into the set (filtering builders).
    pub fn push(&mut self, cand: Cand<'a>) {
        self.cands.push(cand);
    }

    /// The candidates as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[Cand<'a>] {
        &self.cands
    }

    /// The number of candidates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cands.len()
    }

    /// `true` iff the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cands.is_empty()
    }
}

// ─────────────────────────── Verdict / Resolved ─────────────────────────────

/// One layer's verdict over the working set (a pure narrowing step).
///
/// `Unique` short-circuits the fold (a definite single winner); `Narrowed`
/// hands a SMALLER working set to the next layer; `Abstain` passes the set
/// through unchanged. A layer NEVER widens the set or invents a winner.
///
/// `Cand` is `Copy` and carried BY VALUE (not boxed): the warm `Unique` path is
/// the hot path, so a heap box there would be strictly worse than the small
/// stack-only enum. The variant size spread is benign — these are short-lived
/// stack values, never stored in bulk.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum Verdict<'a> {
    /// A definite single winner — short-circuits the fold.
    Unique(Cand<'a>),
    /// A narrowed working set handed to the next layer.
    Narrowed(SmallVec<[Cand<'a>; 4]>),
    /// No opinion — the working set passes through unchanged.
    Abstain,
}

/// The terminal outcome of [`Selector::resolve_one`] — the SINGLE fail-fast
/// rule's product. `None`/`Ambiguous` are NON-COERCIBLE (you cannot pull a
/// `Cand` out of them), so "refuse to guess" is a TYPE, not a convention.
///
/// `One` carries the winning `Cand` BY VALUE (it is `Copy`); see [`Verdict`] for
/// why the by-value, non-boxed encoding is the right call on the hot path.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum Resolved<'a> {
    /// Exactly one winner.
    One(Cand<'a>),
    /// Zero candidates (an absence — `NoSuchBean` for a mandatory point).
    None,
    /// More than one candidate and no layer could choose (`NoUniqueBean`).
    Ambiguous(SmallVec<[Cand<'a>; 4]>),
}

impl<'a> Resolved<'a> {
    /// The winner iff this is [`Resolved::One`] (the only way to get a `Cand`).
    #[must_use]
    pub fn winner(&self) -> Option<&Cand<'a>> {
        match self {
            Resolved::One(c) => Some(c),
            Resolved::None | Resolved::Ambiguous(_) => None,
        }
    }

    /// `true` iff a unique winner was chosen.
    #[must_use]
    pub fn is_unique(&self) -> bool {
        matches!(self, Resolved::One(_))
    }
}

/// One layer of the fixed [`LAYERS`] fold: a name (for the trace) + a pure
/// evaluator `fn(&InjectionPoint, &[Cand]) -> Verdict`.
#[derive(Clone, Copy)]
pub struct Layer {
    /// The layer's stable name (recorded in the [`Trace`] on the `>1` path).
    pub name: &'static str,
    /// The pure narrowing evaluator.
    pub eval: for<'a> fn(&InjectionPoint, &[Cand<'a>]) -> Verdict<'a>,
}

impl std::fmt::Debug for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layer").field("name", &self.name).finish_non_exhaustive()
    }
}

/// The diagnostic fold trace — recorded ONLY on the `>1` (ambiguous) path so the
/// warm `len<=1` path stays allocation-free. Each entry is
/// `(layer_name, in_len, out_len)`.
#[derive(Clone, Debug, Default)]
pub struct Trace(pub SmallVec<[(&'static str, usize, usize); 8]>);

impl Trace {
    /// An empty trace.
    #[must_use]
    pub fn new() -> Self {
        Trace(SmallVec::new())
    }

    /// `true` iff no steps were recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The recorded `(layer, in_len, out_len)` steps.
    #[must_use]
    pub fn steps(&self) -> &[(&'static str, usize, usize)] {
        &self.0
    }
}

// ─────────────────────────── the layers ─────────────────────────────────────

/// The pure layer evaluators of the fixed [`LAYERS`] fold (injection-resolution).
///
/// Each is a `fn(&InjectionPoint, &[Cand]) -> Verdict`. They NARROW or ABSTAIN;
/// none widens the set or invents a winner. The fold driver
/// ([`Selector::resolve_one`]) owns short-circuiting and the SOLE len-rule.
pub mod layers {
    use super::{Cand, CandidateRole, InjectionPoint, Verdict};
    use smallvec::SmallVec;

    fn collect<'a>(it: impl Iterator<Item = Cand<'a>>) -> SmallVec<[Cand<'a>; 4]> {
        it.collect()
    }

    /// `generic_narrow` — keep only candidates whose generic-arg `TypeId`s match
    /// the point's `generics` (free generics-as-qualifier). Abstains when the
    /// point declares no generics.
    #[must_use]
    pub fn generic_narrow<'a>(ip: &InjectionPoint, set: &[Cand<'a>]) -> Verdict<'a> {
        if ip.generics.is_empty() {
            return Verdict::Abstain;
        }
        let kept = collect(
            set.iter()
                .copied()
                .filter(|c| c.generics == ip.generics),
        );
        if kept.len() == set.len() {
            Verdict::Abstain
        } else {
            Verdict::Narrowed(kept)
        }
    }

    /// `qualifier_narrow` — keep only candidates carrying every required typed
    /// MARKER qualifier (set-membership). Open-string qualifiers are handled by
    /// `qualifier_name`; this layer only sees the typed-marker dimension.
    #[must_use]
    pub fn qualifier_narrow<'a>(ip: &InjectionPoint, set: &[Cand<'a>]) -> Verdict<'a> {
        let required: SmallVec<[_; 4]> =
            ip.qualifiers.iter().filter_map(|q| q.marker).collect();
        if required.is_empty() {
            return Verdict::Abstain;
        }
        let kept = collect(
            set.iter()
                .copied()
                .filter(|c| required.iter().all(|m| c.has_marker(*m))),
        );
        if kept.len() == set.len() {
            Verdict::Abstain
        } else {
            Verdict::Narrowed(kept)
        }
    }

    /// `primary_promote` — the FIXED three-step fold (SEAMS C5):
    /// **STEP A FallbackDemote FIRST**, then PrimaryPromote, then the len-rule
    /// hand-off. So a soft `@Fallback` ALWAYS loses to a `Normal` of the same
    /// contract (FallbackDemote dominates primacy), and a unique `@Primary` wins.
    #[must_use]
    pub fn primary_promote<'a>(_ip: &InjectionPoint, set: &[Cand<'a>]) -> Verdict<'a> {
        if set.len() <= 1 {
            return Verdict::Abstain;
        }

        // STEP A — FallbackDemote: if any non-fallback exists, drop the soft
        // fallbacks (a @Fallback loses to any non-fallback of the same contract).
        let has_non_fallback = set.iter().any(|c| !c.role.is_fallback());
        let working: SmallVec<[Cand<'a>; 4]> = if has_non_fallback {
            collect(set.iter().copied().filter(|c| !c.role.is_fallback()))
        } else {
            collect(set.iter().copied())
        };

        // STEP B — PrimaryPromote: a unique @Primary at the min hierarchy_depth
        // wins; two primaries at the same min depth is a PrimaryConflict (folds
        // to Ambiguous via the terminal len-rule, so we Narrow to just them).
        let primaries: SmallVec<[Cand<'a>; 4]> =
            collect(working.iter().copied().filter(|c| c.role.is_primary()));
        if !primaries.is_empty() {
            let min_depth = primaries.iter().map(|c| c.hierarchy_depth).min().unwrap_or(0);
            let at_min: SmallVec<[Cand<'a>; 4]> =
                collect(primaries.iter().copied().filter(|c| c.hierarchy_depth == min_depth));
            return match at_min.as_slice() {
                [one] => Verdict::Unique(*one),
                // Two+ primaries at min depth: a conflict — narrow to them so the
                // terminal len-rule reports NoUniqueBean over exactly the primaries.
                _ => Verdict::Narrowed(at_min),
            };
        }

        // STEP C — no primaries: hand the (fallback-demoted) working set onward.
        if working.len() == set.len() {
            Verdict::Abstain
        } else {
            Verdict::Narrowed(working)
        }
    }

    /// `name_match` — the in-ladder name fallback: if a candidate's canonical
    /// name equals the injection point's declared `name`, it wins (Spring's
    /// by-name disambiguation). Abstains if no candidate name-matches.
    #[must_use]
    pub fn name_match<'a>(ip: &InjectionPoint, set: &[Cand<'a>]) -> Verdict<'a> {
        match set.iter().copied().find(|c| c.name == ip.name) {
            Some(hit) => Verdict::Unique(hit),
            None => Verdict::Abstain,
        }
    }

    /// `qualifier_name` — the open-STRING qualifier match (a `QualifierReq.name`
    /// other than the implicit param name): keep candidates whose canonical name
    /// equals a required string qualifier.
    #[must_use]
    pub fn qualifier_name<'a>(ip: &InjectionPoint, set: &[Cand<'a>]) -> Verdict<'a> {
        let required: SmallVec<[&'static str; 4]> =
            ip.qualifiers.iter().filter_map(|q| q.name).collect();
        if required.is_empty() {
            return Verdict::Abstain;
        }
        let kept = collect(
            set.iter()
                .copied()
                .filter(|c| required.contains(&c.name)),
        );
        if kept.is_empty() || kept.len() == set.len() {
            Verdict::Abstain
        } else {
            Verdict::Narrowed(kept)
        }
    }

    /// `priority_rank` — keep only the candidates that tie for the BEST
    /// `OrderKey` under the one [`cmp_order`](crate::cmp_order). A strict unique
    /// minimum is the winner; ties narrow (handed to the next layer / len-rule).
    #[must_use]
    pub fn priority_rank<'a>(_ip: &InjectionPoint, set: &[Cand<'a>]) -> Verdict<'a> {
        if set.len() <= 1 {
            return Verdict::Abstain;
        }
        let best = set
            .iter()
            .min_by(|a, b| super::cmp_order(&a.order, &b.order))
            .copied();
        let Some(best) = best else { return Verdict::Abstain };
        let winners: SmallVec<[Cand<'a>; 4]> = collect(
            set.iter()
                .copied()
                .filter(|c| super::cmp_order(&c.order, &best.order) == std::cmp::Ordering::Equal),
        );
        match winners.as_slice() {
            [one] => Verdict::Unique(*one),
            _ if winners.len() == set.len() => Verdict::Abstain,
            _ => Verdict::Narrowed(winners),
        }
    }

    /// `default_candidate` — the weak layer: drop `autowire_candidate = false`
    /// (default-candidate) beans from plain by-type autowiring, so a single
    /// remaining strong candidate wins (`get_if_unique`'s lever). Abstains if all
    /// candidates are strong or all are weak.
    #[must_use]
    pub fn default_candidate<'a>(_ip: &InjectionPoint, set: &[Cand<'a>]) -> Verdict<'a> {
        let strong: SmallVec<[Cand<'a>; 4]> =
            collect(set.iter().copied().filter(|c| c.autowire_candidate));
        if strong.is_empty() || strong.len() == set.len() {
            Verdict::Abstain
        } else {
            Verdict::Narrowed(strong)
        }
    }

    /// `resolvable_dep` — the terminal layer (lowest precedence): a placeholder
    /// for the `ResolvableDependency` (`HashMap<TypeId, ErasedBean>`) fold the
    /// registry/engine units flesh out. It currently abstains (the synthetic
    /// resolvable beans fold into the by-type count at the terminal len-rule).
    #[must_use]
    pub fn resolvable_dep<'a>(_ip: &InjectionPoint, _set: &[Cand<'a>]) -> Verdict<'a> {
        let _ = CandidateRole::NORMAL; // keep the import meaningful for future use
        Verdict::Abstain
    }
}

/// THE fixed Selector layer order (a kernel contract, NOT pluggable).
///
/// `[generic_narrow, qualifier_narrow, primary_promote, name_match,
/// qualifier_name, priority_rank, default_candidate, resolvable_dep]` —
/// literal Spring order, decomposed for testability/diagnostics. The
/// `primary_promote` slot is the fused FallbackDemote→PrimaryPromote→len fold.
pub const LAYERS: &[Layer] = &[
    Layer { name: "generic_narrow", eval: layers::generic_narrow },
    Layer { name: "qualifier_narrow", eval: layers::qualifier_narrow },
    Layer { name: "primary_promote", eval: layers::primary_promote },
    Layer { name: "name_match", eval: layers::name_match },
    Layer { name: "qualifier_name", eval: layers::qualifier_name },
    Layer { name: "priority_rank", eval: layers::priority_rank },
    Layer { name: "default_candidate", eval: layers::default_candidate },
    Layer { name: "resolvable_dep", eval: layers::resolvable_dep },
];

// ─────────────────────────── the Selector ───────────────────────────────────

/// THE selection spine (autowiring-resolution). A zero-state facade over the
/// fixed [`LAYERS`] fold; `Selector::resolve_one` is the only entry point.
pub struct Selector;

impl Selector {
    /// Resolve one injection point against a candidate set: the generic fold over
    /// the fixed [`LAYERS`] with the SINGLE terminal fail-fast rule.
    ///
    /// The fold threads a `SmallVec` working set through each layer, short-
    /// circuiting on the first [`Verdict::Unique`] or narrow-to-one; after all
    /// layers the SOLE rule applies: `len 0 -> None`, `len 1 -> One`,
    /// `len >1 -> Ambiguous`. The [`Trace`] is recorded ONLY when the input set
    /// has `>1` candidate (the warm `len<=1` path stays allocation-free).
    ///
    /// Multi-valued points ([`Arity::is_multiple`]) must NOT call this — they use
    /// [`collect_ordered`] (the collect-all + `cmp_order` `Cardinality::Multiple`
    /// path). Calling it for a multiple arity still folds, but the caller is
    /// responsible for routing.
    #[must_use]
    pub fn resolve_one<'a>(
        ip: &InjectionPoint,
        set: &CandidateSet<'a>,
    ) -> (Resolved<'a>, Option<Trace>) {
        let cands = set.as_slice();
        // The trace is recorded only on the >1 path (allocation-light warm path).
        let mut trace = if cands.len() > 1 { Some(Trace::new()) } else { None };

        let mut working: SmallVec<[Cand<'a>; 4]> = cands.iter().copied().collect();

        for layer in LAYERS {
            if working.len() <= 1 {
                break;
            }
            let in_len = working.len();
            let verdict = (layer.eval)(ip, &working);
            match verdict {
                Verdict::Unique(winner) => {
                    if let Some(t) = trace.as_mut() {
                        t.0.push((layer.name, in_len, 1));
                    }
                    return (Resolved::One(winner), trace);
                }
                Verdict::Narrowed(narrowed) => {
                    if let Some(t) = trace.as_mut() {
                        t.0.push((layer.name, in_len, narrowed.len()));
                    }
                    working = narrowed;
                }
                Verdict::Abstain => {
                    if let Some(t) = trace.as_mut() {
                        t.0.push((layer.name, in_len, in_len));
                    }
                }
            }
        }

        // THE SOLE fail-fast rule, applied in EXACTLY ONE place.
        let resolved = match working.as_slice() {
            [] => Resolved::None,
            [one] => Resolved::One(*one),
            _ => Resolved::Ambiguous(working),
        };
        (resolved, trace)
    }
}

/// Collect ALL qualifying candidates in `cmp_order`, the `Cardinality::Multiple`
/// path (collection-injection BYPASSES selection).
///
/// Selection layers (`primary_promote`/`name_match`/…) are STRUCTURALLY skipped;
/// this only sorts the already-filtered set by the one [`cmp_order`](crate::cmp_order)
/// (interface-source beats annotation-source before numerics; lower value wins;
/// `BeanId` registration order as the stable final tie-break). An empty set is an
/// empty collection (never `NoSuchBean`).
#[must_use]
pub fn collect_ordered<'a>(set: &CandidateSet<'a>) -> SmallVec<[Cand<'a>; 4]> {
    let mut v: SmallVec<[Cand<'a>; 4]> = set.as_slice().iter().copied().collect();
    v.sort_by(|a, b| cmp_order(&a.order, &b.order).then_with(|| a.id.0.cmp(&b.id.0)));
    v
}

// ─────────────────────────── error constructors ─────────────────────────────

/// Build a `NoSuchBean` for a missing mandatory point (zero candidates).
#[must_use]
pub fn no_such_bean(ip: &InjectionPoint) -> LeafError {
    LeafError::new(ErrorKind::NoSuchBean).caused_by(Cause::plain(
        "resolving injection point",
        format!(
            "no candidate bean for `{}` (type {:?})",
            ip.name, ip.produced
        ),
    ))
}

/// Build a `NoUniqueBean` naming the ambiguous candidates (the `>1` outcome).
#[must_use]
pub fn no_unique_bean(ip: &InjectionPoint, candidates: &[Cand<'_>]) -> LeafError {
    let names: Vec<&str> = candidates.iter().map(|c| c.name).collect();
    LeafError::new(ErrorKind::NoUniqueBean).caused_by(Cause::plain(
        "resolving injection point",
        format!(
            "{} candidates matched `{}`; expected exactly one — candidates: [{}]",
            candidates.len(),
            ip.name,
            names.join(", ")
        ),
    ))
}

/// Map a [`Resolved`] outcome to a `Result<Cand, LeafError>` honoring the
/// point's [`Arity`] absence tolerance (the terminal mapping the engine reads).
///
/// `Single`: `None -> NoSuchBean`. `Optional`: `None -> Ok(None)`. `Ambiguous`
/// is ALWAYS `NoUniqueBean` (ambiguity is never tolerated, even for `Optional` —
/// only `get_if_unique` swallows it, at the deferral layer).
///
/// # Errors
/// [`ErrorKind::NoSuchBean`] for a mandatory absent point; [`ErrorKind::NoUniqueBean`]
/// for any ambiguous point.
pub fn resolved_to_result<'a>(
    ip: &InjectionPoint,
    resolved: Resolved<'a>,
) -> Result<Option<Cand<'a>>, LeafError> {
    match resolved {
        Resolved::One(c) => Ok(Some(c)),
        Resolved::None => {
            if ip.arity.tolerates_absence() {
                Ok(None)
            } else {
                Err(no_such_bean(ip))
            }
        }
        Resolved::Ambiguous(cands) => Err(no_unique_bean(ip, &cands)),
    }
}

/// The COHERENCE seam check: REJECT a concrete-`TypeId` injection of an advised
/// bean as [`ErrorKind::AdvisedConcreteInjection`] (never a silent un-advised
/// raw-target handoff). An advised bean MUST be injected through a declared
/// service-trait view.
///
/// The `App<Wired>` validation pass calls this on every resolved winner: a
/// `Cand` that is `advised` AND matched by `concrete_match` is the error.
///
/// # Errors
/// [`ErrorKind::AdvisedConcreteInjection`] iff `winner.advised && winner.concrete_match`.
pub fn reject_advised_concrete(ip: &InjectionPoint, winner: &Cand<'_>) -> Result<(), LeafError> {
    if winner.advised && winner.concrete_match {
        Err(LeafError::new(ErrorKind::AdvisedConcreteInjection).caused_by(Cause::plain(
            "wiring injection point",
            format!(
                "bean `{}` is advised but injected at point `{}` by its concrete type; \
                 inject it through a declared service-trait view instead",
                winner.name, ip.name
            ),
        )))
    } else {
        Ok(())
    }
}

// ─────────────────────────── deferral handles ───────────────────────────────

/// The strictness ladder of a deferral resolution (deferral-primitives).
///
/// The three-tier ladder lives in ONE place so the most-missed semantic cannot
/// drift: `Strict` errors on BOTH absence and ambiguity; `AbsenceTolerant`
/// (`get_if_available`) returns `None` on absence but STILL errors on ambiguity;
/// `FullyTolerant` (`get_if_unique`) swallows both.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Strictness {
    /// Error on absence AND ambiguity (the basic `get`).
    Strict,
    /// `None` on absence, error on ambiguity (`get_if_available`).
    AbsenceTolerant,
    /// `None` on both absence and ambiguity (`get_if_unique`).
    FullyTolerant,
}

/// The cardinality of a deferral resolution (deferral-primitives).
///
/// `Single` routes through [`Selector::resolve_one`]; `Multiple` collects all +
/// `cmp_order` (selection bypassed).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cardinality {
    /// One bean (`resolve_one`).
    Single,
    /// All beans (collect-all + `cmp_order`).
    Multiple,
}

/// The ownership-model `Resolve` closure: the ONE driver every deferral handle
/// captures — `Fn(BeanKey, Strictness, Cardinality) -> BoxFuture<Result<Published,_>>`.
///
/// It closes over a [`Weak`] container back-reference (no `Arc` cycle), so a
/// `'static`-storable handle re-resolves the FULLY-built, already-proxied-if-
/// advised target on demand. The engine unit provides the concrete closure; the
/// kernel only pins the shape so the handles are origin-agnostic and dyn-safe.
pub type ResolveFn = dyn Fn(BeanKey, Strictness, Cardinality) -> BoxFuture<'static, Result<Published, LeafError>>
    + Send
    + Sync;

/// The captured resolve closure, ref-counted so each handle clone is cheap.
pub type Resolve = Arc<ResolveFn>;

/// The minimal container back-reference trait a deferral handle's [`Weak`] points
/// at. The engine/container unit implements it; the kernel only needs the
/// `resolve` entry point so [`Lookup`]/[`LazyRef`]/[`Inject`]/[`SelfRef`] are
/// defined here over a stable seam (`Send + Sync` so the `Weak` rides handles).
pub trait Container: Send + Sync {
    /// Resolve `key` with the given strictness/cardinality, publishing the
    /// origin-agnostic [`Published`] (or a [`LeafError`]).
    fn resolve(
        &self,
        key: BeanKey,
        strictness: Strictness,
        cardinality: Cardinality,
    ) -> BoxFuture<'_, Result<Published, LeafError>>;
}

/// The shared back-reference a deferral handle holds: a [`Weak`] to the
/// container (no `Arc` cycle — the container owns the handles transitively).
pub type ContainerRef = Weak<dyn Container>;

/// Build the error for a deferral handle whose container has been dropped.
///
/// [`ErrorKind::Cancelled`] (NOT `NoSuchBean`): a handle outliving its container
/// is a lifecycle/logic fault, surfaced honestly at the call site — and crucially
/// NOT swallowed as "tolerated absence" by [`Lookup::get_if_available`] /
/// [`Lookup::get_if_unique`] (those tolerate a missing BEAN, never a dead container).
fn container_gone() -> LeafError {
    LeafError::new(ErrorKind::Cancelled).caused_by(Cause::plain(
        "resolving deferral handle",
        "the owning container has been dropped",
    ))
}

/// `Lookup<T>` (= Spring's `ObjectProvider`): the lazy 0..N re-resolving handle.
///
/// The honest VISIBLE handle (not a `Deref`-to-`T`, not a transparent proxy):
/// failure is honest at the call site. `get`/`get_if_available`/`get_if_unique`
/// map the three [`Strictness`] tiers; `get_owned` is the prototype fresh-per-
/// call path; the stream methods are the lazy collection counterpart (same
/// [`cmp_order`] as the eager `Vec`, so ordering never diverges). Resolution
/// runs on the CALLER's task through the captured [`Resolve`] closure.
pub struct Lookup<T: ?Sized> {
    key: BeanKey,
    container: ContainerRef,
    _marker: std::marker::PhantomData<fn() -> Arc<T>>,
}

impl<T: ?Sized + 'static> Lookup<T> {
    /// Construct a `Lookup<T>` over a back-reference and the key it re-resolves.
    #[must_use]
    pub fn new(key: BeanKey, container: ContainerRef) -> Self {
        Lookup { key, container, _marker: std::marker::PhantomData }
    }

    /// The key this handle re-resolves on each call.
    #[must_use]
    pub fn key(&self) -> &BeanKey {
        &self.key
    }

    /// Resolve at the given strictness, returning the published handle (or the
    /// honest error). The single private driver every public method routes through.
    async fn resolve_with(&self, strictness: Strictness) -> Result<Published, LeafError> {
        let Some(container) = self.container.upgrade() else {
            return Err(container_gone());
        };
        container
            .resolve(self.key.clone(), strictness, Cardinality::Single)
            .await
    }

    /// `get` (Strict): error on both absence and ambiguity.
    ///
    /// Bounds `T: Sized` because the kernel's concrete downcast path uses the one
    /// [`downcast_ref`]; a `dyn Svc` (`?Sized`) target is the engine unit's
    /// upcast-row variant. The struct stays `?Sized` so `Lookup<dyn Svc>` is
    /// nameable as a field type.
    ///
    /// # Errors
    /// [`ErrorKind::NoSuchBean`]/[`ErrorKind::NoUniqueBean`] on absence/ambiguity.
    pub async fn get(&self) -> Result<Ref<T>, LeafError>
    where
        T: Sized + Send + Sync,
    {
        let published = self.resolve_with(Strictness::Strict).await?;
        published_to_ref::<T>(published)
    }

    /// `get_if_available` (AbsenceTolerant): `None` on absence, error on ambiguity.
    ///
    /// # Errors
    /// [`ErrorKind::NoUniqueBean`] on ambiguity (absence is `Ok(None)`).
    pub async fn get_if_available(&self) -> Result<Option<Ref<T>>, LeafError>
    where
        T: Sized + Send + Sync,
    {
        match self.resolve_with(Strictness::AbsenceTolerant).await {
            Ok(published) => published_to_ref::<T>(published).map(Some),
            Err(e) if e.kind == ErrorKind::NoSuchBean => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `get_if_unique` (FullyTolerant): `None` on BOTH absence and ambiguity.
    pub async fn get_if_unique(&self) -> Option<Ref<T>>
    where
        T: Sized + Send + Sync,
    {
        match self.resolve_with(Strictness::FullyTolerant).await {
            Ok(published) => published_to_ref::<T>(published).ok(),
            Err(_) => None,
        }
    }
}

impl<T: ?Sized> Clone for Lookup<T> {
    fn clone(&self) -> Self {
        Lookup {
            key: self.key.clone(),
            container: self.container.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: ?Sized> std::fmt::Debug for Lookup<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lookup").field("key", &self.key).finish_non_exhaustive()
    }
}

/// `LazyRef<T>` (= `@Lazy`): a `Lookup` that CACHES on first `get` for singleton
/// targets (`OnceCell`-backed), re-resolves for shorter scopes, and NEVER caches
/// a prototype target (the fresh-per-call contract).
///
/// `@Lazy` is fully subsumed: `LazyRef<T>` IS the injection-point case.
pub struct LazyRef<T: ?Sized> {
    lookup: Lookup<T>,
    cache: once_cell::sync::OnceCell<Ref<T>>,
}

impl<T: Send + Sync + 'static> LazyRef<T> {
    /// Construct a `LazyRef<T>` over a back-reference and the key it resolves.
    #[must_use]
    pub fn new(key: BeanKey, container: ContainerRef) -> Self {
        LazyRef { lookup: Lookup::new(key, container), cache: once_cell::sync::OnceCell::new() }
    }

    /// `true` iff the singleton target has already been resolved + cached.
    #[must_use]
    pub fn is_cached(&self) -> bool {
        self.cache.get().is_some()
    }

    /// Resolve (and, for a singleton target, cache) the bean. A `Strict` resolve;
    /// the cache stores the first successful `Ref<T>`.
    ///
    /// # Errors
    /// Propagates the underlying [`Lookup::get`] error on first resolution.
    pub async fn get(&self) -> Result<Ref<T>, LeafError> {
        if let Some(cached) = self.cache.get() {
            return Ok(cached.clone());
        }
        let resolved = self.lookup.get().await?;
        // First-writer wins; a racing initializer's value is dropped.
        let _ = self.cache.set(resolved.clone());
        Ok(self.cache.get().cloned().unwrap_or(resolved))
    }
}

impl<T: ?Sized> std::fmt::Debug for LazyRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyRef")
            .field("key", &self.lookup.key)
            .field("cached", &self.cache.get().is_some())
            .finish_non_exhaustive()
    }
}

/// `Inject<T>` (= jakarta `Provider<T>` parity): the strict-basic `get` handle.
///
/// Faithful in shape to JSR-330 `Provider` (the migration-familiar name kept,
/// the cross-ecosystem identity acknowledged-lost in Rust): `get()` is a
/// `Strict, Single` resolve, re-resolved each call (no cache).
pub struct Inject<T: ?Sized> {
    lookup: Lookup<T>,
}

impl<T: Send + Sync + 'static> Inject<T> {
    /// Construct an `Inject<T>` over a back-reference and the key it resolves.
    #[must_use]
    pub fn new(key: BeanKey, container: ContainerRef) -> Self {
        Inject { lookup: Lookup::new(key, container) }
    }

    /// `get` (Strict, Single, re-resolved each call) — jakarta `Provider::get`.
    ///
    /// # Errors
    /// [`ErrorKind::NoSuchBean`]/[`ErrorKind::NoUniqueBean`] on absence/ambiguity.
    pub async fn get(&self) -> Result<Ref<T>, LeafError> {
        self.lookup.get().await
    }
}

impl<T: ?Sized> std::fmt::Debug for Inject<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inject").field("key", &self.lookup.key).finish_non_exhaustive()
    }
}

/// `SelfRef<T>` (= self-injection, extra-2): the explicit self-injection handle.
///
/// A lazy erased closure over the SAME [`Weak`] container back-ref (NOT a strong
/// `Arc`-of-self, so there is no `Arc`-self-cycle and self never beats a real
/// collaborator — it is not in by-type matching at all). It returns the bean's
/// OWN published handle (the advised handle if the bean is advised; never a raw
/// un-advised self), resolved post-build on first call.
pub struct SelfRef<T: ?Sized> {
    own_key: BeanKey,
    container: ContainerRef,
    _marker: std::marker::PhantomData<fn() -> Arc<T>>,
}

impl<T: Send + Sync + 'static> SelfRef<T> {
    /// Construct a `SelfRef<T>` over the bean's OWN key + the container back-ref.
    /// The macro emits this with `is_self` set and NO normal candidate edge.
    #[must_use]
    pub fn new(own_key: BeanKey, container: ContainerRef) -> Self {
        SelfRef { own_key, container, _marker: std::marker::PhantomData }
    }

    /// Resolve the bean's OWN published (advised, if advised) handle post-build.
    ///
    /// # Errors
    /// [`ErrorKind::NoSuchBean`] if the owning container has been dropped.
    pub async fn get(&self) -> Result<Ref<T>, LeafError> {
        let Some(container) = self.container.upgrade() else {
            return Err(container_gone());
        };
        let published = container
            .resolve(self.own_key.clone(), Strictness::Strict, Cardinality::Single)
            .await?;
        published_to_ref::<T>(published)
    }
}

impl<T: ?Sized> Clone for SelfRef<T> {
    fn clone(&self) -> Self {
        SelfRef {
            own_key: self.own_key.clone(),
            container: self.container.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: ?Sized> std::fmt::Debug for SelfRef<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelfRef").field("own_key", &self.own_key).finish_non_exhaustive()
    }
}

/// Map a resolved [`Published`] to a typed `Ref<T>` for a CONCRETE (`Sized`)
/// target via the one [`downcast_ref`].
///
/// This is the kernel's honest concrete path: a `Lookup<Concrete>` round-trips
/// through `downcast_ref`. A `dyn Svc` (`?Sized`) target rides the engine unit's
/// `provides[]` upcast row — that variant is added by the engine, not here, so
/// the kernel's `get` methods bound `T: Sized`. A prototype `Published::Owned`
/// cannot become a `Ref<T>` (an owned move, not refcounted) — that is the
/// `get_owned` path (a later engine method).
fn published_to_ref<T: Send + Sync + 'static>(published: Published) -> Result<Ref<T>, LeafError> {
    match published {
        Published::Shared(bean) => downcast_ref::<T>(bean).map_err(|_| {
            LeafError::new(ErrorKind::NoSuchBean).caused_by(Cause::plain(
                "resolving deferral handle",
                "resolved bean's concrete type did not match the handle's target type",
            ))
        }),
        Published::Owned(_) => Err(LeafError::new(ErrorKind::NoSuchBean).caused_by(Cause::plain(
            "resolving deferral handle",
            "target is a prototype (owned move); use get_owned, not a Ref<T> handle",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::CandidateRole;
    use crate::handle::Published;
    use crate::identity::{BeanId, BeanKey, MarkerId};
    use crate::order::OrderSource;
    use std::any::TypeId;
    use std::sync::Arc;

    // ── small builders ──────────────────────────────────────────────────────

    fn ip_single() -> InjectionPoint {
        InjectionPoint::single(TypeId::of::<u32>(), "svc")
    }

    fn cand<'a>(id: u32, name: &'a str) -> Cand<'a> {
        Cand::new(BeanId(id), name)
    }

    fn set<'a>(cands: impl IntoIterator<Item = Cand<'a>>) -> CandidateSet<'a> {
        CandidateSet::from_iter_cands(cands)
    }

    // ── Selector::resolve_one ───────────────────────────────────────────────

    #[test]
    fn single_match_resolves_uniquely_with_no_trace() {
        let s = set([cand(0, "a")]);
        let (resolved, trace) = Selector::resolve_one(&ip_single(), &s);
        assert!(resolved.is_unique());
        assert_eq!(resolved.winner().unwrap().id, BeanId(0));
        // Warm len<=1 path is allocation-free: no trace.
        assert!(trace.is_none());
    }

    #[test]
    fn zero_candidates_resolve_to_none() {
        let s = set([]);
        let (resolved, trace) = Selector::resolve_one(&ip_single(), &s);
        assert!(matches!(resolved, Resolved::None));
        assert!(resolved.winner().is_none());
        assert!(trace.is_none());
    }

    #[test]
    fn ambiguity_with_no_tiebreak_is_ambiguous_and_traced() {
        // Two plain candidates, no primary/name/order tie-break, distinct names.
        let s = set([cand(0, "a"), cand(1, "b")]);
        let (resolved, trace) = Selector::resolve_one(&ip_single(), &s);
        match resolved {
            Resolved::Ambiguous(c) => assert_eq!(c.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        // The >1 path records a trace.
        assert!(trace.is_some());
        assert!(!trace.unwrap().is_empty());
    }

    #[test]
    fn ambiguity_maps_to_no_unique_bean_error() {
        let s = set([cand(0, "a"), cand(1, "b")]);
        let (resolved, _) = Selector::resolve_one(&ip_single(), &s);
        let err = resolved_to_result(&ip_single(), resolved).expect_err("ambiguous");
        assert_eq!(err.kind, ErrorKind::NoUniqueBean);
        // The diagnostic names the candidates.
        assert!(err.to_string().contains('a'));
    }

    // ── primary_promote: @Primary tie-break wins ────────────────────────────

    #[test]
    fn primary_tie_break_wins() {
        let mut primary = cand(1, "b");
        primary.role = CandidateRole::PRIMARY;
        let s = set([cand(0, "a"), primary]);
        let (resolved, _) = Selector::resolve_one(&ip_single(), &s);
        assert_eq!(resolved.winner().expect("primary wins").id, BeanId(1));
    }

    #[test]
    fn two_primaries_at_same_depth_are_ambiguous() {
        let mut p0 = cand(0, "a");
        p0.role = CandidateRole::PRIMARY;
        let mut p1 = cand(1, "b");
        p1.role = CandidateRole::PRIMARY;
        let s = set([p0, p1]);
        let (resolved, _) = Selector::resolve_one(&ip_single(), &s);
        // A PrimaryConflict folds to Ambiguous over exactly the primaries.
        match resolved {
            Resolved::Ambiguous(c) => assert_eq!(c.len(), 2),
            other => panic!("expected Ambiguous on primary conflict, got {other:?}"),
        }
    }

    #[test]
    fn local_primary_beats_parent_primary() {
        let mut local = cand(0, "local");
        local.role = CandidateRole::PRIMARY;
        local.hierarchy_depth = 0;
        let mut parent = cand(1, "parent");
        parent.role = CandidateRole::PRIMARY;
        parent.hierarchy_depth = 3;
        let s = set([parent, local]);
        let (resolved, _) = Selector::resolve_one(&ip_single(), &s);
        assert_eq!(resolved.winner().expect("local primary wins").id, BeanId(0));
    }

    // ── @Fallback loses to a user Normal (FallbackDemote runs FIRST) ─────────

    #[test]
    fn fallback_loses_to_user_normal() {
        // SEAMS C5: a @Fallback ALWAYS loses to a non-fallback of the same
        // contract — even when the fallback is ALSO @Primary (the ordering pin).
        let normal = cand(0, "userBean");
        let mut fallback = cand(1, "starterBean");
        fallback.role = CandidateRole::FALLBACK.primary(); // {Primary, fallback=true}
        let s = set([normal, fallback]);
        let (resolved, _) = Selector::resolve_one(&ip_single(), &s);
        // FallbackDemote drops the fallback FIRST, so the plain user bean wins —
        // the fallback's @Primary does NOT rescue it.
        assert_eq!(resolved.winner().expect("user normal wins").id, BeanId(0));
    }

    #[test]
    fn all_fallbacks_fall_through_and_a_primary_among_them_wins() {
        // No non-fallback: the fallbacks survive STEP A; a @Primary among them wins.
        let mut f0 = cand(0, "a");
        f0.role = CandidateRole::FALLBACK;
        let mut f1 = cand(1, "b");
        f1.role = CandidateRole::FALLBACK.primary();
        let s = set([f0, f1]);
        let (resolved, _) = Selector::resolve_one(&ip_single(), &s);
        assert_eq!(resolved.winner().expect("primary fallback wins").id, BeanId(1));
    }

    // ── name fallback ───────────────────────────────────────────────────────

    #[test]
    fn name_fallback_resolves_by_declared_param_name() {
        // Two plain candidates; the point's declared name matches one of them.
        let ip = InjectionPoint::single(TypeId::of::<u32>(), "beta");
        let s = set([cand(0, "alpha"), cand(1, "beta")]);
        let (resolved, _) = Selector::resolve_one(&ip, &s);
        assert_eq!(resolved.winner().expect("name match wins").id, BeanId(1));
    }

    #[test]
    fn primary_promote_runs_before_name_match() {
        // Fixed layer order (SEAMS): primary_promote (slot 3) precedes name_match
        // (slot 4). A @Primary must win over a mere name coincidence — the point
        // name matches the NON-primary candidate, yet the primary still wins.
        let ip = InjectionPoint::single(TypeId::of::<u32>(), "alpha");
        let name_hit = cand(0, "alpha"); // matches the point name, but plain
        let mut primary = cand(1, "beta");
        primary.role = CandidateRole::PRIMARY;
        let s = set([name_hit, primary]);
        let (resolved, _) = Selector::resolve_one(&ip, &s);
        assert_eq!(
            resolved.winner().expect("primary beats name coincidence").id,
            BeanId(1)
        );
    }

    // ── priority_rank uses cmp_order ────────────────────────────────────────

    #[test]
    fn priority_rank_lower_order_value_wins() {
        let mut lo = cand(0, "a");
        lo.order = OrderKey { value: 1, source: OrderSource::Annotation };
        let mut hi = cand(1, "b");
        hi.order = OrderKey { value: 5, source: OrderSource::Annotation };
        // Distinct names that don't match the point name, no primary: priority decides.
        let ip = InjectionPoint::single(TypeId::of::<u32>(), "svc");
        let s = set([hi, lo]);
        let (resolved, _) = Selector::resolve_one(&ip, &s);
        assert_eq!(resolved.winner().expect("lowest order wins").id, BeanId(0));
    }

    // ── default_candidate weak layer ────────────────────────────────────────

    #[test]
    fn default_candidate_weak_is_excluded_leaving_one_winner() {
        // A strong bean + a weak (autowire_candidate=false) bean: the weak one is
        // dropped from plain autowiring, leaving a unique strong winner.
        let strong = cand(0, "strong");
        let mut weak = cand(1, "weak");
        weak.autowire_candidate = false;
        let ip = InjectionPoint::single(TypeId::of::<u32>(), "svc");
        let s = set([strong, weak]);
        let (resolved, _) = Selector::resolve_one(&ip, &s);
        assert_eq!(resolved.winner().expect("strong wins").id, BeanId(0));
    }

    // ── missing mandatory -> NoSuchBean; optional -> Ok(None) ───────────────

    #[test]
    fn missing_mandatory_is_no_such_bean() {
        let s = set([]);
        let (resolved, _) = Selector::resolve_one(&ip_single(), &s);
        let err = resolved_to_result(&ip_single(), resolved).expect_err("missing");
        assert_eq!(err.kind, ErrorKind::NoSuchBean);
    }

    #[test]
    fn missing_optional_is_ok_none() {
        let ip = InjectionPoint::optional(TypeId::of::<u32>(), "svc");
        let s = set([]);
        let (resolved, _) = Selector::resolve_one(&ip, &s);
        let out = resolved_to_result(&ip, resolved).expect("optional absence tolerated");
        assert!(out.is_none());
    }

    #[test]
    fn optional_still_errors_on_ambiguity() {
        // Optional tolerates ABSENCE but NOT ambiguity.
        let ip = InjectionPoint::optional(TypeId::of::<u32>(), "svc");
        let s = set([cand(0, "a"), cand(1, "b")]);
        let (resolved, _) = Selector::resolve_one(&ip, &s);
        let err = resolved_to_result(&ip, resolved).expect_err("ambiguous even if optional");
        assert_eq!(err.kind, ErrorKind::NoUniqueBean);
    }

    // ── collection ordering by cmp_order ────────────────────────────────────

    #[test]
    fn collect_ordered_sorts_by_cmp_order_then_bean_id() {
        let mut a = cand(2, "a");
        a.order = OrderKey { value: 10, source: OrderSource::Implicit };
        let mut b = cand(0, "b");
        b.order = OrderKey { value: 5, source: OrderSource::Implicit };
        let mut c = cand(1, "c");
        c.order = OrderKey { value: 5, source: OrderSource::Interface }; // ties value, Interface wins
        let s = set([a, b, c]);
        let ordered = collect_ordered(&s);
        let ids: Vec<u32> = ordered.iter().map(|c| c.id.0).collect();
        // value 5 Interface (c) < value 5 Implicit (b) < value 10 (a).
        assert_eq!(ids, vec![1, 0, 2]);
    }

    #[test]
    fn collect_ordered_empty_set_is_empty_never_an_error() {
        let s = set([]);
        assert!(collect_ordered(&s).is_empty());
    }

    #[test]
    fn collection_arity_bypasses_selection() {
        let ip = InjectionPoint::collection(TypeId::of::<u32>(), "all", CollectionShape::Vec);
        assert!(ip.arity.is_multiple());
        assert!(ip.arity.tolerates_absence());
        let ip_map = InjectionPoint::collection(TypeId::of::<u32>(), "byName", CollectionShape::Map);
        assert_eq!(ip_map.arity, Arity::Map);
    }

    // ── AdvisedConcreteInjection rejection (COHERENCE seam) ──────────────────

    #[test]
    fn advised_concrete_injection_is_rejected() {
        let mut winner = cand(0, "repo");
        winner.advised = true;
        winner.concrete_match = true;
        let ip = ip_single();
        let err = reject_advised_concrete(&ip, &winner).expect_err("advised concrete rejected");
        assert_eq!(err.kind, ErrorKind::AdvisedConcreteInjection);
    }

    #[test]
    fn advised_through_service_view_is_allowed() {
        // Advised but matched via a dyn-Svc view (concrete_match=false): allowed.
        let mut winner = cand(0, "repo");
        winner.advised = true;
        winner.concrete_match = false;
        assert!(reject_advised_concrete(&ip_single(), &winner).is_ok());
    }

    #[test]
    fn unadvised_concrete_injection_is_allowed() {
        let mut winner = cand(0, "plain");
        winner.advised = false;
        winner.concrete_match = true;
        assert!(reject_advised_concrete(&ip_single(), &winner).is_ok());
    }

    // ── single-phase: deferral/self points are NOT construction edges ────────

    #[test]
    fn deferral_and_self_points_remove_the_construction_edge() {
        let mut deferral = ip_single();
        deferral.kind = PointKind::Deferral;
        assert!(deferral.is_deferred());
        assert!(!deferral.kind.is_construction_edge());

        let mut selfref = ip_single();
        selfref.kind = PointKind::SelfRef;
        assert!(selfref.is_deferred());

        let bean = ip_single();
        assert!(!bean.is_deferred());
        assert!(bean.kind.is_construction_edge());
    }

    #[test]
    fn injection_plan_construction_edges_exclude_deferral() {
        // A plan with one Bean edge + one Deferral edge: only the Bean is a
        // construction-graph edge (the deferral is the cycle break). `TypeId::of`
        // is not a stable const fn, so the slice is built at runtime + leaked to
        // a `&'static` (the real macro emits a const via a static).
        let bean = InjectionPoint::single(TypeId::of::<u32>(), "a");
        let mut deferral = InjectionPoint::single(TypeId::of::<u64>(), "b");
        deferral.kind = PointKind::Deferral;
        let leaked: &'static [InjectionPoint] = Box::leak(Box::new([bean, deferral]));
        let plan = InjectionPlan { points: leaked };
        let edges: Vec<&str> = plan.construction_edges().map(|p| p.name).collect();
        assert_eq!(edges, vec!["a"]);
    }

    // ── QualifierReq ────────────────────────────────────────────────────────

    #[test]
    fn qualifier_req_marker_and_name_constructors() {
        let m = QualifierReq::marker(MarkerId::of("leaf::q::Primary"));
        assert_eq!(m.marker, Some(MarkerId::of("leaf::q::Primary")));
        assert!(m.name.is_none());
        let n = QualifierReq::named("redis");
        assert_eq!(n.name, Some("redis"));
        assert!(QualifierReq::default().is_vacuous());
    }

    #[test]
    fn qualifier_narrow_keeps_only_marked_candidates() {
        // `InjectionPoint.qualifiers`/`generics` are `&'static` (the macro emits
        // const slices); a test pins them in statics. `Cand.markers` is `&'a`, so
        // it can borrow a local.
        const FAST_Q: MarkerId = MarkerId::of("leaf::q::Fast");
        static QUALS: [QualifierReq; 1] = [QualifierReq::marker(FAST_Q)];
        let fast_markers = [FAST_Q];
        let mut fast = cand(0, "fast");
        fast.markers = &fast_markers;
        let slow = cand(1, "slow");
        let ip = InjectionPoint {
            produced: TypeId::of::<u32>(),
            generics: &[],
            qualifiers: &QUALS,
            name: "svc",
            arity: Arity::Single,
            kind: PointKind::Bean,
            collection: None,
        };
        let s = set([fast, slow]);
        let (resolved, _) = Selector::resolve_one(&ip, &s);
        assert_eq!(resolved.winner().expect("only the marked one survives").id, BeanId(0));
    }

    // ── deferral handles over a Weak container back-ref ──────────────────────

    #[derive(Debug, PartialEq)]
    struct Svc {
        v: u32,
    }

    struct FakeContainer {
        // What `resolve` returns for any key.
        outcome: Outcome,
    }

    #[derive(Clone)]
    enum Outcome {
        Shared(u32),
        Absent,
        Ambiguous,
    }

    impl Container for FakeContainer {
        fn resolve(
            &self,
            _key: BeanKey,
            strictness: Strictness,
            _cardinality: Cardinality,
        ) -> BoxFuture<'_, Result<Published, LeafError>> {
            let outcome = self.outcome.clone();
            Box::pin(async move {
                match outcome {
                    Outcome::Shared(v) => Ok(Published::shared_value(Svc { v })),
                    Outcome::Absent => match strictness {
                        Strictness::Strict => Err(LeafError::new(ErrorKind::NoSuchBean)),
                        _ => Err(LeafError::new(ErrorKind::NoSuchBean)),
                    },
                    Outcome::Ambiguous => Err(LeafError::new(ErrorKind::NoUniqueBean)),
                }
            })
        }
    }

    fn container(outcome: Outcome) -> Arc<dyn Container> {
        Arc::new(FakeContainer { outcome })
    }

    #[test]
    fn lookup_handle_is_send_sync_and_clone() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Lookup<Svc>>();
        assert_send_sync::<LazyRef<Svc>>();
        assert_send_sync::<Inject<Svc>>();
        assert_send_sync::<SelfRef<Svc>>();
        let arc = container(Outcome::Shared(1));
        let weak = Arc::downgrade(&arc);
        let l: Lookup<Svc> = Lookup::new(BeanKey::ByName("svc".into()), weak);
        let _l2 = l.clone();
    }

    #[test]
    fn lookup_get_if_available_returns_none_on_absence() {
        let arc = container(Outcome::Absent);
        let weak = Arc::downgrade(&arc);
        let l: Lookup<Svc> = Lookup::new(BeanKey::ByName("svc".into()), weak);
        let out = futures::executor::block_on(l.get_if_available()).expect("absence tolerated");
        assert!(out.is_none());
    }

    #[test]
    fn lookup_get_if_available_still_errors_on_ambiguity() {
        let arc = container(Outcome::Ambiguous);
        let weak = Arc::downgrade(&arc);
        let l: Lookup<Svc> = Lookup::new(BeanKey::ByName("svc".into()), weak);
        let err = futures::executor::block_on(l.get_if_available())
            .expect_err("ambiguity is NOT tolerated by get_if_available");
        assert_eq!(err.kind, ErrorKind::NoUniqueBean);
    }

    #[test]
    fn lookup_get_if_unique_swallows_both_absence_and_ambiguity() {
        for outcome in [Outcome::Absent, Outcome::Ambiguous] {
            let arc = container(outcome);
            let weak = Arc::downgrade(&arc);
            let l: Lookup<Svc> = Lookup::new(BeanKey::ByName("svc".into()), weak);
            let out = futures::executor::block_on(l.get_if_unique());
            assert!(out.is_none());
        }
    }

    #[test]
    fn handle_errors_honestly_when_container_dropped() {
        let arc = container(Outcome::Shared(1));
        let weak = Arc::downgrade(&arc);
        let l: Lookup<Svc> = Lookup::new(BeanKey::ByName("svc".into()), weak);
        drop(arc); // container gone; the Weak no longer upgrades.
        // A dead container is Cancelled (a lifecycle fault), NOT swallowed as
        // "tolerated absence" — get_if_available surfaces it honestly.
        let err = futures::executor::block_on(l.get_if_available()).expect_err("container gone");
        assert_eq!(err.kind, ErrorKind::Cancelled);
    }

    #[test]
    fn lazyref_caches_after_first_get() {
        let arc = container(Outcome::Shared(7));
        let weak = Arc::downgrade(&arc);
        let lazy: LazyRef<Svc> = LazyRef::new(BeanKey::ByName("svc".into()), weak);
        assert!(!lazy.is_cached());
        let r = futures::executor::block_on(lazy.get()).expect("first get");
        assert_eq!(r.v, 7);
        assert!(lazy.is_cached());
        // Second get returns the cached handle (same allocation).
        let r2 = futures::executor::block_on(lazy.get()).expect("cached get");
        assert!(Arc::ptr_eq(r.as_arc(), r2.as_arc()));
    }

    #[test]
    fn lookup_get_resolves_a_shared_target() {
        let arc = container(Outcome::Shared(42));
        let weak = Arc::downgrade(&arc);
        let l: Lookup<Svc> = Lookup::new(BeanKey::ByName("svc".into()), weak);
        let r = futures::executor::block_on(l.get()).expect("resolved");
        assert_eq!(r.v, 42);
    }

    #[test]
    fn inject_get_is_strict_single() {
        let arc = container(Outcome::Shared(9));
        let weak = Arc::downgrade(&arc);
        let inj: Inject<Svc> = Inject::new(BeanKey::ByName("svc".into()), weak);
        let r = futures::executor::block_on(inj.get()).expect("strict get");
        assert_eq!(r.v, 9);
    }

    #[test]
    fn selfref_resolves_its_own_published_handle() {
        let arc = container(Outcome::Shared(5));
        let weak = Arc::downgrade(&arc);
        let s: SelfRef<Svc> = SelfRef::new(BeanKey::ByName("self".into()), weak);
        let r = futures::executor::block_on(s.get()).expect("self resolves");
        assert_eq!(r.v, 5);
    }

    // ── LAYERS shape ────────────────────────────────────────────────────────

    #[test]
    fn layers_are_the_fixed_eight_in_order() {
        let names: Vec<&str> = LAYERS.iter().map(|l| l.name).collect();
        assert_eq!(
            names,
            vec![
                "generic_narrow",
                "qualifier_narrow",
                "primary_promote",
                "name_match",
                "qualifier_name",
                "priority_rank",
                "default_candidate",
                "resolvable_dep",
            ]
        );
    }
}
