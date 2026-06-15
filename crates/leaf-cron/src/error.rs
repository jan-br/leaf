//! The crate's typed parse error + its bridge into leaf-core's `LeafError`.
//!
//! Per phase3/10, a malformed cron is a **Tier-2 assembly failure** aggregated
//! into the `AssemblyReport` at the startup validation pass. This crate surfaces
//! a precise, position-named [`CronError`]; [`CronError::into_leaf`] folds it
//! into the ONE [`LeafError`] causal chain (the spec-string
//! parse is a value-shape conversion, so it rides
//! [`ErrorKind::ConvertError`]) for leaf-boot
//! to render via [`Diagnostic`](leaf_core::Diagnostic).

use leaf_core::{Cause, ErrorKind, LeafError};

/// Why a cron expression failed to parse.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CronError {
    /// The expression had the wrong number of whitespace-separated fields
    /// (only 6 or 7 are accepted).
    FieldCount {
        /// How many fields were found.
        found: usize,
    },
    /// A field was empty (e.g. a trailing comma `1,,3`).
    EmptyField {
        /// Which position.
        field: &'static str,
    },
    /// A value fell outside the field's inclusive bounds.
    OutOfRange {
        /// Which position.
        field: &'static str,
        /// The offending value.
        value: u32,
        /// The field's low bound.
        lo: u32,
        /// The field's high bound.
        hi: u32,
    },
    /// A range `A-B` had `A > B`.
    InvertedRange {
        /// Which position.
        field: &'static str,
        /// The (larger) low end.
        lo: u32,
        /// The (smaller) high end.
        hi: u32,
    },
    /// A `/0` step (a step must be positive).
    ZeroStep {
        /// Which position.
        field: &'static str,
    },
    /// `?` used in a field that does not accept it (only day-of-month /
    /// day-of-week do).
    QuestionNotAllowed {
        /// Which position.
        field: &'static str,
    },
    /// A token that was neither a number, a known name, nor a valid construct.
    BadToken {
        /// Which position.
        field: &'static str,
        /// The offending token.
        token: String,
    },
}

impl std::fmt::Display for CronError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CronError::FieldCount { found } => write!(
                f,
                "cron expression must have 6 or 7 fields, found {found}"
            ),
            CronError::EmptyField { field } => write!(f, "empty {field} field"),
            CronError::OutOfRange {
                field,
                value,
                lo,
                hi,
            } => write!(
                f,
                "{field} value {value} out of range {lo}..={hi}"
            ),
            CronError::InvertedRange { field, lo, hi } => {
                write!(f, "inverted {field} range {lo}-{hi} (low > high)")
            }
            CronError::ZeroStep { field } => write!(f, "{field} step must be positive (not /0)"),
            CronError::QuestionNotAllowed { field } => {
                write!(f, "`?` is not allowed in the {field} field")
            }
            CronError::BadToken { field, token } => {
                write!(f, "unrecognized {field} token `{token}`")
            }
        }
    }
}

impl std::error::Error for CronError {}

impl CronError {
    /// Fold this into the ONE [`LeafError`] causal chain (Tier-2
    /// [`ConvertError`](ErrorKind::ConvertError)), carrying the precise narrative
    /// as a [`Cause`] node so the [`Diagnostic`](leaf_core::Diagnostic) renders it.
    #[must_use]
    pub fn into_leaf(self) -> LeafError {
        LeafError::new(ErrorKind::ConvertError)
            .caused_by(Cause::plain("parsing cron expression", self.to_string()))
    }
}

impl From<CronError> for LeafError {
    fn from(e: CronError) -> Self {
        e.into_leaf()
    }
}
