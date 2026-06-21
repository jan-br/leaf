//! The controller-impl ITERATOR (Task 9): lower each request-mapping method of a
//! `#[controller]`/`#[rest_controller]` `impl` block into ONE generated `Route` bean
//! (configuration-classes phase3/05 / the `#[advisable] impl` family — an attr on a
//! single method cannot emit a sibling registration row, so the impl block is
//! processed as a unit).
//!
//! ## What it emits, per mapped method
//!
//! `#[rest_controller] impl Api { #[get("/products/{sku}")] async fn get(&self, sku:
//! Path<String>) -> Result<ProductDto, LeafError> { .. } }` lowers the `get` method to:
//!
//! - a `#[doc(hidden)]` generated `Route` STRUCT (`__LeafRoute_Api_get`) holding the
//!   DI'd `controller: Ref<Api>` (field injection) plus — for a `#[rest_controller]`
//!   (the `@ResponseBody` policy) — the injected `converter: Ref<dyn
//!   ::leaf_web::HttpMessageConverter>` used to serialize the return,
//! - its `impl ::leaf_web::Route` (`method()` = the mapping verb, `path()` = the
//!   pattern, `handler()` = `self`),
//! - its `impl ::leaf_web::Handler` whose `handle` resolves EACH parameter via its
//!   `FromRequest` extractor (`<ParamTy as ::leaf_web::FromRequest>::from_request` —
//!   trait dispatch on the parameter's STRUCTURAL extractor type, NEVER a spelled
//!   name), invokes `self.controller.<method>(args).await`, and applies the return
//!   policy (`#[rest_controller]` → serialize via the converter; `#[controller]` →
//!   `IntoResponse`),
//! - the `#[component]`-equivalent bean registration (one const `::leaf_core::Descriptor`
//!   into `COMPONENTS`, via the SAME [`crate::descriptor::emit`] currency the
//!   stereotypes use) that `provides` the `dyn ::leaf_web::Route` view, so the server's
//!   `Vec<Ref<dyn Route>>` collection injection finds it.
//!
//! The controller bean itself stays a plain `#[rest_controller]`/`#[controller]`
//! stereotype (the struct macro registered it); this iterator only contributes the
//! per-method `Route` beans. The mapping attrs (`#[get]`/`#[post]`/…) are STRIPPED
//! from the re-emitted impl by the thin macro (the impl-block macro owns the lowering).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ImplItem, ImplItemFn, ItemImpl, ItemStruct, Lit, Meta, Type};

use crate::descriptor::{self, BeanInput, Dependency, EmitError, FieldShape, Scope, ServiceView, Slice};
use crate::stereotype::{self, Stereotype};

/// The mapping-attr names a method must carry to be lowered as a request-mapping
/// handler (the verb-specific sugar + the general `route` form).
const MAPPING_ATTRS: &[&str] = &["get", "post", "put", "delete", "patch", "head", "route"];

/// A request-mapping method: the verb-constant token + the path pattern + the method.
struct MappedMethod<'a> {
    method: &'a ImplItemFn,
    /// The `::leaf_web::http::Method` constant token for the verb (`GET`/`POST`/…).
    verb: TokenStream,
    /// The path PATTERN literal (e.g. `"/products/{sku}"`).
    path: String,
}

/// Lower a `#[controller]`/`#[rest_controller]` `impl` block to its per-method `Route`
/// beans (one const `Descriptor` + the generated `Route`/`Handler` impls per mapped
/// method, through the SAME [`descriptor::emit`] currency the stereotypes use).
///
/// `response_body` is the `@ResponseBody` policy axis: `true` for `#[rest_controller]`
/// (the return is serialized through the injected `HttpMessageConverter`), `false` for
/// `#[controller]` (the return is an `IntoResponse`).
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) when the impl is generic / a trait impl, a mapping
/// attr is malformed, or a mapped method takes no `self` receiver.
pub fn expand_controller_impl(
    item: &ItemImpl,
    response_body: bool,
) -> Result<TokenStream, EmitError> {
    let self_ty = self_ty_of(item)?;
    let controller_ident = type_ident(&self_ty);
    if !item.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{controller_ident}` is a generic `#[controller]`/`#[rest_controller]` \
                 impl: a generic controller has no single concrete type to mint its \
                 per-method `Route` beans. Make the controller concrete."
            ),
        });
    }

    let mut rows = TokenStream::new();
    for inner in &item.items {
        let ImplItem::Fn(func) = inner else { continue };
        let Some(mapped) = mapping_of(func)? else { continue };
        rows.extend(emit_route_bean(&self_ty, &controller_ident, &mapped, response_body)?);
    }
    // The dual-form mismatch guard: this request-mapping impl's @ResponseBody policy
    // (`response_body`) MUST equal the one the controller STRUCT's stereotype declared via
    // its `ControllerKind` marker. So `#[rest_controller] struct` + `#[controller] impl`
    // (or the reverse) is a hard compile error rather than a silent policy disagreement,
    // and a request-mapping impl on a struct never annotated as a controller fails the
    // `ControllerKind` bound. Keyed on the trait/const — never a spelled type name.
    rows.extend(controller_kind_guard(&self_ty, response_body));
    Ok(rows)
}

/// Emit the controller STRUCT's `::leaf_web::ControllerKind` marker carrying its
/// `@ResponseBody` policy (`#[rest_controller]` → `true`, `#[controller]` → `false`).
/// Emitted ALONGSIDE the stereotype rows on the controller struct so the matching
/// request-mapping impl can assert agreement (see `controller_kind_guard`).
pub fn emit_controller_kind(item: &ItemStruct, response_body: bool) -> TokenStream {
    let ident = &item.ident;
    let (impl_g, ty_g, where_c) = item.generics.split_for_impl();
    quote! {
        #[automatically_derived]
        #[doc(hidden)]
        impl #impl_g ::leaf_web::ControllerKind for #ident #ty_g #where_c {
            const RESPONSE_BODY: bool = #response_body;
        }
    }
}

/// The compile-time guard a request-mapping impl emits: assert the controller struct's
/// declared `@ResponseBody` policy equals this impl's, turning a struct/impl stereotype
/// mismatch into a clear `compile_error`. See [`emit_controller_kind`].
fn controller_kind_guard(self_ty: &Type, response_body: bool) -> TokenStream {
    quote! {
        const _: () = {
            ::core::assert!(
                <#self_ty as ::leaf_web::ControllerKind>::RESPONSE_BODY == #response_body,
                "controller stereotype mismatch: this `impl` block and the controller struct \
                 declare different @ResponseBody policies. Use the SAME stereotype on both — \
                 `#[rest_controller]` on the struct AND the impl, or `#[controller]` on both."
            );
        };
    }
}

