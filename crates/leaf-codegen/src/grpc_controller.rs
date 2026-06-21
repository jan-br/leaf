//! The `#[grpc_controller]` controller-impl ITERATOR (Stage 4): lower each RPC method of
//! a `#[grpc_controller] impl ServiceTrait for Bean` block into ONE generated `GrpcRoute`
//! bean — the SECOND `Handler` family, collected by DI exactly like the HTTP `#[rest_controller]`
//! per-method `Route` beans.
//!
//! ## What it emits, per RPC method
//!
//! `#[grpc_controller] impl catalog::Catalog for CatalogController {
//!     async fn get(&self, req: ProductReq) -> Result<Product, Status> { .. } }`
//! lowers `get` to:
//!
//! - a `#[doc(hidden)]` generated `GrpcRoute` STRUCT (`__LeafGrpcRoute_CatalogController_get`)
//!   holding the DI'd `controller: Ref<CatalogController>` (field injection) + the injected
//!   `codec: Ref<::leaf_grpc::ProstCodec>` (prost),
//! - its `impl ::leaf_grpc::GrpcRoute` (`path()` = the `/pkg.Service/Method` constant read
//!   from the Stage-3 trait seam; `handler()` = `self`),
//! - its `impl ::leaf_grpc::GrpcHandler` whose `call` wraps the typed method with
//!   framing/codec via the CALL-SHAPE wrapper (`call_unary`/`call_server_stream`/
//!   `call_client_stream`/`call_bidi`) — the shape read from the Stage-3 trait seam, NEVER
//!   from the textual type of `req`/the return (the no-type-names rule),
//! - the `#[component]`-equivalent bean registration (one const `::leaf_core::Descriptor`
//!   into `COMPONENTS`, via the SAME [`crate::descriptor::emit`] currency the stereotypes
//!   use) that `provides` the `dyn ::leaf_grpc::GrpcRoute` view, so `GrpcDispatch`'s
//!   `Vec<Ref<dyn GrpcRoute>>` collection injection finds it.
//!
//! The controller bean itself stays a plain `#[grpc_controller]` struct (the struct macro
//! registered it + its `GrpcControllerKind` marker); this iterator only contributes the
//! per-method `GrpcRoute` beans. Async methods are desugared NATIVELY here (no separate
//! `#[async_impl]`) and the original RPC impl block is RE-EMITTED unchanged by the macro.
//!
//! ## DRIFT vs. the original Stage-4 plan (Stage 2/3 as they actually landed)
//!
//! The plan assumed a Stage-2 `GrpcService` trait exposing `__leaf_grpc_path(name)` /
//! `__leaf_grpc_shape(name)` const-fn seams and a `Ref<dyn GrpcCodec>` injection. The
//! Stage-3 generator that actually landed instead emits, beside each generated server
//! trait, a `pub mod <service_snake>` of `pub const <METHOD>_DESCRIPTOR:
//! ::leaf_grpc::MethodDescriptor` consts (each carrying `.path` + `.shape`). And
//! [`crate`]'s codec seam (`::leaf_grpc::GrpcCodec`) is NOT object-safe (its methods are
//! generic over `M: prost::Message`), so the codec is injected as the CONCRETE
//! `::leaf_grpc::ProstCodec` bean. This module therefore:
//!
//! - reads the per-method path + shape from `<service-mod>::<METHOD>_DESCRIPTOR` (a `const`
//!   the compiler folds), by the SCREAMING_SNAKE of `sig.ident` — STILL never a type-name
//!   check on `req`/the return, the no-type-names rule held end to end, and
//! - field-injects `codec: ::leaf_core::Ref<::leaf_grpc::ProstCodec>` (the concrete codec
//!   bean), not a `dyn` view.
//!
//! The `GrpcControllerKind` marker (the gRPC twin of `::leaf_web::ControllerKind`) and the
//! four `call_*` framing/codec wrappers are added to leaf-grpc in this stage (the plan's
//! "NOTE for Stage 2" items that Stage 2 deferred), so the emitted absolute `::leaf_grpc::`
//! paths resolve when a user actually writes `#[grpc_controller]`.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ImplItem, ImplItemFn, ItemImpl, ItemStruct, Type};

