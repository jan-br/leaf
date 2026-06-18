# Leaf Web — HTTP transport + raw HTTP controllers (sub-project A) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task (fresh subagent per task, strict TDD, two-stage review). Steps use checkbox (`- [ ]`) tracking. Spec: `docs/superpowers/specs/2026-06-16-web-http-transport-design.md`.

**Goal:** A Spring-inspired, DI-native HTTP layer — leaf web abstractions over a pluggable hyper backend, with `#[controller]`/`#[rest_controller]` handler mapping, request extraction, JSON content, and a filter/control-advice extension model — proven by the storefront serving real HTTP.

**Architecture:** Public API is leaf traits (`Request`/`Response`/`WebServer`/`WebFilter`/`Handler`/`HttpMessageConverter`); hyper/tower/axum live only in `leaf-web-hyper` (swappable backend, a mock backend proves the boundary). The server *assembles itself from the container* — it resolves `Vec<Ref<dyn Route>>` / `Vec<Ref<dyn WebFilter>>` / `Vec<Ref<dyn ControlAdvice>>` via collection + by-trait injection. Controllers/filters/advice are ordinary beans.

**Tech Stack:** Rust, leaf-core DI, `http` crate (neutral `Method`/`StatusCode`/`HeaderMap`/`Uri`), hyper + tower + axum (backend only), leaf-serde (JSON), leaf-tokio (executor), leaf-macros/leaf-codegen (stereotypes).

**HARD CONSTRAINTS (every task):**
- **Dogfood stereotypes — no hand-rolled impls a macro can generate.** Controllers are `#[rest_controller]`, the default server is an `#[auto_config]` `FALLBACK` bean, async trait methods use `#[async_impl]` (no hand-written `BoxFuture`/`Box::pin` unless an eager-prelude/`'static` body forbids it), injection is the trait-dispatched `Ref`/`Vec<Ref<dyn _>>` primitives. If a needed macro is missing, ADD it, don't hand-roll. Applies to framework crates AND the storefront.
- **No type-name detection** — dispatch on traits/structure (`FromRequest`/`IntoResponse`/`syn` shape), never a spelled name.
- **Gate per task (force fresh recompile):** `cargo test --workspace` (0 failures), `cargo clippy --workspace --all-targets -- -D warnings` (exit 0), `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --workspace --no-deps` (exit 0). Emit `#[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]` on macro-generated items (rust-analyzer parity).

**Base:** `main` @ `edbb9d5` (the spec). Branch is `main` (the series lives there).

---

## File structure

- **`crates/leaf-web/`** (NEW — abstractions, `leaf-core`-only):
  - `src/lib.rs` — re-exports + module docs.
  - `src/request.rs` — `Request`, body access.
  - `src/response.rs` — `Response`, `IntoResponse`.
  - `src/handler.rs` — `Handler`, `Route`, `RouteTable`/matching.
  - `src/filter.rs` — `WebFilter`, `Next`, the filter-chain run.
  - `src/extract.rs` — `FromRequest` + `Path`/`Query`/`Json`/`Header`/`State`.
  - `src/content.rs` — `HttpMessageConverter`.
  - `src/advice.rs` — `ControlAdvice`, the error→response chain.
  - `src/server.rs` — `WebServer`, `ServerFactory`, `ServerProperties`, the `Dispatcher` (Route table + filters + advice → handle a `Request`).
  - `src/testing.rs` — an in-memory `MockServer`/direct-dispatch harness (proves the boundary; `#[cfg(any(test, feature = "testing"))]`).
- **`crates/leaf-web-hyper/`** (NEW — backend; deps hyper/tower/axum + leaf-web + leaf-tokio):
  - `src/lib.rs`, `src/server.rs` (the hyper `WebServer` impl + boundary conversion), `src/autoconfig.rs` (`#[auto_config]` `FALLBACK` `WebServer`).
- **`crates/leaf-serde/`** (exists) — add `src/http_converter.rs` (the JSON `HttpMessageConverter`).
- **`crates/leaf-codegen/`** + **`crates/leaf-macros/`** — `#[controller]`/`#[rest_controller]` (stereotype family), the request-mapping method attrs + controller-impl iterator, `#[control_advice]`.
- **`crates/leaf-starter-web/`** (exists) — bundle leaf-web + leaf-web-hyper + leaf-serde.
- **`crates/leaf/`** — `web` feature wiring + prelude (the macros + the leaf-web types as `leaf::web`).
- **`examples/storefront/`** — `web/` feature module: a `#[rest_controller]`, a `#[component]` `WebFilter`, a `#[control_advice]`.

