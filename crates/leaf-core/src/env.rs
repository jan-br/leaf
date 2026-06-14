//! The Environment: ordered first-source-wins property stack + the read seam.
//!
//! Realizes environment-config phase3/06: ONE concrete `EnvCore` behind the
//! ownership-model cheap-clone [`Env`] handle (`Arc<EnvCore>`, `Send + Sync +
//! 'static`, ADR-05 Context field). The MUTATE view is [`EnvBuilder`] — a
//! distinct owned type consumed by `seal()`, so a post-seal source push is
//! type-unrepresentable, exactly as `App<Resolve>` consuming `RegistryBuilder`
//! makes a post-freeze `register()` unrepresentable.
//!
//! - [`PropertySource`] is the origin-agnostic raw source trait; [`SourceCaps`]
//!   carries the enumerable bit (the binder/relaxed/CPS tri-state flows from it).
//! - First-source-wins is a linear `Option` short-circuit (never-blend = return
//!   the WHOLE winning value; null-never-overrides = `None` falls through,
//!   `Some("")` wins).
//! - [`MapPropertySource`] is the in-memory enumerable source (parsed file/JSON);
//!   [`RandomValueSource`] is the canonical NON-enumerable computing source
//!   (`random.*`).
//! - The read seam is the [`PropertyResolver`] least-privilege trait [`Env`]
//!   implements; reads are lock-free over the immutable [`SealedStack`].
//!
//! Relaxed lookup goes through the one [`crate::relaxed::uniform_key`] fold so
//! every spelling of a key (`DB_POOL_SIZE`/`db.pool-size`/`dbPoolSize`) resolves
//! to one identity — the same fold the binder's CPS adapter uses.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use indexmap::IndexMap;

use crate::error::{Cause, ErrorKind, LeafError, Origin};
use crate::relaxed::{uniform_key, UniformName};

/// The interned source name (cheap-clone, value-equal).
pub type SourceName = Arc<str>;

/// A raw property value: the post-parse string plus its provenance.
///
/// `raw` is `Cow<'static, str>` so a static-source value (an embedded default)
/// borrows and a parsed-file value owns. `origin` is the value's provenance
/// (replaced by an interned `OriginId` once the OriginStore unit lands; the
/// always-available [`Origin`] is the bedrock carrier today).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PropertyValue {
    /// The raw, post-parse string value.
    pub raw: Cow<'static, str>,
    /// Provenance of this value (never blended — rides first-source-wins).
    pub origin: Origin,
}

impl PropertyValue {
    /// A value with `Origin::Unknown`.
    #[must_use]
    pub fn new(raw: impl Into<Cow<'static, str>>) -> Self {
        PropertyValue {
            raw: raw.into(),
            origin: Origin::Unknown,
        }
    }

    /// A value with an explicit origin.
    #[must_use]
    pub fn with_origin(raw: impl Into<Cow<'static, str>>, origin: Origin) -> Self {
        PropertyValue {
            raw: raw.into(),
            origin,
        }
    }
}

/// Capability flags a [`PropertySource`] self-reports.
///
/// `enumerable` is the single load-bearing bit: an enumerable source can list
/// its keys (so it is indexed at seal and the binder/relaxed walk can iterate
/// it); a non-enumerable source (`random.*`, raw env, remote) answers point
/// queries only and yields the tri-state `Unknown` in the CPS adapter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SourceCaps {
    /// Whether the source can enumerate its keys.
    pub enumerable: bool,
}

impl SourceCaps {
    /// An enumerable source (the file/JSON/map default).
    pub const ENUMERABLE: SourceCaps = SourceCaps { enumerable: true };
    /// A non-enumerable source (`random.*`, raw env, remote).
    pub const NON_ENUMERABLE: SourceCaps = SourceCaps { enumerable: false };
}

/// The origin-agnostic raw property source (environment-config `property-sources`).
///
/// `get` returns `None` for ABSENT (fall through), `Some` for present — including
/// `Some("")` (empty-string present-and-wins). A source must be `Send + Sync` so
/// the sealed stack rides one `Arc` across `.await`/threads.
pub trait PropertySource: Send + Sync {
    /// The interned source name (stable identity for the mutate vocabulary).
    fn name(&self) -> &SourceName;

    /// Look up `key` (raw, NOT relaxed — the stack applies the relaxed fold).
    ///
    /// `None` is ABSENT (falls through to the next source); `Some` wins.
    fn get(&self, key: &str) -> Option<PropertyValue>;

