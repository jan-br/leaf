//! The [`FromRequest`] argument-extraction seam + the typed extractor wrappers
//! ([`Path`] / [`Query`] / [`Json`] / [`Header`] / [`State`]). Spring's
//! `HandlerMethodArgumentResolver` family, expressed in leaf's vocabulary.
//!
//! A controller method's parameters each resolve from the inbound [`Request`] via
//! their STRUCTURAL extractor type — the controller codegen (Task 9) dispatches on
//! the parameter's shape (`Path<_>` / `Query<_>` / `Json<_>` / `State<_>` /
//! `&Request`), NEVER on a spelled type name, consistent with the no-type-names
//! rule. The codegen threads a uniform per-argument binding context ([`ExtractCtx`],
//! carrying the handler parameter NAME) into every extractor; only the impls that
//! need a fact read it. The request-only extractions get their [`FromRequest`] impls
//! here:
//!
//! - [`Query`]`<`[`HashMap`]`<String, String>>` — the raw query string parsed into
//!   a name→value map.
//! - [`Request`] — the whole request, cloned (the `&Request` extractor).
//!
//! [`Path<T>`] (a `{name}` capture, e.g. `sku` from `/products/{sku}`) is request-only
//! but NAME-dependent: it implements the context-aware [`FromRequestParts`] directly,
//! selecting ITS OWN capture by the handler parameter's name and parsing it via
//! [`FromStr`]. The extractions that need a serde data format ([`Json<T>`] body
//! deserialization, [`Query<T>`] for an arbitrary `Deserialize` target) or the DI
//! container ([`State<T>`], a collaborator bean) are NOT plain `from_request(req)`
//! impls — they need a converter / the handler's captured `ResolveCtx` that only
//! the controller codegen (Task 9) has in scope. leaf-web defines their wrapper
//! types here (so the codegen can dispatch on them structurally) and documents
//! the seam; the serde-backed reads ride the injected
//! [`HttpMessageConverter`] (Task 5) — leaf-web names
//! no serde data format itself.

use std::collections::HashMap;
use std::str::FromStr;

use leaf_core::error::{Cause, ErrorKind, LeafError};

use crate::content::HttpMessageConverter;
use crate::Request;

/// The per-argument BINDING CONTEXT the controller codegen threads into EVERY extractor,
/// uniformly, alongside the request + converter. It carries the static facts about the
/// handler PARAMETER an extractor may need to resolve itself — at present the parameter
/// NAME (the `Pat::Ident` the codegen already reads), which the path-parameter extractor
/// matches against the route's `{name}` captures.
///
/// Threading one neutral context to every extractor keeps the codegen's dispatch the
/// uniform `<Ty as FromRequestParts>::from_request_parts(req, converter, ctx)` — only the
/// impls that NEED a fact read it (e.g. [`Path<T>`] reads the name); the request-only
/// extractors ignore it. The macro never branches on the parameter being a `Path`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtractCtx<'a> {
    /// The handler parameter's name (its `Pat::Ident`), if the codegen had one to give.
    /// The path-parameter extractor matches this against the route's `{name}` captures.
    name: Option<&'a str>,
}

impl<'a> ExtractCtx<'a> {
    /// A context carrying the handler parameter's NAME (the common codegen case — every
    /// `fn handler(&self, sku: Path<String>)` parameter has a `Pat::Ident`).
    #[must_use]
    pub fn named(name: &'a str) -> Self {
        ExtractCtx { name: Some(name) }
    }

    /// A context carrying no parameter name (a destructured / wildcard parameter the
    /// codegen could not name). Name-dependent extractors fail loudly rather than guess.
    #[must_use]
    pub fn empty() -> Self {
        ExtractCtx { name: None }
    }

    /// The handler parameter's name, if any.
    #[must_use]
    pub fn name(&self) -> Option<&'a str> {
        self.name
    }
}

/// Resolve `Self` from an inbound [`Request`] ALONE — the request-only, name-free
/// extractions (Spring's `HandlerMethodArgumentResolver`). This is the simple base the
/// converter-aware [`FromRequestParts`] supersets via a blanket impl; the controller
/// codegen always calls the [`FromRequestParts`] seam (which a `FromRequest` extractor
/// reaches through the blanket), dispatching on the parameter's structural extractor
/// type — never a spelled type name.
///
/// # Errors
///
/// An extractor that cannot produce its value (a missing required path param, a
/// malformed body) returns a loud [`LeafError`] — the dispatcher (Task 6) maps it
/// to a 4xx via the advice chain rather than ever silently defaulting.
pub trait FromRequest: Sized {
    /// Extract `Self` from `req`, or fail loudly with a [`LeafError`].
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] when the request does not carry what this extractor
    /// requires (see the trait docs).
    fn from_request(req: &Request) -> Result<Self, LeafError>;
}

