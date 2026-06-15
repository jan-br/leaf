//! The remaining expression / i18n / resource ABI (definitions, not engines).
//!
//! Realizes the leaf-core surface of expr-i18n-resources (phase3/11): the
//! shared `EvalCx` SHAPE that unifies every `#{...}`/SpEL-analogue site, plus
//! the always-present `MessageSource` and `ResourceLoader` Context-service
//! traits. There is NO runtime interpreter (SEAMS: the closure-only backend
//! stands) — every expression site is a purpose-typed const fn pointer
//! ([`ValueExpr`]/[`CondExprFn`]/[`KeyExprFn`]) compiled by leaf-codegen from
//! the frozen `#{...}` subgrammar; the unification is the [`EvalCx`] context
//! shape + accessor traits, never an erased common `Expression<T>`.
//!
//! Scope note (this unit): these are pure-ABI traits + value types. The live
//! engines (the `#{...}` lowerer in leaf-codegen, the catalog providers in
//! leaf-i18n, the scheme providers in a resource crate) implement them; the
//! `CATALOGS`/`RESOURCES` linkme channels are owned by the discovery unit (they
//! carry the minimal `CatalogRow`/`ResourceRow`; the richer [`ResourceEntry`]/
//! [`CatalogDescriptor`] shapes the macro emits are pinned here). Async at every
//! `dyn` seam is a [`BoxFuture`] (AFIT/RPITIT are not `dyn`-compatible).

use std::any::Any;
use std::sync::Arc;

use crate::cx::Cx;
use crate::env::Env;
use crate::error::LeafError;
use crate::future::BoxFuture;
use crate::handle::ErasedBean;
use crate::identity::ContractId;
use crate::proxy::ResolveError;

// ═══════════════════════════ expression backend ═════════════════════════════

/// An expression-evaluation error — one node of the one [`LeafError`] chain.
///
/// Kept a distinct newtype at the expression seam (the macros emit
/// `Result<_, ExprError>`) so a `#{...}` fault is recognizable, but it lowers
/// into [`LeafError`] via [`From`] so it composes with `?` and the one
/// diagnostic spine.
#[derive(Clone, Debug)]
pub struct ExprError(pub LeafError);

impl ExprError {
    /// Wrap a [`LeafError`] as an expression error.
    #[must_use]
    pub fn new(err: LeafError) -> Self {
        ExprError(err)
    }

    /// The underlying [`LeafError`].
    #[must_use]
    pub fn into_leaf_error(self) -> LeafError {
        self.0
    }
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "expression error: {}", self.0)
    }
}

impl std::error::Error for ExprError {}

impl From<LeafError> for ExprError {
    fn from(err: LeafError) -> Self {
        ExprError(err)
    }
}

impl From<ExprError> for LeafError {
    fn from(e: ExprError) -> LeafError {
        e.0
    }
}

/// The registry-lookup seam an expression's `@bean`/`&factory` segment routes
/// through (NOT an evaluation — the registry's existing `ByName` resolution).
///
/// `bean(name)` is `@name` (`Engine::get_erased(BeanKey::ByName)`); `factory`
/// is `&name` (the `ByName + Deref` flag). `Send + Sync` because a
/// `&dyn BeanResolver` rides the [`EvalCx`].
pub trait BeanResolver: Send + Sync {
    /// Resolve a bean by name (the `@name` form).
    ///
    /// # Errors
    /// Returns a [`ResolveError`] (= [`LeafError`]) on `NoSuchBean`/`NoUniqueBean`.
    fn bean(&self, name: &str) -> Result<ErasedBean, ResolveError>;

    /// Resolve a factory bean by name (the `&name` form: `ByName + Deref`).
    ///
    /// # Errors
    /// Returns a [`ResolveError`] (= [`LeafError`]) on `NoSuchBean`/`NoUniqueBean`.
    fn factory(&self, name: &str) -> Result<ErasedBean, ResolveError>;
}

