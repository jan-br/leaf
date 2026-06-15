//! The scheduling / caching / resource / catalog codegen the thin
//! `#[scheduled]` / `#[cacheable]` / `#[resource]` / `#[catalog]` macros call
//! (scheduling; caching phase3/09; expr-i18n-resources phase3/11).
//!
//! Each of these lowers to ONE hand-writable const row submitted into the matching
//! frozen `linkme` slice via ABSOLUTE `::leaf_core` paths (the thin-macro rule,
//! charter §2.10):
//!
//! - `#[scheduled(cron = "…" | fixed_rate = … | fixed_delay = …)]` → a const
//!   [`leaf_core::ScheduledMethodDescriptor`] (cron / fixed-rate / fixed-delay
//!   [`leaf_core::TriggerSpec`]) + its `.to_row()` [`leaf_core::ScheduledRow`] in
//!   the `SCHEDULED` slice.
//! - `#[cacheable("cacheName")]` → a const [`leaf_core::CacheOpMeta`] + the cache
//!   advisor IDENTITY row in `ADVISORS` (pinned to the `CACHE_ORDER` chain const).
//! - `#[resource("config/app.yaml")]` → a const [`leaf_core::ResourceEntry`]
//!   (`include_bytes!`-backed) + the [`leaf_core::ResourceRow`] in `RESOURCES`.
//! - `#[catalog(basename = "messages", locales = ["en", "de"])]` → a const
//!   [`leaf_core::CatalogDescriptor`] + the [`leaf_core::CatalogRow`] in `CATALOGS`.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::descriptor::EmitError;

// ─────────────────────────────── #[scheduled] ───────────────────────────────

/// The parsed `#[scheduled(...)]` trigger spec (exactly one of cron/fixed-rate/
/// fixed-delay), with the optional one-time `initial_delay` for the latter two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleSpec {
    /// A cron expression (parsed by `leaf-cron` at startup; core never parses cron).
    Cron(String),
    /// Fire every N milliseconds (fixed-rate from the previous scheduled time).
    FixedRate {
        /// Period in milliseconds.
        period_ms: u64,
        /// One-time initial delay in milliseconds.
        initial_delay_ms: u64,
    },
    /// Fire N milliseconds after the previous completion (fixed-delay).
    FixedDelay {
        /// Delay from completion in milliseconds.
        delay_ms: u64,
        /// One-time initial delay in milliseconds.
        initial_delay_ms: u64,
    },
}

