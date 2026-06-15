//! The [`PlaceholderEngine`] — `${...}` resolution over the layered sources,
//! delegating coercion to leaf-core's [`leaf_core::FromConfigValue`].
//!
//! This is the config-side façade over leaf-core's hand-rolled, escape-aware,
//! recursive `${...}` scanner ([`leaf_core::resolve_lenient`] /
//! [`leaf_core::resolve_strict`]): it binds the scanner's `lookup` closure to a
//! SEALED [`leaf_core::Env`] (or any [`leaf_core::PropertySource`] stack via
//! [`leaf_core::SealedStack`]) so a placeholder key is resolved first-source-wins
//! over the layered stack, with `${key:default}` defaulting, `${${meta}}` nesting,
//! cycle detection, and the depth cap all inherited from leaf-core unchanged.
//!
//! `resolve_as::<T>` is the typed read: expand the template strictly, then hand
//! the resolved string to [`leaf_core::FromConfigValue`] (the ONE coercion seam) —
//! never a parallel converter. This is the engine the `@Value` dry-run and the
//! binder's scalar leaf both ultimately drive.

use leaf_core::{
    ConfigValue, ConvertCtx, Env, FromConfigValue, LeafError, PlaceholderSyntax, PropertyValue,
    SealedStack,
};

/// The lookup seam the placeholder scanner walks — anything that answers a raw
/// (relaxed-aware) key with its winning [`PropertyValue`].
///
/// Implemented for [`Env`] (walks the sealed stack + parent) and [`SealedStack`]
/// (a bare stack), so the engine is identical over a full env or a sub-stack.
pub trait LayeredLookup {
    /// First-source-wins raw lookup (NO placeholder expansion).
    fn lookup_raw(&self, key: &str) -> Option<PropertyValue>;
}

impl LayeredLookup for Env {
    fn lookup_raw(&self, key: &str) -> Option<PropertyValue> {
        self.get_raw(key)
    }
}

impl LayeredLookup for SealedStack {
    fn lookup_raw(&self, key: &str) -> Option<PropertyValue> {
        self.get(key)
    }
}

/// The `${...}` resolution engine over a layered source stack.
///
/// Borrows the lookup source + the (frozen) [`PlaceholderSyntax`]; reads are
/// synchronous, lock-free, and allocation-light (the `Cow` fast path borrows
/// when nothing fired). Strict/lenient is the per-call method split, exactly
/// mirroring leaf-core's two-method contract.
pub struct PlaceholderEngine<'a, S: LayeredLookup> {
    source: &'a S,
    syntax: &'a PlaceholderSyntax,
}

impl<'a, S: LayeredLookup> PlaceholderEngine<'a, S> {
    /// Build over `source` with the Spring-default grammar.
    #[must_use]
    pub fn new(source: &'a S, syntax: &'a PlaceholderSyntax) -> Self {
        PlaceholderEngine { source, syntax }
    }

    fn lookup(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |k: &str| self.source.lookup_raw(k).map(|v| v.raw.into_owned())
    }

    /// Resolve `${...}` in `text` LENIENTLY (unresolved left literal).
    #[must_use]
    pub fn resolve(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        leaf_core::resolve_lenient(text, self.syntax, &self.lookup())
    }

    /// Resolve `${...}` in `text` STRICTLY.
    ///
    /// # Errors
    /// [`leaf_core::ErrorKind::UnresolvedValue`] for an unresolved mandatory
    /// `${...}` (no default), a placeholder cycle, or a depth-cap blowout.
    pub fn resolve_strict(&self, text: &str) -> Result<String, LeafError> {
        leaf_core::resolve_strict(text, self.syntax, &self.lookup())
    }

    /// Resolve a KEY's value through the stack, then expand its placeholders.
    ///
    /// `None` iff the key is absent (lenient expansion of the value). This is
    /// the `Env::get`-equivalent driven by this engine.
    #[must_use]
    pub fn resolve_key(&self, key: &str) -> Option<String> {
        let pv = self.source.lookup_raw(key)?;
        Some(
            leaf_core::resolve_lenient(&pv.raw, self.syntax, &self.lookup())
                .into_owned(),
        )
    }