---

## Stage 1 — `leaf-web` abstractions

### Task 1: `leaf-web` crate + `Request`/`Response`/`IntoResponse`

**Files:** Create `crates/leaf-web/Cargo.toml` (deps: `leaf-core.workspace`, `http.workspace`; add `http` to `[workspace.dependencies]` if absent), `crates/leaf-web/src/lib.rs`, `src/request.rs`, `src/response.rs`. Add `leaf-web` to `[workspace.members]` + `[workspace.dependencies]`.

**Contracts:**
```rust
// request.rs
pub struct Request {
    method: http::Method,
    uri: http::Uri,
    headers: http::HeaderMap,
    path_params: Vec<(String, String)>, // filled by the matcher (Task 2)
    body: Bytes,                          // bytes::Bytes (add to workspace deps)
}
impl Request { /* accessors: method(), uri(), path(), header(name), query_str(), body_bytes(), path_param(name) */ }

// response.rs
pub struct Response { status: http::StatusCode, headers: http::HeaderMap, body: Bytes }
impl Response { pub fn new(status) -> Self; pub fn ok() -> Self; pub fn with_body(self, Bytes) -> Self; pub fn with_header(self, k, v) -> Self; /* … */ }
pub trait IntoResponse { fn into_response(self) -> Response; }
impl IntoResponse for Response { /* identity */ }
impl IntoResponse for http::StatusCode { /* empty body */ }
impl IntoResponse for &str / String / () { /* text / empty */ }
impl<T: IntoResponse, E: IntoResponse> IntoResponse for Result<T, E> { /* Ok→T, Err→E */ }
```
- [ ] **Step 1 (test, request.rs `#[cfg(test)]`):** build a `Request` (GET `/p/7?x=1`, a header, a body) and assert `method()`, `path()`, `query_str()`, `header("...")`, `body_bytes()`.
- [ ] **Step 2:** run `cargo test -p leaf-web request` → FAIL (types undefined).
- [ ] **Step 3:** implement `Request` + accessors.
- [ ] **Step 4 (test, response.rs):** `Response::ok().with_body(...)` round-trips; `IntoResponse` for `StatusCode`/`&str`/`Result` produces the right status+body.
- [ ] **Step 5:** implement `Response` + `IntoResponse`. Run tests → PASS.
- [ ] **Step 6: gate + commit** `git commit -m "leaf-web: Request/Response/IntoResponse abstractions"`.

### Task 2: `Handler` + `Route` + route matching

**Files:** `crates/leaf-web/src/handler.rs`; modify `src/lib.rs`.

**Contracts:**
```rust
// Handler is dyn-dispatched + async → BoxFuture (the dyn-async seam). Written by the
// controller macro; users never hand-impl it.
pub trait Handler: Send + Sync {
    fn handle<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, LeafError>>;
}
// A Route is a bean (providing `dyn Route`) the server collects.
pub trait Route: Send + Sync {
    fn method(&self) -> http::Method;
    fn path(&self) -> &str;            // pattern, e.g. "/products/{sku}"
    fn handler(&self) -> &dyn Handler;
}
// Matching: pattern + a concrete path → Option<captured path params>. Use `matchit` (add
// to leaf-web deps) OR a small leaf matcher; expose only leaf types.
pub struct RouteTable { /* built from &[&dyn Route] */ }
impl RouteTable { pub fn build(routes: &[&dyn Route]) -> Self; pub fn match_route(&self, method, path) -> Option<(&dyn Route, Vec<(String,String)>)>; }
```
- [ ] **Step 1 (test):** a `RouteTable` over two fake `Route`s (`GET /a`, `GET /products/{sku}`) matches `/products/COFFEE` → the route + `[("sku","COFFEE")]`; non-match → `None`; wrong method → `None`.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** implement `Handler`/`Route`/`RouteTable` (matcher). A test `Route` impl is fine in the test module (the PRODUCTION Route impls come from the macro in Task 9 — do not hand-roll production routes).
- [ ] **Step 4:** tests PASS.
- [ ] **Step 5: gate + commit.**

### Task 3: `WebFilter` + `Next` + filter chain

