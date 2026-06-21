//! The leaf [`Response`] + the [`IntoResponse`] return-conversion trait.
//! Spring `ServerHttpResponse` / `ResponseEntity`.

use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http::{HeaderMap, StatusCode};
use leaf_core::{BoxStream, LeafError};

use crate::body::{Body, Frame};
use crate::content::HttpMessageConverter;

/// An outbound HTTP response in leaf's backend-free vocabulary: a status, a
/// header map, and a [`Body`] (a buffered [`Body::Full`] or a streamed
/// [`Body::Stream`]) the backend writes at the edge.
// `Response` is intentionally NOT `Clone`/`Debug`: its `Body` may be a one-shot frame
// stream. The error/advice paths build a fresh `Response`, never clone one.
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Body,
}

impl Response {
    /// A response with the given status, no headers, and an empty body.
    #[must_use]
    pub fn new(status: StatusCode) -> Self {
        Response { status, headers: HeaderMap::new(), body: Body::Full(Bytes::new()) }
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

    /// The response body as a byte slice — the buffered bytes of a [`Body::Full`]; empty
    /// for a [`Body::Stream`] (written frame-by-frame at the edge).
    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        match &self.body {
            Body::Full(bytes) => bytes,
            Body::Stream(_) => &[],
        }
    }

