//! The [`FromRequest`] argument-extraction seam + the typed extractor wrappers
//! ([`Path`] / [`Query`] / [`Json`] / [`Header`]). Spring's
//! `HandlerMethodArgumentResolver` family, expressed in leaf's vocabulary.
//!
//! A controller method's parameters each resolve from the inbound [`Request`] via
//! their STRUCTURAL extractor type — the controller codegen (Task 9) dispatches on
//! the parameter's shape (`Path<_>` / `Query<_>` / `Json<_>` / `Header<_>` /
//! `&Request`), NEVER on a spelled type name, consistent with the no-type-names
//! rule. The codegen threads a uniform per-argument binding context ([`ExtractCtx`],
//! carrying the handler parameter NAME plus — for a `Header<_>` parameter — the HTTP
//! header name from its `#[header("X-Foo")]` attribute) into every extractor; only the
//! impls that need a fact read it. The request-only extractions get their [`FromRequest`]
//! impls here:
//!
//! - [`Request`] — the whole request, cloned (the `&Request` extractor).
//!
//! Handler-side DI collaborators are NOT an argument extractor: a controller is an
//! ordinary managed bean that FIELD-INJECTS its collaborators (`Ref<CatalogService>`),
//! the sanctioned leaf way — there is no `State<T>` argument type.
//!
//! [`Path<T>`] (a `{name}` capture, e.g. `sku` from `/products/{sku}`) is request-only
//! but NAME-dependent: it implements the context-aware [`FromRequestParts`] directly,
//! selecting ITS OWN capture by the handler parameter's name and parsing it via
//! [`FromStr`]. [`Header<T>`] is likewise context-aware but reads the HTTP HEADER NAME
//! the codegen carries from the parameter's `#[header("X-Foo")]` attribute (a header name
//! is not a valid Rust ident, so it cannot ride the `Pat::Ident`), looks the header up on
//! the request, and parses it via [`FromStr`]. [`Query<T>`] binds the request's query
//! string into an arbitrary `Deserialize` target `T` (the raw
//! `Query<`[`HashMap`](std::collections::HashMap)`<String, String>>` map is one
//! such `T`) via the `application/x-www-form-urlencoded` data format, which the query's
//! URL-fixed grammar lets leaf-web name directly — so `Query<T>` implements
//! [`FromRequestParts`] for any `T: DeserializeOwned` rather than a plain
//! `from_request(req)`. The extraction that needs the NEGOTIABLE body serde format
//! ([`Json<T>`] body deserialization) is likewise NOT a plain `from_request(req)` impl —
//! it needs the converter that only the controller codegen (Task 9) has in scope.
//! leaf-web defines all the wrapper types here (so the codegen can dispatch on
//! them structurally) and documents the seam; the BODY read rides the injected
//! [`HttpMessageConverter`] (Task 5) — leaf-web names no serde format for the body, only
//! the one fixed query format.

use std::str::FromStr;

use leaf_core::error::{Cause, ErrorKind, LeafError};

use crate::content::HttpMessageConverter;
use crate::Request;

/// The per-argument BINDING CONTEXT the controller codegen threads into EVERY extractor,
/// uniformly, alongside the request + converter. It carries the static facts about the
/// handler PARAMETER an extractor may need to resolve itself:
///
/// - the parameter NAME (the `Pat::Ident` the codegen already reads), which the
///   path-parameter extractor matches against the route's `{name}` captures, and
/// - an optional HEADER NAME (e.g. `X-Tenant-Id`), which the [`Header<T>`] extractor
///   reads off the request. A header name is NOT a valid Rust ident, so the codegen
///   reads it from a `#[header("X-Tenant-Id")]` PARAMETER ATTRIBUTE rather than the
///   parameter's `Pat::Ident` — it rides this seam, not the parameter name.
///
/// Threading one neutral context to every extractor keeps the codegen's dispatch the
/// uniform `<Ty as FromRequestParts>::from_request_parts(req, converter, ctx)` — only the
/// impls that NEED a fact read it (e.g. [`Path<T>`] reads the name, [`Header<T>`] reads
/// the header name); the request-only extractors ignore it. The macro never branches on
/// the parameter being a `Path`/`Header`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtractCtx<'a> {
    /// The handler parameter's name (its `Pat::Ident`), if the codegen had one to give.
    /// The path-parameter extractor matches this against the route's `{name}` captures.
    name: Option<&'a str>,
    /// The HTTP header name a [`Header<T>`] parameter binds, carried from its
    /// `#[header("X-Foo")]` attribute (a header name is not a Rust ident, so it cannot
    /// ride the parameter name). `None` for every non-header parameter.
    header_name: Option<&'a str>,
}

