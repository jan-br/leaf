//! `JsonConverter` — the JSON [`HttpMessageConverter`](leaf_web::HttpMessageConverter)
//! impl (Spring's `MappingJackson2HttpMessageConverter`). It is where the leaf-web
//! content-negotiation abstraction meets a concrete serde data format
//! (`serde_json`).
//!
//! It is contributed to the container by the [`JsonConverterConfig`]
//! `#[configuration]` holder (itself a `#[component]`) via a `#[bean(provides =
//! "dyn ::leaf_web::HttpMessageConverter")]` factory — the stereotype-generated
//! registration that publishes the `dyn HttpMessageConverter` view, NOT a
//! hand-rolled `Provider`. The typed [`HttpMessageConverterExt::read`] convenience
//! lives here too (it needs serde, which leaf-web does not depend on).

use bytes::Bytes;
use leaf_core::error::{Cause, ErrorKind, LeafError};
use leaf_web::HttpMessageConverter;

/// The typed `read<T>` convenience over any [`HttpMessageConverter`] (concrete or
/// `dyn`). It lives in `leaf-serde` — not on the object-safe leaf-web trait —
/// because deserializing into a `T: serde::de::DeserializeOwned` needs serde,
/// which leaf-web deliberately does not depend on. Blanket-impl'd so callers (the
/// `Json` extractor in Task 4, the controller codegen in Task 9) get a typed
/// `read` on an injected `Ref<dyn HttpMessageConverter>`.
pub trait HttpMessageConverterExt: HttpMessageConverter {
    /// Deserialize `body` into `T` via this converter's erased deserializer.
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] (`ConvertError`) when the body is malformed for this
    /// content-type or does not match `T`'s shape.
    fn read<T: serde::de::DeserializeOwned>(&self, body: &[u8]) -> Result<T, LeafError> {
        // Capture the typed value out of the scoped `with_deserializer` callback: the
        // converter lends an erased deserializer, we run `erased_serde::deserialize`
        // and stash the result, then hand it back to the caller.
        let mut slot: Option<T> = None;
        self.with_deserializer(body, &mut |de| {
            let value = erased_serde::deserialize::<T>(de).map_err(read_error)?;
            slot = Some(value);
            Ok(())
        })?;
        // The contract: a successful `with_deserializer` must have run the callback to
        // completion, so the slot is filled. (The converter only returns `Ok(())` after
        // the callback returned `Ok(())`, which sets the slot.)
        slot.ok_or_else(|| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "http-message-converter read",
                "the converter did not run the read callback",
            ))
        })
    }
}

/// Lift an erased-serde deserialize failure onto leaf's one diagnostic spine as a
/// `ConvertError` (a body→type coercion failure).
fn read_error(e: erased_serde::Error) -> LeafError {
    LeafError::new(ErrorKind::ConvertError)
        .caused_by(Cause::plain("http-message-converter read", e.to_string()))
}

impl<C: HttpMessageConverter + ?Sized> HttpMessageConverterExt for C {}

/// The JSON [`HttpMessageConverter`]: serialize a handler return to a JSON body and
/// deserialize a JSON request body, via `serde_json` — Spring's
/// `MappingJackson2HttpMessageConverter`.
///
/// It is contributed to the container as the `dyn HttpMessageConverter` view by the
/// [`JsonConverterConfig`] `#[configuration]` holder's `#[bean]` factory below (the
/// stereotype-generated registration — no hand-rolled `Provider`), so the server
/// collects it by trait injection (`Vec<Ref<dyn HttpMessageConverter>>`).
#[derive(Clone, Copy, Default)]
pub struct JsonConverter;

impl JsonConverter {
    /// A fresh JSON converter (stateless).
    #[must_use]
    pub fn new() -> Self {
        JsonConverter
    }
}

impl HttpMessageConverter for JsonConverter {
    fn content_type(&self) -> &str {
        "application/json"
    }

    fn write(&self, value: &dyn erased_serde::Serialize) -> Result<Bytes, LeafError> {
        // `&dyn erased_serde::Serialize` is itself `serde::Serialize`, so serde_json
        // serializes it directly — the object-safety payoff.
        let vec = serde_json::to_vec(value).map_err(|e| {
            LeafError::new(ErrorKind::ConvertError)
                .caused_by(Cause::plain("json-converter write", e.to_string()))
        })?;
        Ok(Bytes::from(vec))
    }

    fn with_deserializer(
        &self,
        body: &[u8],
        read: &mut dyn FnMut(&mut dyn erased_serde::Deserializer) -> Result<(), LeafError>,
    ) -> Result<(), LeafError> {
        // The serde_json deserializer lives here, on the converter's stack, for the
        // callback's duration — `serde::Deserializer` is impl'd for `&mut Deserializer`,
        // so the erased view borrows this local and cannot escape (the reason this is a
        // scoped callback rather than a returned box).
        let mut json_de = serde_json::Deserializer::from_slice(body);
        let mut erased = <dyn erased_serde::Deserializer>::erase(&mut json_de);
        read(&mut erased)
    }
}