    /// Replace the status (builder style).
    #[must_use]
    pub fn with_status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    /// Replace the body with a buffered blob (builder style). Accepts anything that is
    /// `Into<Bytes>` (`Bytes`/`Vec<u8>`/`&[u8]`/`String`); produces a [`Body::Full`].
    #[must_use]
    pub fn with_body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Body::Full(body.into());
        self
    }

    /// Replace the body with a STREAM of [`Frame`]s (builder style) — the streaming
    /// response path (gRPC data frames + a terminating trailers frame, SSE, etc.). The
    /// backend writes these frames at the edge; nothing is buffered.
    #[must_use]
    pub fn with_body_stream(
        mut self,
        stream: BoxStream<'static, Result<Frame, LeafError>>,
    ) -> Self {
        self.body = Body::Stream(stream);
        self
    }

    /// Take the [`Body`] out of the response (the backend edge consumes this to write the
    /// Full bytes or stream the frames).
    #[must_use]
    pub fn into_body(self) -> Body {
        self.body
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

/// A `#[rest_controller]` (`@ResponseBody`) return carrier: a status code, response
/// headers, and a body `T` the converter serializes — Spring's `ResponseEntity<T>`.
///
/// A `#[rest_controller]` handler returns either a bare serializable value (→ `200` with
/// the converter's content-type + serialized body) OR a `ResponseEntity<T>` to set the
/// status / headers explicitly:
///
/// ```ignore
/// ResponseEntity::created()
///     .with_header(http::header::LOCATION, "/products/TEA")
///     .body(dto) // -> 201 Created + Location + the JSON-serialized dto
/// ```
///
/// The codegen lowers EVERY rest-controller return through the uniform
/// [`IntoResponseWith`] trait (`<#ret as IntoResponseWith>::into_response_with(value,
/// converter)`), so the bare-value and carrier paths share one structural call site — no
/// type-name detection.
#[derive(Clone, Debug)]
pub struct ResponseEntity<T> {
    status: StatusCode,
    headers: HeaderMap,
    body: T,
}

impl<T> ResponseEntity<T> {
    /// A `ResponseEntity` with the given status, no headers, and the body `T`.
    pub fn with_status(status: StatusCode, body: T) -> Self {
        ResponseEntity { status, headers: HeaderMap::new(), body }
    }

    /// Append a header (builder style). A header name/value that cannot be parsed is
    /// silently dropped (the same lenient policy as [`Response::with_header`]).
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

impl ResponseEntity<()> {
    /// A status-only builder (`201 Created`, etc.); attach the body with [`body`].
    ///
    /// [`body`]: ResponseEntity::body
    #[must_use]
    pub fn status(status: StatusCode) -> Self {
        ResponseEntity { status, headers: HeaderMap::new(), body: () }
    }

    /// A `201 Created` status-only builder. Attach the body with [`body`] (typically
    /// alongside a `Location` header via [`with_header`]).
    ///
    /// [`body`]: ResponseEntity::body
    /// [`with_header`]: ResponseEntity::with_header
    #[must_use]
    pub fn created() -> Self {
        ResponseEntity::status(StatusCode::CREATED)
    }

    /// Attach the body, keeping the accumulated status + headers — the bridge from a
    /// status-only builder to a `ResponseEntity<T>` the converter serializes.
    #[must_use]
    pub fn body<T>(self, body: T) -> ResponseEntity<T> {
        ResponseEntity { status: self.status, headers: self.headers, body }
    }
}

/// The `#[rest_controller]` (`@ResponseBody`) return policy as a TRAIT the controller
/// codegen drives uniformly: a handler return becomes a [`Response`], serializing its body
/// through the injected [`HttpMessageConverter`]. Dispatch is purely structural — the
/// codegen emits one `<#ret as IntoResponseWith>::into_response_with(value, converter)`
/// call site for EVERY handler, never branching on the return's spelled type name.
///
/// Two impls cover the policy: a blanket impl over any serializable value (→ `200` +
/// the converter's content-type + serialized body) and an impl for [`ResponseEntity<T>`]
/// (→ its status + headers + the serialized body). They do not overlap: `ResponseEntity`
/// deliberately does not implement `erased_serde::Serialize`.
pub trait IntoResponseWith {
    /// Serialize `self` into a [`Response`] via `converter`.
    ///
    /// # Errors
    ///
    /// Returns the converter's [`LeafError`] when the body cannot be serialized.
    fn into_response_with(
        self,
        converter: &dyn HttpMessageConverter,
    ) -> Result<Response, LeafError>;
}

impl<T: erased_serde::Serialize> IntoResponseWith for T {
    /// A bare serializable value → `200 OK` with the converter's content-type + the
    /// serialized body.
    fn into_response_with(
        self,
        converter: &dyn HttpMessageConverter,
    ) -> Result<Response, LeafError> {
        let body = converter.write(&self)?;
        Ok(Response::ok()
            .with_header(http::header::CONTENT_TYPE, converter.content_type())
            .with_body(body))
    }
}

impl<T: erased_serde::Serialize> IntoResponseWith for ResponseEntity<T> {
    /// A carrier → its status + headers + the converter-serialized body (the converter's
    /// content-type is added unless the carrier already set one).
    fn into_response_with(
        self,
        converter: &dyn HttpMessageConverter,
    ) -> Result<Response, LeafError> {
        let serialized = converter.write(&self.body)?;
        let mut resp = Response::new(self.status).with_body(serialized);
        for (name, value) in &self.headers {
            resp = resp.with_header(name.clone(), value.clone());
        }
        // Set the converter's content-type only if the carrier did not pin one itself.
        if !self.headers.contains_key(http::header::CONTENT_TYPE) {
            resp = resp.with_header(http::header::CONTENT_TYPE, converter.content_type());
        }
        Ok(resp)
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
        let out = resp.into_response();
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

    // ── ResponseEntity + IntoResponseWith (the @ResponseBody return policy, T3b) ──

    use crate::content::HttpMessageConverter;
    use bytes::Bytes as _Bytes;
    use leaf_core::LeafError;

    /// A trivial converter that "serializes" by recording the call and returning a fixed
    /// body — enough to prove the `IntoResponseWith` trait drives the converter without a
    /// serde dependency in leaf-web's own tests.
    struct FixedConverter;

    impl HttpMessageConverter for FixedConverter {
        fn content_type(&self) -> &str {
            "application/test"
        }
        fn write(&self, _value: &dyn erased_serde::Serialize) -> Result<_Bytes, LeafError> {
            Ok(_Bytes::from_static(b"BODY"))
        }
        fn with_deserializer(
            &self,
            _body: &[u8],
            _read: &mut dyn FnMut(&mut dyn erased_serde::Deserializer) -> Result<(), LeafError>,
        ) -> Result<(), LeafError> {
            Ok(())
        }
    }

    #[test]
    fn a_bare_serializable_value_is_200_serialized_via_the_converter() {
        // The plain-value path: a serializable return → 200 with the converter's
        // content-type + the serialized body. Trait dispatch drives the converter.
        let resp = IntoResponseWith::into_response_with(42u32, &FixedConverter)
            .expect("serializes");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body_bytes(), b"BODY".as_slice());
        assert_eq!(
            resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/test"),
        );
    }

    #[test]
    fn a_response_entity_carries_its_status_headers_and_serialized_body() {
        // The carrier path: ResponseEntity::created().with_header(LOCATION, ..).body(dto)
        // → 201 + the Location header + the converter-serialized body (still with the
        // converter's content-type). Same uniform `into_response_with` entry point.
        let entity = ResponseEntity::created()
            .with_header(http::header::LOCATION, "/products/TEA")
            .body(7u32);
        let resp = IntoResponseWith::into_response_with(entity, &FixedConverter)
            .expect("serializes");
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(resp.body_bytes(), b"BODY".as_slice());
        assert_eq!(
            resp.headers().get(http::header::LOCATION).and_then(|v| v.to_str().ok()),
            Some("/products/TEA"),
        );
        assert_eq!(
            resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()),
            Some("application/test"),
            "the converter's content-type is still set on a ResponseEntity body",
        );
    }

    #[test]
    fn with_body_accepts_anything_into_bytes_and_is_full() {
        use crate::body::Body;
        // A &'static [u8], a Vec<u8>, and Bytes all satisfy `impl Into<Bytes>`.
        let resp = Response::ok().with_body(b"hello".as_slice());
        assert_eq!(resp.body_bytes(), b"hello".as_slice());
        match resp.into_body() {
            Body::Full(b) => assert_eq!(b, Bytes::from_static(b"hello")),
            Body::Stream(_) => panic!("with_body must produce Body::Full"),
        }
    }

    #[test]
    fn with_body_stream_is_a_stream_with_empty_body_bytes() {
        use crate::body::{Body, Frame};
        let frames = futures::stream::iter(vec![
            Ok(Frame::Data(Bytes::from_static(b"chunk"))),
        ]);
        let resp = Response::ok().with_body_stream(Box::pin(frames));
        // A streamed response reports empty buffered bytes (it is written frame-by-frame).
        assert_eq!(resp.body_bytes(), b"".as_slice());
        assert!(matches!(resp.into_body(), Body::Stream(_)));
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
