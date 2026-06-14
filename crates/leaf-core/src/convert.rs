//! The canonical coercion trait: [`FromConfigValue`] + [`ConvertCtx`].
//!
//! Trait-native, compile-time-dispatched conversion (binding-conversion phase3/07
//! `type-conversion`): a config field's target type implements [`FromConfigValue`]
//! and is monomorphized at the statically-known type, so a missing impl is a
//! Tier-0 compile error and a coercion failure is a Tier-2 [`ErrorKind::ConvertError`]
//! [`LeafError`] node carrying the raw value, the concrete target type name (free,
//! statically known), and the origin. There is NO mandatory runtime registry on
//! the default path; the erased [`crate::bind::ConversionService`] is an opt-in
//! fallback owned by the binder unit.
//!
//! leaf-core ships the blanket impls every config field needs:
//! - `impl<T: FromStr> FromConfigValue for T` — every number/bool/`String`/`char`
//!   etc. (the FromStr-bridge);
//! - leaf-owned grammar newtypes [`Duration`], [`DataSize`], [`Period`] with the
//!   Spring suffix grammar (`30s`, `10MB`, `2d`);
//! - collections [`Vec<T>`]/[`std::collections::HashSet<T>`] (comma-split) and
//!   [`std::collections::HashMap<String, V>`] (here only the comma/`=` scalar
//!   form; the binder owns indexed-children descent);
//! - `Option<T>` (a blank scalar is `None`).
//!
//! [`ConvertCtx`] carries the strict/lenient [`Leniency`] policy and the optional
//! unit-default hint (from a `#[config(unit="s")]` attribute the binder/value
//! macro forwards — NOT an ambient thread-local, honoring ADR-07). Locale is left
//! as a forward-compatible `()` placeholder until the i18n unit lands.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use crate::error::{Cause, ErrorKind, LeafError, Origin};

/// Strict-vs-lenient conversion policy threaded explicitly via [`ConvertCtx`].
///
/// `Strict` (the framework default) turns a coercion failure into a `Fatal`
/// [`LeafError`]; `Lenient` is the Spring-migrant downgrade lever (the bind/value
/// macro flips it, and the caller can map the error to `Severity::Warn`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Leniency {
    /// Framework default: a coercion failure aborts.
    #[default]
    Strict,
    /// Spring-migrant downgrade: a coercion failure is recoverable.
    Lenient,
}

/// A unit-default hint forwarded from a `#[config(unit="…")]` attribute.
///
/// When a duration/size field's source value omits a suffix (`timeout=30`), the
/// converter applies this unit. It is passed EXPLICITLY (never an ambient global)
/// so the conversion is referentially transparent across `.await`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnitHint {
    /// Nanoseconds (duration default unit).
    Nanos,
    /// Microseconds.
    Micros,
    /// Milliseconds.
    Millis,
    /// Seconds.
    Seconds,
    /// Minutes.
    Minutes,
    /// Hours.
    Hours,
    /// Days.
    Days,
    /// Bytes (data-size default unit).
    Bytes,
    /// Kilobytes (1024 bytes).
    Kilobytes,
    /// Megabytes.
    Megabytes,
    /// Gigabytes.
    Gigabytes,
}

/// The context threaded through every [`FromConfigValue::from_config_value`].
///
/// `#[non_exhaustive]` so adding the locale handle (i18n unit) and any future
/// hint is additive.
#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
pub struct ConvertCtx {
    /// Strict/lenient policy (forwarded by the bind/value macro).
    pub policy: Leniency,
    /// Optional unit default for a suffix-less duration/size value.
    pub unit: Option<UnitHint>,
}

impl Default for ConvertCtx {
    fn default() -> Self {
        ConvertCtx {
            policy: Leniency::Strict,
            unit: None,
        }
    }
}

impl ConvertCtx {
    /// A strict context with no unit hint (the common case).
    #[must_use]
    pub fn strict() -> Self {
        ConvertCtx::default()
    }

    /// A lenient context.
    #[must_use]
    pub fn lenient() -> Self {
        ConvertCtx {
            policy: Leniency::Lenient,
            unit: None,
        }
    }

    /// Set the unit-default hint (builder style).
    #[must_use]
    pub fn with_unit(mut self, unit: UnitHint) -> Self {
        self.unit = Some(unit);
        self
    }
}