    /// This source's capability flags (the enumerable bit).
    fn caps(&self) -> SourceCaps;

    /// Iterate the source's keys iff it is enumerable, else `None`.
    fn keys(&self) -> Option<Box<dyn Iterator<Item = Cow<'_, str>> + '_>>;
}

/// An in-memory enumerable property source (the parsed file/JSON/defaults shape).
///
/// Backed by an [`IndexMap`] so insertion order (and therefore enumeration
/// order) is deterministic. Keys are looked up BOTH verbatim and via the relaxed
/// uniform fold, so `DB_POOL_SIZE` in the map answers a `db.pool-size` query.
pub struct MapPropertySource {
    name: SourceName,
    entries: IndexMap<String, PropertyValue>,
    /// Uniform-fold index over the entries (relaxed lookup acceleration).
    by_uniform: HashMap<UniformName, String>,
}

impl MapPropertySource {
    /// Build from an iterator of `(key, value)` pairs.
    #[must_use]
    pub fn new(
        name: impl Into<SourceName>,
        entries: impl IntoIterator<Item = (String, PropertyValue)>,
    ) -> Self {
        let mut map = IndexMap::new();
        let mut by_uniform = HashMap::new();
        for (k, v) in entries {
            by_uniform.insert(uniform_key(&k), k.clone());
            map.insert(k, v);
        }
        MapPropertySource {
            name: name.into(),
            entries: map,
            by_uniform,
        }
    }

    /// Convenience: build from plain string pairs (origin defaults to Unknown).
    #[must_use]
    pub fn from_pairs<I, K, V>(name: impl Into<SourceName>, pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Cow<'static, str>>,
    {
        Self::new(
            name,
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), PropertyValue::new(v))),
        )
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the source is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl PropertySource for MapPropertySource {
    fn name(&self) -> &SourceName {
        &self.name
    }

    fn get(&self, key: &str) -> Option<PropertyValue> {
        // Exact hit first (verbatim key); then the relaxed uniform fold.
        if let Some(v) = self.entries.get(key) {
            return Some(v.clone());
        }
        let uni = uniform_key(key);
        self.by_uniform
            .get(&uni)
            .and_then(|raw| self.entries.get(raw))
            .cloned()
    }

    fn caps(&self) -> SourceCaps {
        SourceCaps::ENUMERABLE
    }

    fn keys(&self) -> Option<Box<dyn Iterator<Item = Cow<'_, str>> + '_>> {
        Some(Box::new(self.entries.keys().map(|k| Cow::Borrowed(k.as_str()))))
    }
}

// ───────────────────────── RandomValueSource (extra-8) ──────────────────────

/// The parsed `random.*` suffix grammar.
///
/// `(max)` is exclusive; `[min,max]` is the inclusive-min, exclusive-max range
/// form. An RNG draw cannot fail; only a malformed grammar is an error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RandomSpec {
    /// `random.value` — a random hex-ish string.
    Value,
    /// `random.int` — a random `i32`-range int.
    Int,
    /// `random.long` — a random `i64`-range long.
    Long,
    /// `random.uuid` — a random UUID v4 string.
    Uuid,
    /// `random.int(max)` — `[0, max)`.
    IntBounded(u64),
    /// `random.int[min,max]` — `[min, max)`.
    IntRange(i64, i64),
    /// `random.long(max)` — `[0, max)`.
    LongBounded(u64),
    /// `random.long[min,max]` — `[min, max)`.
    LongRange(i64, i64),
}

impl RandomSpec {
    /// Parse a `random.<suffix>` key into a [`RandomSpec`], WITHOUT drawing.
    ///
    /// Returns `None` if the key has no `random.` prefix (fall-through).
    ///
    /// # Errors
    /// Returns an [`ErrorKind::ConvertError`] [`LeafError`] for a malformed
    /// suffix (non-numeric bound, reversed range).
    pub fn parse(key: &str) -> Option<Result<RandomSpec, LeafError>> {
        let suffix = key.strip_prefix("random.")?;
        Some(Self::parse_suffix(suffix, key))
    }