impl<'a> ExtractCtx<'a> {
    /// A context carrying the handler parameter's NAME (the common codegen case — every
    /// `fn handler(&self, sku: Path<String>)` parameter has a `Pat::Ident`).
    #[must_use]
    pub fn named(name: &'a str) -> Self {
        ExtractCtx { name: Some(name), header_name: None }
    }

    /// A context carrying the parameter NAME plus the HTTP HEADER NAME a [`Header<T>`]
    /// parameter binds (from its `#[header("X-Foo")]` attribute). The header name is the
    /// one fact a header extractor needs and the parameter's `Pat::Ident` cannot carry.
    #[must_use]
    pub fn for_header(name: &'a str, header_name: &'a str) -> Self {
        ExtractCtx { name: Some(name), header_name: Some(header_name) }
    }

    /// A context carrying no parameter name (a destructured / wildcard parameter the
    /// codegen could not name). Name-dependent extractors fail loudly rather than guess.
    #[must_use]
    pub fn empty() -> Self {
        ExtractCtx { name: None, header_name: None }
    }

    /// The handler parameter's name, if any.
    #[must_use]
    pub fn name(&self) -> Option<&'a str> {
        self.name
    }

    /// The HTTP header name a [`Header<T>`] parameter binds (its `#[header("X-Foo")]`
    /// attribute), if any. `None` for every non-header parameter.
    #[must_use]
    pub fn header_name(&self) -> Option<&'a str> {
        self.header_name
    }
}

/// Resolve `Self` from an inbound [`Request`] ALONE — the request-only, name-free
/// extractions (Spring's `HandlerMethodArgumentResolver`). This is the simple base the
/// converter-aware [`FromRequestParts`] supersets via a blanket impl; the controller
/// codegen always calls the [`FromRequestParts`] seam (which a `FromRequest` extractor
/// reaches through the blanket), dispatching on the parameter's structural extractor
/// type — never a spelled type name.
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

/// A path-parameter extractor: `T` is read from the captured `{name}` segment whose
/// name matches the handler PARAMETER (e.g. `sku: Path<String>` reads `{sku}` from
/// `/products/{sku}`). Spring's `@PathVariable`.
///
/// `Path<T>` is the ONE request-only extractor that needs the per-argument binding
/// context: its capture is selected BY NAME (the handler parameter's `Pat::Ident`,
/// carried in the [`ExtractCtx`]), so a multi-capture route like
/// `/users/{uid}/orders/{oid}` binds EACH `Path` parameter to ITS OWN capture rather
/// than all to the first. The captured segment is parsed to `T` via [`FromStr`]
/// (`Path<String>` is the identity case; `Path<u32>` parses a numeric segment), so a
/// malformed segment is a loud [`ErrorKind::ConvertError`] — never a panic. Because the
/// name reaches it through the uniform context, the controller codegen still dispatches
/// purely on the parameter's STRUCTURAL extractor type — it never branches on `Path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path<T>(pub T);

/// A query-string extractor: `T` is decoded from the request's query (e.g.
/// `?page=2&size=10` → `Pagination { page: 2, size: 10 }`). Spring's `@RequestParam`.
///
/// `Query<T>` has ONE uniform [`FromRequestParts`] route for ANY `T: DeserializeOwned`:
/// it binds the `application/x-www-form-urlencoded` query string into `T` (which
/// percent-decodes keys+values and treats a repeated key as a loud
/// [`ErrorKind::ConvertError`] — `serde_urlencoded` does not collect repeated keys
/// into a sequence). The
/// raw name→value `Query<HashMap<String, String>>` is just one such `T` — there is no
/// per-target special case. Unlike the request BODY (whose wire format rides the
/// negotiable injected [`HttpMessageConverter`]), the query grammar is fixed by the
/// URL, so leaf-web names the query data format (`serde_urlencoded`) directly — a wire
/// DATA FORMAT, never an HTTP-server backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query<T>(pub T);

/// A request-body extractor: `T` is deserialized from the body in the negotiated
/// content-type. Spring's `@RequestBody`.
///
/// `Json<T>` has NO plain [`FromRequest`] impl (which sees only the request): a body
/// deserialization needs the content format. Instead it implements the
/// CONVERTER-AWARE [`FromRequestParts`], deserializing the body through the INJECTED
/// [`HttpMessageConverter`] (the JSON impl is a `#[component]` bean in `leaf-serde`)
/// — leaf-web names the serde data MODEL (the `T: DeserializeOwned` bound) but no
/// serde FORMAT, which stays on the converter's side. The controller codegen (Task
/// 9) lowers EVERY parameter through the one uniform `FromRequestParts` call site, so
/// `Json<T>` rides the same seam as a request-only `Path`/`Query`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Json<T>(pub T);