/// A path-parameter extractor: `T` is read from the captured `{name}` segment whose
/// name matches the handler PARAMETER (e.g. `sku: Path<String>` reads `{sku}` from
/// `/products/{sku}`). Spring's `@PathVariable`.
///
/// `Path<T>` is the ONE request-only extractor that needs the per-argument binding
/// context: its capture is selected BY NAME (the handler parameter's `Pat::Ident`,
/// carried in the [`ExtractCtx`]), so a multi-capture route like
/// `/users/{uid}/orders/{oid}` binds EACH `Path` parameter to ITS OWN capture rather
/// than all to the first. The captured segment is parsed to `T` via [`FromStr`]
/// (`Path<String>` is the identity case; `Path<u32>` parses a numeric segment), so a
/// malformed segment is a loud [`ErrorKind::ConvertError`] — never a panic. Because the
/// name reaches it through the uniform context, the controller codegen still dispatches
/// purely on the parameter's STRUCTURAL extractor type — it never branches on `Path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path<T>(pub T);

/// A query-string extractor: `T` is decoded from the request's query (e.g.
/// `?page=2&size=10`). Spring's `@RequestParam`.
///
/// The [`FromRequest`] impl here covers `Query<HashMap<String, String>>` — the
/// raw name→value map, needing no serde. A typed `Query<Struct>` rides the serde
/// data format through the controller codegen (Task 9), not a plain impl here
/// (leaf-web names no serde format).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query<T>(pub T);

/// A request-body extractor: `T` is deserialized from the body in the negotiated
/// content-type. Spring's `@RequestBody`.
///
/// `Json<T>` has NO plain [`FromRequest`] impl (which sees only the request): a body
/// deserialization needs the content format. Instead it implements the
/// CONVERTER-AWARE [`FromRequestParts`], deserializing the body through the INJECTED
/// [`HttpMessageConverter`] (the JSON impl is a `#[component]` bean in `leaf-serde`)
/// — leaf-web names the serde data MODEL (the `T: DeserializeOwned` bound) but no
/// serde FORMAT, which stays on the converter's side. The controller codegen (Task
/// 9) lowers EVERY parameter through the one uniform `FromRequestParts` call site, so
/// `Json<T>` rides the same seam as a request-only `Path`/`Query`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Json<T>(pub T);

/// A single-header extractor: `T` is read from the named request header. Spring's
/// `@RequestHeader`.
///
/// `Header<T>` carries no header NAME on its own, so it has no plain
/// [`FromRequest`] impl: the name is a codegen concern (Spring's
/// `@RequestHeader("X-Foo")`). The controller codegen (Task 9) reads the named
/// header off the [`Request`] directly; this wrapper exists so the codegen can
/// dispatch on its structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header<T>(pub T);

/// A DI-collaborator extractor: `T` is a bean resolved from the container (Spring's
/// constructor-injected collaborator, surfaced as a handler argument).
///
/// `State<T>` has NO plain [`FromRequest`] impl: a bean is resolved from the
/// handler's captured `ResolveCtx`, which only the controller codegen (Task 9) has
/// in scope — not from the [`Request`]. This wrapper exists so the codegen can
/// dispatch on its structure and fill it from the captured `Ref<T>`.
#[derive(Clone, Debug)]
pub struct State<T>(pub T);

impl FromRequest for Query<HashMap<String, String>> {
    /// Parse the raw query string into a name→value map (last value wins on a
    /// repeated key). An absent query string yields an empty map (a query is
    /// optional by default; a required key is the typed-target / codegen concern).
    /// Values are taken verbatim — percent-decoding is a backend / typed-target
    /// follow-up, not this raw-map extractor's job.
    fn from_request(req: &Request) -> Result<Self, LeafError> {
        let mut map = HashMap::new();
        if let Some(query) = req.query_str() {
            for pair in query.split('&').filter(|s| !s.is_empty()) {
                let (key, value) = match pair.split_once('=') {
                    Some((k, v)) => (k.to_string(), v.to_string()),
                    None => (pair.to_string(), String::new()),
                };
                map.insert(key, value);
            }
        }
        Ok(Query(map))
    }
}

