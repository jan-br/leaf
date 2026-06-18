//! The leaf [`Request`] — method / uri / headers / path-params / body, wrapping
//! the neutral `http` primitives. Spring `ServerHttpRequest`/`HttpServletRequest`.

use bytes::Bytes;
use http::{HeaderMap, Method, Uri};

/// An inbound HTTP request, in leaf's backend-free vocabulary.
///
/// The backend (`leaf-web-hyper` / a mock) builds one of these at the edge: it
/// wraps the neutral `http` value types ([`Method`]/[`Uri`]/[`HeaderMap`]) plus a
/// [`Bytes`] body it has already collected. `path_params` start empty and are
/// filled by the route matcher (Task 2) once a pattern like `/products/{sku}`
/// captures a concrete segment.
#[derive(Clone, Debug)]
pub struct Request {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    /// `(name, value)` captures filled by the matcher; empty until then.
    path_params: Vec<(String, String)>,
    body: Bytes,
}

impl Request {
    /// Build a request from its parts (the backend / a test constructs this).
    ///
    /// `path_params` start empty — the route matcher fills them once a pattern
    /// captures the concrete path segments.
    #[must_use]
    pub fn new(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Self {
        Request { method, uri, headers, path_params: Vec::new(), body }
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

    /// The request body as a byte slice.
    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        &self.body
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
}
