//! Config-data error + location types (environment-config `config-data`).
//!
//! [`ConfigDataError`] is the loader/planner failure shape; it folds into the
//! one [`leaf_core::LeafError`] causal chain (a [`From`] bridge) so a malformed
//! JSON/YAML document, a missing non-optional location, or an illegal activation
//! document all surface through the single `Diagnostic` renderer.
//!
//! [`ConfigDataLocation`] is the origin-agnostic "where to load from" token a
//! [`crate::ConfigDataLoader`] inspects via `handles` — the `<format>:<path>`
//! shape (`application.yaml`, `configtree:/etc/config/`, `env:`). The scheme is
//! parsed once here so loaders predicate on DATA, never re-parse the raw string.

use leaf_core::{Cause, ErrorKind, LeafError};

/// The class of a [`ConfigDataError`] — mapped onto a [`leaf_core::ErrorKind`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigDataErrorKind {
    /// A document failed to parse (malformed JSON/YAML/properties).
    Malformed,
    /// A required (non-optional) config location was not found / unreadable.
    MissingLocation,
    /// No registered loader claimed a location (the loud anti-DCE-class case).
    NoLoader,
    /// An activated document set a reserved early-control key (profiles/cloud).
    IllegalActivationDocument,
    /// A generic config-IO failure (read error, permission).
    Io,
}

impl ConfigDataErrorKind {
    fn as_error_kind(self) -> ErrorKind {
        match self {
            ConfigDataErrorKind::Malformed | ConfigDataErrorKind::IllegalActivationDocument => {
                ErrorKind::BindError
            }
            ConfigDataErrorKind::MissingLocation
            | ConfigDataErrorKind::NoLoader
            | ConfigDataErrorKind::Io => ErrorKind::ConfigIo,
        }
    }
}

/// A config-data load/plan failure (environment-config `config-data`).
///
/// Carries the [`ConfigDataErrorKind`] class, the offending location string, and
/// a human reason. Folds into [`leaf_core::LeafError`] via [`From`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConfigDataError {
    /// The error class.
    pub kind: ConfigDataErrorKind,
    /// The location string this failure is about (e.g. `application.yaml`).
    pub location: String,
    /// A short human reason.
    pub reason: String,
}

impl ConfigDataError {
    /// Construct a config-data error.
    #[must_use]
    pub fn new(
        kind: ConfigDataErrorKind,
        location: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        ConfigDataError {
            kind,
            location: location.into(),
            reason: reason.into(),
        }
    }

    /// A `Malformed` document error.
    #[must_use]
    pub fn malformed(location: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(ConfigDataErrorKind::Malformed, location, reason)
    }

    /// A `MissingLocation` error (a required location was absent).
    #[must_use]
    pub fn missing(location: impl Into<String>) -> Self {
        Self::new(
            ConfigDataErrorKind::MissingLocation,
            location,
            "required config location not found",
        )
    }

    /// A `NoLoader` error (no loader claimed a location).
    #[must_use]
    pub fn no_loader(location: impl Into<String>) -> Self {
        Self::new(
            ConfigDataErrorKind::NoLoader,
            location,
            "no registered loader handles this location",
        )
    }
}

impl std::fmt::Display for ConfigDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "config-data error at `{}`: {}", self.location, self.reason)
    }
}

impl std::error::Error for ConfigDataError {}

impl From<ConfigDataError> for LeafError {
    fn from(e: ConfigDataError) -> Self {
        LeafError::new(e.kind.as_error_kind())
            .caused_by(Cause::plain("loading config data", e.to_string()))
    }
}

/// The format/scheme prefix of a [`ConfigDataLocation`].
///
/// Parsed once from the raw `<scheme>:<path>` string so a loader's `handles`
/// predicate is a cheap enum match, never a re-parse. A location with no
/// recognized scheme prefix is [`LocationScheme::File`] (the bare-path default,
/// dispatched by extension).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LocationScheme {
    /// A bare/`file:` path; the format loader is chosen by extension.
    File,
    /// A `configtree:<dir>` directory-of-files source (Kubernetes-style).
    ConfigTree,
    /// An `env:` (or `env:<prefix>`) OS-environment source.
    Env,
}