/// The ONE shared evaluation-context SHAPE every compiled expression closure
/// reads (the real unification primitive). A borrowed read-only snapshot — no
/// per-eval allocation.
///
/// `root`/`args`/`result` are the SpEL `#root`/`#args`/`#result` analogues as
/// erased handles a typed closure downcasts; `env` is the sealed property view;
/// `beans` is the registry-lookup seam; `cx` is the optional ambient bundle
/// (the locale `Holder` reads off it). `#[non_exhaustive]` so adding context is
/// not a breaking change.
#[non_exhaustive]
pub struct EvalCx<'a> {
    /// The expression root object (`#root`), if any.
    pub root: Option<&'a (dyn Any + Send + Sync)>,
    /// The method arguments (`#args`), if any.
    pub args: Option<&'a (dyn Any + Send + Sync)>,
    /// The method result (`#result`), if any (post-invocation expressions).
    pub result: Option<&'a (dyn Any + Send + Sync)>,
    /// The sealed environment property view.
    pub env: &'a Env,
    /// The registry-lookup seam for `@bean`/`&factory` segments.
    pub beans: &'a dyn BeanResolver,
    /// The optional ambient context bundle (carries the current locale).
    pub cx: Option<&'a Cx>,
}

impl<'a> EvalCx<'a> {
    /// A minimal evaluation context over an `Env` + a bean resolver (no root/
    /// args/result/cx) — the `@Value`/`@ConditionalOnExpression` shape.
    #[must_use]
    pub fn new(env: &'a Env, beans: &'a dyn BeanResolver) -> Self {
        EvalCx { root: None, args: None, result: None, env, beans, cx: None }
    }

    /// Attach the ambient context bundle (builder style).
    #[must_use]
    pub fn with_cx(mut self, cx: &'a Cx) -> Self {
        self.cx = Some(cx);
        self
    }
}

impl std::fmt::Debug for EvalCx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvalCx").finish_non_exhaustive()
    }
}

/// A `@Value("#{...}")` closure: a monomorphized const fn pointer yielding a
/// typed value (then type-conversion coerces). Compiled by leaf-codegen from
/// the frozen `#{...}` subgrammar — NO interpreter.
pub type ValueExpr<T> = fn(&EvalCx) -> Result<T, ExprError>;

/// A `@ConditionalOnExpression` / `@EventListener(condition = "…")` closure: a
/// boolean predicate over the [`EvalCx`].
pub type CondExprFn = fn(&EvalCx) -> Result<bool, ExprError>;

/// A `@Cacheable(key = "…")` closure: yields a [`CacheKeyValue`] over the
/// method args (the typed cache-key path — no interpreter on the hot path).
pub type KeyExprFn = fn(&EvalCx) -> Result<CacheKeyValue, ExprError>;

/// The OPAQUE, `dyn`-safe expression seam the `${}`-vs-`#{}` dispatcher
/// ([`interpret_with`](crate::placeholder::interpret_with)) routes every `#{...}`
/// segment through (binding-conversion phase3/07, line 63: an
/// "`Option<&dyn ExpressionEvaluator>`" the dispatcher treats as fully opaque +
/// optional).
///
/// This is NOT a runtime interpreter (SEAMS: the closure-only backend stands).
/// The live impl in leaf-codegen routes a `#{...}` body to its monomorphized
/// [`ValueExpr`] const fn-pointer compiled from the frozen `#{...}` subgrammar;
/// leaf-core only pins the `dyn` shape so the placeholder dispatcher can call it
/// without depending on the codegen crate. SYNC-PURE by contract (the expr
/// subsystem hard-errors on an async body), so dispatch never `.await`s.
pub trait ExpressionEvaluator {
    /// Evaluate the (phase-1-expanded) `#{...}` body to its rendered string.
    ///
    /// # Errors
    /// A [`LeafError`] (lowered from the expression's [`ExprError`]) on an
    /// evaluation fault — one node of the one diagnostic chain.
    fn eval(&self, body: &str) -> Result<String, LeafError>;
}

