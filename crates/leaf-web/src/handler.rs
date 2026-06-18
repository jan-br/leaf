//! The dispatch unit ([`Handler`]) + the routing registration ([`Route`]) + the
//! routing table ([`RouteTable`]). Spring's `HandlerAdapter`-invoked handler +
//! `RequestMappingHandlerMapping`.

use http::Method;
use leaf_core::{BoxFuture, LeafError};

use crate::{Request, Response};

/// The dispatch unit: an async `handle` that turns a [`Request`] into a
/// [`Response`] (or a [`LeafError`] the dispatcher maps via the advice chain).
///
/// `Handler` is dyn-dispatched and async, so it returns a [`BoxFuture`] at the
/// `dyn` seam (AFIT/RPITIT are not `dyn`-compatible — leaf's uniform answer).
/// It is WRITTEN BY the controller macro (Task 9): each mapped controller method
/// lowers to one `Handler` whose body resolves arguments, invokes the method,
/// and applies the return policy. Users never hand-impl it (the `#[cfg(test)]`
/// fakes here are the lone Stage-1 exception).
pub trait Handler: Send + Sync {
    /// Handle `req`, yielding a [`Response`] or a [`LeafError`].
    fn handle<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, LeafError>>;
}

/// A routing registration: a `(method, path-pattern) -> Handler` row the server
/// collects from the container (`Vec<Ref<dyn Route>>`, collection injection).
///
/// `path()` is a PATTERN that may contain `{name}` capture segments (e.g.
/// `/products/{sku}`); [`RouteTable`] matches a concrete path against it and
/// extracts the captures. Each route comes from the controller macro (Task 9) —
/// a `#[component]`-equivalent bean providing the `dyn Route` view — never a
/// hand-written impl in production.
pub trait Route: Send + Sync {
    /// The HTTP method this route answers.
    fn method(&self) -> Method;
    /// The path PATTERN, e.g. `/products/{sku}` (`{name}` = a capture segment).
    fn path(&self) -> &str;
    /// The [`Handler`] that runs when this route matches.
    fn handler(&self) -> &dyn Handler;
}

// Make `dyn Route` an injectable VIEW (the by-trait-injection seam, emitted ONCE —
// orphan-rule-OK since `dyn Route` is local to this crate). A controller-macro bean
// (Stage 2) publishes the `dyn Route` view; the server collects EVERY provider as
// `Vec<Ref<dyn Route>>` (collection injection) to build its routing table. This is
// the hand-written equivalent of `#[injectable]` for a framework concern trait —
// the same shape `leaf-core` uses for its own `dyn CacheManager`/`dyn TransactionManager`.
leaf_core::impl_resolve_view!(dyn Route);

/// The captured `(name, value)` path parameters a pattern extracts from a
/// concrete path (e.g. `[("sku", "COFFEE")]` for `/products/{sku}` vs
/// `/products/COFFEE`); empty for an all-literal route.
pub type PathParams = Vec<(String, String)>;

/// A successful [`RouteTable::match_route`]: the matched [`Route`] plus the
/// [`PathParams`] its pattern captured.
pub type RouteMatch<'r> = (&'r dyn Route, PathParams);

/// One compiled path pattern: the `/`-split segments, each either a literal or a
/// `{name}` capture. Built once per [`Route`] at table-build time so matching is
/// a per-segment walk (no per-request re-parse).
struct PatternSegment {
    /// `Some(name)` for a `{name}` capture segment; `None` for a literal.
    capture: Option<String>,
    /// The literal text (the segment itself for a literal; unused for a capture).
    literal: String,
}

/// One table entry: a route index paired with its compiled pattern segments.
struct CompiledRoute<'r> {
    route: &'r dyn Route,
    method: Method,
    segments: Vec<PatternSegment>,
}

/// The routing table: the compiled set of [`Route`]s the server matches each
/// request against. Built from the container-collected routes; exposes only leaf
/// types (no backend matcher leaks through). Matching is structural — a literal
/// segment must equal the concrete segment; a `{name}` segment captures it.
pub struct RouteTable<'r> {
    routes: Vec<CompiledRoute<'r>>,
}

