//! The leaf [`Request`] — method / uri / headers / path-params / body, wrapping
//! the neutral `http` primitives. Spring `ServerHttpRequest`/`HttpServletRequest`.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, Method, Uri};

use crate::body::Body;

/// An inbound HTTP request, in leaf's backend-free vocabulary.
///
/// The backend (`leaf-web-hyper` / a mock) builds one of these at the edge: it
/// wraps the neutral `http` value types ([`Method`]/[`Uri`]/[`HeaderMap`]) plus a
/// [`Body`] (a buffered [`Body::Full`] or a streamed [`Body::Stream`]).
/// `path_params` start empty and are filled by the route matcher (Task 2) once a
/// pattern like `/products/{sku}` captures a concrete segment.
// `Request` is intentionally NOT `Clone`/`Debug`: its `Body` may be a one-shot frame
// stream (a `BoxStream` is neither). The advice error path clones the request's PARTS
// (method/uri/headers/path_params) it needs instead of the whole request.
pub struct Request {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    /// `(name, value)` captures filled by the matcher; empty until then.
    path_params: Vec<(String, String)>,
    /// The request body — buffered ([`Body::Full`]) or streamed ([`Body::Stream`]). The
    /// [`Dispatcher`](crate::Dispatcher) collects a streamed HTTP body to Full BEFORE a
    /// route handler runs, so every extractor / [`body_bytes`](Request::body_bytes) call
    /// sees buffered bytes; a gRPC handler reads the stream directly.
    body: Body,
    /// Type-keyed per-request attributes (Spring's `ServletRequest` attributes /
    /// `http::Extensions`). A [`WebFilter`](crate::WebFilter) attaches a TYPED value
    /// (e.g. a security filter's authenticated `Principal`) here, and a downstream
    /// handler reads it back through the `Extension<T>` extractor. Stored as
    /// `Arc<dyn Any + Send + Sync>` (not `Box`) so the request's PARTS stay cheaply
    /// cloneable — the dispatcher snapshots the parts (via [`parts_clone`](Request::parts_clone))
    /// for the error path, and `Box<dyn Any>` is not `Clone`. This is a PURE leaf
    /// abstraction over std [`Any`]; it names no backend.
    extensions: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl Request {
    /// Build a request from its parts (the backend / a test constructs this).
    ///
    /// `path_params` start empty — the route matcher fills them once a pattern
    /// captures the concrete path segments.
    #[must_use]
    pub fn new(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Self {
        Request {
            method,
            uri,
            headers,
            path_params: Vec::new(),
            body: Body::Full(body),
            extensions: HashMap::new(),
        }
    }

    /// The request [`Method`] (`GET`/`POST`/…).
    #[must_use]
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The full request [`Uri`] (path + optional query).
    #[must_use]
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The path portion of the URI, e.g. `/p/7` for `/p/7?x=1`.
    #[must_use]
    pub fn path(&self) -> &str {
        self.uri.path()
    }

    /// The raw query string (no leading `?`), or `None` if there is none.
    #[must_use]
    pub fn query_str(&self) -> Option<&str> {
        self.uri.query()
    }

    /// The first value of a header by (case-insensitive) name, as a `str`.
    ///
    /// Returns `None` if the header is absent or its value is not valid UTF-8.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The full header map (multi-value / non-UTF-8 access).
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The request body as a byte slice — the buffered bytes of a [`Body::Full`].
    ///
    /// A [`Body::Stream`] reports empty here (it is consumed frame-by-frame, not buffered):
    /// HTTP handlers only ever see a body the [`Dispatcher`](crate::Dispatcher) already
    /// collected to Full, and gRPC handlers read [`into_body`](Request::into_body) directly.
    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        match &self.body {
            Body::Full(bytes) => bytes,
            Body::Stream(_) => &[],
        }
    }

    /// Take the [`Body`] out of the request (a gRPC handler consumes the frame stream this
    /// way; the dispatcher takes it to collect a streamed HTTP body before a handler).
    #[must_use]
    pub fn into_body(self) -> Body {
        self.body
    }

    /// Replace the body (the backend edge installs a [`Body::Stream`]; the dispatcher swaps
    /// in the collected [`Body::Full`] before an HTTP handler runs).
    pub fn set_body(&mut self, body: Body) {
        self.body = body;
    }

    /// Whether the body is the streaming variant (the dispatcher collects it before an
    /// HTTP handler runs).
    #[must_use]
    pub fn body_is_stream(&self) -> bool {
        self.body.is_stream()
    }

    /// Take the body OUT, leaving an empty Full body in its place (the dispatcher collects
    /// the taken stream, then installs the collected Full via [`set_body`](Request::set_body)).
    #[must_use]
    pub fn take_body(&mut self) -> Body {
        std::mem::replace(&mut self.body, Body::Full(Bytes::new()))
    }

    /// A body-less copy of the request's PARTS (method/uri/headers/path_params + extensions)
    /// — what the advice error path consumes (`Request` is not `Clone` because its body may
    /// be a one-shot stream, but the parts ARE cheaply cloneable).
    #[must_use]
    pub fn parts_clone(&self) -> Request {
        Request {
            method: self.method.clone(),
            uri: self.uri.clone(),
            headers: self.headers.clone(),
            path_params: self.path_params.clone(),
            body: Body::Full(Bytes::new()),
            extensions: self.extensions.clone(),
        }
    }

    /// A copy of the request carrying the request's PARTS plus its BUFFERED body bytes — the
    /// `&Request` extractor's source. By the time an HTTP handler runs, the
    /// [`Dispatcher`](crate::Dispatcher) has already collected a streamed body to
    /// [`Body::Full`], so this preserves the body (a still-streaming body, which a handler
    /// never sees, is copied as the empty buffered bytes [`body_bytes`](Request::body_bytes)
    /// reports). `Request` is not `Clone` (its `Body` may be a one-shot stream), so this is
    /// the explicit buffered-copy the whole-request extractor uses.
    #[must_use]
    pub fn buffered_clone(&self) -> Request {
        Request {
            method: self.method.clone(),
            uri: self.uri.clone(),
            headers: self.headers.clone(),
            path_params: self.path_params.clone(),
            body: Body::Full(Bytes::copy_from_slice(self.body_bytes())),
            extensions: self.extensions.clone(),
        }
    }

    /// A captured path parameter by name (e.g. `sku` from `/products/{sku}`),
    /// or `None` if no such capture exists.
    #[must_use]
    pub fn path_param(&self, name: &str) -> Option<&str> {
        self.path_params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// All captured path parameters as `(name, value)` pairs.
    #[must_use]
    pub fn path_params(&self) -> &[(String, String)] {
        &self.path_params
    }

    /// Install the captured path parameters (the route matcher calls this once a
    /// pattern matches; Task 2).
    pub fn set_path_params(&mut self, params: Vec<(String, String)>) {
        self.path_params = params;
    }

    /// The header map, MUTABLY — a [`WebFilter`](crate::WebFilter) that owns the
    /// request may add/strip headers before threading it downstream (e.g. a tracing
    /// filter stamping a correlation id).
    #[must_use]
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    /// Attach a TYPED per-request attribute, keyed by its type, returning the previous
    /// value of the SAME type if one was already present (Spring's request-attribute /
    /// `http::Extensions::insert`). A [`WebFilter`](crate::WebFilter) that owns the
    /// request inserts here — e.g. a security filter authenticates and attaches a typed
    /// `Principal` — and a downstream handler reads it back through the `Extension<T>`
    /// extractor. Dispatch is purely by TYPE, never a textual name.
    pub fn insert_extension<T: Any + Send + Sync + 'static>(&mut self, value: T) -> Option<T> {
        self.extensions
            .insert(TypeId::of::<T>(), Arc::new(value))
            // The slot for `TypeId::of::<T>()` only ever holds an `Arc<T>`, so the
            // downcast cannot fail; `Arc::try_unwrap` recovers the owned `T` when this
            // is the sole handle (the common case right after a fresh insert).
            .and_then(|prev| prev.downcast::<T>().ok())
            .and_then(|arc| Arc::try_unwrap(arc).ok())
    }

    /// Read a TYPED per-request attribute by its type, or `None` if none was attached
    /// (the `Extension<T>` extractor's source). Resolves purely by TYPE.
    #[must_use]
    pub fn extension<T: Any + Send + Sync + 'static>(&self) -> Option<&T> {
        self.extensions
            .get(&TypeId::of::<T>())
            // The slot for `TypeId::of::<T>()` only ever holds an `Arc<T>`.
            .and_then(|value| value.downcast_ref::<T>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::{HeaderValue, Method};

    #[test]
    fn request_exposes_method_path_query_header_and_body() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-trace", HeaderValue::from_static("abc"));
        let req = Request::new(
            Method::GET,
            "/p/7?x=1".parse().expect("uri parses"),
            headers,
            Bytes::from_static(b"hello"),
        );

        assert_eq!(req.method(), &Method::GET);
        assert_eq!(req.path(), "/p/7");
        assert_eq!(req.query_str(), Some("x=1"));
        assert_eq!(req.header("x-trace"), Some("abc"));
        assert_eq!(req.header("missing"), None);
        assert_eq!(req.body_bytes(), b"hello".as_slice());
        // No path params until the matcher (Task 2) fills them.
        assert_eq!(req.path_param("sku"), None);
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Principal {
        user: String,
    }

    #[test]
    fn typed_extension_round_trips_by_type() {
        let mut req = Request::new(Method::GET, "/x".parse().expect("uri"), HeaderMap::new(), Bytes::new());
        // Absent until inserted.
        assert!(req.extension::<Principal>().is_none());

        let prev = req.insert_extension(Principal { user: "alice".to_string() });
        assert!(prev.is_none(), "no prior value of this type");
        assert_eq!(req.extension::<Principal>(), Some(&Principal { user: "alice".to_string() }));

        // Re-inserting the SAME type returns the previous value (keyed by type).
        let prev = req.insert_extension(Principal { user: "bob".to_string() });
        assert_eq!(prev, Some(Principal { user: "alice".to_string() }));
        assert_eq!(req.extension::<Principal>(), Some(&Principal { user: "bob".to_string() }));
    }

    #[test]
    fn parts_clone_carries_extensions_without_a_body() {
        // The dispatcher snapshots the request's PARTS for the advice error path (Request is
        // no longer Clone — its Body may be a one-shot stream). The Arc-backed extensions
        // survive the parts copy.
        let mut req = Request::new(Method::GET, "/x".parse().expect("uri"), HeaderMap::new(), Bytes::new());
        req.insert_extension(Principal { user: "alice".to_string() });
        let parts = req.parts_clone();
        assert_eq!(parts.extension::<Principal>(), Some(&Principal { user: "alice".to_string() }));
    }

    #[test]
    fn new_wraps_bytes_as_a_full_body_and_into_body_yields_it() {
        use crate::body::Body;
        let req = Request::new(
            Method::POST,
            "/x".parse().expect("uri"),
            HeaderMap::new(),
            Bytes::from_static(b"payload"),
        );
        // body_bytes() still reads the Full variant, unchanged.
        assert_eq!(req.body_bytes(), b"payload".as_slice());
        // into_body() hands out the Body; it is the Full variant.
        match req.into_body() {
            Body::Full(b) => assert_eq!(b, Bytes::from_static(b"payload")),
            Body::Stream(_) => panic!("Request::new must wrap Bytes as Body::Full"),
        }
    }

    #[test]
    fn body_bytes_is_empty_for_a_stream_body() {
        use crate::body::{Body, Frame};
        let stream = futures::stream::iter(vec![Ok(Frame::Data(Bytes::from_static(b"abc")))]);
        let mut req = Request::new(Method::POST, "/x".parse().expect("uri"), HeaderMap::new(), Bytes::new());
        req.set_body(Body::Stream(Box::pin(stream)));
        // A streamed body has nothing buffered yet, so body_bytes() reports empty (the
        // dispatcher collects a streamed HTTP body BEFORE a handler sees it; gRPC reads the
        // stream directly). It must NOT panic.
        assert_eq!(req.body_bytes(), b"".as_slice());
    }
}
