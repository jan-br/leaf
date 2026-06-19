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

/// The structural outcome of matching `(method, path)` against the table.
///
/// Richer than a bare `Option` so the dispatcher can DISTINGUISH the two negative
/// cases the routing ethos demands be kept apart:
/// - [`NotFound`](RouteOutcome::NotFound) — no pattern matched the path at all
///   (a `404` / `NoSuchBean` shape).
/// - [`MethodNotAllowed`](RouteOutcome::MethodNotAllowed) — a pattern matched the
///   PATH but no registered route answers the request method; carries the methods
///   that DO match the path (the `Allow` header, a `405`).
pub enum RouteOutcome<'r> {
    /// A route matched the method AND the path: the route + its captured params.
    Matched(RouteMatch<'r>),
    /// A pattern matched the path but not with this method; the listed methods are
    /// the ones whose patterns DO match the path (deduped, registration order).
    MethodNotAllowed(Vec<Method>),
    /// No pattern matched the concrete path at all.
    NotFound,
}

// Hand-written `Debug` (not derived): `dyn Route` is not `Debug`, and requiring it
// would force every macro-generated route bean to derive `Debug`. We render the
// matched route by its PATTERN (`Route::path`) plus the captured params instead.
impl std::fmt::Debug for RouteOutcome<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteOutcome::Matched((route, params)) => {
                f.debug_tuple("Matched").field(&route.path()).field(params).finish()
            }
            RouteOutcome::MethodNotAllowed(methods) => {
                f.debug_tuple("MethodNotAllowed").field(methods).finish()
            }
            RouteOutcome::NotFound => f.write_str("NotFound"),
        }
    }
}

