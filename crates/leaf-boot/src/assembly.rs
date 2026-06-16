//! The cold assembly pass: lift the link-collected leaf-core slices, JOIN each
//! bean `Descriptor` to its macro-emitted `ProviderSeed`, and build the
//! [`leaf_core::RegistryBuilder`] (discovery-codegen phase3/02; registry-core
//! phase3/01).
//!
//! ## The Descriptor → ProviderSeed JOIN
//!
//! The thin `#[component]`/`#[bean]`/… macros emit one const
//! [`leaf_core::Descriptor`] into the frozen [`leaf_core::COMPONENTS`] /
//! [`leaf_core::AUTO_CONFIGS`] slice, and — SEPARATELY — one public const
//! `__leaf_seed_<Ident>: ProviderSeed` beside it (the frozen `Descriptor` row
//! carries NO seed link, so the row stays a bare const). The
//! Descriptor→ProviderSeed pairing is therefore completed HERE, by leaf-boot:
//! the binary crate (`#[leaf::main]` / `build.rs`) emits a per-binary pairing
//! table of `SeedPairing { contract, seed }` — the "macro-emitted mangled pairing
//! consts" — and [`from_slices`] JOINs the link-collected descriptors to it by
//! the stable [`leaf_core::ContractId`].
//!
//! A descriptor with NO matching pairing is a LOUD error (a bean that cannot be
//! constructed must never silently vanish from the registry — the same
//! fail-loud-not-silent discipline the anti-DCE self-check enforces over whole
//! crates, applied per-bean over the seed JOIN).
//!
//! ## Ordering is NEVER read from the slice
//!
//! Link/section order is unspecified (and may be randomized), so the lift reads
//! the slices through the one [`leaf_core::collect_slice`] idiom and lets
//! [`leaf_core::RegistryBuilder::freeze`] compute the one canonical total order
//! from the stable [`leaf_core::ContractId`]. The lift here only accumulates rows.

use std::any::TypeId;
use std::collections::HashMap;

use leaf_core::{
    collect_slice, CandidateRole, Cause, CondExpr, ContractId, Descriptor, ErrorKind, LeafError,
    ProviderSeed, RegistryBuilder, AUTO_CONFIGS, AUTO_CONFIG_ORDERS, COMPONENTS, GUARD_PAIRINGS,
    SEED_PAIRINGS,
};

use crate::autoconfig::AutoConfigCandidate;

/// One macro-emitted Descriptor → ProviderSeed pairing, keyed by the bean's
/// stable [`ContractId`].
///
/// The binary crate's anti-DCE seam (`#[leaf::main]` / `build.rs`) emits one row
/// per participating bean — `SeedPairing { contract: ContractId::of("crate::Ty"),
/// seed: crate::__leaf_seed_Ty }` — and [`from_slices`] JOINs the link-collected
/// `COMPONENTS`/`AUTO_CONFIGS` descriptors against this table by `contract`.
///
/// `Copy` because the inner [`ContractId`] and [`ProviderSeed`] (a `fn` pointer)
/// are both `Copy`, so a pairing table is a plain `&[SeedPairing]` const the
/// macro can hand-write.
#[derive(Clone, Copy)]
pub struct SeedPairing {
    /// The stable cross-build identity of the bean this seed constructs (the JOIN
    /// key against the link-collected `Descriptor.contract`).
    pub contract: ContractId,
    /// The const fn-pointer that BUILDS the bean's `Provider` (the macro-emitted
    /// `__leaf_seed_<Ident>`).
    pub seed: ProviderSeed,
}

impl SeedPairing {
    /// Build one pairing from a bean's [`ContractId`] and its [`ProviderSeed`].
    #[must_use]
    pub fn new(contract: ContractId, seed: ProviderSeed) -> Self {
        SeedPairing { contract, seed }
    }
}

impl std::fmt::Debug for SeedPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The seed is a fn-pointer (not Debug); name only the contract.
        f.debug_struct("SeedPairing")
            .field("contract", &self.contract)
            .finish_non_exhaustive()
    }
}

