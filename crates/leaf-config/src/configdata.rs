//! Config-data document orchestration primitives (the leaf analogue of Spring's
//! `ConfigDataEnvironmentContributor` document model).
//!
//! This module owns the PURE, profile-free vocabulary that leaf-boot's
//! `seal_environment` reads OFF a [`crate::LoadedDocument`] to drive the
//! recursive-import + document-activation orchestration:
//!
//! - [`ACTIVATE_ON_PROFILE`] / [`IMPORT_KEY`] — the reserved control keys (the
//!   leaf analogues of `spring.config.activate.on-profile` /
//!   `spring.config.import`);
//! - [`DocControl`] — the parsed control subset of a loaded document (its
//!   on-profile activation expression + its declared import locations);
//! - [`is_document_active`] — the document-activation FILTER predicate, reusing
//!   leaf-core's [`leaf_core::accepts_profiles`] profile algebra (`a & b | !c`);
//! - [`illegal_activation_key`] — the hard-rule check: a profile-activated
//!   document that re-sets a reserved EARLY-binding key is a loud
//!   [`crate::ConfigDataErrorKind::IllegalActivationDocument`].
//!
//! The orchestration (the recursive worklist + applying these against the frozen
//! [`leaf_core::ActiveProfiles`]) lives in leaf-boot — this module is the pure,
//! exhaustively-unit-testable kernel it composes.

use leaf_core::{accepts_profiles, ActiveProfiles, PropertyValue};

use crate::error::{ConfigDataError, ConfigDataErrorKind};

/// The document-activation key — a document carrying this is DROPPED unless its
/// profile expression is active (leaf's `spring.config.activate.on-profile`).
pub const ACTIVATE_ON_PROFILE: &str = "leaf.config.activate.on-profile";

/// The recursive-import key — a document carrying this folds in the imported
/// location(s) (leaf's `spring.config.import`).
pub const IMPORT_KEY: &str = "leaf.config.import";

/// The reserved EARLY-binding keys a profile-ACTIVATED document may NOT set.
///
/// These are the canonical profile/import control levers that MUST be resolved
/// before document activation runs (profile activation is what DECIDES whether
/// an activated document is even kept, and an import inside an activated doc
/// would re-open the already-resolved worklist phase). A profile-activated
/// document setting one of these raises [`ConfigDataErrorKind::IllegalActivationDocument`]
/// (leaf's analogue of Spring's `InvalidConfigDataPropertyException`).
///
/// The set: the three profile levers (`leaf.profiles.active|include|default`)
/// and the import key (`leaf.config.import`).
pub const RESERVED_ACTIVATION_KEYS: &[&str] = &[
    "leaf.profiles.active",
    "leaf.profiles.include",
    "leaf.profiles.default",
    IMPORT_KEY,
];

/// The parsed control subset of one [`crate::LoadedDocument`]: its on-profile
/// activation expression (if any) + its declared recursive-import locations.
///
/// Derived purely from the flattened props — no profile evaluation here (that is
/// [`is_document_active`]'s job, run against the frozen active set in leaf-boot).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DocControl {
    /// The `leaf.config.activate.on-profile` expression (`None` = always active).
    pub on_profile: Option<String>,
    /// The `leaf.config.import` locations declared by this document (CSV split).
    pub imports: Vec<String>,
}

impl DocControl {
    /// Whether this document declares an on-profile activation gate.
    #[must_use]
    pub fn is_profile_activated(&self) -> bool {
        self.on_profile.is_some()
    }

    /// Parse the control subset off a document's flattened props.
    ///
    /// `leaf.config.import` is read as a single value split on `,` (the relaxed
    /// list form), and ALSO from any indexed `leaf.config.import[N]` keys (the
    /// flattened YAML/JSON sequence form). `leaf.config.activate.on-profile` is
    /// the verbatim expression string.
    #[must_use]
    pub fn parse(props: &[(String, PropertyValue)]) -> Self {
        let mut on_profile = None;
        let mut imports: Vec<String> = Vec::new();
        for (key, value) in props {
            if key == ACTIVATE_ON_PROFILE {
                on_profile = Some(value.raw.trim().to_string());
            } else if key == IMPORT_KEY {
                // Scalar / relaxed-CSV form: `leaf.config.import: a, b`.
                imports.extend(split_locations(&value.raw));
            } else if key.starts_with(IMPORT_KEY)
                && key[IMPORT_KEY.len()..].starts_with('[')
                && key.ends_with(']')
            {
                // Flattened sequence form: `leaf.config.import[0]`, `[1]`, …
                imports.extend(split_locations(&value.raw));
            }
        }
        DocControl { on_profile, imports }
    }
}

/// Split a config-import value into the individual locations (CSV, trimmed,
/// empties dropped).
fn split_locations(raw: &str) -> impl Iterator<Item = String> + '_ {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The document-activation FILTER predicate: whether a document with control
/// `control` is active under the resolved `active` profile set.
///
/// A document with NO `on-profile` gate is always active. A gated document is
/// active iff its profile expression accepts the active set (reusing the SAME
/// [`leaf_core::accepts_profiles`] algebra `OnProfile` uses).
///
/// # Errors
/// A [`ConfigDataError`] (`Malformed`) if the on-profile expression is
/// syntactically invalid (mismatched parens, mixed `&`/`|`, empty operand) —
/// the activation expression is config data, so a bad one is a loud config-data
/// fault, not a silent drop.
pub fn is_document_active(
    control: &DocControl,
    active: &ActiveProfiles,
    location: &str,
) -> Result<bool, ConfigDataError> {
    match &control.on_profile {
        None => Ok(true),
        Some(expr) => accepts_profiles(expr, active).map_err(|e| {
            ConfigDataError::malformed(location, format!("invalid on-profile expression: {e}"))
        }),
    }
}

