# Leaf Web — HTTP transport abstraction + raw HTTP controllers (sub-project A)

Status: **design — pending spec review → implementation plan**
Date: 2026-06-16

## Problem & goal

leaf has the *slots* for a web layer (a `#[controller]` stereotype that is currently a
bare marker, a `web` capability feature, and a `leaf-starter-web` placeholder whose own
docs say the real stack — "`leaf-router + leaf-tokio + leaf-json + leaf-validation`" — is
deferred ecosystem work). This sub-project fills the **base layer**: a real embedded HTTP
server and request-dispatch built as a *leaf abstraction over a pluggable backend*, with
`#[controller]`/`#[rest_controller]` handler mapping — all on top of the DI primitives
(beans, by-trait + collection injection, conditionals, auto-config) finished this session.

It is explicitly the foundation that **gRPC (sub-project B)** stacks on, and an
**extension surface** other crates (security, proxies, global error handling) layer onto —
both deferred to their own specs, but the abstractions here are shaped to make them clean.

## Scope

**In:** the leaf web abstractions; one hyper/tower/axum backend implementing them;
`#[controller]`/`#[rest_controller]` + request-mapping method attrs; argument resolution;
return-value handling + JSON content; the filter/interceptor/`#[control_advice]` extension
model; the auto-config + DI wiring; a storefront REST endpoint as the proof.

**Out (own later specs):** gRPC (`#[grpc_service]`, sub-project B); `leaf-actors` over
`factories`; reactive/streaming (WebFlux analogue); server-side view rendering; OpenAPI;
WebSocket; client-side HTTP.

## Architecture — the abstraction boundary

The **public surface is leaf abstractions**; hyper/tower/axum are *swappable backends*
behind them, never exposed (Spring's `spring-web` over a pluggable Tomcat/Jetty/Netty).

```
 #[rest_controller] / #[controller]        WebFilter / #[control_advice] beans     ← your beans + extension beans
 ───────────────────────────────────────────────────────────────────────────────
 leaf-web ABSTRACTIONS (traits):  Request · Response · WebServer/ServerFactory ·
   Handler · HandlerMapping · WebFilter/FilterChain · HttpMessageConverter         ← the ONLY public web API
 ───────────────────────────────────────────────────────────────────────────────
 leaf-web-hyper BACKEND (the one crate that names hyper/tower/axum)                ← swappable; a mock backend impls the same traits
 ───────────────────────────────────────────────────────────────────────────────
                       leaf-tokio runtime (the shared executor)
```

**Crate topology** (mirrors the leaf-redis ecosystem pattern):
- **`leaf-web`** — the abstractions. `leaf-core`-only (like `leaf-cache`/`leaf-tx` define
  their concern traits). Defines the web traits + the handler/controller model + the
  errors. Names NO HTTP-server library.
- **`leaf-web-hyper`** — the backend integration crate (`leaf-core` + `leaf-web` +
  `leaf-tokio` + `hyper`/`tower`/`axum`). Implements `WebServer`/`Handler` dispatch;
  contributes its `WebServer` as a `FALLBACK` auto-config bean (the default backend,
  overridable). The only crate that touches hyper/tower/axum.
- **`leaf-serde`** (exists) — the JSON `HttpMessageConverter` impl (content negotiation).
- **`leaf-macros`/`leaf-codegen`** — `#[controller]`/`#[rest_controller]` + the
  request-mapping method attrs + the controller-impl iterator that lowers handler methods.
- **`leaf-starter-web`** (exists, placeholder) — updated to bundle `leaf-web` +
  `leaf-web-hyper` + `leaf-serde` + `leaf-validation` + `leaf-tokio`.

**Neutral primitives:** for `Method`, `StatusCode`, `HeaderMap`, `Uri` we reuse the `http`
crate — the ecosystem-standard, server-agnostic vocabulary (hyper/axum/tonic all build on
it; it is not "hyper internals"). Only `Request`/`Response`/`WebServer`/`WebFilter`/… are
leaf traits. (If even those primitives must be fully-leaf, that is a one-line change here;
flagged as a decision.)

## Components

### 1. The web abstractions (`leaf-web`) — Spring `spring-web` analogue

- **`Request`** — method/uri/headers/path-params/query/body (a leaf type wrapping the
  `http` primitives + a body the backend fills). Spring `ServerHttpRequest`/`HttpServletRequest`.
- **`Response`** — status/headers/body, with ergonomic builders + `IntoResponse` (a leaf
  trait: any handler return that can become a `Response`). Spring `ServerHttpResponse` /
  `ResponseEntity`.
- **`Handler`** — the dispatch unit: `async fn handle(&self, Request) -> Result<Response, LeafError>`
  (a `BoxFuture` via `#[async_impl]`). A controller method lowers to one. Spring's
  `HandlerAdapter`-invoked handler.
- **`HandlerMapping` / `Route`** — a `(Method, path-pattern) → Handler` registration. The
  server holds the routing table. Spring `HandlerMapping` (`RequestMappingHandlerMapping`).
- **`WebServer` / `ServerFactory`** — the embedded server bean: `bind + serve` over a
  routing table + filter chain, on the leaf-tokio executor. Spring `WebServer` /
  `WebServerFactory`. Pluggable: `leaf-web-hyper` implements it; a test backend can too.
- **`WebFilter` / `FilterChain`** — the around-advice seam: `async fn filter(&self, Request, next: &Next) -> Result<Response,_>`. Ordered. Spring `Filter` + `HandlerInterceptor`,
  and the tower `Layer` seam underneath (in the backend only).
- **`HttpMessageConverter`** — serialize/deserialize a body to/from a type by content-type.
  `leaf-serde` provides the JSON impl. Spring `HttpMessageConverter`.
- **`ControlAdvice` / exception mapping** — map a `LeafError` (or a typed error) to a
  `Response` (status + body). Spring `@ControllerAdvice` + `@ExceptionHandler`.

### 2. The backend (`leaf-web-hyper`)

Implements `WebServer` on hyper + a tower service stack; the leaf `FilterChain` lowers to
tower `Layer`s; routing uses axum's matcher (or `matchit`) internally. Converts hyper's
request/response to/from the leaf `Request`/`Response` at the boundary — so nothing above
sees hyper. Registered as a `FALLBACK` `WebServer` bean via `#[auto_config]`
(`OnMissingBean(dyn WebServer)` — reusing the view-based back-off), so an alternative
backend supersedes it by providing the same `dyn WebServer` view.

### 3. Stereotypes + request-mapping macros

- **`#[controller]`** — a `@Component` specialization (controller family) + a handler-mapping
  hook: the controller is a managed bean whose request-mapping methods become routes.
- **`#[rest_controller]`** — `#[controller]` + the `@ResponseBody` policy: a handler's
  return value is serialized to the body (JSON) via `HttpMessageConverter`, vs `#[controller]`
  where a handler returns a `Response`/`IntoResponse` directly. (Honest: neither enforces
  REST conventions — full `Response` control stays available.)
- **`#[get("/path")]` / `#[post]` / `#[put]` / `#[delete]` / `#[route(method=…, path=…)]`** —
  request-mapping method attrs on controller methods. Spring `@GetMapping`/`@RequestMapping`.
- **Lowering (the controller-impl iterator):** like `#[advisable]`/`#[configuration] impl`,
  a method attr alone cannot emit sibling registration rows, so the controller's `impl`
  block is processed as a unit: each mapped method is lowered to a `Route` registration
  (a `Handler` that resolves the method's arguments, invokes `self.method(..)`, and applies
  the return-value policy). The macro dispatches on the parameter's **structural** form (a
  leaf `Path<T>`/`Query<T>`/`Json<T>`/`&Request` extractor type — trait dispatch via an
  `ArgFrom`-style trait, NEVER a type-name match, consistent with the no-type-names rule).
