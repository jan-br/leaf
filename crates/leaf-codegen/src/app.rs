//! The application-entrypoint codegen the thin `#[leaf::main]` / `#[runner]` /
//! `#[failure_analyzer]` macros call (discovery-codegen phase3/02; bootstrap-
//! diagnostics phase3/14).
//!
//! Three surfaces:
//!
//! - `#[leaf::main]` — the BINARY-CRATE entrypoint seam: it splices in the Layer-0
//!   anti-DCE force-link shim + the const `ExpectedManifest` self-check anchor
//!   (reusing [`crate::forcelink`]) and wraps the user's `async fn main` body in a
//!   real `fn main()` that constructs and drives the run engine
//!   (`::leaf_boot::Application::new(Primary).run()`). The run ENGINE itself lives
//!   in leaf-boot (out of this unit's scope); this emits the hand-writable entry
//!   shape + the anti-DCE seam the binary owns.
//! - `#[runner]` — a [`leaf_core::Runner`] bean: structurally a `#[component]`
//!   that ALSO declares it is injectable as `dyn ::leaf_core::Runner` (the
//!   `provides[]` upcast the run pipeline collects the runner stream from). No
//!   separate slice — a runner is discovered as a `COMPONENTS` row providing the
//!   `Runner` view.
//! - `#[failure_analyzer]` — a [`leaf_core::FailureAnalyzer`] impl: emit a `static`
//!   instance + a `&'static dyn FailureAnalyzer` row into the frozen
//!   `FAILURE_ANALYZERS` slice (the error-model SPI, reused — never a second trait).
//!
//! Every emitted path is ABSOLUTE `::leaf_core::…` / `::leaf_boot::…` (the thin-
//! macro rule, charter §2.10).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::descriptor::EmitError;
use crate::forcelink::ParticipatingSet;

// ─────────────────────────────── #[leaf::main] ──────────────────────────────

/// The parsed `#[leaf::main(Primary, scan(...))]` arguments: the optional primary
/// application source type + the participating-crate scan list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MainArgs {
    /// The primary application source type passed to `Application::new(...)`. When
    /// `None`, the unit `()` source is used (the bare in-crate-only app).
    pub primary: Option<syn::Path>,
    /// The participating crates the binary force-links (the `scan(a, b)` list, in
    /// ADDITION to the always-present binary crate itself).
    pub scan: Vec<String>,
}

/// Parse the `#[leaf::main]` attribute body: an optional leading primary-source
/// path, then an optional `scan("leaf-redis", "leaf-tokio")` participating list.
///
/// # Errors
/// [`EmitError`] on a malformed body, an unknown key, or a non-string scan entry.
pub fn parse_main_args(attr: TokenStream) -> Result<MainArgs, EmitError> {
    let mut args = MainArgs::default();
    if attr.is_empty() {
        return Ok(args);
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[leaf::main] arguments: {e}"),
    })?;
    for expr in &exprs {
        match expr {
            // `scan(a, b, …)` — the participating-crate list (string literals).
            syn::Expr::Call(call) if call_is(call, "scan") => {
                for arg in &call.args {
                    let name = str_value(arg).ok_or_else(|| EmitError {
                        message: "`scan(...)` entries must be crate-name string literals".into(),
                    })?;
                    args.scan.push(name);
                }
            }
            // A bare path = the primary application source type.
            syn::Expr::Path(p) => {
                if args.primary.is_some() {
                    return Err(EmitError {
                        message: "#[leaf::main] accepts at most one primary source type".into(),
                    });
                }
                args.primary = Some(p.path.clone());
            }
            other => {
                return Err(EmitError {
                    message: format!(
                        "unexpected #[leaf::main] argument `{}` (expected a primary source \
                         type and/or `scan(\"crate\", …)`)",
                        quote! { #other }
                    ),
                });
            }
        }
    }
    Ok(args)
}