/// The `#[configuration]` holder that contributes the [`JsonConverter`] as the
/// `dyn HttpMessageConverter` bean. A managed singleton whose `#[bean]` factory
/// (`json_converter`) returns the concrete converter and declares the
/// `dyn HttpMessageConverter` upcast view — the stereotype-generated registration
/// the server's `Vec<Ref<dyn HttpMessageConverter>>` collection injection finds.
///
/// (The struct stereotype takes no `provides` arg; the `#[configuration]` +
/// `#[bean(provides = "dyn …")]` factory is leaf's idiom for a concrete bean that
/// publishes a `dyn` view — the same shape `leaf-redis`'s cache-manager auto-config
/// uses — so this is dogfooded, not a hand-rolled `Provider`.)
#[leaf_macros::component]
pub struct JsonConverterConfig;

impl JsonConverterConfig {
    /// The no-collaborator constructor the `#[component]` provider calls.
    #[must_use]
    pub fn new() -> Self {
        JsonConverterConfig
    }
}

impl Default for JsonConverterConfig {
    fn default() -> Self {
        JsonConverterConfig::new()
    }
}

#[leaf_macros::configuration]
impl JsonConverterConfig {
    /// Contribute the JSON converter as the `dyn HttpMessageConverter` bean.
    #[bean(provides = "dyn ::leaf_web::HttpMessageConverter")]
    fn json_converter(&self) -> JsonConverter {
        JsonConverter::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpMessageConverterExt as _, JsonConverter};
    use leaf_web::HttpMessageConverter;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct ProductDto {
        sku: String,
        name: String,
        price_cents: u32,
    }

    #[test]
    fn json_converter_reports_application_json() {
        let conv = JsonConverter::new();
        assert_eq!(conv.content_type(), "application/json");
    }

    #[test]
    fn json_converter_round_trips_a_struct() {
        let conv = JsonConverter::new();
        let dto = ProductDto {
            sku: "COFFEE".to_string(),
            name: "House Blend".to_string(),
            price_cents: 1299,
        };

        // write -> bytes
        let body = conv.write(&dto).expect("serializes the dto to JSON bytes");
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"sku":"COFFEE","name":"House Blend","price_cents":1299}"#,
        );

        // bytes -> read (typed, via the leaf-serde extension over the dyn-safe trait)
        let back: ProductDto = conv.read(&body).expect("deserializes the JSON bytes back");
        assert_eq!(back, dto);
    }

    #[test]
    fn json_converter_read_via_dyn_trait_object() {
        // The typed `read<T>` is callable on a `dyn HttpMessageConverter` too (the
        // shape Task 6/9 inject) — proving the object-safety boundary holds.
        let conv = JsonConverter::new();
        let dyn_conv: &dyn HttpMessageConverter = &conv;
        let body = dyn_conv
            .write(&ProductDto {
                sku: "TEA".to_string(),
                name: "Earl Grey".to_string(),
                price_cents: 999,
            })
            .expect("serializes through the dyn trait object");
        let back: ProductDto = dyn_conv.read(&body).expect("reads through the dyn trait object");
        assert_eq!(back.sku, "TEA");
        assert_eq!(back.price_cents, 999);
    }

    #[test]
    fn json_converter_read_of_malformed_body_is_a_loud_leaf_error() {
        let conv = JsonConverter::new();
        let err = conv
            .read::<ProductDto>(b"{ this is not json ")
            .expect_err("a malformed body must surface a LeafError, not a panic");
        assert_eq!(err.kind, leaf_core::error::ErrorKind::ConvertError);
    }

    #[test]
    fn json_converter_is_a_dyn_http_message_converter_bean_in_components() {
        // The dogfood claim: the converter reaches the container as a stereotype-
        // generated `#[component]`/`#[bean]` row (NOT a hand-rolled Provider), exposing
        // the `dyn HttpMessageConverter` view the server's collection injection wants.
        // The macro mints the contract at the IMPL's module (`leaf_serde::http_converter`),
        // not this nested `tests` module — so strip the trailing `::tests` from
        // `module_path!()` to name it.
        let impl_mod = module_path!().trim_end_matches("::tests");
        let contract = leaf_core::ContractId::of(&format!("{impl_mod}::json_converter"));
        let bean = leaf_core::COMPONENTS
            .iter()
            .find(|d| d.contract == contract)
            .copied()
            .expect("the #[bean] json_converter factory reaches ::leaf_core::COMPONENTS");

        // The product is the CONCRETE JsonConverter (the factory return type)...
        assert_eq!(bean.self_type, std::any::TypeId::of::<JsonConverter>());
        // ...published under the `dyn HttpMessageConverter` upcast view (the provides[]
        // row a `Vec<Ref<dyn HttpMessageConverter>>` consumer resolves).
        assert!(
            bean.provides
                .iter()
                .any(|r| r.view == std::any::TypeId::of::<dyn HttpMessageConverter>()),
            "the JSON converter bean must declare the dyn HttpMessageConverter view"
        );
    }
}