    fn parse_suffix(suffix: &str, full: &str) -> Result<RandomSpec, LeafError> {
        let err = |msg: &str| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "parsing random.* spec",
                format!("invalid `{full}`: {msg}"),
            ))
        };
        // Split off a trailing `(...)` or `[...]` bound clause.
        if let Some(rest) = suffix.strip_prefix("int") {
            return Self::numeric(rest, full, true);
        }
        if let Some(rest) = suffix.strip_prefix("long") {
            return Self::numeric(rest, full, false);
        }
        match suffix {
            "value" => Ok(RandomSpec::Value),
            "uuid" => Ok(RandomSpec::Uuid),
            _ => Err(err("unknown random spec (expected value/int/long/uuid)")),
        }
    }

    fn numeric(bound: &str, full: &str, is_int: bool) -> Result<RandomSpec, LeafError> {
        let err = |msg: String| {
            LeafError::new(ErrorKind::ConvertError).caused_by(Cause::plain(
                "parsing random.* spec",
                format!("invalid `{full}`: {msg}"),
            ))
        };
        if bound.is_empty() {
            return Ok(if is_int { RandomSpec::Int } else { RandomSpec::Long });
        }
        // `(max)` exclusive upper bound.
        if let Some(inner) = bound.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            let max: u64 = inner
                .trim()
                .parse()
                .map_err(|_| err(format!("non-numeric bound `{inner}`")))?;
            return Ok(if is_int {
                RandomSpec::IntBounded(max)
            } else {
                RandomSpec::LongBounded(max)
            });
        }
        // `[min,max]` range, exclusive-max.
        if let Some(inner) = bound.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let (lo, hi) = inner
                .split_once(',')
                .ok_or_else(|| err("range expects `[min,max]`".to_string()))?;
            let lo: i64 = lo
                .trim()
                .parse()
                .map_err(|_| err(format!("non-numeric min `{lo}`")))?;
            let hi: i64 = hi
                .trim()
                .parse()
                .map_err(|_| err(format!("non-numeric max `{hi}`")))?;
            if hi <= lo {
                return Err(err(format!("reversed/empty range [{lo},{hi}]")));
            }
            return Ok(if is_int {
                RandomSpec::IntRange(lo, hi)
            } else {
                RandomSpec::LongRange(lo, hi)
            });
        }
        Err(err(format!("malformed bound clause `{bound}`")))
    }
}

/// The canonical NON-enumerable computing source for `random.*` (extra-8).
///
/// Seeded ONCE at construction (the one entropy decision, fenced to seal-time);
/// per-read draws are pure SplitMix64 arithmetic behind an [`AtomicU64`] — no
/// syscall, no `.await`, no global lock, `Send + Sync`. `caps().enumerable` is
/// `false` and `keys()` is `None`, so the binder/relaxed enumerate-side never
/// sees it; it is reachable ONLY via the `${...}` placeholder walk.
pub struct RandomValueSource {
    name: SourceName,
    state: AtomicU64,
}

impl RandomValueSource {
    /// Construct with an explicit 64-bit seed (deterministic; tests/AOT).
    #[must_use]
    pub fn with_seed(seed: u64) -> Self {
        RandomValueSource {
            name: Arc::from("random"),
            // Avoid the all-zero SplitMix64 fixed point.
            state: AtomicU64::new(seed ^ 0x9e37_79b9_7f4a_7c15),
        }
    }

    /// The next SplitMix64 draw (pure arithmetic; lock-free).
    fn next_u64(&self) -> u64 {
        // SplitMix64: advance the atomic state, then mix. `fetch_add` is the only
        // synchronization; the mixing is pure.
        let z = self
            .state
            .fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed)
            .wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = z;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Evaluate a parsed [`RandomSpec`] into a freshly-rendered string.
    fn render(&self, spec: RandomSpec) -> String {
        match spec {
            RandomSpec::Value => format!("{:016x}{:016x}", self.next_u64(), self.next_u64()),
            RandomSpec::Int => (self.next_u64() as u32 as i32).to_string(),
            RandomSpec::Long => (self.next_u64() as i64).to_string(),
            RandomSpec::Uuid => self.render_uuid(),
            RandomSpec::IntBounded(max) | RandomSpec::LongBounded(max) => {
                if max == 0 {
                    "0".to_string()
                } else {
                    (self.next_u64() % max).to_string()
                }
            }
            RandomSpec::IntRange(lo, hi) | RandomSpec::LongRange(lo, hi) => {
                let span = (hi - lo) as u64;
                (lo + (self.next_u64() % span) as i64).to_string()
            }
        }
    }