/// Emit the `#[leaf::main]` artifact: the anti-DCE force-link seam (the `scan(...)`
/// shim + the umbrella's feature-gated `::leaf::force_link!()` invoked from the
/// binary crate) + the const `ExpectedManifest` (over the participating set) PLUS a
/// real `fn main()` that drives the run engine over the user's `async fn` body — the
/// umbrella-only maximal-magic entry (bootstrap-diagnostics phase3/14). The user
/// writes ONLY `#[leaf::main]`: the force-link anchor is fully encapsulated here, so
/// the binary `main` module carries no hand-written `leaf::force_link!();`.
///
/// `binary_crate` is the binary's own Cargo package name (always force-linked); the
/// `args.scan` list adds further participating crates. The emitted `fn main()`
/// delegates to `::leaf::run_main(name, body)` — the umbrella's default-runtime
/// entry driver, which builds the tokio runtime the binary owns, bootstraps + runs
/// the application to Ready (the per-bean wiring + auto-configs + `#[runner]`s all
/// fire there), hands the live `RunningApp` to the user body, then drains the clean
/// shutdown. So `#[leaf::main]` + the single `leaf` dependency IS the working app.
///
/// The user's `async fn main` is renamed to `__leaf_async_main` and called from the
/// `run_main` closure. The body may take ZERO params (the bare orchestration form) or
/// ONE `&::leaf::boot::RunningApp` param (the app-aware form); the call shape adapts.
///
/// NOTE (primary source): a `#[leaf::main(Cfg)]` primary application source is parsed
/// and carried by [`MainArgs`], but the umbrella's `run_main`/`bootstrap` bridge does
/// not yet thread a typed primary `@SpringBootApplication` config into the run
/// pipeline (the slice-collected `COMPONENTS`/`AUTO_CONFIGS` ARE the scan today, so
/// there is no per-app component-scan root). The primary is therefore accepted for
/// forward compatibility but is a `compile_error!`-free no-op; the bare `#[leaf::main]`
/// is the landed shape, and wiring the primary through is the bootstrap bridge's seam.
#[must_use]
pub fn emit_main(binary_crate: &str, args: &MainArgs, user_fn: &syn::ItemFn) -> TokenStream {
    // The force-link shim covers OTHER crates only: the binary crate is always
    // linked (it IS the binary), and `use <self> as _;` inside its own crate would
    // not compile. The ExpectedManifest, however, includes the binary crate too —
    // it contributes its own rows, so the self-check must expect its SourceTag.
    let scan_set = ParticipatingSet::from_names(args.scan.iter().cloned());
    let mut manifest_set = scan_set.clone();
    manifest_set.add(binary_crate);
    let force_link = crate::forcelink::emit_force_link(&scan_set);
    let manifest = crate::forcelink::emit_expected_manifest(&manifest_set);
    // The umbrella's feature-gated force-link, invoked FROM the binary crate so the
    // capability rlibs the umbrella pulls (its `#[cfg(feature = …)]` set) are pinned
    // onto the link graph from the final link unit — the strongest anti-`--gc-sections`
    // anchor. ENCAPSULATING this here is the DX payoff: the user writes ONLY
    // `#[leaf::main]`, never `leaf::force_link!();`. The macro expands at the call site,
    // so its `#[cfg(feature = …)]` gates resolve in the BINARY crate's feature namespace
    // (the binary mirrors the umbrella's capability features). The `scan(...)` shim above
    // covers the author-explicit list; this covers the umbrella's enabled capabilities.
    let umbrella_force_link = quote! { ::leaf::force_link!(); };
    let anti_dce = quote! { #force_link #umbrella_force_link #manifest };

    // The umbrella-only facade aliases (emitted from the binary crate root, where
    // `#[leaf::main]` sits). The annotation macros emit ABSOLUTE crate-root paths
    // (`::leaf_core::`/`::leaf_cache::`/`::leaf_tx::`), which resolve against the crate's
    // direct deps — and an umbrella-only app's only dep is `leaf`. These source aliases
    // (NOT Cargo deps) bind each concern-crate root to the one `leaf` dependency, so a
    // user writes ONLY `use leaf::prelude::*;` + beans + `#[leaf::main]` — no hand-written
    // `extern crate leaf as leaf_core;`. Crate-root `extern crate` is visible crate-wide,
    // so every module's macro-emitted `::leaf_core::` path resolves. `#[allow]` because an
    // app using no `#[cacheable]`/`#[transactional]` leaves the cache/tx alias unused.
    let facade_aliases = quote! {
        #[allow(unused_extern_crates)]
        extern crate leaf as leaf_core;
        #[allow(unused_extern_crates)]
        extern crate leaf as leaf_cache;
        #[allow(unused_extern_crates)]
        extern crate leaf as leaf_tx;
    };
    let anti_dce = quote! { #facade_aliases #anti_dce };

    // The user body, renamed so the generated `fn main` owns the entrypoint symbol.
    let mut inner = user_fn.clone();
    inner.sig.ident = format_ident!("__leaf_async_main");
    let inner_ident = &inner.sig.ident;

    // The call shape adapts to the user fn's arity: the app-aware form takes the live
    // `&RunningApp`, the bare orchestration form ignores it. Anything else is a loud
    // compile_error! steering to the two sanctioned shapes.
    let arity = user_fn.sig.inputs.len();
    let body_call = match arity {
        // The closure returns a BoxFuture borrowing the &RunningApp (run_main's body
        // shape) — `Box::pin` erases the user future into the lifetime-tied boxed
        // future. The bare form ignores the app; the app-aware form threads it.
        0 => quote! {
            (|_app: &::leaf::boot::RunningApp| ::std::boxed::Box::pin(#inner_ident()))
        },
        1 => quote! {
            (|__app: &::leaf::boot::RunningApp| ::std::boxed::Box::pin(#inner_ident(__app)))
        },
        _ => {
            return quote! {
                #user_fn
                ::core::compile_error!(
                    "#[leaf::main] async fn main takes either no parameters (the bare \
                     orchestration form) or one `&leaf::boot::RunningApp` parameter (the \
                     app-aware form)"
                );
            };
        }
    };

    quote! {
        // The anti-DCE seam the binary crate owns (force-link shim + ExpectedManifest).
        #anti_dce

        // The user's annotated body, preserved verbatim under a private name.
        #inner

        // The real entrypoint: drive the run engine through the umbrella's
        // default-runtime entry driver. `run_main` builds the tokio runtime, bootstraps
        // + runs the app to Ready (runners fire, the graph wires, auto-configs
        // participate), hands the live RunningApp to the user body, then drains the
        // clean shutdown — `#[leaf::main]` + one `leaf` dependency IS the app.
        fn main() -> ::core::result::Result<(), ::leaf::LeafError> {
            ::leaf::run_main(#binary_crate, #body_call)
        }
    }
}

/// `true` iff the call expression's callee is the bare ident `name`.
fn call_is(call: &syn::ExprCall, name: &str) -> bool {
    matches!(&*call.func, syn::Expr::Path(p) if p.path.is_ident(name))
}

// ─────────────────────────────── #[runner] ──────────────────────────────────

/// Lower a `#[runner] struct` to the [`crate::descriptor::BeanInput`] for a
/// `#[component]` that ALSO declares it is injectable as `dyn ::leaf_core::Runner`
/// (so the run pipeline collects it from the `Runner` upcast view). A runner is NOT
/// a separate slice — it is a `COMPONENTS` row carrying the `Runner` `provides[]`
/// upcast.
///
/// # Errors
/// [`EmitError`] when the struct is generic (no single concrete type) or its
/// annotation is malformed.
pub fn runner_input(item: &syn::ItemStruct) -> Result<crate::descriptor::BeanInput, EmitError> {
    let mut input = crate::stereotype::struct_input(
        item,
        crate::stereotype::Stereotype::Component,
        None,
        None,
        crate::descriptor::Scope::Singleton,
        // `#[runner]` keeps field injection (no `constructor = …` surface yet — a
        // trivial deferred follow-up per the design).
        None,
    )?;
    // Declare the Runner upcast view so the run pipeline finds it by the `dyn Runner`
    // contract (the one place a runner differs from a plain component).
    input.provides.push(crate::descriptor::ServiceView {
        dyn_ty: syn::parse_str("dyn ::leaf_core::Runner").map_err(|e| EmitError {
            message: format!("failed to build the dyn Runner view: {e}"),
        })?,
    });
    Ok(input)
}

/// Emit the const `#[runner]` artifact — a `COMPONENTS` `Descriptor` row that
/// declares the `dyn ::leaf_core::Runner` upcast view PLUS the per-runner UPCAST THUNK
/// (`__leaf_runner_upcast_<Ident>`) the run pipeline JOINs as a `RunnerPairing`.
///
/// # Errors
/// [`EmitError`] per [`runner_input`].
pub fn emit_runner(item: &syn::ItemStruct) -> Result<TokenStream, EmitError> {
    let rows = crate::descriptor::emit(&runner_input(item)?)?;
    let upcast = emit_runner_upcast(&item.ident.to_string());
    Ok(quote! { #rows #upcast })
}

/// Emit the per-runner UPCAST THUNK for a `#[runner]` bean `ident`: a PUBLIC
/// `fn(::leaf_core::ErasedBean) -> Option<Arc<dyn ::leaf_core::Runner>>` named
/// `__leaf_runner_upcast_<Ident>` that downcasts the erased bean to the concrete
/// runner type and re-wraps it as `Arc<dyn Runner>` (trait upcasting, stable 1.86).
///
/// This is the runner analogue of the `__leaf_methods_<Ident>` method-table thunk:
/// `#[leaf::main]` pairs it with the runner's `ContractId` as a
/// `::leaf_boot::RunnerPairing`, and the run pipeline AUTO-COLLECTS the runner from
/// the live Context by applying this thunk to the resolved erased bean — so a
/// `#[runner]` bean auto-runs with NO hand-written `RunnerUpcast` in user code (the
/// same `TypeId`-keyed view-upcast idea as `TypeRow`).
#[must_use]
pub fn emit_runner_upcast(ident: &str) -> TokenStream {
    let mangled = mangle(ident);
    let upcast_ident = format_ident!("__leaf_runner_upcast_{}", mangled);
    let upcast_row_ident = format_ident!("__LEAF_RUNNER_PAIRING_{}", mangled);
    let ty: syn::Ident = syn::Ident::new(ident, proc_macro2::Span::call_site());
    // The runner bean's module-qualified contract (the RUNNER_PAIRINGS JOIN key),
    // built at the use site exactly like the bean's Descriptor contract.
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };
    quote! {
        // The PUBLIC per-runner upcast thunk: downcast the erased bean to the concrete
        // runner, re-wrap as Arc<dyn Runner>. The run pipeline applies it to the
        // resolved erased bean to auto-collect the runner from the live Context.
        // `missing_docs`: a macro-generated `pub` thunk — allow it so a crate that
        // `#![warn(missing_docs)]`s (e.g. leaf-web's `WebServerRunner`) is not flagged.
        #[allow(non_upper_case_globals, non_snake_case, missing_docs)]
        pub fn #upcast_ident(
            __bean: ::leaf_core::ErasedBean,
        ) -> ::core::option::Option<::std::sync::Arc<dyn ::leaf_core::Runner>> {
            __bean.downcast::<#ty>().ok().map(|__a| __a as ::std::sync::Arc<dyn ::leaf_core::Runner>)
        }
        // Submit the upcast into RUNNER_PAIRINGS (the auto-collect substrate) keyed by
        // ContractId, so the run pipeline auto-collects the runner with no
        // hand-assembled `.with_runner_beans`. The order defaults to implicit
        // (declaration order); same re-export pattern as COMPONENTS.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::RUNNER_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #upcast_row_ident: ::leaf_core::RunnerPairingRow = ::leaf_core::RunnerPairingRow {
            contract: #contract,
            upcast: #upcast_ident,
            order: ::leaf_core::OrderKey::implicit(),
        };
    }
}

// ─────────────────────────── #[failure_analyzer] ────────────────────────────

/// Emit the const `#[failure_analyzer]` artifact for a unit-struct analyzer
/// `ident`: a `static` instance + a `&'static dyn ::leaf_core::FailureAnalyzer` row
/// submitted into the frozen `FAILURE_ANALYZERS` slice (the error-model SPI reused —
/// never a second analyzer trait).
///
/// The user writes the `impl ::leaf_core::FailureAnalyzer for #ident`; this wires
/// its link-time discovery. The annotated item is kept verbatim by the thin macro.
#[must_use]
pub fn emit_failure_analyzer(ident: &str) -> TokenStream {
    let mangled = mangle(ident);
    let instance_ident = format_ident!("__LEAF_ANALYZER_INSTANCE_{}", mangled);
    let row_ident = format_ident!("__LEAF_ANALYZER_{}", mangled);
    let ty: syn::Ident = syn::Ident::new(ident, proc_macro2::Span::call_site());
    quote! {
        // The static analyzer instance (a unit struct, so const-constructible) + the
        // anti-DCE row in the frozen FAILURE_ANALYZERS slice via the re-exported
        // `::leaf_core::linkme` attr path + crate override (a dropped analyzer
        // silently never runs).
        #[allow(non_upper_case_globals)]
        static #instance_ident: #ty = #ty;
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::FAILURE_ANALYZERS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: &dyn ::leaf_core::FailureAnalyzer = &#instance_ident;
    }
}

// ──────────────────────────────── helpers ───────────────────────────────────

/// A spans-free, identifier-safe mangling of an ident for emitted helper names.
fn mangle(ident: &str) -> syn::Ident {
    let safe: String = ident
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    syn::Ident::new(&safe, proc_macro2::Span::call_site())
}

/// The string value of an expression, if it is a string literal.
fn str_value(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s.value()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    fn func(src: &str) -> syn::ItemFn {
        syn::parse_str(src).expect("a valid fn item")
    }

    fn item(src: &str) -> syn::ItemStruct {
        syn::parse_str(src).expect("a valid struct item")
    }

    // ── #[leaf::main] ────────────────────────────────────────────────────────

    #[test]
    fn main_emits_the_anti_dce_seam_and_a_runnable_main() {
        // The headline: #[leaf::main] splices in the force-link shim + the const
        // ExpectedManifest, and wraps the user body in a real `fn main()` that drives
        // the umbrella's default-runtime entry driver.
        let ts = emit_main(
            "my-app",
            &MainArgs::default(),
            &func("async fn main() -> Result<(), ::leaf::LeafError> { Ok(()) }"),
        );
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The anti-DCE seam (from forcelink::emit).
        assert!(s.contains("mod__leaf_force_link"), "got: {s}");
        assert!(s.contains("__LEAF_EXPECTED_MANIFEST"), "got: {s}");
        // The umbrella's feature-gated force-link, invoked from the binary crate so the
        // USER never hand-writes `leaf::force_link!();` (the encapsulated anti-DCE anchor).
        assert!(s.contains("::leaf::force_link!()"), "got: {s}");
        // The umbrella-only facade aliases are AUTO-EMITTED (the user never hand-writes
        // `extern crate leaf as leaf_core;`) so an annotation's `::leaf_core::`/
        // `::leaf_cache::`/`::leaf_tx::` path resolves crate-wide from the one `leaf` dep.
        assert!(s.contains("externcrateleafasleaf_core;"), "got: {s}");
        assert!(s.contains("externcrateleafasleaf_cache;"), "got: {s}");
        assert!(s.contains("externcrateleafasleaf_tx;"), "got: {s}");
        // The binary crate itself is always force-linked.
        assert!(s.contains(r#"::leaf_core::SourceTag("my-app")"#), "got: {s}");
        // A real synchronous `fn main` returning the umbrella-rooted LeafError is emitted.
        assert!(s.contains("fnmain()->::core::result::Result<(),::leaf::LeafError>"), "got: {s}");
        // The user body is preserved under a private name.
        assert!(s.contains("asyncfn__leaf_async_main()"), "got: {s}");
        // It drives the umbrella's run_main entry driver, keyed by the binary name.
        assert!(s.contains(r#"::leaf::run_main("my-app","#), "got: {s}");
        // The bare (zero-param) body is invoked ignoring the RunningApp handle, boxed
        // into the lifetime-tied future run_main's body closure returns.
        assert!(
            s.contains("|_app:&::leaf::boot::RunningApp|::std::boxed::Box::pin(__leaf_async_main())"),
            "got: {s}"
        );
    }

    #[test]
    fn main_invokes_the_umbrella_feature_gated_force_link() {
        // The DX headline: #[leaf::main] itself invokes the umbrella's feature-gated
        // `::leaf::force_link!()` (the binary-originated anti-DCE anchor over the ENABLED
        // CAPABILITY features), so the user never hand-writes `leaf::force_link!();` in
        // their `main` module. This is distinct from the `scan(...)` shim (the
        // author-explicit list): the macro-invoked force_link! pins the rlibs the umbrella
        // pulls via its capability features, gated in the BINARY crate's feature namespace.
        let ts = emit_main(
            "my-app",
            &MainArgs::default(),
            &func("async fn main() -> Result<(), ::leaf::LeafError> { Ok(()) }"),
        );
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("::leaf::force_link!()"), "got: {s}");
    }

    #[test]
    fn main_with_an_app_param_passes_the_running_app() {
        // The app-aware form: a single `&RunningApp` param receives the live app.
        let ts = emit_main(
            "my-app",
            &MainArgs::default(),
            &func(
                "async fn main(app: &::leaf::boot::RunningApp) -> Result<(), ::leaf::LeafError> { Ok(()) }",
            ),
        );
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("|__app:&::leaf::boot::RunningApp|::std::boxed::Box::pin(__leaf_async_main(__app))"),
            "got: {s}"
        );
    }

    #[test]
    fn main_rejects_more_than_one_parameter() {
        let ts = emit_main(
            "my-app",
            &MainArgs::default(),
            &func("async fn main(a: u8, b: u8) -> Result<(), ::leaf::LeafError> { Ok(()) }"),
        );
        let s = flat(&ts);
        assert!(s.contains("compile_error!"), "more than one param is a hard error: {s}");
    }

    #[test]
    fn main_force_links_the_scan_list() {
        let args = parse_main_args(syn::parse_str(r#"scan("leaf-redis", "leaf-tokio")"#).unwrap())
            .expect("parses");
        assert_eq!(args.scan, vec!["leaf-redis".to_string(), "leaf-tokio".to_string()]);
        let s = flat(&emit_main(
            "my-app",
            &args,
            &func("async fn main() -> Result<(), ::leaf::LeafError> { Ok(()) }"),
        ));
        assert!(s.contains("useleaf_redisas_;"), "got: {s}");
        assert!(s.contains("useleaf_tokioas_;"), "got: {s}");
        assert!(s.contains(r#"::leaf_core::SourceTag("leaf-redis")"#), "got: {s}");
    }

    #[test]
    fn main_does_not_self_force_link_the_binary_crate() {
        // The binary crate is always linked (it IS the binary); `use <self> as _;`
        // inside its own crate would not compile. So the force-link shim must NOT
        // reference the binary crate — but the ExpectedManifest MUST (it contributes
        // its own rows, so the self-check expects its SourceTag).
        let s = flat(&emit_main(
            "my-app",
            &MainArgs::default(),
            &func("async fn main() -> Result<(), ::leaf::LeafError> { Ok(()) }"),
        ));
        assert!(!s.contains("usemy_appas_;"), "the binary must not self-force-link: {s}");
        assert!(s.contains(r#"::leaf_core::SourceTag("my-app")"#), "got: {s}");
    }

    #[test]
    fn main_parses_but_does_not_yet_thread_a_primary_source() {
        // A primary application source is parsed + carried for forward compat, but the
        // bridge does not thread it through yet (see the emit_main NOTE) — it is a
        // no-op, never a compile_error, so `#[leaf::main(Cfg)]` still emits a runnable
        // main over the slice-collected scan.
        let args = parse_main_args(syn::parse_str("MyConfig").unwrap()).expect("parses");
        assert!(args.primary.is_some());
        let s = flat(&emit_main(
            "my-app",
            &args,
            &func("async fn main() -> Result<(), ::leaf::LeafError> { Ok(()) }"),
        ));
        assert!(s.contains(r#"::leaf::run_main("my-app","#), "got: {s}");
        assert!(!s.contains("compile_error!"), "a primary source is a no-op, not an error: {s}");
    }

    #[test]
    fn main_rejects_two_primary_sources() {
        let err = parse_main_args(syn::parse_str("A, B").unwrap())
            .expect_err("two primaries error");
        assert!(err.message.contains("at most one primary"), "got: {}", err.message);
    }

    // ── #[runner] ────────────────────────────────────────────────────────────

    #[test]
    fn runner_emits_a_component_row_providing_the_runner_view() {
        // A #[runner] is a COMPONENTS row that ALSO declares the dyn Runner upcast.
        let ts = emit_runner(&item("struct MigrateRunner;")).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
            "got: {s}"
        );
        // The Runner upcast view rides provides[].
        assert!(s.contains("::leaf_core::TypeRow"), "got: {s}");
        assert!(
            s.contains("::core::any::TypeId::of::<dyn::leaf_core::Runner>()"),
            "got: {s}"
        );
    }

    #[test]
    fn runner_rejects_a_generic_target() {
        let err = emit_runner(&item("struct R<T> { inner: T }"))
            .expect_err("a generic runner hard-errors");
        assert!(err.message.contains("register_component!") || err.message.contains("generic"), "got: {}", err.message);
    }

    #[test]
    fn runner_emits_the_per_runner_upcast_thunk() {
        // A #[runner] ALSO emits the per-runner upcast thunk the run pipeline pairs
        // by ContractId (the RunnerPairing the auto-wire test previously hand-wrote).
        let ts = emit_runner(&item("struct MigrateRunner;")).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains(
                "pubfn__leaf_runner_upcast_MigrateRunner(__bean:::leaf_core::ErasedBean,)->::core::option::Option<::std::sync::Arc<dyn::leaf_core::Runner>>"
            ),
            "got: {s}"
        );
        // It downcasts to the concrete runner and re-wraps as Arc<dyn Runner>.
        assert!(s.contains("__bean.downcast::<MigrateRunner>().ok()"), "got: {s}");
        assert!(
            s.contains("__aas::std::sync::Arc<dyn::leaf_core::Runner>"),
            "got: {s}"
        );
    }

    #[test]
    fn runner_upcast_thunk_is_emittable_standalone() {
        // The thunk emitter is the thin macro's delegate (no logic in the macro).
        let ts = emit_runner_upcast("MigrateRunner");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("__leaf_runner_upcast_MigrateRunner"), "got: {s}");
    }

    #[test]
    fn runner_upcast_is_submitted_into_the_runner_pairings_slice() {
        // The upcast is ALSO auto-collected into RUNNER_PAIRINGS keyed by ContractId
        // (the COMPONENTS auto-collect substrate, extended) so the run pipeline
        // auto-collects the runner with no hand-assembled `.with_runner_beans`.
        let s = flat(&emit_runner_upcast("MigrateRunner"));
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::RUNNER_PAIRINGS)]"),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::RunnerPairingRow{contract:"), "got: {s}");
        assert!(s.contains("upcast:__leaf_runner_upcast_MigrateRunner"), "got: {s}");
        assert!(s.contains("order:::leaf_core::OrderKey::implicit()"), "got: {s}");
    }

    // ── #[failure_analyzer] ──────────────────────────────────────────────────

    #[test]
    fn failure_analyzer_emits_a_static_and_a_slice_row() {
        // The error-model SPI is REUSED: a &'static dyn FailureAnalyzer row in the
        // frozen FAILURE_ANALYZERS slice (never a second analyzer trait).
        let ts = emit_failure_analyzer("PortInUseAnalyzer");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::FAILURE_ANALYZERS)]"),
            "got: {s}"
        );
        assert!(s.contains("#[linkme(crate=::leaf_core::linkme)]"), "got: {s}");
        // The row is a reference to a static instance of the user's analyzer type.
        assert!(s.contains("&dyn::leaf_core::FailureAnalyzer=&__LEAF_ANALYZER_INSTANCE_PortInUseAnalyzer"), "got: {s}");
        assert!(s.contains("static__LEAF_ANALYZER_INSTANCE_PortInUseAnalyzer:PortInUseAnalyzer"), "got: {s}");
    }
}
