//! The serde-bridge error type and its conversion to a leaf-core [`LeafError`].
//!
//! serde's `Deserialize` machinery requires a `de::Error: std::error::Error`,
//! so the bridge mints [`SerdeBridgeError`] (a thin display-string carrier with
//! the offending key/value/origin) and converts it into the canonical
//! `ConvertError`/`BindError` [`LeafError`] node at the public boundary — so a
//! serde-bound value still lands on leaf's one diagnostic spine.

use std::fmt;

use leaf_core::error::{Cause, ErrorKind, LeafError, Origin};

/// A serde-side bridge error.
///
/// Implements [`serde::de::Error`] so it can be produced from inside a
/// `Deserialize` impl, and carries the [`Origin`] of the offending config value
/// so the lifted [`LeafError`] keeps as much provenance as the serde seam allows.
#[derive(Clone, Debug)]
pub struct SerdeBridgeError {
    msg: String,
    origin: Origin,
}

impl SerdeBridgeError {
    /// A bridge error with an explicit message and origin.
    #[must_use]
    pub fn new(msg: impl Into<String>, origin: Origin) -> Self {
        SerdeBridgeError {
            msg: msg.into(),
            origin,
        }
    }

    /// The error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.msg
    }

    /// The provenance of the offending value.
    #[must_use]
    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// Attach/override the origin (builder style).
    #[must_use]
    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    /// Lift this into the canonical leaf-core [`LeafError`] of `kind`
    /// (`ConvertError` for the scalar converter path, `BindError` for the
    /// subtree-deserialize path), preserving the [`Origin`].
    #[must_use]
    pub fn into_leaf(self, kind: ErrorKind) -> LeafError {
        LeafError::new(kind)
            .with_origin(self.origin)
            .caused_by(Cause::plain("serde-bridge deserialize", self.msg).with_origin(self.origin))
    }
}

impl fmt::Display for SerdeBridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for SerdeBridgeError {}

impl serde::de::Error for SerdeBridgeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        SerdeBridgeError {
            msg: msg.to_string(),
            origin: Origin::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifts_to_convert_error_keeping_origin() {
        let e = SerdeBridgeError::new("bad u16", Origin::TestDouble);
        let leaf = e.into_leaf(ErrorKind::ConvertError);
        assert_eq!(leaf.kind, ErrorKind::ConvertError);
        assert_eq!(leaf.origin, Origin::TestDouble);
        let rendered = leaf
            .chain
            .first()
            .map(|c| c.detail.to_string())
            .unwrap_or_default();
        assert!(rendered.contains("bad u16"), "carries message: {rendered}");
    }

    #[test]
    fn serde_custom_constructor_has_unknown_origin() {
        use serde::de::Error as _;
        let e = SerdeBridgeError::custom("oops");
        assert_eq!(e.origin(), Origin::Unknown);
        assert_eq!(e.message(), "oops");
    }
}
