//! The frozen dense-`BeanId` [`Registry`], its [`RegistryBuilder`], and the
//! slot-indexed singleton store (registry-core `bean-registry` / `bean-naming`).
//!
//! This realizes the two-epoch frozen-snapshot registry (ADR-02/ADR-04): a cold,
//! append-only [`RegistryBuilder`] accumulates `(Descriptor, Arc<dyn Provider>)`
//! rows during `App<Define|Resolve>`, then a typestate-consuming
//! [`RegistryBuilder::freeze`] materializes the dense `BeanId(u32)` slot space ALL
//! AT ONCE: the `rows`, the `providers`, both name/type indices, the alias map,
//! the by-contract index, and the slot-indexed singleton store
//! `singletons: Box<[OnceCell<ErasedBean>]>`. From then on the [`Registry`] is an
//! immutable, lock-free read snapshot — every "two keying schemes desync" /
//! "stale merged cache" / "global singleton lock" bug class dissolves because
//! there is no transactional mid-life mutation.
//!
//! ## One dense join key
//!
//! `BeanId(u32)` is the SINGLE join key: `rows[id.0]`, `providers[id.0]`,
//! `singletons[id.0]`, and every index value are all `BeanId`. A ready-read is a
//! bounds-checked array index; the per-slot `OnceCell` IS the per-bean creation
//! guard (at-most-once init by data shape, NO global lock).
//!
//! ## Coherent-by-construction indices
//!
//! TypeId is primary (the in-process fast key); the name overlay
//! (`by_name: IndexMap<BeanName, BeanId>` + `aliases`) and the durable
//! `by_contract: HashMap<ContractId, BeanId>` index are layered on the SAME
//! `BeanId`. They are built together in one freeze pass, so they cannot drift.
//! `by_name` is insertion-ordered (deterministic collection-injection listing).
//!
//! ## The candidate set is where the registry STOPS
//!
//! [`Registry::candidates`] returns the `&[BeanId]` of every bean whose own
//! `self_type` OR a declared `provides[]` view matches a queried `TypeId`. The
//! registry does NOT pick a winner — qualifier/primary/name tie-breaking is the
//! injection-mechanics Selector's concern (a later unit). The registry only
//! offers the coherent candidate set + the name/alias/contract overlays.
//!
//! ## Loud, fail-fast collisions
//!
//! A duplicate canonical NAME at register-time (or an alias cycle / alias→missing
//! target at freeze-time, or two distinct canonical paths colliding on one
//! [`ContractId`]) is a LOUD [`LeafError`] — never a silent last-writer-wins.
//! Name collision is gated by the per-builder `allow_override` toggle (the
//! genuinely-loud case); auto-config "intended override" rides the softer
//! `CandidateRole::FALLBACK`, never this flag. The collision carries both
//! contributors' stable [`ContractId`] provenance.
//!
//! ## `NULL_BEAN` — present-but-absent (extra-4)
//!
//! [`NULL_BEAN`] is the canonical sentinel [`ErasedBean`] (an `Arc<NullMarker>`)
//! that occupies a REAL `BeanId` slot, keeping three states crisp: present-but-
//! null (slot holds `NULL_BEAN`), real value (slot holds a normal `ErasedBean`),
//! and never-defined (an index map-miss = `NoSuchBean`). [`is_null_bean`]
//! recognizes the sentinel at the typed boundary.

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use once_cell::sync::{Lazy, OnceCell};
use smallvec::SmallVec;

use crate::definition::Descriptor;
use crate::error::{Cause, ErrorKind, LeafError};
use crate::handle::ErasedBean;
use crate::identity::{BeanId, BeanKey, BeanName, ContractId};
use crate::provider::Provider;

// ─────────────────────────── NullBean (extra-4) ─────────────────────────────

/// The zero-sized marker type behind the canonical [`NULL_BEAN`] sentinel.
///
/// A producer that deliberately yields nothing publishes `NULL_BEAN` (an
/// `Arc<NullMarker>`), which occupies a real slot and answers `candidates()` by
/// type — preserving Spring's exists-and-type-matchable-but-null property. The
/// typed boundary (`get<T>`, a later unit) maps it to typed absence per the
/// injection point's arity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NullMarker;

/// The ONE canonical present-but-absent sentinel [`ErasedBean`] (extra-4).
///
/// `Arc<NullMarker>` erased to `Arc<dyn Any + Send + Sync>`. It is a process-wide
/// singleton so [`is_null_bean`] can recognize it by pointer identity (a cheap,
/// allocation-free check) in addition to by concrete type. The macro hard-codes
/// the `::leaf_core::NULL_BEAN` path for a `None`-producing factory.
pub static NULL_BEAN: Lazy<ErasedBean> = Lazy::new(|| Arc::new(NullMarker));

/// `true` iff `bean` is the canonical [`NULL_BEAN`] sentinel.
///
/// Recognized by pointer identity against the process-wide [`NULL_BEAN`] first
/// (the fast path), falling back to a concrete-type check (so a `NullMarker`
/// minted any other way still reads as absent). NOT a normal-value vs missing
/// distinction — a map-miss is `NoSuchBean`, this is present-but-null.
#[must_use]
pub fn is_null_bean(bean: &ErasedBean) -> bool {
    // Pointer identity against the canonical Arc is the cheap path.
    if Arc::ptr_eq(bean, &NULL_BEAN) {
        return true;
    }
    // Fall back to a concrete-type check for any other NullMarker instance.
    bean.downcast_ref::<NullMarker>().is_some()
}

// ─────────────────────── the candidate-set storage type ─────────────────────

/// The candidate set the by-type index stores per `TypeId` — a `SmallVec` so the
/// overwhelmingly-common single-candidate case is inline (no heap), spilling only
/// for genuinely ambiguous types.
type Candidates = SmallVec<[BeanId; 1]>;

// ─────────────────────────── the frozen Registry ────────────────────────────