    /// Typed resolve: strictly expand `text`, then coerce to `T` via the ONE
    /// [`FromConfigValue`] seam under `cx`.
    ///
    /// # Errors
    /// [`leaf_core::ErrorKind::UnresolvedValue`] if expansion fails, or
    /// [`leaf_core::ErrorKind::ConvertError`] if the resolved string cannot be
    /// coerced to `T`.
    pub fn resolve_as<T: FromConfigValue>(
        &self,
        text: &str,
        cx: &ConvertCtx,
    ) -> Result<T, LeafError> {
        let resolved = self.resolve_strict(text)?;
        let cv = ConfigValue::scalar(resolved);
        T::from_config_value(&cv, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{EnvBuilder, MapPropertySource};
    use std::sync::Arc;

    fn env_with(pairs: &[(&str, &str)]) -> Env {
        let src = MapPropertySource::from_pairs(
            "test",
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(src));
        b.seal_env()
    }

    #[test]
    fn resolves_placeholder_over_layered_stack() {
        let env = env_with(&[("host", "localhost"), ("port", "8080")]);
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(&env, &syn);
        assert_eq!(eng.resolve("http://${host}:${port}"), "http://localhost:8080");
    }

    #[test]
    fn placeholder_default_used_when_absent() {
        let env = env_with(&[]);
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(&env, &syn);
        // lenient: default applies.
        assert_eq!(eng.resolve("${port:9090}"), "9090");
        // strict: default still satisfies.
        assert_eq!(eng.resolve_strict("${port:9090}").unwrap(), "9090");
    }

    #[test]
    fn strict_errors_on_unresolved_lenient_leaves_literal() {
        let env = env_with(&[]);
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(&env, &syn);
        assert_eq!(eng.resolve("${missing}"), "${missing}");
        let err = eng.resolve_strict("${missing}").unwrap_err();
        assert_eq!(err.kind, leaf_core::ErrorKind::UnresolvedValue);
    }

    #[test]
    fn higher_source_overrides_in_placeholder_lookup() {
        // The placeholder walk reads first-source-wins over the layered stack.
        let high = MapPropertySource::from_pairs("high", [("k", "winner")]);
        let low = MapPropertySource::from_pairs("low", [("k", "loser")]);
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(high));
        b.add_last(Arc::new(low));
        let env = b.seal_env();
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(&env, &syn);
        assert_eq!(eng.resolve("${k}"), "winner");
    }

    #[test]
    fn nested_placeholder_in_key_resolves() {
        let env = env_with(&[("meta", "real"), ("real", "value")]);
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(&env, &syn);
        assert_eq!(eng.resolve("${${meta}}"), "value");
    }

    #[test]
    fn circular_placeholder_is_caught_strict() {
        let env = env_with(&[("a", "${b}"), ("b", "${a}")]);
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(&env, &syn);
        let err = eng.resolve_strict("${a}").unwrap_err();
        assert_eq!(err.kind, leaf_core::ErrorKind::UnresolvedValue);
    }

    #[test]
    fn resolve_as_delegates_coercion_to_from_config_value() {
        let env = env_with(&[("max", "443")]);
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(&env, &syn);
        let cx = ConvertCtx::strict();
        let port: u16 = eng.resolve_as("${max}", &cx).unwrap();
        assert_eq!(port, 443);
        // A duration grammar value coerces through the same seam.
        let d: leaf_core::Duration = eng.resolve_as("${timeout:30s}", &cx).unwrap();
        assert_eq!(d.get(), std::time::Duration::from_secs(30));
    }

    #[test]
    fn resolve_as_surfaces_convert_error() {
        let env = env_with(&[("bad", "not-a-number")]);
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(&env, &syn);
        let cx = ConvertCtx::strict();
        let err = eng.resolve_as::<u16>("${bad}", &cx).unwrap_err();
        assert_eq!(err.kind, leaf_core::ErrorKind::ConvertError);
    }

    #[test]
    fn resolve_key_expands_value_placeholders() {
        let env = env_with(&[("base", "localhost"), ("url", "http://${base}/api")]);
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(&env, &syn);
        assert_eq!(eng.resolve_key("url").as_deref(), Some("http://localhost/api"));
        assert_eq!(eng.resolve_key("absent"), None);
    }

    #[test]
    fn works_over_a_bare_sealed_stack() {
        // The engine is identical over a SealedStack (no Env wrapper).
        let env = env_with(&[("x", "y")]);
        let stack = &env.core().stack;
        let syn = PlaceholderSyntax::spring();
        let eng = PlaceholderEngine::new(stack, &syn);
        assert_eq!(eng.resolve("${x}"), "y");
    }
}
