//! The 6/7-field cron expression parser.
//!
//! Grammar (Quartz/Spring-style, second-resolution):
//!
//! ```text
//! ┌───────────── second        (0 - 59)
//! │ ┌───────────── minute       (0 - 59)
//! │ │ ┌───────────── hour        (0 - 23)
//! │ │ │ ┌───────────── day-of-month (1 - 31, or ?)
//! │ │ │ │ ┌───────────── month     (1 - 12 or JAN-DEC)
//! │ │ │ │ │ ┌───────────── day-of-week (0 - 7 or SUN-SAT; 0 and 7 = Sunday, or ?)
//! │ │ │ │ │ │ ┌───────────── year   (optional 7th field, e.g. 1970-2099)
//! * * * * * * *
//! ```
//!
//! Each field supports `*` (any), a single value `N`, a range `A-B`, a step
//! `*/K` or `A-B/K` or `A/K` (from-A, every K), a comma list of any of these,
//! and `?` ("no specific value", accepted on day-of-month / day-of-week as a
//! Quartz alias for `*`). Month and day-of-week accept three-letter names.

use crate::error::CronError;
use crate::field::Field;

/// The fixed inclusive bounds (lo, hi) of each cron position.
#[derive(Clone, Copy)]
struct FieldSpec {
    name: &'static str,
    lo: u32,
    hi: u32,
    /// Optional name table, indexed so `names[i]` denotes value `lo + i`.
    names: &'static [&'static str],
    /// Whether `?` is accepted here (day-of-month / day-of-week).
    allows_question: bool,
}

const MONTHS: &[&str] = &[
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DOW: &[&str] = &["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

const SECOND: FieldSpec = FieldSpec {
    name: "second",
    lo: 0,
    hi: 59,
    names: &[],
    allows_question: false,
};
const MINUTE: FieldSpec = FieldSpec {
    name: "minute",
    lo: 0,
    hi: 59,
    names: &[],
    allows_question: false,
};
const HOUR: FieldSpec = FieldSpec {
    name: "hour",
    lo: 0,
    hi: 23,
    names: &[],
    allows_question: false,
};
const DOM: FieldSpec = FieldSpec {
    name: "day-of-month",
    lo: 1,
    hi: 31,
    names: &[],
    allows_question: true,
};
const MONTH: FieldSpec = FieldSpec {
    name: "month",
    lo: 1,
    hi: 12,
    names: MONTHS,
    allows_question: false,
};
const DOW_SPEC: FieldSpec = FieldSpec {
    name: "day-of-week",
    lo: 0,
    hi: 7,
    names: DOW,
    allows_question: true,
};
const YEAR: FieldSpec = FieldSpec {
    name: "year",
    lo: 1970,
    hi: 2099,
    names: &[],
    allows_question: false,
};

/// A parsed cron expression: a [`Field`] bitset per position.
///
/// `day_of_week` is normalized to a `0..=6` Sunday-based mask (a `7` in the
/// source folds onto `0`). When BOTH day-of-month and day-of-week are restricted
/// (neither is `*`/`?`), the Quartz/Vixie OR-semantics apply at match time
/// ([`dom_restricted`](ParsedCron::dom_restricted) /
/// [`dow_restricted`](ParsedCron::dow_restricted) record this).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedCron {
    /// Seconds bitset (`0..=59`).
    pub second: Field,
    /// Minutes bitset (`0..=59`).
    pub minute: Field,
    /// Hours bitset (`0..=23`).
    pub hour: Field,
    /// Day-of-month bitset (`1..=31`).
    pub day_of_month: Field,
    /// Month bitset (`1..=12`).
    pub month: Field,
    /// Day-of-week bitset, normalized Sunday-based `0..=6`.
    pub day_of_week: Field,
    /// Year bitset (`1970..=2099`).
    pub year: Field,
    /// Whether the source restricted day-of-month (not `*`/`?`).
    pub dom_restricted: bool,
    /// Whether the source restricted day-of-week (not `*`/`?`).
    pub dow_restricted: bool,
}

/// Parse a 6 or 7-field cron expression.
///
/// Six fields = `sec min hour dom month dow`; a seventh appends `year`. Leading/
/// trailing whitespace is trimmed and inner runs of whitespace separate fields.
///
/// # Errors
/// Returns a [`CronError`] for the wrong field count, an out-of-range value, an
/// inverted range, a zero/!positive step, or an unknown name/token.
pub fn parse(expr: &str) -> Result<ParsedCron, CronError> {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 6 && fields.len() != 7 {
        return Err(CronError::FieldCount {
            found: fields.len(),
        });
    }

    let raw_dom = fields[3];
    let raw_dow = fields[5];
    let dom_restricted = !is_wildcard(raw_dom);
    let dow_restricted = !is_wildcard(raw_dow);

    let second = parse_field(fields[0], SECOND)?;
    let minute = parse_field(fields[1], MINUTE)?;
    let hour = parse_field(fields[2], HOUR)?;
    let day_of_month = parse_field(raw_dom, DOM)?;
    let month = parse_field(fields[4], MONTH)?;
    let day_of_week = normalize_dow(parse_field(raw_dow, DOW_SPEC)?);
    let year = if fields.len() == 7 {
        parse_field(fields[6], YEAR)?
    } else {
        // No year field => every year in range is allowed.
        Field::full(YEAR.lo, YEAR.hi)
    };

    Ok(ParsedCron {
        second,
        minute,
        hour,
        day_of_month,
        month,
        day_of_week,
        year,
        dom_restricted,
        dow_restricted,
    })
}

/// Whether a raw field is the unrestricted wildcard (`*` or the Quartz `?`).
fn is_wildcard(raw: &str) -> bool {
    raw == "*" || raw == "?"
}

/// Fold a `0..=7` Sunday-based DOW mask onto the canonical `0..=6` (both 0 and 7
/// mean Sunday).
fn normalize_dow(f: Field) -> Field {
    let mut out = Field::empty();
    for v in 0..=7u32 {
        if f.contains(v) {
            out.set(v % 7);
        }
    }
    out
}

/// Parse one comma-list field into its bitset.
fn parse_field(raw: &str, spec: FieldSpec) -> Result<Field, CronError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(CronError::EmptyField { field: spec.name });
    }
    if raw == "?" {
        if !spec.allows_question {
            return Err(CronError::QuestionNotAllowed { field: spec.name });
        }
        return Ok(Field::full(spec.lo, spec.hi));
    }
    let mut field = Field::empty();
    for part in raw.split(',') {
        parse_part(part, spec, &mut field)?;
    }
    Ok(field)
}

