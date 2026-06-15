//! The binder-side validation adapter (validation, phase3/09 §validation, FACE 3 —
//! the `@ConfigurationProperties` JSR validation half).
//!
//! Validation runs as a SEPARATE pass over an ALREADY-CONVERTED value (Spring's
//! deliberate separation; phase3/09): after the binder binds a typed config tree,
//! the same [`Validate`](leaf_core::Validate) engine runs and each
//! [`Violation`] path is mapped back to the canonical property
//! KEY (`<prefix>.<field-path>`) + an [`Origin`], then aggregated into a single
//! `ValidationError` [`LeafError`] pushed into the binder's fault accumulator (the
//! C2 [`ConfigBindOutcome`](leaf_core::ConfigBindOutcome) `Err(Vec<LeafError>)`).
//!
//! Two pieces:
//!
//! - [`ValidationBindHandler`] — a leaf-core [`BindHandler`] observer. The kernel
//!   binder hands the handler the bound NAME/field at each node but NOT the bound
//!   VALUE (the value lives in the typed `BindResult<T>` the caller owns), so the
//!   handler records the observed bound-node names (diagnostics / origin mapping)
//!   and the typed validation runs through [`validate_config`] on the bound value.
//! - [`validate_config`] — the FACE-3 entry: run [`validate_root`](crate::validate_root)
//!   on the bound value + remap each violation path to the canonical KEY under
//!   `prefix`, returning the aggregated fault (or `None` when clean). The
//!   `#[config_properties]` C2 bind thunk (which the codegen emits with the stock
//!   [`NoopBindHandler`](leaf_core::NoopBindHandler) — JSR validation is THIS
//!   force-link's concern, per the leaf-codegen NOTE) calls it on the bound value.

use std::sync::Mutex;

use leaf_core::{BindCtx, BindHandler, Cause, ErrorKind, LeafError, Origin, Violation};

use crate::violations::render_violation;
use crate::ValidateInto;

/// A [`BindHandler`] observer that RECORDS the canonical names the binder bound
/// (the path/origin context the face-3 mapping reads).
///
/// The kernel binder is value-agnostic toward the handler (it passes the bound NAME,
/// not the typed value), so the observer cannot itself run typed validation; it
/// captures the bound names so a diagnostic can attribute a violation to a real
/// bound key. The typed validation runs via [`validate_config`] on the bound value.
#[derive(Default)]
pub struct ValidationBindHandler {
    bound: Mutex<Vec<String>>,
}

impl ValidationBindHandler {
    /// A fresh handler (no nodes observed yet).
    #[must_use]
    pub fn new() -> Self {
        ValidationBindHandler::default()
    }

    /// The canonical names the binder bound through this handler (in bind order).
    #[must_use]
    pub fn bound_names(&self) -> Vec<String> {
        self.bound.lock().expect("bind handler mutex").clone()
    }
}

impl BindHandler for ValidationBindHandler {
    fn on_success(&self, ctx: &BindCtx<'_>) {
        if let Ok(mut g) = self.bound.lock() {
            g.push(ctx.name.to_string());
        }
    }
}

impl std::fmt::Debug for ValidationBindHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.bound.lock().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("ValidationBindHandler").field("bound_nodes", &n).finish()
    }
}

/// Map a Rust field PATH (`server.port`, `items[0].qty`) under a canonical config
/// `prefix` to the canonical property KEY the binding read it from. An empty path
/// (a violation on the whole object) maps to the prefix itself.
#[must_use]
pub fn canonical_key(prefix: &str, path: &str) -> String {
    match (prefix.is_empty(), path.is_empty()) {
        (true, _) => path.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}.{path}"),
    }
}

/// FACE 3: validate a freshly-bound `@ConfigurationProperties` value, remapping each
/// violation's Rust-field path to the canonical property KEY under `prefix` and
/// aggregating into ONE `ValidationError` [`LeafError`] (or `None` when clean).
///
/// This is the binder-side adapter the C2 bind thunk runs AFTER a successful bind:
/// the SAME [`validate_root`](crate::validate_root) engine the method-validation face
/// uses (one engine, never two), with the property-path remapping the config face
/// adds. The bound value `T` is [`ValidateInto`]; it is adapted to the kernel
/// [`Validate`](leaf_core::Validate) seam via [`AsValidate`](crate::AsValidate) (so this also works for
/// a value reached as `&dyn Validate`).
#[must_use]
pub fn validate_config<T: ValidateInto + ?Sized>(prefix: &str, bound: &T) -> Option<LeafError> {
    let cx = crate::validate_root(bound);
    aggregate_config(prefix, cx.violations())
}

