//! The declarative-advice / AOP codegen the thin `#[advisable]`/`#[aspect]`/
//! `#[advice]`/`#[pointcut]` macros call (declarative-advice phase3/09;
//! proxy-interception phase3/08).
//!
//! A `#[component]` becomes proxyable by being marked `#[advisable]` (the
//! transparent-newtype seam the proxy substrate wraps); an `#[aspect]` is a
//! `#[component]` carrying advice methods; each `#[advice(...)]`/`#[pointcut(...)]`
//! method lowers to ONE flat const advisor IDENTITY row in the frozen `ADVISORS`
//! `linkme` slice (`AdvisorRow { contract, order }`) PLUS a public pairing const
//! the leaf-boot proxy-assembly pass binds to its live [`leaf_core::AdvisorDescriptor`]
//! (the runtime row holds a `&'static dyn Pointcut` + a `MakeInterceptor` that
//! resolves the aspect bean, neither of which is const-constructible at macro time —
//! so this unit emits the minimal anti-DCE identity row + the chain-order metadata,
//! and the live `AdvisorDescriptor` binding is the proxy-assembly pass's concern).
//!
//! Every emitted path is ABSOLUTE `::leaf_core::…` so a user crate's imports cannot
//! shadow the seam (the thin-macro rule, charter §2.10). A generic aspect is a
//! Tier-0 [`EmitError`] hinting `register_proxy!(Concrete)` (a generic type has no
//! single concrete `ContractId`, exactly like a generic bean).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::descriptor::EmitError;

/// The built-in advice kinds, as DATA: each pins the advisor onto one of the
/// frozen `*_ORDER` chain-order consts (the chain-sort is `cmp_chain` over the
/// composite `ChainKey`, NEVER the slice order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdviceKind {
    /// `#[advice(before)]` — runs before the join point.
    Before,
    /// `#[advice(after)]` — runs after the join point returns or throws.
    After,
    /// `#[advice(after_returning)]` — runs after a NORMAL return.
    AfterReturning,
    /// `#[advice(after_throwing)]` — runs after an error.
    AfterThrowing,
    /// `#[advice(around)]` — the around-shaped interceptor (all kinds collapse to
    /// this at lowering; the chain sees ONE kind).
    Around,
}

impl AdviceKind {
    /// Parse the advice-kind keyword (`before`/`after`/…), defaulting to `around`.
    ///
    /// # Errors
    /// [`EmitError`] on an unrecognised keyword.
    pub fn parse(name: &str) -> Result<Self, EmitError> {
        match name {
            "before" => Ok(AdviceKind::Before),
            "after" => Ok(AdviceKind::After),
            "after_returning" => Ok(AdviceKind::AfterReturning),
            "after_throwing" => Ok(AdviceKind::AfterThrowing),
            "around" => Ok(AdviceKind::Around),
            other => Err(EmitError {
                message: format!(
                    "unknown advice kind `{other}` (expected \
                     before/after/after_returning/after_throwing/around)"
                ),
            }),
        }
    }

    /// The default chain-order [`leaf_core::OrderKey`] this kind pins to (the
    /// `DEFAULT_ORDER` floor with an `Implicit` source, since no explicit `order`
    /// was given). Around/before/after share the floor for user aspects; the
    /// built-in infrastructure advisors (tx/cache/validation) carry their own pinned
    /// `*_ORDER` consts emitted by `#[cacheable]`/leaf-tx/leaf-validation directly.
    #[must_use]
    pub fn order_tokens(self) -> TokenStream {
        quote! {
            ::leaf_core::OrderKey {
                value: ::leaf_core::DEFAULT_ORDER,
                source: ::leaf_core::OrderSource::Implicit,
            }
        }
    }
}

/// The parsed `#[advisable]` / `#[aspect]` attribute arguments (currently the
/// closed `order = <int>` axis; the pointcut is read off the method, not here).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdvisorArgs {
    /// An explicit integer chain order (`order = 100`); `None` uses the kind floor.
    pub order: Option<i32>,
}

/// Parse the `#[advice(kind, order = N)]` attribute body into the advice kind +
/// optional explicit order.
///
/// The FIRST bare ident (if any) is the advice kind (`before`/`around`/…),
/// defaulting to `around`; `order = <int>` sets the explicit chain order.
///
/// # Errors
/// [`EmitError`] on a malformed body, an unknown kind, or a non-integer `order`.
pub fn parse_advice_args(attr: TokenStream) -> Result<(AdviceKind, AdvisorArgs), EmitError> {
    let mut kind = AdviceKind::Around;
    let mut args = AdvisorArgs::default();
    if attr.is_empty() {
        return Ok((kind, args));
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[advice] arguments: {e}"),
    })?;
    for expr in &exprs {
        match expr {
            // A bare ident => the advice kind keyword.
            syn::Expr::Path(p) => {
                let name = p
                    .path
                    .get_ident()
                    .map(ToString::to_string)
                    .ok_or_else(|| EmitError {
                        message: "an #[advice] kind must be a bare identifier".into(),
                    })?;
                kind = AdviceKind::parse(&name)?;
            }
            // `order = <int>`.
            syn::Expr::Assign(assign) => {
                let key = assign_ident(&assign.left)?;
                if key != "order" {
                    return Err(EmitError {
                        message: format!("unknown #[advice] argument `{key}` (expected `order`)"),
                    });
                }
                args.order = Some(int_value(&assign.right)?);
            }
            other => {
                return Err(EmitError {
                    message: format!(
                        "unexpected #[advice] argument `{}` (expected a kind keyword or `order = N`)",
                        quote! { #other }
                    ),
                });
            }
        }
    }
    Ok((kind, args))
}

