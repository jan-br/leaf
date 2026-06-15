//! The `LocaleKey` holder + locale fallback chain (locale-context phase3/10,
//! expr-i18n-resources phase3/11).
//!
//! This crate OWNS the i18n concept without putting it in core: the canonical
//! "current locale" is a [`CxKey`](leaf_core::CxKey) declared HERE (not core,
//! per the TOPOLOGY rule "leaf-i18n declares the LocaleKey holder"), with
//! [`Propagation::Inherit`](leaf_core::Propagation::Inherit) so the ambient
//! locale follows an `@Async`/scheduled hop (it is request/user state, safe to
//! inherit — unlike the Isolate tx binding).
//!
//! The declaration is the hand-written shape the thin `#[holder]` macro would
//! emit (the macro is not in leaf-macros yet — see the crate-root NOTE):
//! `impl CxKey for LocaleKey` + a `const`-constructed [`LOCALE`] accessor. The
//! design's "declare-once-enforced at freeze" lands as the kernel's per-`NAME`
//! collision guard over `CxKey` declarations; `LocaleKey` reserves the canonical
//! `"locale"` name (matching the leaf-tokio/leaf-smol ambient probes).

use leaf_core::{Cx, CxKey, Holder, Locale, Propagation};

/// The canonical "current locale" ambient key (i18n).
///
/// `POLICY = Inherit`: the locale is request/user-scoped presentation state, so
/// it is auto-captured across a spawn hop by the facility's `CxDecorator` — a
/// scheduled/`@Async` body renders in the same locale as the request that armed
/// it. `NAME = "locale"` is the canonical bundle-schema name read by
/// [`MessageSource`](leaf_core::MessageSource) when no explicit locale is passed.
pub struct LocaleKey;

impl CxKey for LocaleKey {
    type Value = Locale;
    const NAME: &'static str = "locale";
    const POLICY: Propagation = Propagation::Inherit;
}

/// The typed accessor over [`LocaleKey`] — `#[holder]`-shaped sugar.
///
/// `LOCALE.scope(locale, fut)` binds the locale for the duration of `fut`;
/// `LOCALE.get()` reads the ambient locale; `LOCALE.with(|l| ..)` borrows it.
pub static LOCALE: Holder<LocaleKey> = Holder::new();

/// Read the ambient [`Locale`] from the current [`Cx`], if one is bound.
///
/// The single "current locale" read [`MessageSource`](leaf_core::MessageSource)
/// performs when its `locale` argument is `None` (Spring's
/// `LocaleContextHolder` feeding the message source). `None` when no locale is
/// in scope — the caller then degrades to the configured default locale (a WARN,
/// not a crash — the degraded-not-fatal posture of phase3/11).
#[must_use]
pub fn current_locale() -> Option<Locale> {
    Cx::current().and_then(|c| c.get::<LocaleKey>().cloned())
}

/// The BCP-47 fallback chain for `locale`, most-specific first, ending at the
/// empty-tag root.
///
/// `de-DE-1996` → `["de-DE-1996", "de-DE", "de", ""]`. A catalog lookup walks
/// this chain so a `de-DE` request resolves a `de`-only entry, and an entry under
/// the root (no suffix) is the last resort before the parent Context's source.
/// Subtags are split on `-` (the BCP-47 separator); a leading/trailing or
/// doubled separator is tolerated (no panic).
#[must_use]
pub fn fallback_chain(locale: &Locale) -> Vec<Locale> {
    let tag = locale.tag();
    let mut chain = Vec::new();
    if !tag.is_empty() {
        chain.push(Locale::new(tag));
        // Progressively trim the last `-`-delimited subtag.
        let mut end = tag.len();
        while let Some(pos) = tag[..end].rfind('-') {
            let trimmed = &tag[..pos];
            if !trimmed.is_empty() {
                chain.push(Locale::new(trimmed));
            }
            end = pos;
        }
    }
    // The empty-tag root is always the final candidate.
    chain.push(Locale::new(""));
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn locale_key_is_inherit_and_named_locale() {
        assert_eq!(<LocaleKey as CxKey>::NAME, "locale");
        assert_eq!(<LocaleKey as CxKey>::POLICY, Propagation::Inherit);
        assert_eq!(LOCALE.name(), "locale");
        assert_eq!(LOCALE.policy(), Propagation::Inherit);
    }

    #[test]
    fn current_locale_reads_the_ambient_holder() {
        // No ambient binding => None.
        assert!(current_locale().is_none());
        // Inside a holder scope => the bound locale.
        let seen = block_on(LOCALE.scope(Locale::new("fr-FR"), async { current_locale() }));
        assert_eq!(seen.map(|l| l.tag().to_string()), Some("fr-FR".to_string()));
        // Restored after the scope.
        assert!(current_locale().is_none());
    }

    #[test]
    fn fallback_chain_truncates_subtags() {
        let chain: Vec<String> = fallback_chain(&Locale::new("de-DE-1996"))
            .iter()
            .map(|l| l.tag().to_string())
            .collect();
        assert_eq!(chain, vec!["de-DE-1996", "de-DE", "de", ""]);
    }

    #[test]
    fn fallback_chain_single_subtag() {
        let chain: Vec<String> = fallback_chain(&Locale::new("en"))
            .iter()
            .map(|l| l.tag().to_string())
            .collect();
        assert_eq!(chain, vec!["en", ""]);
    }

    #[test]
    fn fallback_chain_root_is_just_root() {
        let chain: Vec<String> = fallback_chain(&Locale::new(""))
            .iter()
            .map(|l| l.tag().to_string())
            .collect();
        assert_eq!(chain, vec![""]);
    }
}