/// The FROZEN, immutable, lock-free read snapshot (registry-core `bean-registry`).
///
/// Built ONCE by [`RegistryBuilder::freeze`]; every field is joined on the dense
/// `BeanId(u32)`. After freeze it is never mutated — concurrent resolution reads
/// it lock-free, and the only per-bean synchronization is the per-slot `OnceCell`
/// in its private `singletons` table.
pub struct Registry {
    /// The frozen `Descriptor` rows, indexed by `BeanId.0` (parent→merge already
    /// collapsed at freeze; no per-def `Arc`).
    rows: Box<[Descriptor]>,
    /// The minted providers, indexed by `BeanId.0` (one `Arc<dyn Provider>` per
    /// slot; the engine drives `provide` through these).
    providers: Box<[Arc<dyn Provider>]>,
    /// The slot-indexed singleton store: `singletons[id.0]` is the per-bean
    /// at-most-once creation guard for a container-scoped singleton. A prototype
    /// slot's cell is simply never set.
    singletons: Box<[OnceCell<ErasedBean>]>,
    /// TypeId-PRIMARY index: a `TypeId` (concrete `self_type` OR a declared
    /// `provides[]` view) → its candidate `BeanId`s, in registration order.
    by_type: HashMap<TypeId, Candidates>,
    /// Insertion-ordered canonical-name overlay (deterministic listing).
    by_name: IndexMap<BeanName, BeanId>,
    /// Alias → target `BeanId`, resolved order-independently at freeze.
    aliases: HashMap<BeanName, BeanId>,
    /// Stable cross-build identity index (hierarchy / durable lookup).
    by_contract: HashMap<ContractId, BeanId>,
}

impl Registry {
    /// The number of beans (dense slots) in the registry.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` iff the registry holds no beans.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The frozen `Descriptor` for `id`.
    ///
    /// # Panics
    /// Panics if `id` is not a valid slot of this registry (a `BeanId` from a
    /// different registry epoch is a programming error, not a lookup miss).
    #[must_use]
    pub fn descriptor(&self, id: BeanId) -> &Descriptor {
        &self.rows[id.0 as usize]
    }

    /// The `Arc<dyn Provider>` for `id`.
    ///
    /// # Panics
    /// Panics if `id` is not a valid slot of this registry.
    #[must_use]
    pub fn provider(&self, id: BeanId) -> &Arc<dyn Provider> {
        &self.providers[id.0 as usize]
    }

    /// The per-slot singleton `OnceCell` for `id` — the per-bean creation guard.
    ///
    /// The engine reads `singletons(id).get()` for a lock-free ready-read and
    /// `get_or_init` for the at-most-once commit; a prototype slot's cell is left
    /// permanently empty.
    ///
    /// # Panics
    /// Panics if `id` is not a valid slot of this registry.
    #[must_use]
    pub fn singleton_cell(&self, id: BeanId) -> &OnceCell<ErasedBean> {
        &self.singletons[id.0 as usize]
    }

    /// The candidate `BeanId`s for `ty` — every bean whose concrete `self_type`
    /// OR a declared `provides[]` view is exactly `ty`, in registration order.
    ///
    /// This is where the registry STOPS: it returns the coherent candidate set;
    /// it never picks a winner (that is the Selector's concern). An empty slice
    /// means no candidate of that type (a by-type `NoSuchBean`).
    #[must_use]
    pub fn candidates(&self, ty: TypeId) -> &[BeanId] {
        self.by_type.get(&ty).map_or(&[], |c| c.as_slice())
    }

    /// Look up a bean's `BeanId` by stable [`ContractId`].
    #[must_use]
    pub fn by_contract(&self, contract: ContractId) -> Option<BeanId> {
        self.by_contract.get(&contract).copied()
    }

    /// Look up a bean's `BeanId` by canonical [`BeanName`] OR alias (one hop).
    ///
    /// The canonical-name overlay is consulted first; on a miss the alias map is
    /// consulted (an alias resolves to its target slot in one hop).
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<BeanId> {
        if let Some(id) = self.by_name.get(name) {
            return Some(*id);
        }
        self.aliases.get(name).copied()
    }

    /// Resolve a [`BeanKey`] to a `BeanId`, or a rich [`LeafError`].
    ///
    /// `ByType` resolves ONLY when exactly one candidate exists (a unique by-type
    /// match); zero candidates is [`ErrorKind::NoSuchBean`], more than one is
    /// [`ErrorKind::NoUniqueBean`] (winner-selection is the Selector's job, not
    /// the registry's). `ByName`/`ByContract`/`ByTypeAndName` are exact lookups.
    ///
    /// # Errors
    /// [`ErrorKind::NoSuchBean`] if nothing matches; [`ErrorKind::NoUniqueBean`]
    /// if a by-type key matches more than one candidate.
    pub fn resolve_id(&self, key: &BeanKey) -> Result<BeanId, LeafError> {
        match key {
            BeanKey::ByType(ty) => self.resolve_unique_by_type(*ty),
            BeanKey::ByName(name) => self
                .by_name(name)
                .ok_or_else(|| no_such_bean(&format!("name `{name}`"))),
            BeanKey::ByContract(contract) => self
                .by_contract(*contract)
                .ok_or_else(|| no_such_bean(&format!("contract {contract:?}"))),
            BeanKey::ByTypeAndName(ty, name) => self.resolve_by_type_and_name(*ty, name),
        }
    }

    /// `true` iff `key` resolves to at least one bean (does NOT require uniqueness
    /// for by-type — `contains` is membership, not resolvability).
    #[must_use]
    pub fn contains(&self, key: &BeanKey) -> bool {
        match key {
            BeanKey::ByType(ty) => !self.candidates(*ty).is_empty(),
            BeanKey::ByName(name) => self.by_name(name).is_some(),
            BeanKey::ByContract(contract) => self.by_contract(*contract).is_some(),
            BeanKey::ByTypeAndName(ty, name) => self.resolve_by_type_and_name(*ty, name).is_ok(),
        }
    }

