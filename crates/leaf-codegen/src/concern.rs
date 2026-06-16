//! The DECLARATIVE per-concern codegen the thin natural-annotation macros
//! (`#[transactional]` / `#[cacheable]` / `#[cache_put]` / `#[cache_evict]` /
//! `#[validated]` / `#[retryable]` / `#[concurrency_limit]`) call through the
//! `#[advisable] impl` iterator (declarative-advice phase3/09).
//!
//! ## Why these ride the `#[advisable] impl` iterator
//!
//! A proc-macro ATTRIBUTE on a single method cannot emit SIBLING `#[distributed_slice]`
//! rows (it expands only the method), so — exactly like `#[bean]` on a
//! `#[configuration]` class method and `#[advice]` on an `#[aspect]` class method —
//! a per-concern annotation on a `#[advisable] impl` METHOD is a MARKER the impl-block
//! macro reads. The `#[advisable] impl` iterator (which already emits the join-point
//! spec + the downcast-thunk method table) detects each concern attr on a `&self`
//! method and emits, per concern, the THREE pieces the auto-wire test previously
//! hand-wrote:
//!
//! 1. the per-method concern METADATA const where the concern needs one
//!    (`TxAttribute` for tx — though the auto-wire row applies `TxAttribute::DEFAULT`;
//!    `CacheOpMeta` for cache);
//! 2. the `ADVISOR_PAIRINGS` row binding the concern's `Interceptor` (resolved from
//!    the concern crate's `make_interceptor`/pairing builder) keyed by a pointcut over
//!    the advised bean's concrete `TypeId` — so `Application::run` auto-collects the
//!    advisor with NO `.with_advisors`;
//! 3. the per-return `ReturnClassifier` (tx/retry: a `Result::Err` → rollback / retry)
//!    and the arg-key fn (cache `key = "#0"` → a `CacheKeyFn` reading `Call.args[0]`),
//!    baked into the `make_interceptor` closure literal.
//!
//! The concern CRATES (leaf-tx/cache/validation/resilience) own the `Interceptor` +
//! the const metadata types + the generic `make_interceptor`/pairing BUILDERS; this
//! macro is THIN — it only emits the rows referencing those builders by ABSOLUTE
//! `::leaf_tx::` / `::leaf_cache::` / … paths (the user crate links the concern crate).
//!
//! ## The advised-arg / return-type bound
//!
//! The return type `T` (read off the method signature) is baked into the tx/retry
//! return classifier (`Result<T, LeafError>` → the business-`Err` rollback/retry
//! decision) and the cache value type; the FIRST non-receiver arg type `A` is baked
//! into the validation arg-validator and the `key = "#0"` cache key fn. Each rides the
//! settled advised-arg ABI (`Clone + Send + Sync + 'static`).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Type;

use crate::descriptor::EmitError;

/// The per-concern annotations the `#[advisable] impl` iterator recognises on a
/// `&self` method — the natural declarative vocabulary (NOT `#[aspect]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Concern {
    /// `#[transactional(..)]` — demarcate a tx (commit on `Ok`, rollback on `Err`).
    Transactional,
    /// `#[cacheable(..)]` — cache the result (a hit short-circuits the body).
    Cacheable,
    /// `#[cache_put(..)]` — always run + refresh the cached entry.
    CachePut,
    /// `#[cache_evict(..)]` — evict an entry around the body.
    CacheEvict,
    /// `#[validated]` — validate the `@Valid` arg before the body (short-circuit).
    Validated,
    /// `#[retryable(..)]` — retry the method on a retryable error.
    Retryable,
    /// `#[concurrency_limit(n)]` — bound concurrent entries via a `ConcurrencyGate`.
    ConcurrencyLimit,
}

impl Concern {
    /// The attribute keyword that selects this concern (`transactional`, …).
    #[must_use]
    pub fn keyword(self) -> &'static str {
        match self {
            Concern::Transactional => "transactional",
            Concern::Cacheable => "cacheable",
            Concern::CachePut => "cache_put",
            Concern::CacheEvict => "cache_evict",
            Concern::Validated => "validated",
            Concern::Retryable => "retryable",
            Concern::ConcurrencyLimit => "concurrency_limit",
        }
    }

    /// Recognise a concern keyword (the attribute path's last segment).
    #[must_use]
    pub fn from_keyword(name: &str) -> Option<Self> {
        [
            Concern::Transactional,
            Concern::Cacheable,
            Concern::CachePut,
            Concern::CacheEvict,
            Concern::Validated,
            Concern::Retryable,
            Concern::ConcurrencyLimit,
        ]
        .into_iter()
        .find(|c| c.keyword() == name)
    }

    /// Whether this is one of the three cache ops (which share `parse_cache`).
    #[must_use]
    pub fn is_cache(self) -> bool {
        matches!(self, Concern::Cacheable | Concern::CachePut | Concern::CacheEvict)
    }
}

/// The signature facts the `#[advisable] impl` iterator hands the per-concern emitter:
/// the method's stable `Bean::method` path, its return type `T`, and its first
/// non-receiver arg type `A` (the `@Valid` / `key = "#0"` target), if any.
#[derive(Clone, Debug)]
pub struct MethodSig {
    /// The canonical `Bean::method` path (the `MethodKey` the row keys on).
    pub method_path: String,
    /// The method's return type (the tx/retry return classifier `T`, the cache `T`).
    pub ret_type: Type,
    /// The method's first non-receiver argument type (the validation `@Valid` arg /
    /// the `key = "#0"` cache key arg), if the method takes one.
    pub first_arg_type: Option<Type>,
}

// ─────────────────────────── attribute parsing ──────────────────────────────

/// A `manager = …` / `gate = …` parameter TYPE, carrying the SYNTACTIC SHAPE the
/// emitter dispatches on — NEVER a spelled name.
///
/// `manager = LedgerTxManager` ([`syn::Type::Path`]) resolves the CONCRETE manager
/// by its `TypeId` + downcast (the existing, unchanged behavior); `manager = dyn
/// TransactionManager` ([`syn::Type::TraitObject`]) resolves the manager through the
/// GENERAL by-trait injection path (the same `resolve_view` primitive a `Ref<dyn
/// Svc>` injection point drives). [`ManagerRef::is_trait_object`] reads `syn::Type`'s
/// VARIANT, so the dispatch is purely structural.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagerRef {
    /// The parsed parameter type (a concrete path, or a `dyn Trait` trait object).
    pub ty: Type,
}

impl ManagerRef {
    /// `true` iff this names a `dyn Trait` VIEW (a [`syn::Type::TraitObject`]) — the
    /// by-trait path. Dispatch on the type's syntactic shape, never a spelled name.
    #[must_use]
    pub fn is_trait_object(&self) -> bool {
        matches!(self.ty, Type::TraitObject(_))
    }
}

/// The parsed `#[transactional(manager = Mgr, rollback_for(Kind), …)]` args.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransactionalArgs {
    /// The `TransactionManager` the advisor resolves (required — there is no default
    /// manager). A concrete type (`manager = LedgerTxManager`) OR a trait-object view
    /// (`manager = dyn TransactionManager`).
    pub manager: Option<ManagerRef>,
}

/// The parsed `#[cacheable(cache = "users", key = "#0", manager = Mgr)]` args.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheableArgs {
    /// The cache name(s) (`cache = "users"` or a leading positional string).
    pub cache_names: Vec<String>,
    /// The cache manager the advisor resolves (required). A concrete type OR a
    /// trait-object view (`manager = dyn CacheManager`).
    pub manager: Option<ManagerRef>,
    /// `key = "#0"` keys on argument 0 (the only supported key form in v1);
    /// `None` keys on a single per-method entry (the unit key).
    pub key_arg: Option<usize>,
    /// Whether eviction clears the whole cache (`all_entries`, cache_evict only).
    pub all_entries: bool,
    /// Whether `sync` (single-flight) semantics are requested.
    pub sync: bool,
}

/// The parsed `#[retryable(max = 3, backoff = exponential(base = 10, mult = 2))]` args.
#[derive(Clone, Debug, PartialEq)]
pub struct RetryableArgs {
    /// The maximum attempts (default 3).
    pub max: u32,
    /// The backoff policy (default none).
    pub backoff: Backoff,
}

impl Default for RetryableArgs {
    fn default() -> Self {
        RetryableArgs { max: 3, backoff: Backoff::None }
    }
}