/// Emit ONE generated `Route` bean for a mapped method: the `#[doc(hidden)]` route
/// struct (holding the DI'd controller + — for `@ResponseBody` — the converter), its
/// `Route` + `Handler` trait impls, and the `#[component]`-equivalent const
/// registration that `provides` the `dyn ::leaf_web::Route` view.
fn emit_route_bean(
    self_ty: &Type,
    controller_ident: &str,
    mapped: &MappedMethod<'_>,
    response_body: bool,
) -> Result<TokenStream, EmitError> {
    let method = mapped.method;
    let method_ident = &method.sig.ident;

    if !has_self_receiver(method) {
        return Err(EmitError {
            message: format!(
                "`{controller_ident}::{method_ident}` is a request-mapping method but \
                 takes no `self` receiver: a handler method threads the controller bean \
                 through `&self`."
            ),
        });
    }

    // The generated route struct: `__LeafRoute_<Controller>_<method>`. Unique per
    // (controller, method) so two mappings in one module never collide.
    let route_struct_ident = format_ident!("__LeafRoute_{controller_ident}_{method_ident}");

    // The handler's argument resolution: each NON-receiver parameter resolves via its
    // CONVERTER-AWARE `FromRequestParts` extractor — `<ParamTy as
    // ::leaf_web::FromRequestParts>::from_request_parts(req, converter, ctx)` (TRAIT
    // dispatch on the parameter's STRUCTURAL extractor type, never a spelled name). One
    // uniform call site lowers EVERY parameter: the request-only `&Request` extractor
    // rides the `FromRequest` blanket (ignoring both), a `Query<T>` parameter binds the
    // query string via its own `FromRequestParts` impl, the name-dependent `Path<T>` reads
    // the parameter NAME off the threaded binding context, and a `Json<T>` body parameter
    // rides the injected `HttpMessageConverter` — trait resolution, not the macro, picks
    // which.
    //
    // The per-argument `ExtractCtx` carries the handler PARAMETER NAME (the `Pat::Ident`
    // already on the signature) so the `Path<T>` extractor selects ITS OWN `{name}`
    // capture (a multi-capture route binds each `Path` to its own segment, not all to the
    // first). The context is threaded UNIFORMLY to every extractor; the macro never
    // branches on the parameter being a `Path`. A destructured / unnamed parameter yields
    // an empty context (name-dependent extractors then fail loudly rather than guess).
    // The resolved bindings pass positionally to the controller method.
    let args = non_receiver_args(method)?;
    let arg_locals: Vec<syn::Ident> =
        (0..args.len()).map(|i| format_ident!("__arg{i}")).collect();
    let arg_resolves = args.iter().zip(&arg_locals).enumerate().map(|(i, (arg, local))| {
        let ty = &arg.ty;
        let ctx_local = format_ident!("__ctx{i}");
        // The per-argument binding context. A `#[header("X-Foo")]` parameter carries its
        // HTTP HEADER NAME (which is not a valid Rust ident, so it cannot ride the
        // parameter's `Pat::Ident`) — the header-aware context the `Header<T>` extractor
        // reads. Every other parameter carries just its NAME (the `Path<T>` capture key) or
        // an empty context (a destructured / wildcard parameter). The macro picks the
        // CONSTRUCTOR by the presence of the attribute, NOT by the parameter's type — the
        // dispatch stays the uniform structural `FromRequestParts` seam.
        let ctx_init = match (&arg.name, &arg.header_name) {
            (Some(name), Some(header)) => {
                quote! { ::leaf_web::ExtractCtx::for_header(#name, #header) }
            }
            (Some(name), None) => quote! { ::leaf_web::ExtractCtx::named(#name) },
            (None, _) => quote! { ::leaf_web::ExtractCtx::empty() },
        };
        quote! {
            let #ctx_local = #ctx_init;
            let #local =
                <#ty as ::leaf_web::FromRequestParts>::from_request_parts(__req, __converter, &#ctx_local)?;
        }
    });

    // The verb + path pattern the `Route` impl reports.
    let verb = &mapped.verb;
    let path = &mapped.path;

    // The return policy: a `#[rest_controller]` (@ResponseBody) serializes the awaited
    // return through the injected converter into a JSON body; a plain `#[controller]`
    // converts the return via `IntoResponse`. A handler return is `Result<T, LeafError>`
    // — `?` propagates an `Err` so the dispatcher's advice chain maps it.
    let invoke = quote! { self.controller.#method_ident( #(#arg_locals),* ).await };
    let return_policy = if response_body {
        quote! {
            let __value = #invoke?;
            // The @ResponseBody return policy is the uniform `IntoResponseWith` trait,
            // DRIVEN by the injected `dyn ::leaf_web::HttpMessageConverter` (the SAME
            // converter the `Json<T>` arg extraction rode). One STRUCTURAL call site
            // covers BOTH a bare serializable value (→ 200 + the converter's content-type
            // + serialized body) AND a `::leaf_web::ResponseEntity<T>` (→ its status +
            // headers + serialized body) — the trait's two impls pick, never the macro by
            // a spelled return-type name.
            ::leaf_web::IntoResponseWith::into_response_with(__value, __converter)
        }
    } else {
        quote! {
            let __value = #invoke;
            ::core::result::Result::Ok(::leaf_web::IntoResponse::into_response(__value))
        }
    };

    // The struct's injected fields (field injection through `Injectable`): the controller
    // bean AND the `HttpMessageConverter`. The converter is injected for BOTH stereotypes
    // — a `Json<T>` body parameter rides it for EXTRACTION regardless of the return policy
    // (only `#[rest_controller]` additionally uses it to SERIALIZE the return). The
    // generated `handle` binds it once up front so the uniform `from_request_parts` arg
    // loop and the rest-controller return policy share the one `&dyn` view. `&*self.converter`
    // derefs `Ref<dyn _>` to the trait object.
    let deps = vec![
        Dependency {
            name: "controller".into(),
            ty: parse_type(&format!("::leaf_core::Ref<{}>", quote!(#self_ty)))?,
        },
        Dependency {
            name: "converter".into(),
            ty: parse_type("::leaf_core::Ref<dyn ::leaf_web::HttpMessageConverter>")?,
        },
    ];
    let converter_field = quote! { converter: ::leaf_core::Ref<dyn ::leaf_web::HttpMessageConverter>, };

    // Bind the injected converter as a `&dyn` view ONCE at the top of `handle` — used by
    // the uniform `from_request_parts` arg loop (body extraction) and the
    // `#[rest_controller]` return policy (serialize). When a handler has NO parameters AND
    // is a plain `#[controller]` (so the converter is genuinely unused), bind it to `_` so
    // the field stays injected (uniform struct shape) without tripping the unused-var lint.
    let converter_binding = if args.is_empty() && !response_body {
        quote! { let _ = &*self.converter; }
    } else {
        quote! { let __converter: &dyn ::leaf_web::HttpMessageConverter = &*self.converter; }
    };

    // The generated struct definition + its `Route`/`Handler` trait impls. The struct
    // is named in non-camel-case (the `__LeafRoute_*` convention) — emit the rust-
    // analyzer-parity allow so the generated item carries no lint.
    let route_struct_ty: Type = parse_type(&route_struct_ident.to_string())?;
    let items = quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        pub struct #route_struct_ident {
            controller: ::leaf_core::Ref<#self_ty>,
            #converter_field
        }

        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        impl ::leaf_web::Route for #route_struct_ident {
            fn method(&self) -> ::leaf_web::http::Method {
                #verb
            }
            fn path(&self) -> &str {
                #path
            }
            fn handler(&self) -> &dyn ::leaf_web::Handler {
                self
            }
        }

        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        impl ::leaf_web::Handler for #route_struct_ident {
            fn handle<'__a>(
                &'__a self,
                __req: &'__a ::leaf_web::Request,
            ) -> ::leaf_core::BoxFuture<
                '__a,
                ::core::result::Result<::leaf_web::Response, ::leaf_core::LeafError>,
            > {
                ::std::boxed::Box::pin(async move {
                    #converter_binding
                    #(#arg_resolves)*
                    #return_policy
                })
            }
        }
    };

    // The `#[component]`-equivalent bean registration: one const `Descriptor` into
    // COMPONENTS providing the `dyn ::leaf_web::Route` view, with the struct's fields
    // FIELD-injected through `Injectable` (the SAME descriptor currency + field-default
    // construction the stereotypes use — NOT a hand-written Provider).
    let meta = crate::annotation::resolve(&Stereotype::Component.annotation())
        .map_err(|e| EmitError { message: e.to_string() })?;
    let mut input =
        BeanInput::new(route_struct_ty, route_struct_ident.to_string(), route_struct_ident.to_string());
    input.module_qualified = true;
    input.scope = Scope::Singleton;
    input.meta = meta;
    input.slice = Slice::Components;
    input.deps = deps;
    input.inject_via_trait = true;
    input.field_shape = FieldShape::Named;
    input.provides = vec![ServiceView { dyn_ty: parse_type("dyn ::leaf_web::Route")? }];
    let registration = descriptor::emit(&input)?;

    Ok(quote! { #items #registration })
}

/// Whether a method carries a request-mapping attribute and, if so, its verb + path.
/// Returns `Ok(None)` for a plain (non-mapping) method.
///
/// # Errors
/// [`EmitError`] on a malformed mapping attribute (a verb attr without a path string, a
/// `route(..)` without `method`/`path`, an unknown verb).
fn mapping_of(func: &ImplItemFn) -> Result<Option<MappedMethod<'_>>, EmitError> {
    let Some(attr) = func.attrs.iter().find_map(|a| {
        a.path()
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .filter(|n| MAPPING_ATTRS.contains(&n.as_str()))
            .map(|n| (n, a))
    }) else {
        return Ok(None);
    };
    let (name, attr) = attr;
    let method_ident = func.sig.ident.to_string();
    if name == "route" {
        let (verb, path) = parse_route_attr(attr, &method_ident)?;
        return Ok(Some(MappedMethod { method: func, verb, path }));
    }
    // A verb-specific attr: `#[get("/path")]` — one string-literal path argument.
    let path = parse_verb_path(attr, &name, &method_ident)?;
    let verb = verb_token(&name);
    Ok(Some(MappedMethod { method: func, verb, path }))
}

/// The `::leaf_web::http::Method::<VERB>` constant token for a verb-specific mapping
/// attr name (through the `leaf_web` facade re-export of `http`, so an umbrella-only app
/// reaches it via the one `leaf_web` alias — never needing `http` as a direct dep).
fn verb_token(name: &str) -> TokenStream {
    let verb = format_ident!("{}", name.to_uppercase());
    quote! { ::leaf_web::http::Method::#verb }
}

/// Parse a verb-specific attr's single string-literal path (`#[get("/x")]` → `/x`).
///
/// # Errors
/// [`EmitError`] when the attr carries no string-literal path argument.
fn parse_verb_path(
    attr: &syn::Attribute,
    verb: &str,
    method_ident: &str,
) -> Result<String, EmitError> {
    let lit: syn::LitStr = attr.parse_args().map_err(|e| EmitError {
        message: format!(
            "`#[{verb}(..)]` on `{method_ident}` needs a single string path argument \
             (e.g. `#[{verb}(\"/path\")]`): {e}"
        ),
    })?;
    Ok(lit.value())
}

/// Parse the general `#[route(method = "PUT", path = "/x")]` form into a verb token +
/// the path pattern.
///
/// # Errors
/// [`EmitError`] when `method`/`path` is missing, not a string, or the method names no
/// valid HTTP verb.
fn parse_route_attr(
    attr: &syn::Attribute,
    method_ident: &str,
) -> Result<(TokenStream, String), EmitError> {
    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let metas = attr.parse_args_with(parser).map_err(|e| EmitError {
        message: format!("malformed `#[route(..)]` on `{method_ident}`: {e}"),
    })?;
    let mut verb_str = None;
    let mut path = None;
    for meta in metas {
        let Meta::NameValue(nv) = meta else {
            return Err(EmitError {
                message: format!(
                    "`#[route(..)]` on `{method_ident}` takes `method = \"..\", path = \"..\"`"
                ),
            });
        };
        let key = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
        let value = match &nv.value {
            syn::Expr::Lit(syn::ExprLit { lit: Lit::Str(s), .. }) => s.value(),
            _ => {
                return Err(EmitError {
                    message: format!("`{key}` on `{method_ident}`'s `#[route]` must be a string"),
                });
            }
        };
        match key.as_str() {
            "method" => verb_str = Some(value),
            "path" => path = Some(value),
            other => {
                return Err(EmitError {
                    message: format!(
                        "unknown `#[route]` argument `{other}` on `{method_ident}` \
                         (expected `method`/`path`)"
                    ),
                });
            }
        }
    }
    let verb_str = verb_str.ok_or_else(|| EmitError {
        message: format!("`#[route(..)]` on `{method_ident}` is missing `method = \"..\"`"),
    })?;
    let path = path.ok_or_else(|| EmitError {
        message: format!("`#[route(..)]` on `{method_ident}` is missing `path = \"..\"`"),
    })?;
    // Build the verb token from the declared method string via the `http::Method` parse
    // (so an arbitrary verb is supported); the const initializer parses it at use site.
    let verb_upper = verb_str.to_uppercase();
    let verb = format_ident!("{verb_upper}");
    // Only the standard verbs are const associated items on `http::Method`; for those we
    // emit the const directly. (The verb-specific attrs only ever produce standard verbs.)
    let token = match verb_upper.as_str() {
        "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD" | "OPTIONS" | "TRACE"
        | "CONNECT" => quote! { ::leaf_web::http::Method::#verb },
        other => {
            return Err(EmitError {
                message: format!(
                    "`#[route(method = \"{other}\")]` on `{method_ident}` names an unknown \
                     HTTP method (expected a standard verb like GET/POST/PUT/DELETE)"
                ),
            });
        }
    };
    Ok((token, path))
}

/// `true` iff the method takes a `self`/`&self`/`&mut self` receiver.
fn has_self_receiver(func: &ImplItemFn) -> bool {
    func.sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_)))
}

/// One NON-receiver handler parameter: its extractor TYPE (used verbatim so the trait
/// dispatch is purely structural, never a name match) + its parameter NAME (the
/// `Pat::Ident`, if it has one — threaded into the extractor's binding [`ExtractCtx`] so
/// the name-dependent `Path<T>` extractor selects its own `{name}` capture) + an OPTIONAL
/// HTTP header NAME (from a `#[header("X-Foo")]` parameter attribute — a header name is
/// not a valid Rust ident, so it rides this attribute instead of the parameter name, and
/// the `Header<T>` extractor reads it off the binding context).
struct HandlerArg {
    /// The parameter's extractor type, used VERBATIM (structural trait dispatch).
    ty: Type,
    /// The parameter's name (`Pat::Ident`), or `None` for a destructured / wildcard pat.
    name: Option<String>,
    /// The HTTP header name from a `#[header("X-Foo")]` parameter attribute, or `None`.
    header_name: Option<String>,
}

/// The parameter-attribute name that carries a `Header<T>` extractor's HTTP header name.
const HEADER_ATTR: &str = "header";

/// The NON-receiver arguments of a method, in order — each is one handler parameter
/// resolved via its `FromRequestParts` extractor. The type is used VERBATIM (purely
/// structural dispatch); the name (if the parameter is a plain `Pat::Ident`) rides into
/// the per-argument binding context, and a `#[header("X-Foo")]` parameter attribute
/// supplies the header name for a `Header<T>` extractor.
///
/// # Errors
/// [`EmitError`] when a `#[header(..)]` parameter attribute is malformed (not a single
/// string-literal header name).
fn non_receiver_args(func: &ImplItemFn) -> Result<Vec<HandlerArg>, EmitError> {
    func.sig
        .inputs
        .iter()
        .filter_map(|a| match a {
            FnArg::Typed(pat_ty) => {
                let name = match &*pat_ty.pat {
                    syn::Pat::Ident(pi) => Some(pi.ident.to_string()),
                    _ => None,
                };
                let header_name = match header_attr_value(&pat_ty.attrs) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                Some(Ok(HandlerArg { ty: (*pat_ty.ty).clone(), name, header_name }))
            }
            FnArg::Receiver(_) => None,
        })
        .collect()
}

/// The HTTP header name carried by a parameter's `#[header("X-Foo")]` attribute, if it has
/// one (matched on the attribute path's LAST segment so a `#[leaf::header(..)]`-qualified
/// form is recognised too). `None` when no `#[header(..)]` attribute is present.
///
/// # Errors
/// [`EmitError`] when the `#[header(..)]` attribute carries no single string-literal header
/// name (e.g. `#[header]` or `#[header(X-Foo)]`).
fn header_attr_value(attrs: &[syn::Attribute]) -> Result<Option<String>, EmitError> {
    let Some(attr) = attrs
        .iter()
        .find(|a| a.path().segments.last().is_some_and(|s| s.ident == HEADER_ATTR))
    else {
        return Ok(None);
    };
    let lit: syn::LitStr = attr.parse_args().map_err(|e| EmitError {
        message: format!(
            "`#[{HEADER_ATTR}(..)]` needs a single string header-name argument \
             (e.g. `#[{HEADER_ATTR}(\"X-Tenant-Id\")]`): {e}"
        ),
    })?;
    Ok(Some(lit.value()))
}

/// The NON-receiver argument count of a method — the structural ARITY the
/// `#[control_advice]` impl dispatches on (the error alone vs. error + request).
fn non_receiver_arg_count(func: &ImplItemFn) -> usize {
    func.sig.inputs.iter().filter(|a| matches!(a, FnArg::Typed(_))).count()
}

/// The concrete `Self` type of an impl block.
///
/// # Errors
/// [`EmitError`] for a trait impl — `#[controller]`/`#[rest_controller]` apply to the
/// controller's inherent impl (the handler-method carrier).
fn self_ty_of(item: &ItemImpl) -> Result<Type, EmitError> {
    if item.trait_.is_some() {
        return Err(EmitError {
            message: "`#[controller]`/`#[rest_controller]` apply to an inherent \
                      `impl Controller { .. }` block (the handler-method carrier), not a \
                      trait impl."
                .into(),
        });
    }
    Ok((*item.self_ty).clone())
}

/// The leading-ident name of a `Self` type (`Api` / `Repo<u32>` → `Api`/`Repo`), used as
/// the per-method route-struct identity base + diagnostics.
fn type_ident(ty: &Type) -> String {
    match ty {
        Type::Path(tp) => tp
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_else(|| "Self".into()),
        _ => "Self".into(),
    }
}

/// Parse a type-expression string into a [`syn::Type`] (the generated field/view types
/// are built from leaf-absolute path strings).
fn parse_type(s: &str) -> Result<Type, EmitError> {
    syn::parse_str(s).map_err(|e| EmitError {
        message: format!("internal: could not parse generated type `{s}`: {e}"),
    })
}

// ═══════════════════════════════ #[control_advice] ═══════════════════════════════
//
// The global-error-handling stereotype (Spring's `@ControllerAdvice` + `@ExceptionHandler`),
// expressed in leaf's DI (Task 10). Like `#[controller]`/`#[configuration]`/`#[advisable]`
// it is a DUAL-FORM macro:
//
// - on a STRUCT (`#[control_advice] struct Errors;`): the advice BEAN — structurally a
//   `#[component]` (so the advice is registered + resolvable, its collaborators
//   field-injected) that ALSO `provides` the `dyn ::leaf_web::ControlAdvice` view, so the
//   server's `Vec<Ref<dyn ControlAdvice>>` collection injection finds it. Mirrors
//   `#[runner]`'s `provides`-the-`dyn Runner`-view shape.
// - on an inherent IMPL BLOCK (`#[control_advice] impl Errors { #[exception_handler]
//   fn not_found(&self, e: &LeafError, req: &Request) -> Option<Response> {..} }`): the
//   request-mapping analogue — the macro reads each `#[exception_handler]` METHOD and
//   generates ONE `impl ::leaf_web::ControlAdvice for Errors` whose `handle` delegates to
//   the handler method(s), first-`Some`-wins in declaration order. A method-position attr
//   alone cannot emit the sibling trait impl, so the impl block is processed as a unit
//   (the same constraint `#[bean]`/`#[advice]` hit).

/// The attribute name a method must carry to be wired as an exception handler.
const EXCEPTION_HANDLER_ATTR: &str = "exception_handler";

/// Lower a `#[control_advice] struct Errors;` to its `#[component]`-equivalent bean
/// registration PLUS the `dyn ::leaf_web::ControlAdvice` `provides[]` view (so the
/// server's `Vec<Ref<dyn ControlAdvice>>` collection injection finds it). Structurally a
/// plain `#[component]` differing ONLY in the declared advice view — exactly the
/// `#[runner]` `provides`-a-view shape.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) when the struct is generic / its stereotype
/// annotation is malformed (surfaced by [`stereotype::struct_input`]).
pub fn expand_control_advice_struct(
    item: &ItemStruct,
    attr: TokenStream,
) -> Result<TokenStream, EmitError> {
    let args = stereotype::parse_args(attr)?;
    let mut input = stereotype::struct_input(
        item,
        Stereotype::Component,
        args.name,
        args.role,
        args.scope,
        args.constructor,
    )?;
    // Declare the ControlAdvice upcast view so the server collects this bean by the
    // `dyn ControlAdvice` contract (the one place an advice differs from a plain
    // component) — the SAME provides[] machinery the stereotypes/`#[runner]` use.
    input.provides.push(ServiceView { dyn_ty: parse_type("dyn ::leaf_web::ControlAdvice")? });
    descriptor::emit(&input)
}

// ═══════════════════════════════ #[web_filter] ═══════════════════════════════
//
// The around-advice filter stereotype (Spring's servlet `Filter` / `HandlerInterceptor`),
// expressed in leaf's DI (Task T4). A STRUCT stereotype — structurally a `#[component]`
// (so the filter is registered + resolvable, its collaborators field-injected) that ALSO
// `provides` the `dyn ::leaf_web::WebFilter` view, so the server's
// `Vec<Ref<dyn WebFilter>>` collection injection finds it. The exact `provides`-a-view
// shape `#[runner]` (the `dyn Runner` view) and `#[control_advice]`-struct (the `dyn
// ControlAdvice` view) use — no type-name detection, no hand-rolled marker-trait impl.
//
// The user supplies the behaviour separately:
//     #[web_filter] struct AccessLog { /* injected fields */ }
//     #[async_impl] impl WebFilter for AccessLog { async fn filter(..) {..} }
// and (optionally) the chain `fn order(&self) -> i32` in that `impl WebFilter` block —
// the order lives on the user's trait impl (a struct stereotype cannot inject a method
// into a trait impl the user writes), so `#[web_filter]` itself is provides-view-only.

/// Lower a `#[web_filter] struct AccessLog { .. }` to its `#[component]`-equivalent bean
/// registration PLUS the `dyn ::leaf_web::WebFilter` `provides[]` view (so the server's
/// `Vec<Ref<dyn WebFilter>>` collection injection finds it). Structurally a plain
/// `#[component]` differing ONLY in the declared filter view — exactly the `#[runner]` /
/// `#[control_advice]`-struct `provides`-a-view shape.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) when the struct is generic / its stereotype
/// annotation is malformed (surfaced by [`stereotype::struct_input`]).
pub fn expand_web_filter_struct(
    item: &ItemStruct,
    attr: TokenStream,
) -> Result<TokenStream, EmitError> {
    let args = stereotype::parse_args(attr)?;
    let mut input = stereotype::struct_input(
        item,
        Stereotype::Component,
        args.name,
        args.role,
        args.scope,
        args.constructor,
    )?;
    // Declare the WebFilter upcast view so the server collects this bean by the
    // `dyn WebFilter` contract (the one place a filter differs from a plain component) —
    // the SAME provides[] machinery the stereotypes/`#[runner]`/`#[control_advice]` use.
    input.provides.push(ServiceView { dyn_ty: parse_type("dyn ::leaf_web::WebFilter")? });
    descriptor::emit(&input)
}

/// Lower a `#[keep_alive] struct EmbeddedServer { .. }` to its `#[component]`-equivalent
/// bean registration PLUS the `dyn ::leaf_core::KeepAlive` `provides[]` view (so leaf-boot
/// collects this bean by the `dyn KeepAlive` contract and SPAWNS its long-running
/// `start(ctx)` onto the lifecycle machinery). Structurally a plain `#[component]` whose
/// ONLY extra is that one declared view — the SAME `provides`-a-view shape `#[runner]`
/// (`dyn Runner`) and `#[web_filter]` (`dyn WebFilter`) use. The user supplies the
/// behaviour separately as `impl ::leaf_core::KeepAlive for EmbeddedServer`.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) on a generic struct (the stereotype shared rule) or
/// a malformed field/attr.
pub fn expand_keep_alive_struct(
    item: &ItemStruct,
    attr: TokenStream,
) -> Result<TokenStream, EmitError> {
    let args = stereotype::parse_args(attr)?;
    let mut input = stereotype::struct_input(
        item,
        Stereotype::Component,
        args.name,
        args.role,
        args.scope,
        args.constructor,
    )?;
    // Declare the KeepAlive upcast view so leaf-boot collects this bean by the
    // `dyn ::leaf_core::KeepAlive` contract (the one place a keep-alive differs from a
    // plain component) — the SAME provides[] machinery `#[runner]`/`#[web_filter]` use.
    // It is a leaf-CORE view (not leaf-web): the lifecycle trait lives in leaf-core, so
    // the embedded server publishes it through `::leaf_core::` directly (an absolute path
    // resolvable from any crate, including the umbrella facade).
    input.provides.push(ServiceView { dyn_ty: parse_type("dyn ::leaf_core::KeepAlive")? });
    descriptor::emit(&input)
}

/// Lower a `#[control_advice] impl Errors { #[exception_handler] fn .. }` block to ONE
/// generated `impl ::leaf_web::ControlAdvice for Errors` whose `handle` delegates to the
/// `#[exception_handler]` method(s) — tried in declaration order, first `Some` wins.
///
/// Each handler method takes `&self`, the `&LeafError`, and OPTIONALLY a `&Request`
/// (the structural shape: a one-extra-param handler receives the request, a zero-extra
/// handler does not — dispatch on the method's ARITY, never a spelled type name). The
/// generated `handle` threads `err`/`req` into each in turn.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) when the impl is generic / a trait impl, no method
/// carries `#[exception_handler]`, or a handler method takes no `self` receiver.
pub fn expand_control_advice_impl(item: &ItemImpl) -> Result<TokenStream, EmitError> {
    expand_control_advice_impl_with(item, TokenStream::new())
}

/// The `#[control_advice(order = N)]` impl form: same lowering as
/// [`expand_control_advice_impl`], but an explicit `order = N` arg makes the generated
/// `impl ControlAdvice` emit `fn order(&self) -> i32 { N }` — the dispatcher's stable
/// precedence sort key (the dispatcher stable-sorts advice by `order()`; the trait
/// default is `0`). Mirrors the `#[advice(order = N)]` / `#[aspect(order = N)]` order
/// plumbing: parse an integer `order`, thread it into the generated impl. No `order`
/// arg → no `fn order` override (the trait default `0` stands).
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) per [`expand_control_advice_impl`], plus a
/// malformed `order` arg (a non-integer / unknown key).
pub fn expand_control_advice_impl_with(
    item: &ItemImpl,
    attr: TokenStream,
) -> Result<TokenStream, EmitError> {
    let order = parse_advice_order(attr)?;
    let self_ty = self_ty_of(item)?;
    let advice_ident = type_ident(&self_ty);
    if !item.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{advice_ident}` is a generic `#[control_advice]` impl: a generic advice \
                 has no single concrete type to mint its `ControlAdvice` impl. Make the \
                 advice concrete."
            ),
        });
    }

    // Each `#[exception_handler]` method → one delegation arm in `handle`. The first
    // arm returning `Some` short-circuits (the first-match chain).
    let mut arms = Vec::new();
    for inner in &item.items {
        let ImplItem::Fn(func) = inner else { continue };
        if find_attr(&func.attrs, EXCEPTION_HANDLER_ATTR).is_none() {
            continue;
        }
        let method_ident = &func.sig.ident;
        if !has_self_receiver(func) {
            return Err(EmitError {
                message: format!(
                    "`{advice_ident}::{method_ident}` is an `#[exception_handler]` but takes \
                     no `self` receiver: an exception handler threads the advice bean \
                     through `&self`."
                ),
            });
        }
        // Dispatch on the handler's ARITY (structural): one non-receiver param → the
        // error alone; two → the error + the request. Never a spelled type name.
        let extra = non_receiver_arg_count(func);
        let call = if extra >= 2 {
            quote! { self.#method_ident(__err, __req) }
        } else {
            quote! { self.#method_ident(__err) }
        };
        arms.push(quote! {
            if let ::core::option::Option::Some(__resp) = #call {
                return ::core::option::Option::Some(__resp);
            }
        });
    }

    if arms.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{advice_ident}` is a `#[control_advice]` impl with no `#[exception_handler]` \
                 method: a control-advice carries at least one `fn handler(&self, e: \
                 &LeafError[, req: &Request]) -> Option<Response>` exception handler."
            ),
        });
    }

    // The generated trait impl. `handle` walks the handler arms (first `Some` wins) and
    // declines (`None`) if none map the error — the dispatcher's chain / default floor
    // takes over. The error/request binding idents are `__err`/`__req` even when an
    // arm ignores the request (the unused-binding allow covers it).
    // An explicit `order = N` emits `fn order(&self) -> i32 { N }` (the dispatcher's
    // stable precedence sort key); omitted → no override (the trait default `0` stands).
    let order_fn = order.map(|n| {
        quote! {
            fn order(&self) -> i32 { #n }
        }
    });
    Ok(quote! {
        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        impl ::leaf_web::ControlAdvice for #self_ty {
            fn handle(
                &self,
                __err: &::leaf_core::LeafError,
                __req: &::leaf_web::Request,
            ) -> ::core::option::Option<::leaf_web::Response> {
                let _ = __req;
                #(#arms)*
                ::core::option::Option::None
            }
            #order_fn
        }
    })
}