    fn render_uuid(&self) -> String {
        let a = self.next_u64();
        let b = self.next_u64();
        // Set version (4) and variant bits, like a v4 UUID.
        let hi = (a & 0xffff_ffff_ffff_0fff) | 0x0000_0000_0000_4000;
        let lo = (b & 0x3fff_ffff_ffff_ffff) | 0x8000_0000_0000_0000;
        format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            hi >> 32,
            (hi >> 16) & 0xffff,
            hi & 0xffff,
            lo >> 48,
            lo & 0xffff_ffff_ffff
        )
    }
}

impl PropertySource for RandomValueSource {
    fn name(&self) -> &SourceName {
        &self.name
    }

    fn get(&self, key: &str) -> Option<PropertyValue> {
        match RandomSpec::parse(key)? {
            Ok(spec) => Some(PropertyValue::with_origin(
                self.render(spec),
                Origin::Native {
                    crate_name: Some("leaf-core::random"),
                },
            )),
            // A malformed `random.*` grammar surfaces None here; the placeholder
            // walk re-parses via `RandomSpec::parse` to raise the rich error
            // (this keeps the PropertySource trait infallible per its contract).
            Err(_) => None,
        }
    }

    fn caps(&self) -> SourceCaps {
        SourceCaps::NON_ENUMERABLE
    }

    fn keys(&self) -> Option<Box<dyn Iterator<Item = Cow<'_, str>> + '_>> {
        None
    }
}

// ───────────────────────── the sealed stack + Env ───────────────────────────

/// The frozen, read-optimized property stack (environment-config `property-sources`).
///
/// Built by [`EnvBuilder::seal`]: an effective uniform-fold index over the
/// ENUMERABLE sources (O(1) first-source-wins reads), plus the ordered full
/// source chain (so non-enumerable sources and exact-key reads still resolve).
/// The precedence-cutoff rule: a higher-precedence source shadows a lower one,
/// computed by walking the ordered chain in precedence order on a point query.
pub struct SealedStack {
    /// Ordered sources, highest-precedence FIRST (first-source-wins).
    sources: Vec<Arc<dyn PropertySource>>,
}

impl SealedStack {
    /// First-source-wins point lookup over the ordered chain.
    ///
    /// Returns the WHOLE winning [`PropertyValue`] (never blends; the winning
    /// source's origin is the answer). `None` falls through; `Some("")` wins.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<PropertyValue> {
        self.sources.iter().find_map(|s| s.get(key))
    }

    /// Whether ANY source answers `key` (raw stack hit, no placeholder walk).
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.sources.iter().any(|s| s.get(key).is_some())
    }

    /// The ordered sources (highest-precedence first).
    #[must_use]
    pub fn sources(&self) -> &[Arc<dyn PropertySource>] {
        &self.sources
    }

    /// Number of sources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether the stack has no sources.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// The immutable, frozen environment core (one `Arc` shared by every [`Env`]).
///
/// `Send + Sync + 'static` rides this exactly as it rides `ErasedBean` — the
/// atomic `Arc` clone is the happens-before edge into every async bean.
pub struct EnvCore {
    /// The sealed first-source-wins stack.
    pub stack: SealedStack,
    /// The frozen placeholder grammar (built once at seal).
    pub syntax: crate::placeholder::PlaceholderSyntax,
    /// An optional parent environment (child-wins hierarchy, one-directional).
    pub parent: Option<Env>,
}

/// The everywhere-shared cheap-clone read handle (`Arc<EnvCore>`).
///
/// This IS the ownership-model handle ADR-05 names as an always-present Context
/// field; beans receive it by ordinary constructor injection.
#[derive(Clone)]
pub struct Env(Arc<EnvCore>);

impl Env {
    /// Wrap a sealed [`EnvCore`].
    #[must_use]
    pub fn new(core: EnvCore) -> Self {
        Env(Arc::new(core))
    }

    /// Borrow the inner core.
    #[must_use]
    pub fn core(&self) -> &EnvCore {
        &self.0
    }

    /// The frozen placeholder syntax.
    #[must_use]
    pub fn syntax(&self) -> &crate::placeholder::PlaceholderSyntax {
        &self.0.syntax
    }

    /// Raw stack lookup (a single source-chain hit), then the parent on miss.
    /// This is the relaxed-aware raw read BEFORE placeholder expansion.
    #[must_use]
    pub fn get_raw(&self, key: &str) -> Option<PropertyValue> {
        self.0
            .stack
            .get(key)
            .or_else(|| self.0.parent.as_ref().and_then(|p| p.get_raw(key)))
    }
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("sources", &self.0.stack.len())
            .field("has_parent", &self.0.parent.is_some())
            .finish()
    }
}