/// The illegal-activation HARD RULE: if `control` is a profile-activated
/// document AND the props set any [`RESERVED_ACTIVATION_KEYS`], return the
/// offending key.
///
/// Returns `Some(key)` naming the FIRST reserved key found; `None` if the
/// document is clean (or is not profile-activated, in which case the early
/// levers are legal). The caller raises
/// [`ConfigDataErrorKind::IllegalActivationDocument`].
#[must_use]
pub fn illegal_activation_key<'a>(
    control: &DocControl,
    props: &'a [(String, PropertyValue)],
) -> Option<&'a str> {
    if !control.is_profile_activated() {
        return None;
    }
    props.iter().find_map(|(key, _)| {
        let base = key.split('[').next().unwrap_or(key);
        if RESERVED_ACTIVATION_KEYS.contains(&base) {
            Some(key.as_str())
        } else {
            None
        }
    })
}

/// Build the loud [`ConfigDataError`] for a profile-activated document that set
/// a reserved early-binding `key` (leaf's `InvalidConfigDataPropertyException`).
#[must_use]
pub fn illegal_activation_error(location: &str, key: &str) -> ConfigDataError {
    ConfigDataError::new(
        ConfigDataErrorKind::IllegalActivationDocument,
        location,
        format!(
            "property `{key}` is not allowed in a profile-activated document \
             (it sets a reserved early-binding key)"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{resolve_active, ProfileLevers};
    use std::sync::Arc;

    fn props(pairs: &[(&str, &str)]) -> Vec<(String, PropertyValue)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), PropertyValue::new((*v).to_string())))
            .collect()
    }

    fn active(names: &[&str]) -> ActiveProfiles {
        let levers = ProfileLevers {
            active: names.iter().map(|n| Arc::<str>::from(*n)).collect(),
            include: Vec::new(),
            groups: std::collections::HashMap::new(),
            default: Arc::<str>::from("default"),
        };
        resolve_active(levers, false).unwrap()
    }

    #[test]
    fn parse_extracts_on_profile_expression() {
        let c = DocControl::parse(&props(&[
            ("leaf.config.activate.on-profile", "prod & eu"),
            ("db.url", "x"),
        ]));
        assert_eq!(c.on_profile.as_deref(), Some("prod & eu"));
        assert!(c.is_profile_activated());
    }

    #[test]
    fn parse_extracts_csv_imports() {
        let c = DocControl::parse(&props(&[("leaf.config.import", "a.json, b.yaml")]));
        assert_eq!(c.imports, vec!["a.json".to_string(), "b.yaml".to_string()]);
    }

    #[test]
    fn parse_extracts_indexed_sequence_imports() {
        let c = DocControl::parse(&props(&[
            ("leaf.config.import[0]", "a.json"),
            ("leaf.config.import[1]", "b.yaml"),
        ]));
        assert_eq!(c.imports, vec!["a.json".to_string(), "b.yaml".to_string()]);
    }

    #[test]
    fn ungated_document_is_always_active() {
        let c = DocControl::default();
        assert!(is_document_active(&c, &active(&[]), "x.yaml").unwrap());
    }

    #[test]
    fn gated_document_dropped_when_profile_inactive() {
        let c = DocControl {
            on_profile: Some("prod".to_string()),
            imports: vec![],
        };
        assert!(!is_document_active(&c, &active(&["dev"]), "x.yaml").unwrap());
    }

    #[test]
    fn gated_document_kept_when_profile_active() {
        let c = DocControl {
            on_profile: Some("prod".to_string()),
            imports: vec![],
        };
        assert!(is_document_active(&c, &active(&["prod"]), "x.yaml").unwrap());
    }

    #[test]
    fn gated_document_uses_full_profile_algebra() {
        let c = DocControl {
            on_profile: Some("prod & !legacy".to_string()),
            imports: vec![],
        };
        assert!(is_document_active(&c, &active(&["prod"]), "x.yaml").unwrap());
        assert!(!is_document_active(&c, &active(&["prod", "legacy"]), "x.yaml").unwrap());
    }

    #[test]
    fn malformed_on_profile_is_a_loud_error() {
        let c = DocControl {
            on_profile: Some("prod & | dev".to_string()),
            imports: vec![],
        };
        let err = is_document_active(&c, &active(&["prod"]), "x.yaml").unwrap_err();
        assert_eq!(err.kind, ConfigDataErrorKind::Malformed);
    }

    #[test]
    fn non_activated_document_may_set_reserved_keys() {
        let p = props(&[("leaf.profiles.active", "prod")]);
        let c = DocControl::parse(&p);
        assert!(illegal_activation_key(&c, &p).is_none());
    }

    #[test]
    fn activated_document_setting_active_profiles_is_illegal() {
        let p = props(&[
            ("leaf.config.activate.on-profile", "prod"),
            ("leaf.profiles.active", "extra"),
        ]);
        let c = DocControl::parse(&p);
        assert_eq!(illegal_activation_key(&c, &p), Some("leaf.profiles.active"));
    }

    #[test]
    fn activated_document_setting_import_is_illegal() {
        let p = props(&[
            ("leaf.config.activate.on-profile", "prod"),
            ("leaf.config.import", "more.json"),
        ]);
        let c = DocControl::parse(&p);
        assert_eq!(illegal_activation_key(&c, &p), Some("leaf.config.import"));
    }
}
