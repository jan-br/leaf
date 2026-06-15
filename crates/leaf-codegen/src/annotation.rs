//! The annotation / meta-annotation model (annotation-model, phase3/02).
//!
//! This is the compile-time engine the thin `#[component]`/`#[service]`/stereotype
//! macros call to flatten a *composed* annotation set into the ONE flat const
//! [`leaf_core::AnnotationMetadata`] row a [`leaf_core::Descriptor`] carries. It
//! runs ENTIRELY here (a normal, unit-testable library) at macro-expansion time;
//! the macro emits already-merged `&'static` const data, so the runtime sees only
//! O(1)/O(n) const reads (design doc `annotation-model`).
//!
//! Three jobs, all pure functions over the parsed model:
//!
//! 1. **composed-annotation merge** — meta-presence flatten: a stereotype's
//!    transitive marker closure (`@RestController` ⇒ `[RestController, Controller,
//!    Component]`) plus attribute inheritance with **distance-ordered nearest-wins**
//!    (a nearer meta level overrides a farther one).
//! 2. **attribute aliasing + the `@AliasFor` validator** — explicit aliases
//!    (two local attributes are the same value) and meta-annotation attribute
//!    overrides (a local attribute forwards into a base marker's attribute),
//!    validated at expansion (reciprocity / same default / no conflicting alias)
//!    → an [`AliasError`] the macro turns into `compile_error!` (strictly earlier
//!    than Spring).
//! 3. **annotation distance** — the rank used to order attribute sources so the
//!    nearest declaration wins.
//!
//! The lowering ([`AnnotationModel::lower`]) targets the FROZEN core shape — the
//! flat `AnnotationMetadata { qualifiers, markers, depends_on, candidate_role,
//! autowire_candidate }` (NOT the open `attrs` bag the early design sketch
//! floated; that never landed in core). Attributes are a codegen-internal driver:
//! they feed alias resolution and populate the typed fields, then are discarded.

use std::collections::BTreeMap;

use proc_macro2::TokenStream;
use quote::quote;

/// A codegen-internal attribute value parsed from an annotation argument.
///
/// This is the value vocabulary the merge/alias engine reasons over; it is NOT a
/// core type (the frozen `AnnotationMetadata` has no open `attrs` bag). Values
/// carry enough structure to compare for the `@AliasFor` "identical default"
/// check and to drive nearest-wins overriding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttrValue {
    /// A boolean literal (`primary = true`).
    Bool(bool),
    /// An integer literal (`order = 5`).
    Int(i64),
    /// A string literal (`name = "userService"`).
    Str(String),
    /// A path / ident value (`role = Role::Infrastructure`), kept as its rendered
    /// path string for comparison and re-emission.
    Path(String),
    /// A list of values (`markers = [Primary, Fast]`).
    List(Vec<AttrValue>),
}

/// One `@AliasFor` declaration on an annotation attribute.
///
/// Two shapes (Spring's two `@AliasFor` modes):
/// - **explicit alias** (`target_marker = None`): two attributes *within the same
///   annotation* are mirror images — setting one sets the other.
/// - **meta override** (`target_marker = Some(path)`): a local attribute forwards
///   into an attribute of a composed (meta) annotation, overriding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasFor {
    /// The local attribute that carries the `@AliasFor`.
    pub from_attr: String,
    /// The target marker path, or `None` for a same-annotation explicit alias.
    pub target_marker: Option<String>,
    /// The target attribute the alias forwards to.
    pub target_attr: String,
}

/// A parsed annotation: a canonical marker path, its explicitly-set attributes,
/// its `@AliasFor` declarations, the meta-annotations it is composed from, and the
/// default values its schema declares.
///
/// `meta` are the annotations *this* annotation is itself annotated with (its
/// composition edges) — the merge walks them to build the transitive marker
/// closure and to inherit attributes at increasing distance.
#[derive(Clone, Debug, Default)]
pub struct Annotation {
    /// The canonical marker path (e.g. `"leaf::Component"`); also the `MarkerId`
    /// minting input.
    pub path: String,
    /// Attributes explicitly set at this annotation's use site.
    pub attrs: BTreeMap<String, AttrValue>,
    /// The attribute defaults this annotation's schema declares (used by the
    /// `@AliasFor` "identical default" check and as the lowest-precedence source).
    pub defaults: BTreeMap<String, AttrValue>,
    /// `@AliasFor` declarations on this annotation's attributes.
    pub aliases: Vec<AliasFor>,
    /// The meta-annotations this annotation is composed from (its closure edges).
    pub meta: Vec<Annotation>,
}