**Files:** `crates/leaf-web/src/filter.rs`.

**Contracts:**
```rust
pub trait WebFilter: Send + Sync {
    // around-advice: inspect/modify, call next, or short-circuit.
    fn filter<'a>(&'a self, req: Request, next: Next<'a>) -> BoxFuture<'a, Result<Response, LeafError>>;
    fn order(&self) -> i32 { 0 } // OrderHint-style; lower = earlier
}
pub struct Next<'a> { /* the remaining filters + the terminal dispatch */ }
impl<'a> Next<'a> { pub fn run(self, req: Request) -> BoxFuture<'a, Result<Response, LeafError>>; }
```
- [ ] **Step 1 (test):** two test filters (one logs by pushing to a shared Vec, one short-circuits on a header) compose: ordered execution; short-circuit skips the terminal; pass-through reaches the terminal.
- [ ] **Step 2:** run → FAIL.
- [ ] **Step 3:** implement `WebFilter`/`Next` + the chain runner (order by `order()` then stable). Test filters via `#[async_impl]` where they're in-crate impls.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

### Task 4: `FromRequest` extractors

**Files:** `crates/leaf-web/src/extract.rs`.

**Contracts:**
```rust
pub trait FromRequest: Sized { fn from_request(req: &Request) -> Result<Self, LeafError>; }
pub struct Path<T>(pub T);   // T: from the single path param (or a tuple/struct via leaf-serde later)
pub struct Query<T>(pub T);  // T: Deserialize from the query string (leaf-serde)
pub struct Json<T>(pub T);   // T: Deserialize from the JSON body (leaf-serde; Task 5 converter)
pub struct Header<T>(pub T); // a named header
// State is DI: resolve a bean from the ResolveCtx/container the handler closure captured.
pub struct State<T>(pub T);
impl FromRequest for Path<String> / Query<...> / Json<...> / &Request-equivalent { … }
```
- [ ] **Step 1 (test):** `Path<String>::from_request` reads a path param; `Query<HashMap<String,String>>` parses the query; a missing-required extraction is a loud `LeafError` (→ 4xx later).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement the extractors (Json/Query lean on leaf-serde — if Task 5 not yet done, stub Json to a follow-on test; prefer ordering Task 5 before the Json extractor test). `State` resolution is wired in Task 9 (it needs the handler's captured ctx) — here define the type + a `from_request_with_state` seam or document that `State` is resolved by the controller codegen, not `FromRequest`.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

### Task 5: `HttpMessageConverter` + leaf-serde JSON impl

**Files:** `crates/leaf-web/src/content.rs`; `crates/leaf-serde/src/http_converter.rs` (modify leaf-serde Cargo to dep leaf-web, or define the trait in leaf-web and impl in leaf-serde).

**Contracts:**
```rust
// leaf-web
pub trait HttpMessageConverter: Send + Sync {
    fn content_type(&self) -> &str;                 // "application/json"
    fn write(&self, value: &dyn ErasedSerialize) -> Result<Bytes, LeafError>;   // serialize → body
    fn read<T: DeserializeOwned>(&self, body: &[u8]) -> Result<T, LeafError>;   // deserialize body
}
// leaf-serde: JsonConverter implementing it via serde_json. A #[component] bean.
```
- [ ] **Step 1 (test, leaf-serde):** `JsonConverter` round-trips a `#[derive(Serialize, Deserialize)]` struct (write → bytes → read).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement `HttpMessageConverter` (leaf-web) + `JsonConverter` (leaf-serde, a `#[component]` bean — NOT a hand-registered Provider). Wire `Json<T>` extractor (Task 4) + the rest-controller serialize-return through it.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

### Task 6: `ControlAdvice` + `WebServer`/`ServerFactory` + `Dispatcher`

**Files:** `crates/leaf-web/src/advice.rs`, `src/server.rs`.

**Contracts:**
```rust
// advice.rs — global error handling (Spring @ControllerAdvice/@ExceptionHandler)
pub trait ControlAdvice: Send + Sync {
    fn handle(&self, err: &LeafError, req: &Request) -> Option<Response>; // Some = this advice maps it
    fn order(&self) -> i32 { 0 }
}
// server.rs
pub struct ServerProperties { pub host: String, pub port: u16 } // @ConfigurationProperties "leaf.web.server"
pub trait WebServer: Send + Sync {
    fn serve<'a>(&'a self, dispatcher: Arc<Dispatcher>, props: &'a ServerProperties) -> BoxFuture<'a, Result<(), LeafError>>;
}
// The Dispatcher: the protocol-agnostic request engine. Backends feed it a Request, it runs
// filters → match route → handler → return-policy → ControlAdvice on Err.
pub struct Dispatcher { table: RouteTable, filters: Vec<Arc<dyn WebFilter>>, advice: Vec<Arc<dyn ControlAdvice>> }
impl Dispatcher {
    pub fn new(routes, filters, advice) -> Self;        // ordering applied here
    pub async fn dispatch(&self, req: Request) -> Response; // never errors out — maps via advice/default
}
```
- [ ] **Step 1 (test):** a `Dispatcher` over a route that returns `Err(LeafError)` + a `ControlAdvice` mapping it to 404 → `dispatch` yields 404; no advice → a default 500; a successful route → its response through the filters.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement `ControlAdvice`, `ServerProperties`, `WebServer`, and `Dispatcher::{new,dispatch}` (filters → match → handler → IntoResponse/serialize → advice on Err → default). Default error mapping is one built-in `ControlAdvice` (overridable).
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

### Task 7: in-memory `MockServer` + the DI-assembly proof (no hyper)

**Files:** `crates/leaf-web/src/testing.rs`; an integration test `crates/leaf-web/tests/dispatch_through_mock.rs`.

- [ ] **Step 1 (test):** register (via leaf-boot test harness, the same used in leaf-core tests) two `dyn Route` beans, one `dyn WebFilter` bean, one `dyn ControlAdvice` bean; build a `Dispatcher` from `Vec<Ref<dyn Route>>`/`Vec<Ref<dyn WebFilter>>`/`Vec<Ref<dyn ControlAdvice>>` resolved FROM THE CONTAINER (collection injection); drive a `Request` through a `MockServer` and assert the response. This proves (a) the abstraction is backend-free and (b) the container-assembly shape works end-to-end with no hyper.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement `MockServer` (a `WebServer` impl that just holds the `Dispatcher` + exposes `handle(Request) -> Response` for tests). The Routes/filters/advice in the test are `#[component]` beans (dogfood) providing the `dyn` views.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

---

## Stage 2 — controller macros (`leaf-codegen`/`leaf-macros`)

### Task 8: `#[controller]` / `#[rest_controller]` stereotypes

**Files:** `crates/leaf-codegen/src/stereotype.rs` (add `Stereotype::Controller` exists; add `RestController` + the controller-family marker), `crates/leaf-macros/src/lib.rs` (`#[rest_controller]` proc-macro; `#[controller]` exists as a stereotype — confirm it lowers to a `@Component`).

- [ ] **Step 1 (test, leaf-codegen token test):** `#[rest_controller] struct Api;` emits a `@Component` Descriptor whose `meta.markers` include the `Controller` (and `RestController`) markers; `#[controller]` likewise without the rest marker.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** add the `RestController` stereotype (markers `[RestController, Controller, Component]`) mirroring how `Service`/`Repository` are defined; `#[rest_controller]` proc-macro delegating to `stereotype::emit_struct`.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

### Task 9: request-mapping attrs + the controller-impl iterator (the core codegen)

**Files:** `crates/leaf-codegen/src/web_controller.rs` (NEW — the lowering), `crates/leaf-macros/src/lib.rs` (the `#[get]`/`#[post]`/`#[put]`/`#[delete]`/`#[route]` markers + extend the controller impl handling, mirroring `#[advisable]`/`#[configuration] impl`).

**The model:** like `#[advisable]`, a method attr alone can't emit sibling rows, so the controller's `impl` block is processed as a unit. Each mapped method lowers to ONE `#[doc(hidden)]` generated `Route` bean: a struct holding `Ref<TheController>` (DI'd), implementing `Route` (method+path) and a `Handler` whose `handle` resolves each parameter via its `FromRequest` extractor (dispatch on the param's structural extractor type — `Path<_>`/`Query<_>`/`Json<_>`/`State<_>`/`&Request` — via a trait, NEVER a name match), invokes `self.controller.method(args).await`, and applies the return policy (`#[controller]` → `IntoResponse`; `#[rest_controller]` → serialize via the injected `HttpMessageConverter`). The generated `Route` is a `#[component]`-equivalent bean providing `dyn Route` (emit through the SAME descriptor/seed machinery the stereotypes use — do not hand-write the Provider).

- [ ] **Step 1 (test, token):** `#[rest_controller] impl Api { #[get("/products/{sku}")] async fn get(&self, sku: Path<String>) -> Result<ProductDto, LeafError> {..} }` emits a generated `Route` bean: provides `dyn ::leaf_web::Route`, `method()==GET`, `path()=="/products/{sku}"`, the handler resolves `Path<String>` via `FromRequest`, calls the method, and serializes the return via the converter. Assert the emitted tokens contain the `provides = "dyn ...Route"` descriptor + the `<Path<String> as FromRequest>::from_request` call + the converter `write`.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement `web_controller::expand_controller_impl` (iterate methods with a mapping attr; build the `Route` bean per method via the existing descriptor/seed emitter; arg-resolution loop over `FromRequest`; return policy by stereotype). Reuse `#[async_impl]`-style desugaring for the generated `Handler::handle`.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

### Task 10: `#[control_advice]`

**Files:** `crates/leaf-macros/src/lib.rs`, `crates/leaf-codegen/src/web_controller.rs`.

- [ ] **Step 1 (test, token):** `#[control_advice] struct Errors;` + an impl with an `#[exception_handler]` method `fn not_found(&self, e: &LeafError) -> Option<Response>` emits a `#[component]` bean providing `dyn ControlAdvice` whose `handle` delegates to the method.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement `#[control_advice]` (a `@Component` providing `dyn ControlAdvice`; the impl iterator wires `#[exception_handler]` methods). Simplest first cut: a single `handle` method on the advice impl (defer multi-`#[exception_handler]` dispatch if needed, but prefer the method-iterator for parity with controllers).
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

---

## Stage 3 — the hyper backend (`leaf-web-hyper`)

### Task 11: the hyper `WebServer` impl

**Files:** Create `crates/leaf-web-hyper/Cargo.toml` (deps: leaf-core, leaf-web, leaf-tokio, `hyper`, `hyper-util`, `tower`, `http-body-util`, `bytes`; add to workspace), `src/lib.rs`, `src/server.rs`.

- [ ] **Step 1 (test, integration `tests/serves_http.rs`):** build a `Dispatcher` with one route (`GET /ping` → 200 "pong"); a `HyperServer` serves it on `127.0.0.1:0` (ephemeral); a real HTTP client (`reqwest` or hyper client, dev-dep) GETs `/ping` → 200 "pong"; a logging filter records the request; an `Err` route + advice → mapped status.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement `HyperServer: WebServer` — bind+serve on leaf-tokio's executor; per connection/request, convert hyper `Request<Incoming>` → leaf `Request` (collect the body via `http-body-util`), call `dispatcher.dispatch(req).await`, convert leaf `Response` → hyper response. NOTHING leaf-web-facing exposes hyper. Use `#[async_impl]` for the `WebServer::serve` impl.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

### Task 12: the `#[auto_config]` default `WebServer` + server-run wiring

**Files:** `crates/leaf-web-hyper/src/autoconfig.rs`; possibly a `leaf-web` `WebServerRunner` (a `#[runner]`/lifecycle bean that resolves the `WebServer` + builds the `Dispatcher` from the container + serves).

- [ ] **Step 1 (test, integration):** booting an app (leaf-boot harness) with `leaf-web-hyper` linked + a `#[rest_controller]` registers a `FALLBACK` `dyn WebServer` (the hyper one), and a `WebServerRunner` resolves `Vec<Ref<dyn Route>>`+filters+advice, builds the `Dispatcher`, and serves — assert the endpoint responds. A user-provided `dyn WebServer` supersedes the FALLBACK (OnMissingBean).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement: `HyperServerAutoConfig` (`#[auto_config] impl { #[bean(provides="dyn ::leaf_web::WebServer")] #[conditional(on_missing_bean(dyn ::leaf_web::WebServer))] fn web_server(&self) -> HyperServer }`); a `WebServerRunner` (`#[runner]` or a lifecycle bean) that injects `Ref<dyn WebServer>` + `Vec<Ref<dyn Route>>` + `Vec<Ref<dyn WebFilter>>` + `Vec<Ref<dyn ControlAdvice>>` + `Ref<AppProperties>`/`ServerProperties`, builds the `Dispatcher`, and runs `serve`. All beans, no manual registration.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

---

## Stage 4 — integration + the storefront proof

### Task 13: `leaf-starter-web` bundle + facade `web` feature + prelude

**Files:** `crates/leaf-starter-web/Cargo.toml`+`src/lib.rs` (add leaf-web + leaf-web-hyper + leaf-serde re-exports), `crates/leaf/Cargo.toml` (`web` feature → the new deps), `crates/leaf/src/lib.rs` (`pub use leaf_web as web;` + the `::leaf_web::` facade-alias re-exports the macros emit), `crates/leaf/src/prelude.rs` (`controller`, `rest_controller`, `get`, `post`, `put`, `delete`, `route`, `control_advice`, `exception_handler` + the leaf-web types `Request`/`Response`/`Json`/`Path`/`Query`/`State`/`WebFilter`/`ControlAdvice`).

- [ ] **Step 1 (test):** an umbrella-only integration test (under `crates/leaf` or a fixture) with `use leaf::prelude::*;` defines a `#[rest_controller]` with a `#[get]` and boots — proving the macro paths resolve through the facade aliases (mirrors the existing `::leaf_cache::`/`::leaf_tx::` alias pattern).
- [ ] **Step 2:** FAIL (unresolved macro/type paths).
- [ ] **Step 3:** wire the bundle + feature + prelude + facade re-exports (the `extern crate leaf as leaf_web` alias + `pub use leaf_web::{…}` of the macro-referenced symbols, exactly like leaf_cache/leaf_tx).
- [ ] **Step 4:** PASS.
- [ ] **Step 5: gate + commit.**

### Task 14: storefront REST proof

**Files:** `examples/storefront/Cargo.toml` (enable `web`), `examples/storefront/src/web/mod.rs`, `src/web/catalog_controller.rs`, `src/web/order_controller.rs`, `src/web/access_log_filter.rs`, `src/web/error_advice.rs`; `src/main.rs`/module wiring.

- [ ] **Step 1 (test, integration `examples/storefront/tests/http.rs`):** boot the storefront, `GET /products/COFFEE` → 200 JSON `{sku,name,price_cents}` (resolved via `CatalogService`); `GET /products/NOPE` → 404 (the `#[control_advice]`); `POST /orders` with a JSON body → 201/200 with the created order (via `OrderService`); the access-log filter recorded both.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** implement, ALL via stereotypes (dogfood): `#[rest_controller] CatalogController { catalog: Ref<CatalogService> }` with `#[get("/products/{sku}")]`; `#[rest_controller] OrderController { orders: Ref<OrderService> }` with `#[post("/orders")]`; `#[component] AccessLogFilter` (a `WebFilter`, via `#[async_impl]`); `#[control_advice] StorefrontErrors` mapping unknown-SKU → 404. No hand-written `Route`/`Provider`/`Handler` impls.
- [ ] **Step 4:** PASS; `cargo run -p storefront` serves (document the manual curl too).
- [ ] **Step 5: gate + commit.**

---

## Self-review notes
- **Spec coverage:** abstractions (T1–T6), pluggability/mock proof (T7), stereotypes+mapping (T8–T9), control-advice (T10), hyper backend + auto-config (T11–T12), DI-assembly via collection injection (T7, T12), bundle/facade (T13), storefront proof + filter + advice (T14). gRPC/actors correctly absent (deferred).
- **Type consistency:** `Request`/`Response`/`IntoResponse` (T1) → used by `Handler`/`Route` (T2), `FromRequest` (T4), `Dispatcher` (T6), the macro (T9), the backend (T11). `WebServer`/`Dispatcher`/`WebFilter`/`ControlAdvice`/`Route` names are stable across tasks. `HttpMessageConverter` (T5) used by `Json` (T4) + rest-controller return (T9).
- **Ordering caveat:** Task 4's `Json`/`Query` extractors depend on Task 5's converter — implement Task 5 before the Json extractor test, or split T4 so `Path`/`Header` land first and `Json`/`Query` follow T5. Flagged so the executor sequences it.
- **Dogfooding:** every production Route/filter/advice/server-default is a stereotype/`#[auto_config]` bean; the only hand-written trait impls are in `#[cfg(test)]` fakes and the ONE backend (`HyperServer`, which legitimately bridges hyper).