impl<'r> RouteTable<'r> {
    /// Build a table over the given routes (compiling each path PATTERN into its
    /// segments once).
    #[must_use]
    pub fn build(routes: &[&'r dyn Route]) -> Self {
        let routes = routes
            .iter()
            .map(|&route| CompiledRoute {
                route,
                method: route.method(),
                segments: compile_pattern(route.path()),
            })
            .collect();
        RouteTable { routes }
    }

    /// Match `(method, path)` against the table.
    ///
    /// Returns the matching [`Route`] plus the captured `(name, value)` path
    /// params (empty for an all-literal route), or `None` when no route matches
    /// the path with that method. A path that matches a pattern but with a
    /// different method does not match (no method fall-through here).
    #[must_use]
    pub fn match_route(&self, method: &Method, path: &str) -> Option<RouteMatch<'r>> {
        let segments: Vec<&str> = split_path(path);
        for entry in &self.routes {
            if entry.method != *method {
                continue;
            }
            if let Some(params) = match_segments(&entry.segments, &segments) {
                return Some((entry.route, params));
            }
        }
        None
    }
}

/// Split a path into its non-empty `/`-delimited segments (so `/` and `` both
/// yield no segments, and a trailing slash does not add an empty segment).
fn split_path(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Compile a path PATTERN into its segments: a `{name}` segment becomes a
/// capture, anything else a literal.
fn compile_pattern(pattern: &str) -> Vec<PatternSegment> {
    split_path(pattern)
        .into_iter()
        .map(|seg| {
            if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                PatternSegment { capture: Some(name.to_string()), literal: String::new() }
            } else {
                PatternSegment { capture: None, literal: seg.to_string() }
            }
        })
        .collect()
}

/// Match compiled pattern segments against concrete path segments, returning the
/// captured params on success (a literal must be equal; a capture binds the
/// concrete value) or `None` on a length / literal mismatch.
fn match_segments(pattern: &[PatternSegment], concrete: &[&str]) -> Option<PathParams> {
    if pattern.len() != concrete.len() {
        return None;
    }
    let mut params = Vec::new();
    for (pat, &seg) in pattern.iter().zip(concrete.iter()) {
        match &pat.capture {
            Some(name) => params.push((name.clone(), seg.to_string())),
            None if pat.literal == seg => {}
            None => return None,
        }
    }
    Some(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Request;
    use http::Method;
    use leaf_core::BoxFuture;

    /// A `#[cfg(test)]` fake `Route` (a hand-written impl is the ONE legitimate
    /// kind in Stage 1 — PRODUCTION `Route`s come from the controller macro in
    /// Task 9, never hand-rolled). Its handler echoes a fixed marker.
    struct FakeRoute {
        method: Method,
        path: &'static str,
        handler: FakeHandler,
    }

    struct FakeHandler;

    impl Handler for FakeHandler {
        fn handle<'a>(
            &'a self,
            _req: &'a Request,
        ) -> BoxFuture<'a, Result<crate::Response, leaf_core::LeafError>> {
            Box::pin(async { Ok(crate::Response::ok()) })
        }
    }

    impl Route for FakeRoute {
        fn method(&self) -> Method {
            self.method.clone()
        }
        fn path(&self) -> &str {
            self.path
        }
        fn handler(&self) -> &dyn Handler {
            &self.handler
        }
    }

    #[test]
    fn route_table_matches_literal_and_captures_params() {
        let a = FakeRoute { method: Method::GET, path: "/a", handler: FakeHandler };
        let p = FakeRoute {
            method: Method::GET,
            path: "/products/{sku}",
            handler: FakeHandler,
        };
        let routes: Vec<&dyn Route> = vec![&a, &p];
        let table = RouteTable::build(&routes);

        // Parameterized match: captures `sku`.
        let (matched, params) = table
            .match_route(&Method::GET, "/products/COFFEE")
            .expect("matches the /products/{sku} route");
        assert_eq!(matched.path(), "/products/{sku}");
        assert_eq!(params, vec![("sku".to_string(), "COFFEE".to_string())]);

        // Literal match: no captures.
        let (lit, lit_params) =
            table.match_route(&Method::GET, "/a").expect("matches the literal /a route");
        assert_eq!(lit.path(), "/a");
        assert!(lit_params.is_empty());
    }

    #[test]
    fn route_table_non_match_is_none() {
        let a = FakeRoute { method: Method::GET, path: "/a", handler: FakeHandler };
        let routes: Vec<&dyn Route> = vec![&a];
        let table = RouteTable::build(&routes);

        // Unknown path → None.
        assert!(table.match_route(&Method::GET, "/nope").is_none());
        // Wrong segment count → None.
        assert!(table.match_route(&Method::GET, "/a/b").is_none());
    }

    #[test]
    fn route_table_wrong_method_is_none() {
        let p = FakeRoute {
            method: Method::GET,
            path: "/products/{sku}",
            handler: FakeHandler,
        };
        let routes: Vec<&dyn Route> = vec![&p];
        let table = RouteTable::build(&routes);

        // Path matches but the method does not → None.
        assert!(table.match_route(&Method::POST, "/products/COFFEE").is_none());
    }
}
