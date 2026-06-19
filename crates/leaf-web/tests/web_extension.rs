//! Integration test `[web-extension]` — the Task-T5 request-extensions proof:
//! a `WebFilter` attaches a TYPED per-request attribute (`Principal`) onto the
//! owned [`Request`] it threads through `next.run(req)`, and a downstream terminal
//! reads it back via the `Extension<T>` extractor — end to end, through the
//! [`Dispatcher`]. This is the seam leaf-security's auth `WebFilter` needs: a
//! filter authenticates and hands the handler a typed `Principal`.

use std::sync::Arc;

use bytes::Bytes;
use http::{Method, StatusCode};
use leaf_core::{BoxFuture, LeafError};
use leaf_web::extract::{ExtractCtx, FromRequestParts};
use leaf_web::testing::MockServer;
use leaf_web::{
    Dispatcher, Extension, Handler, Next, Request, Response, Route, WebFilter,
};

/// The typed per-request attribute a security filter authenticates and hands down.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Principal {
    user: String,
}

/// A filter that "authenticates" by inserting a typed `Principal` extension onto the
/// request, then continues the chain — exactly the leaf-security `WebFilter` shape.
struct AuthFilter;

#[leaf_macros::async_impl]
impl WebFilter for AuthFilter {
    async fn filter(&self, mut req: Request, next: Next<'_>) -> Result<Response, LeafError> {
        req.insert_extension(Principal { user: "alice".to_string() });
        next.run(req).await
    }
}

/// A handler that reads the `Extension<Principal>` the filter attached and echoes the
/// user name as the body — proving the typed attribute flowed end-to-end.
struct WhoAmIHandler;

impl Handler for WhoAmIHandler {
    fn handle<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, LeafError>> {
        Box::pin(async move {
            let Extension(principal) = req.extension::<Principal>().cloned().map(Extension).ok_or_else(
                || LeafError::new(leaf_core::ErrorKind::ConvertError),
            )?;
            Ok(Response::ok().with_body(Bytes::from(principal.user)))
        })
    }
}

struct WhoAmIRoute;
impl Route for WhoAmIRoute {
    fn method(&self) -> Method {
        Method::GET
    }
    fn path(&self) -> &str {
        "/whoami"
    }
    fn handler(&self) -> &dyn Handler {
        &WhoAmIHandler
    }
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

fn get(path: &str) -> Request {
    Request::new(Method::GET, path.parse().expect("uri"), http::HeaderMap::new(), Bytes::new())
}

#[test]
fn a_filter_attaches_a_typed_extension_a_handler_reads_end_to_end() {
    let route: Arc<dyn Route> = Arc::new(WhoAmIRoute);
    let filter: Arc<dyn WebFilter> = Arc::new(AuthFilter);
    let dispatcher = Dispatcher::new(vec![route], vec![filter], vec![]);
    let server = MockServer::new(Arc::new(dispatcher));

    let resp = block(server.handle(get("/whoami")));
    assert_eq!(resp.status(), StatusCode::OK);
    // The filter's typed `Principal` reached the handler through the request.
    assert_eq!(resp.body_bytes(), b"alice".as_slice());
}

#[test]
fn the_extension_extractor_reads_the_typed_attribute() {
    // The `Extension<T>` extractor reads the typed attribute off the request through the
    // uniform `FromRequestParts` seam — no name needed, dispatch is purely structural.
    let mut req = get("/x");
    req.insert_extension(Principal { user: "bob".to_string() });

    let conv = TestConverter;
    let ctx = ExtractCtx::empty();
    let Extension(p) = <Extension<Principal> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
        .expect("the extension extractor reads the typed attribute");
    assert_eq!(p, Principal { user: "bob".to_string() });
}

#[test]
fn a_missing_extension_is_a_loud_convert_error_not_a_panic() {
    // Extracting `Extension<T>` when no such extension was set is a loud ConvertError
    // (the dispatcher maps it to 400), never a panic, never a silent default.
    let req = get("/x");
    let conv = TestConverter;
    let ctx = ExtractCtx::empty();
    let err = <Extension<Principal> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
        .expect_err("a missing extension must surface a LeafError, not a default");
    assert_eq!(err.kind, leaf_core::ErrorKind::ConvertError);
}

/// A no-op converter: the `Extension<T>` extraction never touches it (it reads the
/// typed attribute straight off the request), but the seam takes one.
struct TestConverter;
impl leaf_web::HttpMessageConverter for TestConverter {
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