/// The backoff policy a `#[retryable(backoff = …)]` selects.
#[derive(Clone, Debug, PartialEq)]
pub enum Backoff {
    /// No backoff (retry immediately).
    None,
    /// A fixed delay between attempts (milliseconds).
    Fixed(u64),
    /// Exponential backoff from a base delay (ms) times a multiplier per attempt.
    Exponential {
        /// The base delay in milliseconds.
        base_ms: u64,
        /// The per-attempt multiplier.
        mult: f64,
    },
}

/// The parsed `#[concurrency_limit(n, gate = MyGate)]` args.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConcurrencyLimitArgs {
    /// The concrete `ConcurrencyGate` bean TYPE the advisor resolves (required — the
    /// gate is sized by the bean's own `::new()`; `n` is the documented intent).
    pub gate: Option<String>,
}

/// Parse `#[transactional(manager = Mgr)]`.
///
/// # Errors
/// [`EmitError`] on a malformed body, an unknown key, or a missing `manager`.
pub fn parse_transactional(attr: TokenStream) -> Result<TransactionalArgs, EmitError> {
    let mut args = TransactionalArgs::default();
    for item in attr_items(attr, "transactional")? {
        match item {
            AttrItem::Assign { key, value } if key == "manager" => {
                args.manager = Some(ManagerRef { ty: type_value(value, "manager")? });
            }
            AttrItem::Assign { key, .. } => return Err(unknown(&key, "transactional", "manager")),
            AttrItem::Call { name, .. } if name == "rollback_for" || name == "no_rollback_for" => {
                // The rollback-rule list is accepted (the auto-wire interceptor applies
                // the default any-`Err`-rolls-back rule; a finer rule is the programmatic
                // builder's concern), so the natural annotation parses without error.
            }
            AttrItem::Call { name, .. } => return Err(unknown(&name, "transactional", "manager")),
            AttrItem::Positional(e) => return Err(positional(&e, "transactional")),
        }
    }
    if args.manager.is_none() {
        return Err(EmitError {
            message: "#[transactional] requires `manager = <TransactionManager bean type>` \
                      (there is no default manager bean type)"
                .into(),
        });
    }
    Ok(args)
}

/// Parse `#[cacheable("users", key = "#0", manager = Mgr)]` (and `#[cache_put]` /
/// `#[cache_evict(all_entries)]` — the same arg grammar).
///
/// # Errors
/// [`EmitError`] on a malformed body, an unknown key, no cache name, no `manager`, or
/// an unsupported `key` form.
pub fn parse_cache(attr: TokenStream, concern: Concern) -> Result<CacheableArgs, EmitError> {
    let mut args = CacheableArgs::default();
    let kw = concern.keyword();
    for item in attr_items(attr, kw)? {
        match item {
            AttrItem::Positional(syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s), ..
            })) => args.cache_names.push(s.value()),
            AttrItem::Positional(syn::Expr::Path(p)) if p.path.is_ident("all_entries") => {
                args.all_entries = true;
            }
            AttrItem::Positional(syn::Expr::Path(p)) if p.path.is_ident("sync") => {
                args.sync = true;
            }
            AttrItem::Positional(e) => return Err(positional(&e, kw)),
            AttrItem::Assign { key, value } => match key.as_str() {
                "cache" | "value" => {
                    let value = value.expect_expr(&key)?;
                    let name = str_value(&value).ok_or_else(|| EmitError {
                        message: format!("`{key}` must be a cache-name string"),
                    })?;
                    args.cache_names.push(name);
                }
                "manager" => args.manager = Some(ManagerRef { ty: type_value(value, "manager")? }),
                "key" => args.key_arg = Some(parse_key(&value.expect_expr(&key)?)?),
                "all_entries" => args.all_entries = bool_value(&value.expect_expr(&key)?)?,
                "sync" => args.sync = bool_value(&value.expect_expr(&key)?)?,
                other => {
                    return Err(EmitError {
                        message: format!(
                            "unknown #[{kw}] argument `{other}` \
                             (expected `cache`/`key`/`manager`/`all_entries`/`sync`)"
                        ),
                    });
                }
            },
            AttrItem::Call { name, .. } => return Err(unknown(&name, kw, "cache/key/manager")),
        }
    }
    if args.cache_names.is_empty() {
        return Err(EmitError {
            message: format!("#[{kw}] requires at least one cache name (`\"users\"` or `cache = \"users\"`)"),
        });
    }
    if args.manager.is_none() {
        return Err(EmitError {
            message: format!("#[{kw}] requires `manager = <CacheManager bean type>`"),
        });
    }
    Ok(args)
}

/// Parse `#[retryable(max = 3, backoff = exponential(base = 10, mult = 2.0))]`.
///
/// # Errors
/// [`EmitError`] on a malformed body, an unknown key, or a malformed backoff.
pub fn parse_retryable(attr: TokenStream) -> Result<RetryableArgs, EmitError> {
    let mut args = RetryableArgs::default();
    for item in attr_items(attr, "retryable")? {
        match item {
            AttrItem::Assign { key, value } => match key.as_str() {
                "max" | "max_attempts" => args.max = uint_value(&value.expect_expr(&key)?)? as u32,
                "backoff" => args.backoff = parse_backoff(&value.expect_expr(&key)?)?,
                other => return Err(unknown(other, "retryable", "max/backoff")),
            },
            AttrItem::Call { name, .. } if name == "backoff" => {
                return Err(EmitError {
                    message: "#[retryable] `backoff` is `backoff = fixed(ms)` / \
                              `backoff = exponential(base = ms, mult = f)` / `backoff = none`"
                        .into(),
                });
            }
            AttrItem::Call { name, .. } => return Err(unknown(&name, "retryable", "max/backoff")),
            AttrItem::Positional(e) => return Err(positional(&e, "retryable")),
        }
    }
    Ok(args)
}

/// Parse `#[concurrency_limit(2, gate = MyGate)]` — the `n` (intent) + the gate type.
///
/// # Errors
/// [`EmitError`] on a malformed body, an unknown key, or a missing `gate`.
pub fn parse_concurrency_limit(attr: TokenStream) -> Result<ConcurrencyLimitArgs, EmitError> {
    let mut args = ConcurrencyLimitArgs::default();
    for item in attr_items(attr, "concurrency_limit")? {
        match item {
            // The leading positional int is the documented limit intent (the gate
            // bean is sized by its own `::new()`; the row only needs the gate TYPE).
            AttrItem::Positional(syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(_), .. })) => {}
            AttrItem::Positional(e) => return Err(positional(&e, "concurrency_limit")),
            AttrItem::Assign { key, value } if key == "gate" => {
                let ty = type_value(value, "gate")?;
                args.gate = Some(quote! { #ty }.to_string());
            }
            AttrItem::Assign { key, .. } => return Err(unknown(&key, "concurrency_limit", "gate")),
            AttrItem::Call { name, .. } => return Err(unknown(&name, "concurrency_limit", "gate")),
        }
    }
    if args.gate.is_none() {
        return Err(EmitError {
            message: "#[concurrency_limit] requires `gate = <ConcurrencyGate bean type>` \
                      (the gate bean is sized from `n` by its own `::new()`)"
                .into(),
        });
    }
    Ok(args)
}

/// Parse a `key = "#N"` form into the positional arg index N. Only `"#N"` is
/// supported in v1 (key on a single positional arg).
fn parse_key(value: &syn::Expr) -> Result<usize, EmitError> {
    let s = str_value(value).ok_or_else(|| EmitError {
        message: "`key` must be a string of the form `\"#0\"` (key on positional arg 0)".into(),
    })?;
    let n = s.strip_prefix('#').and_then(|d| d.parse::<usize>().ok()).ok_or_else(|| EmitError {
        message: format!(
            "unsupported cache key `{s:?}`: v1 supports only `\"#N\"` (a positional arg index)"
        ),
    })?;
    Ok(n)
}

