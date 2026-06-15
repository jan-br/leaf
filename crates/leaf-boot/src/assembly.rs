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

use std::collections::HashMap;

use leaf_core::{
    collect_slice, Cause, ContractId, Descriptor, ErrorKind, LeafError, ProviderSeed,
    RegistryBuilder, AUTO_CONFIGS, COMPONENTS,
};

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

/// The built-in framework pairings leaf-boot ALWAYS knows about — the seeds for
/// the beans leaf-boot itself force-links.
///
/// leaf-boot force-links leaf-tokio by default (per TOPOLOGY), so leaf-tokio's
/// `applicationTaskExecutor` `Descriptor` lands in [`COMPONENTS`]; leaf-boot owns
/// the JOIN to leaf-tokio's [`ProviderSeed`] (`APPLICATION_TASK_EXECUTOR_SEED`) so
/// the binary author never hand-writes the framework's own seed. Empty when the
/// `tokio` feature is off (the embedder brings its own runtime + pairings).
#[must_use]
fn builtin_pairings() -> Vec<SeedPairing> {
    #[cfg(feature = "tokio")]
    {
        vec![SeedPairing::new(
            ContractId::of(leaf_tokio::APPLICATION_TASK_EXECUTOR_CONTRACT),
            leaf_tokio::APPLICATION_TASK_EXECUTOR_SEED,
        )]
    }
    #[cfg(not(feature = "tokio"))]
    {
        Vec::new()
    }
}

/// Lift the link-collected bean channels ([`COMPONENTS`] + [`AUTO_CONFIGS`]) and
/// JOIN each `Descriptor` to its [`ProviderSeed`] via `pairings` (plus leaf-boot's
/// `builtin_pairings` for the framework beans it force-links), building the
/// append-only [`RegistryBuilder`] (NOT yet frozen — the `App<Resolve>` assembly
/// fixpoint runs conditions/exclusions/registrars before `seal()`).
///
/// Both bean channels carry the identical const `Descriptor` shape (an auto-config
/// differs only in the channel + its `CandidateRole::FALLBACK`), so both lift
/// through the same JOIN.
///
/// # Errors
/// A [`LeafError`] (`ErrorKind::AntiDce`) if a lifted `Descriptor` has no matching
/// `SeedPairing` (an unconstructible bean must be loud, never a silent skip), or
/// the builder's own loud name/collision guard fires at `register`.
pub fn from_slices(pairings: &[SeedPairing]) -> Result<RegistryBuilder, LeafError> {
    // Index the pairing table by ContractId for an O(1) per-descriptor JOIN.
    // leaf-boot's built-in framework pairings seed the table first; a binary's
    // explicit `pairings` win on a contract collision (they override).
    let builtins = builtin_pairings();
    let mut seed_of: HashMap<ContractId, ProviderSeed> =
        HashMap::with_capacity(pairings.len() + builtins.len());
    for p in &builtins {
        seed_of.insert(p.contract, p.seed);
    }
    for p in pairings {
        // A duplicate pairing for one contract WITHIN the binary's own table is a
        // loud build-seam error (a built-in is silently overridable, a self-dup is
        // not — it signals a double-emitted `__leaf_seed_*`).
        if seed_of.insert(p.contract, p.seed).is_some()
            && !builtins.iter().any(|b| b.contract == p.contract)
        {
            return Err(duplicate_pairing(p.contract));
        }
    }

    let mut builder = RegistryBuilder::new();

    // Lift BOTH bean channels through the one read idiom (never indexed by link
    // position — the freeze computes order from the stable ContractId).
    let components: Vec<Descriptor> = collect_slice(&COMPONENTS);
    let auto_configs: Vec<Descriptor> = collect_slice(&AUTO_CONFIGS);

    for descriptor in components.into_iter().chain(auto_configs) {
        // JOIN the bare row to its construction recipe by the stable identity.
        let Some(&seed) = seed_of.get(&descriptor.contract) else {
            return Err(missing_seed(&descriptor));
        };
        // Invoke the seed ONCE (at register) to mint the stored Arc<dyn Provider>.
        builder.register(descriptor, seed())?;
    }

    Ok(builder)
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
    fn lifting_is_total_over_the_link_collected_bean_channels() {
        // The lift reads EVERY row in COMPONENTS + AUTO_CONFIGS through the one
        // collect_slice idiom; with leaf-boot's built-in pairings covering the
        // framework beans it force-links (leaf-tokio's applicationTaskExecutor
        // under the default `tokio` feature), the bare lift succeeds and the
        // builder holds exactly one row per link-collected descriptor.
        let builder = from_slices(&[]).expect("the bare lift succeeds via built-in pairings");
        assert_eq!(builder.len(), COMPONENTS.len() + AUTO_CONFIGS.len());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn the_builtin_pairing_joins_leaf_tokios_force_linked_executor() {
        // leaf-boot force-links leaf-tokio by default, so its applicationTaskExecutor
        // Descriptor is link-collected; leaf-boot's built-in pairing JOINs it to
        // leaf-tokio's ProviderSeed with no binary-supplied entry. The lifted
        // builder freezes + the executor resolves by its stable contract.
        let registry = from_slices(&[]).expect("lift").freeze().expect("freeze");
        let id = registry
            .by_contract(ContractId::of(leaf_tokio::APPLICATION_TASK_EXECUTOR_CONTRACT))
            .expect("the force-linked executor is registered + JOINed to its seed");
        assert_eq!(
            registry.descriptor(id).declared_name,
            Some("applicationTaskExecutor")
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