/// Parse the OPTIONAL `order = N` argument of a `#[control_advice(order = N)]` struct/
/// impl attribute into an explicit `i32` chain order, mirroring how `#[advice(order =
/// N)]` parses its order. An empty attribute → `None` (the trait default `0` stands).
///
/// # Errors
/// [`EmitError`] on a malformed attribute, an unknown key, or a non-integer `order`.
fn parse_advice_order(attr: TokenStream) -> Result<Option<i32>, EmitError> {
    if attr.is_empty() {
        return Ok(None);
    }
    let parser = syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated;
    let metas = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed `#[control_advice(..)]` argument: {e}"),
    })?;
    let mut order = None;
    for meta in metas {
        let Meta::NameValue(nv) = meta else {
            return Err(EmitError {
                message: "`#[control_advice(..)]` arguments must be `key = value` pairs".into(),
            });
        };
        let key = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
        if key != "order" {
            return Err(EmitError {
                message: format!(
                    "unknown `#[control_advice]` argument `{key}` (expected `order = N`)"
                ),
            });
        }
        order = Some(order_int_value(&nv.value)?);
    }
    Ok(order)
}

/// The integer value of an `order = N` right-hand side (allowing a leading `-`),
/// mirroring `advisor::int_value`.
fn order_int_value(expr: &syn::Expr) -> Result<i32, EmitError> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: Lit::Int(i), .. }) => {
            i.base10_parse::<i32>().map_err(|e| EmitError {
                message: format!("`order` must be an i32 integer: {e}"),
            })
        }
        syn::Expr::Unary(syn::ExprUnary { op: syn::UnOp::Neg(_), expr, .. }) => {
            Ok(-order_int_value(expr)?)
        }
        other => Err(EmitError {
            message: format!("`order` must be an integer literal, got `{}`", quote! { #other }),
        }),
    }
}