impl Annotation {
    /// Construct a bare annotation with just a marker path.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Annotation { path: path.into(), ..Annotation::default() }
    }

    /// Builder: set an explicit attribute.
    #[must_use]
    pub fn with_attr(mut self, key: impl Into<String>, value: AttrValue) -> Self {
        self.attrs.insert(key.into(), value);
        self
    }

    /// Builder: declare a schema default for an attribute.
    #[must_use]
    pub fn with_default(mut self, key: impl Into<String>, value: AttrValue) -> Self {
        self.defaults.insert(key.into(), value);
        self
    }

    /// Builder: add an `@AliasFor` declaration.
    #[must_use]
    pub fn with_alias(mut self, alias: AliasFor) -> Self {
        self.aliases.push(alias);
        self
    }

    /// Builder: add a meta-annotation (a composition edge).
    #[must_use]
    pub fn with_meta(mut self, meta: Annotation) -> Self {
        self.meta.push(meta);
        self
    }
}

/// The product of [`merge`]: the flattened, distance-ordered view of a composed
/// annotation set.
///
/// `markers` is the transitive marker closure (self first, then each reachable
/// meta-annotation in increasing distance, deduplicated nearest-wins). This is the
/// data the lowering drops into `Descriptor.meta.markers`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergedAnnotation {
    /// The flattened marker paths, nearest-first, deduplicated.
    pub markers: Vec<String>,
    /// The distance (hop count from the root) at which each marker was first
    /// reached. The root marker is at distance 0; nearest-wins.
    pub distances: BTreeMap<String, usize>,
    /// The resolved attributes, nearest-wins by distance: for each attribute key,
    /// the value declared at the smallest distance (explicit beats default at the
    /// same distance).
    pub attrs: BTreeMap<String, AttrValue>,
}

impl MergedAnnotation {
    /// The distance (BFS hop count from the root) at which `marker` was first
    /// reached, or `None` if it is not in the closure.
    ///
    /// This is the rank nearest-wins attribute resolution orders on: a nearer
    /// source overrides a farther one.
    #[must_use]
    pub fn distance_of(&self, marker: &str) -> Option<usize> {
        self.distances.get(marker).copied()
    }

    /// The resolved value of attribute `key`, nearest-wins, or `None` if no source
    /// in the closure declares it.
    #[must_use]
    pub fn attr(&self, key: &str) -> Option<&AttrValue> {
        self.attrs.get(key)
    }