/// A single-header extractor: `T` is read from the named request header and parsed via
/// [`FromStr`]. Spring's `@RequestHeader("X-Foo")`.
///
/// `Header<T>` carries no header NAME on its own — a header name (e.g. `X-Tenant-Id`) is
/// not a valid Rust ident, so it cannot ride the handler parameter's `Pat::Ident` the way
/// a [`Path<T>`] capture name does. Instead the controller codegen reads it from a
/// `#[header("X-Tenant-Id")]` PARAMETER ATTRIBUTE and threads it through the per-argument
/// binding [`ExtractCtx`] (the `header_name` seam). Like [`Path<T>`], `Header<T>` is
/// therefore a context-aware [`FromRequestParts`] extractor rather than a plain
/// `from_request(req)`: it reads the header name off the [`ExtractCtx`], looks the header up
/// on the [`Request`] ([`Request::header`]), and parses it to `T` via [`FromStr`]
/// (`Header<String>` is the identity case; `Header<u32>` parses a numeric header). A missing
/// header, a missing header name (no `#[header(..)]` attr), or a parse failure is a loud
/// [`ErrorKind::ConvertError`] the dispatcher maps to a 4xx — never a panic, never a silent
/// default. Because the name reaches it through the uniform context, the controller codegen
/// still dispatches purely on the parameter's STRUCTURAL extractor type — it never branches
/// on `Header`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header<T>(pub T);

/// A per-request EXTENSION extractor: `T` is read from the request's type-keyed
/// extensions map (the typed attribute an upstream [`WebFilter`](crate::WebFilter)
/// attached via [`Request::insert_extension`]). Spring's request-attribute access /
/// axum's `Extension<T>`.
///
/// This is the seam a security/auth filter uses: it authenticates and attaches a
/// typed `Principal` to the [`Request`] it threads through `next.run(req)`, and the
/// downstream handler binds it with an `Extension<Principal>` parameter. The extractor
/// needs NO name from the [`ExtractCtx`] — it resolves PURELY by the type `T` (the
/// no-type-names rule is about the codegen never branching on a spelled type; resolving
/// a value by its `TypeId` is the std `Any` mechanism, not a textual-name check). It
/// rides the same uniform [`FromRequestParts`] seam as every other extractor, so it
/// requires NO special codegen — trait resolution picks this impl structurally.
///
/// A missing extension (no value of type `T` was attached) is a loud
/// [`ErrorKind::ConvertError`] the dispatcher maps to a `400` — never a panic, never a
/// silent default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extension<T>(pub T);

impl<T: serde::de::DeserializeOwned> Query<T> {
    /// Deserialize the request's query string into `T` via the
    /// `application/x-www-form-urlencoded` data format, which percent-decodes keys and
    /// values (`?q=hello%20world` → `hello world`, `+` → space). An absent query string
    /// deserializes the EMPTY input, so an all-optional `T` (or a map) binds to its empty
    /// form and a missing REQUIRED field is a loud [`ErrorKind::ConvertError`] — never a
    /// silent default.
    ///
    /// REPEATED-KEY behavior: the raw `Query<HashMap<String, String>>` map is LAST-WINS
    /// (`?sort=a&sort=b` → `sort = "b"`; a map cannot hold duplicates). A TYPED `Query<T>`
    /// struct does NOT collapse a repeat — `serde_urlencoded` decodes each `key=value` pair
    /// independently and surfaces a repeated key against a struct field as a loud
    /// [`ErrorKind::ConvertError`] (`duplicate field`), and it does NOT collect repeated
    /// keys into a sequence (a `Vec<_>` field over a repeated key likewise fails loudly) —
    /// never a silent partial bind. A handler that needs ALL values of a repeated key reads
    /// the raw last-wins `Query<HashMap>` (or a single delimited value) itself.
    ///
    /// This is the one place leaf-web names a query data FORMAT; the body's format
    /// stays negotiable on the injected [`HttpMessageConverter`], but the query
    /// grammar is fixed by the URL, so there is nothing to negotiate.
    fn deserialize_query(req: &Request) -> Result<T, LeafError> {
        let query = req.query_str().unwrap_or("");
        serde_urlencoded::from_str::<T>(query).map_err(|e| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "query parameters",
                format!("query string `{query}` did not bind: {e}"),
            ))
        })
    }
}

impl FromRequest for Request {
    /// The whole request, copied — the `&Request` extractor (a controller method that wants
    /// raw access). [`Request`] is not `Clone` (its [`Body`](crate::Body) may be a one-shot
    /// stream), so this copies the request's parts plus its BUFFERED body bytes via
    /// [`buffered_clone`](Request::buffered_clone). The body is always `Full` here: the
    /// [`Dispatcher`](crate::Dispatcher) collects a streamed HTTP body before any handler runs.
    fn from_request(req: &Request) -> Result<Self, LeafError> {
        Ok(req.buffered_clone())
    }
}