/// The erased payload a [`KeyExprFn`] yields — a typed-hash cache key fragment.
///
/// Kept a thin newtype here (the rich `CacheKey { method, payload }` lives in
/// the advice module) so the expression backend does not depend on the advice
/// surface; the cache advisor lifts this into its own key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CacheKeyValue(pub Box<[u8]>);

// ═══════════════════════════ messages / i18n ════════════════════════════════

/// A BCP-47 locale tag (i18n). Kept a thin owned wrapper so the kernel carries
/// no ICU dependency; tag parsing/negotiation lives in leaf-i18n.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Locale(pub Arc<str>);

impl Locale {
    /// Build a locale from a BCP-47 tag string.
    #[must_use]
    pub fn new(tag: impl Into<Arc<str>>) -> Self {
        Locale(tag.into())
    }

    /// The raw tag.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.0
    }
}

/// One message-formatting argument (i18n). Borrowed so a `message(..)` call
/// allocates nothing for the argument list.
#[derive(Clone, Copy, Debug)]
pub enum Arg<'a> {
    /// A string argument.
    Str(&'a str),
    /// A signed-integer argument.
    Int(i64),
    /// A floating-point argument.
    Float(f64),
    /// A boolean argument.
    Bool(bool),
}

/// A resolved message pattern (the catalog lookup result, pre-formatting).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MessagePattern(pub Arc<str>);

/// An object that carries its own message code + args + default (i18n
/// `MessageSourceResolvable`) — how a validation violation presents to the
/// [`MessageSource`]. Object-safe.
///
/// `Send + Sync` because a `&dyn MessageResolvable` is captured by the `Send`
/// [`BoxFuture`] returned from [`MessageSource::resolve`] (the async-across-`dyn`
/// standard).
pub trait MessageResolvable: Send + Sync {
    /// The candidate message codes (tried in order).
    fn codes(&self) -> &[&str];
    /// The formatting arguments.
    fn arguments(&self) -> &[Arg<'_>];
    /// The default message if no code resolves.
    fn default_message(&self) -> Option<&str>;
}

/// The hierarchy-aware message resolution facade (i18n) — an always-present
/// Context service (the bare `Engine` lacks it). `Send + Sync`, async at the
/// `dyn` seam (a catalog provider may open a resource).
pub trait MessageSource: Send + Sync {
    /// Resolve `code` with `args` for `locale` (or the ambient locale when
    /// `None`); a miss walks to the parent Context's `MessageSource`.
    ///
    /// # Errors
    /// Returns [`ErrorKind::NoSuchMessage`](crate::ErrorKind::NoSuchMessage) when
    /// no code resolves and no default is available.
    fn message<'a>(
        &'a self,
        code: &'a str,
        args: &'a [Arg<'a>],
        locale: Option<&'a Locale>,
    ) -> BoxFuture<'a, Result<Arc<str>, LeafError>>;

    /// Resolve `code`, falling back to `default` on a miss (never errors).
    fn message_or<'a>(
        &'a self,
        code: &'a str,
        args: &'a [Arg<'a>],
        default: &'a str,
        locale: Option<&'a Locale>,
    ) -> BoxFuture<'a, Arc<str>>;

    /// Resolve a [`MessageResolvable`] (the validation-violation shape).
    ///
    /// # Errors
    /// Returns [`ErrorKind::NoSuchMessage`](crate::ErrorKind::NoSuchMessage) when
    /// no code resolves and the resolvable carries no default.
    fn resolve<'a>(
        &'a self,
        r: &'a dyn MessageResolvable,
        locale: Option<&'a Locale>,
    ) -> BoxFuture<'a, Result<Arc<str>, LeafError>>;
}

/// One catalog backend (i18n) — a `Role::Infrastructure` bean, origin-agnostic.
/// The hierarchy [`MessageSource`] fans out over the discovered providers.
pub trait MessageCatalogProvider: Send + Sync {
    /// Look up the raw pattern for `code` in `locale`, if present.
    fn lookup<'a>(
        &'a self,
        code: &'a str,
        locale: &'a Locale,
    ) -> BoxFuture<'a, Option<MessagePattern>>;

    /// The provider's name (for diagnostics / chain ordering).
    fn name(&self) -> &str;
}