impl FromRequest for Request {
    /// The whole request, cloned — the `&Request` extractor (a controller method
    /// that wants raw access). [`Request`] is a cheap clone (the body is
    /// [`Bytes`](bytes::Bytes)).
    fn from_request(req: &Request) -> Result<Self, LeafError> {
        Ok(req.clone())
    }
}

/// The CONVERTER-AWARE argument-extraction seam the controller codegen calls
/// uniformly, once per handler parameter:
/// `<ParamTy as FromRequestParts>::from_request_parts(req, converter, ctx)`.
///
/// It is the superset of [`FromRequest`]: a parameter type satisfies it EITHER via
/// the request alone (the [`FromRequest`] blanket below — `Query<HashMap>`, the
/// whole-[`Request`]) OR by reading the per-argument binding [`ExtractCtx`] (the
/// name-dependent [`Path<T>`]) OR by riding the injected [`HttpMessageConverter`]
/// (the [`Json<T>`] body deserialization). The codegen dispatches on the parameter's
/// STRUCTURAL extractor type purely through TRAIT resolution (which impl applies),
/// never a spelled type name — so one uniform call site lowers every parameter,
/// `Path<T>` and `Json<T>` included.
///
/// leaf-web names the serde DATA MODEL (the `serde::de::DeserializeOwned` bound on
/// the [`Json<T>`] impl, the same boundary [`erased_serde`] already crosses) but no
/// serde FORMAT: the concrete wire format is the injected converter's
/// (`leaf-serde`'s `JsonConverter` = `serde_json`).
///
/// # Errors
///
/// An extractor that cannot produce its value (a missing path param, a malformed
/// body) returns a loud [`LeafError`] — the dispatcher maps it to a 4xx via the
/// advice chain rather than ever silently defaulting.
pub trait FromRequestParts: Sized {
    /// Extract `Self` from `req`, using `converter` for the body-deserializing
    /// extractors ([`Json<T>`]) and `ctx` for the name-dependent ones ([`Path<T>`]);
    /// the request-only extractors ignore both.
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] when the request/body does not carry what this
    /// extractor requires (see the trait docs).
    fn from_request_parts(
        req: &Request,
        converter: &dyn HttpMessageConverter,
        ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError>;
}

/// Every request-only NAME-FREE [`FromRequest`] extractor (`Query<HashMap>`, the
/// whole [`Request`]) ALSO satisfies the converter-aware [`FromRequestParts`] — it
/// just ignores the converter and the binding context. This blanket is why the
/// controller codegen can call ONE uniform `from_request_parts(req, converter, ctx)`
/// per parameter and let trait resolution pick the request-only path, the
/// name-dependent [`Path<T>`] path, or the converter-backed [`Json<T>`] path.
impl<T: FromRequest> FromRequestParts for T {
    fn from_request_parts(
        req: &Request,
        _converter: &dyn HttpMessageConverter,
        _ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError> {
        T::from_request(req)
    }
}

/// `Path<T>` reads ITS OWN captured `{name}` segment — selected by the handler
/// PARAMETER name carried in the binding [`ExtractCtx`] — and parses it to `T` via
/// [`FromStr`]. This is why a multi-capture route like `/users/{uid}/orders/{oid}`
/// binds EACH `Path` parameter to its own capture (not all to the first): each
/// parameter's `ExtractCtx::name` selects the matching `{name}`. `Path<String>` is
/// the identity parse; `Path<u32>` parses a numeric segment.
///
/// A missing capture (no `{name}` matched on the request) or a parse failure is a
/// loud [`ErrorKind::ConvertError`] the dispatcher maps via the advice chain — never
/// a panic, never a silent default. `T::Err: Display` so the failed parse's own
/// message is carried in the cause.
impl<T> FromRequestParts for Path<T>
where
    T: FromStr,
    T::Err: core::fmt::Display,
{
    fn from_request_parts(
        req: &Request,
        _converter: &dyn HttpMessageConverter,
        ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError> {
        let name = ctx.name().ok_or_else(|| {
            missing(
                "path parameter",
                "the path extractor has no parameter name to select its capture",
            )
        })?;
        let raw = req.path_param(name).ok_or_else(|| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "path parameter",
                format!("no captured path parameter `{{{name}}}` on the request"),
            ))
        })?;
        let value = raw.parse::<T>().map_err(|e| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "path parameter",
                format!("path parameter `{name}` = `{raw}` did not parse: {e}"),
            ))
        })?;
        Ok(Path(value))
    }
}