/// The CONVERTER-AWARE argument-extraction seam the controller codegen calls
/// uniformly, once per handler parameter:
/// `<ParamTy as FromRequestParts>::from_request_parts(req, converter, ctx)`.
///
/// It is the superset of [`FromRequest`]: a parameter type satisfies it EITHER via
/// the request alone (the [`FromRequest`] blanket below — the whole-[`Request`]) OR by
/// reading the per-argument binding [`ExtractCtx`] (the name-dependent [`Path<T>`]) OR
/// by binding the query string ([`Query<T>`] via the `application/x-www-form-urlencoded`
/// data format) OR by riding the injected [`HttpMessageConverter`] (the [`Json<T>`] body
/// deserialization). The codegen dispatches on the parameter's STRUCTURAL extractor type
/// purely through TRAIT resolution (which impl applies), never a spelled type name — so
/// one uniform call site lowers every parameter, `Path<T>` / `Query<T>` / `Json<T>`
/// included.
///
/// leaf-web names the serde DATA MODEL (the `serde::de::DeserializeOwned` bound on the
/// [`Json<T>`]/[`Query<T>`] impls, the same boundary [`erased_serde`] already crosses).
/// For the BODY it names no serde FORMAT — the concrete wire format is the injected
/// converter's (`leaf-serde`'s `JsonConverter` = `serde_json`); for the QUERY, whose
/// grammar is fixed by the URL and not negotiable, it names the one fixed format
/// (`serde_urlencoded`) directly.
///
/// # Errors
///
/// An extractor that cannot produce its value (a missing path param, a malformed
/// body) returns a loud [`LeafError`] — the dispatcher maps it to a 4xx via the
/// advice chain rather than ever silently defaulting.
pub trait FromRequestParts: Sized {
    /// Extract `Self` from `req`, using `converter` for the body-deserializing
    /// extractors ([`Json<T>`]) and `ctx` for the name-dependent ones ([`Path<T>`]);
    /// the request-only extractors ignore both.
    ///
    /// # Errors
    ///
    /// Returns a [`LeafError`] when the request/body does not carry what this
    /// extractor requires (see the trait docs).
    fn from_request_parts(
        req: &Request,
        converter: &dyn HttpMessageConverter,
        ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError>;
}

/// Every request-only NAME-FREE [`FromRequest`] extractor (the whole [`Request`]) ALSO
/// satisfies the converter-aware [`FromRequestParts`] — it just ignores the converter
/// and the binding context. This blanket is why the controller codegen can call ONE
/// uniform `from_request_parts(req, converter, ctx)` per parameter and let trait
/// resolution pick the request-only path, the name-dependent [`Path<T>`] path, the
/// query-binding [`Query<T>`] path, or the converter-backed [`Json<T>`] path.
impl<T: FromRequest> FromRequestParts for T {
    fn from_request_parts(
        req: &Request,
        _converter: &dyn HttpMessageConverter,
        _ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError> {
        T::from_request(req)
    }
}

/// `Path<T>` reads ITS OWN captured `{name}` segment — selected by the handler
/// PARAMETER name carried in the binding [`ExtractCtx`] — and parses it to `T` via
/// [`FromStr`]. This is why a multi-capture route like `/users/{uid}/orders/{oid}`
/// binds EACH `Path` parameter to its own capture (not all to the first): each
/// parameter's `ExtractCtx::name` selects the matching `{name}`. `Path<String>` is
/// the identity parse; `Path<u32>` parses a numeric segment.
///
/// A missing capture (no `{name}` matched on the request) or a parse failure is a
/// loud [`ErrorKind::ConvertError`] the dispatcher maps via the advice chain — never
/// a panic, never a silent default. `T::Err: Display` so the failed parse's own
/// message is carried in the cause.
impl<T> FromRequestParts for Path<T>
where
    T: FromStr,
    T::Err: core::fmt::Display,
{
    fn from_request_parts(
        req: &Request,
        _converter: &dyn HttpMessageConverter,
        ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError> {
        let name = ctx.name().ok_or_else(|| {
            missing(
                "path parameter",
                "the path extractor has no parameter name to select its capture",
            )
        })?;
        let raw = req.path_param(name).ok_or_else(|| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "path parameter",
                format!("no captured path parameter `{{{name}}}` on the request"),
            ))
        })?;
        let value = raw.parse::<T>().map_err(|e| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "path parameter",
                format!("path parameter `{name}` = `{raw}` did not parse: {e}"),
            ))
        })?;
        Ok(Path(value))
    }
}