- **Argument resolution** — `Path<T>`, `Query<T>`, `Json<T>` (body), `Header<T>`,
  `State<Ref<Bean>>` (DI a collaborator), `&Request`. Each is a leaf extractor trait
  (`FromRequest`) the handler codegen calls — Spring's `HandlerMethodArgumentResolver`.

### 4. DI integration — the payoff of by-trait + collection injection

The controller-impl macro emits, per mapped method, a `Route` bean providing `dyn Handler`
keyed by `(method, path)`. The **server resolves its routing table and chain by injection**:
- `Vec<Ref<dyn Route>>` — every route any controller contributed (collection injection).
- `Vec<Ref<dyn WebFilter>>` — every filter/interceptor any crate contributed, ordered.
- `Vec<Ref<dyn ControlAdvice>>` — every exception handler any crate contributed.

So a controller is an ordinary bean (DI'd collaborators via `State`/constructor); the
server is auto-configured and **assembles itself from the container** — no central
registry, no codegen-time wiring. This is exactly what the collection + by-trait injection
were built for.

### 5. The extension model (security, proxies, global error handling)

Other crates extend the web layer by **contributing beans**, never by core changes:
- A `leaf-security` crate contributes `#[component]` `WebFilter` beans (auth, CSRF) — the
  server collects + orders them (Spring Security's filter chain).
- A proxy/gateway crate contributes filters or alternative `Route`s.
- Global error handling: `#[control_advice]` beans contribute `ControlAdvice` mappings;
  the dispatcher consults them when a handler returns `Err`.
All ordered by `@Order`/`OrderHint`, all auto-configured + overridable via the
`OnMissingBean`/precedence model. Spring's filter chain + `@ControllerAdvice`, expressed in
leaf's DI.

## Data flow (request lifecycle)

1. `WebServer` (backend) accepts a connection, builds a leaf `Request` at the boundary.
2. The **filter chain** runs (ordered `WebFilter`s): security, logging, etc. — each may
   short-circuit with a `Response` or call `next`.
3. **Handler mapping** matches `(method, path)` → the `Route`'s `Handler`.
4. The handler **resolves arguments** (`FromRequest` extractors: path/query/body/state).
5. The handler **invokes** the controller method (`self.method(args).await`).
6. **Return-value handling:** `#[controller]` → the returned `IntoResponse`; `#[rest_controller]`
   → serialize the return via `HttpMessageConverter` (JSON) into the body.
7. On `Err(LeafError)`: the **`ControlAdvice` chain** maps it to a `Response` (status+body);
   default mapping if none matches.
8. The `Response` flows back out through the filter chain → the backend writes it.

## Error handling

- Handlers return `Result<T, LeafError>` (the one error spine). A typed domain error can be
  mapped by a `#[control_advice]` `ControlAdvice` to a status + body.
- A default `ControlAdvice` maps `LeafError` kinds to sensible statuses (e.g.
  `NoSuchBean`/not-found → 404 conventions are app-defined; construction/internal → 500),
  overridable by a user `ControlAdvice` (precedence).
- Filter/extractor failures (bad body, missing param) map to 4xx via the same chain.
- All failures ride the existing single `LeafError` causal chain.

## Testing strategy

- **Unit (`leaf-web`):** the abstractions — `IntoResponse`, the `FromRequest` extractors,
  the handler-mapping matcher, content negotiation, the `ControlAdvice` chain ordering.
- **Pluggability proof:** a **mock/in-memory `WebServer` backend** in `leaf-web` tests that
  implements the same traits and drives a request through the chain with NO hyper — proving
  the abstraction is real and the transport is swappable.
- **Codegen (`leaf-codegen`):** token tests that `#[rest_controller] impl { #[get("/x")] fn h() }`
  emits a `Route` bean providing `dyn Handler` keyed on `(GET, "/x")`, with arg-resolution
  + the serialize-return policy; `#[controller]` emits the return-`Response` policy.
- **Integration (`leaf-web-hyper`):** boot a real server on an ephemeral port, issue HTTP
  requests, assert routing + JSON + a filter + a `#[control_advice]` error mapping
  end-to-end.
- **The storefront proof:** add a `#[rest_controller]` (e.g. `GET /products/{sku}` →
  `CatalogService`, `POST /orders` → `OrderService`) so `cargo run -p storefront` serves
  real HTTP, with a logging `WebFilter` + a `#[control_advice]` mapping unknown-SKU → 404.

## Spring concept → leaf mapping

| Spring | leaf |
|---|---|
| `@Controller` (stereotype + handler-mapping hook) | `#[controller]` |
| `@RestController` (`@Controller`+`@ResponseBody`) | `#[rest_controller]` |
| `@GetMapping`/`@RequestMapping` | `#[get("/..")]`/`#[route(..)]` |
| `WebServer`/`WebServerFactory` (pluggable Tomcat/Netty) | `WebServer`/`ServerFactory` (pluggable backend) |
| `DispatcherServlet` + `HandlerMapping`/`HandlerAdapter` | the server's routing table + `Handler` dispatch |
| `HandlerMethodArgumentResolver` | `FromRequest` extractors (`Path`/`Query`/`Json`/`State`) |
| `HttpMessageConverter` | `HttpMessageConverter` (`leaf-serde` JSON) |
| servlet `Filter` / `HandlerInterceptor` | `WebFilter`/`FilterChain` |
| `@ControllerAdvice` + `@ExceptionHandler` | `#[control_advice]` + `ControlAdvice` |
| auto-configured embedded server | `leaf-web-hyper` `#[auto_config]` `FALLBACK` `WebServer` |

## Decisions made

- The public web API is **leaf abstractions**; hyper/tower/axum live only in `leaf-web-hyper`
  (swappable backend) — Spring's pluggable-server model.
- Neutral HTTP value types (`Method`/`StatusCode`/`HeaderMap`/`Uri`) reuse the `http` crate
  (not server-internals). *(Decision to revisit if fully-leaf primitives are wanted.)*
- The controller family = inbound endpoints: `#[controller]`/`#[rest_controller]` (HTTP)
  now, `#[grpc_service]` (gRPC) in sub-project B — NOT `#[service]`.
- `#[rest_controller]` adds only the serialize-return (`@ResponseBody`) policy; it does NOT
  enforce REST — full `Response` control stays available.
- The server assembles from the container via **collection + by-trait injection** (`Vec<Ref<dyn Route>>`/
  `WebFilter`/`ControlAdvice`); extension crates contribute beans, never patch the core.
- Argument/return dispatch is **structural/trait-based** (`FromRequest`/`IntoResponse`),
  never type-name matching.
- The request/dispatch path is **plain** (no actor machinery); `leaf-actors`/`factories` is
  a separate later capability that handlers may *use* but the web layer never knows about.

## Deferred follow-ups

gRPC (sub-project B, on the same backend/port); `leaf-actors` over `factories`; reactive/
streaming; server-side views; OpenAPI/schema; WebSocket; an HTTP client. The abstractions
here are shaped so each slots in without reworking the core (gRPC adds a second `Handler`
family on the shared `WebServer`; security adds `WebFilter` beans; etc.).