/// A "where to load from" token (environment-config `config-data`).
///
/// The origin-agnostic location a loader's `handles` inspects. The `<scheme>:`
/// prefix (if any) is parsed once into [`LocationScheme`]; for a bare path the
/// extension drives format selection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConfigDataLocation {
    raw: String,
    scheme: LocationScheme,
    /// The path component (the raw string minus any recognized scheme prefix).
    path: String,
    /// Whether a missing location is tolerated (`optional:` prefix) — a missing
    /// non-optional location is a [`ConfigDataErrorKind::MissingLocation`].
    optional: bool,
}

impl ConfigDataLocation {
    /// Parse a raw location string into a structured location.
    ///
    /// Recognizes an `optional:` prefix and the `configtree:`/`env:`/`file:`
    /// schemes; a bare path is [`LocationScheme::File`].
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let mut rest = raw;
        let mut optional = false;
        if let Some(stripped) = rest.strip_prefix("optional:") {
            optional = true;
            rest = stripped;
        }
        let (scheme, path) = if let Some(p) = rest.strip_prefix("configtree:") {
            (LocationScheme::ConfigTree, p.to_string())
        } else if rest == "env" {
            (LocationScheme::Env, String::new())
        } else if let Some(p) = rest.strip_prefix("env:") {
            (LocationScheme::Env, p.to_string())
        } else if let Some(p) = rest.strip_prefix("file:") {
            (LocationScheme::File, p.to_string())
        } else {
            (LocationScheme::File, rest.to_string())
        };
        ConfigDataLocation {
            raw: raw.to_string(),
            scheme,
            path,
            optional,
        }
    }

    /// A bare-file location with an explicit optional flag.
    #[must_use]
    pub fn file(path: impl Into<String>, optional: bool) -> Self {
        let path = path.into();
        let raw = if optional {
            format!("optional:{path}")
        } else {
            path.clone()
        };
        ConfigDataLocation {
            raw,
            scheme: LocationScheme::File,
            path,
            optional,
        }
    }

    /// The raw, un-parsed location string (including any prefixes).
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The parsed scheme.
    #[must_use]
    pub fn scheme(&self) -> &LocationScheme {
        &self.scheme
    }

    /// The path component (raw minus the recognized scheme prefix).
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Whether a missing location is tolerated.
    #[must_use]
    pub fn is_optional(&self) -> bool {
        self.optional
    }

    /// The lowercase file extension of the path (no leading dot), if any.
    #[must_use]
    pub fn extension(&self) -> Option<String> {
        std::path::Path::new(&self.path)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_path_as_file_with_extension() {
        let loc = ConfigDataLocation::parse("application.yaml");
        assert_eq!(loc.scheme(), &LocationScheme::File);
        assert_eq!(loc.path(), "application.yaml");
        assert_eq!(loc.extension().as_deref(), Some("yaml"));
        assert!(!loc.is_optional());
    }

    #[test]
    fn parses_optional_prefix() {
        let loc = ConfigDataLocation::parse("optional:missing.json");
        assert!(loc.is_optional());
        assert_eq!(loc.path(), "missing.json");
        assert_eq!(loc.extension().as_deref(), Some("json"));
    }

    #[test]
    fn parses_configtree_and_env_schemes() {
        let tree = ConfigDataLocation::parse("configtree:/etc/config/");
        assert_eq!(tree.scheme(), &LocationScheme::ConfigTree);
        assert_eq!(tree.path(), "/etc/config/");

        let env = ConfigDataLocation::parse("env:");
        assert_eq!(env.scheme(), &LocationScheme::Env);

        let env_bare = ConfigDataLocation::parse("env");
        assert_eq!(env_bare.scheme(), &LocationScheme::Env);

        let env_prefixed = ConfigDataLocation::parse("optional:env:APP_");
        assert_eq!(env_prefixed.scheme(), &LocationScheme::Env);
        assert_eq!(env_prefixed.path(), "APP_");
        assert!(env_prefixed.is_optional());
    }

    #[test]
    fn error_folds_into_leaf_error_with_right_kind() {
        let malformed: LeafError = ConfigDataError::malformed("a.json", "bad").into();
        assert_eq!(malformed.kind, ErrorKind::BindError);

        let missing: LeafError = ConfigDataError::missing("x.yaml").into();
        assert_eq!(missing.kind, ErrorKind::ConfigIo);

        let no_loader: LeafError = ConfigDataError::no_loader("weird:thing").into();
        assert_eq!(no_loader.kind, ErrorKind::ConfigIo);
    }
}