/// The link-collected base seed layer: every macro-emitted `__leaf_seed_<Ident>`
/// auto-collected into the [`SEED_PAIRINGS`] distributed slice.
///
/// This is the SAME maximal-magic channel a user `#[component]` force-links its seed
/// through — including the framework's own force-linked beans (e.g. leaf-tokio's
/// `applicationTaskExecutor`, a `#[component(role = "infrastructure")]` whose seed
/// force-links here like any other bean). leaf-boot no longer hand-writes a special
/// `builtin_pairings` JOIN for the runtime facility: the facility's seed rides this
/// slice, so `from_slices` discovers it generically. The binary's explicit `pairings`
/// arg OVERRIDES this base on a `ContractId` collision (the escape hatch).
///
/// There is exactly ONE seed pairing per `contract` (the struct field-default recipe,
/// the `#[bean]` factory, or the `constructor = <path>` referenced-constructor recipe —
/// a bean carries one and only one). A second row for the same `contract` is therefore a
/// genuine double-emitted `__leaf_seed_*`: a loud build-seam error.
///
/// # Errors
/// A [`LeafError`] (`ErrorKind::AntiDce`) if one contract has two rows in the
/// link-collected `SEED_PAIRINGS` slice (a double-emitted seed).
fn slice_seed_index() -> Result<HashMap<ContractId, ProviderSeed>, LeafError> {
    let rows = collect_slice(&SEED_PAIRINGS);
    let mut seed_of: HashMap<ContractId, ProviderSeed> = HashMap::with_capacity(rows.len());
    for row in rows {
        if seed_of.insert(row.contract, row.seed).is_some() {
            return Err(duplicate_pairing(row.contract));
        }
    }
    Ok(seed_of)
}

/// Build the `ContractId → ProviderSeed` JOIN index from the link-collected
/// [`SEED_PAIRINGS`] base layer (every force-linked bean's macro-emitted seed) +
/// the binary's `pairings` (which win on a collision). A duplicate WITHIN the
/// binary's own `pairings` table for a contract NOT already force-linked is a loud
/// build-seam error (a double-emitted `__leaf_seed_*`).
///
/// # Errors
/// A [`LeafError`] (`ErrorKind::AntiDce`) if a contract is double-emitted into
/// `SEED_PAIRINGS`, or one contract has more than one non-base pairing in `pairings`.
fn build_seed_index(
    pairings: &[SeedPairing],
) -> Result<HashMap<ContractId, ProviderSeed>, LeafError> {
    // The force-linked base: every `SEED_PAIRINGS` row (the auto-collect channel the
    // run pipeline ALSO folds into its `self.seeds`, so an identical row here is an
    // override, never a self-dup).
    let mut seed_of = slice_seed_index()?;
    for p in pairings {
        // A duplicate pairing for one contract WITHIN the binary's own table is a
        // loud build-seam error (a force-linked base row is silently overridable, a
        // self-dup is not — it signals a double-emitted `__leaf_seed_*`).
        if seed_of.insert(p.contract, p.seed).is_some()
            && !slice_has_contract(p.contract)
        {
            return Err(duplicate_pairing(p.contract));
        }
    }
    Ok(seed_of)
}

/// Whether the link-collected [`SEED_PAIRINGS`] base layer already carries a seed for
/// `contract` (so an explicit `pairings` entry for it is an OVERRIDE, not a self-dup).
fn slice_has_contract(contract: ContractId) -> bool {
    collect_slice(&SEED_PAIRINGS)
        .iter()
        .any(|r| r.contract == contract)
}

