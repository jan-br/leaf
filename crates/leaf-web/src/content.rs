//! The [`HttpMessageConverter`] content-negotiation seam (Spring's
//! `HttpMessageConverter`): serialize a handler return into a body and
//! deserialize a request body into a typed value, keyed by content-type.
//!
//! This trait is the leaf-web ABSTRACTION; the concrete JSON impl
//! (`JsonConverter`) lives in `leaf-serde` as a `#[component]` bean and is
//! injected here as `Ref<dyn HttpMessageConverter>` — leaf-web names no serde
//! data format itself.
//!
//! ## Why `erased_serde`
//!
//! A converter is a `dyn` bean (collected/injected), so its trait must be
//! object-safe — but a `dyn` object cannot carry a generic `write<T: Serialize>` /
//! `read<T: DeserializeOwned>` method. The standard answer is the `erased-serde`
//! object-safety boundary: [`write`](HttpMessageConverter::write) takes a
//! `&dyn erased_serde::Serialize`, and
//! [`with_deserializer`](HttpMessageConverter::with_deserializer) lends a
//! `&mut dyn erased_serde::Deserializer` to a scoped callback a typed reader drives
//! (a callback, not a returned box — the underlying `serde::Deserializer` borrows a
//! stack local at the format boundary, so it cannot escape the converter). leaf-web
//! depends only on `erased-serde` (object-safety vocabulary), never on `serde`
//! itself — the typed `read<T: DeserializeOwned>` convenience + the serde data
//! format (serde_json) both live in `leaf-serde`, so the abstraction stays backend-
//! and format-free.

use bytes::Bytes;
use leaf_core::LeafError;

/// Serialize a value into an HTTP body and deserialize a body into a value, for
/// one content-type (Spring's `HttpMessageConverter`).
///
/// Object-safe: it is contributed as a `dyn HttpMessageConverter` bean and
/// injected (`Ref<dyn HttpMessageConverter>`) into the rest-controller codegen
/// (return serialization, Task 9) and the `Json` extractor (body deserialization,
/// Task 4). The typed `read<T>` convenience lives in `leaf-serde` (where serde is
/// a dependency) as a blanket extension over this trait, so it is callable on both
/// a concrete converter and a `dyn` one.
pub trait HttpMessageConverter: Send + Sync {
    /// The MIME content-type this converter produces/consumes, e.g.
    /// `"application/json"`. Used for the `Content-Type` header and (later)
    /// content negotiation.
    fn content_type(&self) -> &str;

    /// Serialize `value` into a body. The value is the erased
    /// `erased_serde::Serialize` view of a handler return, so the trait stays
    /// object-safe.
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] when the value cannot be serialized in this
    /// content-type.
    fn write(&self, value: &dyn erased_serde::Serialize) -> Result<Bytes, LeafError>;

    /// Lend a `&mut dyn erased_serde::Deserializer` over `body` to the scoped `read`
    /// callback for a typed reader to drive, returning whatever it yields.
    ///
    /// Object-safe (no generic on the trait object) and borrow-correct: the
    /// underlying format deserializer lives on the converter's stack for the
    /// callback's duration, so it never escapes. `leaf-serde`'s typed `read<T>`
    /// extension passes a callback that runs `erased_serde::deserialize::<T>`.
    ///
    /// # Errors
    ///
    /// Returns the callback's [`LeafError`] (a malformed body or typed-shape
    /// mismatch), or a converter-level [`LeafError`] if the body cannot be opened in
    /// this content-type.
    fn with_deserializer(
        &self,
        body: &[u8],
        read: &mut dyn FnMut(&mut dyn erased_serde::Deserializer) -> Result<(), LeafError>,
    ) -> Result<(), LeafError>;
}

// Make `dyn HttpMessageConverter` an injectable VIEW (the by-trait-injection seam,
// emitted ONCE — orphan-rule-OK since `dyn HttpMessageConverter` is local to this
// crate). `leaf-serde`'s `JsonConverter` publishes the `dyn HttpMessageConverter` view
// (`#[bean(provides = "dyn ::leaf_web::HttpMessageConverter")]`); the rest-controller
// codegen (Stage 2) and the `Json` extractor inject it as `Ref<dyn HttpMessageConverter>`
// (and the server may collect `Vec<Ref<dyn HttpMessageConverter>>` for negotiation).
leaf_core::impl_resolve_view!(dyn HttpMessageConverter);