/// The origin-carrying value a converter reads.
///
/// On the scalar default path this is a borrowed raw string plus the upstream
/// [`Origin`] (replaced by an interned `OriginId` once the OriginStore unit
/// lands). The binder supplies list/map cursors via the typed `bind` descent
/// instead — those never reach [`FromConfigValue`] directly.
#[derive(Clone, Debug)]
pub struct ConfigValue<'a> {
    /// The raw, post-placeholder string value.
    pub raw: Cow<'a, str>,
    /// Provenance of the value (file:line / source).
    pub origin: Origin,
}

impl<'a> ConfigValue<'a> {
    /// A scalar value with `Origin::Unknown` (tests / programmatic use).
    #[must_use]
    pub fn scalar(raw: impl Into<Cow<'a, str>>) -> Self {
        ConfigValue {
            raw: raw.into(),
            origin: Origin::Unknown,
        }
    }

    /// Attach an origin (builder style).
    #[must_use]
    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    /// The trimmed raw string (Spring trims scalar property values).
    #[must_use]
    pub fn trimmed(&self) -> &str {
        self.raw.trim()
    }
}

/// THE canonical coercion trait (binding-conversion phase3/07).
///
/// Implemented at a statically-known target type; a missing impl is a compile
/// error, and a failure is the one [`ErrorKind::ConvertError`] [`LeafError`].
pub trait FromConfigValue: Sized {
    /// Coerce `v` to `Self` under the policy/hints in `cx`.
    ///
    /// # Errors
    /// Returns an [`ErrorKind::ConvertError`] [`LeafError`] (carrying the raw
    /// value, the target type name, and the origin) when the value cannot be
    /// coerced.
    fn from_config_value(v: &ConfigValue<'_>, cx: &ConvertCtx) -> Result<Self, LeafError>;
}

/// Build a uniform `ConvertError` node naming the value, the target type, and
/// the origin (the diagnostic shape the spec mandates).
fn convert_error(raw: &str, target: &'static str, origin: Origin) -> LeafError {
    LeafError::new(ErrorKind::ConvertError)
        .with_origin(origin)
        .caused_by(
            Cause::plain(
                "converting config value",
                format!("cannot convert {raw:?} to `{target}`"),
            )
            .with_origin(origin),
        )
}

// ── the FromStr bridge ──
//
// NOTE on the design vs stable-Rust coherence reality: the phase3/07 sketch
// writes `impl<T: FromStr> FromConfigValue for T`, but on stable that blanket
// CONFLICTS with the collection impls (`Vec<T>`/`HashSet<T>`/`HashMap`/`Option<T>`)
// because the coherence checker conservatively assumes std MIGHT add `FromStr`
// for those types in a future version (the standard E0119 false-positive). The
// idiomatic stable realization — used by every config crate (config-rs, figment,
// envy) — is to enumerate the scalar target types via one macro that forwards to
// `FromStr`. This preserves the design's INTENT (trait-native, compile-time
// dispatch, a missing impl is a compile error, the failure is one ConvertError)
// while leaving the collection/Option/grammar impls coherent. A foreign scalar
// type the user owns implements `FromConfigValue` directly (or via the macro
// unit's `#[derive(LeafEnum)]` / `Conv<T>` orphan-escape).

/// Implement [`FromConfigValue`] for a `FromStr` scalar target, forwarding to
/// `FromStr` and naming the target type in any [`ErrorKind::ConvertError`].
#[macro_export]
macro_rules! impl_from_config_value_via_fromstr {
    ($($t:ty),+ $(,)?) => {
        $(
            impl $crate::convert::FromConfigValue for $t {
                fn from_config_value(
                    v: &$crate::convert::ConfigValue<'_>,
                    _cx: &$crate::convert::ConvertCtx,
                ) -> ::core::result::Result<Self, $crate::error::LeafError> {
                    let s = v.trimmed();
                    <$t as ::core::str::FromStr>::from_str(s).map_err(|_| {
                        $crate::convert::convert_error_public(s, ::core::stringify!($t), v.origin)
                    })
                }
            }
        )+
    };
}

/// Public re-export of the internal [`convert_error`] builder so the
/// [`impl_from_config_value_via_fromstr`] macro (which expands in downstream
/// crates) can mint the canonical `ConvertError` node.
#[doc(hidden)]
#[must_use]
pub fn convert_error_public(raw: &str, target: &'static str, origin: Origin) -> LeafError {
    convert_error(raw, target, origin)
}

impl_from_config_value_via_fromstr!(
    bool,
    char,
    String,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    f32,
    f64,
    std::net::IpAddr,
    std::net::Ipv4Addr,
    std::net::Ipv6Addr,
    std::net::SocketAddr,
    std::path::PathBuf,
    std::num::NonZeroU16,
    std::num::NonZeroU32,
    std::num::NonZeroU64,
    std::num::NonZeroUsize,
);

// ── leaf-owned grammar newtypes (Spring suffix grammar) ──

/// A leaf-owned duration with the Spring suffix grammar (`ns`/`us`/`ms`/`s`/`m`/
/// `h`/`d`), defaulting to the [`ConvertCtx`] unit hint (else seconds) for a
/// bare number. Wraps [`std::time::Duration`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Duration(pub std::time::Duration);

impl Duration {
    /// The wrapped [`std::time::Duration`].
    #[must_use]
    pub fn get(self) -> std::time::Duration {
        self.0
    }
}

impl FromConfigValue for Duration {
    fn from_config_value(v: &ConfigValue<'_>, cx: &ConvertCtx) -> Result<Self, LeafError> {
        let s = v.trimmed();
        let (num, unit) = split_suffix(s);
        let value: u64 = num
            .parse()
            .map_err(|_| convert_error(s, "Duration", v.origin))?;
        let nanos: u128 = match unit {
            "ns" => u128::from(value),
            "us" | "µs" => u128::from(value) * 1_000,
            "ms" => u128::from(value) * 1_000_000,
            "s" => u128::from(value) * 1_000_000_000,
            "m" => u128::from(value) * 60 * 1_000_000_000,
            "h" => u128::from(value) * 3_600 * 1_000_000_000,
            "d" => u128::from(value) * 86_400 * 1_000_000_000,
            "" => {
                // No suffix: apply the unit hint, default seconds.
                let mult = match cx.unit {
                    Some(UnitHint::Nanos) => 1,
                    Some(UnitHint::Micros) => 1_000,
                    Some(UnitHint::Millis) => 1_000_000,
                    Some(UnitHint::Minutes) => 60 * 1_000_000_000,
                    Some(UnitHint::Hours) => 3_600 * 1_000_000_000,
                    Some(UnitHint::Days) => 86_400 * 1_000_000_000,
                    // Seconds is the duration default; non-duration hints fall back.
                    _ => 1_000_000_000,
                };
                u128::from(value) * mult
            }
            _ => return Err(convert_error(s, "Duration", v.origin)),
        };
        let secs = (nanos / 1_000_000_000) as u64;
        let sub = (nanos % 1_000_000_000) as u32;
        Ok(Duration(std::time::Duration::new(secs, sub)))
    }
}

/// A leaf-owned data size with the Spring suffix grammar (`B`/`KB`/`MB`/`GB`/
/// `TB`, base-1024), defaulting to the unit hint (else bytes) for a bare number.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct DataSize(pub u64);

impl DataSize {
    /// The size in bytes.
    #[must_use]
    pub fn bytes(self) -> u64 {
        self.0
    }
}

impl FromConfigValue for DataSize {
    fn from_config_value(v: &ConfigValue<'_>, cx: &ConvertCtx) -> Result<Self, LeafError> {
        let s = v.trimmed();
        let (num, unit) = split_suffix(s);
        let value: u64 = num
            .parse()
            .map_err(|_| convert_error(s, "DataSize", v.origin))?;
        let mult: u64 = match unit.to_ascii_uppercase().as_str() {
            "B" => 1,
            "KB" => 1024,
            "MB" => 1024 * 1024,
            "GB" => 1024 * 1024 * 1024,
            "TB" => 1024 * 1024 * 1024 * 1024,
            "" => match cx.unit {
                Some(UnitHint::Kilobytes) => 1024,
                Some(UnitHint::Megabytes) => 1024 * 1024,
                Some(UnitHint::Gigabytes) => 1024 * 1024 * 1024,
                _ => 1,
            },
            _ => return Err(convert_error(s, "DataSize", v.origin)),
        };
        value
            .checked_mul(mult)
            .map(DataSize)
            .ok_or_else(|| convert_error(s, "DataSize", v.origin))
    }
}

/// A leaf-owned ISO-8601-ish period with the Spring suffix grammar (`y`/`m`/`w`/
/// `d`), captured as a normalized day count for the common case. A bare number
/// is days.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Period {
    /// The number of days the period denotes (weeks ×7, years ×365, months ×30).
    pub days: i64,
}