/// Like [`validate_config`] but over an already-built `&dyn Validate` (the erased
/// face — the binder reaching the value through the kernel object-safe trait).
#[must_use]
pub fn validate_config_dyn(prefix: &str, bound: &dyn leaf_core::Validate) -> Option<LeafError> {
    let mut cx = leaf_core::ValidationContext::new();
    bound.validate(&mut cx);
    aggregate_config(prefix, cx.violations())
}

fn aggregate_config(prefix: &str, violations: &[Violation]) -> Option<LeafError> {
    if violations.is_empty() {
        return None;
    }
    let mut err = LeafError::new(ErrorKind::ValidationError).with_origin(Origin::Unknown);
    for v in violations {
        // Remap the Rust-field path to the canonical config KEY, then render.
        let mut mapped = v.clone();
        mapped.path = canonical_key(prefix, &v.path);
        err = err.caused_by(Cause::plain(
            "binding @ConfigurationProperties",
            render_violation(&mapped),
        ));
    }
    Some(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cascade::AsValidate;
    use leaf_macros::Validate;

    // A @ConfigurationProperties bean: app.* with a range(min=1) field. The hand
    // `impl ValidateInto` is now DERIVED — the `max-connections` canonical segment
    // (from the `max_connections` field) + the `host` not_empty check reproduce the
    // prior hand impl, so the unchanged canonical-KEY assertions below still hold.
    #[derive(Debug, Default, Validate)]
    struct PoolProps {
        #[validate(min = 1)]
        max_connections: i64,
        #[validate(not_empty)]
        host: String,
    }

    #[test]
    fn canonical_key_joins_prefix_and_path() {
        assert_eq!(canonical_key("app", "max-connections"), "app.max-connections");
        assert_eq!(canonical_key("app", ""), "app", "a whole-object violation maps to the prefix");
        assert_eq!(canonical_key("", "x"), "x");
    }

    #[test]
    fn a_clean_bound_value_is_not_an_error() {
        let props = PoolProps { max_connections: 8, host: "db".into() };
        assert!(validate_config("app", &props).is_none(), "valid config binds without fault");
    }

    #[test]
    fn a_range_min_violation_fails_at_validate_with_the_canonical_key() {
        // max_connections = 0 violates range(min=1) — the headline config face test.
        let props = PoolProps { max_connections: 0, host: "db".into() };
        let err = validate_config("app", &props).expect("the bind validates and FAILS");
        assert_eq!(err.kind, ErrorKind::ValidationError);
        assert_eq!(err.chain.len(), 1, "one violation");
        let detail = err.chain[0].detail.to_string();
        assert!(
            detail.contains("app.max-connections: validation.min [min=1]"),
            "the violation path mapped to the canonical KEY `app.max-connections`, got: {detail}"
        );
    }

    #[test]
    fn multiple_violations_aggregate_with_canonical_keys() {
        let props = PoolProps { max_connections: 0, host: "".into() };
        let err = validate_config("app", &props).expect("fails");
        assert_eq!(err.chain.len(), 2, "both fields aggregated");
        let details: Vec<String> = err.chain.iter().map(|c| c.detail.to_string()).collect();
        assert!(details.iter().any(|d| d.contains("app.max-connections")));
        assert!(details.iter().any(|d| d.contains("app.host")));
    }

    #[test]
    fn validate_config_dyn_works_over_an_erased_value() {
        let props = PoolProps { max_connections: 0, host: "db".into() };
        let adapter = AsValidate(&props);
        let err = validate_config_dyn("app", &adapter).expect("fails via dyn Validate");
        assert!(err.chain[0].detail.to_string().contains("app.max-connections"));
    }

    #[test]
    fn the_handler_records_bound_node_names() {
        use leaf_core::CanonicalName;
        let h = ValidationBindHandler::new();
        let name = CanonicalName::parse("app.max-connections").expect("parse");
        h.on_success(&BindCtx { name: &name, field: Some("max_connections") });
        assert_eq!(h.bound_names(), vec!["app.max-connections".to_string()]);
    }
}