/// A build-time report over a set of routes: the compiled [`RouteTable`] plus any
/// duplicate `(method, exact-pattern)` registrations detected while compiling.
///
/// Duplicate exact patterns are a wiring bug (the matcher would silently shadow by
/// slice order); the routing ethos is "loud or nothing", so [`RouteTable::build`]
/// emits an `eprintln!` diagnostic for each, and this report exposes them for tests
/// / callers that want to act on them.
pub struct RouteReport<'r> {
    /// The compiled table (ready to match).
    pub table: RouteTable<'r>,
    /// The duplicate `(method, exact-pattern)` pairs found at build time.
    pub duplicates: Vec<(Method, String)>,
}

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
    /// segments once), ordered by SPECIFICITY and surfacing duplicate-route bugs.
    ///
    /// Convenience wrapper over [`build_report`](RouteTable::build_report) for
    /// callers that only want the table; duplicate `(method, exact-pattern)`
    /// registrations are still reported loudly via `eprintln!` (see `build_report`).
    #[must_use]
    pub fn build(routes: &[&'r dyn Route]) -> Self {
        RouteTable::build_report(routes).table
    }

    /// Build a table AND a [`RouteReport`] (the table + detected duplicates).
    ///
    /// Two ordering/diagnostic guarantees the bare `Vec`-order matcher could not
    /// give:
    /// - SPECIFICITY ordering — routes are stably sorted so a MORE specific
    ///   pattern (more literal segments / fewer captures) is tried before a less
    ///   specific same-arity one. A literal segment therefore beats a `{capture}`
    ///   of the same arity REGARDLESS of contributed (linkme) order, killing the
    ///   `/users/me` vs `/users/{id}` coin-flip. The sort is stable, so genuinely
    ///   equal-specificity routes keep their contributed order.
    /// - DUPLICATE detection — a repeated `(method, exact-pattern)` is a wiring bug
    ///   (the matcher would silently shadow by slice order). Each is collected into
    ///   the report AND emitted via `eprintln!` so it is loud at startup.
    #[must_use]
    pub fn build_report(routes: &[&'r dyn Route]) -> RouteReport<'r> {
        let mut compiled: Vec<CompiledRoute<'r>> = routes
            .iter()
            .map(|&route| CompiledRoute {
                route,
                method: route.method(),
                segments: compile_pattern(route.path()),
            })
            .collect();

        // Detect duplicate (method, exact-pattern) registrations on the COMPILED
        // segments (so equivalent-but-differently-written patterns — trailing
        // slash, `//` — collapse to the same key, no textual-name comparison).
        let mut seen: Vec<(Method, String)> = Vec::new();
        let mut duplicates: Vec<(Method, String)> = Vec::new();
        for entry in &compiled {
            let key = (entry.method.clone(), canonical_pattern(&entry.segments));
            if seen.contains(&key) {
                if !duplicates.contains(&key) {
                    eprintln!(
                        "leaf-web: duplicate route registration — {} {} declared more than once \
                         (one will shadow the other); each (method, path) must be unique",
                        key.0, key.1,
                    );
                    duplicates.push(key);
                }
            } else {
                seen.push(key);
            }
        }

        // Order by specificity: most specific first (a smaller specificity rank is
        // tried earlier). Stable, so equal-specificity routes keep contributed order.
        compiled.sort_by_key(|e| specificity_rank(&e.segments));

        RouteReport { table: RouteTable { routes: compiled }, duplicates }
    }

    /// Match `(method, path)` against the table, returning a [`RouteOutcome`].
    ///
    /// Walks the specificity-ordered routes: the first whose pattern matches the
    /// path AND whose method equals `method` wins ([`Matched`](RouteOutcome::Matched)).
    /// If no method matches but at least one pattern matched the PATH, the outcome is
    /// [`MethodNotAllowed`](RouteOutcome::MethodNotAllowed) carrying those methods
    /// (the `Allow` set); otherwise [`NotFound`](RouteOutcome::NotFound).
    #[must_use]
    pub fn match_route(&self, method: &Method, path: &str) -> RouteOutcome<'r> {
        let segments: Vec<&str> = split_path(path);
        let mut allowed: Vec<Method> = Vec::new();
        for entry in &self.routes {
            if let Some(params) = match_segments(&entry.segments, &segments) {
                if entry.method == *method {
                    return RouteOutcome::Matched((entry.route, params));
                }
                // Path matched, method did not: record it for the Allow set.
                if !allowed.contains(&entry.method) {
                    allowed.push(entry.method.clone());
                }
            }
        }
        if allowed.is_empty() {
            RouteOutcome::NotFound
        } else {
            RouteOutcome::MethodNotAllowed(allowed)
        }
    }
}

/// A stable specificity rank for a pattern: routes sort ascending by this, so a
/// SMALLER rank is tried first (more specific). A literal segment is more specific
/// than a capture at the same position, so we rank purely by the count of capture
/// segments (fewer captures = more specific = smaller rank). This is structural —
/// it counts capture vs literal segments, never inspecting any segment's text.
fn specificity_rank(segments: &[PatternSegment]) -> usize {
    segments.iter().filter(|s| s.capture.is_some()).count()
}

/// A canonical string key for a compiled pattern (every capture normalised to
/// `{}` so the KEY ignores capture-name spelling — two routes differing only in a
/// capture's name still collide). Used only for duplicate detection, never for
/// matching behaviour, and built from the compiled segments (so trailing-slash /
/// `//` variants of the same pattern share a key).
fn canonical_pattern(segments: &[PatternSegment]) -> String {
    let mut out = String::new();
    for seg in segments {
        out.push('/');
        if seg.capture.is_some() {
            out.push_str("{}");
        } else {
            out.push_str(&seg.literal);
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
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
        let (matched, params) = match table.match_route(&Method::GET, "/products/COFFEE") {
            RouteOutcome::Matched(m) => m,
            other => panic!("expected the /products/{{sku}} route, got {other:?}"),
        };
        assert_eq!(matched.path(), "/products/{sku}");
        assert_eq!(params, vec![("sku".to_string(), "COFFEE".to_string())]);

        // Literal match: no captures.
        let (lit, lit_params) = match table.match_route(&Method::GET, "/a") {
            RouteOutcome::Matched(m) => m,
            other => panic!("expected the literal /a route, got {other:?}"),
        };
        assert_eq!(lit.path(), "/a");
        assert!(lit_params.is_empty());
    }

    #[test]
    fn route_table_non_match_is_not_found() {
        let a = FakeRoute { method: Method::GET, path: "/a", handler: FakeHandler };
        let routes: Vec<&dyn Route> = vec![&a];
        let table = RouteTable::build(&routes);

        // Unknown path → NotFound.
        assert!(matches!(table.match_route(&Method::GET, "/nope"), RouteOutcome::NotFound));
        // Wrong segment count → NotFound.
        assert!(matches!(table.match_route(&Method::GET, "/a/b"), RouteOutcome::NotFound));
    }

    #[test]
    fn route_table_wrong_method_is_method_not_allowed() {
        let p = FakeRoute {
            method: Method::GET,
            path: "/products/{sku}",
            handler: FakeHandler,
        };
        let routes: Vec<&dyn Route> = vec![&p];
        let table = RouteTable::build(&routes);

        // Path matches but the method does not → MethodNotAllowed listing the
        // methods whose patterns DO match the concrete path.
        match table.match_route(&Method::POST, "/products/COFFEE") {
            RouteOutcome::MethodNotAllowed(allowed) => {
                assert_eq!(allowed, vec![Method::GET]);
            }
            other => panic!("expected MethodNotAllowed, got {other:?}"),
        }
    }

    /// A literal segment must beat a same-arity `{capture}` REGARDLESS of the
    /// order the routes were contributed in (linkme collection order is
    /// non-deterministic). We build the table in BOTH insertion orders and assert
    /// `/users/me` always resolves to the literal route, never the capture.
    #[test]
    fn literal_beats_capture_regardless_of_order() {
        let me = FakeRoute { method: Method::GET, path: "/users/me", handler: FakeHandler };
        let id = FakeRoute { method: Method::GET, path: "/users/{id}", handler: FakeHandler };

        for routes in [
            vec![&me as &dyn Route, &id as &dyn Route],
            vec![&id as &dyn Route, &me as &dyn Route],
        ] {
            let table = RouteTable::build(&routes);
            let matched = match table.match_route(&Method::GET, "/users/me") {
                RouteOutcome::Matched((route, _)) => route,
                other => panic!("expected a match, got {other:?}"),
            };
            assert_eq!(
                matched.path(),
                "/users/me",
                "the literal route must win over the same-arity capture"
            );

            // The capture still answers a non-literal concrete path.
            let cap = match table.match_route(&Method::GET, "/users/42") {
                RouteOutcome::Matched((route, params)) => {
                    assert_eq!(params, vec![("id".to_string(), "42".to_string())]);
                    route
                }
                other => panic!("expected the capture to match /users/42, got {other:?}"),
            };
            assert_eq!(cap.path(), "/users/{id}");
        }
    }

    /// Two routes with the same (method, exact pattern) is a wiring bug: `build`
    /// must surface a LOUD diagnostic rather than silently shadow by slice order.
    #[test]
    fn duplicate_route_is_a_loud_diagnostic() {
        let a = FakeRoute { method: Method::GET, path: "/dup", handler: FakeHandler };
        let b = FakeRoute { method: Method::GET, path: "/dup", handler: FakeHandler };
        let routes: Vec<&dyn Route> = vec![&a, &b];

        let report = RouteTable::build_report(&routes);
        assert!(
            report.duplicates.iter().any(|(m, p)| *m == Method::GET && p == "/dup"),
            "build must report the duplicate (GET, /dup), got {:?}",
            report.duplicates
        );
    }
}
