//! Stable cross-build identity: [`ContractId`] and the one [`contract_hash`] fn.
//!
//! `ContractId(u64)` is leaf's **stable, cross-build, cross-crate** identity for
//! everything durable: auto-config exclusion keys, name-collision provenance,
//! hierarchy cross-registry identity, run-participant stream tie-breaks, the
//! open [`ErrorKind::Integration`](crate::ErrorKind::Integration) error arm,
//! interned `AppType`/`MarkerId` values. `TypeId` is the in-process fast key
//! only — it is **never** serialized and **never** stable across builds.
//!
//! Pinned by phase4/SEAMS.md seam #2: there is exactly ONE hash entry point,
//! [`contract_hash`], fixed to **FNV-1a 64-bit** over the UTF-8 bytes of an
//! author-stable canonical path string. FNV-1a (not Fx) because it is
//! byte-order-independent, trivially `const fn` on stable, and identically
//! reproducible across compiler builds and platforms — the load-bearing
//! cross-build invariant. The top bit is NEVER reserved or salted (a salt
//! would make a config-file exclude string match in one build and miss in the
//! next — the exact silent break this seam exists to prevent).
//!
//! `ContractId` is a **semver surface**: a crate rename, a bean module-move, or
//! changing a re-exported type path shifts the id and is a breaking change.

use std::any::TypeId;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::Origin;

/// FNV-1a 64-bit offset basis (SEAMS seam #2, fixed).
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime (SEAMS seam #2, fixed).
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The SINGLE stable-identity hash entry point: FNV-1a 64-bit over the UTF-8
/// bytes of `canonical_path`.
///
/// Both macro-built Descriptor paths (`crate::module::Ident`) and hand-authored
/// FQN strings (`"web.servlet"`, `"leaf::Component"`) flow through this one
/// function — one algorithm, never two. `const fn` so it can seed const
/// `ContractId`/`MarkerId`/`AppType` values at compile time.
///
/// NEVER salt the result; the id must be byte-identical across every reader and
/// every rebuild.
#[must_use]
pub const fn contract_hash(canonical_path: &str) -> u64 {
    let bytes = canonical_path.as_bytes();
    let mut hash = FNV_OFFSET_BASIS;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    hash
}

/// Stable cross-build identity wrapping a [`contract_hash`] result.
///
/// See the module docs: this is a semver surface and is never salted.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContractId(pub u64);

impl ContractId {
    /// Build a `ContractId` from an author-stable canonical path string.
    ///
    /// `const` so const sites (`MarkerId`, `AppType`, built-in markers) can mint
    /// ids at compile time through the same one algorithm.
    #[must_use]
    pub const fn of(canonical_path: &str) -> Self {
        ContractId(contract_hash(canonical_path))
    }
}

impl std::fmt::Debug for ContractId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hex is the natural rendering for a hash-derived id.
        write!(f, "ContractId(0x{:016x})", self.0)
    }
}

/// Interned marker identity (qualifier markers, `@Primary`/`@Fallback`-style
/// markers, custom-qualifier marker types), keyed by the SAME stable
/// [`contract_hash`] over a canonical marker path — NEVER a `TypeId`.
///
/// `MarkerId` is the compile-safe single-marker qualifier key from
/// injection-resolution: it is minted at const sites by the macro and survives
/// across builds/crates exactly because it rides the one cross-build hash.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MarkerId(pub u64);

impl MarkerId {
    /// Mint a `MarkerId` from a canonical marker path string.
    ///
    /// `const` so macro-emitted const marker tables can intern at compile time
    /// through the one [`contract_hash`].
    #[must_use]
    pub const fn of(canonical_path: &str) -> Self {
        MarkerId(contract_hash(canonical_path))
    }
}

impl std::fmt::Debug for MarkerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MarkerId(0x{:016x})", self.0)
    }
}

/// The dense in-process slot id of an interned [`Origin`] in an [`OriginStore`].
///
/// `OriginId(u32)` is leaf's config-provenance counterpart to [`BeanId`]: a
/// dense, `Copy`, process-local key handed out by an [`OriginStore`] when an
/// [`Origin`] is interned, and resolved back to the `&Origin` by index. Like
/// `BeanId` it is NEVER serialized (durable identity is the resolved `Origin`
/// data itself) and threads cheaply through the config-load + error-render spine
/// in place of carrying the (potentially `'static`-leaking) `Origin::File` path
/// at every value.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OriginId(pub u32);