/// Parse a `backoff = …` right-hand side into a [`Backoff`].
fn parse_backoff(value: &syn::Expr) -> Result<Backoff, EmitError> {
    match value {
        // `backoff = none`
        syn::Expr::Path(p) if p.path.is_ident("none") => Ok(Backoff::None),
        // `backoff = fixed(ms)` / `backoff = exponential(base = ms, mult = f)`
        syn::Expr::Call(call) => {
            let name = call_name(&call.func).ok_or_else(|| EmitError {
                message: "backoff must be `fixed(ms)` / `exponential(base = ms, mult = f)` / `none`"
                    .into(),
            })?;
            match name.as_str() {
                "fixed" => {
                    let ms = call
                        .args
                        .first()
                        .and_then(uint_lit)
                        .ok_or_else(|| EmitError {
                            message: "`fixed(ms)` needs one integer-millisecond argument".into(),
                        })?;
                    Ok(Backoff::Fixed(ms))
                }
                "exponential" => parse_exponential(&call.args),
                other => Err(EmitError {
                    message: format!(
                        "unknown backoff `{other}` (expected `fixed`/`exponential`/`none`)"
                    ),
                }),
            }
        }
        other => Err(EmitError {
            message: format!(
                "backoff must be `fixed(ms)` / `exponential(base = ms, mult = f)` / `none`, got `{}`",
                quote! { #other }
            ),
        }),
    }
}

/// Parse the `exponential(base = ms, mult = f)` named arguments.
fn parse_exponential(
    args: &syn::punctuated::Punctuated<syn::Expr, syn::Token![,]>,
) -> Result<Backoff, EmitError> {
    let mut base_ms: Option<u64> = None;
    let mut mult: Option<f64> = None;
    for a in args {
        let syn::Expr::Assign(assign) = a else {
            return Err(EmitError {
                message: "`exponential` arguments are `base = ms, mult = f`".into(),
            });
        };
        let key = assign_ident(&assign.left)?;
        match key.as_str() {
            "base" => base_ms = Some(uint_value(&assign.right)?),
            "mult" => mult = Some(float_value(&assign.right)?),
            other => {
                return Err(EmitError {
                    message: format!("unknown `exponential` arg `{other}` (expected `base`/`mult`)"),
                });
            }
        }
    }
    Ok(Backoff::Exponential {
        base_ms: base_ms.ok_or_else(|| EmitError {
            message: "`exponential` needs `base = <ms>`".into(),
        })?,
        mult: mult.unwrap_or(2.0),
    })
}

// ────────────────────────────── emission ────────────────────────────────────

/// Emit the full per-method concern artifact for one concern attribute on a
/// `#[advisable]`-impl method of bean `bean_ident` (concrete `self_ty`): the metadata
/// const(s) where needed + the `ADVISOR_PAIRINGS` row keyed by a pointcut over the
/// bean's `TypeId`, referencing the concern crate's builder.
///
/// `args_attr` is the concern attribute's inner tokens (`manager = Mgr`, …).
///
/// # Errors
/// [`EmitError`] on a malformed concern attribute or a missing required field.
pub fn emit_concern(
    concern: Concern,
    args_attr: TokenStream,
    bean_ident: &str,
    self_ty: &Type,
    sig: &MethodSig,
) -> Result<TokenStream, EmitError> {
    match concern {
        Concern::Transactional => {
            emit_transactional(&parse_transactional(args_attr)?, bean_ident, self_ty, sig)
        }
        Concern::Cacheable | Concern::CachePut | Concern::CacheEvict => {
            emit_cache(concern, &parse_cache(args_attr, concern)?, bean_ident, self_ty, sig)
        }
        Concern::Validated => {
            // `#[validated]` carries no config; reject any stray args.
            if !attr_items(args_attr, "validated")?.is_empty() {
                return Err(EmitError {
                    message: "#[validated] takes no arguments".into(),
                });
            }
            emit_validated(bean_ident, self_ty, sig)
        }
        Concern::Retryable => {
            emit_retryable(&parse_retryable(args_attr)?, bean_ident, self_ty, sig)
        }
        Concern::ConcurrencyLimit => {
            emit_concurrency_limit(&parse_concurrency_limit(args_attr)?, bean_ident, self_ty, sig)
        }
    }
}

/// The mangled per-bean-method identity base (`Bean_method`) for the emitted consts.
fn ident_base(bean_ident: &str, sig: &MethodSig) -> String {
    format!("{bean_ident}_{}", method_name(sig))
}

/// The method's bare name (`Bean::method` → `method`).
fn method_name(sig: &MethodSig) -> &str {
    sig.method_path.rsplit("::").next().unwrap_or(&sig.method_path)
}

/// A per-method UNIQUE advisor `ContractId` token expression for one concern. The
/// run pipeline merges + indexes `ADVISOR_PAIRINGS` rows by `ContractId`
/// (`merge_by_contract`, `InstalledProxies` `by_id`), so EVERY emitted row MUST carry
/// a distinct contract — else two `#[transactional]` beans (or two `#[cacheable]`
/// methods on one bean) collide and only one is advised. The contract is the concern's
/// family base (`leaf::tx::TransactionAdvisor`) suffixed by the module-qualified
/// `Bean::method` (so it is stable + collision-free across beans and methods).
fn unique_contract(family_base: &str, bean_ident: &str, sig: &MethodSig) -> TokenStream {
    let method = method_name(sig);
    quote! {
        ::leaf_core::ContractId::of(::core::concat!(
            #family_base, "@", ::core::module_path!(), "::", #bean_ident, "::", #method
        ))
    }
}

/// The const `&'static [TypeId]` of the advised bean + the matching pointcut static,
/// for an arbitrary const-constructible `Pointcut` type referenced by absolute path.
fn pointcut_statics(
    self_ty: &Type,
    types_ident: &syn::Ident,
    pointcut_ident: &syn::Ident,
    pointcut_ty: TokenStream,
) -> TokenStream {
    quote! {
        #[allow(non_upper_case_globals)]
        static #types_ident: [::core::any::TypeId; 1] =
            [const { ::core::any::TypeId::of::<#self_ty>() }];
        #[allow(non_upper_case_globals)]
        static #pointcut_ident: #pointcut_ty = #pointcut_ty::new(&#types_ident);
    }
}

/// Emit the `#[transactional]` artifact: the `ADVISOR_PAIRINGS` row binding the named
/// manager `M` + the per-return `Result<T, _>` classifier, keyed by a `TxPointcut`
/// over the bean's `TypeId` at `TX_ORDER`.
fn emit_transactional(
    args: &TransactionalArgs,
    bean_ident: &str,
    self_ty: &Type,
    sig: &MethodSig,
) -> Result<TokenStream, EmitError> {
    let base = mangle(&ident_base(bean_ident, sig));
    let types_ident = format_ident!("__LEAF_TX_TYPES_{}", base);
    let pointcut_ident = format_ident!("__LEAF_TX_POINTCUT_{}", base);
    let row_ident = format_ident!("__LEAF_TX_ADVISOR_{}", base);
    let manager = args.manager.as_ref().ok_or_else(|| EmitError {
        message: "#[transactional] requires `manager = …`".into(),
    })?;
    let ret = result_ok_ty(&sig.ret_type);
    let contract = unique_contract("leaf::tx::TransactionAdvisor", bean_ident, sig);
    let statics =
        pointcut_statics(self_ty, &types_ident, &pointcut_ident, quote! { ::leaf_tx::TxPointcut });
    // Dispatch on the manager parameter's SYNTACTIC SHAPE (never a spelled name): a
    // `dyn TransactionManager` trait object resolves through the GENERAL by-trait
    // injection path (resolve_view); a concrete type keeps the ByType + downcast path.
    let make = if manager.is_trait_object() {
        quote! { ::leaf_tx::make_transaction_interceptor_for_view::<#ret>() }
    } else {
        let mty = &manager.ty;
        quote! { ::leaf_tx::make_transaction_interceptor_for::<#mty, #ret>() }
    };
    Ok(quote! {
        #statics
        // The tx advisor auto-wire row: the named manager `M` + the `Result<T,_>`
        // classifier (the business `Err` → rollback decision), keyed by the bean's
        // concrete TypeId, at the pinned TX_ORDER (INSIDE cache). The contract is
        // per-method-unique (the row index merges by ContractId).
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::AdvisorPairingRow = ::leaf_core::AdvisorPairingRow {
            contract: #contract,
            order: ::leaf_tx::tx_order_key(),
            role: ::leaf_core::Role::Infrastructure,
            pointcut: &#pointcut_ident,
            make_interceptor: #make,
        };
    })
}

/// Emit a cache concern artifact (`#[cacheable]`/`#[cache_put]`/`#[cache_evict]`): the
/// per-method `CacheOpMeta` const + the `ADVISOR_PAIRINGS` row binding the named
/// manager `M` + the per-return `T` + the `key = "#N"` key fn, at `CACHE_ORDER`.
fn emit_cache(
    concern: Concern,
    args: &CacheableArgs,
    bean_ident: &str,
    self_ty: &Type,
    sig: &MethodSig,
) -> Result<TokenStream, EmitError> {
    let base = mangle(&ident_base(bean_ident, sig));
    let types_ident = format_ident!("__LEAF_CACHE_TYPES_{}", base);
    let pointcut_ident = format_ident!("__LEAF_CACHE_POINTCUT_{}", base);
    let meta_ident = format_ident!("__LEAF_CACHE_META_{}", base);
    let row_ident = format_ident!("__LEAF_CACHE_ADVISOR_{}", base);

    let manager = args.manager.as_ref().ok_or_else(|| EmitError {
        message: format!("#[{}] requires `manager = …`", concern.keyword()),
    })?;
    let names = args.cache_names.iter().map(|n| quote! { #n });
    let all = args.all_entries;
    let sync = args.sync;
    let op = match concern {
        Concern::Cacheable => quote! { ::leaf_cache::CacheOp::Cacheable },
        Concern::CachePut => quote! { ::leaf_cache::CacheOp::CachePut },
        Concern::CacheEvict => quote! { ::leaf_cache::CacheOp::CacheEvict },
        _ => unreachable!("emit_cache is only called for the three cache concerns"),
    };
    // The cached/refreshed value type is the method's return `T`; an evict's body
    // return is also `T` (it is passed through). A `Result<T,_>` returner caches the
    // Result verbatim (Clone) — the value type is the full return type.
    let value_ty = &sig.ret_type;
    let method_key = &sig.method_path;

    // The key fn: `key = "#N"` reads positional arg N off Call.args (the typed key
    // closure); unset is the unit key (a single per-method entry).
    let key_fn = match args.key_arg {
        None => quote! { ::leaf_cache::unit_key_fn() },
        Some(n) => {
            let arg_ty = sig.first_arg_type.as_ref().ok_or_else(|| EmitError {
                message: format!(
                    "#[{}] key = \"#{n}\" but the method takes no argument to key on",
                    concern.keyword()
                ),
            })?;
            if n != 0 {
                return Err(EmitError {
                    message: format!(
                        "#[{}] key = \"#{n}\": v1 supports only `key = \"#0\"` (the first arg)",
                        concern.keyword()
                    ),
                });
            }
            // A typed key closure over the real `(A,)` arg tuple on Call.args: hash the
            // arg's Debug repr into the key payload (erasure-free read, per-arg key).
            quote! {
                (|__call: &::leaf_core::Call<'_>| -> ::core::option::Option<::std::boxed::Box<[u8]>> {
                    let (__a0,) = __call.args.downcast_ref::<(#arg_ty,)>()?;
                    ::core::option::Option::Some(
                        ::std::format!("{:?}", __a0).into_bytes().into_boxed_slice(),
                    )
                }) as ::leaf_cache::CacheKeyFn
            }
        }
    };

    let contract = unique_contract("leaf::cache::CacheAdvisor", bean_ident, sig);
    let statics = pointcut_statics(
        self_ty,
        &types_ident,
        &pointcut_ident,
        quote! { ::leaf_cache::CachePointcut },
    );
    // Dispatch on the manager parameter's SYNTACTIC SHAPE (never a spelled name): a
    // `dyn CacheManager` trait object resolves through the GENERAL by-trait injection
    // path (resolve_view); a concrete type keeps the ByType + downcast path. Only the
    // builder fn differs — the per-method op/meta/key/T are baked identically.
    let build = if manager.is_trait_object() {
        quote! {
            ::leaf_cache::build_cache_interceptor_view::<#value_ty>(
                __c,
                ::leaf_core::MethodKey::of(#method_key),
                #op,
                &#meta_ident,
                #key_fn,
            )
        }
    } else {
        let mty = &manager.ty;
        quote! {
            ::leaf_cache::build_cache_interceptor::<#mty, #value_ty>(
                __c,
                ::leaf_core::MethodKey::of(#method_key),
                #op,
                &#meta_ident,
                #key_fn,
            )
        }
    };
    Ok(quote! {
        #statics
        // The PUBLIC per-method CacheOpMeta const the cache interceptor reads.
        #[allow(non_upper_case_globals)]
        pub const #meta_ident: ::leaf_core::CacheOpMeta = ::leaf_core::CacheOpMeta {
            cache_names: &[ #(#names),* ],
            all_entries: #all,
            before_invocation: false,
            sync: #sync,
        };
        // The cache advisor auto-wire row: resolve the named manager + build a
        // single-rule CacheInterceptor over the method's return `T` + the key fn,
        // keyed by the bean's TypeId at CACHE_ORDER (OUTSIDE tx). The contract is
        // per-method-unique so two cache methods on one bean do not collide.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::AdvisorPairingRow = ::leaf_core::AdvisorPairingRow {
            contract: #contract,
            order: ::leaf_cache::cache_order_key(),
            role: ::leaf_core::Role::Infrastructure,
            pointcut: &#pointcut_ident,
            make_interceptor: |__c: &dyn ::leaf_core::Container| {
                ::std::boxed::Box::pin(#build)
            },
        };
    })
}

/// Emit the `#[validated]` artifact: the `ADVISOR_PAIRINGS` row binding the
/// single-`@Valid`-arg validator over the method's first arg type `A`, keyed by a
/// `ValidationPointcut` over the bean's `TypeId` at `VALIDATE_ORDER`.
fn emit_validated(
    bean_ident: &str,
    self_ty: &Type,
    sig: &MethodSig,
) -> Result<TokenStream, EmitError> {
    let base = mangle(&ident_base(bean_ident, sig));
    let types_ident = format_ident!("__LEAF_VALID_TYPES_{}", base);
    let pointcut_ident = format_ident!("__LEAF_VALID_POINTCUT_{}", base);
    let row_ident = format_ident!("__LEAF_VALID_ADVISOR_{}", base);
    let arg_ty = sig.first_arg_type.as_ref().ok_or_else(|| EmitError {
        message: format!(
            "#[validated] on `{}` validates the method's first `@Valid` argument, \
             but the method takes no argument",
            sig.method_path
        ),
    })?;
    let contract = unique_contract("leaf::validation::MethodValidationAdvisor", bean_ident, sig);
    let statics = pointcut_statics(
        self_ty,
        &types_ident,
        &pointcut_ident,
        quote! { ::leaf_validation::ValidationPointcut },
    );
    Ok(quote! {
        #statics
        // The validation advisor auto-wire row: validate the single `@Valid` arg `A`
        // before the body (a bad arg short-circuits), keyed by the bean's TypeId at
        // VALIDATE_ORDER (OUTERMOST). The contract is per-method-unique.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::AdvisorPairingRow = ::leaf_core::AdvisorPairingRow {
            contract: #contract,
            order: ::leaf_validation::validation_order_key(),
            role: ::leaf_core::Role::Infrastructure,
            pointcut: &#pointcut_ident,
            make_interceptor: ::leaf_validation::single_arg_make_interceptor::<#arg_ty>(),
        };
    })
}

/// Emit the `#[retryable]` artifact: the `ADVISOR_PAIRINGS` row binding a
/// `RetryInterceptor` (max attempts + backoff + the `Result<T,_>` classifier), keyed
/// by a `ResiliencePointcut` over the bean's `TypeId` at `RETRY_ORDER`.
fn emit_retryable(
    args: &RetryableArgs,
    bean_ident: &str,
    self_ty: &Type,
    sig: &MethodSig,
) -> Result<TokenStream, EmitError> {
    let base = mangle(&ident_base(bean_ident, sig));
    let types_ident = format_ident!("__LEAF_RETRY_TYPES_{}", base);
    let pointcut_ident = format_ident!("__LEAF_RETRY_POINTCUT_{}", base);
    let row_ident = format_ident!("__LEAF_RETRY_ADVISOR_{}", base);
    let max = args.max;
    let ret = result_ok_ty(&sig.ret_type);
    let backoff = lower_backoff(&args.backoff);
    let contract = unique_contract("leaf::resilience::RetryAdvisor", bean_ident, sig);
    let statics = pointcut_statics(
        self_ty,
        &types_ident,
        &pointcut_ident,
        quote! { ::leaf_resilience::ResiliencePointcut },
    );
    Ok(quote! {
        #statics
        // The retry advisor auto-wire row: re-proceed the args-bearing method up to
        // `max` attempts on a retryable error, with the `Result<T,_>` classifier so a
        // business `Err` drives the retry, keyed by the bean's TypeId at RETRY_ORDER.
        // The contract is per-method-unique.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::AdvisorPairingRow = ::leaf_core::AdvisorPairingRow {
            contract: #contract,
            order: ::leaf_resilience::retry_order_key(),
            role: ::leaf_core::Role::Infrastructure,
            pointcut: &#pointcut_ident,
            make_interceptor: |_c: &dyn ::leaf_core::Container| {
                ::std::boxed::Box::pin(async move {
                    let __retry = ::leaf_resilience::ResilientRetry::new(
                        ::leaf_core::RetryPolicy::new(#max),
                        #backoff,
                    );
                    // Bind the PROCESS-DEFAULT reactive sleeper (the runtime install
                    // seam): a timer-backed sleeper if a runtime crate installed one
                    // (e.g. leaf-tokio's `install_tokio_sleeper`), else the
                    // runtime-free `ImmediateSleeper`. Without this the timed backoff
                    // would silently sleep ZERO. leaf-codegen names no runtime crate,
                    // so it consults the process-default slot rather than resolving a
                    // concrete sleeper type from the container.
                    // Bind the PROCESS-DEFAULT reactive sleeper (the runtime install
                    // seam): a timer-backed sleeper if a runtime crate installed one
                    // (e.g. leaf-tokio's `install_tokio_sleeper`), else the
                    // runtime-free `ImmediateSleeper`. Without this the timed backoff
                    // would silently sleep ZERO. leaf-codegen names no runtime crate,
                    // so it consults the process-default slot rather than resolving a
                    // concrete sleeper type from the container.
                    let __interceptor = ::leaf_resilience::RetryInterceptor::new(__retry)
                        .with_sleeper(::leaf_resilience::default_sleeper())
                        .with_return_classifier(::leaf_resilience::result_classifier::<#ret>());
                    ::core::result::Result::Ok(
                        ::std::sync::Arc::new(__interceptor) as ::std::sync::Arc<dyn ::leaf_core::Interceptor>,
                    )
                })
            },
        };
    })
}

/// Emit the `#[concurrency_limit]` artifact: the `ADVISOR_PAIRINGS` row binding the
/// named gate `G`, keyed by a `ResiliencePointcut` over the bean's `TypeId` at
/// `CONCURRENCY_ORDER`.
fn emit_concurrency_limit(
    args: &ConcurrencyLimitArgs,
    bean_ident: &str,
    self_ty: &Type,
    sig: &MethodSig,
) -> Result<TokenStream, EmitError> {
    let base = mangle(&ident_base(bean_ident, sig));
    let types_ident = format_ident!("__LEAF_LIMIT_TYPES_{}", base);
    let pointcut_ident = format_ident!("__LEAF_LIMIT_POINTCUT_{}", base);
    let row_ident = format_ident!("__LEAF_LIMIT_ADVISOR_{}", base);
    let gate: Type = syn::parse_str(args.gate.as_deref().unwrap_or_default())
        .map_err(|e| EmitError { message: format!("`gate` is not a type: {e}") })?;
    let contract = unique_contract("leaf::resilience::ConcurrencyLimitAdvisor", bean_ident, sig);
    let statics = pointcut_statics(
        self_ty,
        &types_ident,
        &pointcut_ident,
        quote! { ::leaf_resilience::ResiliencePointcut },
    );
    Ok(quote! {
        #statics
        // The concurrency-limit advisor auto-wire row: resolve the named gate bean +
        // wrap it in a ConcurrencyLimitInterceptor, keyed by the bean's TypeId at
        // CONCURRENCY_ORDER (INSIDE tx — the permit is held only for the actual work).
        // The contract is per-method-unique.
        #[allow(non_upper_case_globals)]
        #[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]
        #[linkme(crate = ::leaf_core::linkme)]
        static #row_ident: ::leaf_core::AdvisorPairingRow = ::leaf_core::AdvisorPairingRow {
            contract: #contract,
            order: ::leaf_resilience::concurrency_order_key(),
            role: ::leaf_core::Role::Infrastructure,
            pointcut: &#pointcut_ident,
            make_interceptor: ::leaf_resilience::make_concurrency_interceptor::<#gate>(),
        };
    })
}

/// Lower a [`Backoff`] to its `Arc<dyn BackoffPolicy>` const expression.
fn lower_backoff(backoff: &Backoff) -> TokenStream {
    match backoff {
        Backoff::None => quote! {
            ::std::sync::Arc::new(::leaf_resilience::NoBackoff)
                as ::std::sync::Arc<dyn ::leaf_core::BackoffPolicy>
        },
        Backoff::Fixed(ms) => quote! {
            ::std::sync::Arc::new(::leaf_resilience::FixedBackoff {
                delay: ::core::time::Duration::from_millis(#ms),
            }) as ::std::sync::Arc<dyn ::leaf_core::BackoffPolicy>
        },
        Backoff::Exponential { base_ms, mult } => quote! {
            ::std::sync::Arc::new(::leaf_resilience::ExponentialBackoff::new(
                ::core::time::Duration::from_millis(#base_ms),
                #mult,
            )) as ::std::sync::Arc<dyn ::leaf_core::BackoffPolicy>
        },
    }
}

/// The `Ok` type of a `Result<T, E>` return (for the tx/retry classifier), or the
/// whole type if it is not a `Result<…>` (the classifier degrades to "never classify
/// a non-Result return" — the interceptor only uses it for a `Result`-returning method).
fn result_ok_ty(ret: &Type) -> Type {
    if let Type::Path(tp) = ret
        && let Some(seg) = tp.path.segments.last()
        && seg.ident == "Result"
        && let syn::PathArguments::AngleBracketed(ab) = &seg.arguments
        && let Some(syn::GenericArgument::Type(ok)) = ab.args.first()
    {
        return ok.clone();
    }
    ret.clone()
}

// ─────────────────────── small attr-item parsing ────────────────────────────

/// One parsed top-level attribute item: `key = value`, `name(..)` call, or a bare
/// positional expression.
enum AttrItem {
    /// `key = value`. The value is captured as a [`syn::Type`] when `key` is a
    /// type-valued key (`manager`/`gate` — so `manager = dyn CacheManager` parses,
    /// where a `syn::Expr` cannot), else as a [`syn::Expr`].
    Assign { key: String, value: AttrValue },
    /// `name(inner, …)` (e.g. `rollback_for(Kind)`) — only the callee name is read
    /// (the rollback-rule list is accepted; the default any-`Err` rule applies in v1).
    Call { name: String },
    /// A bare positional expression (a string cache name, `all_entries`, an int `n`).
    Positional(syn::Expr),
}

/// The right-hand side of a `key = value` item: a TYPE (for the type-valued
/// `manager`/`gate` keys, so a `dyn Trait` trait object parses) or an EXPRESSION
/// (every other key — strings, bools, ints, the `backoff = …` calls).
enum AttrValue {
    /// A type RHS (`manager = dyn CacheManager` / `manager = Concrete` / `gate = G`).
    Type(Type),
    /// An expression RHS (`cache = "u"`, `key = "#0"`, `max = 3`, `backoff = …`).
    Expr(syn::Expr),
}

/// Keys whose `key = value` RHS is parsed as a [`syn::Type`] rather than a
/// [`syn::Expr`] — so a trait-object view (`dyn CacheManager`) is acceptable. A
/// concrete path is a valid `syn::Type` too, so the concrete form is unaffected.
fn is_type_valued_key(key: &str) -> bool {
    matches!(key, "manager" | "gate")
}

/// Parse an attribute body into its top-level comma-separated items.
///
/// Hand-split on top-level commas (respecting `()`/`[]`/`{}` nesting) so each segment
/// can be parsed on its own grammar: a `manager = …` / `gate = …` segment parses its
/// RHS as a [`syn::Type`] (a `dyn Trait` trait object is a type, NOT a `syn::Expr`),
/// while every other segment parses as a [`syn::Expr`] exactly as before.
fn attr_items(attr: TokenStream, kw: &str) -> Result<Vec<AttrItem>, EmitError> {
    if attr.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for segment in split_top_level_commas(attr) {
        items.push(parse_attr_item(segment, kw)?);
    }
    Ok(items)
}

/// Split a token stream on its TOP-LEVEL commas (a comma not nested inside any
/// delimiter group), dropping the separators. Trailing empty segments are skipped.
fn split_top_level_commas(attr: TokenStream) -> Vec<TokenStream> {
    let mut segments = Vec::new();
    let mut current = TokenStream::new();
    for tt in attr {
        match &tt {
            proc_macro2::TokenTree::Punct(p) if p.as_char() == ',' => {
                segments.push(std::mem::take(&mut current));
            }
            _ => current.extend(std::iter::once(tt)),
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Parse ONE comma-separated segment into an [`AttrItem`]: a `key = value` assign
/// (value parsed as a [`syn::Type`] for `manager`/`gate`, else a [`syn::Expr`]), a
/// `name(..)` call, or a bare positional expression.
fn parse_attr_item(segment: TokenStream, kw: &str) -> Result<AttrItem, EmitError> {
    // A `key = …` assign: peel the leading `ident =`, then parse the RHS on the
    // grammar the key selects (Type for manager/gate so `dyn Trait` is accepted).
    if let Some((key, rhs)) = split_assign(&segment) {
        if is_type_valued_key(&key) {
            let ty: Type = syn::parse2(rhs).map_err(|e| EmitError {
                message: format!("`{key}` must be a bean TYPE (a path or `dyn Trait`): {e}"),
            })?;
            return Ok(AttrItem::Assign { key, value: AttrValue::Type(ty) });
        }
        let expr: syn::Expr = syn::parse2(rhs).map_err(|e| EmitError {
            message: format!("malformed #[{kw}] argument `{key} = …`: {e}"),
        })?;
        return Ok(AttrItem::Assign { key, value: AttrValue::Expr(expr) });
    }
    // Not an assign: parse the whole segment as an expression (a call or positional).
    let expr: syn::Expr = syn::parse2(segment).map_err(|e| EmitError {
        message: format!("malformed #[{kw}] argument: {e}"),
    })?;
    match expr {
        syn::Expr::Call(call) => {
            let name = call_name(&call.func).ok_or_else(|| EmitError {
                message: format!("#[{kw}]: a call argument needs a bare name, e.g. `rollback_for(..)`"),
            })?;
            Ok(AttrItem::Call { name })
        }
        other => Ok(AttrItem::Positional(other)),
    }
}

/// If `segment` is `ident = <rest>`, return `(ident, <rest>)` (the RHS tokens);
/// otherwise `None`. Recognises the assign only when the FIRST token is a bare
/// identifier and the SECOND is a lone `=` (never `==`/`=>`/…), so a positional
/// expression that merely contains `=` deeper is not misread.
fn split_assign(segment: &TokenStream) -> Option<(String, TokenStream)> {
    let mut iter = segment.clone().into_iter();
    let key = match iter.next()? {
        proc_macro2::TokenTree::Ident(id) => id.to_string(),
        _ => return None,
    };
    match iter.next()? {
        proc_macro2::TokenTree::Punct(p)
            if p.as_char() == '=' && p.spacing() == proc_macro2::Spacing::Alone => {}
        _ => return None,
    }
    Some((key, iter.collect()))
}

/// An "unknown argument" error naming the expected keys.
fn unknown(key: &str, kw: &str, expected: &str) -> EmitError {
    EmitError {
        message: format!("unknown #[{kw}] argument `{key}` (expected `{expected}`)"),
    }
}

/// An "unexpected positional argument" error.
fn positional(expr: &syn::Expr, kw: &str) -> EmitError {
    EmitError {
        message: format!("unexpected #[{kw}] argument `{}`", quote! { #expr }),
    }
}

/// A spans-free, identifier-safe mangling for emitted helper names.
fn mangle(ident: &str) -> syn::Ident {
    let safe: String = ident
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    syn::Ident::new(&safe, proc_macro2::Span::call_site())
}

/// The bare ident of an assignment / call left-hand side.
fn assign_ident(expr: &syn::Expr) -> Result<String, EmitError> {
    match expr {
        syn::Expr::Path(p) => p.path.get_ident().map(ToString::to_string).ok_or_else(|| EmitError {
            message: "a named argument must use a bare identifier key".into(),
        }),
        _ => Err(EmitError {
            message: "a named argument must use a bare identifier key".into(),
        }),
    }
}

/// The bare name of a call's callee (`exponential(..)` → `exponential`).
fn call_name(func: &syn::Expr) -> Option<String> {
    match func {
        syn::Expr::Path(p) => p.path.get_ident().map(ToString::to_string),
        _ => None,
    }
}

/// A `key = <type>` right-hand side as a [`syn::Type`]. The value is ALREADY parsed
/// as a type by [`parse_attr_item`] for the type-valued keys (`manager`/`gate`), so
/// this only unwraps it; an `AttrValue::Expr` here is a parser-internal invariant
/// violation, surfaced as a clear error.
fn type_value(value: AttrValue, key: &str) -> Result<Type, EmitError> {
    match value {
        AttrValue::Type(ty) => Ok(ty),
        AttrValue::Expr(e) => Err(EmitError {
            message: format!("`{key}` must be a bean TYPE, got `{}`", quote! { #e }),
        }),
    }
}

impl AttrValue {
    /// Unwrap the EXPRESSION RHS, erroring if the value was parsed as a type (only
    /// the type-valued keys parse as a type, so this is the non-`manager`/`gate` path).
    fn expect_expr(self, key: &str) -> Result<syn::Expr, EmitError> {
        match self {
            AttrValue::Expr(e) => Ok(e),
            AttrValue::Type(ty) => Err(EmitError {
                message: format!("`{key}` must be a value, got the type `{}`", quote! { #ty }),
            }),
        }
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

/// The unsigned integer value of a numeric right-hand side.
fn uint_value(expr: &syn::Expr) -> Result<u64, EmitError> {
    uint_lit(expr).ok_or_else(|| EmitError {
        message: format!("expected an integer literal, got `{}`", quote! { #expr }),
    })
}

/// The unsigned integer value of an int-literal expression, else `None`.
fn uint_lit(expr: &syn::Expr) -> Option<u64> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(i), .. }) => i.base10_parse::<u64>().ok(),
        _ => None,
    }
}

/// The float value of a numeric right-hand side (allows an int literal coerced to f64).
fn float_value(expr: &syn::Expr) -> Result<f64, EmitError> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Float(f), .. }) => {
            f.base10_parse::<f64>().map_err(|e| EmitError {
                message: format!("expected a float: {e}"),
            })
        }
        syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(i), .. }) => {
            i.base10_parse::<f64>().map_err(|e| EmitError {
                message: format!("expected a number: {e}"),
            })
        }
        other => Err(EmitError {
            message: format!("expected a float, got `{}`", quote! { #other }),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(ts: &TokenStream) -> String {
        ts.to_string().split_whitespace().collect::<String>()
    }

    fn ty(s: &str) -> Type {
        syn::parse_str(s).expect("a valid type")
    }

    fn sig(method: &str, ret: &str, arg: Option<&str>) -> MethodSig {
        MethodSig {
            method_path: method.into(),
            ret_type: ty(ret),
            first_arg_type: arg.map(ty),
        }
    }

    fn parse(s: &str) -> TokenStream {
        s.parse().expect("tokens")
    }

    fn mgr_str(m: &ManagerRef) -> String {
        let t = &m.ty;
        quote! { #t }.to_string().split_whitespace().collect()
    }

    // ── keyword recognition ──────────────────────────────────────────────────

    #[test]
    fn recognises_every_concern_keyword() {
        assert_eq!(Concern::from_keyword("transactional"), Some(Concern::Transactional));
        assert_eq!(Concern::from_keyword("cacheable"), Some(Concern::Cacheable));
        assert_eq!(Concern::from_keyword("cache_put"), Some(Concern::CachePut));
        assert_eq!(Concern::from_keyword("cache_evict"), Some(Concern::CacheEvict));
        assert_eq!(Concern::from_keyword("validated"), Some(Concern::Validated));
        assert_eq!(Concern::from_keyword("retryable"), Some(Concern::Retryable));
        assert_eq!(Concern::from_keyword("concurrency_limit"), Some(Concern::ConcurrencyLimit));
        assert_eq!(Concern::from_keyword("bean"), None);
    }

    // ── #[transactional] ───────────────────────────────────────────────────────

    #[test]
    fn transactional_emits_a_tx_advisor_pairing_row_keyed_by_the_bean_type() {
        let args = parse_transactional(parse("manager = LedgerTxManager")).expect("parses");
        let mgr = args.manager.as_ref().expect("a manager");
        assert_eq!(mgr_str(mgr), "LedgerTxManager");
        assert!(!mgr.is_trait_object(), "a concrete path is not a trait object");
        let ts = emit_transactional(
            &args,
            "LedgerService",
            &ty("LedgerService"),
            &sig("LedgerService::record", "Result<i64, LeafError>", Some("i64")),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The bean-TypeId pointcut + the row in ADVISOR_PAIRINGS.
        assert!(
            s.contains("::core::any::TypeId::of::<LedgerService>()"),
            "keyed by the bean TypeId: {s}"
        );
        assert!(s.contains("::leaf_tx::TxPointcut::new"), "got: {s}");
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::ADVISOR_PAIRINGS)]"),
            "got: {s}"
        );
        // The named manager + the `Result<i64,_>` Ok-type classifier ride the builder,
        // at the pinned TX_ORDER, with a per-method-unique contract.
        assert!(
            s.contains("::leaf_tx::make_transaction_interceptor_for::<LedgerTxManager,i64>"),
            "got: {s}"
        );
        assert!(s.contains("order:::leaf_tx::tx_order_key()"), "got: {s}");
        assert!(s.contains("role:::leaf_core::Role::Infrastructure"), "got: {s}");
        // The contract is the family base @ the module-qualified Bean::method.
        assert!(
            s.contains(r#""leaf::tx::TransactionAdvisor","@""#),
            "per-method-unique contract: {s}"
        );
    }

    #[test]
    fn transactional_requires_a_manager() {
        let err = parse_transactional(TokenStream::new()).expect_err("no manager errors");
        assert!(err.message.contains("manager"), "got: {}", err.message);
    }

    #[test]
    fn transactional_accepts_rollback_for_without_error() {
        // The natural annotation `rollback_for(..)` parses (the default rule applies).
        parse_transactional(parse("manager = M, rollback_for(MyErr)")).expect("rollback_for parses");
    }

    #[test]
    fn transactional_manager_dyn_view_dispatches_to_the_by_trait_builder() {
        // `manager = dyn TransactionManager` is a trait object (NOT a syn::Expr) — it
        // parses as a Type and dispatches on the syntactic SHAPE to the by-view builder
        // (resolve_view), NEVER on the spelled name.
        let args = parse_transactional(parse("manager = dyn TransactionManager")).expect("parses");
        let mgr = args.manager.as_ref().expect("a manager");
        assert!(mgr.is_trait_object(), "a `dyn Trait` manager is a trait object");
        let s = flat(
            &emit_transactional(
                &args,
                "LedgerService",
                &ty("LedgerService"),
                &sig("LedgerService::record", "Result<i64, LeafError>", Some("i64")),
            )
            .expect("emits"),
        );
        // The by-VIEW builder (no concrete manager generic), with only the return-T.
        assert!(
            s.contains("::leaf_tx::make_transaction_interceptor_for_view::<i64>()"),
            "the dyn-view manager rides the by-trait builder: {s}"
        );
        assert!(
            !s.contains("make_transaction_interceptor_for::<"),
            "the dyn-view path does NOT use the concrete ByType builder: {s}"
        );
    }

    #[test]
    fn transactional_manager_qualified_dyn_path_is_still_a_trait_object() {
        // A path-qualified trait object (`dyn ::leaf::core::TransactionManager`) is
        // still a Type::TraitObject — the dispatch is purely structural.
        let args =
            parse_transactional(parse("manager = dyn leaf::core::TransactionManager")).expect("parses");
        assert!(args.manager.as_ref().expect("manager").is_trait_object());
    }

    // ── #[cacheable] / #[cache_put] / #[cache_evict] ─────────────────────────────

    #[test]
    fn cacheable_emits_meta_plus_a_cache_advisor_row_with_a_unit_key() {
        let args = parse_cache(parse(r#""users", manager = Mgr"#), Concern::Cacheable).expect("parses");
        assert_eq!(args.cache_names, vec!["users".to_string()]);
        assert_eq!(args.key_arg, None);
        let ts = emit_cache(
            Concern::Cacheable,
            &args,
            "UserService",
            &ty("UserService"),
            &sig("UserService::find", "i64", None),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("::leaf_core::CacheOpMeta{"), "got: {s}");
        assert!(s.contains(r#"cache_names:&["users"]"#), "got: {s}");
        assert!(s.contains("::core::any::TypeId::of::<UserService>()"), "got: {s}");
        assert!(s.contains("::leaf_cache::CachePointcut::new"), "got: {s}");
        assert!(s.contains("::leaf_cache::CacheOp::Cacheable"), "got: {s}");
        assert!(
            s.contains("::leaf_cache::build_cache_interceptor::<Mgr,i64>"),
            "the manager + return-T ride the builder: {s}"
        );
        assert!(s.contains("::leaf_cache::unit_key_fn()"), "unit key by default: {s}");
        assert!(s.contains("order:::leaf_cache::cache_order_key()"), "at CACHE_ORDER: {s}");
        assert!(
            s.contains(r#""leaf::cache::CacheAdvisor","@""#),
            "per-method-unique contract: {s}"
        );
    }

    #[test]
    fn cacheable_manager_dyn_view_dispatches_to_the_by_trait_builder() {
        // `manager = dyn CacheManager` (a trait object, not a syn::Expr) dispatches on
        // SHAPE to the by-view builder (resolve_view), never on the spelled name. The
        // per-method op/meta/key/T are baked identically to the concrete path.
        let args = parse_cache(parse(r##""prices", key = "#0", manager = dyn CacheManager"##), Concern::Cacheable)
            .expect("parses");
        assert!(args.manager.as_ref().expect("manager").is_trait_object());
        let s = flat(
            &emit_cache(
                Concern::Cacheable,
                &args,
                "CatalogService",
                &ty("CatalogService"),
                &sig("CatalogService::price_of", "Result<i64, LeafError>", Some("String")),
            )
            .expect("emits"),
        );
        // The by-VIEW builder (no concrete manager generic), only the value type T.
        assert!(
            s.contains("::leaf_cache::build_cache_interceptor_view::<Result<i64,LeafError>>"),
            "the dyn-view manager rides the by-trait builder: {s}"
        );
        assert!(
            !s.contains("::leaf_cache::build_cache_interceptor::<"),
            "the dyn-view path does NOT use the concrete ByType builder: {s}"
        );
        // The key fn + meta are unchanged.
        assert!(s.contains("__call.args.downcast_ref::<(String,)>()"), "the key fn rides: {s}");
        assert!(s.contains(r#"cache_names:&["prices"]"#), "the meta rides: {s}");
    }

    #[test]
    fn cacheable_key_zero_emits_a_typed_arg_key_fn() {
        let args =
            parse_cache(parse(r##"cache = "users", key = "#0", manager = Mgr"##), Concern::Cacheable)
                .expect("parses");
        assert_eq!(args.key_arg, Some(0));
        let ts = emit_cache(
            Concern::Cacheable,
            &args,
            "UserService",
            &ty("UserService"),
            &sig("UserService::find", "i64", Some("u64")),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        // The key fn downcasts the `(u64,)` arg tuple off Call.args (per-arg key).
        assert!(s.contains("__call.args.downcast_ref::<(u64,)>()"), "got: {s}");
        assert!(s.contains("as::leaf_cache::CacheKeyFn"), "got: {s}");
    }

    #[test]
    fn cache_evict_all_entries_sets_the_flag_and_the_op() {
        let args =
            parse_cache(parse(r#""users", all_entries, manager = Mgr"#), Concern::CacheEvict)
                .expect("parses");
        assert!(args.all_entries);
        let s = flat(
            &emit_cache(
                Concern::CacheEvict,
                &args,
                "UserService",
                &ty("UserService"),
                &sig("UserService::evict", "i64", None),
            )
            .expect("emits"),
        );
        assert!(s.contains("all_entries:true"), "got: {s}");
        assert!(s.contains("::leaf_cache::CacheOp::CacheEvict"), "got: {s}");
    }

    #[test]
    fn cache_put_uses_the_put_op() {
        let args = parse_cache(parse(r#""users", manager = Mgr"#), Concern::CachePut).expect("parses");
        let s = flat(
            &emit_cache(
                Concern::CachePut,
                &args,
                "S",
                &ty("S"),
                &sig("S::refresh", "i64", None),
            )
            .expect("emits"),
        );
        assert!(s.contains("::leaf_cache::CacheOp::CachePut"), "got: {s}");
    }

    #[test]
    fn cacheable_requires_a_name_and_a_manager() {
        let err = parse_cache(parse("manager = M"), Concern::Cacheable).expect_err("no name errors");
        assert!(err.message.contains("cache name"), "got: {}", err.message);
        let err = parse_cache(parse(r#""users""#), Concern::Cacheable).expect_err("no manager errors");
        assert!(err.message.contains("manager"), "got: {}", err.message);
    }

    #[test]
    fn cacheable_key_on_a_no_arg_method_is_an_error() {
        let args = parse_cache(parse(r##""u", key = "#0", manager = M"##), Concern::Cacheable).unwrap();
        let err = emit_cache(
            Concern::Cacheable,
            &args,
            "S",
            &ty("S"),
            &sig("S::find", "i64", None),
        )
        .expect_err("keying a no-arg method errors");
        assert!(err.message.contains("no argument to key on"), "got: {}", err.message);
    }

    #[test]
    fn cacheable_key_non_zero_is_rejected_in_v1() {
        let err = parse_cache(parse(r##""u", key = "#3", manager = M"##), Concern::Cacheable)
            .expect("parses the index")
            .key_arg;
        assert_eq!(err, Some(3));
        let args = CacheableArgs { key_arg: Some(3), ..Default::default() };
        let err = emit_cache(
            Concern::Cacheable,
            &CacheableArgs {
                cache_names: vec!["u".into()],
                manager: Some(ManagerRef { ty: ty("M") }),
                ..args
            },
            "S",
            &ty("S"),
            &sig("S::find", "i64", Some("u64")),
        )
        .expect_err("a non-#0 key is rejected in v1");
        assert!(err.message.contains("only `key = \"#0\"`"), "got: {}", err.message);
    }

    // ── #[validated] ─────────────────────────────────────────────────────────────

    #[test]
    fn validated_emits_a_validation_advisor_row_over_the_first_arg() {
        let ts = emit_validated(
            "SignupService",
            &ty("SignupService"),
            &sig("SignupService::create", "Result<String, LeafError>", Some("CreateUser")),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("::core::any::TypeId::of::<SignupService>()"), "got: {s}");
        assert!(s.contains("::leaf_validation::ValidationPointcut::new"), "got: {s}");
        assert!(
            s.contains("::leaf_validation::single_arg_make_interceptor::<CreateUser>()"),
            "the validator is keyed on the first arg type: {s}"
        );
        assert!(s.contains("order:::leaf_validation::validation_order_key()"), "got: {s}");
    }

    #[test]
    fn validated_on_a_no_arg_method_is_an_error() {
        let err = emit_validated("S", &ty("S"), &sig("S::ping", "()", None))
            .expect_err("validating a no-arg method errors");
        assert!(err.message.contains("no argument"), "got: {}", err.message);
    }

    // ── #[retryable] ───────────────────────────────────────────────────────────

    #[test]
    fn retryable_defaults_to_three_attempts_no_backoff() {
        let args = parse_retryable(TokenStream::new()).expect("empty parses");
        assert_eq!(args, RetryableArgs { max: 3, backoff: Backoff::None });
    }

    #[test]
    fn retryable_emits_a_retry_advisor_row_with_the_classifier() {
        let args = parse_retryable(parse("max = 5")).expect("parses");
        assert_eq!(args.max, 5);
        let ts = emit_retryable(
            &args,
            "FlakyService",
            &ty("FlakyService"),
            &sig("FlakyService::flaky", "Result<i64, LeafError>", Some("i64")),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("::core::any::TypeId::of::<FlakyService>()"), "got: {s}");
        assert!(s.contains("::leaf_resilience::ResiliencePointcut::new"), "got: {s}");
        assert!(s.contains("::leaf_core::RetryPolicy::new(5u32)"), "got: {s}");
        assert!(s.contains("::leaf_resilience::NoBackoff"), "got: {s}");
        // The `Result<i64,_>` Ok-type classifier so a business `Err` drives the retry.
        assert!(s.contains("::leaf_resilience::result_classifier::<i64>()"), "got: {s}");
        assert!(s.contains("::leaf_resilience::retry_order_key()"), "got: {s}");
        // The emitted interceptor binds the PROCESS-DEFAULT reactive sleeper so a
        // timed backoff is REAL once a runtime sleeper is installed (it degrades to
        // the runtime-free ImmediateSleeper when none is — never a no-op silently).
        assert!(s.contains(".with_sleeper(::leaf_resilience::default_sleeper())"), "got: {s}");
    }

    #[test]
    fn retryable_exponential_backoff_lowers_to_the_const_policy() {
        let args = parse_retryable(parse("max = 4, backoff = exponential(base = 10, mult = 2.0)"))
            .expect("parses");
        assert_eq!(args.backoff, Backoff::Exponential { base_ms: 10, mult: 2.0 });
        let s = flat(
            &emit_retryable(
                &args,
                "S",
                &ty("S"),
                &sig("S::flaky", "Result<i64, LeafError>", None),
            )
            .expect("emits"),
        );
        assert!(s.contains("::leaf_resilience::ExponentialBackoff::new"), "got: {s}");
        assert!(s.contains("from_millis(10u64)"), "got: {s}");
    }

    #[test]
    fn retryable_fixed_backoff_lowers() {
        let args = parse_retryable(parse("backoff = fixed(50)")).expect("parses");
        assert_eq!(args.backoff, Backoff::Fixed(50));
        let s = flat(
            &emit_retryable(&args, "S", &ty("S"), &sig("S::f", "Result<i64,E>", None)).expect("emits"),
        );
        assert!(s.contains("::leaf_resilience::FixedBackoff"), "got: {s}");
        assert!(s.contains("from_millis(50u64)"), "got: {s}");
    }

    // ── #[concurrency_limit] ─────────────────────────────────────────────────────

    #[test]
    fn concurrency_limit_emits_a_gate_advisor_row() {
        let args = parse_concurrency_limit(parse("2, gate = LimitGate")).expect("parses");
        assert_eq!(args.gate.as_deref(), Some("LimitGate"));
        let ts = emit_concurrency_limit(
            &args,
            "GuardedService",
            &ty("GuardedService"),
            &sig("GuardedService::guarded", "i64", Some("i64")),
        )
        .expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("valid items");
        let s = flat(&ts);
        assert!(s.contains("::core::any::TypeId::of::<GuardedService>()"), "got: {s}");
        assert!(
            s.contains("::leaf_resilience::make_concurrency_interceptor::<LimitGate>()"),
            "got: {s}"
        );
        assert!(s.contains("order:::leaf_resilience::concurrency_order_key()"), "got: {s}");
    }

    #[test]
    fn concurrency_limit_requires_a_gate() {
        let err = parse_concurrency_limit(parse("2")).expect_err("no gate errors");
        assert!(err.message.contains("gate"), "got: {}", err.message);
    }

    // ── emit_concern dispatch + result-ok-ty helper ──────────────────────────────

    #[test]
    fn emit_concern_dispatches_to_the_right_concern() {
        let ts = emit_concern(
            Concern::Validated,
            TokenStream::new(),
            "S",
            &ty("S"),
            &sig("S::create", "Result<String, E>", Some("Req")),
        )
        .expect("emits");
        assert!(flat(&ts).contains("::leaf_validation::single_arg_make_interceptor::<Req>()"));
    }

    #[test]
    fn result_ok_ty_unwraps_result_else_passes_through() {
        let ok = result_ok_ty(&ty("Result<i64, E>"));
        assert_eq!(quote! { #ok }.to_string(), "i64");
        let passthrough = result_ok_ty(&ty("u32"));
        assert_eq!(quote! { #passthrough }.to_string(), "u32");
    }
}
