//! The leaf [`Response`] + the [`IntoResponse`] return-conversion trait.
//! Spring `ServerHttpResponse` / `ResponseEntity`.

use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http::{HeaderMap, StatusCode};

/// An outbound HTTP response in leaf's backend-free vocabulary: a status, a
/// header map, and a [`Bytes`] body the backend writes at the edge.
#[derive(Clone, Debug)]
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Response {
    /// A response with the given status, no headers, and an empty body.
    #[must_use]
    pub fn new(status: StatusCode) -> Self {
        Response { status, headers: HeaderMap::new(), body: Bytes::new() }
    }

    /// A `200 OK` response with no headers and an empty body.
    #[must_use]
    pub fn ok() -> Self {
        Response::new(StatusCode::OK)
    }

    /// The response status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The response header map.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The response body as a byte slice.
    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        &self.body
    }

    /// Replace the status (builder style).
    #[must_use]
    pub fn with_status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    /// Replace the body (builder style).
    #[must_use]
    pub fn with_body(mut self, body: Bytes) -> Self {
        self.body = body;
        self
    }

    /// Append a header (builder style).
    ///
    /// Lenient: a header name/value that cannot be parsed is silently dropped —
    /// the response stays well-formed rather than panicking at the edge. The
    /// typed converters / codegen pass already-valid header constants.
    #[must_use]
    pub fn with_header<K, V>(mut self, name: K, value: V) -> Self
    where
        K: TryInto<HeaderName>,
        V: TryInto<HeaderValue>,
    {
        if let (Ok(n), Ok(v)) = (name.try_into(), value.try_into()) {
            self.headers.insert(n, v);
        }
        self
    }
}

/// Any handler return that can become a [`Response`] (Spring `ResponseEntity`
/// adaptation). The controller codegen calls this for `#[controller]` handlers;
/// `#[rest_controller]` serializes via an `HttpMessageConverter` instead.
pub trait IntoResponse {
    /// Convert `self` into a [`Response`].
    fn into_response(self) -> Response;
}

impl IntoResponse for Response {
    fn into_response(self) -> Response {
        self
    }
}

impl IntoResponse for StatusCode {
    /// The status with an empty body.
    fn into_response(self) -> Response {
        Response::new(self)
    }
}

impl IntoResponse for String {
    /// `200 OK` with the string as a UTF-8 body.
    fn into_response(self) -> Response {
        Response::ok().with_body(Bytes::from(self.into_bytes()))
    }
}

impl IntoResponse for &str {
    /// `200 OK` with the string as a UTF-8 body.
    fn into_response(self) -> Response {
        self.to_owned().into_response()
    }
}

impl IntoResponse for () {
    /// `204 No Content` with an empty body.
    fn into_response(self) -> Response {
        Response::new(StatusCode::NO_CONTENT)
    }
}

impl<T: IntoResponse, E: IntoResponse> IntoResponse for Result<T, E> {
    /// `Ok` → the success arm's response; `Err` → the error arm's response.
    fn into_response(self) -> Response {
        match self {
            Ok(t) => t.into_response(),
            Err(e) => e.into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::CONTENT_TYPE;

    #[test]
    fn response_builders_round_trip() {
        let resp = Response::ok()
            .with_header(CONTENT_TYPE, "text/plain")
            .with_body(Bytes::from_static(b"hello"));

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"hello".as_slice());
        assert_eq!(
            resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("text/plain"),
        );
    }

    #[test]
    fn into_response_for_response_is_identity() {
        let resp = Response::new(StatusCode::CREATED).with_body(Bytes::from_static(b"x"));
        let out = resp.clone().into_response();
        assert_eq!(out.status(), StatusCode::CREATED);
        assert_eq!(out.body_bytes(), b"x".as_slice());
    }

    #[test]
    fn into_response_for_status_code_is_empty_body() {
        let out = StatusCode::NOT_FOUND.into_response();
        assert_eq!(out.status(), StatusCode::NOT_FOUND);
        assert!(out.body_bytes().is_empty());
    }

    #[test]
    fn into_response_for_str_and_string_is_200_text() {
        let from_str = "hi there".into_response();
        assert_eq!(from_str.status(), StatusCode::OK);
        assert_eq!(from_str.body_bytes(), b"hi there".as_slice());

        let from_string = String::from("owned").into_response();
        assert_eq!(from_string.status(), StatusCode::OK);
        assert_eq!(from_string.body_bytes(), b"owned".as_slice());
    }

    #[test]
    fn into_response_for_unit_is_204_empty() {
        let out = ().into_response();
        assert_eq!(out.status(), StatusCode::NO_CONTENT);
        assert!(out.body_bytes().is_empty());
    }

    #[test]
    fn into_response_for_result_picks_ok_or_err_arm() {
        let ok: Result<&str, StatusCode> = Ok("yes");
        let ok_resp = ok.into_response();
        assert_eq!(ok_resp.status(), StatusCode::OK);
        assert_eq!(ok_resp.body_bytes(), b"yes".as_slice());

        let err: Result<&str, StatusCode> = Err(StatusCode::BAD_REQUEST);
        let err_resp = err.into_response();
        assert_eq!(err_resp.status(), StatusCode::BAD_REQUEST);
        assert!(err_resp.body_bytes().is_empty());
    }
}