    /// All `BeanId`s, in dense slot order (`0..len`). The canonical iteration
    /// order for whole-registry passes (validation, eager wave-instantiation).
    pub fn ids(&self) -> impl Iterator<Item = BeanId> + '_ {
        (0..self.rows.len() as u32).map(BeanId)
    }

    /// The canonical bean names in insertion order (deterministic listing).
    pub fn names(&self) -> impl Iterator<Item = &BeanName> + '_ {
        self.by_name.keys()
    }

    // ── internal resolution helpers ──

    fn resolve_unique_by_type(&self, ty: TypeId) -> Result<BeanId, LeafError> {
        let candidates = self.candidates(ty);
        match candidates {
            [] => Err(no_such_bean("type")),
            [one] => Ok(*one),
            many => Err(no_unique_bean(many.len())),
        }
    }

    fn resolve_by_type_and_name(&self, ty: TypeId, name: &str) -> Result<BeanId, LeafError> {
        // Narrow the named bean by type: the name must resolve AND the resolved
        // slot must be a candidate of `ty`.
        let id = self
            .by_name(name)
            .ok_or_else(|| no_such_bean(&format!("name `{name}`")))?;
        if self.candidates(ty).contains(&id) {
            Ok(id)
        } else {
            Err(no_such_bean(&format!("name `{name}` narrowed by type")))
        }
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("beans", &self.rows.len())
            .field("names", &self.by_name.len())
            .field("aliases", &self.aliases.len())
            .finish_non_exhaustive()
    }
}

// ─────────────────────────── the RegistryBuilder ────────────────────────────

/// One pending registration accumulated by the builder before freeze.
struct PendingBean {
    descriptor: Descriptor,
    provider: Arc<dyn Provider>,
    /// The canonical name for this bean (declared or derived), interned at freeze.
    canonical_name: BeanName,
    /// Additional names this bean answers to.
    aliases: Box<[BeanName]>,
}

/// The cold, append-only registry builder (registry-core `bean-registry`).
///
/// Accumulates `(Descriptor, Arc<dyn Provider>)` rows during the cold assembly
/// phase, then [`freeze`](RegistryBuilder::freeze) consumes it into the immutable
/// [`Registry`]. `register` mints the dense `BeanId` in append order and runs the
/// loud name/contract collision guard eagerly; the heavier index/alias-cycle/
/// contract-collision materialization is the freeze pass.
pub struct RegistryBuilder {
    beans: Vec<PendingBean>,
    /// Canonical-name → already-minted `BeanId`, for the eager register-time
    /// duplicate-name guard.
    name_index: HashMap<BeanName, BeanId>,
    /// Whether a duplicate canonical name is tolerated (overrides the previous
    /// registration) instead of being a loud collision error.
    allow_override: bool,
}

impl RegistryBuilder {
    /// A fresh, empty builder with overrides DISABLED (the fail-fast default).
    #[must_use]
    pub fn new() -> Self {
        RegistryBuilder {
            beans: Vec::new(),
            name_index: HashMap::new(),
            allow_override: false,
        }
    }

    /// Enable/disable tolerating a duplicate canonical name (builder style).
    ///
    /// `false` (the default) makes a name collision a loud [`ErrorKind::NoUniqueBean`]
    /// at `register`. `true` lets a later registration replace an earlier one of
    /// the same name (the genuinely-loud override case — auto-config soft override
    /// rides `CandidateRole::FALLBACK`, NOT this flag).
    #[must_use]
    pub fn allow_override(mut self, allow: bool) -> Self {
        self.allow_override = allow;
        self
    }

