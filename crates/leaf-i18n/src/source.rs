//! [`HierarchicalMessageSource`] — the always-present hierarchy-aware
//! [`MessageSource`] bean (expr-i18n-resources
//! phase3/11).
//!
//! Resolution is the unification primitive of phase3/11: a sync-pure resolution
//! over (a) the discovered catalog providers, (b) the ambient `Cx`
//! (`Holder<LocaleKey>`), and (c) a parent source for the container hierarchy.
//! The walk order, given `message(code, args, locale)`:
//!
//! 1. **Locale.** `locale = locale.or(ambient LocaleKey).or(default_locale)`
//!    — the single "current locale" read of phase3/11 (None reads
//!    [`current_locale`]); a missing ambient degrades to
//!    the configured default, never a crash.
//! 2. **Per-locale fallback.** For each tag in
//!    [`fallback_chain`] (`de-DE` → `de` → root), ask each
//!    provider IN ORDER (child catalogs shadow parent — registration order is
//!    precedence).
//! 3. **Hierarchy.** On a total local miss, delegate to the `parent`
//!    [`MessageSource`] (the `Context::parent()` chain — one-directional, child
//!    shadows parent), passing the RESOLVED locale so the parent does not re-read
//!    a possibly-different ambient.
//! 4. **Default / error.** `message_or`/`resolve` fall back to the supplied
//!    default; `message`/`resolve`-without-default produce a
//!    [`ErrorKind::NoSuchMessage`] node.
//!
//! A resolved pattern's `{n}` placeholders are substituted by
//! [`format_pattern`]. The async-across-`dyn`
//! boxing is accepted (phase3/11 5b); a compiled-in [`StaticCatalog`](crate::StaticCatalog) hit is a
//! ready future, so a hit allocates only the `Box<dyn Future>` and the `Arc<str>`.

use std::sync::Arc;

use leaf_core::{
    Arg, BoxFuture, Cause, ErrorKind, LeafError, Locale, MessageCatalogProvider, MessagePattern,
    MessageResolvable, MessageSource,
};

use crate::format::format_pattern;
use crate::locale::{current_locale, fallback_chain};

/// The hierarchy-aware [`MessageSource`] over a list of catalog providers.
///
/// Built via [`HierarchicalMessageSource::builder`]. Providers are consulted in
/// registration order (earlier shadows later). Holds an optional `parent`
/// [`MessageSource`] (the container hierarchy) and a `default_locale` used when
/// neither an explicit nor an ambient locale is available.
pub struct HierarchicalMessageSource {
    providers: Box<[Arc<dyn MessageCatalogProvider>]>,
    parent: Option<Arc<dyn MessageSource>>,
    default_locale: Locale,
}

impl HierarchicalMessageSource {
    /// Start building a source (default locale defaults to the root `""` tag).
    #[must_use]
    pub fn builder() -> HierarchicalMessageSourceBuilder {
        HierarchicalMessageSourceBuilder {
            providers: Vec::new(),
            parent: None,
            default_locale: Locale::new(""),
        }
    }

    /// The effective locale for a request: explicit, else ambient, else default.
    fn effective_locale(&self, locale: Option<&Locale>) -> Locale {
        locale
            .cloned()
            .or_else(current_locale)
            .unwrap_or_else(|| self.default_locale.clone())
    }

    /// Walk the providers across the locale fallback chain, returning the first
    /// hit's raw pattern (no arg substitution yet).
    async fn lookup_pattern(&self, code: &str, locale: &Locale) -> Option<MessagePattern> {
        for tag in fallback_chain(locale) {
            for provider in &self.providers {
                if let Some(pat) = provider.lookup(code, &tag).await {
                    return Some(pat);
                }
            }
        }
        None
    }