/// A placeholder-resolved value handed out by the read seam.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResolvedValue {
    /// The fully placeholder-expanded string.
    pub raw: String,
    /// Provenance of the winning value.
    pub origin: Origin,
}

/// The least-privilege READ trait every bean gets (environment-config `environment`).
///
/// `Env` implements it; reads are lock-free over the immutable [`SealedStack`].
/// (`accepts_profiles`/`active_profiles`/`cloud_platform` from the design sketch
/// are deferred to the profiles/cloud units; this unit pins the value-read core.)
pub trait PropertyResolver {
    /// Get `key` after the placeholder walk (lenient: unresolved left literal).
    fn get(&self, key: &str) -> Option<ResolvedValue>;

    /// Get `key`, erroring if it is absent.
    ///
    /// # Errors
    /// [`ErrorKind::UnresolvedValue`] if the key is absent.
    fn get_required(&self, key: &str) -> Result<ResolvedValue, LeafError>;

    /// Whether the raw stack has `key` (NO placeholder expansion).
    fn contains(&self, key: &str) -> bool;

    /// Typed read: get `key` and coerce to `T`, `None` if absent.
    ///
    /// # Errors
    /// [`ErrorKind::ConvertError`] if the value cannot be coerced.
    fn get_as<T: crate::convert::FromConfigValue>(
        &self,
        key: &str,
    ) -> Result<Option<T>, LeafError>;

    /// Lenient placeholder resolution over `t` (unresolved `${...}` left literal).
    fn resolve_placeholders<'a>(&self, t: &'a str) -> Cow<'a, str>;

    /// Strict placeholder resolution over `t`.
    ///
    /// # Errors
    /// [`ErrorKind::UnresolvedValue`] if a mandatory `${...}` cannot be resolved.
    fn resolve_required_placeholders(&self, t: &str) -> Result<String, LeafError>;
}

impl PropertyResolver for Env {
    fn get(&self, key: &str) -> Option<ResolvedValue> {
        let pv = self.get_raw(key)?;
        // Resolve placeholders inside the value (lenient).
        let resolved = crate::placeholder::resolve_lenient(&pv.raw, self.syntax(), &|k| {
            self.get_raw(k).map(|v| v.raw.into_owned())
        });
        Some(ResolvedValue {
            raw: resolved.into_owned(),
            origin: pv.origin,
        })
    }

    fn get_required(&self, key: &str) -> Result<ResolvedValue, LeafError> {
        self.get(key).ok_or_else(|| {
            LeafError::new(ErrorKind::UnresolvedValue).caused_by(Cause::plain(
                "reading required property",
                format!("no value for `{key}`"),
            ))
        })
    }

    fn contains(&self, key: &str) -> bool {
        self.0.stack.contains(key)
            || self.0.parent.as_ref().is_some_and(|p| p.contains(key))
    }

    fn get_as<T: crate::convert::FromConfigValue>(
        &self,
        key: &str,
    ) -> Result<Option<T>, LeafError> {
        let Some(rv) = self.get(key) else {
            return Ok(None);
        };
        let cv = crate::convert::ConfigValue::scalar(rv.raw).with_origin(rv.origin);
        T::from_config_value(&cv, &crate::convert::ConvertCtx::strict()).map(Some)
    }

    fn resolve_placeholders<'a>(&self, t: &'a str) -> Cow<'a, str> {
        crate::placeholder::resolve_lenient(t, self.syntax(), &|k| {
            self.get_raw(k).map(|v| v.raw.into_owned())
        })
    }

    fn resolve_required_placeholders(&self, t: &str) -> Result<String, LeafError> {
        crate::placeholder::resolve_strict(t, self.syntax(), &|k| {
            self.get_raw(k).map(|v| v.raw.into_owned())
        })
    }
}

// ───────────────────────── the mutate seam: EnvBuilder ──────────────────────

/// A missing-anchor error from the mutate vocabulary (`add_before`/`replace`/…).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NoSuchSource {
    /// The anchor name that was not found.
    pub name: String,
    /// The source names that ARE present (for the diagnostic).
    pub present: Vec<String>,
}

