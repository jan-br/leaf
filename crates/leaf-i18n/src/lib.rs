//! `leaf-i18n` — owns the i18n CONCEPT without putting it in core.
//!
//! Realizes the `messages-i18n` feature of phase3/11 (`expr-i18n-resources`) and
//! the `locale-context` slice of phase3/10 (`execution-context`) over the
//! leaf-core ABI. Per the TOPOLOGY rule, the i18n concept is OWNED here (a
//! `leaf-core`-only optional integration crate), never hardcoded in the kernel:
//!
//! - **[`LocaleKey`] holder ([`locale`]).** The canonical "current locale" is a
//!   [`CxKey`](leaf_core::CxKey) declared HERE with
//!   [`Propagation::Inherit`](leaf_core::Propagation::Inherit) (locale is
//!   request/user presentation state, safe to inherit across a spawn — unlike the
//!   Isolate tx binding). The [`LOCALE`] accessor is the `#[holder]`-shaped sugar.
//!   `MessageSource` reads it via [`current_locale`] when no explicit locale is
//!   passed (Spring's `LocaleContextHolder` feeding the source).
//! - **[`StaticCatalog`] ([`catalog`]).** An in-memory
//!   [`MessageCatalogProvider`](leaf_core::MessageCatalogProvider): the flat
//!   `(locale, code) → pattern` table the `register_catalog!`/build.rs codegen
//!   fronts would emit and `Context::refresh()` auto-detects as a
//!   `Role::Infrastructure` bean.
//! - **[`HierarchicalMessageSource`] ([`source`]).** The hierarchy-aware
//!   [`MessageSource`](leaf_core::MessageSource): resolves a code by walking the
//!   per-locale fallback chain over the providers, reads the ambient
//!   [`LocaleKey`] when locale is `None`, delegates to a parent source on a local
//!   miss (the container hierarchy, child-shadows-parent), and substitutes
//!   positional message-format args ([`format`](mod@format)).
//!
//! All resolution rides the ONE leaf-core spine: the async-across-`dyn`
//! [`BoxFuture`](leaf_core::BoxFuture) seam, the [`Arc<str>`](std::sync::Arc)
//! resolved-message ownership, and the
//! [`ErrorKind::NoSuchMessage`](leaf_core::ErrorKind::NoSuchMessage) diagnostic
//! node. No external i18n/ICU dependency; only `leaf-core`.
//!
//! ## `#[holder]` (WIRED) + deferred catalog CONSUMPTION (honest NOTEs)
//!
//! - **`#[holder]` macro — WIRED.** [`LocaleKey`] now declares the canonical
//!   `"locale"` key via `#[holder(name = "locale", policy = inherit, value =
//!   leaf_core::Locale)]`: the macro emits the `impl CxKey for LocaleKey` + the
//!   const-constructed [`LOCALE`](leaf_core::Holder) accessor `static` (the same
//!   hand pattern this crate used to spell out, now sugared). The design's
//!   "declare-once-enforced at freeze" remains the kernel's per-`NAME` `CxKey`
//!   collision guard, not enforced in this crate.
//! - **Catalog CONSUMPTION + the `CATALOGS` self-check.** The `#[catalog]` macro
//!   (the emission side) is wired in leaf-macros; what is still deferred is catalog
//!   CONSUMPTION: the `ExpectedManifest`/`CATALOGS`-linkme anti-DCE self-check and
//!   the `messageSource` magic-name-or-`DelegatingMessageSource` install are
//!   leaf-boot's concern (per the phase3/11 crate hints). This crate ships the
//!   RUNTIME shapes ([`StaticCatalog`]/[`HierarchicalMessageSource`]) those fronts
//!   target; a catalog is built explicitly via its builder here.
//! - **Locale-sensitive formatting.** `{n}` substitution ([`format`](mod@format)) is the
//!   POSITIONAL, locale-insensitive subset; locale-aware number/date/plural
//!   formatting is delegated to the type-conversion neighbour (phase3/11) and is
//!   not yet wired.
//! - **Resource-backed catalogs.** A directory-watching / `classpath:`-bundle
//!   catalog provider (a `ResourceLoader` consumer) is out of scope here; the
//!   compiled-in [`StaticCatalog`] is the always-ready variant.

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod catalog;
pub mod format;
pub mod locale;
pub mod source;

pub use catalog::{StaticCatalog, StaticCatalogBuilder};
pub use format::format_pattern;
pub use locale::{current_locale, fallback_chain, LocaleKey, LOCALE};
pub use source::{HierarchicalMessageSource, HierarchicalMessageSourceBuilder};