impl std::fmt::Debug for OriginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OriginId({})", self.0)
    }
}

/// The interner that mints dense [`OriginId`]s for [`Origin`] provenance values.
///
/// This is the fine-`file:line` provenance layer the config loaders stamp: the
/// loaders call [`intern_file`](OriginStore::intern_file) to record a parsed
/// property's `path:line`, get back a `Copy` [`OriginId`], and later the error
/// renderer calls [`resolve`](OriginStore::resolve) to point at the exact source
/// line. The always-available coarse [`Origin`] on a `PropertyValue` stays the
/// bedrock carrier; this store is the additive refined layer on top.
///
/// Built during the SINGLE-THREADED config load, then frozen and read — so it
/// holds NO lock (mirroring the `freeze()`-then-read discipline of [`BeanId`]).
/// Interning DEDUPLICATES: an equal [`Origin`] (or an equal `path:line`) yields
/// the same id, and a file path string is interned exactly once and shared by
/// every value on that line range.
///
/// ## The `&'static str` path
///
/// [`Origin::File`]'s `path` is `&'static str` (it is plain `Copy` data that the
/// macros also fill with `file!()`). A config file path is a RUNTIME string, so
/// the store interns each distinct path once and leaks it to `&'static str`.
/// This is sound and bounded: config load happens once at startup over a handful
/// of files, the leak is per-DISTINCT-path (deduplicated via the path index),
/// and the resulting `'static` provenance lives for the whole process exactly
/// like the macro-emitted `file!()` strings it sits beside.
#[derive(Default)]
pub struct OriginStore {
    /// Slot table: `OriginId.0` indexes this directly (dense, append-only).
    origins: Vec<Origin>,
    /// Dedup index for fully-resolved [`Origin`] values (incl. `Native`/`Span`).
    by_origin: HashMap<Origin, OriginId>,
    /// Dedup index for interned file paths → the one leaked `&'static str`, so a
    /// repeated path is leaked at most once.
    paths: HashMap<String, &'static str>,
}

impl OriginStore {
    /// An empty store (built up during the config load, then read).
    #[must_use]
    pub fn new() -> Self {
        OriginStore::default()
    }

    /// Number of interned origins (slot count).
    #[must_use]
    pub fn len(&self) -> usize {
        self.origins.len()
    }

    /// Whether no origin has been interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.origins.is_empty()
    }

    /// Intern an already-resolved [`Origin`] (`Native`/`Span`/`File`/…), returning
    /// its dense [`OriginId`]. An equal `Origin` deduplicates to the same id.
    ///
    /// Note: for a file provenance whose `path` is a RUNTIME string, prefer
    /// [`intern_file`](OriginStore::intern_file) so the path is interned/leaked
    /// once; passing a borrowed-`'static` `Origin::File` here interns by value.
    pub fn intern(&mut self, origin: Origin) -> OriginId {
        if let Some(&id) = self.by_origin.get(&origin) {
            return id;
        }
        let id = OriginId(self.origins.len() as u32);
        self.origins.push(origin);
        self.by_origin.insert(origin, id);
        id
    }

    /// Intern a fine config-file `path:line` provenance, returning its dense
    /// [`OriginId`]. The `path` may be a runtime string; it is interned (leaked to
    /// `&'static str`) exactly once and shared across every line on that path.
    /// Equal `(path, line)` pairs deduplicate to the same id.
    pub fn intern_file(&mut self, path: &str, line: u32) -> OriginId {
        let static_path = self.intern_path(path);
        self.intern(Origin::File { path: static_path, line })
    }

    /// Resolve an [`OriginId`] back to its [`Origin`]. An out-of-range id yields
    /// [`Origin::Unknown`] (graceful — a stale/foreign id never panics).
    #[must_use]
    pub fn resolve(&self, id: OriginId) -> &Origin {
        self.origins.get(id.0 as usize).unwrap_or(&Origin::Unknown)
    }

    /// Intern a path string to a process-`'static` slice, deduplicating so a
    /// repeated path is leaked at most once.
    fn intern_path(&mut self, path: &str) -> &'static str {
        if let Some(&s) = self.paths.get(path) {
            return s;
        }
        let leaked: &'static str = Box::leak(path.to_owned().into_boxed_str());
        self.paths.insert(path.to_owned(), leaked);
        leaked
    }
}