/// The const catalog-registration row the `register_catalog!` macro emits
/// (i18n). The `CATALOGS` linkme channel itself (carrying the minimal
/// `CatalogRow`) is owned by the discovery unit; this is the richer shape a
/// later i18n unit lifts.
#[derive(Clone, Copy, Debug)]
pub struct CatalogDescriptor {
    /// Stable cross-build identity of the catalog contribution.
    pub contract: ContractId,
    /// The base name (`messages`) the bundle files derive from.
    pub basename: &'static str,
    /// The locales this catalog ships.
    pub locales: &'static [&'static str],
}

// ═══════════════════════════ resource loading ═══════════════════════════════

/// A resource URI scheme (resource-loading): `file` / `url` / `classpath`. A
/// [`ResourceProvider`] handles exactly one scheme.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Scheme {
    /// A filesystem path (`file:`).
    File,
    /// A network URL (`http:`/`https:`).
    Url,
    /// A compiled-in classpath resource (`classpath:`) — the closed
    /// `include_bytes!`-backed [`RESOURCES`](crate::RESOURCES) table.
    Classpath,
}

/// A resource location (resource-loading) — a scheme + a logical path.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Location {
    /// The URI scheme.
    pub scheme: Scheme,
    /// The scheme-relative logical path.
    pub path: Arc<str>,
}

impl Location {
    /// Build a location.
    #[must_use]
    pub fn new(scheme: Scheme, path: impl Into<Arc<str>>) -> Self {
        Location { scheme, path: path.into() }
    }

    /// A `classpath:` location.
    #[must_use]
    pub fn classpath(path: impl Into<Arc<str>>) -> Self {
        Location::new(Scheme::Classpath, path)
    }
}

/// A resource-resolution pattern (`classpath*:/**/*.ftl`) — the
/// [`ResourcePatternResolver`] glob input.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Pattern(pub Arc<str>);

/// Tri-state resource existence (resource-loading): a remote/lazy resource may
/// not be cheaply knowable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Existence {
    /// Existence is known (`true`/`false`).
    Known(bool),
    /// Existence is not cheaply determinable without opening.
    Unknown,
}

/// A resource's stable identity (resource-loading) — its origin +
/// [`Location`].
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ResourceId {
    /// The resource location.
    pub location: Location,
}

impl ResourceId {
    /// Build a resource id from a location.
    #[must_use]
    pub fn new(location: Location) -> Self {
        ResourceId { location }
    }
}

/// A leaf-owned async byte reader (resource-loading) — runtime-agnostic
/// `AsyncRead`-shaped seam so the kernel names no runtime. `Send` because a
/// reader may cross the executor.
///
/// Scope note: the concrete poll-based read surface is fleshed out by the
/// resource unit; this pins the marker trait the `Resource::open` future yields
/// so downstream readers depend on one named type.
pub trait ResourceReader: Send {
    /// Read the next chunk into `buf`, returning the number of bytes read (0 =
    /// EOF).
    ///
    /// # Errors
    /// Returns a [`LeafError`] on an IO fault.
    fn read_chunk<'a>(&'a mut self, buf: &'a mut [u8]) -> BoxFuture<'a, Result<usize, LeafError>>;
}

/// An opened/openable resource (resource-loading). Origin-agnostic: a
/// classpath, file, or URL resource are one `dyn Resource`.
pub trait Resource: Send + Sync {
    /// The resource's stable identity.
    fn id(&self) -> &ResourceId;

    /// Whether the resource exists (tri-state).
    fn exists(&self) -> Existence;

    /// The last-modified time, if knowable.
    fn last_modified(&self) -> Option<std::time::SystemTime>;

