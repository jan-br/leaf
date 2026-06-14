//! The Binder tree-descent engine + `BindTarget`/`BindResult`/`BindHandler`.
//!
//! Realizes binding-conversion `binder` + the CPS adapter (`extra-7`): a derived
//! `const NodeSchema` schema + a hand-written recursive walker over the
//! binder-facing [`ConfigurationPropertySource`] (CPS) adapter, with the
//! [`BindHandler`] observer unified onto the resolve-handler shape.
//!
//! - [`ConfigurationPropertySource`] is the tri-state VIEW over the sealed stack:
//!   [`ConfigurationPropertyState`] is `Present`/`Absent` for an enumerable
//!   source and `Unknown` for a non-enumerable one (the `SourceCaps` enumerable
//!   bit flows straight through). [`StackCps`] is the stock adapter over an
//!   [`crate::env::Env`].
//! - [`BindTarget`] self-describes via a `const SCHEMA: &'static NodeSchema` and
//!   a cursor-calling `bind` fn; the binder descends one segment per object level
//!   building child [`CanonicalName`]s.
//! - [`BindResult`] is a THREE-state monad: `Bound`/`Unbound`/`Failed` — `Unbound`
//!   is NOT an error (distinct from `Failed`).
//! - Collection REPLACEMENT (one source owns the whole List/Map) is answered via
//!   [`ConfigurationPropertySource::contains_descendant_of`]: the highest-precedence
//!   source whose state is `Present` for any descendant owns the collection; it is
//!   never assembled across sources (never-blend).
//!
//! The erased [`ConversionService`] is the OPT-IN fallback for the genuinely
//! type-erased binder path; the default path is monomorphized [`FromConfigValue`].

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use crate::convert::{ConfigValue, ConvertCtx, FromConfigValue, Leniency};
use crate::env::{Env, PropertyValue};
use crate::error::{Cause, ErrorKind, LeafError};
use crate::relaxed::{CanonicalName, Segment};

/// The tri-state a [`ConfigurationPropertySource`] reports for a descendant
/// query (extra-7): an enumerable source answers with certainty; a
/// non-enumerable source answers [`Unknown`](ConfigurationPropertyState::Unknown).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigurationPropertyState {
    /// At least one descendant of the queried name is present.
    Present,
    /// No descendant is present (certain — enumerable source).
    Absent,
    /// The source cannot tell (non-enumerable; candidate-probe instead).
    Unknown,
}

/// The binder-facing relaxed/tri-state view over the sealed stack (extra-7).
///
/// One adapter wraps the WHOLE first-source-wins stack; `get` delegates key
/// identity to relaxed-binding's uniform fold, `contains_descendant_of` answers
/// the collection-owner query, and `iter` enumerates iff every consulted source
/// is enumerable.
pub trait ConfigurationPropertySource: Send + Sync {
    /// Relaxed point lookup for `name`.
    fn get(&self, name: &CanonicalName) -> Option<PropertyValue>;

    /// Whether any descendant of `name` is present (collection-owner query).
    fn contains_descendant_of(&self, name: &CanonicalName) -> ConfigurationPropertyState;

    /// Enumerate the canonical names this source can list, iff fully enumerable.
    fn iter(&self) -> Option<Box<dyn Iterator<Item = CanonicalName> + '_>>;
}

/// The stock CPS adapter over an [`Env`]'s sealed stack.
///
/// Reads are sync, lock-free, non-blocking over the immutable stack (every async
/// source was drained at seal). Enumeration is `Some` iff EVERY source in the
/// stack is enumerable; `contains_descendant_of` is `Unknown` if any consulted
/// source is non-enumerable and no enumerable source already answered `Present`.
pub struct StackCps {
    env: Env,
}

impl StackCps {
    /// Wrap an [`Env`].
    #[must_use]
    pub fn new(env: Env) -> Self {
        StackCps { env }
    }

    /// The wrapped environment.
    #[must_use]
    pub fn env(&self) -> &Env {
        &self.env
    }
}