/// `Header<T>` reads its NAMED request header — the header name carried in the binding
/// [`ExtractCtx`] (from the parameter's `#[header("X-Foo")]` attribute, NOT its
/// `Pat::Ident`, because a header name is not a valid Rust ident) — and parses it to `T`
/// via [`FromStr`]. `Header<String>` is the identity parse; `Header<u32>` parses a numeric
/// header value.
///
/// A missing header name in the context (no `#[header(..)]` attribute), a header absent
/// from the request, or a parse failure is a loud [`ErrorKind::ConvertError`] the
/// dispatcher maps via the advice chain — never a panic, never a silent default.
/// `T::Err: Display` so the failed parse's own message is carried in the cause.
impl<T> FromRequestParts for Header<T>
where
    T: FromStr,
    T::Err: core::fmt::Display,
{
    fn from_request_parts(
        req: &Request,
        _converter: &dyn HttpMessageConverter,
        ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError> {
        let name = ctx.header_name().ok_or_else(|| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "header",
                "the header extractor has no header name to read \
                 (annotate the parameter with `#[header(\"X-Foo\")]`)",
            ))
        })?;
        let raw = req.header(name).ok_or_else(|| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "header",
                format!("no `{name}` header on the request"),
            ))
        })?;
        let value = raw.parse::<T>().map_err(|e| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "header",
                format!("header `{name}` = `{raw}` did not parse: {e}"),
            ))
        })?;
        Ok(Header(value))
    }
}

/// `Extension<T>` reads the typed per-request attribute an upstream
/// [`WebFilter`](crate::WebFilter) attached (via [`Request::insert_extension`]) and
/// returns it cloned. It needs NO name from the [`ExtractCtx`] — it resolves purely by
/// the type `T` through std [`Any`](std::any::Any), so the controller codegen dispatches
/// on the parameter's STRUCTURAL extractor type via plain trait resolution, exactly like
/// every other extractor (no special codegen).
///
/// A missing extension (no value of type `T` on the request) is a loud
/// [`ErrorKind::ConvertError`] the dispatcher maps to a `400` — never a panic, never a
/// silent default. `T: Clone` because the request owns the attribute (an `Arc`-backed
/// store shared across the dispatcher's request clone); the extractor hands the handler
/// its own copy.
impl<T: Clone + Send + Sync + 'static> FromRequestParts for Extension<T> {
    fn from_request_parts(
        req: &Request,
        _converter: &dyn HttpMessageConverter,
        _ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError> {
        req.extension::<T>().cloned().map(Extension).ok_or_else(|| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "request extension",
                "no extension of the requested type was attached to the request \
                 (an upstream WebFilter must `insert_extension` it)",
            ))
        })
    }
}

/// `Json<T>` body extraction: deserialize the request body into `T` through the
/// INJECTED [`HttpMessageConverter`] (the format-agnostic seam). leaf-web names no
/// serde format — it runs [`erased_serde::deserialize`] over the deserializer the
/// converter lends via [`HttpMessageConverter::with_deserializer`], so the concrete
/// format (serde_json, in `leaf-serde`'s converter) stays on the converter's side.
///
/// This is the ONE extractor that genuinely needs the converter, which is exactly
/// why the codegen threads it through [`FromRequestParts`] rather than the
/// request-only [`FromRequest`].
impl<T: serde::de::DeserializeOwned> FromRequestParts for Json<T> {
    fn from_request_parts(
        req: &Request,
        converter: &dyn HttpMessageConverter,
        _ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError> {
        // Capture the typed value out of the converter's scoped `with_deserializer`
        // callback: it lends an erased deserializer over the body, we run
        // `erased_serde::deserialize::<T>` and stash the result.
        let mut slot: Option<T> = None;
        converter.with_deserializer(req.body_bytes(), &mut |de| {
            slot = Some(erased_serde::deserialize::<T>(de).map_err(|e| {
                LeafError::new(ErrorKind::ConvertError)
                    .caused_by(Cause::plain("json body extraction", e.to_string()))
            })?);
            Ok(())
        })?;
        // A successful `with_deserializer` ran the callback to completion (it only
        // returns `Ok(())` after the callback's `Ok(())`, which fills the slot).
        slot.map(Json).ok_or_else(|| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "json body extraction",
                "the converter did not run the read callback",
            ))
        })
    }
}