use crate::descriptor::{self, BeanInput, Dependency, EmitError, FieldShape, Scope, ServiceView, Slice};
use crate::stereotype::Stereotype;

/// The four gRPC call shapes (§5). The shape selects WHICH framing/codec wrapper the
/// generated `GrpcHandler::call` invokes around the typed user method — read from the
/// Stage-3 trait seam, NEVER inferred from the textual type of `req`/the return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallShape {
    /// `async fn m(&self, req: T) -> Result<U, Status>`.
    Unary,
    /// `async fn m(&self, req: T) -> Result<Streaming<U>, Status>`.
    ServerStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<U, Status>`.
    ClientStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<Streaming<U>, Status>`.
    Bidi,
}

/// Lower a `#[grpc_controller] impl ServiceTrait for Bean` block to its per-RPC `GrpcRoute`
/// beans (one const `Descriptor` + the generated `GrpcRoute`/`GrpcHandler` impls per RPC
/// method, through the SAME [`descriptor::emit`] currency the stereotypes use). The macro
/// re-emits the original impl block (with async desugared); this function emits the sibling
/// `GrpcRoute` registration rows a method-position attr alone cannot.
///
/// # Errors
/// [`EmitError`] (→ `compile_error!`) when the impl is generic, NOT a trait impl (a
/// `#[grpc_controller]` impl implements the Stage-3 service trait), or an RPC method takes
/// no `self` receiver.
pub fn expand_grpc_controller_impl(item: &ItemImpl) -> Result<TokenStream, EmitError> {
    let service_trait = service_trait_of(item)?;
    let self_ty = (*item.self_ty).clone();
    let controller_ident = type_ident(&self_ty);
    if !item.generics.params.is_empty() {
        return Err(EmitError {
            message: format!(
                "`{controller_ident}` is a generic `#[grpc_controller]` impl: a generic \
                 controller has no single concrete type to mint its per-method `GrpcRoute` \
                 beans. Make the controller concrete."
            ),
        });
    }

    let mut rows = TokenStream::new();
    for inner in &item.items {
        let ImplItem::Fn(func) = inner else { continue };
        rows.extend(emit_grpc_route_bean(&self_ty, &service_trait, &controller_ident, func)?);
    }
    // The dual-form consistency guard: assert the controller STRUCT carries the
    // `GrpcControllerKind` marker the struct stereotype emits (so a `#[grpc_controller] impl`
    // on a struct never annotated `#[grpc_controller]` fails loudly). Keyed on the trait, not
    // a spelled type name.
    rows.extend(grpc_controller_kind_guard(&self_ty));
    Ok(rows)
}