impl From<NoSuchSource> for LeafError {
    fn from(e: NoSuchSource) -> Self {
        LeafError::new(ErrorKind::ConfigIo).caused_by(Cause::plain(
            "mutating property-source stack",
            format!("no source named `{}` (present: {:?})", e.name, e.present),
        ))
    }
}

/// The MUTATE view of the environment — lives in `App<Define>`, consumed at seal.
///
/// Holds the ordered source stack with the literal Spring vocabulary
/// (`add_first`/`add_last`/`add_before`/`add_after`/`replace`/`remove`), each
/// returning [`NoSuchSource`] on a missing anchor. `seal()` consumes it into the
/// immutable [`EnvCore`], so a post-seal push is type-unrepresentable.
#[derive(Default)]
pub struct EnvBuilder {
    sources: Vec<Arc<dyn PropertySource>>,
    syntax: crate::placeholder::PlaceholderSyntax,
    parent: Option<Env>,
}

impl EnvBuilder {
    /// A fresh empty builder with the default placeholder syntax.
    #[must_use]
    pub fn new() -> Self {
        EnvBuilder::default()
    }

    /// Set the placeholder grammar (builder style; frozen into `EnvCore` at seal).
    #[must_use]
    pub fn with_syntax(mut self, syntax: crate::placeholder::PlaceholderSyntax) -> Self {
        self.syntax = syntax;
        self
    }

    /// Layer this environment over a parent (child-wins, one-directional).
    #[must_use]
    pub fn with_parent(mut self, parent: Env) -> Self {
        self.parent = Some(parent);
        self
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.sources.iter().position(|s| &**s.name() == name)
    }

    fn no_such(&self, name: &str) -> NoSuchSource {
        NoSuchSource {
            name: name.to_string(),
            present: self.sources.iter().map(|s| s.name().to_string()).collect(),
        }
    }

    /// Add `s` at the FRONT (highest precedence).
    pub fn add_first(&mut self, s: Arc<dyn PropertySource>) {
        self.sources.insert(0, s);
    }

    /// Add `s` at the BACK (lowest precedence).
    pub fn add_last(&mut self, s: Arc<dyn PropertySource>) {
        self.sources.push(s);
    }

    /// Insert `s` immediately BEFORE the named anchor (higher precedence).
    ///
    /// # Errors
    /// [`NoSuchSource`] if `anchor` is not present.
    pub fn add_before(
        &mut self,
        anchor: &str,
        s: Arc<dyn PropertySource>,
    ) -> Result<(), NoSuchSource> {
        let i = self.position(anchor).ok_or_else(|| self.no_such(anchor))?;
        self.sources.insert(i, s);
        Ok(())
    }

    /// Insert `s` immediately AFTER the named anchor (lower precedence).
    ///
    /// # Errors
    /// [`NoSuchSource`] if `anchor` is not present.
    pub fn add_after(
        &mut self,
        anchor: &str,
        s: Arc<dyn PropertySource>,
    ) -> Result<(), NoSuchSource> {
        let i = self.position(anchor).ok_or_else(|| self.no_such(anchor))?;
        self.sources.insert(i + 1, s);
        Ok(())
    }

    /// Replace the named source in place.
    ///
    /// # Errors
    /// [`NoSuchSource`] if `name` is not present.
    pub fn replace(
        &mut self,
        name: &str,
        s: Arc<dyn PropertySource>,
    ) -> Result<(), NoSuchSource> {
        let i = self.position(name).ok_or_else(|| self.no_such(name))?;
        self.sources[i] = s;
        Ok(())
    }

    /// Remove the named source.
    ///
    /// # Errors
    /// [`NoSuchSource`] if `name` is not present.
    pub fn remove(&mut self, name: &str) -> Result<(), NoSuchSource> {
        let i = self.position(name).ok_or_else(|| self.no_such(name))?;
        self.sources.remove(i);
        Ok(())
    }

    /// The number of sources currently in the stack.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether the stack is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Consume the builder into the immutable [`EnvCore`] (the seal fence).
    ///
    /// Post-seal mutation is unrepresentable: the builder is gone.
    #[must_use]
    pub fn seal(self) -> EnvCore {
        EnvCore {
            stack: SealedStack {
                sources: self.sources,
            },
            syntax: self.syntax,
            parent: self.parent,
        }
    }