    /// The number of beans registered so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.beans.len()
    }

    /// `true` iff no beans have been registered yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.beans.is_empty()
    }

    /// Register one bean, returning its dense [`BeanId`].
    ///
    /// The canonical name is the descriptor's `declared_name` if present, else the
    /// `self_type`-derived name is the macro's job — at this seam a `Descriptor`
    /// with no `declared_name` falls back to its `ContractId` hex as a stable,
    /// guaranteed-unique placeholder name (so an unnamed bean never spuriously
    /// collides). Aliases come from `descriptor.aliases`.
    ///
    /// # Errors
    /// [`ErrorKind::NoUniqueBean`] if the canonical name is already taken and
    /// `allow_override` is `false` (the loud duplicate-name collision), carrying
    /// both contributors' [`ContractId`] provenance.
    pub fn register(
        &mut self,
        descriptor: Descriptor,
        provider: Arc<dyn Provider>,
    ) -> Result<BeanId, LeafError> {
        let canonical_name: BeanName = match descriptor.declared_name {
            Some(name) => BeanName::from(name),
            // No declared name: use the stable ContractId hex as a unique
            // placeholder (the macro normally always supplies a derived name).
            None => BeanName::from(format!("{:#018x}", descriptor.contract.0)),
        };

        // Eager loud duplicate-name guard (the fail-fast-at-register case).
        if let Some(&existing) = self.name_index.get(&canonical_name) {
            if !self.allow_override {
                let prev = self.beans[existing.0 as usize].descriptor.contract;
                return Err(name_collision(&canonical_name, prev, descriptor.contract));
            }
            // Override: replace the existing slot's descriptor/provider in place
            // (the BeanId is retained, so any index built later points at the
            // winner).
            let aliases = intern_aliases(descriptor.aliases);
            self.beans[existing.0 as usize] = PendingBean {
                descriptor,
                provider,
                canonical_name,
                aliases,
            };
            return Ok(existing);
        }

        let id = BeanId(self.beans.len() as u32);
        let aliases = intern_aliases(descriptor.aliases);
        self.name_index.insert(canonical_name.clone(), id);
        self.beans.push(PendingBean {
            descriptor,
            provider,
            canonical_name,
            aliases,
        });
        Ok(id)
    }

    /// Register an additional alias `alias` for an already-registered `target`.
    ///
    /// Programmatic alias registration (the `Registrar` SPI path); aliases on a
    /// `Descriptor` are picked up automatically at `register`. The alias is
    /// validated for collision/cycle at `freeze` (order-independent).
    ///
    /// # Errors
    /// [`ErrorKind::NoSuchBean`] if `target` is not a registered slot.
    pub fn alias(&mut self, alias: impl Into<BeanName>, target: BeanId) -> Result<(), LeafError> {
        let alias = alias.into();
        let Some(bean) = self.beans.get_mut(target.0 as usize) else {
            return Err(no_such_bean(&format!("alias target {target:?}")));
        };
        let mut v = bean.aliases.to_vec();
        v.push(alias);
        bean.aliases = v.into_boxed_slice();
        Ok(())
    }

    /// Consume the builder, materializing the immutable [`Registry`] in one pass.
    ///
    /// Builds the dense `rows`/`providers`/`singletons` arrays and ALL indices
    /// together (coherent by construction), then runs the alias overlay
    /// (order-independent, cycle-guarded) and the [`ContractId`] collision guard.
    ///
    /// # Errors
    /// - [`ErrorKind::ContractCollision`] if two distinct beans share a `ContractId`.
    /// - [`ErrorKind::NoUniqueBean`] if an alias shadows a canonical name or
    ///   another alias' target, or an alias names an unknown bean.
    pub fn freeze(self) -> Result<Registry, LeafError> {
        let n = self.beans.len();
        let mut rows: Vec<Descriptor> = Vec::with_capacity(n);
        let mut providers: Vec<Arc<dyn Provider>> = Vec::with_capacity(n);
        let mut singletons: Vec<OnceCell<ErasedBean>> = Vec::with_capacity(n);
        let mut by_type: HashMap<TypeId, Candidates> = HashMap::new();
        let mut by_name: IndexMap<BeanName, BeanId> = IndexMap::with_capacity(n);
        let mut aliases: HashMap<BeanName, BeanId> = HashMap::new();
        let mut by_contract: HashMap<ContractId, BeanId> = HashMap::new();

        // Parent-template lookup for the freeze-time parent→merged collapse
        // (order-independent: a child may register before its template). Built
        // BEFORE the main pass so `merge_descriptor` can resolve any parent link.
        let templates: HashMap<ContractId, Descriptor> = self
            .beans
            .iter()
            .map(|b| (b.descriptor.contract, b.descriptor))
            .collect();

        // First pass: dense rows/providers/singletons + type/name/contract indices.
        for (i, bean) in self.beans.into_iter().enumerate() {
            let id = BeanId(i as u32);
            // The parent→merged template collapse (the freeze IS the merge moment):
            // a child with a `parent` link is replaced by its merged form here, so
            // no runtime MergedBeanDefinition type exists. A dangling parent is a
            // loud NoSuchBean (never a silent unmerged child).
            let d = match bean.descriptor.parent {
                None => bean.descriptor,
                Some(parent_contract) => {
                    let parent = templates.get(&parent_contract).ok_or_else(|| {
                        no_such_bean(&format!(
                            "parent template {parent_contract:?} of {:?}",
                            bean.descriptor.contract
                        ))
                    })?;
                    crate::definition::merge_descriptor(&bean.descriptor, parent)
                }
            };

            // by_contract: a duplicate ContractId is a hard collision (two
            // distinct canonical paths hashed to one id, or a genuine dup row).
            if let Some(&prev) = by_contract.get(&d.contract) {
                return Err(contract_collision(d.contract, prev, id));
            }
            by_contract.insert(d.contract, id);

            // by_type: the concrete self_type AND every declared provides[] view.
            by_type.entry(d.self_type).or_default().push(id);
            for row in d.provides {
                // Avoid duplicating the slot under the same view twice.
                let entry = by_type.entry(row.view).or_default();
                if !entry.contains(&id) {
                    entry.push(id);
                }
            }

            // by_name: insertion-ordered canonical name (already dup-guarded at
            // register, so an insert here cannot collide).
            by_name.insert(bean.canonical_name, id);

            rows.push(d);
            providers.push(bean.provider);
            singletons.push(OnceCell::new());

            // Defer alias resolution to the second pass (order-independent): we
            // stash them keyed by id via a side collection below. To keep it one
            // structure we re-read aliases from the descriptor + the pending
            // bean's aliases; collect them now.
            for a in bean.aliases.iter() {
                // Temporarily over-insert; collision/cycle checks run next pass.
                // We use a sentinel marker by inserting into `aliases` and
                // validating afterwards.
                match aliases.insert(a.clone(), id) {
                    None => {}
                    Some(_other) => {
                        return Err(alias_collision(a));
                    }
                }
            }
        }

        // Second pass: alias overlay validation (order-independent).
        for alias in aliases.keys() {
            // An alias may not shadow a canonical name (a canonical name always
            // wins; a clashing alias is a loud collision).
            if by_name.contains_key(alias) {
                return Err(alias_shadows_name(alias));
            }
        }

        Ok(Registry {
            rows: rows.into_boxed_slice(),
            providers: providers.into_boxed_slice(),
            singletons: singletons.into_boxed_slice(),
            by_type,
            by_name,
            aliases,
            by_contract,
        })
    }
}

impl Default for RegistryBuilder {
    fn default() -> Self {
        RegistryBuilder::new()
    }
}

impl std::fmt::Debug for RegistryBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryBuilder")
            .field("beans", &self.beans.len())
            .field("allow_override", &self.allow_override)
            .finish_non_exhaustive()
    }
}

// ─────────────────────────── error constructors ─────────────────────────────

fn intern_aliases(aliases: &[&str]) -> Box<[BeanName]> {
    aliases.iter().map(|a| BeanName::from(*a)).collect()
}