    /// Resolve `code` to a rendered string for the RESOLVED `locale`, consulting
    /// the local providers then the parent. `None` on a total miss.
    async fn resolve_code(&self, code: &str, args: &[Arg<'_>], locale: &Locale) -> Option<Arc<str>> {
        if let Some(pat) = self.lookup_pattern(code, locale).await {
            return Some(Arc::from(format_pattern(&pat.0, args)));
        }
        // Hierarchy: delegate to the parent with the ALREADY-resolved locale, so
        // the parent does not re-read a possibly-different ambient binding.
        if let Some(parent) = &self.parent
            && let Ok(s) = parent.message(code, args, Some(locale)).await
        {
            return Some(s);
        }
        None
    }
}

impl MessageSource for HierarchicalMessageSource {
    fn message<'a>(
        &'a self,
        code: &'a str,
        args: &'a [Arg<'a>],
        locale: Option<&'a Locale>,
    ) -> BoxFuture<'a, Result<Arc<str>, LeafError>> {
        Box::pin(async move {
            let loc = self.effective_locale(locale);
            match self.resolve_code(code, args, &loc).await {
                Some(s) => Ok(s),
                None => Err(no_such_message(code, &loc)),
            }
        })
    }

    fn message_or<'a>(
        &'a self,
        code: &'a str,
        args: &'a [Arg<'a>],
        default: &'a str,
        locale: Option<&'a Locale>,
    ) -> BoxFuture<'a, Arc<str>> {
        Box::pin(async move {
            let loc = self.effective_locale(locale);
            match self.resolve_code(code, args, &loc).await {
                Some(s) => s,
                // The default itself carries `{n}` placeholders (Spring parity).
                None => Arc::from(format_pattern(default, args)),
            }
        })
    }

    fn resolve<'a>(
        &'a self,
        r: &'a dyn MessageResolvable,
        locale: Option<&'a Locale>,
    ) -> BoxFuture<'a, Result<Arc<str>, LeafError>> {
        Box::pin(async move {
            let loc = self.effective_locale(locale);
            let args = r.arguments();
            for code in r.codes() {
                if let Some(s) = self.resolve_code(code, args, &loc).await {
                    return Ok(s);
                }
            }
            match r.default_message() {
                Some(d) => Ok(Arc::from(format_pattern(d, args))),
                None => {
                    let code = r.codes().first().copied().unwrap_or("");
                    Err(no_such_message(code, &loc))
                }
            }
        })
    }
}

/// Build a [`ErrorKind::NoSuchMessage`] node naming the unresolved code+locale —
/// one node of the one diagnostic chain.
fn no_such_message(code: &str, locale: &Locale) -> LeafError {
    LeafError::new(ErrorKind::NoSuchMessage).caused_by(Cause::plain(
        "resolving message",
        format!("no message for code `{code}` in locale `{}`", locale.tag()),
    ))
}

/// Builder for [`HierarchicalMessageSource`].
pub struct HierarchicalMessageSourceBuilder {
    providers: Vec<Arc<dyn MessageCatalogProvider>>,
    parent: Option<Arc<dyn MessageSource>>,
    default_locale: Locale,
}

impl HierarchicalMessageSourceBuilder {
    /// Append a catalog provider (earlier-added shadows later-added).
    #[must_use]
    pub fn provider(mut self, p: Arc<dyn MessageCatalogProvider>) -> Self {
        self.providers.push(p);
        self
    }

    /// Set the parent [`MessageSource`] (the container hierarchy delegate).
    #[must_use]
    pub fn parent(mut self, parent: Arc<dyn MessageSource>) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Set the default locale used when neither an explicit nor an ambient
    /// locale is available.
    #[must_use]
    pub fn default_locale(mut self, locale: Locale) -> Self {
        self.default_locale = locale;
        self
    }