impl ScheduleSpec {
    /// Lower this spec to a const `::leaf_core::TriggerSpec` token expression. The
    /// millisecond durations lower through the const `::core::time::Duration`
    /// constructor wrapped in the `::leaf_core::Duration` newtype.
    #[must_use]
    pub fn lower(&self) -> TokenStream {
        match self {
            ScheduleSpec::Cron(expr) => quote! { ::leaf_core::TriggerSpec::Cron(#expr) },
            ScheduleSpec::FixedRate { period_ms, initial_delay_ms } => {
                let period = duration_ms(*period_ms);
                let initial = duration_ms(*initial_delay_ms);
                quote! {
                    ::leaf_core::TriggerSpec::FixedRate {
                        period: #period,
                        initial_delay: #initial,
                    }
                }
            }
            ScheduleSpec::FixedDelay { delay_ms, initial_delay_ms } => {
                let delay = duration_ms(*delay_ms);
                let initial = duration_ms(*initial_delay_ms);
                quote! {
                    ::leaf_core::TriggerSpec::FixedDelay {
                        delay: #delay,
                        initial_delay: #initial,
                    }
                }
            }
        }
    }
}

/// A const `std::time::Duration` from a millisecond count. `TriggerSpec`'s
/// `period`/`delay`/`initial_delay` fields are the std `Duration` (NOT the leaf
/// `Duration` config newtype), so the const `::core::time::Duration::from_millis`
/// is emitted directly.
fn duration_ms(ms: u64) -> TokenStream {
    quote! { ::core::time::Duration::from_millis(#ms) }
}

/// Parse the `#[scheduled(cron = "…" | fixed_rate = N | fixed_delay = N,
/// initial_delay = M)]` attribute body into a [`ScheduleSpec`]. Exactly one of
/// `cron`/`fixed_rate`/`fixed_delay` is required; `initial_delay` (ms) is optional.
///
/// # Errors
/// [`EmitError`] on a malformed body, zero or more than one trigger key, an unknown
/// key, or a mistyped value.
pub fn parse_schedule(attr: TokenStream) -> Result<ScheduleSpec, EmitError> {
    if attr.is_empty() {
        return Err(EmitError {
            message: "#[scheduled] requires one of `cron`/`fixed_rate`/`fixed_delay`".into(),
        });
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[scheduled] arguments: {e}"),
    })?;
    let mut cron: Option<String> = None;
    let mut fixed_rate: Option<u64> = None;
    let mut fixed_delay: Option<u64> = None;
    let mut initial_delay: u64 = 0;
    for expr in &exprs {
        let syn::Expr::Assign(assign) = expr else {
            return Err(EmitError {
                message: format!(
                    "#[scheduled] arguments must be `key = value` pairs, got `{}`",
                    quote! { #expr }
                ),
            });
        };
        let key = assign_ident(&assign.left)?;
        match key.as_str() {
            "cron" => {
                cron = Some(str_value(&assign.right).ok_or_else(|| EmitError {
                    message: "`cron` must be a string expression".into(),
                })?);
            }
            "fixed_rate" => fixed_rate = Some(uint_value(&assign.right)?),
            "fixed_delay" => fixed_delay = Some(uint_value(&assign.right)?),
            "initial_delay" => initial_delay = uint_value(&assign.right)?,
            other => {
                return Err(EmitError {
                    message: format!(
                        "unknown #[scheduled] argument `{other}` \
                         (expected `cron`/`fixed_rate`/`fixed_delay`/`initial_delay`)"
                    ),
                });
            }
        }
    }
    match (cron, fixed_rate, fixed_delay) {
        (Some(expr), None, None) => Ok(ScheduleSpec::Cron(expr)),
        (None, Some(period_ms), None) => {
            Ok(ScheduleSpec::FixedRate { period_ms, initial_delay_ms: initial_delay })
        }
        (None, None, Some(delay_ms)) => {
            Ok(ScheduleSpec::FixedDelay { delay_ms, initial_delay_ms: initial_delay })
        }
        (None, None, None) => Err(EmitError {
            message: "#[scheduled] requires one of `cron`/`fixed_rate`/`fixed_delay`".into(),
        }),
        _ => Err(EmitError {
            message: "#[scheduled] accepts exactly one of `cron`/`fixed_rate`/`fixed_delay`".into(),
        }),
    }
}

/// Emit the const scheduling artifact for a `#[scheduled]` method on bean `ident`:
/// a const `::leaf_core::ScheduledMethodDescriptor` (bean + `bean::method`
/// [`leaf_core::MethodKey`] + the [`ScheduleSpec`] trigger), PLUS its `.to_row()`
/// [`leaf_core::ScheduledRow`] submitted into the frozen `SCHEDULED` slice (the
/// anti-DCE identity; a dropped descriptor is a task that silently never fires).
///
/// The descriptor is a PUBLIC pairing const (`__leaf_scheduled_<Bean>_<Method>`) so
/// the leaf-boot `after_init` post-processor can bind it to the live bean `Ref` and
/// register `(Trigger, body)` into the `SchedulerCore`.
#[must_use]
pub fn emit_scheduled(ident: &str, method: &str, spec: &ScheduleSpec) -> TokenStream {
    let mangled = mangle(&format!("{ident}_{method}"));
    let desc_ident = format_ident!("__leaf_scheduled_{}", mangled);
    let row_ident = format_ident!("__LEAF_SCHEDULED_{}", mangled);
    let spec_tokens = spec.lower();
    let bean_contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };
    // The method identity is the canonical `bean::method` path (the same one hash).
    let method_key = quote! {
        ::leaf_core::MethodKey::of(
            ::core::concat!(::core::module_path!(), "::", #ident, "::", #method)
        )
    };
    quote! {
        // The PUBLIC ScheduledMethodDescriptor pairing const: the leaf-boot
        // after_init post-processor reads it to resolve the Trigger + bind the body
        // to the live bean Ref (arming only after the all-singletons barrier).
        #[allow(non_upper_case_globals)]
        pub const #desc_ident: ::leaf_core::ScheduledMethodDescriptor =
            ::leaf_core::ScheduledMethodDescriptor::new(
                #bean_contract,
                #method_key,
                #spec_tokens,
            );
        // The cheap anti-DCE identity row in the frozen SCHEDULED slice via the bare
        // ::linkme attr path (the only form that resolves cross-crate).
        #[::linkme::distributed_slice(::leaf_core::SCHEDULED)]
        static #row_ident: ::leaf_core::ScheduledRow = #desc_ident.to_row();
    }
}

// ─────────────────────────────── #[cacheable] ───────────────────────────────

/// The parsed `#[cacheable(...)]` cache-op metadata (the cache name(s) + the flag
/// axes the const `::leaf_core::CacheOpMeta` carries).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheArgs {
    /// The cache name(s) this op targets (at least one).
    pub cache_names: Vec<String>,
    /// Whether the op runs BEFORE the method body (`before_invocation`).
    pub before_invocation: bool,
    /// Whether `sync` (single-flight) semantics are requested.
    pub sync: bool,
    /// Whether eviction clears the whole cache (`all_entries`).
    pub all_entries: bool,
}

/// Parse the `#[cacheable("name", sync = true, …)]` attribute body. The first
/// positional string (and any further positional strings, or a `caches = [..]`
/// list) names the target cache(s); `sync`/`before_invocation`/`all_entries` are
/// boolean flags.
///
/// # Errors
/// [`EmitError`] on a malformed body, no cache name, an unknown key, or a mistyped
/// value.
pub fn parse_cache_args(attr: TokenStream) -> Result<CacheArgs, EmitError> {
    let mut args = CacheArgs::default();
    if attr.is_empty() {
        return Err(EmitError {
            message: "#[cacheable] requires at least one cache name".into(),
        });
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[cacheable] arguments: {e}"),
    })?;
    for expr in &exprs {
        match expr {
            // A positional string literal = a cache name.
            syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => {
                args.cache_names.push(s.value());
            }
            syn::Expr::Assign(assign) => {
                let key = assign_ident(&assign.left)?;
                match key.as_str() {
                    "sync" => args.sync = bool_value(&assign.right)?,
                    "before_invocation" => args.before_invocation = bool_value(&assign.right)?,
                    "all_entries" => args.all_entries = bool_value(&assign.right)?,
                    other => {
                        return Err(EmitError {
                            message: format!(
                                "unknown #[cacheable] argument `{other}` \
                                 (expected `sync`/`before_invocation`/`all_entries`)"
                            ),
                        });
                    }
                }
            }
            other => {
                return Err(EmitError {
                    message: format!(
                        "#[cacheable] expects cache-name strings then `name = value` flags, \
                         got `{}`",
                        quote! { #other }
                    ),
                });
            }
        }
    }
    if args.cache_names.is_empty() {
        return Err(EmitError {
            message: "#[cacheable] requires at least one cache name".into(),
        });
    }
    Ok(args)
}

/// Emit the const caching artifact for a `#[cacheable]` method on bean `ident`: a
/// const `::leaf_core::CacheOpMeta` (cache names + flags) PLUS the cache advisor
/// IDENTITY row in the frozen `ADVISORS` slice pinned to the `CACHE_ORDER` chain
/// const (the cache interceptor is an infrastructure advisor — `Role::Infrastructure`,
/// so it wraps application advice).
///
/// The `CacheOpMeta` is a PUBLIC pairing const (`__leaf_cache_<Bean>_<Method>`) the
/// leaf-cache advisor reads at refresh; the `ADVISORS` row is the anti-DCE identity.
#[must_use]
pub fn emit_cacheable(ident: &str, method: &str, args: &CacheArgs) -> TokenStream {
    let mangled = mangle(&format!("{ident}_{method}"));
    let meta_ident = format_ident!("__leaf_cache_{}", mangled);
    let row_ident = format_ident!("__LEAF_CACHE_ADVISOR_{}", mangled);
    let names = args.cache_names.iter().map(|n| quote! { #n });
    let before = args.before_invocation;
    let sync = args.sync;
    let all = args.all_entries;
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident, "::", #method)
        )
    };
    quote! {
        // The PUBLIC CacheOpMeta pairing const the leaf-cache advisor reads at refresh.
        #[allow(non_upper_case_globals)]
        pub const #meta_ident: ::leaf_core::CacheOpMeta = ::leaf_core::CacheOpMeta {
            cache_names: &[ #(#names),* ],
            all_entries: #all,
            before_invocation: #before,
            sync: #sync,
        };
        // The cache advisor IDENTITY row in the frozen ADVISORS slice, pinned to the
        // CACHE_ORDER chain const (the cache interceptor is infrastructure advice).
        #[::linkme::distributed_slice(::leaf_core::ADVISORS)]
        static #row_ident: ::leaf_core::AdvisorRow = ::leaf_core::AdvisorRow {
            contract: #contract,
            order: ::leaf_core::OrderKey {
                value: ::leaf_core::CACHE_ORDER,
                source: ::leaf_core::OrderSource::Annotation,
            },
        };
    }
}

// ─────────────────────────────── #[resource] ────────────────────────────────

/// Emit the const resource artifact for a `#[resource("path")]` declaration: a
/// const `::leaf_core::ResourceEntry` (the logical path + an `include_bytes!`-backed
/// bytes accessor) PLUS the `::leaf_core::ResourceRow { contract, location }`
/// submitted into the frozen `RESOURCES` slice (the anti-DCE identity).
///
/// `ident` is the const item the macro is applied to (the pairing key); `path` is
/// the logical classpath path the bundle file lives at.
#[must_use]
pub fn emit_resource(ident: &str, path: &str) -> TokenStream {
    let mangled = mangle(ident);
    let entry_ident = format_ident!("__leaf_resource_{}", mangled);
    let row_ident = format_ident!("__LEAF_RESOURCE_{}", mangled);
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };
    quote! {
        // The PUBLIC ResourceEntry pairing const: the include_bytes!-backed accessor
        // is hand-writable; the leaf resource loader reads it at refresh.
        #[allow(non_upper_case_globals)]
        pub const #entry_ident: ::leaf_core::ResourceEntry = ::leaf_core::ResourceEntry {
            logical_path: #path,
            bytes_fn: || ::core::include_bytes!(#path),
        };
        // The anti-DCE RESOURCES identity row (a dropped resource is silently absent).
        #[::linkme::distributed_slice(::leaf_core::RESOURCES)]
        static #row_ident: ::leaf_core::ResourceRow = ::leaf_core::ResourceRow {
            contract: #contract,
            location: #path,
        };
    }
}