    /// Lower this merged annotation to a [`leaf_core::AnnotationMetadata`] const
    /// expression, emitted as a [`TokenStream`] of ABSOLUTE `::leaf_core` paths.
    ///
    /// This is the one field annotation-model OWNS: the result drops verbatim into
    /// `Descriptor.meta`. Every path is absolute (`::leaf_core::…`) so a user
    /// crate's imports cannot shadow them, matching the thin-macro rule. The frozen
    /// closed schema is targeted exactly: `markers` is the flattened distance-
    /// ordered marker closure; `qualifiers`/`depends_on` come from the `qualifiers`
    /// / `depends_on` attributes; `candidate_role` from `primary`/`fallback`;
    /// `autowire_candidate` from the `autowire_candidate` opt-out (default `true`).
    #[must_use]
    pub fn lower(&self) -> TokenStream {
        let markers = self.markers.iter().map(|m| {
            quote! { ::leaf_core::MarkerId::of(#m) }
        });
        let qualifiers = self.str_list("qualifiers").into_iter().map(|q| {
            quote! { ::leaf_core::MarkerId::of(#q) }
        });
        let depends_on = self.str_list("depends_on").into_iter().map(|d| {
            quote! { ::leaf_core::ContractId::of(#d) }
        });
        let candidate_role = self.candidate_role_tokens();
        let autowire = self.bool_attr("autowire_candidate", true);

        quote! {
            ::leaf_core::AnnotationMetadata {
                qualifiers: &[ #(#qualifiers),* ],
                markers: &[ #(#markers),* ],
                depends_on: &[ #(#depends_on),* ],
                candidate_role: #candidate_role,
                autowire_candidate: #autowire,
            }
        }
    }

    /// The string elements of a list-valued attribute (or empty if unset/non-list).
    fn str_list(&self, key: &str) -> Vec<String> {
        match self.attrs.get(key) {
            Some(AttrValue::List(items)) => items
                .iter()
                .filter_map(|v| match v {
                    AttrValue::Str(s) => Some(s.clone()),
                    AttrValue::Path(p) => Some(p.clone()),
                    _ => None,
                })
                .collect(),
            // A single string is treated as a one-element list (ergonomic).
            Some(AttrValue::Str(s)) => vec![s.clone()],
            _ => Vec::new(),
        }
    }

    /// Read a boolean attribute, falling back to `default` when unset/non-bool.
    fn bool_attr(&self, key: &str, default: bool) -> bool {
        match self.attrs.get(key) {
            Some(AttrValue::Bool(b)) => *b,
            _ => default,
        }
    }

    /// Build the `::leaf_core::CandidateRole` expression from the `primary` /
    /// `fallback` boolean attributes (SEAMS C5: the 2-axis role — a `@Fallback`
    /// CAN also be `@Primary`, so the axes compose).
    fn candidate_role_tokens(&self) -> TokenStream {
        let primary = self.bool_attr("primary", false);
        let fallback = self.bool_attr("fallback", false);
        match (primary, fallback) {
            (false, false) => quote! { ::leaf_core::CandidateRole::NORMAL },
            (true, false) => quote! { ::leaf_core::CandidateRole::PRIMARY },
            (false, true) => quote! { ::leaf_core::CandidateRole::FALLBACK },
            // A @Fallback that is ALSO @Primary: the FALLBACK base with .primary().
            (true, true) => quote! { ::leaf_core::CandidateRole::FALLBACK.primary() },
        }
    }
}

/// Composed-annotation merge: flatten a composed annotation (self + its transitive
/// meta-annotation closure) into one distance-ordered, deduplicated view.
///
/// The walk is breadth-first from the root so a marker reached at the SHORTEST
/// distance is the one kept (nearest-wins); a diamond (two paths to the same
/// marker) yields exactly one occurrence at its nearest distance. The root's own
/// marker is always at distance 0 and therefore always first.
#[must_use]
pub fn merge(root: &Annotation) -> MergedAnnotation {
    let mut markers = Vec::new();
    let mut distances: BTreeMap<String, usize> = BTreeMap::new();
    let mut attrs: BTreeMap<String, AttrValue> = BTreeMap::new();
    // The (distance, tier) at which each attribute's current value was resolved;
    // a strictly-smaller key overrides. tier 0 = explicit, tier 1 = default, so an
    // explicit attribute beats a default at the SAME distance.
    let mut attr_rank: BTreeMap<String, (usize, u8)> = BTreeMap::new();
    // BFS over the composition DAG, keeping the first (nearest) occurrence.
    let mut frontier: Vec<&Annotation> = vec![root];
    let mut depth = 0usize;
    while !frontier.is_empty() {
        let mut next: Vec<&Annotation> = Vec::new();
        for ann in &frontier {
            if !distances.contains_key(&ann.path) {
                markers.push(ann.path.clone());
                distances.insert(ann.path.clone(), depth);
            }
            // Explicit attributes (tier 0) then schema defaults (tier 1).
            for (k, v) in &ann.attrs {
                consider_attr(&mut attrs, &mut attr_rank, k, v, (depth, 0));
            }
            for (k, v) in &ann.defaults {
                consider_attr(&mut attrs, &mut attr_rank, k, v, (depth, 1));
            }
            for m in &ann.meta {
                next.push(m);
            }
        }
        frontier = next;
        depth += 1;
    }
    MergedAnnotation { markers, distances, attrs }
}

// ─────────────────────── @AliasFor: alias resolution ────────────────────────

/// A malformed-`@AliasFor` (or otherwise invalid alias) diagnostic.
///
/// The thin macro turns this into a `compile_error!` at expansion (Tier 0 in the
/// design doc's timing tiers) — strictly earlier than Spring, which only fails at
/// first reflective read. Carries a human-readable message for the macro to emit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AliasError {
    /// The annotation marker path the offending alias is declared on.
    pub on_marker: String,
    /// The human-readable explanation (emitted verbatim by `compile_error!`).
    pub message: String,
}

impl std::fmt::Display for AliasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@AliasFor on `{}`: {}", self.on_marker, self.message)
    }
}

impl std::error::Error for AliasError {}

/// Resolve a composed annotation: validate its `@AliasFor` graph, apply alias
/// forwarding (explicit mirrors + meta-annotation attribute overrides), then merge
/// (transitive marker closure + distance-ordered nearest-wins attributes).
///
/// This is the one entry point the lowering calls. It returns an [`AliasError`]
/// (→ `compile_error!`) on any malformed alias rather than silently mis-merging.
pub fn resolve(root: &Annotation) -> Result<MergedAnnotation, AliasError> {
    let applied = apply_aliases(root)?;
    Ok(merge(&applied))
}

/// Validate every `@AliasFor` in the composition tree and return an augmented copy
/// of the annotation with alias forwarding applied (explicit mirrors materialized,
/// meta overrides pushed into their target meta nodes).
fn apply_aliases(ann: &Annotation) -> Result<Annotation, AliasError> {
    let mut out = ann.clone();
    // Recurse first so meta nodes are themselves alias-applied.
    out.meta = ann
        .meta
        .iter()
        .map(apply_aliases)
        .collect::<Result<Vec<_>, _>>()?;

    for alias in &ann.aliases {
        validate_alias(ann, alias)?;
        let Some(value) = ann.attrs.get(&alias.from_attr).cloned() else {
            // Nothing set at the use site => nothing to forward (the default flows
            // through the normal merge).
            continue;
        };
        match &alias.target_marker {
            // Explicit same-annotation alias: mirror the value onto the target.
            None => {
                out.attrs
                    .entry(alias.target_attr.clone())
                    .or_insert(value);
            }
            // Meta override: push the value into the matching meta node's attrs so
            // the merge's nearest-wins picks the override over the base default.
            Some(marker) => {
                forward_into_meta(&mut out.meta, marker, &alias.target_attr, &value);
            }
        }
    }
    Ok(out)
}

/// Push `value` into attribute `attr` of the meta node whose path is `marker`
/// (transitively), so the override is visible to the distance fold.
fn forward_into_meta(meta: &mut [Annotation], marker: &str, attr: &str, value: &AttrValue) {
    for node in meta.iter_mut() {
        if node.path == marker {
            node.attrs.insert(attr.to_string(), value.clone());
        }
        forward_into_meta(&mut node.meta, marker, attr, value);
    }
}

/// Validate a single `@AliasFor` declaration against the schema reciprocity /
/// same-default / no-conflict rules. Errors are Tier-0 `compile_error!`s.
fn validate_alias(ann: &Annotation, alias: &AliasFor) -> Result<(), AliasError> {
    let err = |message: String| AliasError { on_marker: ann.path.clone(), message };

    // An alias must not point at itself.
    if alias.target_marker.is_none() && alias.from_attr == alias.target_attr {
        return Err(err(format!(
            "attribute `{}` is aliased to itself",
            alias.from_attr
        )));
    }

    // No two aliases on this annotation may forward the SAME from_attr to
    // different targets (a conflicting alias). Checked BEFORE reciprocity so a
    // genuinely-conflicting declaration is reported as such, not as a reciprocity
    // failure of one of the conflicting halves.
    let conflict = ann.aliases.iter().any(|other| {
        !std::ptr::eq(other, alias)
            && other.from_attr == alias.from_attr
            && (other.target_marker != alias.target_marker
                || other.target_attr != alias.target_attr)
    });
    if conflict {
        return Err(err(format!(
            "attribute `{}` has conflicting @AliasFor targets",
            alias.from_attr
        )));
    }

    // Explicit (same-annotation) alias: reciprocity + identical default.
    if alias.target_marker.is_none() {
        // Reciprocity: the target must declare the mirror @AliasFor back.
        let reciprocal = ann.aliases.iter().any(|other| {
            other.target_marker.is_none()
                && other.from_attr == alias.target_attr
                && other.target_attr == alias.from_attr
        });
        if !reciprocal {
            return Err(err(format!(
                "explicit alias `{}` -> `{}` is not reciprocal (the target must \
                 declare the mirror @AliasFor)",
                alias.from_attr, alias.target_attr
            )));
        }
        // Identical default: both attributes must declare the SAME default.
        if let (Some(a), Some(b)) =
            (ann.defaults.get(&alias.from_attr), ann.defaults.get(&alias.target_attr))
            && a != b
        {
            return Err(err(format!(
                "aliased attributes `{}` and `{}` declare different defaults",
                alias.from_attr, alias.target_attr
            )));
        }
    }
    Ok(())
}

/// Fold one candidate attribute value into the resolved map, keeping the
/// nearest-wins (smallest `(distance, tier)`) value.
fn consider_attr(
    attrs: &mut BTreeMap<String, AttrValue>,
    attr_rank: &mut BTreeMap<String, (usize, u8)>,
    key: &str,
    value: &AttrValue,
    rank: (usize, u8),
) {
    match attr_rank.get(key) {
        Some(&existing) if existing <= rank => {} // a nearer source already won
        _ => {
            attrs.insert(key.to_string(), value.clone());
            attr_rank.insert(key.to_string(), rank);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `@Component` — the root marker every stereotype transitively implies.
    fn component() -> Annotation {
        Annotation::new("leaf::Component")
    }

    /// `@Controller` — composed of `@Component` (a one-hop meta-edge).
    fn controller() -> Annotation {
        Annotation::new("leaf::Controller").with_meta(component())
    }

    /// `@RestController` — composed of `@Controller` (two hops to `@Component`).
    fn rest_controller() -> Annotation {
        Annotation::new("leaf::RestController").with_meta(controller())
    }

    // ── composed-annotation merge: transitive marker closure ──────────────────

    #[test]
    fn composed_annotation_merges_transitive_marker_closure() {
        // The headline case: @RestController = @Controller = @Component. The merge
        // flattens the WHOLE closure (self + every reachable meta) into one marker
        // set, nearest-first, deduplicated.
        let merged = merge(&rest_controller());
        assert_eq!(
            merged.markers,
            vec![
                "leaf::RestController".to_string(),
                "leaf::Controller".to_string(),
                "leaf::Component".to_string(),
            ],
            "the flattened set is the self marker + the transitive meta closure"
        );
    }

    // ── annotation distance ranks ─────────────────────────────────────────────

    #[test]
    fn distance_ranks_nearest_to_farthest() {
        // @RestController(0) → @Controller(1) → @Component(2). Distance is the
        // hop count from the root; it is what nearest-wins attribute resolution
        // ranks on.
        let merged = merge(&rest_controller());
        assert_eq!(merged.distance_of("leaf::RestController"), Some(0));
        assert_eq!(merged.distance_of("leaf::Controller"), Some(1));
        assert_eq!(merged.distance_of("leaf::Component"), Some(2));
        // A marker not in the closure has no distance.
        assert_eq!(merged.distance_of("leaf::Service"), None);
    }

    #[test]
    fn distance_keeps_the_shorter_path_in_a_diamond() {
        // Diamond: @Component is reachable at distance 2 via A and also at distance
        // 1 directly. The SHORTER distance wins (BFS nearest-first).
        let a = Annotation::new("leaf::A").with_meta(component());
        let near = Annotation::new("leaf::Near")
            .with_meta(component()) // distance 1
            .with_meta(a); // also reaches @Component at distance 2
        let merged = merge(&near);
        assert_eq!(
            merged.distance_of("leaf::Component"),
            Some(1),
            "nearest path wins"
        );
    }

    // ── attribute inheritance: distance-ordered nearest-wins ──────────────────

    #[test]
    fn merge_resolves_attributes_nearest_wins() {
        // @Component declares `name=""` (a far default). A composed @Service sets
        // `name="svc"` at distance 0. The NEAREST set value wins.
        let component_with_attr = Annotation::new("leaf::Component")
            .with_attr("name", AttrValue::Str(String::new()));
        let service = Annotation::new("leaf::Service")
            .with_attr("name", AttrValue::Str("svc".into()))
            .with_meta(component_with_attr);
        let merged = merge(&service);
        assert_eq!(
            merged.attr("name"),
            Some(&AttrValue::Str("svc".into())),
            "the nearest (distance-0) explicit attribute wins"
        );
    }

    #[test]
    fn merge_inherits_attribute_from_meta_when_nearer_absent() {
        // The root does not set `marker`; a meta-annotation does. The inherited
        // value flows up (a farther source fills an unset nearer one).
        let qualifier = Annotation::new("leaf::Qualifier")
            .with_attr("value", AttrValue::Str("fast".into()));
        let composed = Annotation::new("leaf::Fast").with_meta(qualifier);
        let merged = merge(&composed);
        assert_eq!(merged.attr("value"), Some(&AttrValue::Str("fast".into())));
    }

    // ── @AliasFor alias resolution (happy path) ───────────────────────────────

    #[test]
    fn explicit_alias_mirrors_within_the_same_annotation() {
        // Spring's same-annotation @AliasFor: `value` and `name` are mirror images.
        // Setting `value` makes `name` resolve to the same thing (and vice versa).
        // Spring requires reciprocity: BOTH attributes declare @AliasFor at each
        // other (and share a default).
        let ann = Annotation::new("leaf::Service")
            .with_attr("value", AttrValue::Str("userService".into()))
            .with_default("value", AttrValue::Str(String::new()))
            .with_default("name", AttrValue::Str(String::new()))
            .with_alias(AliasFor {
                from_attr: "value".into(),
                target_marker: None,
                target_attr: "name".into(),
            })
            .with_alias(AliasFor {
                from_attr: "name".into(),
                target_marker: None,
                target_attr: "value".into(),
            });
        let merged = resolve(&ann).expect("a well-formed explicit alias resolves");
        assert_eq!(merged.attr("value"), Some(&AttrValue::Str("userService".into())));
        assert_eq!(
            merged.attr("name"),
            Some(&AttrValue::Str("userService".into())),
            "the explicit alias mirrors the value onto its target attribute"
        );
    }

    #[test]
    fn meta_override_alias_forwards_into_a_base_marker_attribute() {
        // Spring's meta @AliasFor: a composed annotation's local attribute forwards
        // into (overrides) an attribute of a meta-annotation it is composed from.
        // @MyComponent(name="x") forwards `name` into @Component's `name`.
        let component = Annotation::new("leaf::Component")
            .with_attr("name", AttrValue::Str(String::new()));
        let my = Annotation::new("leaf::MyComponent")
            .with_attr("name", AttrValue::Str("x".into()))
            .with_alias(AliasFor {
                from_attr: "name".into(),
                target_marker: Some("leaf::Component".into()),
                target_attr: "name".into(),
            })
            .with_meta(component);
        let merged = resolve(&my).expect("a well-formed meta override resolves");
        // The local override beats @Component's far default.
        assert_eq!(merged.attr("name"), Some(&AttrValue::Str("x".into())));
    }

    // ── @AliasFor validator: conflicting / non-reciprocal / bad default ────────

    #[test]
    fn conflicting_aliases_error() {
        // The SAME attribute aliased to two different targets is a hard error.
        let ann = Annotation::new("leaf::Bad")
            .with_alias(AliasFor {
                from_attr: "value".into(),
                target_marker: None,
                target_attr: "name".into(),
            })
            .with_alias(AliasFor {
                from_attr: "value".into(),
                target_marker: None,
                target_attr: "id".into(),
            });
        let err = resolve(&ann).expect_err("conflicting alias targets must error");
        assert_eq!(err.on_marker, "leaf::Bad");
        assert!(err.message.contains("conflicting"), "got: {}", err.message);
    }

    #[test]
    fn non_reciprocal_explicit_alias_errors() {
        // An explicit (same-annotation) alias that the target does not mirror back
        // is a hard error (Spring's reciprocity rule, enforced at expansion).
        let ann = Annotation::new("leaf::OneWay").with_alias(AliasFor {
            from_attr: "value".into(),
            target_marker: None,
            target_attr: "name".into(),
        });
        let err = resolve(&ann).expect_err("a one-way explicit alias must error");
        assert!(err.message.contains("reciprocal"), "got: {}", err.message);
    }

    #[test]
    fn mismatched_aliased_defaults_error() {
        // Reciprocal but the two aliased attributes declare DIFFERENT defaults —
        // Spring requires identical defaults; flagged at expansion.
        let ann = Annotation::new("leaf::Mismatch")
            .with_default("value", AttrValue::Str("a".into()))
            .with_default("name", AttrValue::Str("b".into()))
            .with_alias(AliasFor {
                from_attr: "value".into(),
                target_marker: None,
                target_attr: "name".into(),
            })
            .with_alias(AliasFor {
                from_attr: "name".into(),
                target_marker: None,
                target_attr: "value".into(),
            });
        let err = resolve(&ann).expect_err("mismatched defaults must error");
        assert!(err.message.contains("default"), "got: {}", err.message);
    }

    #[test]
    fn self_alias_errors() {
        // An attribute aliased to itself is meaningless and rejected.
        let ann = Annotation::new("leaf::SelfRef").with_alias(AliasFor {
            from_attr: "value".into(),
            target_marker: None,
            target_attr: "value".into(),
        });
        let err = resolve(&ann).expect_err("a self-alias must error");
        assert!(err.message.contains("itself"), "got: {}", err.message);
    }

    // ── lowering to leaf_core::AnnotationMetadata const data ──────────────────

    /// Render a `TokenStream` to a whitespace-collapsed string so assertions are
    /// robust against `quote!`'s token spacing (`:: leaf_core` vs `::leaf_core`).
    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    #[test]
    fn lower_emits_absolute_core_annotation_metadata() {
        // A plain @Component lowers to a const AnnotationMetadata expression using
        // ABSOLUTE ::leaf_core paths (the macro hard-codes these so user-crate
        // imports cannot shadow them).
        let merged = resolve(&component()).expect("resolves");
        let ts = merged.lower();
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::AnnotationMetadata"), "got: {s}");
        // The marker closure lowers to ::leaf_core::MarkerId::of("path").
        assert!(s.contains(r#"::leaf_core::MarkerId::of("leaf::Component")"#), "got: {s}");
        // Every frozen field is named (closed-schema const).
        for field in [
            "markers", "qualifiers", "depends_on", "candidate_role", "autowire_candidate",
        ] {
            assert!(s.contains(&format!("{field}:")), "missing field {field} in: {s}");
        }
    }

    #[test]
    fn lower_marker_closure_is_distance_ordered() {
        // The emitted `markers` array preserves nearest-first order (self, then the
        // meta closure) so the runtime `meta.markers` reads in the same order.
        let merged = resolve(&rest_controller()).expect("resolves");
        let s = flat(&merged.lower());
        let rc = s.find("RestController").expect("self marker present");
        let c = s.find(r#"of("leaf::Controller")"#).expect("controller present");
        let comp = s.find(r#"of("leaf::Component")"#).expect("component present");
        assert!(rc < c && c < comp, "markers must be nearest-first: {s}");
    }

    #[test]
    fn lower_primary_and_fallback_attributes_set_candidate_role() {
        // `primary = true` / `fallback = true` boolean attributes lower into the
        // 2-axis ::leaf_core::CandidateRole (SEAMS C5: a @Fallback CAN be @Primary).
        let ann = Annotation::new("leaf::Service")
            .with_attr("primary", AttrValue::Bool(true))
            .with_attr("fallback", AttrValue::Bool(true));
        let merged = resolve(&ann).expect("resolves");
        let s = flat(&merged.lower());
        assert!(s.contains("::leaf_core::CandidateRole"), "got: {s}");
        // Primary => the `.primary()` derivation; fallback => FALLBACK base.
        assert!(s.contains("FALLBACK") && s.contains("primary"), "got: {s}");
    }

    #[test]
    fn lower_plain_role_is_normal_and_autowire_default_true() {
        // No primary/fallback => CandidateRole::NORMAL; no opt-out => autowire true.
        let merged = resolve(&component()).expect("resolves");
        let s = flat(&merged.lower());
        assert!(s.contains("::leaf_core::CandidateRole::NORMAL"), "got: {s}");
        assert!(s.contains("autowire_candidate:true"), "got: {s}");
    }

    #[test]
    fn lower_depends_on_emits_contract_ids() {
        // A `depends_on = ["a::Foo", "b::Bar"]` list lowers to ContractId edges the
        // WiringPlan reads — via absolute ::leaf_core::ContractId::of paths.
        let ann = Annotation::new("leaf::Service").with_attr(
            "depends_on",
            AttrValue::List(vec![
                AttrValue::Str("a::Foo".into()),
                AttrValue::Str("b::Bar".into()),
            ]),
        );
        let merged = resolve(&ann).expect("resolves");
        let s = flat(&merged.lower());
        assert!(s.contains(r#"::leaf_core::ContractId::of("a::Foo")"#), "got: {s}");
        assert!(s.contains(r#"::leaf_core::ContractId::of("b::Bar")"#), "got: {s}");
    }

    #[test]
    fn lower_qualifiers_emit_marker_ids() {
        // A `qualifiers = ["leaf::q::Fast"]` list lowers to MarkerId qualifier keys.
        let ann = Annotation::new("leaf::Service").with_attr(
            "qualifiers",
            AttrValue::List(vec![AttrValue::Str("leaf::q::Fast".into())]),
        );
        let merged = resolve(&ann).expect("resolves");
        let s = flat(&merged.lower());
        assert!(s.contains(r#"::leaf_core::MarkerId::of("leaf::q::Fast")"#), "got: {s}");
    }

    #[test]
    fn lower_output_parses_as_an_expression() {
        // The lowered tokens must be a well-formed Rust expression (the macro drops
        // it into `meta: &{ ... }`). Parsing it with syn proves it is grammatical.
        let merged = resolve(&rest_controller()).expect("resolves");
        let ts = merged.lower();
        syn::parse2::<syn::Expr>(ts).expect("lowered metadata is a valid expression");
    }

    #[test]
    fn end_to_end_stereotype_with_meta_override_and_role_lowers() {
        // A realistic composed stereotype: @PrimaryService is @Service (=@Component)
        // with primary=true and a forwarded name override. The whole pipeline —
        // alias validate+apply → merge (closure + nearest-wins) → lower — produces
        // one valid const expression carrying the full transitive marker set, the
        // promoted role, and the override.
        let service = Annotation::new("leaf::Service").with_meta(component());
        let stereotype = Annotation::new("leaf::PrimaryService")
            .with_attr("primary", AttrValue::Bool(true))
            .with_attr("name", AttrValue::Str("widget".into()))
            .with_alias(AliasFor {
                from_attr: "name".into(),
                target_marker: Some("leaf::Service".into()),
                target_attr: "name".into(),
            })
            .with_meta(service);
        let merged = resolve(&stereotype).expect("the full pipeline resolves");
        // Transitive closure: PrimaryService, Service, Component.
        assert_eq!(
            merged.markers,
            vec![
                "leaf::PrimaryService".to_string(),
                "leaf::Service".to_string(),
                "leaf::Component".to_string(),
            ]
        );
        // The override forwarded `name`.
        assert_eq!(merged.attr("name"), Some(&AttrValue::Str("widget".into())));
        // Lowers to a valid const expression with PRIMARY role and all 3 markers.
        let ts = merged.lower();
        syn::parse2::<syn::Expr>(ts.clone()).expect("valid expr");
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::CandidateRole::PRIMARY"), "got: {s}");
        assert!(s.contains(r#"of("leaf::PrimaryService")"#));
        assert!(s.contains(r#"of("leaf::Service")"#));
        assert!(s.contains(r#"of("leaf::Component")"#));
    }

    #[test]
    fn merge_deduplicates_diamond_meta_closure() {
        // A diamond: a stereotype implies two markers that BOTH imply @Component.
        // @Component must appear exactly once (nearest occurrence kept).
        let a = Annotation::new("leaf::A").with_meta(component());
        let b = Annotation::new("leaf::B").with_meta(component());
        let diamond = Annotation::new("leaf::Diamond").with_meta(a).with_meta(b);
        let merged = merge(&diamond);
        let comp_count =
            merged.markers.iter().filter(|m| *m == "leaf::Component").count();
        assert_eq!(comp_count, 1, "diamond meta closure deduplicates @Component");
        // The self marker is always first.
        assert_eq!(merged.markers[0], "leaf::Diamond");
    }
}