impl FromConfigValue for Period {
    fn from_config_value(v: &ConfigValue<'_>, _cx: &ConvertCtx) -> Result<Self, LeafError> {
        let s = v.trimmed();
        let (num, unit) = split_suffix(s);
        let value: i64 = num
            .parse()
            .map_err(|_| convert_error(s, "Period", v.origin))?;
        let days = match unit {
            "d" | "" => value,
            "w" => value * 7,
            "m" => value * 30,
            "y" => value * 365,
            _ => return Err(convert_error(s, "Period", v.origin)),
        };
        Ok(Period { days })
    }
}

/// Split a numeric-prefix + alpha-suffix value (`30s`, `10MB`, `2d`) into its
/// `(number, suffix)` parts. The number part keeps a leading sign and digits;
/// the suffix is the trailing non-digit run.
fn split_suffix(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut split = 0;
    // Allow a leading sign on the number.
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        split = i;
    }
    (&s[..split], s[split..].trim())
}

// ── Option<T>: a blank scalar is None, else delegate ──

impl<T: FromConfigValue> FromConfigValue for Option<T> {
    fn from_config_value(v: &ConfigValue<'_>, cx: &ConvertCtx) -> Result<Self, LeafError> {
        if v.trimmed().is_empty() {
            Ok(None)
        } else {
            T::from_config_value(v, cx).map(Some)
        }
    }
}