impl ConfigurationPropertySource for StackCps {
    fn get(&self, name: &CanonicalName) -> Option<PropertyValue> {
        // The Env stack already applies the relaxed uniform fold per source.
        self.env.get_raw(&name.to_dotted())
    }

    fn contains_descendant_of(&self, name: &CanonicalName) -> ConfigurationPropertyState {
        let prefix = name.to_dotted();
        let mut saw_unknown = false;
        for src in self.env.core().stack.sources() {
            match src.keys() {
                Some(mut keys) => {
                    // Enumerable: a key equal to or under the prefix => Present.
                    if keys.any(|k| is_descendant_or_self(&k, &prefix)) {
                        return ConfigurationPropertyState::Present;
                    }
                }
                None => {
                    // Non-enumerable: we cannot be sure it has no descendant.
                    saw_unknown = true;
                }
            }
        }
        if saw_unknown {
            ConfigurationPropertyState::Unknown
        } else {
            ConfigurationPropertyState::Absent
        }
    }

    fn iter(&self) -> Option<Box<dyn Iterator<Item = CanonicalName> + '_>> {
        // Enumerable iff every source can enumerate.
        let sources = self.env.core().stack.sources();
        if sources.iter().any(|s| s.keys().is_none()) {
            return None;
        }
        let mut names: Vec<CanonicalName> = Vec::new();
        for src in sources {
            if let Some(keys) = src.keys() {
                for k in keys {
                    if let Ok(n) = CanonicalName::parse(&k) {
                        names.push(n);
                    }
                }
            }
        }
        Some(Box::new(names.into_iter()))
    }
}

/// Whether the dotted `key` is the prefix itself or a descendant of it.
fn is_descendant_or_self(key: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if key == prefix {
        return true;
    }
    // A descendant continues with `.` or `[`.
    key.strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
}

// ───────────────────────── the bind schema (NodeSchema) ─────────────────────

/// How a bindable object is constructed (binding-conversion `binder`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindMethod {
    /// `Default` + settable fields (the JavaBean convention leaf defines).
    JavaBean,
    /// A single non-injected constructor (records / value objects).
    ValueObject,
}

/// One field of an [`NodeSchema::Object`].
#[derive(Clone, Copy, Debug)]
pub struct Field {
    /// The field's canonical kebab name (derived from the snake_case ident).
    pub canonical: &'static str,
    /// The field's value schema (fn-pointer indirection handles recursion).
    pub schema: &'static NodeSchema,
    /// Whether the field has a default (so absence is `Unbound`, not an error).
    pub has_default: bool,
}