/// `Json<T>` body extraction: deserialize the request body into `T` through the
/// INJECTED [`HttpMessageConverter`] (the format-agnostic seam). leaf-web names no
/// serde format — it runs [`erased_serde::deserialize`] over the deserializer the
/// converter lends via [`HttpMessageConverter::with_deserializer`], so the concrete
/// format (serde_json, in `leaf-serde`'s converter) stays on the converter's side.
///
/// This is the ONE extractor that genuinely needs the converter, which is exactly
/// why the codegen threads it through [`FromRequestParts`] rather than the
/// request-only [`FromRequest`].
impl<T: serde::de::DeserializeOwned> FromRequestParts for Json<T> {
    fn from_request_parts(
        req: &Request,
        converter: &dyn HttpMessageConverter,
        _ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError> {
        // Capture the typed value out of the converter's scoped `with_deserializer`
        // callback: it lends an erased deserializer over the body, we run
        // `erased_serde::deserialize::<T>` and stash the result.
        let mut slot: Option<T> = None;
        converter.with_deserializer(req.body_bytes(), &mut |de| {
            slot = Some(erased_serde::deserialize::<T>(de).map_err(|e| {
                LeafError::new(ErrorKind::ConvertError)
                    .caused_by(Cause::plain("json body extraction", e.to_string()))
            })?);
            Ok(())
        })?;
        // A successful `with_deserializer` ran the callback to completion (it only
        // returns `Ok(())` after the callback's `Ok(())`, which fills the slot).
        slot.map(Json).ok_or_else(|| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "json body extraction",
                "the converter did not run the read callback",
            ))
        })
    }
}

