//! `#[async_impl]` — desugar native `async fn` methods in an `impl` block into the
//! `BoxFuture` form leaf's object-safe (`dyn`-dispatched) traits require.
//!
//! leaf's core traits (`Runner`, `TransactionManager`, `CacheManager`, …) are used as
//! trait objects, and `async fn` in a trait is not `dyn`-compatible, so their methods
//! return `::leaf_core::BoxFuture` (the hand-rolled "async-across-`dyn`"
//! seam). Writing an impl by hand otherwise means the visible machinery
//! `fn f<'a>(&'a self, …) -> ::leaf_core::BoxFuture<'a, T> { Box::pin(async move { … }) }`.
//!
//! This macro lets the user write the native form
//! ```ignore
//! #[async_impl]
//! impl Runner for StartupRunner {
//!     async fn run(&self, args: &ApplicationArguments) -> Result<(), LeafError> { todo!() }
//! }
//! ```
//! and rewrites each `async fn` to the `BoxFuture` shape — emitting leaf's own
//! `::leaf_core::BoxFuture` alias (so the `Send`/lifetime bounds stay under leaf's
//! control, unlike the `async-trait` crate).
//!
//! It is **impl-only and signature-preserving**: a single lifetime is threaded across
//! the receiver and every elided reference argument (leaf's unified-`'a` trait
//! convention), so the desugared method matches the trait's declared
//! `fn f<'a>(&'a self, …) -> BoxFuture<'a, T>` signature without touching the trait
//! definition. Non-`async` methods and non-method items pass through untouched.

use proc_macro2::TokenStream;
use quote::quote;
use syn::visit_mut::{self, VisitMut};
use syn::{parse_quote, FnArg, ImplItem, ImplItemFn, ItemImpl, Lifetime, ReturnType, Type};

