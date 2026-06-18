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
use syn::{FnArg, ImplItem, ImplItemFn, ItemImpl, Lit, Meta, Type};

use crate::descriptor::{self, BeanInput, Dependency, EmitError, FieldShape, Scope, ServiceView, Slice};
use crate::stereotype::Stereotype;

/// The mapping-attr names a method must carry to be lowered as a request-mapping
/// handler (the verb-specific sugar + the general `route` form).
const MAPPING_ATTRS: &[&str] = &["get", "post", "put", "delete", "patch", "head", "route"];

/// A request-mapping method: the verb-constant token + the path pattern + the method.
struct MappedMethod<'a> {
    method: &'a ImplItemFn,
    /// The `::http::Method` constant token for the verb (`GET`/`POST`/…).
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
    Ok(rows)
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
    // `FromRequest` extractor — `<ParamTy as ::leaf_web::FromRequest>::from_request(req)`
    // (TRAIT dispatch on the parameter's STRUCTURAL extractor type, never a spelled
    // name). The resolved bindings are passed positionally to the controller method.
    let arg_types = non_receiver_arg_types(method);
    let arg_locals: Vec<syn::Ident> =
        (0..arg_types.len()).map(|i| format_ident!("__arg{i}")).collect();
    let arg_resolves = arg_types.iter().zip(&arg_locals).map(|(ty, local)| {
        quote! {
            let #local = <#ty as ::leaf_web::FromRequest>::from_request(__req)?;
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
            // The injected `dyn ::leaf_web::HttpMessageConverter` serializes the return
            // (@ResponseBody). `Ref<dyn _>` derefs to the trait object, so the trait
            // methods auto-deref through it.
            let __converter: &dyn ::leaf_web::HttpMessageConverter = &*self.converter;
            let __body = __converter.write(&__value)?;
            ::core::result::Result::Ok(
                ::leaf_web::Response::ok()
                    .with_header(::http::header::CONTENT_TYPE, __converter.content_type())
                    .with_body(__body),
            )
        }
    } else {
        quote! {
            let __value = #invoke;
            ::core::result::Result::Ok(::leaf_web::IntoResponse::into_response(__value))
        }
    };

    // The struct's injected fields (field injection through `Injectable`): the
    // controller bean always; the converter only on the `@ResponseBody` path.
    let mut deps = vec![Dependency {
        name: "controller".into(),
        ty: parse_type(&format!("::leaf_core::Ref<{}>", quote!(#self_ty)))?,
    }];
    let converter_field = if response_body {
        deps.push(Dependency {
            name: "converter".into(),
            ty: parse_type("::leaf_core::Ref<dyn ::leaf_web::HttpMessageConverter>")?,
        });
        quote! { converter: ::leaf_core::Ref<dyn ::leaf_web::HttpMessageConverter>, }
    } else {
        TokenStream::new()
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
            fn method(&self) -> ::http::Method {
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

/// The `::http::Method::<VERB>` constant token for a verb-specific mapping attr name.
fn verb_token(name: &str) -> TokenStream {
    let verb = format_ident!("{}", name.to_uppercase());
    quote! { ::http::Method::#verb }
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
        | "CONNECT" => quote! { ::http::Method::#verb },
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

/// The NON-receiver argument types of a method, in order — each is one handler
/// parameter resolved via its `FromRequest` extractor (the type is used VERBATIM so the
/// trait dispatch is purely structural, never a name match).
fn non_receiver_arg_types(func: &ImplItemFn) -> Vec<Type> {
    func.sig
        .inputs
        .iter()
        .filter_map(|a| match a {
            FnArg::Typed(pat_ty) => Some((*pat_ty.ty).clone()),
            FnArg::Receiver(_) => None,
        })
        .collect()
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
        assert!(s.contains("::http::Method::GET"), "method() == GET: {s}");
        assert!(s.contains(r#""/products/{sku}""#), "path() == the pattern: {s}");

        // (c) the arg resolves via `<Path<String> as FromRequest>::from_request` (trait
        //     dispatch on the structural extractor type, NOT a name match).
        assert!(
            s.contains("<Path<String>as::leaf_web::FromRequest>::from_request"),
            "the Path<String> arg resolves via FromRequest: {s}"
        );
        // The controller method is invoked on the injected controller.
        assert!(
            s.contains(".get(") && s.contains(".await"),
            "the handler invokes the controller method: {s}"
        );
        // The return is serialized through the injected converter's `write` (@ResponseBody).
        assert!(
            s.contains(".write(") && s.contains("HttpMessageConverter"),
            "a #[rest_controller] return serializes via the injected converter: {s}"
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
        assert!(s.contains("::http::Method::POST"), "the POST verb: {s}");
        assert!(s.contains(r#""/orders""#), "the POST path: {s}");
        assert!(s.contains(r#""/orders/{id}""#), "the GET path: {s}");
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
        assert!(s.contains("::http::Method::PUT"), "the explicit verb: {s}");
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
}