    /// Open the resource for streaming reads.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the resource cannot be opened.
    fn open<'a>(&'a self) -> BoxFuture<'a, Result<Box<dyn ResourceReader>, LeafError>>;

    /// Read the whole resource into an owned byte buffer.
    ///
    /// # Errors
    /// Returns a [`LeafError`] on an IO fault.
    fn read_to_bytes<'a>(&'a self) -> BoxFuture<'a, Result<Vec<u8>, LeafError>>;
}

/// The origin-agnostic single-resource loader (resource-loading) — an
/// always-present Context service.
pub trait ResourceLoader: Send + Sync {
    /// Resolve a single location to a (possibly non-existent) resource.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the scheme is unknown or the location is
    /// malformed.
    fn resolve(&self, loc: &Location) -> Result<Box<dyn Resource>, LeafError>;
}

/// The pattern-capable resource loader (resource-loading) — the glob extension
/// over [`ResourceLoader`].
pub trait ResourcePatternResolver: ResourceLoader {
    /// Resolve all resources matching `pat`.
    ///
    /// # Errors
    /// Returns a [`LeafError`] on a malformed pattern or IO fault.
    fn resolve_pattern<'a>(
        &'a self,
        pat: &'a Pattern,
    ) -> BoxFuture<'a, Result<Vec<Box<dyn Resource>>, LeafError>>;
}

/// One scheme handler (resource-loading): `file`/`url`/`classpath`. The
/// always-present loader composes a scheme-map over the discovered providers.
pub trait ResourceProvider: Send + Sync {
    /// The scheme this provider handles.
    fn scheme(&self) -> Scheme;

    /// Resolve a single location within this provider's scheme.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the location is malformed for this scheme.
    fn resolve(&self, loc: &Location) -> Result<Box<dyn Resource>, LeafError>;

    /// Resolve all resources matching `pat` within this provider's scheme.
    ///
    /// # Errors
    /// Returns a [`LeafError`] on a malformed pattern or IO fault.
    fn resolve_pattern<'a>(
        &'a self,
        pat: &'a Pattern,
    ) -> BoxFuture<'a, Result<Vec<Box<dyn Resource>>, LeafError>>;
}

/// One compiled-in classpath resource row (resource-loading) — the
/// `#[resource("config/app.yaml")]` macro emits one const `ResourceEntry`
/// (`include_bytes!`-backed, hand-writable). The `RESOURCES` linkme channel
/// itself (carrying the minimal `ResourceRow`) is owned by the discovery unit;
/// this is the richer shape a later resource unit lifts.
#[derive(Clone, Copy, Debug)]
pub struct ResourceEntry {
    /// The logical classpath path (`config/app.yaml`).
    pub logical_path: &'static str,
    /// The compiled-in bytes accessor (`|| include_bytes!(..)`).
    pub bytes_fn: fn() -> &'static [u8],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{Env, EnvBuilder};
    use crate::error::{Cause, ErrorKind};

    fn empty_env() -> Env {
        EnvBuilder::new().seal_env()
    }

    // A bean resolver that never resolves anything (the bare shape).
    struct NoBeans;
    impl BeanResolver for NoBeans {
        fn bean(&self, name: &str) -> Result<ErasedBean, ResolveError> {
            Err(LeafError::new(ErrorKind::NoSuchBean)
                .caused_by(Cause::plain("resolving @bean", name.to_string())))
        }
        fn factory(&self, name: &str) -> Result<ErasedBean, ResolveError> {
            Err(LeafError::new(ErrorKind::NoSuchBean)
                .caused_by(Cause::plain("resolving &factory", name.to_string())))
        }
    }

    // ── ExprError <-> LeafError bridge ───────────────────────────────────────

    #[test]
    fn expr_error_round_trips_through_leaf_error() {
        let le = LeafError::new(ErrorKind::UnresolvedValue);
        let ee: ExprError = le.clone().into();
        let back: LeafError = ee.into();
        assert_eq!(back.kind, ErrorKind::UnresolvedValue);
    }