// ─────────────────────────────── #[catalog] ─────────────────────────────────

/// The parsed `#[catalog(basename = "…", locales = ["…", …])]` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogArgs {
    /// The base name (`messages`) the bundle files derive from.
    pub basename: String,
    /// The locales this catalog ships (`["en", "de"]`).
    pub locales: Vec<String>,
}

/// Parse the `#[catalog(basename = "messages", locales = ["en", "de"])]` body.
///
/// # Errors
/// [`EmitError`] on a malformed body, a missing `basename`, an unknown key, or a
/// mistyped value.
pub fn parse_catalog_args(attr: TokenStream) -> Result<CatalogArgs, EmitError> {
    let mut args = CatalogArgs::default();
    let mut saw_basename = false;
    if attr.is_empty() {
        return Err(EmitError {
            message: "#[catalog] requires a `basename`".into(),
        });
    }
    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let exprs = syn::parse::Parser::parse2(parser, attr).map_err(|e| EmitError {
        message: format!("malformed #[catalog] arguments: {e}"),
    })?;
    for expr in &exprs {
        let syn::Expr::Assign(assign) = expr else {
            return Err(EmitError {
                message: format!(
                    "#[catalog] arguments must be `key = value` pairs, got `{}`",
                    quote! { #expr }
                ),
            });
        };
        let key = assign_ident(&assign.left)?;
        match key.as_str() {
            "basename" => {
                args.basename = str_value(&assign.right).ok_or_else(|| EmitError {
                    message: "`basename` must be a string".into(),
                })?;
                saw_basename = true;
            }
            "locales" => args.locales = str_array(&assign.right)?,
            other => {
                return Err(EmitError {
                    message: format!(
                        "unknown #[catalog] argument `{other}` (expected `basename`/`locales`)"
                    ),
                });
            }
        }
    }
    if !saw_basename {
        return Err(EmitError {
            message: "#[catalog] requires a `basename`".into(),
        });
    }
    Ok(args)
}

/// Emit the const catalog artifact for a `#[catalog]` declaration on item `ident`:
/// a const `::leaf_core::CatalogDescriptor` (basename + locales) PLUS the
/// `::leaf_core::CatalogRow { contract }` submitted into the frozen `CATALOGS` slice.
#[must_use]
pub fn emit_catalog(ident: &str, args: &CatalogArgs) -> TokenStream {
    let mangled = mangle(ident);
    let desc_ident = format_ident!("__leaf_catalog_{}", mangled);
    let row_ident = format_ident!("__LEAF_CATALOG_{}", mangled);
    let basename = &args.basename;
    let locales = args.locales.iter().map(|l| quote! { #l });
    let contract = quote! {
        ::leaf_core::ContractId::of(
            ::core::concat!(::core::module_path!(), "::", #ident)
        )
    };
    quote! {
        // The PUBLIC CatalogDescriptor pairing const the i18n MessageSource reads.
        #[allow(non_upper_case_globals)]
        pub const #desc_ident: ::leaf_core::CatalogDescriptor = ::leaf_core::CatalogDescriptor {
            contract: #contract,
            basename: #basename,
            locales: &[ #(#locales),* ],
        };
        // The anti-DCE CATALOGS identity row.
        #[::linkme::distributed_slice(::leaf_core::CATALOGS)]
        static #row_ident: ::leaf_core::CatalogRow = ::leaf_core::CatalogRow {
            contract: #contract,
        };
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

/// The bare ident of an assignment left-hand side.
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

/// The string value of a `key = "literal"` right-hand side.
fn str_value(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s.value()),
        _ => None,
    }
}

/// The boolean value of a `key = true/false` right-hand side.
fn bool_value(expr: &syn::Expr) -> Result<bool, EmitError> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(b), .. }) => Ok(b.value),
        other => Err(EmitError {
            message: format!("expected a bool, got `{}`", quote! { #other }),
        }),
    }
}

/// The unsigned integer value of a numeric right-hand side (milliseconds, etc.).
fn uint_value(expr: &syn::Expr) -> Result<u64, EmitError> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(i), .. }) => {
            i.base10_parse::<u64>().map_err(|e| EmitError {
                message: format!("expected a non-negative integer: {e}"),
            })
        }
        other => Err(EmitError {
            message: format!("expected an integer literal, got `{}`", quote! { #other }),
        }),
    }
}