/// The const, derived bind schema for a [`BindTarget`].
///
/// Recursive types (`Vec<Self>`) are expressible because each variant holds a
/// `&'static NodeSchema` (a const fn-pointer-style indirection resolved at the
/// referenced `const`), never an inline owned schema.
#[derive(Clone, Copy, Debug)]
pub enum NodeSchema {
    /// A leaf scalar coerced via [`FromConfigValue`].
    Scalar,
    /// A nested object with a bind method and fields.
    Object {
        /// How the object is constructed.
        method: BindMethod,
        /// The object's fields.
        fields: &'static [Field],
    },
    /// A homogeneous list of the element schema.
    List(&'static NodeSchema),
    /// A string-keyed map of the value schema.
    Map(&'static NodeSchema),
    /// A homogeneous set of the element schema.
    Set(&'static NodeSchema),
}

/// The THREE-state bind monad (binding-conversion `binder`).
///
/// `Unbound` (nothing in the config for this target) is NOT an error — it is
/// distinct from `Failed` (a present-but-bad value). The binder collects every
/// `Failed` into the aggregated report rather than failing on the first.
#[derive(Debug)]
pub enum BindResult<T> {
    /// A value was bound from the config.
    Bound(T),
    /// Nothing in the config matched (use a default / leave the existing value).
    Unbound,
    /// A present value failed to bind/convert/validate.
    Failed(LeafError),
}

impl<T> BindResult<T> {
    /// `true` iff this is [`BindResult::Bound`].
    #[must_use]
    pub fn is_bound(&self) -> bool {
        matches!(self, BindResult::Bound(_))
    }

    /// `true` iff this is [`BindResult::Unbound`].
    #[must_use]
    pub fn is_unbound(&self) -> bool {
        matches!(self, BindResult::Unbound)
    }

    /// The bound value, if any.
    #[must_use]
    pub fn bound(self) -> Option<T> {
        match self {
            BindResult::Bound(v) => Some(v),
            _ => None,
        }
    }

    /// Convert to a `Result`, treating `Unbound` as the supplied default.
    ///
    /// # Errors
    /// Propagates a [`BindResult::Failed`] error.
    pub fn or_default(self, default: T) -> Result<T, LeafError> {
        match self {
            BindResult::Bound(v) => Ok(v),
            BindResult::Unbound => Ok(default),
            BindResult::Failed(e) => Err(e),
        }
    }
}

// ───────────────────────── BindHandler (observer) ──────────────────────────

/// Context handed to a [`BindHandler`] callback (the bound name + field path).
#[derive(Clone, Debug)]
pub struct BindCtx<'a> {
    /// The canonical name being bound at this node.
    pub name: &'a CanonicalName,
    /// The static field name (for an object field), if any.
    pub field: Option<&'static str>,
}

/// The bind observer (binding-conversion `binder`) — the SAME observer shape the
/// resolve handler uses. The stock `ValidationBindHandler` (a later unit) runs
/// `Validate::validate` after each Object node.
pub trait BindHandler {
    /// Called when a node bind starts.
    fn on_start(&self, _ctx: &BindCtx<'_>) {}
    /// Called when a node binds successfully.
    fn on_success(&self, _ctx: &BindCtx<'_>) {}
    /// Called when a node bind fails.
    fn on_failure(&self, _ctx: &BindCtx<'_>, _err: &LeafError) {}
    /// Called when a node bind finishes (success or failure).
    fn on_finish(&self, _ctx: &BindCtx<'_>) {}
}

/// A no-op [`BindHandler`] (the default when no observer is installed).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopBindHandler;
impl BindHandler for NoopBindHandler {}

// ───────────────────────── the erased ConversionService ─────────────────────

/// An erased converter for the opt-in [`ConversionService`] fallback.
pub trait Converter: Send + Sync {
    /// Convert `v` to the target type, returning an erased value.
    ///
    /// # Errors
    /// An [`ErrorKind::ConvertError`] [`LeafError`] on failure.
    fn convert(
        &self,
        v: &ConfigValue<'_>,
        cx: &ConvertCtx,
    ) -> Result<Box<dyn Any + Send>, LeafError>;

    /// The target [`TypeId`] this converter produces.
    fn target(&self) -> TypeId;
}

/// The frozen-after-seal erased conversion fallback (OPT-IN; the default path is
/// monomorphized [`FromConfigValue`]). Built once; reads are lock-free.
#[derive(Default)]
pub struct ConversionService {
    by_target: HashMap<TypeId, Arc<dyn Converter>>,
}

impl ConversionService {
    /// An empty service (the common case — the default path uses no registry).
    #[must_use]
    pub fn new() -> Self {
        ConversionService::default()
    }

    /// Register an erased converter (cold path; pre-seal).
    pub fn register(&mut self, conv: Arc<dyn Converter>) {
        self.by_target.insert(conv.target(), conv);
    }

    /// Whether a converter is registered for `target`.
    #[must_use]
    pub fn has(&self, target: TypeId) -> bool {
        self.by_target.contains_key(&target)
    }

    /// Convert via the erased path for `target`, if a converter is registered.
    ///
    /// # Errors
    /// Propagates the converter's [`LeafError`].
    pub fn convert(
        &self,
        target: TypeId,
        v: &ConfigValue<'_>,
        cx: &ConvertCtx,
    ) -> Option<Result<Box<dyn Any + Send>, LeafError>> {
        self.by_target.get(&target).map(|c| c.convert(v, cx))
    }
}

// ───────────────────────── the Binder + BindCursor ──────────────────────────

/// The recursive tree-descent binder (binding-conversion `binder`).
///
/// Holds the CPS adapter, the (opt-in) conversion service, the bind handler, and
/// the conversion policy. `bind::<T>` descends `T::SCHEMA` from a prefix; the
/// derived (or hand-written) `T::bind` calls the [`BindCursor`] helpers.
pub struct Binder<'s> {
    cps: &'s dyn ConfigurationPropertySource,
    #[allow(dead_code)]
    conv: &'s ConversionService,
    handler: &'s dyn BindHandler,
    policy: Leniency,
}

impl<'s> Binder<'s> {
    /// Build a binder over a CPS adapter, conversion service, and handler.
    #[must_use]
    pub fn new(
        cps: &'s dyn ConfigurationPropertySource,
        conv: &'s ConversionService,
        handler: &'s dyn BindHandler,
    ) -> Self {
        Binder {
            cps,
            conv,
            handler,
            policy: Leniency::Strict,
        }
    }

    /// Set the conversion policy (builder style).
    #[must_use]
    pub fn with_policy(mut self, policy: Leniency) -> Self {
        self.policy = policy;
        self
    }

    /// Bind a [`BindTarget`] rooted at `prefix`.
    #[must_use]
    pub fn bind<T: BindTarget>(&self, prefix: &CanonicalName) -> BindResult<T> {
        let ctx = BindCtx {
            name: prefix,
            field: None,
        };
        self.handler.on_start(&ctx);
        let mut cursor = BindCursor {
            binder: self,
            prefix: prefix.clone(),
        };
        let r = T::bind(&mut cursor);
        match &r {
            BindResult::Bound(_) => self.handler.on_success(&ctx),
            BindResult::Failed(e) => self.handler.on_failure(&ctx, e),
            BindResult::Unbound => {}
        }
        self.handler.on_finish(&ctx);
        r
    }

    /// Bind a [`BindTarget`] rooted at `prefix`, falling back to `T::default()`
    /// when nothing is bound.
    ///
    /// # Errors
    /// Propagates a [`BindResult::Failed`] error.
    pub fn bind_or_create<T: BindTarget + Default>(
        &self,
        prefix: &CanonicalName,
    ) -> Result<T, LeafError> {
        self.bind(prefix).or_default(T::default())
    }
}

/// The cursor a [`BindTarget::bind`] fn uses to read fields at the current
/// prefix (the seam the derive macro emits against).
pub struct BindCursor<'a, 'b: 'a> {
    binder: &'a Binder<'b>,
    prefix: CanonicalName,
}

impl<'a, 'b: 'a> BindCursor<'a, 'b> {
    /// The current prefix.
    #[must_use]
    pub fn prefix(&self) -> &CanonicalName {
        &self.prefix
    }

    /// Bind a SCALAR field `name` to `T` via [`FromConfigValue`].
    ///
    /// Returns `Bound`/`Unbound` (absent)/`Failed` (present-but-bad).
    #[must_use]
    pub fn scalar<T: FromConfigValue>(&self, name: &'static str) -> BindResult<T> {
        let child = self.prefix.child(Segment::Named(name.into()));
        let ctx = BindCtx {
            name: &child,
            field: Some(name),
        };
        self.binder.handler.on_start(&ctx);
        let result = match self.binder.cps.get(&child) {
            Some(pv) => {
                let cx = ConvertCtx {
                    policy: self.binder.policy,
                    unit: None,
                };
                let cv = ConfigValue::scalar(pv.raw.into_owned()).with_origin(pv.origin);
                match T::from_config_value(&cv, &cx) {
                    Ok(v) => BindResult::Bound(v),
                    Err(e) => BindResult::Failed(e),
                }
            }
            None => BindResult::Unbound,
        };
        match &result {
            BindResult::Bound(_) => self.binder.handler.on_success(&ctx),
            BindResult::Failed(e) => self.binder.handler.on_failure(&ctx, e),
            BindResult::Unbound => {}
        }
        self.binder.handler.on_finish(&ctx);
        result
    }

    /// Bind a NESTED object field `name` to a [`BindTarget`] `T`.
    #[must_use]
    pub fn nested<T: BindTarget>(&self, name: &'static str) -> BindResult<T> {
        let child = self.prefix.child(Segment::Named(name.into()));
        self.binder.bind::<T>(&child)
    }

    /// Bind a homogeneous LIST field `name` to `Vec<T>` via indexed-children
    /// descent (`name[0]`, `name[1]`, …) — collection REPLACEMENT applies: the
    /// elements all come from the first source that owns the collection.
    #[must_use]
    pub fn list<T: FromConfigValue>(&self, name: &'static str) -> BindResult<Vec<T>> {
        let base = self.prefix.child(Segment::Named(name.into()));
        // First, the inline comma-split scalar form (`name=a,b,c`).
        if let Some(pv) = self.binder.cps.get(&base) {
            let cx = ConvertCtx {
                policy: self.binder.policy,
                unit: None,
            };
            let cv = ConfigValue::scalar(pv.raw.into_owned()).with_origin(pv.origin);
            return match Vec::<T>::from_config_value(&cv, &cx) {
                Ok(v) => BindResult::Bound(v),
                Err(e) => BindResult::Failed(e),
            };
        }
        // Else, indexed-children descent.
        let mut out: Vec<T> = Vec::new();
        let mut i: u32 = 0;
        loop {
            let elem = base.child(Segment::Indexed(i));
            let Some(pv) = self.binder.cps.get(&elem) else {
                break;
            };
            let cx = ConvertCtx {
                policy: self.binder.policy,
                unit: None,
            };
            let cv = ConfigValue::scalar(pv.raw.into_owned()).with_origin(pv.origin);
            match T::from_config_value(&cv, &cx) {
                Ok(v) => out.push(v),
                Err(e) => return BindResult::Failed(e),
            }
            i += 1;
        }
        if out.is_empty() {
            BindResult::Unbound
        } else {
            BindResult::Bound(out)
        }
    }

    /// Bind a LIST of NESTED objects field `name` to `Vec<T>` via indexed-children
    /// descent (`name[0].host`, `name[1].host`, …).
    #[must_use]
    pub fn list_nested<T: BindTarget>(&self, name: &'static str) -> BindResult<Vec<T>> {
        let base = self.prefix.child(Segment::Named(name.into()));
        let mut out: Vec<T> = Vec::new();
        let mut i: u32 = 0;
        loop {
            let elem = base.child(Segment::Indexed(i));
            // Stop when the element's subtree is certainly absent.
            match self.binder.cps.contains_descendant_of(&elem) {
                ConfigurationPropertyState::Absent => break,
                _ => match self.binder.bind::<T>(&elem) {
                    BindResult::Bound(v) => out.push(v),
                    BindResult::Unbound => break,
                    BindResult::Failed(e) => return BindResult::Failed(e),
                },
            }
            i += 1;
        }
        if out.is_empty() {
            BindResult::Unbound
        } else {
            BindResult::Bound(out)
        }
    }
}

/// A self-describing bindable target (binding-conversion `binder`).
///
/// The derive macro emits the `const SCHEMA` + the cursor-calling `bind`; here
/// it is hand-implementable so the engine is testable.
pub trait BindTarget: Sized {
    /// The const, derived schema (recursion via `&'static NodeSchema`).
    const SCHEMA: &'static NodeSchema;

    /// Bind `Self` from the cursor at the current prefix.
    fn bind(cursor: &mut BindCursor<'_, '_>) -> BindResult<Self>;
}

/// Lift a `Cause` into a `BindError` node (the diagnostic shape for the binder).
#[must_use]
pub fn bind_error(prefix: &CanonicalName, detail: impl Into<String>) -> LeafError {
    LeafError::new(ErrorKind::BindError).caused_by(Cause::plain(
        "binding config target",
        format!("at `{prefix}`: {}", detail.into()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{EnvBuilder, MapPropertySource, RandomValueSource};
    use std::sync::Arc;

    fn cps_from(pairs: &[(&str, &str)]) -> StackCps {
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(MapPropertySource::from_pairs(
            "test",
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
        )));
        StackCps::new(b.seal_env())
    }

    // ── a flat JavaBean target ─────────────────────────────────────────────

    #[derive(Debug, Default, PartialEq)]
    struct ServerProps {
        port: u16,
        host: String,
    }

    static SERVER_FIELDS: &[Field] = &[
        Field {
            canonical: "port",
            schema: &NodeSchema::Scalar,
            has_default: true,
        },
        Field {
            canonical: "host",
            schema: &NodeSchema::Scalar,
            has_default: true,
        },
    ];
    static SERVER_SCHEMA: NodeSchema = NodeSchema::Object {
        method: BindMethod::JavaBean,
        fields: SERVER_FIELDS,
    };

    impl BindTarget for ServerProps {
        const SCHEMA: &'static NodeSchema = &SERVER_SCHEMA;
        fn bind(cursor: &mut BindCursor<'_, '_>) -> BindResult<Self> {
            let mut out = ServerProps::default();
            let mut any = false;
            match cursor.scalar::<u16>("port") {
                BindResult::Bound(v) => {
                    out.port = v;
                    any = true;
                }
                BindResult::Unbound => {}
                BindResult::Failed(e) => return BindResult::Failed(e),
            }
            match cursor.scalar::<String>("host") {
                BindResult::Bound(v) => {
                    out.host = v;
                    any = true;
                }
                BindResult::Unbound => {}
                BindResult::Failed(e) => return BindResult::Failed(e),
            }
            if any {
                BindResult::Bound(out)
            } else {
                BindResult::Unbound
            }
        }
    }

    #[test]
    fn binds_a_flat_javabean_target() {
        let cps = cps_from(&[("server.port", "8443"), ("server.host", "leaf.dev")]);
        let conv = ConversionService::new();
        let h = NoopBindHandler;
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("server").unwrap();
        let r = binder.bind::<ServerProps>(&prefix);
        assert_eq!(
            r.bound().unwrap(),
            ServerProps {
                port: 8443,
                host: "leaf.dev".into()
            }
        );
    }

    #[test]
    fn relaxed_keys_bind_via_uniform_fold() {
        // The binder reads through the relaxed fold: SERVER_PORT binds server.port.
        let cps = cps_from(&[("SERVER_PORT", "9000"), ("server.host", "h")]);
        let conv = ConversionService::new();
        let h = NoopBindHandler;
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("server").unwrap();
        let r = binder.bind::<ServerProps>(&prefix);
        assert_eq!(r.bound().unwrap().port, 9000);
    }

    #[test]
    fn absent_target_is_unbound_not_failed() {
        let cps = cps_from(&[("other.thing", "x")]);
        let conv = ConversionService::new();
        let h = NoopBindHandler;
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("server").unwrap();
        let r = binder.bind::<ServerProps>(&prefix);
        assert!(r.is_unbound());
        // bind_or_create falls back to Default.
        let v = binder.bind_or_create::<ServerProps>(&prefix).unwrap();
        assert_eq!(v, ServerProps::default());
    }

    #[test]
    fn present_but_bad_value_is_failed_with_convert_error() {
        let cps = cps_from(&[("server.port", "not-a-number")]);
        let conv = ConversionService::new();
        let h = NoopBindHandler;
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("server").unwrap();
        let r = binder.bind::<ServerProps>(&prefix);
        match r {
            BindResult::Failed(e) => assert_eq!(e.kind, ErrorKind::ConvertError),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ── a nested target (object descent) ───────────────────────────────────

    #[derive(Debug, Default, PartialEq)]
    struct AppProps {
        name: String,
        server: ServerProps,
    }

    static APP_FIELDS: &[Field] = &[
        Field {
            canonical: "name",
            schema: &NodeSchema::Scalar,
            has_default: true,
        },
        Field {
            canonical: "server",
            schema: &SERVER_SCHEMA,
            has_default: true,
        },
    ];
    static APP_SCHEMA: NodeSchema = NodeSchema::Object {
        method: BindMethod::JavaBean,
        fields: APP_FIELDS,
    };

    impl BindTarget for AppProps {
        const SCHEMA: &'static NodeSchema = &APP_SCHEMA;
        fn bind(cursor: &mut BindCursor<'_, '_>) -> BindResult<Self> {
            let mut out = AppProps::default();
            let mut any = false;
            if let BindResult::Bound(v) = cursor.scalar::<String>("name") {
                out.name = v;
                any = true;
            }
            match cursor.nested::<ServerProps>("server") {
                BindResult::Bound(v) => {
                    out.server = v;
                    any = true;
                }
                BindResult::Unbound => {}
                BindResult::Failed(e) => return BindResult::Failed(e),
            }
            if any {
                BindResult::Bound(out)
            } else {
                BindResult::Unbound
            }
        }
    }

    #[test]
    fn binds_nested_objects_via_descent() {
        let cps = cps_from(&[
            ("app.name", "leaf"),
            ("app.server.port", "8080"),
            ("app.server.host", "0.0.0.0"),
        ]);
        let conv = ConversionService::new();
        let h = NoopBindHandler;
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("app").unwrap();
        let r = binder.bind::<AppProps>(&prefix).bound().unwrap();
        assert_eq!(r.name, "leaf");
        assert_eq!(r.server.port, 8080);
        assert_eq!(r.server.host, "0.0.0.0");
    }

    // ── collection descent (list of scalars + list of nested) ──────────────

    #[derive(Debug, Default, PartialEq)]
    struct Pools {
        names: Vec<String>,
        servers: Vec<ServerProps>,
    }

    static POOLS_SCHEMA: NodeSchema = NodeSchema::Object {
        method: BindMethod::JavaBean,
        fields: &[],
    };

    impl BindTarget for Pools {
        const SCHEMA: &'static NodeSchema = &POOLS_SCHEMA;
        fn bind(cursor: &mut BindCursor<'_, '_>) -> BindResult<Self> {
            let mut out = Pools::default();
            let mut any = false;
            if let BindResult::Bound(v) = cursor.list::<String>("names") {
                out.names = v;
                any = true;
            }
            match cursor.list_nested::<ServerProps>("servers") {
                BindResult::Bound(v) => {
                    out.servers = v;
                    any = true;
                }
                BindResult::Unbound => {}
                BindResult::Failed(e) => return BindResult::Failed(e),
            }
            if any {
                BindResult::Bound(out)
            } else {
                BindResult::Unbound
            }
        }
    }

    #[test]
    fn binds_a_list_of_scalars_indexed_children() {
        let cps = cps_from(&[
            ("pools.names[0]", "a"),
            ("pools.names[1]", "b"),
            ("pools.names[2]", "c"),
        ]);
        let conv = ConversionService::new();
        let h = NoopBindHandler;
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("pools").unwrap();
        let r = binder.bind::<Pools>(&prefix).bound().unwrap();
        assert_eq!(r.names, vec!["a", "b", "c"]);
    }

    #[test]
    fn binds_a_list_of_scalars_inline_comma_form() {
        let cps = cps_from(&[("pools.names", "x,y,z")]);
        let conv = ConversionService::new();
        let h = NoopBindHandler;
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("pools").unwrap();
        let r = binder.bind::<Pools>(&prefix).bound().unwrap();
        assert_eq!(r.names, vec!["x", "y", "z"]);
    }

    #[test]
    fn binds_a_list_of_nested_objects() {
        let cps = cps_from(&[
            ("pools.servers[0].port", "1"),
            ("pools.servers[0].host", "h0"),
            ("pools.servers[1].port", "2"),
            ("pools.servers[1].host", "h1"),
        ]);
        let conv = ConversionService::new();
        let h = NoopBindHandler;
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("pools").unwrap();
        let r = binder.bind::<Pools>(&prefix).bound().unwrap();
        assert_eq!(r.servers.len(), 2);
        assert_eq!(r.servers[0].host, "h0");
        assert_eq!(r.servers[1].port, 2);
    }

    // ── CPS tri-state ──────────────────────────────────────────────────────

    #[test]
    fn cps_contains_descendant_of_is_present_absent_for_enumerable() {
        let cps = cps_from(&[("a.b.c", "1")]);
        let present = CanonicalName::parse("a.b").unwrap();
        assert_eq!(
            cps.contains_descendant_of(&present),
            ConfigurationPropertyState::Present
        );
        let absent = CanonicalName::parse("x.y").unwrap();
        assert_eq!(
            cps.contains_descendant_of(&absent),
            ConfigurationPropertyState::Absent
        );
    }

    #[test]
    fn cps_contains_descendant_of_is_unknown_with_a_non_enumerable_source() {
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(RandomValueSource::with_seed(1)));
        let cps = StackCps::new(b.seal_env());
        let name = CanonicalName::parse("anything").unwrap();
        // No enumerable source can prove Absent; the non-enumerable yields Unknown.
        assert_eq!(
            cps.contains_descendant_of(&name),
            ConfigurationPropertyState::Unknown
        );
        // And the stack is not enumerable.
        assert!(cps.iter().is_none());
    }

    // ── ConversionService (opt-in erased path) ─────────────────────────────

    struct U16Converter;
    impl Converter for U16Converter {
        fn convert(
            &self,
            v: &ConfigValue<'_>,
            cx: &ConvertCtx,
        ) -> Result<Box<dyn Any + Send>, LeafError> {
            u16::from_config_value(v, cx).map(|n| Box::new(n) as Box<dyn Any + Send>)
        }
        fn target(&self) -> TypeId {
            TypeId::of::<u16>()
        }
    }

    #[test]
    fn conversion_service_erased_fallback_round_trip() {
        let mut svc = ConversionService::new();
        svc.register(Arc::new(U16Converter));
        assert!(svc.has(TypeId::of::<u16>()));
        let cv = ConfigValue::scalar("7");
        let boxed = svc
            .convert(TypeId::of::<u16>(), &cv, &ConvertCtx::strict())
            .unwrap()
            .unwrap();
        let n = boxed.downcast::<u16>().unwrap();
        assert_eq!(*n, 7);
    }

    // ── BindHandler observer fires ─────────────────────────────────────────

    #[test]
    fn bind_handler_observes_success_and_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Default)]
        struct Counting {
            starts: AtomicUsize,
            successes: AtomicUsize,
            failures: AtomicUsize,
        }
        impl BindHandler for Counting {
            fn on_start(&self, _c: &BindCtx<'_>) {
                self.starts.fetch_add(1, Ordering::Relaxed);
            }
            fn on_success(&self, _c: &BindCtx<'_>) {
                self.successes.fetch_add(1, Ordering::Relaxed);
            }
            fn on_failure(&self, _c: &BindCtx<'_>, _e: &LeafError) {
                self.failures.fetch_add(1, Ordering::Relaxed);
            }
        }
        let cps = cps_from(&[("server.port", "bad")]);
        let conv = ConversionService::new();
        let h = Counting::default();
        let binder = Binder::new(&cps, &conv, &h);
        let prefix = CanonicalName::parse("server").unwrap();
        let _ = binder.bind::<ServerProps>(&prefix);
        assert!(h.starts.load(Ordering::Relaxed) >= 1);
        assert!(h.failures.load(Ordering::Relaxed) >= 1);
    }
}
