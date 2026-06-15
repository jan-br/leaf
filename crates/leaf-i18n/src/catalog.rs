//! [`StaticCatalog`] — an in-memory [`MessageCatalogProvider`](leaf_core::MessageCatalogProvider)
//! (expr-i18n-resources phase3/11).
//!
//! A catalog is the origin-agnostic backend the hierarchy [`MessageSource`] fans
//! out over. `StaticCatalog` is the compiled-in / hand-built variant: a map of
//! `(locale-tag, code) → pattern` populated at construction. It is the testable
//! shape the `register_catalog!` macro / build.rs `.ftl`/`.properties` codegen
//! would emit (those codegen fronts are NOTed as deferred at the crate root) and
//! the kind of provider `Context::refresh()` auto-detects as a
//! `Role::Infrastructure` bean.
//!
//! Lookup is an exact `(tag, code)` hit (ready-future, no IO). The per-locale
//! FALLBACK walk (`de-DE` → `de` → root) is the [`MessageSource`]'s job
//! ([`crate::source`]), so a catalog stays a flat, dumb table — one
//! responsibility each.

use std::collections::HashMap;

use leaf_core::{BoxFuture, Locale, MessageCatalogProvider, MessagePattern};

/// An in-memory message catalog: `(locale-tag, code) → pattern`.
///
/// Construct via [`StaticCatalog::builder`]; entries are keyed by the EXACT
/// locale tag (`"de"`, `"de-DE"`, or `""` for the locale-neutral root). The
/// owning [`MessageSource`](leaf_core::MessageSource) supplies the fallback walk,
/// so register a pattern under exactly the tag(s) it should answer for.
pub struct StaticCatalog {
    name: String,
    // (locale-tag, code) → pattern. A BTree-free flat map: read-mostly, small.
    entries: HashMap<(String, String), MessagePattern>,
}

impl StaticCatalog {
    /// Start building a catalog named `name` (the diagnostic / chain-order name).
    #[must_use]
    pub fn builder(name: impl Into<String>) -> StaticCatalogBuilder {
        StaticCatalogBuilder {
            name: name.into(),
            entries: HashMap::new(),
        }
    }

    /// The number of `(locale, code)` entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the catalog holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl MessageCatalogProvider for StaticCatalog {
    fn lookup<'a>(
        &'a self,
        code: &'a str,
        locale: &'a Locale,
    ) -> BoxFuture<'a, Option<MessagePattern>> {
        // Exact (tag, code) lookup — ready future, no IO (phase3/11: compiled-in
        // catalog lookups return ready futures).
        Box::pin(async move {
            self.entries
                .get(&(locale.tag().to_string(), code.to_string()))
                .cloned()
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Builder for a [`StaticCatalog`].
pub struct StaticCatalogBuilder {
    name: String,
    entries: HashMap<(String, String), MessagePattern>,
}

impl StaticCatalogBuilder {
    /// Add `code → pattern` under the exact `locale` tag.
    #[must_use]
    pub fn entry(
        mut self,
        locale: impl Into<String>,
        code: impl Into<String>,
        pattern: impl Into<String>,
    ) -> Self {
        self.entries.insert(
            (locale.into(), code.into()),
            MessagePattern(pattern.into().into()),
        );
        self
    }

    /// Add `code → pattern` under the locale-neutral root (the `""` tag).
    #[must_use]
    pub fn root_entry(self, code: impl Into<String>, pattern: impl Into<String>) -> Self {
        self.entry("", code, pattern)
    }

    /// Finish building.
    #[must_use]
    pub fn build(self) -> StaticCatalog {
        StaticCatalog {
            name: self.name,
            entries: self.entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    fn sample() -> StaticCatalog {
        StaticCatalog::builder("messages")
            .entry("en", "greeting", "Hello, {0}!")
            .entry("de", "greeting", "Hallo, {0}!")
            .root_entry("app.name", "Leaf")
            .build()
    }

    #[test]
    fn looks_up_exact_locale_and_code() {
        let cat = sample();
        let en = block_on(cat.lookup("greeting", &Locale::new("en")));
        assert_eq!(en.map(|p| p.0.to_string()), Some("Hello, {0}!".to_string()));
        let de = block_on(cat.lookup("greeting", &Locale::new("de")));
        assert_eq!(de.map(|p| p.0.to_string()), Some("Hallo, {0}!".to_string()));
    }

    #[test]
    fn miss_on_unknown_code_or_locale() {
        let cat = sample();
        assert!(block_on(cat.lookup("absent", &Locale::new("en"))).is_none());
        // No exact `de-DE` entry — the catalog does NOT do fallback itself.
        assert!(block_on(cat.lookup("greeting", &Locale::new("de-DE"))).is_none());
    }

    #[test]
    fn root_entry_is_under_empty_tag() {
        let cat = sample();
        let hit = block_on(cat.lookup("app.name", &Locale::new("")));
        assert_eq!(hit.map(|p| p.0.to_string()), Some("Leaf".to_string()));
    }

    #[test]
    fn name_and_len() {
        let cat = sample();
        assert_eq!(cat.name(), "messages");
        assert_eq!(cat.len(), 3);
        assert!(!cat.is_empty());
        assert!(StaticCatalog::builder("empty").build().is_empty());
    }

    #[test]
    fn object_safe_as_dyn_provider() {
        let cat = sample();
        let p: &dyn MessageCatalogProvider = &cat;
        assert_eq!(p.name(), "messages");
        assert!(block_on(p.lookup("greeting", &Locale::new("en"))).is_some());
    }
}