    // ── ValueExpr / CondExprFn are plain monomorphized fn pointers ───────────

    #[test]
    fn value_expr_is_a_const_fn_pointer_evaluated_over_evalcx() {
        // A @Value("#{server.port ?: 8080}") analogue, hand-lowered to a closure.
        const PORT: ValueExpr<u16> = |_cx: &EvalCx| Ok(8080u16);
        let env = empty_env();
        let beans = NoBeans;
        let cx = EvalCx::new(&env, &beans);
        assert_eq!((PORT)(&cx).unwrap(), 8080);
    }

    #[test]
    fn cond_expr_fn_is_a_boolean_predicate() {
        const PRED: CondExprFn = |_cx: &EvalCx| Ok(true);
        let env = empty_env();
        let beans = NoBeans;
        let cx = EvalCx::new(&env, &beans);
        assert!((PRED)(&cx).unwrap());
    }

    #[test]
    fn key_expr_fn_yields_a_cache_key_value() {
        const KEY: KeyExprFn = |_cx: &EvalCx| Ok(CacheKeyValue(Box::from(&b"k"[..])));
        let env = empty_env();
        let beans = NoBeans;
        let cx = EvalCx::new(&env, &beans);
        assert_eq!((KEY)(&cx).unwrap(), CacheKeyValue(Box::from(&b"k"[..])));
    }

    #[test]
    fn eval_cx_carries_optional_cx() {
        let env = empty_env();
        let beans = NoBeans;
        let cx_bundle = crate::cx::Cx::empty();
        let ev = EvalCx::new(&env, &beans).with_cx(&cx_bundle);
        assert!(ev.cx.is_some());
    }

    // ── BeanResolver seam ────────────────────────────────────────────────────

    #[test]
    fn bean_resolver_is_object_safe_and_routes_by_name() {
        let r: &dyn BeanResolver = &NoBeans;
        assert_eq!(r.bean("foo").unwrap_err().kind, ErrorKind::NoSuchBean);
        assert_eq!(r.factory("bar").unwrap_err().kind, ErrorKind::NoSuchBean);
    }

    // ── MessageSource / i18n ─────────────────────────────────────────────────