    /// Finish building.
    #[must_use]
    pub fn build(self) -> HierarchicalMessageSource {
        HierarchicalMessageSource {
            providers: self.providers.into_boxed_slice(),
            parent: self.parent,
            default_locale: self.default_locale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::StaticCatalog;
    use crate::locale::LOCALE;
    use futures::executor::block_on;

    fn source() -> HierarchicalMessageSource {
        let cat = StaticCatalog::builder("messages")
            .entry("en", "greeting", "Hello, {0}!")
            .entry("de", "greeting", "Hallo, {0}!")
            .entry("de", "farewell", "Tschüss")
            // A `de-DE`-specific override exists for `greeting` only.
            .entry("de-DE", "greeting", "Moin, {0}!")
            .root_entry("app.name", "Leaf")
            .build();
        HierarchicalMessageSource::builder()
            .provider(Arc::new(cat))
            .default_locale(Locale::new("en"))
            .build()
    }

    #[test]
    fn resolves_by_code_for_an_explicit_locale() {
        let ms = source();
        let s = block_on(ms.message("greeting", &[Arg::Str("Jan")], Some(&Locale::new("en"))))
            .unwrap();
        assert_eq!(&*s, "Hello, Jan!");
    }

    #[test]
    fn falls_back_along_the_locale_chain() {
        let ms = source();
        // `de-CH` has no entry; falls back to `de`.
        let s = block_on(ms.message("farewell", &[], Some(&Locale::new("de-CH")))).unwrap();
        assert_eq!(&*s, "Tschüss");
    }

    #[test]
    fn more_specific_locale_shadows_less_specific() {
        let ms = source();
        // `de-DE` has its own `greeting` override; `de` does NOT win.
        let s =
            block_on(ms.message("greeting", &[Arg::Str("Jan")], Some(&Locale::new("de-DE")))).unwrap();
        assert_eq!(&*s, "Moin, Jan!");
    }

    #[test]
    fn root_entry_resolves_for_any_locale() {
        let ms = source();
        let s = block_on(ms.message("app.name", &[], Some(&Locale::new("fr-FR")))).unwrap();
        assert_eq!(&*s, "Leaf");
    }

    #[test]
    fn unknown_code_is_no_such_message() {
        let ms = source();
        let e = block_on(ms.message("absent", &[], Some(&Locale::new("en")))).unwrap_err();
        assert_eq!(e.kind, ErrorKind::NoSuchMessage);
    }

    #[test]
    fn message_or_falls_back_to_default() {
        let ms = source();
        let s = block_on(ms.message_or("absent", &[Arg::Str("X")], "default {0}", None));
        assert_eq!(&*s, "default X");
    }

    #[test]
    fn reads_ambient_locale_when_none_passed() {
        let ms = source();
        // No locale arg, no ambient => effective locale is the default (en).
        let en = block_on(ms.message("greeting", &[Arg::Str("Jan")], None)).unwrap();
        assert_eq!(&*en, "Hello, Jan!");

        // With an ambient LocaleKey of `de`, the SAME None-locale call resolves
        // the German pattern — the canonical Holder<LocaleKey> read.
        let de = block_on(LOCALE.scope(Locale::new("de"), async {
            ms.message("greeting", &[Arg::Str("Jan")], None).await
        }))
        .unwrap();
        assert_eq!(&*de, "Hallo, Jan!");
    }

    #[test]
    fn resolvable_tries_codes_then_default() {
        struct V;
        impl MessageResolvable for V {
            fn codes(&self) -> &[&str] {
                &["absent", "farewell"]
            }
            fn arguments(&self) -> &[Arg<'_>] {
                &[]
            }
            fn default_message(&self) -> Option<&str> {
                Some("fallback")
            }
        }
        let ms = source();
        // Second code resolves before the default (in `de`).
        let s = block_on(ms.resolve(&V, Some(&Locale::new("de")))).unwrap();
        assert_eq!(&*s, "Tschüss");
    }

    #[test]
    fn hierarchy_delegates_to_parent_on_local_miss() {
        // Parent owns `only.in.parent`; child does not.
        let parent_cat = StaticCatalog::builder("parent")
            .entry("en", "only.in.parent", "from parent")
            .build();
        let parent: Arc<dyn MessageSource> = Arc::new(
            HierarchicalMessageSource::builder()
                .provider(Arc::new(parent_cat))
                .default_locale(Locale::new("en"))
                .build(),
        );
        let child_cat = StaticCatalog::builder("child")
            .entry("en", "greeting", "child hello")
            .build();
        let child = HierarchicalMessageSource::builder()
            .provider(Arc::new(child_cat))
            .parent(parent)
            .default_locale(Locale::new("en"))
            .build();

        // Child shadows: its own code resolves locally.
        let local = block_on(child.message("greeting", &[], Some(&Locale::new("en")))).unwrap();
        assert_eq!(&*local, "child hello");
        // A local miss walks to the parent.
        let inherited =
            block_on(child.message("only.in.parent", &[], Some(&Locale::new("en")))).unwrap();
        assert_eq!(&*inherited, "from parent");
    }

    #[test]
    fn object_safe_as_dyn_message_source() {
        let ms = source();
        let dynms: &dyn MessageSource = &ms;
        let s = block_on(dynms.message("app.name", &[], Some(&Locale::new("en")))).unwrap();
        assert_eq!(&*s, "Leaf");
    }
}