/// The dense in-process bean slot id materialized at `freeze()`.
///
/// `BeanId(u32)` is leaf's per-registry dense slot key: the frozen registry's
/// `rows`, `singletons`, and both indices are all joined on `BeanId.0`, so a
/// ready-read is a bounds-checked array index. It is process-local and is NEVER
/// serialized — durable identity is [`ContractId`]. Dense + `Copy` so it threads
/// cheaply through the resolution spine.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BeanId(pub u32);

impl std::fmt::Debug for BeanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BeanId({})", self.0)
    }
}

/// The interned canonical bean name.
///
/// `Arc<str>` (interned at `freeze()`): cheap-clone identity with
/// integer-cheap comparison and no `String` churn (registry-core bean-naming).
/// Each registry interns its own names; cross-registry identity is by
/// string-equality at the [`BeanKey::ByName`] boundary, so there is no shared
/// contended interner.
pub type BeanName = Arc<str>;

/// The keyed lookup currency the registry resolves to a [`BeanId`].
///
/// The registry exposes `resolve_id(&BeanKey) -> Result<BeanId, _>`; these are
/// the four orthogonal ways a consumer (or a hierarchy parent walk) names a
/// bean. `ByName` uses an interned [`BeanName`] so equality is value-based.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum BeanKey {
    /// By concrete or `dyn`-service [`TypeId`] (the in-process fast key).
    ByType(TypeId),
    /// By interned canonical [`BeanName`] (string-equal across registries).
    ByName(BeanName),
    /// By stable cross-build [`ContractId`] (hierarchy / durable identity).
    ByContract(ContractId),
    /// By a `(TypeId, BeanName)` pair — type-narrowed name disambiguation.
    ByTypeAndName(TypeId, BeanName),
}