/// Parse the `#[advisable]` / `#[aspect]` attribute body — only the optional
/// `order = <int>` axis (the headline marker carries no other config).
///
/// # Errors
/// [`EmitError`] on a malformed body or an unknown key.
pub fn parse_advisor_args(attr: TokenStream) -> Result<AdvisorArgs, EmitError> {
    let mut args = AdvisorArgs::default();
    if attr.is_empty() {
        return Ok(args);
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed advisor arguments: {e}"),
    })?;
    for expr in &exprs {
        match expr {
            syn::Expr::Assign(assign) => {
                let key = assign_ident(&assign.left)?;
                if key != "order" {
                    return Err(EmitError {
                        message: format!("unknown advisor argument `{key}` (expected `order`)"),
                    });
                }
                args.order = Some(int_value(&assign.right)?);
            }
            other => {
                return Err(EmitError {
                    message: format!(
                        "unexpected advisor argument `{}` (expected `order = N`)",
                        quote! { #other }
                    ),
                });
            }
        }
    }
    Ok(args)
}

/// Emit the const advisor artifact for one advice/aspect element `ident`: the
/// minimal anti-DCE `::leaf_core::AdvisorRow { contract, order }` submitted into
/// the frozen `ADVISORS` slice, plus a PUBLIC pairing const carrying the chain
/// `::leaf_core::OrderKey` (named `__leaf_advisor_<Ident>` so the leaf-boot
/// proxy-assembly pass can bind the live `AdvisorDescriptor` to the row's identity).
///
/// `is_generic` makes the element a Tier-0 [`EmitError`] (a generic aspect has no
/// single concrete `ContractId`), hinting the `register_proxy!(Concrete)` escape.
///
/// # Errors
/// [`EmitError`] when the target is generic.
pub fn emit_advisor(
    ident: &str,
    kind: AdviceKind,
    args: &AdvisorArgs,
    is_generic: bool,
) -> Result<TokenStream, EmitError> {
    if is_generic {
        return Err(EmitError {
            message: format!(
                "`{ident}` is a generic aspect: a generic aspect has no single \
                 concrete type to register as a const advisor row. Register a \
                 concrete instantiation with `register_proxy!({ident}<Concrete>)`."
            ),
        });
    }
    let mangled = mangle(ident);
    let row_ident = format_ident!("__LEAF_ADVISOR_{}", mangled);
    let order_ident = format_ident!("__leaf_advisor_{}", mangled);
    let order = match args.order {
        // An explicit `order = N` is Annotation-sourced (it beats an Implicit floor
        // at an equal value, per the OrderSource tie-break).
        Some(n) => quote! {
            ::leaf_core::OrderKey {
                value: #n,
                source: ::leaf_core::OrderSource::Annotation,
            }
        },
        None => kind.order_tokens(),
    };
    // The advisor's stable identity is its module-qualified `module::Ident`
    // (a thin macro cannot resolve the module at expansion, so it is deferred to
    // the const initializer at the use site, exactly like a bean's contract).
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };
    Ok(quote! {
        // The PUBLIC chain-order pairing const: the leaf-boot proxy-assembly pass
        // reads this OrderKey beside the row to build the live AdvisorDescriptor
        // (whose &'static dyn Pointcut + MakeInterceptor are NOT const-constructible
        // at macro time — the bean bridge resolves the aspect at refresh).
        #[allow(non_upper_case_globals)]
        pub const #order_ident: ::leaf_core::OrderKey = #order;
        // The minimal anti-DCE advisor IDENTITY row in the frozen ADVISORS slice
        // (a dropped advisor is silently un-applied — the expected-vs-found
        // self-check catches it). The chain SORT is cmp_chain over the composite
        // ChainKey, never this slice's link order.
        #[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISORS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::AdvisorRow = ::leaf_core::AdvisorRow {
            contract: #contract,
            order: #order_ident,
        };
    })
}