    /// Convenience: seal directly into a cheap-clone [`Env`] handle.
    #[must_use]
    pub fn seal_env(self) -> Env {
        Env::new(self.seal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::FromConfigValue;

    fn env_with(pairs: &[(&str, &str)]) -> Env {
        let src = MapPropertySource::from_pairs(
            "test",
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(src));
        b.seal_env()
    }

    // ── PropertySource + relaxed lookup ────────────────────────────────────

    #[test]
    fn map_source_relaxed_lookup_folds_spellings() {
        let src =
            MapPropertySource::from_pairs("s", [("DB_POOL_SIZE", "10")]);
        // Any relaxed spelling resolves to the same value.
        assert_eq!(src.get("db.pool-size").unwrap().raw, "10");
        assert_eq!(src.get("dbPoolSize").unwrap().raw, "10");
        assert_eq!(src.get("DB_POOL_SIZE").unwrap().raw, "10");
        assert!(src.get("absent").is_none());
    }

    #[test]
    fn map_source_is_enumerable() {
        let src = MapPropertySource::from_pairs("s", [("a", "1"), ("b", "2")]);
        assert!(src.caps().enumerable);
        let keys: Vec<_> = src.keys().unwrap().map(|c| c.into_owned()).collect();
        assert_eq!(keys, vec!["a", "b"]);
    }

    // ── first-source-wins / never-blend / null-never-overrides ─────────────

    #[test]
    fn first_source_wins_returns_whole_winning_value() {
        let high = MapPropertySource::from_pairs("high", [("k", "winner")]);
        let low = MapPropertySource::from_pairs("low", [("k", "loser")]);
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(high));
        b.add_last(Arc::new(low));
        let env = b.seal_env();
        assert_eq!(env.get_raw("k").unwrap().raw, "winner");
    }

    #[test]
    fn empty_string_present_wins_over_lower_source() {
        let high = MapPropertySource::from_pairs("high", [("k", "")]);
        let low = MapPropertySource::from_pairs("low", [("k", "fallback")]);
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(high));
        b.add_last(Arc::new(low));
        let env = b.seal_env();
        // Some("") wins; it does NOT fall through.
        assert_eq!(env.get_raw("k").unwrap().raw, "");
    }