/// Derive the default bean name from a simple (last-path-segment) type ident,
/// applying Spring's `Introspector.decapitalize` rule INCLUDING the acronym
/// edge case (registry-core bean-naming).
///
/// Rule (verbatim from `java.beans.Introspector.decapitalize`):
/// - if the first TWO chars are uppercase, the name is returned UNCHANGED
///   (acronym preservation: `URLFooServiceImpl` stays, `IOService` stays);
/// - otherwise the leading char is lowercased (`UserService` → `userService`,
///   `Url` → `url`);
/// - an empty string is returned unchanged.
///
/// Returns a [`Cow`] so the common already-canonical / acronym / empty cases
/// allocate nothing; only a genuine decapitalize owns a new `String`. This is a
/// pure, `const`-friendly-in-spirit fn living in leaf-core precisely so it is
/// unit-testable in a normal crate and shared by `derive`-time macro expansion.
#[must_use]
pub fn derive_default_name(simple: &str) -> Cow<'_, str> {
    let mut chars = simple.chars();
    let Some(first) = chars.next() else {
        // Empty: nothing to do.
        return Cow::Borrowed(simple);
    };
    // Acronym edge case: first TWO chars uppercase => preserve verbatim. We must
    // honor full Unicode uppercase, matching Java's Character.isUpperCase intent
    // closely enough for ASCII idents (the only legal Rust idents here).
    if first.is_uppercase()
        && let Some(second) = chars.next()
            && second.is_uppercase() {
                return Cow::Borrowed(simple);
            }
    // Already canonical (leading char not uppercase => no change, incl. `_`/
    // lowercase / digit leads).
    if !first.is_uppercase() {
        return Cow::Borrowed(simple);
    }
    // Genuine decapitalize: lowercase ONLY the first char, keep the tail.
    let mut out = String::with_capacity(simple.len());
    for lc in first.to_lowercase() {
        out.push(lc);
    }
    out.push_str(&simple[first.len_utf8()..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_hashes_to_the_offset_basis() {
        // FNV-1a of the empty input is, by definition, the offset basis.
        assert_eq!(contract_hash(""), FNV_OFFSET_BASIS);
    }

    #[test]
    fn known_fnv1a_64_vectors() {
        // Canonical published FNV-1a 64-bit test vectors. If these ever change,
        // every persisted ContractId in the ecosystem silently shifts — so they
        // are pinned here as a cross-build-stability regression guard.
        assert_eq!(contract_hash("a"), 0xaf63dc4c8601ec8c);
        assert_eq!(contract_hash("foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn distinct_paths_produce_distinct_ids() {
        let a = ContractId::of("leaf_redis::RedisAutoConfig");
        let b = ContractId::of("leaf_redis::MongoAutoConfig");
        assert_ne!(a, b);
    }

    #[test]
    fn identical_paths_produce_identical_ids_reproducibly() {
        // Reproducibility is the whole point: same string => same id, always.
        let a = ContractId::of("web.servlet");
        let b = ContractId::of("web.servlet");
        assert_eq!(a, b);
        assert_eq!(a.0, contract_hash("web.servlet"));
    }

    #[test]
    fn const_evaluable() {
        // Must be usable in const context (macro-emitted const Descriptor rows).
        const ID: ContractId = ContractId::of("leaf::Component");
        assert_eq!(ID.0, contract_hash("leaf::Component"));
    }

    #[test]
    fn debug_renders_hex() {
        let id = ContractId(0xdead_beef);
        assert_eq!(format!("{id:?}"), "ContractId(0x00000000deadbeef)");
    }

    // ── BeanId ──────────────────────────────────────────────────────────────

    #[test]
    fn bean_id_is_a_dense_copy_u32_key() {
        let a = BeanId(0);
        let b = BeanId(1);
        assert_ne!(a, b);
        // Copy + dense ordering (slot order = id order).
        let c = a;
        assert_eq!(a, c);
        assert!(BeanId(2) > BeanId(1));
        // The raw slot index is directly readable.
        assert_eq!(BeanId(7).0 as usize, 7usize);
    }

    // ── MarkerId ────────────────────────────────────────────────────────────

    #[test]
    fn marker_id_is_minted_through_the_one_contract_hash() {
        // MarkerId is interned marker identity over the SAME stable hash, never
        // a TypeId; same canonical path => same id, across builds.
        let m = MarkerId::of("leaf::marker::Primary");
        assert_eq!(m.0, contract_hash("leaf::marker::Primary"));
        assert_eq!(m, MarkerId::of("leaf::marker::Primary"));
        assert_ne!(m, MarkerId::of("leaf::marker::Fallback"));
    }

    #[test]
    fn marker_id_is_const_evaluable() {
        const M: MarkerId = MarkerId::of("leaf::marker::Qualifier");
        assert_eq!(M.0, contract_hash("leaf::marker::Qualifier"));
    }

    // ── BeanKey ─────────────────────────────────────────────────────────────

    #[test]
    fn bean_key_by_type_and_by_name_are_distinct_lookups() {
        use std::any::TypeId;
        let by_type = BeanKey::ByType(TypeId::of::<u32>());
        let by_name: BeanKey = BeanKey::ByName(BeanName::from("userService"));
        assert_ne!(by_type, by_name);
        // Same name string => equal key (Arc<str> compares by value).
        assert_eq!(
            BeanKey::ByName(BeanName::from("a")),
            BeanKey::ByName(BeanName::from("a"))
        );
    }

    #[test]
    fn bean_key_carries_contract_and_type_and_name_variants() {
        use std::any::TypeId;
        let k = BeanKey::ByContract(ContractId::of("crate::Foo"));
        assert!(matches!(k, BeanKey::ByContract(_)));
        let k2 = BeanKey::ByTypeAndName(TypeId::of::<u8>(), BeanName::from("x"));
        assert!(matches!(k2, BeanKey::ByTypeAndName(_, _)));
    }

    // ── derive_default_name (Spring decapitalize + acronym edge case) ─────────

    #[test]
    fn derive_default_name_decapitalizes_the_first_char() {
        // Ordinary case: lowercase the leading char (Spring's Introspector rule).
        assert_eq!(derive_default_name("UserService"), "userService");
        assert_eq!(derive_default_name("FooBar"), "fooBar");
        assert_eq!(derive_default_name("A"), "a");
    }

    #[test]
    fn derive_default_name_preserves_acronym_when_first_two_chars_uppercase() {
        // THE edge case: if the first TWO chars are uppercase, the name is left
        // as-is (Spring: "URL" / "URLFooServiceImpl" stay verbatim).
        assert_eq!(derive_default_name("URLFooServiceImpl"), "URLFooServiceImpl");
        assert_eq!(derive_default_name("URL"), "URL");
        assert_eq!(derive_default_name("IOService"), "IOService");
        // Exactly two uppercase then lowercase: still preserved (first two upper).
        assert_eq!(derive_default_name("ID"), "ID");
    }

    #[test]
    fn derive_default_name_lowercases_single_leading_capital_before_lowercase() {
        // First char upper, second char lower => decapitalize (not an acronym).
        assert_eq!(derive_default_name("Url"), "url");
        assert_eq!(derive_default_name("Id"), "id");
    }

    #[test]
    fn derive_default_name_passes_through_already_lowercase_and_empty() {
        assert_eq!(derive_default_name("userService"), "userService");
        assert_eq!(derive_default_name(""), "");
        // A non-letter leading char is left untouched (nothing to decapitalize).
        assert_eq!(derive_default_name("_Hidden"), "_Hidden");
    }

    #[test]
    fn derive_default_name_borrows_when_unchanged_and_owns_when_changed() {
        // Cow discipline: no allocation when the name is already canonical.
        assert!(matches!(derive_default_name("alreadyLower"), Cow::Borrowed(_)));
        assert!(matches!(derive_default_name("URLThing"), Cow::Borrowed(_)));
        assert!(matches!(derive_default_name(""), Cow::Borrowed(_)));
        // Allocation only on the genuine decapitalize.
        assert!(matches!(derive_default_name("UserService"), Cow::Owned(_)));
    }

    // ── OriginId / OriginStore (the interned fine-origin layer) ───────────────

    use crate::error::Origin;

    #[test]
    fn origin_id_is_a_dense_copy_u32_key() {
        // Mirrors BeanId: a dense, Copy, process-local slot key.
        let a = OriginId(0);
        let b = OriginId(1);
        assert_ne!(a, b);
        let c = a;
        assert_eq!(a, c);
        assert!(OriginId(2) > OriginId(1));
        assert_eq!(OriginId(7).0 as usize, 7usize);
    }

    #[test]
    fn interning_two_equal_origins_yields_the_same_id() {
        // The whole point of the interner: equal origins dedup to one slot.
        let mut store = OriginStore::new();
        let o = Origin::Native { crate_name: Some("leaf-config::yaml") };
        let a = store.intern(o);
        let b = store.intern(o);
        assert_eq!(a, b);
        // A distinct origin gets a distinct id.
        let c = store.intern(Origin::Native { crate_name: Some("leaf-config::json") });
        assert_ne!(a, c);
    }

    #[test]
    fn resolve_round_trips_the_interned_origin() {
        let mut store = OriginStore::new();
        let o = Origin::Native { crate_name: Some("leaf-config::env") };
        let id = store.intern(o);
        assert_eq!(*store.resolve(id), o);
    }

    #[test]
    fn file_origin_interns_and_resolves_with_line() {
        // The headline: a config value's fine file:line origin round-trips.
        let mut store = OriginStore::new();
        let id = store.intern_file("application.yaml", 7);
        match *store.resolve(id) {
            Origin::File { path, line } => {
                assert_eq!(path, "application.yaml");
                assert_eq!(line, 7);
            }
            other => panic!("expected Origin::File, got {other:?}"),
        }
    }

    #[test]
    fn file_origin_dedups_by_path_and_line() {
        // Same (path, line) => same id; the path string is interned once.
        let mut store = OriginStore::new();
        let a = store.intern_file("application.yaml", 7);
        let b = store.intern_file("application.yaml", 7);
        assert_eq!(a, b);
        // Same path, different line => distinct id (but the path is shared).
        let c = store.intern_file("application.yaml", 8);
        assert_ne!(a, c);
        // The two file origins on the same path resolve to the SAME &'static str.
        let (Origin::File { path: pa, .. }, Origin::File { path: pc, .. }) =
            (*store.resolve(a), *store.resolve(c))
        else {
            panic!("expected File origins");
        };
        assert!(std::ptr::eq(pa.as_ptr(), pc.as_ptr()));
    }

    #[test]
    fn resolving_an_unknown_id_yields_unknown_not_a_panic() {
        // Graceful degradation: an out-of-range id never panics.
        let store = OriginStore::new();
        assert_eq!(*store.resolve(OriginId(999)), Origin::Unknown);
    }

    #[test]
    fn default_store_is_empty_and_origin_id_debugs_densely() {
        let store = OriginStore::default();
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
        assert_eq!(format!("{:?}", OriginId(3)), "OriginId(3)");
    }
}