/// The lifetime threaded across receiver + reference args + the produced future. A
/// distinctive name avoids colliding with a user-declared lifetime on the method.
fn async_lifetime() -> Lifetime {
    parse_quote!('__leaf_async)
}

/// Fills every ELIDED lifetime in an argument type with the unified lifetime, recursively:
/// an unlabelled `&T` reference, and any explicit placeholder `'_` (including nested ones
/// like `ResolveCtx<'_>`). Named lifetimes (`'a`) are left untouched. This matches leaf's
/// unified-`'a` trait convention — `&ResolveCtx<'_>` becomes `&'a ResolveCtx<'a>` — so a
/// desugared method matches a trait signature like `&'a ResolveCtx<'a>` exactly. It is
/// STRUCTURAL (operates on the AST shape), never a type-name comparison.
struct LifetimeFiller {
    lt: Lifetime,
}

impl VisitMut for LifetimeFiller {
    fn visit_type_reference_mut(&mut self, node: &mut syn::TypeReference) {
        if node.lifetime.is_none() {
            node.lifetime = Some(self.lt.clone());
        }
        visit_mut::visit_type_reference_mut(self, node);
    }

    fn visit_lifetime_mut(&mut self, node: &mut Lifetime) {
        if node.ident == "_" {
            *node = self.lt.clone();
        }
    }
}

/// Rewrite every `async fn` method of `item` into the `BoxFuture` form; pass everything
/// else through unchanged.
pub fn expand(item: &ItemImpl) -> TokenStream {
    let mut out = item.clone();
    for impl_item in &mut out.items {
        if let ImplItem::Fn(method) = impl_item
            && method.sig.asyncness.is_some()
        {
            desugar(method);
        }
    }
    quote! { #out }
}

/// Desugar one `async fn` method in place: strip `async`, thread the lifetime over the
/// receiver and elided reference args, wrap the return in `BoxFuture`, and box the body.
fn desugar(method: &mut ImplItemFn) {
    let lt = async_lifetime();

    // `async fn` → `fn` (the boxed future carries the asynchrony).
    method.sig.asyncness = None;

    // Thread the lifetime over the receiver (`&self`/`&mut self` → `&'lt …`) and EVERY
    // elided lifetime in each argument type (recursively — `&ResolveCtx<'_>` →
    // `&'lt ResolveCtx<'lt>`). Owned args and named lifetimes are left untouched.
    let mut filler = LifetimeFiller { lt: lt.clone() };
    for arg in &mut method.sig.inputs {
        match arg {
            FnArg::Receiver(recv) => {
                if let Some((_amp, life)) = &mut recv.reference
                    && life.is_none()
                {
                    *life = Some(lt.clone());
                }
            }
            FnArg::Typed(pat_ty) => filler.visit_type_mut(&mut pat_ty.ty),
        }
    }

    // The future's output is the method's declared return (or `()`).
    let output: Type = match &method.sig.output {
        ReturnType::Default => parse_quote!(()),
        ReturnType::Type(_, ty) => (**ty).clone(),
    };
    method.sig.output = parse_quote!(-> ::leaf_core::BoxFuture<#lt, #output>);

    // Declare the lifetime on the method (front of the generic list).
    method
        .sig
        .generics
        .params
        .insert(0, syn::GenericParam::Lifetime(syn::LifetimeParam::new(lt)));

    // Box the original body as a `Send` future.
    let body = &method.block;
    method.block = parse_quote!({ ::std::boxed::Box::pin(async move #body) });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    #[test]
    fn desugars_an_async_trait_method_to_a_boxfuture() {
        let item: ItemImpl = parse_quote! {
            impl Runner for StartupRunner {
                async fn run(&self, args: &ApplicationArguments) -> Result<(), LeafError> {
                    self.go(args).await
                }
            }
        };
        let s = flat(&expand(&item));
        // `async fn` is gone; the body is boxed.
        assert!(!s.contains("asyncfnrun"), "async stripped from the signature: {s}");
        assert!(s.contains("Box::pin(asyncmove{"), "body is a boxed future: {s}");
        // The return is leaf's BoxFuture over the declared output.
        assert!(
            s.contains("->::leaf_core::BoxFuture<'__leaf_async,Result<(),LeafError>>"),
            "return is BoxFuture<'lt, Result>: {s}"
        );
        // The lifetime is threaded over `&self` and the `&` arg (unified-'a convention).
        assert!(s.contains("&'__leaf_asyncself"), "receiver carries the lifetime: {s}");
        assert!(
            s.contains("args:&'__leaf_asyncApplicationArguments"),
            "the reference arg carries the lifetime: {s}"
        );
        assert!(s.contains("fnrun<'__leaf_async"), "the lifetime is declared: {s}");
    }

    #[test]
    fn a_method_with_no_return_boxes_unit() {
        let item: ItemImpl = parse_quote! {
            impl T for S {
                async fn tick(&self) {
                    self.beat().await;
                }
            }
        };
        let s = flat(&expand(&item));
        assert!(s.contains("->::leaf_core::BoxFuture<'__leaf_async,()>"), "unit output: {s}");
    }

    #[test]
    fn a_nested_elided_lifetime_is_filled_recursively() {
        // `&ResolveCtx<'_>` must become `&'lt ResolveCtx<'lt>` to match a trait signature
        // like `fn begin<'a>(&'a self, cx: &'a ResolveCtx<'a>) -> BoxFuture<'a, _>`.
        let item: ItemImpl = parse_quote! {
            impl TransactionManager for Tm {
                async fn begin(&self, cx: &ResolveCtx<'_>) -> Result<TxState, LeafError> {
                    self.inner.begin(cx).await
                }
            }
        };
        let s = flat(&expand(&item));
        assert!(
            s.contains("cx:&'__leaf_asyncResolveCtx<'__leaf_async>"),
            "outer AND nested lifetimes are filled: {s}"
        );
    }

    #[test]
    fn a_mut_receiver_carries_the_lifetime() {
        let item: ItemImpl = parse_quote! {
            impl T for S {
                async fn poll(&mut self) -> Result<(), LeafError> { self.step().await }
            }
        };
        let s = flat(&expand(&item));
        assert!(s.contains("&'__leaf_asyncmutself"), "&mut self carries the lifetime: {s}");
    }

    #[test]
    fn an_owned_arg_keeps_its_type_and_sync_methods_pass_through() {
        let item: ItemImpl = parse_quote! {
            impl TransactionManager for Tm {
                async fn commit(&self, st: TxState) -> Result<(), LeafError> { self.inner.commit(st).await }
                fn name(&self) -> &str { "tm" }
            }
        };
        let s = flat(&expand(&item));
        // Owned arg unchanged; receiver gets the lifetime.
        assert!(s.contains("st:TxState"), "owned arg unchanged: {s}");
        assert!(s.contains("&'__leaf_asyncself,st:TxState"), "only the receiver is borrowed: {s}");
        // The sync method is untouched (no BoxFuture, no Box::pin around it).
        assert!(s.contains(r#"fnname(&self)->&str{"tm"}"#), "sync method passes through: {s}");
    }
}