/// Lift the link-collected [`COMPONENTS`] channel and JOIN each `Descriptor` to its
/// [`ProviderSeed`] via `pairings` (plus the link-collected [`SEED_PAIRINGS`] base
/// for every force-linked bean — including the framework's own runtime facility),
/// building the append-only [`RegistryBuilder`] (NOT yet frozen — the `App<Resolve>`
/// assembly fixpoint runs conditions/exclusions/auto-config before `seal()`).
///
/// The [`AUTO_CONFIGS`] channel is deliberately NOT registered here: an auto-config
/// is gated by the `exclude > back-off > default` ladder
/// ([`run_autoconfig`](crate::run_autoconfig)), which registers each SURVIVOR itself
/// (at `CandidateRole::FALLBACK`). Registering it here too would (a) defeat its
/// `#[conditional]` guard end-to-end and (b) trip the builder's loud double-register
/// collision guard against the ladder. The run path builds its candidate set from the
/// same `AUTO_CONFIGS` slice + the same seed/guard JOIN tables (see
/// `collect_autoconfig_candidates`).
///
/// The anti-DCE seed JOIN is STILL validated over `AUTO_CONFIGS`: an auto-config with
/// no matching `SeedPairing` is an unconstructible bean and must be loud here, exactly
/// as for a component (the ladder is the registrar, but the seed must exist so the
/// ladder can mint the bean).
///
/// # Errors
/// A [`LeafError`] (`ErrorKind::AntiDce`) if a lifted/validated `Descriptor` has no
/// matching `SeedPairing` (an unconstructible bean must be loud, never a silent skip),
/// or the builder's own loud name/collision guard fires at `register`.
pub fn from_slices(pairings: &[SeedPairing]) -> Result<RegistryBuilder, LeafError> {
    // Index the pairing table by ContractId for an O(1) per-descriptor JOIN.
    let seed_of = build_seed_index(pairings)?;

    let mut builder = RegistryBuilder::new();

    // Register the COMPONENTS channel (the unconditional user/framework beans). Read
    // through the one collect_slice idiom (never indexed by link position — the freeze
    // computes order from the stable ContractId).
    for descriptor in collect_slice(&COMPONENTS) {
        // JOIN the bare row to its construction recipe by the stable identity.
        let Some(&seed) = seed_of.get(&descriptor.contract) else {
            return Err(missing_seed(&descriptor));
        };
        // Invoke the seed ONCE (at register) to mint the stored Arc<dyn Provider>.
        builder.register(descriptor, seed())?;
    }

    // The AUTO_CONFIGS channel is gated by the ladder (NOT registered here), but its
    // anti-DCE seed JOIN is still validated: an auto-config with no SeedPairing cannot
    // be constructed by the ladder either, so surface it loud at the same seam.
    for descriptor in collect_slice(&AUTO_CONFIGS) {
        if !seed_of.contains_key(&descriptor.contract) {
            return Err(missing_seed(&descriptor));
        }
    }

    Ok(builder)
}

/// Build the auto-config candidate set from the link-collected [`AUTO_CONFIGS`]
/// channel, JOINing each `Descriptor` to its [`ProviderSeed`] (by `ContractId`, via
/// `pairings` + the link-collected [`SEED_PAIRINGS`] base) and its back-off guard (by
/// `ContractId`, from the link-collected [`GUARD_PAIRINGS`]; `None` when the auto-config
/// declares no `#[conditional]`).
///
/// This is the run path's input to [`run_autoconfig`](crate::run_autoconfig): the
/// `from_slices` lift holds the auto-configs BACK from the builder, and the ladder
/// gates them here — the same `AUTO_CONFIGS` + seed-table + guard-table JOIN sources
/// `from_slices` validates against.
///
/// # Errors
/// A [`LeafError`] (`ErrorKind::AntiDce`) if a duplicate seed pairing for one contract
/// exists (the same build-seam guard `from_slices` enforces), or an `AUTO_CONFIGS`
/// descriptor has no matching `SeedPairing` (an unconstructible candidate).
pub fn collect_autoconfig_candidates(
    pairings: &[SeedPairing],
) -> Result<Vec<AutoConfigCandidate>, LeafError> {
    let seed_of = build_seed_index(pairings)?;

    // The guard JOIN table (ContractId → const guard tree); an auto-config with no
    // `#[conditional]` has no row here (None → registers unconditionally at Fallback).
    let guard_of: HashMap<ContractId, &'static CondExpr> = collect_slice(&GUARD_PAIRINGS)
        .into_iter()
        .map(|r| (r.contract, r.guard))
        .collect();

    // The auto-config-ordering JOIN table (ContractId → OrderHint); an auto-config with
    // no explicit order (the common case) has no row here and keeps OrderHint::DEFAULT —
    // a late/early `@AutoConfigureAfter`/`@AutoConfigureBefore`-style hint declares its
    // OWN order beside its `AUTO_CONFIGS` Descriptor (never a peer's, so no type-name
    // coupling). `run_autoconfig`'s batch sort reads it via the candidate.
    let order_of: HashMap<ContractId, leaf_core::OrderHint> = collect_slice(&AUTO_CONFIG_ORDERS)
        .into_iter()
        .map(|r| (r.contract, r.order))
        .collect();

    let mut candidates = Vec::new();
    for descriptor in collect_slice(&AUTO_CONFIGS) {
        let Some(&seed) = seed_of.get(&descriptor.contract) else {
            return Err(missing_seed(&descriptor));
        };
        let guard = guard_of.get(&descriptor.contract).copied();
        let order = order_of
            .get(&descriptor.contract)
            .copied()
            .unwrap_or(leaf_core::OrderHint::DEFAULT);
        candidates.push(AutoConfigCandidate::with_order(descriptor, seed, guard, order));
    }
    Ok(candidates)
}

