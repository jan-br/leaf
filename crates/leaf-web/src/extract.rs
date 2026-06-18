//! The [`FromRequest`] argument-extraction seam + the typed extractor wrappers
//! ([`Path`] / [`Query`] / [`Json`] / [`Header`] / [`State`]). Spring's
//! `HandlerMethodArgumentResolver` family, expressed in leaf's vocabulary.
//!
//! A controller method's parameters each resolve from the inbound [`Request`] via
//! their STRUCTURAL extractor type — the controller codegen (Task 9) dispatches on
//! the parameter's shape (`Path<_>` / `Query<_>` / `Json<_>` / `State<_>` /
//! `&Request`), NEVER on a spelled type name, consistent with the no-type-names
//! rule. The two extractions that are unambiguous from the request alone get their
//! [`FromRequest`] impls here:
//!
//! - [`Path<String>`] — the sole captured path parameter (e.g. `sku` from
//!   `/products/{sku}`).
//! - [`Query`]`<`[`HashMap`]`<String, String>>` — the raw query string parsed into
//!   a name→value map.
//! - [`Request`] — the whole request, cloned (the `&Request` extractor).
//!
//! The extractions that need a serde data format ([`Json<T>`] body
//! deserialization, [`Query<T>`] for an arbitrary `Deserialize` target) or the DI
//! container ([`State<T>`], a collaborator bean) are NOT plain `from_request(req)`
//! impls — they need a converter / the handler's captured `ResolveCtx` that only
//! the controller codegen (Task 9) has in scope. leaf-web defines their wrapper
//! types here (so the codegen can dispatch on them structurally) and documents
//! the seam; the serde-backed reads ride the injected
//! [`HttpMessageConverter`](crate::HttpMessageConverter) (Task 5) — leaf-web names
//! no serde data format itself.

use std::collections::HashMap;

use leaf_core::error::{Cause, ErrorKind, LeafError};

use crate::Request;

/// Resolve `Self` from an inbound [`Request`] (Spring's
/// `HandlerMethodArgumentResolver`). Each controller-method parameter is one of
/// these; the controller codegen (Task 9) calls
/// `<Param as FromRequest>::from_request(req)` per argument, dispatching on the
/// parameter's structural extractor type — never a spelled type name.
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

/// A path-parameter extractor: `T` is read from a captured `{name}` segment (e.g.
/// `sku` from `/products/{sku}`). Spring's `@PathVariable`.
///
/// The [`FromRequest`] impl here covers the single-capture `Path<String>` case
/// (the common controller shape). A multi-capture / typed `Path<(A, B)>` or
/// `Path<Struct>` is a serde-backed follow-up the controller codegen drives
/// through the converter (Task 9), not a plain `from_request` impl.
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
/// `Json<T>` has NO plain [`FromRequest`] impl: deserializing a body into a
/// `T: DeserializeOwned` needs a serde data format, which leaf-web deliberately
/// does not depend on. The controller codegen (Task 9) resolves a `Json<T>`
/// parameter through the injected
/// [`HttpMessageConverter`](crate::HttpMessageConverter) (the JSON impl is a
/// `#[component]` bean in `leaf-serde`) — this wrapper exists so the codegen can
/// dispatch on its structure.
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

impl FromRequest for Path<String> {
    /// Read the sole captured path parameter. Loud [`LeafError`] if the request
    /// carries no path capture (the route pattern declared none, or the matcher
    /// did not run).
    fn from_request(req: &Request) -> Result<Self, LeafError> {
        match req.path_params().first() {
            Some((_, value)) => Ok(Path(value.clone())),
            None => Err(missing("path parameter", "no captured path parameter on the request")),
        }
    }
}

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
    fn path_extractor_reads_the_captured_param() {
        let req = request("/products/COFFEE", vec![("sku".to_string(), "COFFEE".to_string())]);
        let Path(sku) = Path::<String>::from_request(&req).expect("reads the path param");
        assert_eq!(sku, "COFFEE");
    }

    #[test]
    fn path_extractor_with_no_capture_is_a_loud_error() {
        let req = request("/products", vec![]);
        let err = Path::<String>::from_request(&req)
            .expect_err("a missing path param must surface a LeafError, not a default");
        assert_eq!(err.kind, ErrorKind::ConvertError);
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
}