/// The string array value of a `key = ["a", "b"]` right-hand side.
fn str_array(expr: &syn::Expr) -> Result<Vec<String>, EmitError> {
    match expr {
        syn::Expr::Array(arr) => {
            let mut out = Vec::new();
            for elem in &arr.elems {
                out.push(str_value(elem).ok_or_else(|| EmitError {
                    message: "array elements must be string literals".into(),
                })?);
            }
            Ok(out)
        }
        other => Err(EmitError {
            message: format!("expected a `[\"…\", …]` string array, got `{}`", quote! { #other }),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    // ── #[scheduled] ─────────────────────────────────────────────────────────

    #[test]
    fn scheduled_cron_lowers_to_a_const_descriptor_and_a_slice_row() {
        // The headline: #[scheduled(cron = "…")] emits a const
        // ScheduledMethodDescriptor + its .to_row() into the frozen SCHEDULED slice.
        let spec = parse_schedule(syn::parse_str(r#"cron = "0 0 * * * *""#).unwrap())
            .expect("parses");
        assert_eq!(spec, ScheduleSpec::Cron("0 0 * * * *".into()));
        let ts = emit_scheduled("Cleanup", "run", &spec);
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::ScheduledMethodDescriptor::new("), "got: {s}");
        // `flat()` collapses the whitespace inside the cron literal, so assert on the
        // collapsed form (the un-flattened emitted token keeps the spaces — proven by
        // parse2 above succeeding and `spec` equalling the spaced cron string).
        assert!(s.contains(r#"::leaf_core::TriggerSpec::Cron("00****")"#), "got: {s}");
        assert!(
            s.contains("#[::linkme::distributed_slice(::leaf_core::SCHEDULED)]"),
            "got: {s}"
        );
        // The slice row is the descriptor's .to_row() (the cheap identity row).
        assert!(s.contains(".to_row()"), "got: {s}");
    }

    #[test]
    fn scheduled_fixed_rate_lowers_to_a_duration_trigger_spec() {
        let spec = parse_schedule(syn::parse_str("fixed_rate = 5000").unwrap()).expect("parses");
        assert_eq!(spec, ScheduleSpec::FixedRate { period_ms: 5000, initial_delay_ms: 0 });
        let s = flat(&emit_scheduled("Poller", "poll", &spec));
        assert!(s.contains("::leaf_core::TriggerSpec::FixedRate{"), "got: {s}");
        assert!(
            s.contains("::core::time::Duration::from_millis(5000u64)"),
            "got: {s}"
        );
    }

    #[test]
    fn scheduled_fixed_delay_with_initial_delay() {
        let spec = parse_schedule(syn::parse_str("fixed_delay = 1000, initial_delay = 200").unwrap())
            .expect("parses");
        assert_eq!(
            spec,
            ScheduleSpec::FixedDelay { delay_ms: 1000, initial_delay_ms: 200 }
        );
        let s = flat(&emit_scheduled("W", "tick", &spec));
        assert!(s.contains("::leaf_core::TriggerSpec::FixedDelay{"), "got: {s}");
        assert!(s.contains("from_millis(200u64)"), "got: {s}");
    }

    #[test]
    fn the_scheduled_method_key_is_the_canonical_bean_method_path() {
        let spec = ScheduleSpec::Cron("* * * * * *".into());
        let s = flat(&emit_scheduled("Cleanup", "run", &spec));
        // The method identity is module::Bean::method, qualified at the def site.
        assert!(
            s.contains(
                r#"::leaf_core::MethodKey::of(::core::concat!(::core::module_path!(),"::","Cleanup","::","run"))"#
            ),
            "got: {s}"
        );
    }

    #[test]
    fn scheduled_requires_exactly_one_trigger() {
        let err = parse_schedule(TokenStream::new()).expect_err("empty errors");
        assert!(err.message.contains("requires one of"), "got: {}", err.message);
        let err = parse_schedule(syn::parse_str(r#"cron = "x", fixed_rate = 1"#).unwrap())
            .expect_err("two triggers error");
        assert!(err.message.contains("exactly one"), "got: {}", err.message);
    }

    // ── #[cacheable] ─────────────────────────────────────────────────────────

    #[test]
    fn cacheable_emits_a_cache_op_meta_and_a_cache_advisor_row() {
        let args = parse_cache_args(syn::parse_str(r#""users", sync = true"#).unwrap())
            .expect("parses");
        assert_eq!(args.cache_names, vec!["users".to_string()]);
        assert!(args.sync);
        let ts = emit_cacheable("UserService", "find", &args);
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::CacheOpMeta{"), "got: {s}");
        assert!(s.contains(r#"cache_names:&["users"]"#), "got: {s}");
        assert!(s.contains("sync:true"), "got: {s}");
        // The cache advisor identity row rides ADVISORS, pinned to CACHE_ORDER.
        assert!(
            s.contains("#[::linkme::distributed_slice(::leaf_core::ADVISORS)]"),
            "got: {s}"
        );
        assert!(s.contains("value:::leaf_core::CACHE_ORDER"), "got: {s}");
    }

    #[test]
    fn cacheable_requires_a_cache_name() {
        let err = parse_cache_args(TokenStream::new()).expect_err("no name errors");
        assert!(err.message.contains("at least one cache name"), "got: {}", err.message);
        let err = parse_cache_args(syn::parse_str("sync = true").unwrap())
            .expect_err("flags-only errors");
        assert!(err.message.contains("at least one cache name"), "got: {}", err.message);
    }

    // ── #[resource] ──────────────────────────────────────────────────────────

    #[test]
    fn resource_emits_an_include_bytes_entry_and_a_resources_row() {
        let ts = emit_resource("MESSAGES", "config/app.yaml");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::ResourceEntry{"), "got: {s}");
        assert!(s.contains(r#"logical_path:"config/app.yaml""#), "got: {s}");
        // The bytes accessor is an include_bytes!-backed const fn pointer.
        assert!(s.contains(r#"bytes_fn:||::core::include_bytes!("config/app.yaml")"#), "got: {s}");
        assert!(
            s.contains("#[::linkme::distributed_slice(::leaf_core::RESOURCES)]"),
            "got: {s}"
        );
        assert!(s.contains(r#"location:"config/app.yaml""#), "got: {s}");
    }

    // ── #[catalog] ───────────────────────────────────────────────────────────

    #[test]
    fn catalog_emits_a_catalog_descriptor_and_a_catalogs_row() {
        let args =
            parse_catalog_args(syn::parse_str(r#"basename = "messages", locales = ["en", "de"]"#).unwrap())
                .expect("parses");
        assert_eq!(args.basename, "messages");
        assert_eq!(args.locales, vec!["en".to_string(), "de".to_string()]);
        let ts = emit_catalog("AppMessages", &args);
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::CatalogDescriptor{"), "got: {s}");
        assert!(s.contains(r#"basename:"messages""#), "got: {s}");
        assert!(s.contains(r#"locales:&["en","de"]"#), "got: {s}");
        assert!(
            s.contains("#[::linkme::distributed_slice(::leaf_core::CATALOGS)]"),
            "got: {s}"
        );
        assert!(s.contains("::leaf_core::CatalogRow{"), "got: {s}");
    }

    #[test]
    fn catalog_requires_a_basename() {
        let err = parse_catalog_args(syn::parse_str(r#"locales = ["en"]"#).unwrap())
            .expect_err("no basename errors");
        assert!(err.message.contains("basename"), "got: {}", err.message);
    }
}