/// The auto-config back-off seed-probe over the link-collected [`COMPONENTS`] channel:
/// the `(self_type, candidate_role)` of every component bean `from_slices` registers,
/// so the FIRST auto-config candidate's `OnMissingBean`/`OnSingleCandidate` back-off
/// sees the user/framework beans already in the builder.
///
/// Each component contributes its concrete `self_type` AND one entry per `dyn Trait`
/// VIEW it provides (`d.provides[*].view`), so a `provides[]`-aware back-off
/// (`on_missing_bean(dyn V)`) sees a user bean of a DIFFERENTLY-named concrete type
/// that provides the view `V` (Spring's `@ConditionalOnMissingBean(Interface)` — the
/// "redis CacheManager overrides the in-memory default" case). A view entry rides the
/// SAME candidate role as its bean, so [`BuilderProbe`](crate::BuilderProbe)'s
/// primary/fallback verdict is identical whether probed by concrete type or by view.
///
/// The auto-configs themselves are NOT in this probe (the ladder grows the probe
/// incrementally as each survivor registers — see [`run_autoconfig`](crate::run_autoconfig)).
#[must_use]
pub fn component_seed_probe() -> Vec<(TypeId, CandidateRole)> {
    let mut out = Vec::new();
    for d in collect_slice(&COMPONENTS) {
        out.push((d.self_type, d.meta.candidate_role));
        for row in d.provides {
            out.push((row.view, d.meta.candidate_role));
        }
    }
    out
}

fn missing_seed(descriptor: &Descriptor) -> LeafError {
    let name = descriptor.declared_name.unwrap_or("<unnamed>");
    LeafError::new(ErrorKind::AntiDce).caused_by(Cause::plain(
        "joining bean to its provider seed",
        format!(
            "the COMPONENTS row `{name}` ({:?}) has no matching SeedPairing — its \
             `ProviderSeed` was not emitted into the binary's pairing table (a \
             dropped or unregistered `__leaf_seed_*` const). The bean cannot be \
             constructed.",
            descriptor.contract
        ),
    ))
}