/// Emit ONE generated `GrpcRoute` bean for an RPC method: the `#[doc(hidden)]` route struct
/// (holding the DI'd controller + the codec), its `GrpcRoute` + `GrpcHandler` trait impls,
/// and the `#[component]`-equivalent const registration that `provides` the
/// `dyn ::leaf_grpc::GrpcRoute` view.
fn emit_grpc_route_bean(
    self_ty: &Type,
    service_trait: &syn::Path,
    controller_ident: &str,
    method: &ImplItemFn,
) -> Result<TokenStream, EmitError> {
    let method_ident = &method.sig.ident;
    let method_name = method_ident.to_string();

    if !has_self_receiver(method) {
        return Err(EmitError {
            message: format!(
                "`{controller_ident}::{method_name}` is a `#[grpc_controller]` RPC method but \
                 takes no `self` receiver: a handler method threads the controller bean \
                 through `&self`."
            ),
        });
    }

    // The generated route struct: `__LeafGrpcRoute_<Controller>_<method>`. Unique per
    // (controller, method) so two RPCs in one module never collide.
    let route_struct_ident = format_ident!("__LeafGrpcRoute_{controller_ident}_{method_name}");
    let route_struct_ty: Type = parse_type(&route_struct_ident.to_string())?;

    // The Stage-3 descriptor const for this method: `<service-mod>::<METHOD>_DESCRIPTOR`,
    // keyed by the SCREAMING_SNAKE of the method NAME (a const the compiler folds), NEVER a
    // spelled `/pkg.Service/Method` literal — so the macro carries no proto knowledge and an
    // aliased message type is irrelevant. DRIFT: the original plan assumed a `<Trait as
    // GrpcService>::__leaf_grpc_path(name)` const-fn; the seam Stage 3 actually emits is this
    // per-method `MethodDescriptor` const beside the trait.
    let descriptor_path = descriptor_const_path(service_trait, &method_name);
    // `path()` reads the descriptor's `.path` field (the `/pkg.Service/Method` const).
    let path_expr = quote! { #descriptor_path.path };
    // The CALL-SHAPE wrapper: which framing/codec adapter `call` wraps the typed method with
    // (unary/server/client/bidi). See [`shape_dispatch`] for the descriptor-authority +
    // structural-selection design (and why one wrapper is emitted, not a four-arm match).
    let shape = classify_call_shape(method);
    let dispatch = shape_dispatch(&descriptor_path, shape, method_ident);

    // The struct's injected fields (field injection through `Injectable`): the controller bean
    // + the prost codec. `&*self.controller` / `&*self.codec` deref the `Ref<…>` to the
    // value inside the wrapper. DRIFT: `GrpcCodec` is NOT object-safe (generic methods), so
    // the CONCRETE `ProstCodec` bean is injected, not a `dyn` view.
    let deps = vec![
        Dependency {
            name: "controller".into(),
            ty: parse_type(&format!("::leaf_core::Ref<{}>", quote!(#self_ty)))?,
        },
        Dependency {
            name: "codec".into(),
            ty: parse_type("::leaf_core::Ref<::leaf_grpc::ProstCodec>")?,
        },
    ];

    let items = quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        pub struct #route_struct_ident {
            controller: ::leaf_core::Ref<#self_ty>,
            codec: ::leaf_core::Ref<::leaf_grpc::ProstCodec>,
        }

        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        impl ::leaf_grpc::GrpcRoute for #route_struct_ident {
            fn path(&self) -> &str {
                #path_expr
            }
            fn handler(&self) -> &dyn ::leaf_grpc::GrpcHandler {
                self
            }
        }

        #[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
        impl ::leaf_grpc::GrpcHandler for #route_struct_ident {
            fn call<'__a>(
                &'__a self,
                __req: ::leaf_web::Request,
            ) -> ::leaf_core::BoxFuture<'__a, ::leaf_web::Response> {
                ::std::boxed::Box::pin(async move {
                    let __controller = &*self.controller;
                    let __codec: &::leaf_grpc::ProstCodec = &*self.codec;
                    #dispatch
                })
            }
        }
    };

    // The `#[component]`-equivalent bean registration providing the `dyn ::leaf_grpc::GrpcRoute`
    // view, FIELD-injected through `Injectable` (the SAME descriptor currency + field-default
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
    input.provides = vec![ServiceView { dyn_ty: parse_type("dyn ::leaf_grpc::GrpcRoute")? }];
    let registration = descriptor::emit(&input)?;

    Ok(quote! { #items #registration })
}

/// The CALL-SHAPE dispatch expression: wrap the typed user method with the ONE framing/codec
/// adapter for its gRPC shape (`call_unary`/`call_server_stream`/`call_client_stream`/
/// `call_bidi`).
///
/// ## Why ONE wrapper, not a four-arm `match` on the descriptor const (a deviation)
///
/// The original plan emitted a four-arm `match #descriptor.shape { … }` so "the compiler
/// picks the wrapper from the const shape." That does NOT compile: Rust typechecks ALL match
/// arms before any const-folding, and the four wrappers carry DIFFERENT closure-return bounds
/// (`call_server_stream`/`call_bidi` require `Result<Streaming<U>, Status>`, `call_unary`/
/// `call_client_stream` require `Result<U, Status>`) — a single native method signature
/// (e.g. unary `T -> Result<U, Status>`) can satisfy only its own arm, so the others are
/// hard type errors. The macro must therefore emit exactly ONE wrapper call, chosen at MACRO
/// time. The macro cannot read the descriptor const's RUNTIME `.shape` value, so the shape is
/// classified STRUCTURALLY (see [`classify_call_shape`]) — the SAME `(client_streaming,
/// server_streaming)` classification Stage-3 itself performs, expressed over the framework's
/// own `Streaming<…>` wrapper on the param/return (NOT a user message-type name, and via
/// `syn::Ident`-to-`syn::Ident` equality so no ident-to-string-literal type-name detection
/// is introduced).
///
/// The descriptor const stays AUTHORITATIVE for `path()` and for the runtime shape; this
/// also emits a `const _` assertion that the structurally-classified shape EQUALS
/// `#descriptor.shape`, so a structural/descriptor disagreement is a hard compile error (the
/// descriptor remains the source of truth — the structural pick can never silently diverge).
fn shape_dispatch(
    descriptor_path: &TokenStream,
    shape: CallShape,
    method_ident: &syn::Ident,
) -> TokenStream {
    // The typed invocation, referenced by method name on the injected controller Ref. The
    // wrapper supplies `__msg` (T for unary/server-stream, Streaming<T> for client-stream/bidi)
    // and consumes the method's returned future. ONE invocation form serves all four shapes —
    // the wrapper differs, not the call.
    let invoke = quote! { __controller.#method_ident(__msg).await };
    let (wrapper, shape_variant) = match shape {
        CallShape::Unary => (quote!(call_unary), quote!(Unary)),
        CallShape::ServerStream => (quote!(call_server_stream), quote!(ServerStream)),
        CallShape::ClientStream => (quote!(call_client_stream), quote!(ClientStream)),
        CallShape::Bidi => (quote!(call_bidi), quote!(Bidi)),
    };
    quote! {
        // The descriptor stays AUTHORITATIVE: assert the structurally-classified shape equals
        // the Stage-3 descriptor const's shape (a const-folded check; a disagreement is a hard
        // compile error, so the wire shape never silently diverges from the descriptor).
        const _: () = ::core::assert!(
            matches!(#descriptor_path.shape, ::leaf_grpc::CallShape::#shape_variant),
            "gRPC call-shape mismatch: the method's signature shape disagrees with its \
             Stage-3 descriptor shape. Regenerate the service trait from the .proto."
        );
        ::leaf_grpc::#wrapper(__req, __codec, |__msg| async move { #invoke }).await
    }
}

/// Classify an RPC method's [`CallShape`] STRUCTURALLY from the framework's `Streaming<…>`
/// wrapper on the (first non-receiver) parameter + the return's `Ok` type — the SAME
/// `(client_streaming, server_streaming)` classification Stage-3 performs. A `Streaming<T>`
/// PARAMETER ⇒ client-streaming; a `Result<Streaming<U>, _>` RETURN ⇒ server-streaming.
///
/// This is structural arg/return handling (the allowed kind), NEVER a branch on a user
/// message-type name, and uses `syn::Ident`-to-`syn::Ident` equality — so it introduces NO
/// ident-to-string-literal type-name detection. The descriptor const remains authoritative;
/// [`shape_dispatch`] emits a compile-time assertion that this classification matches it.
fn classify_call_shape(method: &ImplItemFn) -> CallShape {
    let client_streaming = first_param_type(method).is_some_and(is_streaming_wrapped);
    let server_streaming = result_ok_type(&method.sig.output).is_some_and(is_streaming_wrapped);
    match (client_streaming, server_streaming) {
        (false, false) => CallShape::Unary,
        (false, true) => CallShape::ServerStream,
        (true, false) => CallShape::ClientStream,
        (true, true) => CallShape::Bidi,
    }
}

/// The type of the FIRST non-receiver parameter (the single request `T` or `Streaming<T>`),
/// or `None` for a no-argument method.
fn first_param_type(method: &ImplItemFn) -> Option<&Type> {
    method.sig.inputs.iter().find_map(|a| match a {
        FnArg::Typed(pt) => Some(&*pt.ty),
        FnArg::Receiver(_) => None,
    })
}

/// The `Ok` type of a `Result<Ok, Err>` return (the `U` or `Streaming<U>`), or `None` for a
/// non-`Result` / unit return.
fn result_ok_type(output: &syn::ReturnType) -> Option<&Type> {
    let syn::ReturnType::Type(_, ty) = output else { return None };
    let Type::Path(tp) = &**ty else { return None };
    let last = tp.path.segments.last()?;
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else { return None };
    args.args.iter().find_map(|a| match a {
        syn::GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}

/// `true` iff `ty` is the framework's `Streaming<…>` wrapper — matched by the LAST path
/// segment's ident via `syn::Ident`-to-`syn::Ident` equality (the framework's OWN streaming
/// marker, not a user message-type name; no ident-to-string-literal comparison). The
/// `::leaf_grpc::Streaming` / `leaf_grpc::Streaming` / bare `Streaming` spellings all
/// classify, so an aliased message type inside the wrapper is irrelevant.
fn is_streaming_wrapped(ty: &Type) -> bool {
    let Type::Path(tp) = ty else { return false };
    let streaming = format_ident!("Streaming");
    tp.path.segments.last().is_some_and(|s| s.ident == streaming)
}

/// The `<service-mod>::<METHOD>_DESCRIPTOR` const path for a method NAME. DRIFT-aware: the
/// Stage-3 generator emits the per-method `MethodDescriptor` consts inside a `pub mod
/// <service_snake>` placed beside the generated server trait. So for a trait path
/// `catalog::Catalog`, the descriptor of `get` lives at `catalog::catalog::GET_DESCRIPTOR`
/// (the trait's parent module + the service-snake module + `<SCREAMING>_DESCRIPTOR`). Keyed
/// by the method NAME the macro already has from `sig.ident` — never a spelled path literal.
fn descriptor_const_path(service_trait: &syn::Path, method_name: &str) -> TokenStream {
    let const_ident = format_ident!("{}_DESCRIPTOR", screaming_snake(method_name));
    let module_snake = service_trait
        .segments
        .last()
        .map(|s| screaming_snake(&s.ident.to_string()).to_lowercase())
        .unwrap_or_default();
    let module_ident = format_ident!("{module_snake}");
    // The parent module of the trait (all segments but the trait name). When the trait is a
    // bare ident, the descriptor module is a sibling in the current scope.
    let parent: Vec<&syn::PathSegment> =
        service_trait.segments.iter().take(service_trait.segments.len().saturating_sub(1)).collect();
    let leading = if service_trait.leading_colon.is_some() {
        quote! { :: }
    } else {
        quote! {}
    };
    quote! { #leading #(#parent ::)* #module_ident :: #const_ident }
}

/// SCREAMING_SNAKE of an identifier (`Get` → `GET`, `getProduct`/`get_product` → both
/// `GET_PRODUCT`), mirroring `leaf_grpc_build::service_gen::const_ident`. Pure case mechanics
/// over the method's OWN ident — NOT type-name detection (no behaviour is keyed on the text).
fn screaming_snake(ident: &str) -> String {
    let mut out = String::new();
    let mut prev_lower_or_digit = false;
    for ch in ident.chars() {
        if ch == '_' {
            out.push('_');
            prev_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() && prev_lower_or_digit {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
        prev_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
    }
    out
}

/// `true` iff the method takes a `self`/`&self`/`&mut self` receiver.
fn has_self_receiver(func: &ImplItemFn) -> bool {
    func.sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_)))
}

/// The service-trait PATH a `#[grpc_controller] impl Trait for Bean` block implements.
///
/// # Errors
/// [`EmitError`] for an inherent impl — `#[grpc_controller]` lowers a `impl ServiceTrait for
/// Controller { .. }` block (the Stage-3 generated server trait the controller satisfies).
fn service_trait_of(item: &ItemImpl) -> Result<syn::Path, EmitError> {
    match &item.trait_ {
        Some((_, path, _)) => Ok(path.clone()),
        None => Err(EmitError {
            message: "`#[grpc_controller]` applies to a `impl ServiceTrait for Controller { .. }` \
                      trait impl (the Stage-3 generated gRPC server trait), not an inherent impl."
                .into(),
        }),
    }
}

/// The leading-ident name of the `Self` type (`CatalogController` / `Repo<u32>` →
/// `CatalogController`/`Repo`), the per-method route-struct identity base + diagnostics.
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

/// Parse a type-expression string into a [`syn::Type`] (the generated field/view types are
/// built from leaf-absolute path strings).
fn parse_type(s: &str) -> Result<Type, EmitError> {
    syn::parse_str(s).map_err(|e| EmitError {
        message: format!("internal: could not parse generated type `{s}`: {e}"),
    })
}

/// Emit the controller STRUCT's `::leaf_grpc::GrpcControllerKind` marker — the gRPC twin of
/// `::leaf_web::ControllerKind`. Emitted ALONGSIDE the stereotype rows on the controller
/// struct so the matching RPC impl can assert the struct really is a `#[grpc_controller]`
/// (see [`grpc_controller_kind_guard`]).
pub fn emit_grpc_controller_kind(item: &ItemStruct) -> TokenStream {
    let ident = &item.ident;
    let (impl_g, ty_g, where_c) = item.generics.split_for_impl();
    quote! {
        #[automatically_derived]
        #[doc(hidden)]
        impl #impl_g ::leaf_grpc::GrpcControllerKind for #ident #ty_g #where_c {
            const IS_GRPC_CONTROLLER: bool = true;
        }
    }
}

/// The compile-time guard an RPC impl emits: assert the controller struct carries the
/// `GrpcControllerKind` marker (the struct stereotype emits it), turning a `#[grpc_controller]
/// impl` on a non-`#[grpc_controller]` struct into a clear `compile_error`. Keyed on the
/// trait/const — never a spelled type name. See [`emit_grpc_controller_kind`].
fn grpc_controller_kind_guard(self_ty: &Type) -> TokenStream {
    quote! {
        const _: () = {
            ::core::assert!(
                <#self_ty as ::leaf_grpc::GrpcControllerKind>::IS_GRPC_CONTROLLER,
                "gRPC controller stereotype mismatch: this `impl` block lowers RPC methods but \
                 its controller struct is not a `#[grpc_controller]`. Put `#[grpc_controller]` \
                 on BOTH the struct AND the `impl ServiceTrait for Controller` block."
            );
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stereotype;

    fn impl_item(src: &str) -> ItemImpl {
        syn::parse_str(src).expect("a valid impl block")
    }

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    #[test]
    fn a_unary_rpc_method_emits_a_grpc_route_bean() {
        // The headline Stage-4 lowering: a unary `async fn get(&self, req: ProductReq)
        // -> Result<Product, Status>` on a `#[grpc_controller] impl catalog::Catalog`
        // lowers to a generated `GrpcRoute` bean that
        //   (a) provides the `dyn ::leaf_grpc::GrpcRoute` view (so GrpcDispatch collects it),
        //   (b) reports `path()` read from the Stage-3 descriptor seam BY METHOD NAME,
        //   (c) field-injects the controller Ref + the codec Ref,
        //   (d) wraps the typed method with the UNARY framing/codec wrapper, the shape read
        //       from the descriptor seam — never inferred from the type of `req`/the return.
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let ts = expand_grpc_controller_impl(&item).expect("emits");
        // The whole emitted artifact must PARSE as a Rust item sequence.
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);

        // (a) the generated bean PROVIDES the `dyn ::leaf_grpc::GrpcRoute` view.
        assert!(
            s.contains("::core::any::TypeId::of::<dyn::leaf_grpc::GrpcRoute>()"),
            "the GrpcRoute bean must declare the `dyn ::leaf_grpc::GrpcRoute` provides[] view: {s}"
        );
        // It rides the COMPONENTS channel (a plain `#[component]`-equivalent bean).
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "the GrpcRoute bean is a COMPONENTS row: {s}"
        );

        // (b) `path()` is read from the Stage-3 descriptor seam BY METHOD NAME (the
        //     `<service-mod>::<METHOD>_DESCRIPTOR.path` const — never a spelled literal).
        //     DRIFT: the real Stage-3 seam is a per-method `MethodDescriptor` const, not a
        //     `<Trait as GrpcService>::__leaf_grpc_path("get")` const-fn.
        assert!(
            s.contains("catalog::catalog::GET_DESCRIPTOR.path"),
            "path() reads the Stage-3 descriptor seam by method name: {s}"
        );

        // (c) the controller Ref + the concrete codec Ref are field-injected.
        assert!(
            s.contains("controller:::leaf_core::Ref<CatalogController>"),
            "the controller is field-injected as Ref<Controller>: {s}"
        );
        // DRIFT: GrpcCodec is NOT object-safe, so the concrete ProstCodec bean is injected.
        assert!(
            s.contains("codec:::leaf_core::Ref<::leaf_grpc::ProstCodec>"),
            "the prost codec is field-injected as Ref<ProstCodec>: {s}"
        );

        // (d) the typed method is wrapped through the UNARY framing/codec wrapper.
        assert!(
            s.contains("::leaf_grpc::call_unary("),
            "a unary method wraps through ::leaf_grpc::call_unary: {s}"
        );
        // The controller method is invoked inside the wrapper (by NAME, on the injected Ref).
        assert!(s.contains(".get(") && s.contains(".await"), "invokes the controller method: {s}");
    }

    #[test]
    fn a_server_stream_rpc_wraps_through_call_server_stream() {
        // Server-stream: `async fn list(&self, req: ListReq) -> Result<Streaming<Product>, Status>`.
        // The shape is read from the Stage-3 descriptor seam (a const the macro names), so the
        // codegen wraps through `call_server_stream` — never inferred from the `Streaming<U>`
        // RETURN type (no type-name detection).
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn list(&self, req: ListReq) -> Result<Streaming<Product>, Status> { todo!() }
            }"#,
        );
        let ts = expand_grpc_controller_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("::leaf_grpc::call_server_stream("),
            "a server-stream method wraps through call_server_stream: {s}"
        );
        assert!(s.contains(".list(") && s.contains(".await"), "invokes the controller method: {s}");
    }

    #[test]
    fn a_client_stream_rpc_wraps_through_call_client_stream() {
        // Client-stream: `async fn upload(&self, reqs: Streaming<Chunk>) -> Result<Summary, Status>`.
        // The wrapper de-frames the inbound stream into a `Streaming<T>` and hands it to the
        // method; the shape is read from the descriptor seam, never from the `Streaming<T>` PARAM type.
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn upload(&self, reqs: Streaming<Chunk>) -> Result<Summary, Status> { todo!() }
            }"#,
        );
        let ts = expand_grpc_controller_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("::leaf_grpc::call_client_stream("),
            "a client-stream method wraps through call_client_stream: {s}"
        );
    }

    #[test]
    fn a_bidi_rpc_wraps_through_call_bidi() {
        // Bidi: `async fn chat(&self, reqs: Streaming<Msg>) -> Result<Streaming<Msg>, Status>`.
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn chat(&self, reqs: Streaming<Msg>) -> Result<Streaming<Msg>, Status> { todo!() }
            }"#,
        );
        let ts = expand_grpc_controller_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("::leaf_grpc::call_bidi("),
            "a bidi method wraps through call_bidi: {s}"
        );
    }

    fn struct_item(src: &str) -> ItemStruct {
        syn::parse_str(src).expect("a valid struct item")
    }

    #[test]
    fn a_grpc_controller_struct_is_a_component_and_emits_the_kind_marker() {
        // The struct form: `#[grpc_controller] struct CatalogController { .. }` is a
        // `#[component]`-equivalent bean (field injection of collaborators) that ALSO emits
        // the `GrpcControllerKind` marker the impl-form guard asserts.
        let rows = stereotype::emit_struct(
            &struct_item("struct CatalogController { repo: ::leaf_core::Ref<Repo> }"),
            Stereotype::Component,
            TokenStream::new(),
        )
        .expect("emits");
        let kind = emit_grpc_controller_kind(&struct_item("struct CatalogController;"));
        let ts = quote! { #rows #kind };
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // It rides the COMPONENTS channel (a plain `#[component]`-equivalent bean).
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "the controller bean is a COMPONENTS row: {s}"
        );
        // The collaborator is field-injected through the Injectable trait (no hand-rolled ctor).
        assert!(
            s.contains("<::leaf_core::Ref<Repo>as::leaf_core::Injectable>::inject(__cx).await?"),
            "the controller's collaborator is field-injected: {s}"
        );
        // The dual-form marker is emitted.
        assert!(
            s.contains("impl::leaf_grpc::GrpcControllerKindforCatalogController"),
            "the struct emits the GrpcControllerKind marker: {s}"
        );
    }

    #[test]
    fn a_grpc_controller_impl_emits_the_kind_mismatch_guard() {
        // Every RPC impl appends a compile-time guard asserting the controller struct carries
        // the `GrpcControllerKind` marker — so a `#[grpc_controller] impl` on a struct never
        // annotated `#[grpc_controller]` (which lacks the marker) is a hard compile error.
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let s = flat(&expand_grpc_controller_impl(&item).expect("emits"));
        assert!(
            s.contains("<CatalogControlleras::leaf_grpc::GrpcControllerKind>::IS_GRPC_CONTROLLER"),
            "the impl asserts the struct carries the GrpcControllerKind marker: {s}"
        );
    }

    #[test]
    fn a_generic_grpc_controller_impl_is_a_hard_error() {
        let item = impl_item(
            r#"impl<T> catalog::Catalog for CatalogController<T> {
                async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let err = expand_grpc_controller_impl(&item)
            .expect_err("a generic grpc-controller impl hard-errors");
        assert!(err.message.contains("generic"), "got: {}", err.message);
    }

    #[test]
    fn an_inherent_grpc_controller_impl_is_a_hard_error() {
        // `#[grpc_controller]` lowers a `impl ServiceTrait for Controller` trait impl (the
        // Stage-3 generated server trait). An inherent `impl Controller { .. }` has no trait
        // to read the path/shape seam from, so it is a loud error.
        let item = impl_item(
            r#"impl CatalogController {
                async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let err = expand_grpc_controller_impl(&item)
            .expect_err("an inherent grpc-controller impl hard-errors");
        assert!(err.message.contains("trait impl"), "got: {}", err.message);
    }

    #[test]
    fn an_rpc_method_without_a_self_receiver_is_an_error() {
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn get(req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let err = expand_grpc_controller_impl(&item)
            .expect_err("an RPC method needs a self receiver");
        assert!(err.message.contains("self"), "got: {}", err.message);
    }
}