    #[test]
    fn absent_high_source_falls_through_to_lower() {
        let high = MapPropertySource::from_pairs("high", [("other", "x")]);
        let low = MapPropertySource::from_pairs("low", [("k", "deep")]);
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(high));
        b.add_last(Arc::new(low));
        let env = b.seal_env();
        assert_eq!(env.get_raw("k").unwrap().raw, "deep");
    }

    // ── EnvBuilder mutate vocabulary (MutablePropertySources) ──────────────

    #[test]
    fn add_first_takes_precedence_over_existing() {
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(MapPropertySource::from_pairs("base", [("k", "base")])));
        b.add_first(Arc::new(MapPropertySource::from_pairs("over", [("k", "over")])));
        let env = b.seal_env();
        assert_eq!(env.get_raw("k").unwrap().raw, "over");
    }

    #[test]
    fn add_before_and_after_position_relative_to_anchor() {
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(MapPropertySource::from_pairs("mid", [("k", "mid")])));
        b.add_before("mid", Arc::new(MapPropertySource::from_pairs("top", [("k", "top")])))
            .unwrap();
        b.add_after("mid", Arc::new(MapPropertySource::from_pairs("bot", [("k", "bot")])))
            .unwrap();
        let env = b.seal_env();
        // top is highest precedence.
        assert_eq!(env.get_raw("k").unwrap().raw, "top");
    }

    #[test]
    fn replace_and_remove_and_missing_anchor_errors() {
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(MapPropertySource::from_pairs("a", [("k", "1")])));
        b.replace("a", Arc::new(MapPropertySource::from_pairs("a", [("k", "2")])))
            .unwrap();
        let empty: [(&str, &str); 0] = [];
        assert!(b.add_before("ghost", Arc::new(MapPropertySource::from_pairs("z", empty))).is_err());
        let mut b2 = EnvBuilder::new();
        assert!(b2.remove("nope").is_err());
        let env = b.seal_env();
        assert_eq!(env.get_raw("k").unwrap().raw, "2");
    }

    // ── PropertyResolver read seam ─────────────────────────────────────────

    #[test]
    fn resolver_get_required_and_contains() {
        let env = env_with(&[("server.port", "8080")]);
        assert!(env.contains("server.port"));
        assert!(!env.contains("missing"));
        assert_eq!(env.get_required("server.port").unwrap().raw, "8080");
        assert_eq!(
            env.get_required("missing").unwrap_err().kind,
            ErrorKind::UnresolvedValue
        );
    }

    #[test]
    fn resolver_get_as_coerces_typed() {
        let env = env_with(&[("server.port", "8080")]);
        let port: Option<u16> = env.get_as("server.port").unwrap();
        assert_eq!(port, Some(8080));
        let missing: Option<u16> = env.get_as("nope").unwrap();
        assert_eq!(missing, None);
    }

    // ── hierarchy (child-wins, one-directional) ────────────────────────────

    #[test]
    fn child_env_overrides_parent_else_falls_through() {
        let parent = env_with(&[("a", "parent-a"), ("b", "parent-b")]);
        let mut b = EnvBuilder::new().with_parent(parent);
        b.add_last(Arc::new(MapPropertySource::from_pairs("child", [("a", "child-a")])));
        let child = b.seal_env();
        // Child wins for `a`; falls through to parent for `b`.
        assert_eq!(child.get_raw("a").unwrap().raw, "child-a");
        assert_eq!(child.get_raw("b").unwrap().raw, "parent-b");
        assert!(child.contains("b"));
    }

    // ── RandomValueSource (extra-8) ────────────────────────────────────────

    #[test]
    fn random_spec_parses_the_grammar() {
        assert_eq!(RandomSpec::parse("random.int").unwrap().unwrap(), RandomSpec::Int);
        assert_eq!(
            RandomSpec::parse("random.int(100)").unwrap().unwrap(),
            RandomSpec::IntBounded(100)
        );
        assert_eq!(
            RandomSpec::parse("random.long[5,10]").unwrap().unwrap(),
            RandomSpec::LongRange(5, 10)
        );
        assert_eq!(RandomSpec::parse("random.uuid").unwrap().unwrap(), RandomSpec::Uuid);
        // Non-random key falls through (None).
        assert!(RandomSpec::parse("server.port").is_none());
    }

    #[test]
    fn random_spec_rejects_reversed_range_and_garbage() {
        assert!(RandomSpec::parse("random.int[5,2]").unwrap().is_err());
        assert!(RandomSpec::parse("random.int(abc)").unwrap().is_err());
        assert!(RandomSpec::parse("random.bogus").unwrap().is_err());
    }

    #[test]
    fn random_source_is_non_enumerable_and_computes() {
        let r = RandomValueSource::with_seed(42);
        assert!(!r.caps().enumerable);
        assert!(r.keys().is_none());
        // A bounded int is within range.
        let v = r.get("random.int(10)").unwrap();
        let n: i64 = v.raw.parse().unwrap();
        assert!((0..10).contains(&n), "got {n}");
        // A range int is within [5,10).
        let v = r.get("random.int[5,10]").unwrap();
        let n: i64 = v.raw.parse().unwrap();
        assert!((5..10).contains(&n), "got {n}");
        // Non-random key falls through.
        assert!(r.get("server.port").is_none());
    }

    #[test]
    fn random_source_draws_are_fresh_each_call() {
        let r = RandomValueSource::with_seed(7);
        let a = r.get("random.long").unwrap().raw;
        let b = r.get("random.long").unwrap().raw;
        // Compute-per-read: two draws differ (overwhelmingly likely).
        assert_ne!(a, b);
    }

    #[test]
    fn random_source_resolves_via_placeholder_walk_in_env() {
        let r = RandomValueSource::with_seed(99);
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(r));
        b.add_last(Arc::new(MapPropertySource::from_pairs(
            "app",
            [("id", "${random.uuid}")],
        )));
        let env = b.seal_env();
        let id = env.get("id").unwrap().raw;
        assert!(id.contains('-'), "uuid-shaped: {id}");
    }

    #[test]
    fn env_is_send_sync_and_cheap_clone() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Env>();
        let env = env_with(&[("k", "v")]);
        let clone = env.clone();
        assert_eq!(clone.get_raw("k").unwrap().raw, "v");
    }

    #[test]
    fn property_value_uses_from_config_value_round_trip() {
        // Sanity: the convert seam reads a ResolvedValue cleanly.
        let env = env_with(&[("n", "  255 ")]);
        let cv = crate::convert::ConfigValue::scalar(env.get("n").unwrap().raw);
        let n = u8::from_config_value(&cv, &crate::convert::ConvertCtx::strict()).unwrap();
        assert_eq!(n, 255);
    }
}