    struct ConstMessages;
    impl MessageSource for ConstMessages {
        fn message<'a>(
            &'a self,
            code: &'a str,
            _args: &'a [Arg<'a>],
            _locale: Option<&'a Locale>,
        ) -> BoxFuture<'a, Result<Arc<str>, LeafError>> {
            Box::pin(async move {
                if code == "greeting" {
                    Ok(Arc::from("hello"))
                } else {
                    Err(LeafError::new(ErrorKind::NoSuchMessage))
                }
            })
        }
        fn message_or<'a>(
            &'a self,
            code: &'a str,
            args: &'a [Arg<'a>],
            default: &'a str,
            locale: Option<&'a Locale>,
        ) -> BoxFuture<'a, Arc<str>> {
            Box::pin(async move {
                match self.message(code, args, locale).await {
                    Ok(s) => s,
                    Err(_) => Arc::from(default),
                }
            })
        }
        fn resolve<'a>(
            &'a self,
            r: &'a dyn MessageResolvable,
            locale: Option<&'a Locale>,
        ) -> BoxFuture<'a, Result<Arc<str>, LeafError>> {
            Box::pin(async move {
                for code in r.codes() {
                    if let Ok(s) = self.message(code, r.arguments(), locale).await {
                        return Ok(s);
                    }
                }
                match r.default_message() {
                    Some(d) => Ok(Arc::from(d)),
                    None => Err(LeafError::new(ErrorKind::NoSuchMessage)),
                }
            })
        }
    }

    #[test]
    fn message_source_resolves_or_misses() {
        let ms: &dyn MessageSource = &ConstMessages;
        let hit = futures::executor::block_on(ms.message("greeting", &[], None)).unwrap();
        assert_eq!(&*hit, "hello");
        let miss = futures::executor::block_on(ms.message("absent", &[], None));
        assert_eq!(miss.unwrap_err().kind, ErrorKind::NoSuchMessage);
    }

    #[test]
    fn message_or_falls_back_to_default() {
        let ms: &dyn MessageSource = &ConstMessages;
        let s = futures::executor::block_on(ms.message_or("absent", &[], "fallback", None));
        assert_eq!(&*s, "fallback");
    }

    #[test]
    fn message_resolvable_resolves_via_codes_then_default() {
        struct V;
        impl MessageResolvable for V {
            fn codes(&self) -> &[&str] {
                &["absent", "greeting"]
            }
            fn arguments(&self) -> &[Arg<'_>] {
                &[]
            }
            fn default_message(&self) -> Option<&str> {
                Some("def")
            }
        }
        let ms: &dyn MessageSource = &ConstMessages;
        let s = futures::executor::block_on(ms.resolve(&V, None)).unwrap();
        assert_eq!(&*s, "hello", "second code resolves before default");
    }

    #[test]
    fn locale_carries_a_tag() {
        let l = Locale::new("en-US");
        assert_eq!(l.tag(), "en-US");
    }

    #[test]
    fn catalog_descriptor_is_const_constructible() {
        const CAT: CatalogDescriptor = CatalogDescriptor {
            contract: ContractId::of("app::Messages"),
            basename: "messages",
            locales: &["en", "de"],
        };
        assert_eq!(CAT.basename, "messages");
        assert_eq!(CAT.locales.len(), 2);
    }

    // ── resource loading ─────────────────────────────────────────────────────

    struct InMemResource {
        id: ResourceId,
        bytes: &'static [u8],
    }
    impl Resource for InMemResource {
        fn id(&self) -> &ResourceId {
            &self.id
        }
        fn exists(&self) -> Existence {
            Existence::Known(true)
        }
        fn last_modified(&self) -> Option<std::time::SystemTime> {
            None
        }
        fn open<'a>(&'a self) -> BoxFuture<'a, Result<Box<dyn ResourceReader>, LeafError>> {
            Box::pin(async { Err(LeafError::new(ErrorKind::ConfigIo)) })
        }
        fn read_to_bytes<'a>(&'a self) -> BoxFuture<'a, Result<Vec<u8>, LeafError>> {
            Box::pin(async move { Ok(self.bytes.to_vec()) })
        }
    }

    struct ClasspathLoader;
    impl ResourceLoader for ClasspathLoader {
        fn resolve(&self, loc: &Location) -> Result<Box<dyn Resource>, LeafError> {
            Ok(Box::new(InMemResource {
                id: ResourceId::new(loc.clone()),
                bytes: b"data",
            }))
        }
    }

    #[test]
    fn resource_loader_resolves_and_reads() {
        let loader: &dyn ResourceLoader = &ClasspathLoader;
        let loc = Location::classpath("messages.properties");
        let res = loader.resolve(&loc).unwrap();
        assert!(matches!(res.exists(), Existence::Known(true)));
        let bytes = futures::executor::block_on(res.read_to_bytes()).unwrap();
        assert_eq!(bytes, b"data");
        assert_eq!(res.id().location.scheme, Scheme::Classpath);
    }

    #[test]
    fn existence_is_tri_state() {
        assert!(matches!(Existence::Known(false), Existence::Known(false)));
        assert!(matches!(Existence::Unknown, Existence::Unknown));
    }

    #[test]
    fn resource_entry_is_const_constructible() {
        const E: ResourceEntry = ResourceEntry {
            logical_path: "config/app.yaml",
            bytes_fn: || b"port: 8080",
        };
        assert_eq!(E.logical_path, "config/app.yaml");
        assert_eq!((E.bytes_fn)(), b"port: 8080");
    }

    #[test]
    fn scheme_and_location_compose() {
        let loc = Location::new(Scheme::File, "/etc/app.conf");
        assert_eq!(loc.scheme, Scheme::File);
        assert_eq!(&*loc.path, "/etc/app.conf");
    }
}