/// Parse one comma-separated element (which may carry a `/step`).
fn parse_part(part: &str, spec: FieldSpec, field: &mut Field) -> Result<(), CronError> {
    let (base, step) = match part.split_once('/') {
        Some((b, s)) => {
            let step: u32 = s.parse().map_err(|_| CronError::BadToken {
                field: spec.name,
                token: part.to_string(),
            })?;
            if step == 0 {
                return Err(CronError::ZeroStep { field: spec.name });
            }
            (b, Some(step))
        }
        None => (part, None),
    };

    // Resolve the base into an inclusive [lo, hi] sweep.
    let (lo, hi) = if base == "*" {
        (spec.lo, spec.hi)
    } else if let Some((a, b)) = base.split_once('-') {
        let a = parse_value(a, spec, part)?;
        let b = parse_value(b, spec, part)?;
        if a > b {
            return Err(CronError::InvertedRange {
                field: spec.name,
                lo: a,
                hi: b,
            });
        }
        (a, b)
    } else {
        let v = parse_value(base, spec, part)?;
        match step {
            // `A/K` (no upper bound) sweeps A..=hi by K (Quartz semantics).
            Some(_) => (v, spec.hi),
            // A bare single value.
            None => (v, v),
        }
    };

    let step = step.unwrap_or(1);
    let mut v = lo;
    while v <= hi {
        field.set(v);
        v += step;
    }
    Ok(())
}

/// Parse a single numeric-or-named value, validated against the field bounds.
fn parse_value(tok: &str, spec: FieldSpec, whole: &str) -> Result<u32, CronError> {
    let tok = tok.trim();
    // Try a three-letter name first (month / day-of-week).
    if !spec.names.is_empty() {
        let lower = tok.to_ascii_lowercase();
        if let Some(i) = spec.names.iter().position(|n| *n == lower) {
            return Ok(spec.lo + i as u32);
        }
    }
    let v: u32 = tok.parse().map_err(|_| CronError::BadToken {
        field: spec.name,
        token: whole.to_string(),
    })?;
    if v < spec.lo || v > spec.hi {
        return Err(CronError::OutOfRange {
            field: spec.name,
            value: v,
            lo: spec.lo,
            hi: spec.hi,
        });
    }
    Ok(v)
}
