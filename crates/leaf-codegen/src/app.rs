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

/// Emit the `#[leaf::main]` artifact: the anti-DCE force-link shim + the const
/// `ExpectedManifest` (over the participating set) PLUS a real `fn main()` that
/// drives the run engine over the user's `async fn` body.
///
/// `binary_crate` is the binary's own Cargo package name (always force-linked); the
/// `args.scan` list adds further participating crates. The emitted `fn main()`
/// builds a runtime, runs the user body, then drives
/// `::leaf_boot::Application::new(Primary).run()` — the run ENGINE is leaf-boot's
/// (NOTE below); this emits the hand-writable entry shape.
///
/// The user's `async fn main` is renamed to `__leaf_async_main` and called from the
/// generated synchronous `fn main()`.
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
    let anti_dce = quote! { #force_link #manifest };

    // The user body, renamed so the generated `fn main` owns the entrypoint symbol.
    let mut inner = user_fn.clone();
    inner.sig.ident = format_ident!("__leaf_async_main");
    let inner_ident = &inner.sig.ident;

    // The primary source for `Application::new(...)` — the named type's `::default()`
    // value, or the unit source when unspecified.
    let primary = match &args.primary {
        Some(path) => quote! { <#path as ::core::default::Default>::default() },
        None => quote! { () },
    };

    quote! {
        // The anti-DCE seam the binary crate owns (force-link shim + ExpectedManifest).
        #anti_dce

        // The user's annotated body, preserved verbatim under a private name.
        #inner

        // NOTE (cross-crate, leaf-boot): the run ENGINE
        // (`::leaf_boot::Application::new(Primary).run()` — the App<Define→Resolve→
        // Wired→Running> phase machine + Context::refresh) lives in leaf-boot, which
        // is out of this unit's scope. The generated entrypoint emits the
        // hand-writable shape that constructs the Application over the primary source
        // and drives it, then runs the user body; binding it to the real engine is
        // leaf-boot's concern (this unit owns the binary-crate anti-DCE seam +
        // the entry shape).
        fn main() -> ::core::result::Result<(), ::leaf_core::LeafError> {
            ::leaf_boot::Application::new(#primary).run(#inner_ident)
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
        crate::descriptor::Scope::Singleton,
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
/// declares the `dyn ::leaf_core::Runner` upcast view.
///
/// # Errors
/// [`EmitError`] per [`runner_input`].
pub fn emit_runner(item: &syn::ItemStruct) -> Result<TokenStream, EmitError> {
    crate::descriptor::emit(&runner_input(item)?)
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
        // anti-DCE row in the frozen FAILURE_ANALYZERS slice via the bare ::linkme
        // attr path (a dropped analyzer silently never runs).
        #[allow(non_upper_case_globals)]
        static #instance_ident: #ty = #ty;
        #[::linkme::distributed_slice(::leaf_core::FAILURE_ANALYZERS)]
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
        // ExpectedManifest, and wraps the user body in a real `fn main()`.
        let ts = emit_main(
            "my-app",
            &MainArgs::default(),
            &func("async fn main() { println!(\"hi\"); }"),
        );
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The anti-DCE seam (from forcelink::emit).
        assert!(s.contains("mod__leaf_force_link"), "got: {s}");
        assert!(s.contains("__LEAF_EXPECTED_MANIFEST"), "got: {s}");
        // The binary crate itself is always force-linked.
        assert!(s.contains(r#"::leaf_core::SourceTag("my-app")"#), "got: {s}");
        // A real synchronous `fn main` is emitted.
        assert!(s.contains("fnmain()->::core::result::Result<(),::leaf_core::LeafError>"), "got: {s}");
        // The user body is preserved under a private name.
        assert!(s.contains("async fn__leaf_async_main()") || s.contains("asyncfn__leaf_async_main()"), "got: {s}");
        // It drives the leaf-boot run engine entry shape.
        assert!(s.contains("::leaf_boot::Application::new"), "got: {s}");
    }

    #[test]
    fn main_force_links_the_scan_list() {
        let args = parse_main_args(syn::parse_str(r#"scan("leaf-redis", "leaf-tokio")"#).unwrap())
            .expect("parses");
        assert_eq!(args.scan, vec!["leaf-redis".to_string(), "leaf-tokio".to_string()]);
        let s = flat(&emit_main("my-app", &args, &func("async fn main() {}")));
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
        let s = flat(&emit_main("my-app", &MainArgs::default(), &func("async fn main() {}")));
        assert!(!s.contains("usemy_appas_;"), "the binary must not self-force-link: {s}");
        assert!(s.contains(r#"::leaf_core::SourceTag("my-app")"#), "got: {s}");
    }

    #[test]
    fn main_passes_the_primary_source_to_the_application() {
        let args = parse_main_args(syn::parse_str("MyConfig").unwrap()).expect("parses");
        assert!(args.primary.is_some());
        let s = flat(&emit_main("my-app", &args, &func("async fn main() {}")));
        assert!(
            s.contains("::leaf_boot::Application::new(<MyConfigas::core::default::Default>::default())"),
            "got: {s}"
        );
    }

    #[test]
    fn main_with_no_primary_uses_the_unit_source() {
        let s = flat(&emit_main("my-app", &MainArgs::default(), &func("async fn main() {}")));
        assert!(s.contains("::leaf_boot::Application::new(())"), "got: {s}");
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
            s.contains("#[::linkme::distributed_slice(::leaf_core::COMPONENTS)]"),
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

    // ── #[failure_analyzer] ──────────────────────────────────────────────────

    #[test]
    fn failure_analyzer_emits_a_static_and_a_slice_row() {
        // The error-model SPI is REUSED: a &'static dyn FailureAnalyzer row in the
        // frozen FAILURE_ANALYZERS slice (never a second analyzer trait).
        let ts = emit_failure_analyzer("PortInUseAnalyzer");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(
            s.contains("#[::linkme::distributed_slice(::leaf_core::FAILURE_ANALYZERS)]"),
            "got: {s}"
        );
        // The row is a reference to a static instance of the user's analyzer type.
        assert!(s.contains("&dyn::leaf_core::FailureAnalyzer=&__LEAF_ANALYZER_INSTANCE_PortInUseAnalyzer"), "got: {s}");
        assert!(s.contains("static__LEAF_ANALYZER_INSTANCE_PortInUseAnalyzer:PortInUseAnalyzer"), "got: {s}");
    }
}