/// `Query<T>` query-string extraction: deserialize the request's query into an
/// arbitrary `Deserialize` target `T` via the `application/x-www-form-urlencoded`
/// data format (`?page=2&size=10` → `Pagination { page: 2, size: 10 }`), which
/// percent-decodes keys+values and treats a repeated key as a loud
/// [`ErrorKind::ConvertError`] (`serde_urlencoded` does not collect repeated keys
/// into a sequence).
///
/// This is the TYPED counterpart to the raw `FromRequest for Query<HashMap<String,
/// String>>` map: `Query<HashMap<String, String>>` rides THIS impl too (a `HashMap`
/// is `DeserializeOwned`), so the raw-map and typed paths share one decoding route.
/// Unlike the body's negotiable wire format (the injected [`HttpMessageConverter`]),
/// the query grammar is fixed by the URL, so leaf-web names the query format directly
/// here — a wire DATA FORMAT, never an HTTP-server backend.
///
/// A missing REQUIRED field (or a value that does not parse to its field type) is a
/// loud [`ErrorKind::ConvertError`] the dispatcher maps via the advice chain — never
/// a silent default.
impl<T: serde::de::DeserializeOwned> FromRequestParts for Query<T> {
    fn from_request_parts(
        req: &Request,
        _converter: &dyn HttpMessageConverter,
        _ctx: &ExtractCtx<'_>,
    ) -> Result<Self, LeafError> {
        Query::<T>::deserialize_query(req).map(Query)
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
    use std::collections::HashMap;

    fn request(uri: &str, params: Vec<(String, String)>) -> Request {
        let mut req =
            Request::new(Method::GET, uri.parse().expect("uri parses"), http::HeaderMap::new(), Bytes::new());
        req.set_path_params(params);
        req
    }

    /// Bind `Query<HashMap<String, String>>` through the SAME `FromRequestParts` seam
    /// the controller codegen calls — `HashMap` is just one `T: DeserializeOwned`.
    fn query_map(req: &Request) -> HashMap<String, String> {
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("q");
        let Query(map) = <Query<HashMap<String, String>> as FromRequestParts>::from_request_parts(
            req, &conv, &ctx,
        )
        .expect("the query map binds");
        map
    }

    #[test]
    fn query_extractor_parses_the_query_into_a_map() {
        let req = request("/search?page=2&size=10", vec![]);
        let map = query_map(&req);
        assert_eq!(map.get("page").map(String::as_str), Some("2"));
        assert_eq!(map.get("size").map(String::as_str), Some("10"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn query_extractor_with_no_query_is_an_empty_map() {
        let req = request("/search", vec![]);
        let map = query_map(&req);
        assert!(map.is_empty());
    }

    #[test]
    fn query_map_percent_decodes_keys_and_values() {
        // `?q=hello%20world` must deliver the DECODED "hello world", not the raw
        // `hello%20world` (the gap T1b fixes). Keys are decoded too, and `+` is a space.
        let req = request("/search?q=hello%20world&full+name=Jane%20Doe", vec![]);
        let map = query_map(&req);
        assert_eq!(map.get("q").map(String::as_str), Some("hello world"));
        assert_eq!(map.get("full name").map(String::as_str), Some("Jane Doe"));
    }

    #[test]
    fn typed_query_struct_binds_via_form_urlencoded() {
        // `Query<Pagination{page,size}>` must compile and deserialize from the query
        // string via form-urlencoded — the gap T1b fixes (only `Query<HashMap>` worked).
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Pagination {
            page: u32,
            size: u32,
        }
        let req = request("/search?page=2&size=10", vec![]);
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("pagination");
        let Query(p) = <Query<Pagination> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect("the typed query binds via form-urlencoded");
        assert_eq!(p, Pagination { page: 2, size: 10 });
    }

    #[test]
    fn typed_query_struct_percent_decodes_values() {
        // The typed path rides serde_urlencoded, which percent-decodes for free.
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Search {
            q: String,
        }
        let req = request("/search?q=hello%20world", vec![]);
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("search");
        let Query(s) = <Query<Search> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect("the typed query binds and decodes");
        assert_eq!(s, Search { q: "hello world".to_string() });
    }

    #[test]
    fn typed_query_with_no_query_against_required_field_is_a_loud_error() {
        // A required field absent from an empty query is a loud ConvertError, not a default.
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Required {
            page: u32,
        }
        let req = request("/search", vec![]);
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("required");
        let err = <Query<Required> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect_err("a missing required query field must surface a LeafError, not a default");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn query_map_repeated_key_is_last_wins() {
        // DOCUMENTED behavior for the raw `Query<HashMap>` map: a repeated key collapses
        // to LAST-WINS (`?sort=asc&sort=desc` → `sort = "desc"`) — a map cannot hold
        // duplicates. A handler that needs ALL values of a repeated key reads them off the
        // map (or a single delimited value) itself.
        let req = request("/items?sort=asc&sort=desc", vec![]);
        let map = query_map(&req);
        assert_eq!(map.get("sort").map(String::as_str), Some("desc"));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn typed_query_repeated_key_against_scalar_field_is_a_loud_error() {
        // DOCUMENTED behavior for a TYPED `Query<T>` struct: `serde_urlencoded` does NOT
        // collapse a repeated key — a repeated `?sort=asc&sort=desc` against a single
        // `sort: String` field is a loud `ConvertError` (duplicate field), never a silent
        // last-wins bind. (For all-values semantics use the raw `Query<HashMap>` last-wins
        // map or a single delimited value; `serde_urlencoded` does not collect into a
        // `Vec`, so a `Vec<_>` field over a repeated key likewise fails loudly.)
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Sort {
            #[allow(dead_code)]
            sort: String,
        }
        let req = request("/items?sort=asc&sort=desc", vec![]);
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("sort");
        let err = <Query<Sort> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect_err("a repeated key against a scalar struct field is a loud ConvertError");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn request_extractor_clones_the_whole_request() {
        let req = request("/p/7?x=1", vec![("id".to_string(), "7".to_string())]);
        let whole = Request::from_request(&req).expect("clones the request");
        assert_eq!(whole.path(), "/p/7");
        assert_eq!(whole.query_str(), Some("x=1"));
        assert_eq!(whole.path_param("id"), Some("7"));
    }

    // ── FromRequestParts (the converter-aware extraction seam, Task 14) ──────────

    use crate::content::HttpMessageConverter;

    /// A tiny `HttpMessageConverter` test double over JSON-ish bytes: it deserializes
    /// the body via a JSON deserializer it owns on the stack (the same `with_deserializer`
    /// lend-a-scoped-deserializer shape the real `leaf-serde` converter has), so the
    /// converter-aware `Json<T>` extraction can be proven IN leaf-web with no leaf-serde.
    struct TestJsonConverter;

    impl HttpMessageConverter for TestJsonConverter {
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

    fn request_with_body(body: &[u8]) -> Request {
        Request::new(Method::POST, "/orders".parse().expect("uri"), http::HeaderMap::new(), Bytes::copy_from_slice(body))
    }

    #[derive(serde::Deserialize, PartialEq, Debug)]
    struct NewOrder {
        sku: String,
        qty: u32,
    }

    #[test]
    fn json_body_extracts_through_the_converter() {
        // `Json<T>` is the converter-backed extraction: `FromRequestParts` hands the
        // injected converter the body and gets back the typed `T` (NEVER a plain
        // `FromRequest` — leaf-web names no serde FORMAT; the format rides the converter).
        let req = request_with_body(br#"{"sku":"COFFEE","qty":2}"#);
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("body");
        let Json(order) =
            <Json<NewOrder> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
                .expect("the JSON body deserializes through the converter");
        assert_eq!(order, NewOrder { sku: "COFFEE".to_string(), qty: 2 });
    }

    #[test]
    fn json_body_malformed_is_a_loud_error() {
        let req = request_with_body(b"{ not json ");
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("body");
        let err = <Json<NewOrder> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect_err("a malformed JSON body must surface a LeafError, not a default");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn from_request_parts_blanket_falls_back_to_from_request() {
        // The plain `FromRequest` whole-`&Request` extractor ALSO satisfies the
        // converter-aware `FromRequestParts` via the blanket impl, so the controller
        // codegen calls ONE uniform `from_request_parts(req, converter, ctx)` per parameter
        // (trait dispatch on the parameter's structural extractor type, never a name).
        let req = request("/search?page=2", vec![("id".to_string(), "9".to_string())]);
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::named("req");
        let whole = <Request as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect("&Request rides the FromRequest blanket through FromRequestParts");
        assert_eq!(whole.path(), "/search");
        assert_eq!(whole.query_str(), Some("page=2"));
        assert_eq!(whole.path_param("id"), Some("9"));
    }

    // ── Path<T> reads its OWN capture BY NAME via the ExtractCtx (Task T1a) ───────────

    #[test]
    fn path_binds_each_param_to_its_own_named_capture_not_the_first() {
        // The regression this task fixes: a route `/users/{uid}/orders/{oid}` with two
        // path params must bind EACH to ITS OWN named capture — `uid` is the first, `oid`
        // the second. The old `path_params().first()` impl bound BOTH to the first.
        let req = request(
            "/users/7/orders/42",
            vec![("uid".to_string(), "7".to_string()), ("oid".to_string(), "42".to_string())],
        );
        let conv = TestJsonConverter;

        // `uid: Path<u64>` reads the FIRST capture, parsed as u64.
        let Path(uid) = <Path<u64> as FromRequestParts>::from_request_parts(
            &req,
            &conv,
            &ExtractCtx::named("uid"),
        )
        .expect("uid reads its own named capture");
        assert_eq!(uid, 7u64);

        // `oid: Path<String>` reads the SECOND capture by its own name — NOT the first.
        let Path(oid) = <Path<String> as FromRequestParts>::from_request_parts(
            &req,
            &conv,
            &ExtractCtx::named("oid"),
        )
        .expect("oid reads its own named capture");
        assert_eq!(oid, "42", "the second param must NOT bind to the first capture");
    }

    #[test]
    fn path_parses_a_typed_segment_and_a_bad_parse_is_a_loud_convert_error() {
        // `Path<u32>` parses a numeric segment; a non-numeric segment is a mapped
        // `ConvertError`, NOT a panic.
        let ok = request("/items/123", vec![("id".to_string(), "123".to_string())]);
        let conv = TestJsonConverter;
        let Path(id) = <Path<u32> as FromRequestParts>::from_request_parts(
            &ok,
            &conv,
            &ExtractCtx::named("id"),
        )
        .expect("a numeric segment parses to u32");
        assert_eq!(id, 123u32);

        let bad = request("/items/abc", vec![("id".to_string(), "abc".to_string())]);
        let err = <Path<u32> as FromRequestParts>::from_request_parts(
            &bad,
            &conv,
            &ExtractCtx::named("id"),
        )
        .expect_err("a non-numeric segment must surface a LeafError, not panic");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn path_with_no_matching_capture_is_a_loud_error() {
        // No `{sku}` capture on the request → a loud ConvertError, never a silent default.
        let req = request("/products", vec![]);
        let conv = TestJsonConverter;
        let err = <Path<String> as FromRequestParts>::from_request_parts(
            &req,
            &conv,
            &ExtractCtx::named("sku"),
        )
        .expect_err("a missing named capture must surface a LeafError, not a default");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    // ── Header<T> reads its NAMED header via the ExtractCtx header name (Task T1c) ─────

    /// Build a request carrying one header, so the named-header extractor has something
    /// to read.
    fn request_with_header(name: &'static str, value: &'static str) -> Request {
        let mut headers = http::HeaderMap::new();
        headers.insert(name, http::HeaderValue::from_static(value));
        Request::new(Method::GET, "/h".parse().expect("uri"), headers, Bytes::new())
    }

    #[test]
    fn header_extractor_reads_the_named_header() {
        // `Header<String>` reads the header NAME the codegen carries in the ExtractCtx
        // (`#[header("X-Api-Key")]`), parses it via FromStr, and returns the value. The
        // header name is NOT a Rust ident, so it rides the binding context, not the param
        // name.
        let req = request_with_header("X-Api-Key", "s3cr3t");
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::for_header("k", "X-Api-Key");
        let Header(k) = <Header<String> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect("the named header binds");
        assert_eq!(k, "s3cr3t");
    }

    #[test]
    fn header_extractor_parses_a_typed_value() {
        // `Header<u32>` parses the header value via FromStr (the identity case is
        // `Header<String>`); a numeric header value parses to u32.
        let req = request_with_header("X-Count", "42");
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::for_header("count", "X-Count");
        let Header(n) = <Header<u32> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect("the numeric header parses to u32");
        assert_eq!(n, 42u32);
    }

    #[test]
    fn header_extractor_missing_header_is_a_loud_error() {
        // A missing header (the request does not carry `X-Api-Key`) is a loud
        // ConvertError the dispatcher maps to 400 — never a silent default.
        let req = request_with_header("X-Other", "v");
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::for_header("k", "X-Api-Key");
        let err = <Header<String> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect_err("a missing required header must surface a LeafError, not a default");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn header_extractor_bad_parse_is_a_loud_error() {
        // A present-but-unparseable header value (`abc` against `Header<u32>`) is a loud
        // ConvertError, never a panic.
        let req = request_with_header("X-Count", "abc");
        let conv = TestJsonConverter;
        let ctx = ExtractCtx::for_header("count", "X-Count");
        let err = <Header<u32> as FromRequestParts>::from_request_parts(&req, &conv, &ctx)
            .expect_err("a non-numeric header value must surface a LeafError, not panic");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }

    #[test]
    fn header_extractor_with_no_header_name_in_context_is_a_loud_error() {
        // Without a header name in the binding context (the codegen had no `#[header(..)]`
        // attr to give), the extractor fails loudly rather than guessing.
        let req = request_with_header("X-Api-Key", "v");
        let conv = TestJsonConverter;
        let err = <Header<String> as FromRequestParts>::from_request_parts(
            &req,
            &conv,
            &ExtractCtx::named("k"),
        )
        .expect_err("a header extractor with no header name must fail loudly");
        assert_eq!(err.kind, ErrorKind::ConvertError);
    }
}