/// Build the loud [`LeafError`] a failed extraction surfaces (the dispatcher maps
/// it to a 4xx via the advice chain — never a silent default).
fn missing(what: &'static str, detail: &'static str) -> LeafError {
    LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(what, detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::Method;

    fn request(uri: &str, params: Vec<(String, String)>) -> Request {
        let mut req =
            Request::new(Method::GET, uri.parse().expect("uri parses"), http::HeaderMap::new(), Bytes::new());
        req.set_path_params(params);
        req
    }

    #[test]
    fn query_extractor_parses_the_query_into_a_map() {
        let req = request("/search?page=2&size=10", vec![]);
        let Query(map) =
            Query::<HashMap<String, String>>::from_request(&req).expect("parses the query");
        assert_eq!(map.get("page").map(String::as_str), Some("2"));
        assert_eq!(map.get("size").map(String::as_str), Some("10"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn query_extractor_with_no_query_is_an_empty_map() {
        let req = request("/search", vec![]);
        let Query(map) =
            Query::<HashMap<String, String>>::from_request(&req).expect("empty query → empty map");
        assert!(map.is_empty());
    }

    #[test]
    fn request_extractor_clones_the_whole_request() {
        let req = request("/p/7?x=1", vec![("id".to_string(), "7".to_string())]);
        let whole = Request::from_request(&req).expect("clones the request");
        assert_eq!(whole.path(), "/p/7");
        assert_eq!(whole.query_str(), Some("x=1"));
        assert_eq!(whole.path_param("id"), Some("7"));
    }

    // ── FromRequestParts (the converter-aware extraction seam, Task 14) ──────────

    use crate::content::HttpMessageConverter;

    /// A tiny `HttpMessageConverter` test double over JSON-ish bytes: it deserializes
    /// the body via a JSON deserializer it owns on the stack (the same `with_deserializer`
    /// lend-a-scoped-deserializer shape the real `leaf-serde` converter has), so the
    /// converter-aware `Json<T>` extraction can be proven IN leaf-web with no leaf-serde.
    struct TestJsonConverter;

    impl HttpMessageConverter for TestJsonConverter {
        fn content_type(&self) -> &str {
            "application/json"
        }
        fn write(&self, _value: &dyn erased_serde::Serialize) -> Result<bytes::Bytes, LeafError> {
            Ok(bytes::Bytes::new())
        }
        fn with_deserializer(
            &self,
            body: &[u8],
            read: &mut dyn FnMut(&mut dyn erased_serde::Deserializer) -> Result<(), LeafError>,
        ) -> Result<(), LeafError> {
            let mut de = serde_json::Deserializer::from_slice(body);
            let mut erased = <dyn erased_serde::Deserializer>::erase(&mut de);
            read(&mut erased)
        }
    }

    fn request_with_body(body: &[u8]) -> Request {
        Request::new(Method::POST, "/orders".parse().expect("uri"), http::HeaderMap::new(), Bytes::copy_from_slice(body))
    }

    #[derive(serde::Deserialize, PartialEq, Debug)]
    struct NewOrder {
        sku: String,
        qty: u32,
    }

    #[test]
    fn json_body_extracts_through_the_converter() {
        // `Json<T>` is the converter-backed extraction: `FromRequestParts` hands the
        // injected converter the body and gets back the typed `T` (NEVER a plain
        // `FromRequest` — leaf-web names no serde FORMAT; the format rides the converter).
        let req = request_with_body(br#"{"sku":"COFFEE","qty":2}"#);
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("body");
        let Json(order) =
            <Json<NewOrder> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
                .expect("the JSON body deserializes through the converter");
        assert_eq!(order, NewOrder { sku: "COFFEE".to_string(), qty: 2 });
    }

    #[test]
    fn json_body_malformed_is_a_loud_error() {
        let req = request_with_body(b"{ not json ");
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("body");
        let err = <Json<NewOrder> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect_err("a malformed JSON body must surface a LeafError, not a default");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn from_request_parts_blanket_falls_back_to_from_request() {
        // Every plain `FromRequest` extractor (Query/&Request) ALSO satisfies the
        // converter-aware `FromRequestParts` via the blanket impl, so the controller
        // codegen calls ONE uniform `from_request_parts(req, converter, ctx)` per parameter
        // (trait dispatch on the parameter's structural extractor type, never a name).
        let req = request("/search?page=2", vec![]);
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("q");
        let Query(map) =
            <Query<HashMap<String, String>> as FromRequestParts>::from_request_parts(
                &req, &conv, &ctx,
            )
            .expect("Query rides the FromRequest blanket through FromRequestParts");
        assert_eq!(map.get("page").map(String::as_str), Some("2"));
    }

    // ── Path<T> reads its OWN capture BY NAME via the ExtractCtx (Task T1a) ───────────

    #[test]
    fn path_binds_each_param_to_its_own_named_capture_not_the_first() {
        // The regression this task fixes: a route `/users/{uid}/orders/{oid}` with two
        // path params must bind EACH to ITS OWN named capture — `uid` is the first, `oid`
        // the second. The old `path_params().first()` impl bound BOTH to the first.
        let req = request(
            "/users/7/orders/42",
            vec![("uid".to_string(), "7".to_string()), ("oid".to_string(), "42".to_string())],
        );
        let conv = TestJsonConverter;

        // `uid: Path<u64>` reads the FIRST capture, parsed as u64.
        let Path(uid) = <Path<u64> as FromRequestParts>::from_request_parts(
            &req,
            &conv,
            &ExtractCtx::named("uid"),
        )
        .expect("uid reads its own named capture");
        assert_eq!(uid, 7u64);

        // `oid: Path<String>` reads the SECOND capture by its own name — NOT the first.
        let Path(oid) = <Path<String> as FromRequestParts>::from_request_parts(
            &req,
            &conv,
            &ExtractCtx::named("oid"),
        )
        .expect("oid reads its own named capture");
        assert_eq!(oid, "42", "the second param must NOT bind to the first capture");
    }

    #[test]
    fn path_parses_a_typed_segment_and_a_bad_parse_is_a_loud_convert_error() {
        // `Path<u32>` parses a numeric segment; a non-numeric segment is a mapped
        // `ConvertError`, NOT a panic.
        let ok = request("/items/123", vec![("id".to_string(), "123".to_string())]);
        let conv = TestJsonConverter;
        let Path(id) = <Path<u32> as FromRequestParts>::from_request_parts(
            &ok,
            &conv,
            &ExtractCtx::named("id"),
        )
        .expect("a numeric segment parses to u32");
        assert_eq!(id, 123u32);

        let bad = request("/items/abc", vec![("id".to_string(), "abc".to_string())]);
        let err = <Path<u32> as FromRequestParts>::from_request_parts(
            &bad,
            &conv,
            &ExtractCtx::named("id"),
        )
        .expect_err("a non-numeric segment must surface a LeafError, not panic");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn path_with_no_matching_capture_is_a_loud_error() {
        // No `{sku}` capture on the request → a loud ConvertError, never a silent default.
        let req = request("/products", vec![]);
        let conv = TestJsonConverter;
        let err = <Path<String> as FromRequestParts>::from_request_parts(
            &req,
            &conv,
            &ExtractCtx::named("sku"),
        )
        .expect_err("a missing named capture must surface a LeafError, not a default");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }
}