/// Emit the PUBLIC advisor → runtime-advice pairing for an `#[aspect]` STRUCT bean:
/// one const `::leaf_core::AdvisorPairingRow` submitted into the auto-collect
/// `ADVISOR_PAIRINGS` slice, so the run pipeline auto-collects the LIVE advisor (the
/// pointcut + the `make_interceptor` bean bridge) with NO hand-assembled
/// `.with_advisors`.
///
/// Unlike [`emit_advisor`] (which emits only the anti-DCE `ADVISORS` IDENTITY row +
/// the chain-order const), this row carries the two pieces leaf-boot's hand-written
/// `AdvisorPairing` used to supply — both const-constructible for an aspect bean:
///
/// - **the pointcut** — `::leaf_core::Anything` (the aspect advises every matched join
///   point; a finer pointcut is the `#[advice]`-method form's future concern), a unit
///   struct usable as a `&'static dyn Pointcut`;
/// - **the `make_interceptor`** — a const `fn(&dyn Container) -> BoxFuture<Arc<dyn
///   Interceptor>>` that RESOLVES the aspect bean by its `ContractId` and downcasts it
///   to `Arc<dyn Interceptor>` (the aspect bean IS the interceptor). The user writes
///   `impl Interceptor for <Aspect>`; this wires its discovery + resolution.
///
/// The `order`/`role` mirror the `ADVISORS` identity row (`order = args.order` else
/// the around floor; `role = Application` — a user aspect).
#[must_use]
pub fn emit_advisor_pairing(ident: &str, self_ty: &syn::Type, args: &AdvisorArgs) -> TokenStream {
    let mangled = mangle(ident);
    let pairing_row_ident = format_ident!("__LEAF_ADVISOR_PAIRING_{}", mangled);
    let order = match args.order {
        Some(n) => quote! {
            ::leaf_core::OrderKey {
                value: #n,
                source: ::leaf_core::OrderSource::Annotation,
            }
        },
        None => AdviceKind::Around.order_tokens(),
    };
    // The aspect's module-qualified contract (the ADVISOR_PAIRINGS JOIN key + the
    // ContractId the make_interceptor resolves the aspect bean by), built at the use
    // site exactly like the bean's Descriptor contract.
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };
    quote! {
        // Submit the LIVE advisor pairing into ADVISOR_PAIRINGS (the auto-collect
        // substrate) keyed by ContractId, so the run pipeline reifies it into an
        // AdvisorDescriptor with no hand-assembled `.with_advisors`. Same re-export
        // pattern as COMPONENTS.
        #[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #pairing_row_ident: ::leaf_core::AdvisorPairingRow =
            ::leaf_core::AdvisorPairingRow {
                contract: #contract,
                order: #order,
                role: ::leaf_core::Role::Application,
                // The aspect advises every matched join point (the const pointcut).
                pointcut: &::leaf_core::Anything,
                // The bean bridge: resolve the aspect bean by ContractId + upcast it to
                // Arc<dyn Interceptor> (the aspect bean IS the interceptor). The
                // resolved Published carries the engine-resolvable Arc<#self_ty>; we
                // downcast it and re-wrap as the trait object.
                make_interceptor: |__c: &dyn ::leaf_core::Container| {
                    ::std::boxed::Box::pin(async move {
                        let __published = __c
                            .resolve(
                                ::leaf_core::BeanKey::ByContract(#contract),
                                ::leaf_core::Strictness::Strict,
                                ::leaf_core::Cardinality::Single,
                            )
                            .await?;
                        let __mismatch = || ::leaf_core::LeafError::new(
                            ::leaf_core::ErrorKind::ConstructionFailed,
                        );
                        let __erased = __published.into_shared().ok_or_else(__mismatch)?;
                        match __erased.downcast::<#self_ty>() {
                            ::core::result::Result::Ok(__aspect) => ::core::result::Result::Ok(
                                __aspect as ::std::sync::Arc<dyn ::leaf_core::Interceptor>,
                            ),
                            ::core::result::Result::Err(_) => {
                                ::core::result::Result::Err(__mismatch())
                            }
                        }
                    })
                },
            };
    }
}

/// One advisable method's join-point spec the macro emits (the const input
/// [`emit_join_points`] lowers to a `::leaf_core::MethodJoinPointSpec`).
///
/// A bare `#[advisable]`/`#[aspect]` STRUCT attr cannot enumerate the bean's methods
/// (a struct-position attr sees no impl), so the struct form emits NO method specs;
/// a method-aware form (an impl-block lowering or the binary's collected method
/// table) supplies these.
#[derive(Clone, Debug)]
pub struct MethodSpec {
    /// The canonical `Bean::method` path minting the method's stable `MethodKey`.
    pub method_path: String,
    /// The method's callable ident on the concrete bean (e.g. `place_order`) — what
    /// the [`emit_method_table`] downcast thunk invokes on the downcast target. The
    /// join-point form ([`emit_join_points`]) does not read it; defaults to the last
    /// `::`-segment of `method_path` via [`MethodSpec::call_ident`] when unset.
    pub method_ident: Option<String>,
    /// `true` iff the method is `async fn` — the thunk `.await`s the call before
    /// packing the [`ErasedRet`](leaf_core::ErasedRet) (a sync method is awaited via an immediate value).
    pub is_async: bool,
    /// The method's argument types (lowered through the const `TypeId`-of seam).
    pub arg_types: Vec<syn::Type>,
    /// The method's return type (lowered through the const `TypeId`-of seam).
    pub ret_type: syn::Type,
}

impl MethodSpec {
    /// The callable method ident — the explicit [`MethodSpec::method_ident`], else the
    /// last `::`-segment of [`MethodSpec::method_path`] (`"Svc::place"` → `"place"`).
    #[must_use]
    pub fn call_ident(&self) -> &str {
        match &self.method_ident {
            Some(id) => id.as_str(),
            None => self.method_path.rsplit("::").next().unwrap_or(&self.method_path),
        }
    }
}

