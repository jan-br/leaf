//! Typed reads over the uniform stringly [`AttrSlice`] carriage.
//!
//! conditions-autoconfig (phase3/05): per-member attribute typing is validated
//! at MACRO time, lowering to a uniform `&'static [Attr]` the impl reads back.
//! These helpers are the read side — small, total, side-effect-free.

use leaf_core::{Attr, AttrSlice};
use std::any::TypeId;

/// First `Attr::Str` value for `key`, if present.
#[must_use]
pub fn str_of<'a>(attrs: &'a AttrSlice, key: &str) -> Option<&'a str> {
    attrs.iter().find_map(|a| match a {
        Attr::Str(k, v) if *k == key => Some(*v),
        _ => None,
    })
}

/// First `Attr::Bool` value for `key`, if present.
#[must_use]
pub fn bool_of(attrs: &AttrSlice, key: &str) -> Option<bool> {
    attrs.iter().find_map(|a| match a {
        Attr::Bool(k, v) if *k == key => Some(*v),
        _ => None,
    })
}

/// `Attr::Bool` value for `key`, or `default` when absent.
#[must_use]
pub fn bool_or(attrs: &AttrSlice, key: &str, default: bool) -> bool {
    bool_of(attrs, key).unwrap_or(default)
}

/// First `Attr::Int` value for `key`, if present.
#[must_use]
pub fn int_of(attrs: &AttrSlice, key: &str) -> Option<i64> {
    attrs.iter().find_map(|a| match a {
        Attr::Int(k, v) if *k == key => Some(*v),
        _ => None,
    })
}

/// First `Attr::Type` value for `key`, if present.
#[must_use]
pub fn type_of(attrs: &AttrSlice, key: &str) -> Option<TypeId> {
    attrs.iter().find_map(|a| match a {
        Attr::Type(k, v) if *k == key => Some(*v),
        _ => None,
    })
}

/// All `Attr::Str` values for `key`, in order (the multi-name property form).
#[must_use]
pub fn all_str<'a>(attrs: &'a AttrSlice, key: &str) -> Vec<&'a str> {
    attrs
        .iter()
        .filter_map(|a| match a {
            Attr::Str(k, v) if *k == key => Some(*v),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    static ATTRS: &[Attr] = &[
        Attr::Str("name", "leaf.redis.enabled"),
        Attr::Str("name", "leaf.redis.url"),
        Attr::Str("having_value", "true"),
        Attr::Bool("match_if_missing", true),
        Attr::Int("at_least", 17),
    ];

    #[test]
    fn str_of_returns_the_first_match() {
        assert_eq!(str_of(&ATTRS, "name"), Some("leaf.redis.enabled"));
        assert_eq!(str_of(&ATTRS, "having_value"), Some("true"));
        assert_eq!(str_of(&ATTRS, "absent"), None);
    }

    #[test]
    fn all_str_returns_every_match_in_order() {
        assert_eq!(
            all_str(&ATTRS, "name"),
            vec!["leaf.redis.enabled", "leaf.redis.url"]
        );
    }

    #[test]
    fn bool_and_int_reads() {
        assert_eq!(bool_of(&ATTRS, "match_if_missing"), Some(true));
        assert!(!bool_or(&ATTRS, "missing", false));
        assert!(bool_or(&ATTRS, "match_if_missing", false));
        assert_eq!(int_of(&ATTRS, "at_least"), Some(17));
    }
}