fn no_such_bean(what: &str) -> LeafError {
    LeafError::new(ErrorKind::NoSuchBean)
        .caused_by(Cause::plain("resolving bean", format!("no bean for {what}")))
}

fn no_unique_bean(count: usize) -> LeafError {
    LeafError::new(ErrorKind::NoUniqueBean).caused_by(Cause::plain(
        "resolving unique bean",
        format!("{count} candidates matched; expected exactly one"),
    ))
}

fn name_collision(name: &str, prev: ContractId, next: ContractId) -> LeafError {
    LeafError::new(ErrorKind::NoUniqueBean).caused_by(Cause::plain(
        "registering bean",
        format!("duplicate bean name `{name}`: already registered by {prev:?}, now {next:?} (enable allow_override to replace)"),
    ))
}

fn contract_collision(contract: ContractId, prev: BeanId, next: BeanId) -> LeafError {
    LeafError::new(ErrorKind::ContractCollision).caused_by(Cause::plain(
        "freezing registry",
        format!("ContractId {contract:?} collides between {prev:?} and {next:?}"),
    ))
}

fn alias_collision(alias: &str) -> LeafError {
    LeafError::new(ErrorKind::NoUniqueBean).caused_by(Cause::plain(
        "freezing registry",
        format!("alias `{alias}` is claimed by more than one bean"),
    ))
}