fn duplicate_pairing(contract: ContractId) -> LeafError {
    LeafError::new(ErrorKind::AntiDce).caused_by(Cause::plain(
        "building the seed pairing table",
        format!("ContractId {contract:?} has more than one SeedPairing"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;
    use std::sync::Arc;

    use leaf_core::{
        AnnotationMetadata, BoxFuture, Origin, Provider, Published, ResolveCtx, Role, ScopeDef,
    };

    // ── a hand-built bean + provider that the registered seeds construct ────────
    //
    // We cannot submit a real const Descriptor to COMPONENTS from this unit test
    // (TypeId::of is not a stable const fn, so the macro builds the row at the use
    // site). The from_slices JOIN over the REAL link-collected slice is proven by
    // the `tests/from_slices.rs` integration test with a genuine #[component].
    // Here we unit-test the JOIN + error shapes over the pure helpers.

    #[derive(Debug)]
    struct Probe;

    struct ProbeProvider {
        descriptor: Descriptor,
    }
    impl Provider for ProbeProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(Probe)) })
        }
    }

    fn probe_descriptor() -> Descriptor {
        Descriptor {
            contract: ContractId::of("test::Probe"),
            self_type: TypeId::of::<Probe>(),
            provides: &[],
            declared_name: Some("probe"),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("leaf-boot") },
        }
    }

    fn probe_seed() -> Arc<dyn Provider> {
        Arc::new(ProbeProvider { descriptor: probe_descriptor() })
    }

    #[test]
    fn seed_pairing_carries_a_contract_and_a_seed() {
        let p = SeedPairing::new(ContractId::of("test::Probe"), probe_seed);
        assert_eq!(p.contract, ContractId::of("test::Probe"));
        // Debug names only the contract (the seed is a fn-pointer).
        assert!(format!("{p:?}").contains("SeedPairing"));
    }

    #[test]
    fn a_duplicate_pairing_for_one_contract_is_loud() {
        let dup = vec![
            SeedPairing::new(ContractId::of("test::Probe"), probe_seed),
            SeedPairing::new(ContractId::of("test::Probe"), probe_seed),
        ];
        let err = from_slices(&dup).expect_err("two pairings for one contract is loud");
        assert_eq!(err.kind, ErrorKind::AntiDce);
        assert!(err.to_string().contains("more than one"), "got: {err}");
    }

    #[test]
    fn lifting_registers_only_the_components_channel() {
        // The lift registers ONLY the COMPONENTS channel through the one collect_slice
        // idiom (the AUTO_CONFIGS channel is held back for the ladder to gate); with
        // the link-collected SEED_PAIRINGS base covering every force-linked bean's seed
        // (including leaf-tokio's applicationTaskExecutor under the default `tokio`
        // feature), the bare lift succeeds and the builder holds exactly one row per
        // COMPONENTS descriptor — NOT the auto-configs.
        let builder = from_slices(&[]).expect("the bare lift succeeds via the SEED_PAIRINGS base");
        assert_eq!(builder.len(), COMPONENTS.len());
    }

    #[test]
    fn collect_candidates_is_total_over_the_auto_configs_channel() {
        // The run path builds one AutoConfigCandidate per AUTO_CONFIGS row (held back
        // from from_slices); with the SEED_PAIRINGS base covering the force-linked
        // framework beans, the bare collect succeeds and yields one candidate per row.
        let cands =
            collect_autoconfig_candidates(&[]).expect("collect succeeds via the SEED_PAIRINGS base");
        assert_eq!(cands.len(), AUTO_CONFIGS.len());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn the_force_linked_seed_joins_leaf_tokios_executor() {
        // leaf-boot force-links leaf-tokio by default, so its applicationTaskExecutor
        // `#[component(role = "infrastructure")]` Descriptor is link-collected into
        // COMPONENTS AND its macro-emitted seed force-links into SEED_PAIRINGS — the
        // SAME maximal-magic channel a user component uses. from_slices JOINs it from
        // the slice with NO binary-supplied entry + NO hand-written builtin pairing.
        // The lifted builder freezes + the executor resolves by its stable contract.
        let registry = from_slices(&[]).expect("lift").freeze().expect("freeze");
        let id = registry
            .by_contract(ContractId::of(leaf_tokio::APPLICATION_TASK_EXECUTOR_CONTRACT))
            .expect("the force-linked executor is registered + JOINed to its slice seed");
        assert_eq!(
            registry.descriptor(id).declared_name,
            Some("applicationTaskExecutor")
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn leaf_tokios_executor_seed_force_links_into_seed_pairings() {
        // The proof the special case is GONE: the facility's seed is now in the
        // link-collected SEED_PAIRINGS slice (force-linked by the macro), not a
        // hand-written leaf-boot builtin pairing. So from_slices JOINs it like any
        // user bean.
        let contract = ContractId::of(leaf_tokio::APPLICATION_TASK_EXECUTOR_CONTRACT);
        assert!(
            collect_slice(&SEED_PAIRINGS).iter().any(|r| r.contract == contract),
            "the applicationTaskExecutor seed must force-link into SEED_PAIRINGS"
        );
    }

    #[test]
    fn missing_seed_error_names_the_bean_and_is_anti_dce() {
        // The pure error constructor names the bean + carries ErrorKind::AntiDce.
        let err = missing_seed(&probe_descriptor());
        assert_eq!(err.kind, ErrorKind::AntiDce);
        assert!(err.to_string().contains("probe"), "got: {err}");
        assert!(err.to_string().contains("cannot be constructed"), "got: {err}");
    }
}