/// Emit the PUBLIC per-bean join-point spec pairing const for an advisable bean: a
/// const `::leaf_core::BeanJoinPointsSpec` named `__leaf_joinpoints_<Ident>` carrying
/// the bean's concrete `TypeId` (the `within::<T>()` pointcut key), a reference to the
/// bean's OWN flat `AnnotationMetadata` static (`__LEAF_META_<Ident>`, the
/// `annotated::<A>()` pointcut key — the SAME static the descriptor emitter emits
/// beside the row), and one const `::leaf_core::MethodJoinPointSpec` per advisable
/// method.
///
/// This is the proxy analogue of the `__leaf_seed_<Ident>` ProviderSeed / the
/// `__leaf_advisor_<Ident>` order pairing: leaf-boot's proxy-assembly pass JOINs it
/// to the bean's frozen `BeanId` by `ContractId` and reifies it into the runtime
/// `BeanJoinPoints` that `ProxyPlan::freeze` runs pointcuts over — so the proxy plan
/// is built from REAL macro-emitted per-bean data, never a hand-mirrored view.
#[must_use]
pub fn emit_join_points(ident: &str, self_ty: &syn::Type, methods: &[MethodSpec]) -> TokenStream {
    let mangled = mangle(ident);
    let spec_ident = format_ident!("__leaf_joinpoints_{}", mangled);
    let spec_row_ident = format_ident!("__LEAF_JOINPOINT_PAIRING_{}", mangled);
    let meta_ident = format_ident!("__LEAF_META_{}", mangled);
    let bean_type = quote! { const { ::core::any::TypeId::of::<#self_ty>() } };
    // The advisable bean's module-qualified contract (the JOINPOINT_PAIRINGS JOIN
    // key), built at the use site exactly like the bean's Descriptor contract.
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };

    let method_rows = methods.iter().map(|m| {
        let path = &m.method_path;
        let ret = &m.ret_type;
        let args = m.arg_types.iter().map(|a| quote! { const { ::core::any::TypeId::of::<#a>() } });
        quote! {
            ::leaf_core::MethodJoinPointSpec {
                method: ::leaf_core::MethodKey::of(#path),
                arg_types: &[ #(#args),* ],
                ret_type: const { ::core::any::TypeId::of::<#ret>() },
            }
        }
    });

    quote! {
        // The PUBLIC per-bean join-point spec pairing const (the const twin of
        // BeanJoinPoints): leaf-boot's proxy-assembly pass JOINs it by ContractId,
        // reifies it (building each method's SmallVec), and runs every admitted
        // advisor's pointcut over it at ProxyPlan::freeze. The markers reference the
        // bean's OWN __LEAF_META_<Ident> static (annotation-model owns it).
        #[allow(non_upper_case_globals)]
        pub const #spec_ident: ::leaf_core::BeanJoinPointsSpec = ::leaf_core::BeanJoinPointsSpec {
            bean_type: #bean_type,
            markers: &#meta_ident,
            methods: &[ #(#method_rows),* ],
        };
        // Submit the spec into JOINPOINT_PAIRINGS (the auto-collect substrate) keyed
        // by ContractId, so the proxy-assembly pass finds it with no hand-assembled
        // `.with_join_points`. Same re-export pattern as COMPONENTS.
        #[::leaf_core::linkme::distributed_slice(::leaf_core::JOINPOINT_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #spec_row_ident: ::leaf_core::JoinPointPairingRow =
            ::leaf_core::JoinPointPairingRow {
                contract: #contract,
                spec: &#spec_ident,
            };
    }
}

/// Emit the PUBLIC per-bean METHOD TABLE pairing const for an advisable bean: a
/// `&'static ::leaf_core::MethodTable` named `__leaf_methods_<Ident>` carrying one
/// `::leaf_core::MethodEntry` per advised method, each with a generated DOWNCAST
/// THUNK that drives the REAL method over the resolved [`leaf_core::ErasedBean`].
///
/// This is the proxy analogue of `__leaf_joinpoints_<Ident>` (the join-point spec the
/// `ProxyPlan` matches pointcuts over): leaf-boot's `InstalledProxies::install_with_tables`
/// JOINs it to the bean's frozen `BeanId` by `ContractId`, and
/// `InstalledProxies::invoke` terminates the auto-installed `AdviceChain` in the
/// matching `MethodEntry.invoke` thunk — so a `#[advisable]`/`#[aspect]` bean is advised
/// with NO hand-written `MethodTable`/`MethodEntry` in user code.
///
/// ## The thunk shape (the settled erased dispatch ABI)
///
/// Each thunk realizes the leaf-core `ErasedArgs`/`ErasedRet` ABI (proxy-interception
/// §R2 erased fallback, the design's Phase-4 measure now settled): the per-bean glue
/// keeps the COMMON typed path a single owned `Box<dyn Any+Send+Sync>` move (never a
/// per-arg box). The carrier is the method's POSITIONAL ARGUMENT TUPLE
/// `(A0, A1, …)` — `()` for a no-arg method, `(A0,)` for one arg — so a thunk:
///
/// 1. `::std::sync::Arc::clone(bean).downcast::<#self_ty>()` → the concrete target
///    (a `DowncastMismatch` if the published bean is not the expected type);
/// 2. `args.unpack::<(A0, …)>()` → the typed tuple (a `DowncastMismatch` on a
///    carrier-type mismatch), destructured into the positional params;
/// 3. calls (`.await`s, if `is_async`) the real method on the downcast target;
/// 4. `::leaf_core::ErasedRet::pack(ret)` → the erased return the chain unwinds.
///
/// ## The advised-arg bound (`Clone + Send + Sync + 'static`)
///
/// Each `Ai` is `Clone + Send + Sync + 'static` — the LOAD-BEARING advised-method
/// argument constraint (Spring's args are re-invocable objects): the args ride
/// `Call.args` so every interceptor INSPECTS them (cache-key-from-a-param, a `@Valid`
/// arg), and a REPLAYABLE `Next::proceed` (retry) re-runs the args-bearing target by
/// re-cloning a fresh copy off `Call.args` per attempt. `ErasedArgs::pack` carries the
/// monomorphized clone thunk, so the tail re-supplies fresh args without the take-once
/// cell — args-bearing declarative retry is now sound, not a v1 limitation. A non-`Clone`
/// arg type is a compile error at the `pack::<(A0, …)>` site (the `Clone` bound), steered
/// to "make the argument `Clone`" — the documented advised-method constraint.
///
/// A bare struct form supplies no methods (an empty table); the method-aware
/// impl-block form / the binary supplies the per-method specs.
#[must_use]
pub fn emit_method_table(ident: &str, self_ty: &syn::Type, methods: &[MethodSpec]) -> TokenStream {
    let mangled = mangle(ident);
    let table_ident = format_ident!("__leaf_methods_{}", mangled);
    let table_row_ident = format_ident!("__LEAF_METHOD_TABLE_PAIRING_{}", mangled);
    // The advised bean's module-qualified contract (the METHOD_TABLE_PAIRINGS JOIN
    // key), built at the use site exactly like the bean's Descriptor contract.
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };

    // One generated downcast-thunk fn per advised method + the MethodEntry row that
    // points at it. The thunk fn is named off the bean + method so two methods on one
    // bean (and two beans in one module) never collide.
    let mut thunk_fns = TokenStream::new();
    let mut entries = TokenStream::new();
    for method in methods {
        let path = &method.method_path;
        let call_ident = format_ident!("{}", method.call_ident());
        let thunk_ident = format_ident!("__leaf_invoke_{}_{}", mangled, mangle(method.call_ident()));

        // The positional arg binding idents + the tuple type the carrier unpacks to.
        let arg_idents: Vec<syn::Ident> =
            (0..method.arg_types.len()).map(|i| format_ident!("__a{}", i)).collect();
        let arg_tys = &method.arg_types;
        let tuple_ty = quote! { ( #( #arg_tys, )* ) };
        let unpack_pat = quote! { ( #( #arg_idents, )* ) };

        // The real-method call on the downcast target; `.await` iff the method is async.
        let call = quote! { __target.#call_ident( #( #arg_idents ),* ) };
        let invoke_call = if method.is_async {
            quote! { #call.await }
        } else {
            quote! { #call }
        };

        thunk_fns.extend(quote! {
            // The macro-emitted downcast thunk: unpack the ErasedArgs tuple, downcast
            // the target to the concrete bean, call (await) the real method, pack the
            // ErasedRet. The single owned-tuple move IS the typed common path (no
            // per-arg box); a carrier/target mismatch is a loud DowncastMismatch.
            #[allow(non_snake_case)]
            fn #thunk_ident(
                __bean: &::leaf_core::ErasedBean,
                __args: ::leaf_core::ErasedArgs,
                __cx: &::leaf_core::ResolveCtx<'_>,
            ) -> ::leaf_core::BoxFuture<'static, ::core::result::Result<::leaf_core::ErasedRet, ::leaf_core::AdviceError>> {
                let __ = __cx;
                let __downcast = ::std::sync::Arc::clone(__bean).downcast::<#self_ty>();
                ::std::boxed::Box::pin(async move {
                    let __target = __downcast.map_err(|_| {
                        ::leaf_core::AdviceError::DowncastMismatch {
                            method: ::leaf_core::MethodKey::of(#path),
                        }
                    })?;
                    let #unpack_pat = __args.unpack::<#tuple_ty>().map_err(|_| {
                        ::leaf_core::AdviceError::DowncastMismatch {
                            method: ::leaf_core::MethodKey::of(#path),
                        }
                    })?;
                    ::core::result::Result::Ok(::leaf_core::ErasedRet::pack(#invoke_call))
                })
            }
        });

        entries.extend(quote! {
            ::leaf_core::MethodEntry {
                key: ::leaf_core::MethodKey::of(#path),
                invoke: #thunk_ident,
            },
        });
    }

    quote! {
        #thunk_fns
        // The PUBLIC per-bean method table pairing const (the downcast-thunk index):
        // leaf-boot's InstalledProxies::install_with_tables JOINs it by ContractId, and
        // InstalledProxies::invoke routes a call by MethodKey through the auto-installed
        // chain, terminating in the matching MethodEntry.invoke thunk.
        #[allow(non_upper_case_globals)]
        pub static #table_ident: &::leaf_core::MethodTable =
            &::leaf_core::MethodTable(&[ #entries ]);
        // Submit the table into METHOD_TABLE_PAIRINGS (the auto-collect substrate)
        // keyed by ContractId, so the auto-proxy install finds it with no
        // hand-assembled `.with_method_tables`. Same re-export pattern as COMPONENTS.
        #[::leaf_core::linkme::distributed_slice(::leaf_core::METHOD_TABLE_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #table_row_ident: ::leaf_core::MethodTablePairingRow =
            ::leaf_core::MethodTablePairingRow {
                contract: #contract,
                table: #table_ident,
            };
    }
}

/// The leading-ident name of a `register_proxy!(Concrete)` type (`Aspect<u32>` →
/// `Aspect`), used as the concrete advisor's identity base.
///
/// # Errors
/// [`EmitError`] if the type has no nameable leading ident.
pub fn proxy_ident(ty: &syn::Type) -> Result<String, EmitError> {
    match ty {
        syn::Type::Path(tp) => tp
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .ok_or_else(|| EmitError {
                message: "register_proxy! expects a concrete type with a nameable identifier".into(),
            }),
        _ => Err(EmitError {
            message: "register_proxy! expects a concrete type with a nameable identifier".into(),
        }),
    }
}

/// A spans-free, identifier-safe mangling of an ident for emitted helper names.
fn mangle(ident: &str) -> syn::Ident {
    let safe: String = ident
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    syn::Ident::new(&safe, proc_macro2::Span::call_site())
}

/// The bare ident of an assignment left-hand side (`order = N` → `order`).
fn assign_ident(expr: &syn::Expr) -> Result<String, EmitError> {
    match expr {
        syn::Expr::Path(p) => p
            .path
            .get_ident()
            .map(ToString::to_string)
            .ok_or_else(|| EmitError {
                message: "a named argument must use a bare identifier key".into(),
            }),
        _ => Err(EmitError {
            message: "a named argument must use a bare identifier key".into(),
        }),
    }
}

/// The integer value of an `order = <int>` right-hand side (allowing a leading `-`).
fn int_value(expr: &syn::Expr) -> Result<i32, EmitError> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(i), .. }) => {
            i.base10_parse::<i32>().map_err(|e| EmitError {
                message: format!("`order` must be an i32 integer: {e}"),
            })
        }
        syn::Expr::Unary(syn::ExprUnary { op: syn::UnOp::Neg(_), expr, .. }) => {
            Ok(-int_value(expr)?)
        }
        other => Err(EmitError {
            message: format!("`order` must be an integer literal, got `{}`", quote! { #other }),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    #[test]
    fn emits_a_live_advisor_pairing_into_the_advisor_pairings_slice() {
        // The headline auto-collect closure: an #[aspect] struct ALSO emits a LIVE
        // ::leaf_core::AdvisorPairingRow into ADVISOR_PAIRINGS — the const pointcut +
        // the make_interceptor bean bridge the hand `.with_advisors` table used to
        // supply, so the run pipeline auto-collects the advisor with no `.with_advisors`.
        let ty: syn::Type = syn::parse_str("AuditAspect").expect("a type");
        let ts = emit_advisor_pairing("AuditAspect", &ty, &AdvisorArgs::default());
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]"),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::AdvisorPairingRow{"), "got: {s}");
        // The const pointcut is `Anything` (the aspect advises every matched join point).
        assert!(s.contains("pointcut:&::leaf_core::Anything"), "got: {s}");
        // The make_interceptor RESOLVES the aspect bean + upcasts it to Arc<dyn Interceptor>.
        assert!(s.contains("make_interceptor:"), "got: {s}");
        assert!(s.contains("BeanKey::ByContract"), "got: {s}");
        assert!(s.contains(".downcast::<AuditAspect>()"), "got: {s}");
        assert!(s.contains("dyn::leaf_core::Interceptor"), "got: {s}");
        // A user aspect is Application-role at the around-order floor by default.
        assert!(s.contains("role:::leaf_core::Role::Application"), "got: {s}");
    }

    #[test]
    fn an_explicit_aspect_order_rides_the_advisor_pairing_row() {
        let args = AdvisorArgs { order: Some(50) };
        let ty: syn::Type = syn::parse_str("A").expect("a type");
        let s = flat(&emit_advisor_pairing("A", &ty, &args));
        assert!(s.contains("value:50i32"), "got: {s}");
        assert!(s.contains("source:::leaf_core::OrderSource::Annotation"), "got: {s}");
    }

    #[test]
    fn emits_an_absolute_core_advisor_row_into_the_advisors_slice() {
        // The headline: an aspect lowers to one const ::leaf_core::AdvisorRow
        // submitted into the frozen ADVISORS slice via the re-exported
        // ::leaf_core::linkme attr path + crate override, the SLICE absolute
        // ::leaf_core::ADVISORS.
        let ts = emit_advisor("TxAspect", AdviceKind::Around, &AdvisorArgs::default(), false)
            .expect("a concrete aspect emits");
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISORS)]"),
            "got: {s}"
        );
        assert!(s.contains("#[linkme(crate=::leaf_core::linkme)]"), "got: {s}");
        assert!(s.contains("::leaf_core::AdvisorRow{"), "got: {s}");
    }

    #[test]
    fn advisor_contract_is_module_qualified_at_the_definition_site() {
        // A thin macro cannot resolve the aspect's module at expansion, so the
        // advisor identity is module-qualified at the definition site.
        let s = flat(
            &emit_advisor("TxAspect", AdviceKind::Around, &AdvisorArgs::default(), false)
                .expect("emits"),
        );
        assert!(
            s.contains(
                r#"::leaf_core::ContractId::of(::core::concat!(::core::module_path!(),"::","TxAspect"))"#
            ),
            "got: {s}"
        );
    }

    #[test]
    fn the_chain_order_rides_a_public_pairing_const() {
        // The chain OrderKey is a PUBLIC const under the deterministic pairing name
        // so the leaf-boot proxy-assembly pass can bind the live AdvisorDescriptor.
        let s = flat(
            &emit_advisor("TxAspect", AdviceKind::Around, &AdvisorArgs::default(), false)
                .expect("emits"),
        );
        assert!(
            s.contains("pubconst__leaf_advisor_TxAspect:::leaf_core::OrderKey"),
            "got: {s}"
        );
        // The default around order is the DEFAULT_ORDER floor with an Implicit source.
        assert!(s.contains("value:::leaf_core::DEFAULT_ORDER"), "got: {s}");
        assert!(s.contains("source:::leaf_core::OrderSource::Implicit"), "got: {s}");
    }

    #[test]
    fn an_explicit_order_overrides_the_kind_floor() {
        let args = AdvisorArgs { order: Some(100) };
        let s = flat(&emit_advisor("A", AdviceKind::Around, &args, false).expect("emits"));
        assert!(s.contains("value:100i32"), "got: {s}");
        assert!(s.contains("source:::leaf_core::OrderSource::Annotation"), "got: {s}");
        assert!(!s.contains("::leaf_core::DEFAULT_ORDER"), "explicit order wins: {s}");
    }

    #[test]
    fn a_negative_explicit_order_is_allowed() {
        let (_, args) = parse_advice_args(syn::parse_str("around, order = -50").expect("tokens"))
            .expect("parses");
        assert_eq!(args.order, Some(-50));
    }

    #[test]
    fn a_generic_aspect_is_a_hard_error_with_a_register_proxy_hint() {
        let err = emit_advisor("Generic", AdviceKind::Around, &AdvisorArgs::default(), true)
            .expect_err("a generic aspect must hard-error");
        assert!(err.message.contains("generic"), "got: {}", err.message);
        assert!(err.message.contains("register_proxy!"), "got: {}", err.message);
    }

    #[test]
    fn advice_kind_keyword_parses_and_defaults_to_around() {
        let (kind, _) = parse_advice_args(TokenStream::new()).expect("empty parses");
        assert_eq!(kind, AdviceKind::Around);
        let (kind, _) = parse_advice_args(syn::parse_str("before").expect("tokens")).expect("parses");
        assert_eq!(kind, AdviceKind::Before);
        let (kind, _) =
            parse_advice_args(syn::parse_str("after_throwing").expect("tokens")).expect("parses");
        assert_eq!(kind, AdviceKind::AfterThrowing);
    }

    #[test]
    fn an_unknown_advice_kind_is_a_hard_error() {
        let err = parse_advice_args(syn::parse_str("sideways").expect("tokens"))
            .expect_err("unknown kind errors");
        assert!(err.message.contains("unknown advice kind"), "got: {}", err.message);
    }

    #[test]
    fn an_unknown_advisor_arg_is_a_hard_error() {
        let err = parse_advisor_args(syn::parse_str(r#"bogus = 1"#).expect("tokens"))
            .expect_err("unknown arg errors");
        assert!(err.message.contains("unknown advisor argument"), "got: {}", err.message);
    }

    #[test]
    fn proxy_ident_reads_the_leading_ident_of_a_concrete_type() {
        let ty: syn::Type = syn::parse_str("Aspect<u32>").expect("a type");
        assert_eq!(proxy_ident(&ty).expect("nameable"), "Aspect");
    }

    // ── per-bean join-point spec (the proxy plan input) ──────────────────────────

    fn ty(s: &str) -> syn::Type {
        syn::parse_str(s).expect("a valid type")
    }

    #[test]
    fn emits_a_public_bean_join_points_spec_pairing_const() {
        // The headline: an #[advisable] bean emits a PUBLIC const
        // `__leaf_joinpoints_<Ident>` of the macro-emitted ::leaf_core::BeanJoinPointsSpec
        // type (the const twin of BeanJoinPoints), so leaf-boot's ProxyPlan::freeze can
        // JOIN it by ContractId and run pointcuts over the bean — the same pairing-const
        // pattern as the __leaf_seed_<Ident> ProviderSeed.
        let ts = emit_join_points("OrderService", &ty("OrderService"), &[]);
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        assert!(
            s.contains("pubconst__leaf_joinpoints_OrderService:::leaf_core::BeanJoinPointsSpec"),
            "got: {s}"
        );
        // The bean_type rides the const TypeId-of seam.
        assert!(
            s.contains("bean_type:const{::core::any::TypeId::of::<OrderService>()}"),
            "got: {s}"
        );
        // The markers reference the bean's OWN flat AnnotationMetadata static (the
        // SAME `__LEAF_META_<Ident>` the descriptor emitter emits — annotated::<A>()).
        assert!(s.contains("markers:&__LEAF_META_OrderService"), "got: {s}");
        // A bare struct form has no enumerable methods (the struct attr cannot see
        // them — an honest empty spec; the impl-aware form / binary supplies methods).
        assert!(s.contains("methods:&[]"), "got: {s}");
        // The spec is ALSO auto-collected into JOINPOINT_PAIRINGS keyed by ContractId
        // (the COMPONENTS auto-collect substrate, extended) so the proxy-assembly pass
        // finds it with no hand-assembled `.with_join_points`.
        assert!(
            s.contains(
                "#[::leaf_core::linkme::distributed_slice(::leaf_core::JOINPOINT_PAIRINGS)]"
            ),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::JoinPointPairingRow{contract:"), "got: {s}");
        assert!(s.contains("spec:&__leaf_joinpoints_OrderService"), "got: {s}");
    }

    #[test]
    fn join_points_spec_carries_declared_method_rows() {
        // The method-aware form (the impl-block / binary path) emits one const
        // MethodJoinPointSpec per advisable method: its MethodKey + const arg/ret types.
        let methods = vec![MethodSpec {
            method_path: "OrderService::place_order".into(),
            method_ident: Some("place_order".into()),
            is_async: false,
            arg_types: vec![ty("i64")],
            ret_type: ty("i64"),
        }];
        let s = flat(&emit_join_points("OrderService", &ty("OrderService"), &methods));
        assert!(s.contains("::leaf_core::MethodJoinPointSpec"), "got: {s}");
        assert!(
            s.contains(r#"::leaf_core::MethodKey::of("OrderService::place_order")"#),
            "got: {s}"
        );
        assert!(s.contains("ret_type:const{::core::any::TypeId::of::<i64>()}"), "got: {s}");
        assert!(s.contains("arg_types:&[const{::core::any::TypeId::of::<i64>()}]"), "got: {s}");
    }

    // ── per-bean method table (the transparent-proxy downcast thunks) ─────────────

    #[test]
    fn method_spec_call_ident_falls_back_to_the_last_path_segment() {
        let m = MethodSpec {
            method_path: "OrderService::place_order".into(),
            method_ident: None,
            is_async: false,
            arg_types: vec![],
            ret_type: ty("()"),
        };
        assert_eq!(m.call_ident(), "place_order");
        let m2 = MethodSpec { method_ident: Some("renamed".into()), ..m };
        assert_eq!(m2.call_ident(), "renamed");
    }

    #[test]
    fn emits_a_public_method_table_pairing_static() {
        // The headline: an advisable bean emits a PUBLIC `__leaf_methods_<Ident>` of
        // the runtime `&'static ::leaf_core::MethodTable` type (one downcast-thunk
        // MethodEntry per advised method), so leaf-boot's InstalledProxies JOINs it by
        // ContractId — the proxy analogue of `__leaf_methods_<Ident>` the auto-wire
        // test previously hand-wrote.
        let methods = vec![MethodSpec {
            method_path: "OrderService::place_order".into(),
            method_ident: Some("place_order".into()),
            is_async: false,
            arg_types: vec![ty("i64")],
            ret_type: ty("i64"),
        }];
        let ts = emit_method_table("OrderService", &ty("OrderService"), &methods);
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        assert!(
            s.contains(
                "pubstatic__leaf_methods_OrderService:&::leaf_core::MethodTable=&::leaf_core::MethodTable(&["
            ),
            "got: {s}"
        );
        // One MethodEntry keyed by the method's stable MethodKey, pointing at the thunk.
        assert!(s.contains("::leaf_core::MethodEntry{"), "got: {s}");
        assert!(
            s.contains(r#"key:::leaf_core::MethodKey::of("OrderService::place_order")"#),
            "got: {s}"
        );
        assert!(s.contains("invoke:__leaf_invoke_OrderService_place_order"), "got: {s}");
        // The table is ALSO auto-collected into METHOD_TABLE_PAIRINGS keyed by
        // ContractId (the COMPONENTS auto-collect substrate, extended) so the
        // auto-proxy install finds it with no hand-assembled `.with_method_tables`.
        assert!(
            s.contains(
                "#[::leaf_core::linkme::distributed_slice(::leaf_core::METHOD_TABLE_PAIRINGS)]"
            ),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::MethodTablePairingRow{contract:"), "got: {s}");
        assert!(s.contains("table:__leaf_methods_OrderService"), "got: {s}");
    }

    #[test]
    fn method_table_thunk_downcasts_unpacks_calls_and_packs() {
        // The thunk realizes the ErasedArgs/ErasedRet ABI: downcast the ErasedBean to
        // the concrete bean, unpack the positional arg tuple, call the real method,
        // pack the ErasedRet — the exact shape the auto-wire test hand-wrote.
        let methods = vec![MethodSpec {
            method_path: "OrderService::place_order".into(),
            method_ident: Some("place_order".into()),
            is_async: false,
            arg_types: vec![ty("i64")],
            ret_type: ty("i64"),
        }];
        let s = flat(&emit_method_table("OrderService", &ty("OrderService"), &methods));
        // The thunk fn has the MethodEntry.invoke fn-pointer signature.
        assert!(
            s.contains(
                "fn__leaf_invoke_OrderService_place_order(__bean:&::leaf_core::ErasedBean,__args:::leaf_core::ErasedArgs,__cx:&::leaf_core::ResolveCtx<'_>,)"
            ),
            "got: {s}"
        );
        // (1) downcast the erased bean to the concrete bean type.
        assert!(
            s.contains("::std::sync::Arc::clone(__bean).downcast::<OrderService>()"),
            "got: {s}"
        );
        // (2) unpack the POSITIONAL ARG TUPLE (one arg => a 1-tuple).
        assert!(s.contains("__args.unpack::<(i64,)>()"), "got: {s}");
        // (3) call the real method on the downcast target with the unpacked args.
        assert!(s.contains("__target.place_order(__a0)"), "got: {s}");
        // (4) pack the typed return into the ErasedRet.
        assert!(s.contains("::leaf_core::ErasedRet::pack(__target.place_order(__a0))"), "got: {s}");
        // A mismatch on either downcast is a loud DowncastMismatch (never silent).
        assert!(s.contains("::leaf_core::AdviceError::DowncastMismatch"), "got: {s}");
    }

    #[test]
    fn method_table_thunk_awaits_an_async_method() {
        // An async method is `.await`ed before the ErasedRet is packed.
        let methods = vec![MethodSpec {
            method_path: "OrderService::place_order".into(),
            method_ident: Some("place_order".into()),
            is_async: true,
            arg_types: vec![ty("i64")],
            ret_type: ty("i64"),
        }];
        let s = flat(&emit_method_table("OrderService", &ty("OrderService"), &methods));
        assert!(
            s.contains("::leaf_core::ErasedRet::pack(__target.place_order(__a0).await)"),
            "got: {s}"
        );
    }

    #[test]
    fn method_table_thunk_handles_a_no_arg_method() {
        // A no-arg method unpacks the unit tuple `()` and calls with no args.
        let methods = vec![MethodSpec {
            method_path: "Svc::ping".into(),
            method_ident: Some("ping".into()),
            is_async: false,
            arg_types: vec![],
            ret_type: ty("u8"),
        }];
        let s = flat(&emit_method_table("Svc", &ty("Svc"), &methods));
        assert!(s.contains("__args.unpack::<()>()"), "got: {s}");
        assert!(s.contains("::leaf_core::ErasedRet::pack(__target.ping())"), "got: {s}");
    }

    #[test]
    fn method_table_thunk_handles_multiple_args() {
        // Two args => a 2-tuple carrier destructured into both positional params.
        let methods = vec![MethodSpec {
            method_path: "Svc::add".into(),
            method_ident: Some("add".into()),
            is_async: false,
            arg_types: vec![ty("i64"), ty("u32")],
            ret_type: ty("i64"),
        }];
        let s = flat(&emit_method_table("Svc", &ty("Svc"), &methods));
        assert!(s.contains("__args.unpack::<(i64,u32,)>()"), "got: {s}");
        assert!(s.contains("__target.add(__a0,__a1)"), "got: {s}");
    }

    #[test]
    fn an_empty_method_table_is_a_valid_empty_const() {
        // A bare struct form has no enumerable methods — an honest empty table (the
        // bean is registered + matchable but has no transparently-invocable methods).
        let ts = emit_method_table("Bare", &ty("Bare"), &[]);
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("pubstatic__leaf_methods_Bare:&::leaf_core::MethodTable=&::leaf_core::MethodTable(&[])"),
            "got: {s}"
        );
    }
}