fn alias_shadows_name(alias: &str) -> LeafError {
    LeafError::new(ErrorKind::NoUniqueBean).caused_by(Cause::plain(
        "freezing registry",
        format!("alias `{alias}` shadows a canonical bean name"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{
        AnnotationMetadata, Role, ScopeDef, TypeRow, UpcastFn,
    };
    use crate::error::Origin;
    use crate::future::BoxFuture;
    use crate::handle::{downcast_ref, Published};
    use crate::identity::ContractId;
    use crate::provider::{Provider, ResolveCtx};

    // ── test fixtures: concrete beans + a trivial Provider ─────────────────────

    #[derive(Debug, PartialEq)]
    struct Alpha {
        n: u32,
    }
    #[derive(Debug, PartialEq)]
    struct Beta;

    // A declared injectable view (dyn-service) Alpha provides.
    trait Service: Send + Sync {}
    impl Service for Alpha {}
    impl Service for Beta {}

    fn upcast_noop(b: ErasedBean) -> ErasedBean {
        b
    }

    /// A Provider that publishes a fresh shared `Alpha { n }`, counting how often
    /// it ran (so the once-only OnceCell contract is observable).
    struct AlphaProvider {
        descriptor: Descriptor,
        n: u32,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Provider for AlphaProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            let n = self.n;
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move { Ok(Published::shared_value(Alpha { n })) })
        }
    }

    /// A no-op Provider for a `Beta` (only used to occupy a slot).
    struct BetaProvider {
        descriptor: Descriptor,
    }
    impl Provider for BetaProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(Beta)) })
        }
    }

    // Build a descriptor with a given contract path + declared name + provides.
    fn descriptor(
        contract: &str,
        self_type: TypeId,
        name: Option<&'static str>,
        provides: &'static [TypeRow],
        aliases: &'static [&'static str],
    ) -> Descriptor {
        // ContractId is computed from a runtime string here; the macro emits a
        // const. The shape is what matters for the registry.
        let _ = name; // declared_name is supplied below via the literal arg
        Descriptor {
            contract: ContractId::of(contract),
            self_type,
            provides,
            declared_name: name,
            aliases,
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("leaf-core") },
        }
    }

    fn alpha_provider(
        contract: &str,
        n: u32,
    ) -> (Descriptor, Arc<dyn Provider>, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let d = descriptor(contract, TypeId::of::<Alpha>(), Some("alpha"), &[], &[]);
        let p: Arc<dyn Provider> = Arc::new(AlphaProvider {
            descriptor: d,
            n,
            calls: calls.clone(),
        });
        (d, p, calls)
    }

    // A provides table for Alpha: it provides `dyn Service`. `TypeId::of` is not
    // const, so the row's `view` is built at runtime and the slice is leaked to
    // get a `&'static [TypeRow]` (a real macro emits a const via a static).
    fn alpha_provides_service() -> &'static [TypeRow] {
        // Leak a single-row provides table whose view is `dyn Service`.
        let row = TypeRow {
            view: TypeId::of::<dyn Service>(),
            upcast: upcast_noop as UpcastFn,
        };
        Box::leak(Box::new([row]))
    }

    // ── register + freeze + lookup by type / name / contract ───────────────────

    #[test]
    fn register_mints_dense_bean_ids_in_order() {
        let mut b = RegistryBuilder::new();
        let (d0, p0, _) = alpha_provider("crate::A", 1);
        let d1 = descriptor("crate::B", TypeId::of::<Beta>(), Some("beta"), &[], &[]);
        let p1: Arc<dyn Provider> = Arc::new(BetaProvider { descriptor: d1 });

        let id0 = b.register(d0, p0).expect("register A");
        let id1 = b.register(d1, p1).expect("register B");
        assert_eq!(id0, BeanId(0));
        assert_eq!(id1, BeanId(1));
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn freeze_then_lookup_by_type_name_and_contract() {
        let mut b = RegistryBuilder::new();
        let (d0, p0, _) = alpha_provider("crate::A", 7);
        b.register(d0, p0).expect("register");
        let reg = b.freeze().expect("freeze");

        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());

        // by type (concrete self_type): unique candidate.
        let by_type = reg
            .resolve_id(&BeanKey::ByType(TypeId::of::<Alpha>()))
            .expect("by type");
        assert_eq!(by_type, BeanId(0));

        // by name (canonical).
        let by_name = reg
            .resolve_id(&BeanKey::ByName(BeanName::from("alpha")))
            .expect("by name");
        assert_eq!(by_name, BeanId(0));

        // by contract (stable cross-build identity).
        let by_contract = reg
            .resolve_id(&BeanKey::ByContract(ContractId::of("crate::A")))
            .expect("by contract");
        assert_eq!(by_contract, BeanId(0));

        // The descriptor + provider are reachable by id.
        assert_eq!(reg.descriptor(BeanId(0)).declared_name, Some("alpha"));
        assert_eq!(reg.provider(BeanId(0)).descriptor().declared_name, Some("alpha"));
    }

    #[test]
    fn lookup_misses_are_no_such_bean() {
        let reg = RegistryBuilder::new().freeze().expect("freeze empty");
        let err = reg
            .resolve_id(&BeanKey::ByName(BeanName::from("nope")))
            .expect_err("miss");
        assert_eq!(err.kind, ErrorKind::NoSuchBean);
        let err2 = reg
            .resolve_id(&BeanKey::ByContract(ContractId::of("crate::Nope")))
            .expect_err("miss");
        assert_eq!(err2.kind, ErrorKind::NoSuchBean);
        let err3 = reg
            .resolve_id(&BeanKey::ByType(TypeId::of::<Alpha>()))
            .expect_err("miss");
        assert_eq!(err3.kind, ErrorKind::NoSuchBean);
    }

    // ── candidate set (for a dyn-Service contract) ─────────────────────────────

    #[test]
    fn candidates_include_concrete_self_type_and_declared_views() {
        let mut b = RegistryBuilder::new();

        // Two Alphas both providing `dyn Service`, plus a Beta providing it too.
        let provides = alpha_provides_service();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let d0 = descriptor("crate::A0", TypeId::of::<Alpha>(), Some("a0"), provides, &[]);
        let p0: Arc<dyn Provider> = Arc::new(AlphaProvider { descriptor: d0, n: 1, calls: calls.clone() });
        let d1 = descriptor("crate::A1", TypeId::of::<Alpha>(), Some("a1"), provides, &[]);
        let p1: Arc<dyn Provider> = Arc::new(AlphaProvider { descriptor: d1, n: 2, calls });

        let id0 = b.register(d0, p0).expect("a0");
        let id1 = b.register(d1, p1).expect("a1");
        let reg = b.freeze().expect("freeze");

        // The concrete type Alpha has TWO candidates (a0, a1) in registration order.
        let concrete = reg.candidates(TypeId::of::<Alpha>());
        assert_eq!(concrete, &[id0, id1]);

        // The `dyn Service` view also has both as candidates.
        let view = reg.candidates(TypeId::of::<dyn Service>());
        assert_eq!(view, &[id0, id1]);

        // A by-type resolve of an AMBIGUOUS type is NoUniqueBean (registry stops
        // at the candidate set; the Selector picks a winner).
        let err = reg
            .resolve_id(&BeanKey::ByType(TypeId::of::<Alpha>()))
            .expect_err("ambiguous");
        assert_eq!(err.kind, ErrorKind::NoUniqueBean);

        // ByTypeAndName narrows to the unique winner.
        let narrowed = reg
            .resolve_id(&BeanKey::ByTypeAndName(TypeId::of::<Alpha>(), BeanName::from("a1")))
            .expect("narrowed");
        assert_eq!(narrowed, id1);
    }

    #[test]
    fn candidates_for_an_unknown_type_is_empty() {
        let reg = RegistryBuilder::new().freeze().expect("freeze");
        assert!(reg.candidates(TypeId::of::<Alpha>()).is_empty());
    }

    // ── name collision is a loud error (override disabled) ─────────────────────

    #[test]
    fn duplicate_canonical_name_is_a_loud_collision() {
        let mut b = RegistryBuilder::new();
        let (d0, p0, _) = alpha_provider("crate::A", 1);
        // A second bean declaring the SAME name "alpha" (different contract).
        let d1 = descriptor("crate::B", TypeId::of::<Beta>(), Some("alpha"), &[], &[]);
        let p1: Arc<dyn Provider> = Arc::new(BetaProvider { descriptor: d1 });

        b.register(d0, p0).expect("first alpha");
        let err = b.register(d1, p1).expect_err("dup name must be loud");
        assert_eq!(err.kind, ErrorKind::NoUniqueBean);
        // The diagnostic names the colliding bean.
        assert!(err.to_string().contains("alpha"));
    }

    #[test]
    fn allow_override_replaces_the_earlier_registration() {
        let mut b = RegistryBuilder::new().allow_override(true);
        let (d0, p0, _) = alpha_provider("crate::A", 1);
        let d1 = descriptor("crate::B", TypeId::of::<Beta>(), Some("alpha"), &[], &[]);
        let p1: Arc<dyn Provider> = Arc::new(BetaProvider { descriptor: d1 });

        let id0 = b.register(d0, p0).expect("first alpha");
        let id1 = b.register(d1, p1).expect("override alpha");
        // Same slot is retained (the BeanId is stable across the override).
        assert_eq!(id0, id1);
        let reg = b.freeze().expect("freeze");
        // The winner is the LAST registration (contract B, self_type Beta).
        assert_eq!(reg.descriptor(id1).contract, ContractId::of("crate::B"));
        assert_eq!(reg.len(), 1);
    }

    // ── ContractId collision at freeze ─────────────────────────────────────────

    #[test]
    fn duplicate_contract_id_is_a_freeze_collision() {
        let mut b = RegistryBuilder::new();
        // Two beans with DIFFERENT names but the SAME ContractId.
        let d0 = descriptor("crate::Same", TypeId::of::<Alpha>(), Some("one"), &[], &[]);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p0: Arc<dyn Provider> = Arc::new(AlphaProvider { descriptor: d0, n: 1, calls });
        let d1 = descriptor("crate::Same", TypeId::of::<Beta>(), Some("two"), &[], &[]);
        let p1: Arc<dyn Provider> = Arc::new(BetaProvider { descriptor: d1 });

        b.register(d0, p0).expect("one");
        b.register(d1, p1).expect("two");
        let err = b.freeze().expect_err("contract collision");
        assert_eq!(err.kind, ErrorKind::ContractCollision);
    }

    // ── the singleton OnceCell store: get_or_init once-only ────────────────────

    #[test]
    fn singleton_cell_get_or_init_runs_exactly_once() {
        let mut b = RegistryBuilder::new();
        let (d0, p0, calls) = alpha_provider("crate::A", 42);
        let id = b.register(d0, p0).expect("register");
        let reg = b.freeze().expect("freeze");

        // The slot starts empty.
        assert!(reg.singleton_cell(id).get().is_none());

        // Drive the provider once and commit into the slot (what Engine::create
        // does). We simulate the engine's commit by calling provide + storing.
        let cx = ResolveCtx::root();
        let published =
            futures::executor::block_on(reg.provider(id).provide(&cx)).expect("provided");
        let bean = published.into_shared().expect("shared");

        let stored = reg
            .singleton_cell(id)
            .get_or_init(|| bean.clone());
        // The cell now holds the value.
        assert!(reg.singleton_cell(id).get().is_some());
        let r = downcast_ref::<Alpha>(stored.clone()).expect("downcast");
        assert_eq!(r.n, 42);

        // A second get_or_init returns the SAME cached Arc and never re-inits.
        let again = reg.singleton_cell(id).get_or_init(|| {
            panic!("must not re-init an already-initialized OnceCell");
        });
        assert!(Arc::ptr_eq(stored, again));

        // The provider ran exactly once (the engine would drive it once).
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    // ── aliases ─────────────────────────────────────────────────────────────────

    #[test]
    fn descriptor_aliases_resolve_by_name() {
        let mut b = RegistryBuilder::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let d0 = descriptor("crate::A", TypeId::of::<Alpha>(), Some("alpha"), &[], &["al", "a"]);
        let p0: Arc<dyn Provider> = Arc::new(AlphaProvider { descriptor: d0, n: 1, calls });
        let id = b.register(d0, p0).expect("register");
        let reg = b.freeze().expect("freeze");

        // Canonical name resolves.
        assert_eq!(reg.by_name("alpha"), Some(id));
        // Both aliases resolve in one hop to the same slot.
        assert_eq!(reg.by_name("al"), Some(id));
        assert_eq!(reg.by_name("a"), Some(id));
        // A non-name misses.
        assert_eq!(reg.by_name("zzz"), None);
    }

    #[test]
    fn programmatic_alias_registration_resolves() {
        let mut b = RegistryBuilder::new();
        let (d0, p0, _) = alpha_provider("crate::A", 1);
        let id = b.register(d0, p0).expect("register");
        b.alias("nick", id).expect("alias");
        let reg = b.freeze().expect("freeze");
        assert_eq!(reg.by_name("nick"), Some(id));
    }

    #[test]
    fn alias_to_unknown_target_is_an_error() {
        let mut b = RegistryBuilder::new();
        let err = b.alias("x", BeanId(99)).expect_err("bad target");
        assert_eq!(err.kind, ErrorKind::NoSuchBean);
    }

    #[test]
    fn alias_shadowing_a_canonical_name_is_a_freeze_error() {
        let mut b = RegistryBuilder::new();
        // Bean 0 canonical name "alpha"; bean 1 aliases "alpha" (a clash).
        let (d0, p0, _) = alpha_provider("crate::A", 1);
        let d1 = descriptor("crate::B", TypeId::of::<Beta>(), Some("beta"), &[], &["alpha"]);
        let p1: Arc<dyn Provider> = Arc::new(BetaProvider { descriptor: d1 });
        b.register(d0, p0).expect("a");
        b.register(d1, p1).expect("b");
        let err = b.freeze().expect_err("alias shadows canonical name");
        assert_eq!(err.kind, ErrorKind::NoUniqueBean);
    }

    #[test]
    fn two_beans_claiming_the_same_alias_is_a_freeze_error() {
        let mut b = RegistryBuilder::new();
        // Bean 0 declares alias "shared" via its descriptor; bean 1 claims the same
        // alias programmatically — two DISTINCT beans claiming one alias.
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let d0 = descriptor("crate::A", TypeId::of::<Alpha>(), Some("alpha"), &[], &["shared"]);
        let p0: Arc<dyn Provider> = Arc::new(AlphaProvider { descriptor: d0, n: 1, calls });
        let d1 = descriptor("crate::B", TypeId::of::<Beta>(), Some("beta"), &[], &[]);
        let p1: Arc<dyn Provider> = Arc::new(BetaProvider { descriptor: d1 });
        b.register(d0, p0).expect("a");
        let id1 = b.register(d1, p1).expect("b");
        b.alias("shared", id1).expect("queue clashing alias on bean 1");
        let err = b.freeze().expect_err("two beans claim `shared`");
        assert_eq!(err.kind, ErrorKind::NoUniqueBean);
    }

    // ── NULL_BEAN (extra-4) ─────────────────────────────────────────────────────

    #[test]
    fn null_bean_is_recognized_by_identity_and_type() {
        let null = NULL_BEAN.clone();
        assert!(is_null_bean(&null));
        // A freshly-minted NullMarker also reads as null (origin-agnostic).
        let fresh: ErasedBean = Arc::new(NullMarker);
        assert!(is_null_bean(&fresh));
        // A real bean is NOT null.
        let real: ErasedBean = Arc::new(Alpha { n: 1 });
        assert!(!is_null_bean(&real));
    }

    #[test]
    fn null_bean_occupies_a_real_slot_and_is_type_matchable_as_present() {
        // A present-but-null singleton: the slot holds NULL_BEAN, distinct from a
        // never-defined map-miss.
        let mut b = RegistryBuilder::new();
        let (d0, p0, _) = alpha_provider("crate::A", 1);
        let id = b.register(d0, p0).expect("register");
        let reg = b.freeze().expect("freeze");

        // Commit NULL_BEAN into the slot (a None-producing factory would).
        let stored = reg.singleton_cell(id).get_or_init(|| NULL_BEAN.clone());
        assert!(is_null_bean(stored));
        // The slot still answers candidates() by type (present, type-matchable).
        assert_eq!(reg.candidates(TypeId::of::<Alpha>()), &[id]);
        // contains() by name is true (present), distinct from a never-defined miss.
        assert!(reg.contains(&BeanKey::ByName(BeanName::from("alpha"))));
        assert!(!reg.contains(&BeanKey::ByName(BeanName::from("ghost"))));
    }

    // ── misc invariants ─────────────────────────────────────────────────────────

    #[test]
    fn ids_iterate_dense_slot_order() {
        let mut b = RegistryBuilder::new();
        let (d0, p0, _) = alpha_provider("crate::A", 1);
        let d1 = descriptor("crate::B", TypeId::of::<Beta>(), Some("beta"), &[], &[]);
        let p1: Arc<dyn Provider> = Arc::new(BetaProvider { descriptor: d1 });
        b.register(d0, p0).expect("a");
        b.register(d1, p1).expect("b");
        let reg = b.freeze().expect("freeze");
        let ids: Vec<BeanId> = reg.ids().collect();
        assert_eq!(ids, vec![BeanId(0), BeanId(1)]);
        // names() lists in insertion order.
        let names: Vec<String> = reg.names().map(|n| n.to_string()).collect();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn unnamed_bean_gets_a_unique_contract_derived_placeholder_name() {
        // Two unnamed beans must not spuriously collide on a name.
        let mut b = RegistryBuilder::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let d0 = descriptor("crate::A", TypeId::of::<Alpha>(), None, &[], &[]);
        let p0: Arc<dyn Provider> = Arc::new(AlphaProvider { descriptor: d0, n: 1, calls });
        let d1 = descriptor("crate::B", TypeId::of::<Beta>(), None, &[], &[]);
        let p1: Arc<dyn Provider> = Arc::new(BetaProvider { descriptor: d1 });
        let id0 = b.register(d0, p0).expect("a");
        let id1 = b.register(d1, p1).expect("b");
        assert_ne!(id0, id1);
        let reg = b.freeze().expect("freeze");
        assert_eq!(reg.len(), 2);
    }

    // ── parent → merged template collapse at freeze (closure 5) ───────────────

    #[test]
    fn freeze_collapses_a_child_descriptor_against_its_parent_template() {
        use crate::definition::{CandidateRole, Role};
        use crate::identity::MarkerId;

        // A parent TEMPLATE carrying qualifiers + a PRIMARY candidate_role +
        // an alias, registered as Infrastructure/PROTOTYPE.
        static PARENT_META: AnnotationMetadata = AnnotationMetadata {
            qualifiers: &[MarkerId::of("leaf::q::template")],
            markers: &[],
            depends_on: &[],
            candidate_role: CandidateRole::PRIMARY,
            autowire_candidate: true,
        };
        let parent = Descriptor {
            contract: ContractId::of("crate::AbstractRepo"),
            self_type: TypeId::of::<Beta>(),
            provides: &[],
            declared_name: Some("abstractRepo"),
            aliases: &["repoTemplate"],
            scope: ScopeDef::PROTOTYPE,
            role: Role::Infrastructure,
            meta: &PARENT_META,
            parent: None,
            origin: Origin::Native { crate_name: Some("tmpl") },
        };
        let p_parent: Arc<dyn Provider> = Arc::new(BetaProvider { descriptor: parent });

        // The CHILD declares NO meta of its own + a SINGLETON/Application scope,
        // and points at the parent template by ContractId.
        let child = Descriptor {
            contract: ContractId::of("crate::OrderRepo"),
            self_type: TypeId::of::<Alpha>(),
            provides: &[],
            declared_name: Some("orderRepo"),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: Some(ContractId::of("crate::AbstractRepo")),
            origin: Origin::Native { crate_name: Some("app") },
        };
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p_child: Arc<dyn Provider> =
            Arc::new(AlphaProvider { descriptor: child, n: 1, calls });

        let mut b = RegistryBuilder::new();
        b.register(parent, p_parent).expect("register parent");
        let child_id = b.register(child, p_child).expect("register child");
        let reg = b.freeze().expect("freeze");

        let merged = reg.descriptor(child_id);
        // Child kept its OWN scope + role.
        assert_eq!(merged.scope, ScopeDef::SINGLETON);
        assert_eq!(merged.role, Role::Application);
        // Inherited the parent template's meta (qualifiers + candidate_role).
        assert_eq!(merged.meta.qualifiers.len(), 1);
        assert_eq!(merged.meta.candidate_role, CandidateRole::PRIMARY);
        // Inherited the parent's alias (child declared none).
        assert_eq!(merged.aliases, &["repoTemplate"]);
        // The parent link is collapsed at freeze (no recompute later).
        assert!(merged.parent.is_none());
        // Identity stays the child's.
        assert_eq!(merged.contract, ContractId::of("crate::OrderRepo"));
    }

    #[test]
    fn freeze_with_a_dangling_parent_is_a_loud_error() {
        // A child naming a parent template that was never registered must NOT
        // silently keep the child unmerged — it is a loud NoSuchBean at freeze.
        let child = descriptor("crate::Child", TypeId::of::<Alpha>(), Some("child"), &[], &[]);
        let child = Descriptor { parent: Some(ContractId::of("crate::MissingParent")), ..child };
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p: Arc<dyn Provider> = Arc::new(AlphaProvider { descriptor: child, n: 1, calls });
        let mut b = RegistryBuilder::new();
        b.register(child, p).expect("register child");
        let err = b.freeze().expect_err("dangling parent");
        assert_eq!(err.kind, ErrorKind::NoSuchBean);
    }
}