// ── collections: comma-split scalar form (the binder owns indexed children) ──

impl<T: FromConfigValue> FromConfigValue for Vec<T> {
    fn from_config_value(v: &ConfigValue<'_>, cx: &ConvertCtx) -> Result<Self, LeafError> {
        split_list(&v.raw)
            .map(|item| T::from_config_value(&ConfigValue::scalar(item).with_origin(v.origin), cx))
            .collect()
    }
}

impl<T> FromConfigValue for HashSet<T>
where
    T: FromConfigValue + Eq + Hash,
{
    fn from_config_value(v: &ConfigValue<'_>, cx: &ConvertCtx) -> Result<Self, LeafError> {
        split_list(&v.raw)
            .map(|item| T::from_config_value(&ConfigValue::scalar(item).with_origin(v.origin), cx))
            .collect()
    }
}

impl<V> FromConfigValue for HashMap<String, V>
where
    V: FromConfigValue,
{
    fn from_config_value(v: &ConfigValue<'_>, cx: &ConvertCtx) -> Result<Self, LeafError> {
        // Scalar map form: `k1=v1,k2=v2` (the binder owns the indexed-children
        // descent for nested maps; this is the inline-string convenience form).
        let mut out = HashMap::new();
        for entry in split_list(&v.raw) {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (k, val) = entry
                .split_once('=')
                .ok_or_else(|| convert_error(entry, "HashMap entry (expected `k=v`)", v.origin))?;
            let value =
                V::from_config_value(&ConfigValue::scalar(val.trim()).with_origin(v.origin), cx)?;
            out.insert(k.trim().to_string(), value);
        }
        Ok(out)
    }
}