/// Find an attribute by its path's last segment (`#[exception_handler]`), matching the
/// last segment so a `#[leaf::exception_handler]`-qualified form is recognised too.
fn find_attr<'a>(attrs: &'a [syn::Attribute], name: &str) -> Option<&'a syn::Attribute> {
    attrs
        .iter()
        .find(|a| a.path().segments.last().is_some_and(|s| s.ident == name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impl_item(src: &str) -> ItemImpl {
        syn::parse_str(src).expect("a valid impl block")
    }

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    #[test]
    fn a_rest_controller_get_method_emits_a_route_bean_with_extraction_and_serialize() {
        // The headline Task-9 lowering: a `#[get("/products/{sku}")]` method on a
        // `#[rest_controller]` lowers to a generated `Route` bean that
        //   (a) provides the `dyn ::leaf_web::Route` view (so the server collects it),
        //   (b) reports `method() == GET` and `path() == "/products/{sku}"`,
        //   (c) resolves the `Path<String>` arg via `FromRequest` (trait dispatch on the
        //       parameter's STRUCTURAL extractor type — never a spelled name), calls the
        //       controller method, and SERIALIZES the return via the injected converter.
        let item = impl_item(
            r#"impl Api {
                #[get("/products/{sku}")]
                async fn get(&self, sku: Path<String>) -> Result<ProductDto, LeafError> {
                    todo!()
                }
            }"#,
        );
        let ts = expand_controller_impl(&item, true).expect("emits");
        // The whole emitted artifact must PARSE as a Rust item sequence.
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);

        // (a) the generated bean PROVIDES the `dyn ::leaf_web::Route` view (the upcast row
        //     the server's `Vec<Ref<dyn Route>>` collection injection resolves).
        assert!(
            s.contains("::core::any::TypeId::of::<dyn::leaf_web::Route>()"),
            "the Route bean must declare the `dyn ::leaf_web::Route` provides[] view: {s}"
        );
        // It rides the COMPONENTS channel (a plain `#[component]`-equivalent bean).
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "the Route bean is a COMPONENTS row: {s}"
        );

        // (b) the verb + the path PATTERN.
        assert!(s.contains("::leaf_web::http::Method::GET"), "method() == GET: {s}");
        assert!(s.contains(r#""/products/{sku}""#), "path() == the pattern: {s}");

        // (c) the arg resolves via `<Path<String> as FromRequestParts>::from_request_parts`
        //     (trait dispatch on the structural extractor type, NOT a name match) — the
        //     converter-aware seam the codegen calls uniformly per parameter, so a
        //     `Json<T>` body parameter (which rides the injected converter) lowers through
        //     the SAME call site as a request-only `Path`/`Query`.
        assert!(
            s.contains("<Path<String>as::leaf_web::FromRequestParts>::from_request_parts"),
            "the Path<String> arg resolves via FromRequestParts: {s}"
        );
        // The controller method is invoked on the injected controller.
        assert!(
            s.contains(".get(") && s.contains(".await"),
            "the handler invokes the controller method: {s}"
        );
        // The return rides the uniform `IntoResponseWith` trait, driven by the injected
        // converter (@ResponseBody) — so a bare value serializes to 200 AND a
        // `ResponseEntity<T>` sets its own status/headers, through ONE structural call
        // site (no type-name detection on the return).
        assert!(
            s.contains("::leaf_web::IntoResponseWith::into_response_with")
                && s.contains("HttpMessageConverter"),
            "a #[rest_controller] return goes through the IntoResponseWith trait: {s}"
        );
    }

    #[test]
    fn each_path_param_is_extracted_with_its_own_name_in_the_binding_context() {
        // Task T1a: a multi-capture route binds EACH `Path` parameter to ITS OWN
        // `{name}`. The codegen threads a per-argument `ExtractCtx` carrying the handler
        // PARAMETER NAME (`uid`/`oid`) into the uniform `from_request_parts` call — so the
        // name-dependent `Path<T>` extractor selects its own capture. The dispatch stays
        // the uniform `<Ty as FromRequestParts>::from_request_parts(req, conv, ctx)`; the
        // macro NEVER branches on the parameter being a `Path`.
        let item = impl_item(
            r#"impl Api {
                #[get("/users/{uid}/orders/{oid}")]
                async fn get(&self, uid: Path<u64>, oid: Path<String>) -> Result<(), LeafError> {
                    todo!()
                }
            }"#,
        );
        let ts = expand_controller_impl(&item, true).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // Each extractor call carries the parameter's OWN name in an `ExtractCtx::named`.
        assert!(
            s.contains(r#"::leaf_web::ExtractCtx::named("uid")"#),
            "the first param's extractor gets ExtractCtx::named(\"uid\"): {s}"
        );
        assert!(
            s.contains(r#"::leaf_web::ExtractCtx::named("oid")"#),
            "the second param's extractor gets ExtractCtx::named(\"oid\"): {s}"
        );
        // The dispatch is still the uniform FromRequestParts seam — never a Path-name branch.
        assert!(
            s.contains("<Path<u64>as::leaf_web::FromRequestParts>::from_request_parts")
                && s.contains("<Path<String>as::leaf_web::FromRequestParts>::from_request_parts"),
            "both params lower through the uniform FromRequestParts call site: {s}"
        );
        // The context is threaded as the third argument to the uniform call.
        assert!(
            s.contains("from_request_parts(__req,__converter,&__ctx0)")
                && s.contains("from_request_parts(__req,__converter,&__ctx1)"),
            "each extractor call passes its own binding context: {s}"
        );
    }

    #[test]
    fn a_header_param_carries_its_header_name_via_a_header_attribute() {
        // Task T1c: a `#[header("X-Api-Key")]` parameter attribute carries the HTTP header
        // NAME (which is NOT a valid Rust ident, so it cannot ride the parameter's
        // `Pat::Ident`). The codegen consumes the attribute, threads the header name into
        // the per-argument `ExtractCtx::for_header(<param-name>, <header-name>)`, and STRIPS
        // the `#[header(..)]` attribute from the re-emitted handler call (so the controller
        // method does not see a stray attribute). Dispatch stays the uniform structural
        // `FromRequestParts` seam — the macro never branches on the parameter being a
        // `Header`.
        let item = impl_item(
            r#"impl Api {
                #[get("/secure")]
                async fn secure(&self, #[header("X-Api-Key")] k: Header<String>) -> Result<(), LeafError> {
                    todo!()
                }
            }"#,
        );
        let ts = expand_controller_impl(&item, true).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The extractor call carries BOTH the parameter name and the header name.
        assert!(
            s.contains(r#"::leaf_web::ExtractCtx::for_header("k","X-Api-Key")"#),
            "the header param's extractor gets ExtractCtx::for_header(\"k\", \"X-Api-Key\"): {s}"
        );
        // It lowers through the SAME uniform FromRequestParts seam — never a Header-name branch.
        assert!(
            s.contains("<Header<String>as::leaf_web::FromRequestParts>::from_request_parts"),
            "the Header<String> param lowers through the uniform FromRequestParts call site: {s}"
        );
        // The `#[header(..)]` attribute is CONSUMED — the emitted controller invocation does
        // not carry a stray `header` attribute token.
        assert!(
            !s.contains("#[header") && !s.contains("# [header"),
            "the #[header(..)] attribute is stripped from the emitted artifact: {s}"
        );
    }

    #[test]
    fn a_plain_controller_get_method_returns_an_into_response_not_a_serialized_body() {
        // A plain `#[controller]` (NO @ResponseBody) applies the `IntoResponse` return
        // policy directly — the return is NOT serialized through the converter.
        let item = impl_item(
            r#"impl Pages {
                #[get("/")]
                async fn home(&self) -> Response {
                    todo!()
                }
            }"#,
        );
        let s = flat(&expand_controller_impl(&item, false).expect("emits"));
        assert!(
            s.contains("::leaf_web::IntoResponse"),
            "a #[controller] return goes through IntoResponse: {s}"
        );
        // No converter serialize on the plain-controller path.
        assert!(
            !s.contains(".write("),
            "a plain #[controller] must NOT serialize via a converter: {s}"
        );
    }

    #[test]
    fn two_mapped_methods_emit_two_route_beans() {
        // The impl-iterator pay-off: two mapping methods => two generated `Route` beans
        // (sidestepping the attr-on-method limitation, like `#[bean]`/`#[advice]`).
        let item = impl_item(
            r#"impl Orders {
                #[post("/orders")]
                async fn create(&self, body: Json<NewOrder>) -> Result<OrderDto, LeafError> { todo!() }
                #[get("/orders/{id}")]
                async fn get(&self, id: Path<String>) -> Result<OrderDto, LeafError> { todo!() }
                fn helper(&self) -> u8 { 0 }
            }"#,
        );
        let ts = expand_controller_impl(&item, true).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert_eq!(
            s.matches("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]")
                .count(),
            2,
            "two mapping methods => two COMPONENTS Route rows: {s}"
        );
        assert!(s.contains("::leaf_web::http::Method::POST"), "the POST verb: {s}");
        assert!(s.contains(r#""/orders""#), "the POST path: {s}");
        assert!(s.contains(r#""/orders/{id}""#), "the GET path: {s}");
        // The `Json<NewOrder>` body param lowers through the SAME `FromRequestParts` call
        // site as the `Path<String>` — the converter is threaded in so the body
        // deserializes through the injected converter (no special-cased name dispatch).
        assert!(
            s.contains("<Json<NewOrder>as::leaf_web::FromRequestParts>::from_request_parts"),
            "the Json<NewOrder> body resolves via FromRequestParts with the converter: {s}"
        );
        // The non-mapping helper method does NOT contribute a Route.
        assert_eq!(
            s.matches("::core::any::TypeId::of::<dyn::leaf_web::Route>()").count(),
            2,
            "only the two mapping methods provide the dyn Route view: {s}"
        );
    }

    #[test]
    fn the_route_method_attr_drives_an_explicit_verb_and_path() {
        // `#[route(method = "PUT", path = "/x")]` is the general form the verb-specific
        // attrs sugar — an explicit verb + path.
        let item = impl_item(
            r#"impl Api {
                #[route(method = "PUT", path = "/widgets/{id}")]
                async fn replace(&self, id: Path<String>) -> Result<(), LeafError> { todo!() }
            }"#,
        );
        let s = flat(&expand_controller_impl(&item, true).expect("emits"));
        assert!(s.contains("::leaf_web::http::Method::PUT"), "the explicit verb: {s}");
        assert!(s.contains(r#""/widgets/{id}""#), "the explicit path: {s}");
    }

    #[test]
    fn a_generic_controller_impl_is_a_hard_error() {
        let item = impl_item(
            r#"impl<T> Api<T> { #[get("/x")] async fn x(&self) -> Result<(), LeafError> { todo!() } }"#,
        );
        let err = expand_controller_impl(&item, true)
            .expect_err("a generic controller impl hard-errors");
        assert!(err.message.contains("generic"), "got: {}", err.message);
    }

    #[test]
    fn a_mapping_on_a_method_without_a_self_receiver_is_an_error() {
        let item = impl_item(
            r#"impl Api { #[get("/x")] async fn x() -> Result<(), LeafError> { todo!() } }"#,
        );
        let err = expand_controller_impl(&item, true)
            .expect_err("a mapping method needs a self receiver");
        assert!(err.message.contains("self"), "got: {}", err.message);
    }

    // ── #[control_advice] (the global-error-handling stereotype, Task 10) ─────────

    fn struct_item(src: &str) -> ItemStruct {
        syn::parse_str(src).expect("a valid struct item")
    }

    #[test]
    fn a_control_advice_struct_provides_the_dyn_control_advice_view() {
        // The struct form: `#[control_advice] struct Errors;` is a `#[component]`-equivalent
        // bean that ALSO `provides` the `dyn ::leaf_web::ControlAdvice` view (so the
        // server's `Vec<Ref<dyn ControlAdvice>>` collection injection finds it). Mirrors
        // `#[runner]`'s `provides`-the-`dyn Runner`-view shape.
        let ts = expand_control_advice_struct(&struct_item("struct Errors;"), TokenStream::new())
            .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        // It rides the COMPONENTS channel (a plain `#[component]`-equivalent bean).
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "the advice bean is a COMPONENTS row: {s}"
        );
        // It declares the `dyn ::leaf_web::ControlAdvice` provides[] view.
        assert!(
            s.contains("::core::any::TypeId::of::<dyn::leaf_web::ControlAdvice>()"),
            "the advice bean must declare the `dyn ::leaf_web::ControlAdvice` view: {s}"
        );
    }

    #[test]
    fn a_control_advice_impl_delegates_handle_to_the_exception_handler_method() {
        // The impl form: an `#[exception_handler]` method `fn not_found(&self, e:
        // &LeafError) -> Option<Response>` lowers to an `impl ::leaf_web::ControlAdvice`
        // whose `handle` delegates to the method (passing the error + request through).
        let item = impl_item(
            r#"impl Errors {
                #[exception_handler]
                fn not_found(&self, e: &LeafError, req: &Request) -> Option<Response> {
                    todo!()
                }
            }"#,
        );
        let ts = expand_control_advice_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        // The generated trait impl.
        assert!(
            s.contains("impl::leaf_web::ControlAdviceforErrors"),
            "the impl form generates `impl ControlAdvice for Errors`: {s}"
        );
        // Its `handle` delegates to the exception-handler method.
        assert!(
            s.contains("fnhandle") && s.contains("self.not_found("),
            "`handle` delegates to the #[exception_handler] method: {s}"
        );
    }

    #[test]
    fn a_control_advice_impl_with_no_arg_handler_still_delegates() {
        // The minimal handler shape: an `#[exception_handler]` taking only `&self` +
        // `&LeafError` (no `&Request`) is supported — `handle` passes just the error.
        let item = impl_item(
            r#"impl Errors {
                #[exception_handler]
                fn map(&self, e: &LeafError) -> Option<Response> { todo!() }
            }"#,
        );
        let s = flat(&expand_control_advice_impl(&item).expect("emits"));
        assert!(s.contains("self.map("), "delegates to the handler: {s}");
    }

    #[test]
    fn a_control_advice_impl_tries_each_exception_handler_in_order() {
        // Multiple `#[exception_handler]` methods are tried in declaration order; the
        // first returning `Some` wins (the `?`-free first-match chain in `handle`).
        let item = impl_item(
            r#"impl Errors {
                #[exception_handler]
                fn not_found(&self, e: &LeafError) -> Option<Response> { todo!() }
                #[exception_handler]
                fn bad_request(&self, e: &LeafError) -> Option<Response> { todo!() }
                fn helper(&self) -> u8 { 0 }
            }"#,
        );
        let s = flat(&expand_control_advice_impl(&item).expect("emits"));
        assert!(s.contains("self.not_found("), "first handler tried: {s}");
        assert!(s.contains("self.bad_request("), "second handler tried: {s}");
        // The non-handler helper does NOT participate.
        assert!(!s.contains("self.helper("), "a non-handler method is not wired: {s}");
    }

    #[test]
    fn a_control_advice_impl_with_no_exception_handler_is_an_error() {
        // An impl with no `#[exception_handler]` method has nothing to delegate to.
        let item = impl_item(r#"impl Errors { fn helper(&self) -> u8 { 0 } }"#);
        let err = expand_control_advice_impl(&item)
            .expect_err("a control-advice impl needs an exception handler");
        assert!(err.message.contains("exception_handler"), "got: {}", err.message);
    }

    #[test]
    fn a_generic_control_advice_impl_is_a_hard_error() {
        let item = impl_item(
            r#"impl<T> Errors<T> { #[exception_handler] fn h(&self, e: &LeafError) -> Option<Response> { todo!() } }"#,
        );
        let err = expand_control_advice_impl(&item)
            .expect_err("a generic control-advice impl hard-errors");
        assert!(err.message.contains("generic"), "got: {}", err.message);
    }

    // ── #[control_advice(order = N)] — the dispatcher-precedence knob (Task T4 b) ──

    #[test]
    fn a_control_advice_impl_with_an_order_arg_emits_a_fn_order() {
        // `#[control_advice(order = 5)]` on the impl form makes the generated `impl
        // ControlAdvice` emit `fn order(&self) -> i32 { 5 }` so the dispatcher's
        // stable order-sort gives this advice a deterministic precedence (advice.rs
        // defaults to 0 otherwise). Mirrors the `#[advice(order = N)]` plumbing.
        let item = impl_item(
            r#"impl Errors {
                #[exception_handler]
                fn h(&self, e: &LeafError) -> Option<Response> { todo!() }
            }"#,
        );
        let attr: TokenStream = syn::parse_str("order = 5").expect("tokens");
        let ts = expand_control_advice_impl_with(&item, attr).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        assert!(
            s.contains("fnorder(&self)->i32{5i32}") || s.contains("fnorder(&self)->i32{5}"),
            "an order arg emits `fn order(&self) -> i32 {{ 5 }}`: {s}"
        );
    }

    #[test]
    fn a_control_advice_impl_without_order_emits_no_order_override() {
        // No `order` arg → the generated impl emits NO `fn order` (the trait default 0
        // applies), exactly as before the order knob existed.
        let item = impl_item(
            r#"impl Errors {
                #[exception_handler]
                fn h(&self, e: &LeafError) -> Option<Response> { todo!() }
            }"#,
        );
        let s = flat(&expand_control_advice_impl_with(&item, TokenStream::new()).expect("emits"));
        assert!(!s.contains("fnorder"), "no order arg → no `fn order` override: {s}");
    }

    // ── #[web_filter] STRUCT stereotype (Task T4 a) ──

    #[test]
    fn a_web_filter_struct_provides_the_dyn_web_filter_view() {
        // `#[web_filter] struct AccessLog;` is a `#[component]`-equivalent bean that
        // ALSO `provides` the `dyn ::leaf_web::WebFilter` view (so the server's
        // `Vec<Ref<dyn WebFilter>>` collection injection finds it) — the SAME
        // `provides`-a-view shape `#[runner]`/`#[control_advice]`-struct use.
        let ts = expand_web_filter_struct(&struct_item("struct AccessLog;"), TokenStream::new())
            .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        // It rides the COMPONENTS channel (a plain `#[component]`-equivalent bean).
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "the filter bean is a COMPONENTS row: {s}"
        );
        // It declares the `dyn ::leaf_web::WebFilter` provides[] view.
        assert!(
            s.contains("::core::any::TypeId::of::<dyn::leaf_web::WebFilter>()"),
            "the filter bean must declare the `dyn ::leaf_web::WebFilter` view: {s}"
        );
    }

    #[test]
    fn a_web_filter_struct_field_injects_its_collaborators() {
        // `#[web_filter]` is structurally a `#[component]`: its fields are injection
        // points routed through `Injectable` (trait dispatch), exactly like any
        // stereotype. So the collaborator a filter needs is field-injected.
        let ts = expand_web_filter_struct(
            &struct_item("struct AccessLog { audit: leaf_core::Ref<Audit> }"),
            TokenStream::new(),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("<leaf_core::Ref<Audit>as::leaf_core::Injectable>::inject(__cx).await?"),
            "a `#[web_filter]` field is field-injected via the Injectable trait: {s}"
        );
        assert!(
            s.contains("::core::any::TypeId::of::<dyn::leaf_web::WebFilter>()"),
            "and still provides the `dyn WebFilter` view: {s}"
        );
    }

    #[test]
    fn a_web_filter_struct_honours_name_and_scope_args() {
        // The shared stereotype args (`name`/`scope`/`role`/`constructor`) flow through
        // exactly like every other struct stereotype.
        let attr: TokenStream = syn::parse_str(r#"name = "accessLog""#).expect("tokens");
        let s = flat(&expand_web_filter_struct(&struct_item("struct AccessLog;"), attr).expect("emits"));
        assert!(s.contains(r#"Some("accessLog")"#), "an explicit name flows through: {s}");
    }

    // ── #[keep_alive] STRUCT stereotype (Stage 2) ──

    #[test]
    fn a_keep_alive_struct_provides_the_dyn_keep_alive_view() {
        // `#[keep_alive] struct EmbeddedServer;` is a `#[component]`-equivalent bean that
        // ALSO `provides` the `dyn ::leaf_core::KeepAlive` view (so leaf-boot collects it
        // by that contract and spawns its long-running `start`) — the SAME `provides`-a-view
        // shape `#[runner]`/`#[web_filter]` use. The view is leaf-CORE, not leaf-web.
        let ts = expand_keep_alive_struct(&struct_item("struct EmbeddedServer;"), TokenStream::new())
            .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        // It rides the COMPONENTS channel (a plain `#[component]`-equivalent bean), NOT the
        // RUNNER_PAIRINGS channel — the embedded server is OFF the runner stream by design.
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "the keep-alive bean is a COMPONENTS row: {s}"
        );
        assert!(
            !s.contains("RUNNER_PAIRINGS"),
            "a #[keep_alive] bean is NOT a #[runner] — it must not emit a runner pairing: {s}"
        );
        // It declares the `dyn ::leaf_core::KeepAlive` provides[] view.
        assert!(
            s.contains("::core::any::TypeId::of::<dyn::leaf_core::KeepAlive>()"),
            "the keep-alive bean must declare the `dyn ::leaf_core::KeepAlive` view: {s}"
        );
    }

    #[test]
    fn a_keep_alive_struct_field_injects_its_collaborators() {
        // `#[keep_alive]` is structurally a `#[component]`: its fields are injection points
        // routed through `Injectable` (trait dispatch), exactly like the embedded server's
        // injected backend/routes/filters/advice/props.
        let ts = expand_keep_alive_struct(
            &struct_item("struct EmbeddedServer { server: leaf_core::Ref<Backend> }"),
            TokenStream::new(),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("<leaf_core::Ref<Backend>as::leaf_core::Injectable>::inject(__cx).await?"),
            "a `#[keep_alive]` field is field-injected via the Injectable trait: {s}"
        );
        assert!(
            s.contains("::core::any::TypeId::of::<dyn::leaf_core::KeepAlive>()"),
            "and still provides the `dyn KeepAlive` view: {s}"
        );
    }

    #[test]
    fn emit_controller_kind_declares_the_struct_response_body_policy() {
        // The controller STRUCT emits a `ControllerKind` marker carrying its @ResponseBody
        // policy, so the matching request-mapping impl can assert agreement.
        let rest = flat(&emit_controller_kind(&struct_item("struct Api;"), true));
        assert!(
            rest.contains("impl::leaf_web::ControllerKindforApi"),
            "a #[rest_controller] struct impls ControllerKind: {rest}"
        );
        assert!(
            rest.contains("constRESPONSE_BODY:bool=true"),
            "a #[rest_controller] struct declares RESPONSE_BODY = true: {rest}"
        );
        let plain = flat(&emit_controller_kind(&struct_item("struct Api;"), false));
        assert!(
            plain.contains("constRESPONSE_BODY:bool=false"),
            "a #[controller] struct declares RESPONSE_BODY = false: {plain}"
        );
    }

    #[test]
    fn a_controller_impl_emits_the_controller_kind_mismatch_guard() {
        // Every request-mapping impl appends a compile-time guard asserting the controller
        // struct's declared @ResponseBody policy equals THIS impl's — so a
        // `#[rest_controller] struct` + `#[controller] impl` mismatch (or a `#[get]` impl on
        // a struct never annotated as a controller, which fails the `ControllerKind` bound)
        // is a hard compile error, not a silent policy disagreement.
        let item = impl_item(
            r#"impl Api {
                #[get("/x")]
                async fn x(&self) -> Result<Dto, LeafError> { todo!() }
            }"#,
        );
        let rest = flat(&expand_controller_impl(&item, true).expect("emits"));
        assert!(
            rest.contains("<Apias::leaf_web::ControllerKind>::RESPONSE_BODY==true"),
            "a #[rest_controller] impl asserts the struct policy is `true`: {rest}"
        );
        let plain = flat(&expand_controller_impl(&item, false).expect("emits"));
        assert!(
            plain.contains("<Apias::leaf_web::ControllerKind>::RESPONSE_BODY==false"),
            "a #[controller] impl asserts the struct policy is `false`: {plain}"
        );
    }
}