/// Split a comma-separated scalar list, trimming each element and dropping a
/// single trailing empty (so `"a,b,"` is two elements). An all-blank string is
/// the empty list.
fn split_list(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv<T: FromConfigValue>(raw: &str) -> Result<T, LeafError> {
        T::from_config_value(&ConfigValue::scalar(raw.to_string()), &ConvertCtx::strict())
    }

    // ── the FromStr bridge: u16, bool, numbers ─────────────────────────────

    #[test]
    fn fromstr_bridge_coerces_u16() {
        assert_eq!(conv::<u16>("8080").unwrap(), 8080);
        // Spring trims scalar values.
        assert_eq!(conv::<u16>("  443  ").unwrap(), 443);
    }

    #[test]
    fn fromstr_bridge_coerces_bool() {
        assert!(conv::<bool>("true").unwrap());
        assert!(!conv::<bool>("false").unwrap());
    }

    #[test]
    fn u16_overflow_is_a_convert_error_naming_target_and_value() {
        let err = conv::<u16>("70000").expect_err("overflows u16");
        assert_eq!(err.kind, ErrorKind::ConvertError);
        let rendered = err
            .chain
            .first()
            .map(|c| c.detail.to_string())
            .unwrap_or_default();
        assert!(rendered.contains("70000"), "names value: {rendered}");
        assert!(rendered.contains("u16"), "names target: {rendered}");
    }

    // ── Duration grammar ────────────────────────────────────────────────────

    #[test]
    fn duration_suffix_grammar() {
        assert_eq!(
            conv::<Duration>("30s").unwrap().get(),
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            conv::<Duration>("500ms").unwrap().get(),
            std::time::Duration::from_millis(500)
        );
        assert_eq!(
            conv::<Duration>("2h").unwrap().get(),
            std::time::Duration::from_secs(7200)
        );
        assert_eq!(
            conv::<Duration>("1d").unwrap().get(),
            std::time::Duration::from_secs(86_400)
        );
    }

    #[test]
    fn duration_bare_number_defaults_to_seconds_else_unit_hint() {
        // Default unit is seconds.
        assert_eq!(
            conv::<Duration>("5").unwrap().get(),
            std::time::Duration::from_secs(5)
        );
        // A unit hint overrides the default.
        let cx = ConvertCtx::strict().with_unit(UnitHint::Millis);
        let d = Duration::from_config_value(&ConfigValue::scalar("250"), &cx).unwrap();
        assert_eq!(d.get(), std::time::Duration::from_millis(250));
    }

    #[test]
    fn duration_bad_value_is_convert_error() {
        assert_eq!(
            conv::<Duration>("abc").unwrap_err().kind,
            ErrorKind::ConvertError
        );
        assert_eq!(
            conv::<Duration>("10x").unwrap_err().kind,
            ErrorKind::ConvertError
        );
    }

    // ── DataSize grammar ────────────────────────────────────────────────────

    #[test]
    fn datasize_suffix_grammar_base_1024() {
        assert_eq!(conv::<DataSize>("10MB").unwrap().bytes(), 10 * 1024 * 1024);
        assert_eq!(conv::<DataSize>("512B").unwrap().bytes(), 512);
        assert_eq!(conv::<DataSize>("1KB").unwrap().bytes(), 1024);
        // Bare number defaults to bytes.
        assert_eq!(conv::<DataSize>("2048").unwrap().bytes(), 2048);
    }

    // ── Period grammar ──────────────────────────────────────────────────────

    #[test]
    fn period_suffix_grammar() {
        assert_eq!(conv::<Period>("2d").unwrap().days, 2);
        assert_eq!(conv::<Period>("3w").unwrap().days, 21);
        assert_eq!(conv::<Period>("1y").unwrap().days, 365);
        assert_eq!(conv::<Period>("10").unwrap().days, 10);
    }

    // ── Option<T> ───────────────────────────────────────────────────────────

    #[test]
    fn option_blank_is_none_else_some() {
        assert_eq!(conv::<Option<u16>>("").unwrap(), None);
        assert_eq!(conv::<Option<u16>>("  ").unwrap(), None);
        assert_eq!(conv::<Option<u16>>("42").unwrap(), Some(42));
    }

    // ── collections ─────────────────────────────────────────────────────────

    #[test]
    fn vec_comma_split() {
        assert_eq!(conv::<Vec<u16>>("1,2,3").unwrap(), vec![1, 2, 3]);
        assert_eq!(conv::<Vec<u16>>("1, 2 , 3").unwrap(), vec![1, 2, 3]);
        // Trailing empty element dropped.
        assert_eq!(conv::<Vec<u16>>("7,").unwrap(), vec![7]);
        assert!(conv::<Vec<u16>>("").unwrap().is_empty());
    }

    #[test]
    fn vec_propagates_element_convert_error() {
        let err = conv::<Vec<u16>>("1,bad,3").expect_err("bad element");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn hashset_dedups() {
        let s = conv::<HashSet<u16>>("1,2,2,3").unwrap();
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn hashmap_scalar_kv_form() {
        let m = conv::<HashMap<String, u16>>("a=1,b=2").unwrap();
        assert_eq!(m.get("a"), Some(&1));
        assert_eq!(m.get("b"), Some(&2));
    }

    #[test]
    fn hashmap_missing_eq_is_convert_error() {
        assert_eq!(
            conv::<HashMap<String, u16>>("a=1,oops").unwrap_err().kind,
            ErrorKind::ConvertError
        );
    }

    #[test]
    fn convert_error_carries_origin() {
        let v = ConfigValue::scalar("nope").with_origin(Origin::TestDouble);
        let err = u16::from_config_value(&v, &ConvertCtx::strict()).unwrap_err();
        assert_eq!(err.origin, Origin::TestDouble);
    }
}
