//! The representative integration-crate contract, end to end: drive the REAL
//! leaf-boot `run_autoconfig` ladder over `leaf-redis`'s hand-emitted `AUTO_CONFIGS`
//! row + `CondExpr` guard, and assert it (a) registers the Redis cache manager at
//! `CandidateRole::FALLBACK` when enabled-and-unclaimed, (b) backs off when the
//! `OnProperty` gate is unset, (c) backs off when a user `CacheManager` of the
//! contributed type already exists, and (d) is removed by an exclusion.
//!
//! This needs NO live Redis: `run_autoconfig` invokes the seed (which opens a LAZY
//! `redis::Client` — URL validation only, no socket) and evaluates the guard over
//! the sealed `Env` + the growing definition set. The condition impls resolve through
//! the `CONDITIONS` catalog leaf-conditions force-links (the dev-dep here).

use std::any::TypeId;
use std::sync::Arc;

use leaf_boot::{run_autoconfig, AutoConfigCandidate, ExclusionSet};
use leaf_core::{
    ActiveProfiles, CandidateRole, ConditionReportClass, ContractId, Env, EnvBuilder,
    MapPropertySource, RegistryBuilder,
};

use leaf_redis::{
    redis_cache_manager_descriptor, RedisCacheManager, REDIS_AUTO_CONFIG_GUARD,
    REDIS_CACHE_MANAGER_CONTRACT, REDIS_CACHE_MANAGER_SEED,
};

/// Build the one Redis auto-config candidate from the crate's macro-emitted public
/// artifacts (the contributed `AUTO_CONFIGS` descriptor + its seed + its guard) —
/// exactly the JOIN leaf-boot performs over the slices.
fn redis_candidate() -> AutoConfigCandidate {
    AutoConfigCandidate::new(
        redis_cache_manager_descriptor(),
        REDIS_CACHE_MANAGER_SEED,
        Some(&REDIS_AUTO_CONFIG_GUARD),
    )
}

fn env_with(pairs: &[(&str, &str)]) -> Env {
    let src = MapPropertySource::from_pairs(
        "test",
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
    );
    let mut b = EnvBuilder::new();
    b.add_last(Arc::new(src));
    b.seal_env()
}

fn run(
    env: &Env,
    exclusions: &ExclusionSet,
    seed_probe: &[(TypeId, CandidateRole)],
) -> (usize, RegistryBuilder, leaf_core::ConditionReport) {
    let mut builder = RegistryBuilder::new();
    let cands = [redis_candidate()];
    let out = run_autoconfig(
        &cands,
        env,
        &mut builder,
        exclusions,
        &ActiveProfiles::default(),
        seed_probe,
    )
    .expect("the ladder runs");
    (out.registered, builder, out.report)
}

#[test]
fn registers_the_redis_cache_manager_at_fallback_when_enabled_and_unclaimed() {
    // leaf.redis.enabled present + no user CacheManager → the guard matches → the
    // auto-config registers its Arc<dyn CacheManager> at FALLBACK.
    let (registered, builder, _report) =
        run(&env_with(&[("leaf.redis.enabled", "true")]), &ExclusionSet::new(), &[]);
    assert_eq!(registered, 1, "enabled + unclaimed → the Redis cache manager wires");
    assert_eq!(builder.len(), 1);
}

#[test]
fn the_registered_bean_carries_the_fallback_soft_override_role() {
    // The contributed descriptor is FALLBACK so a user bean transparently supersedes.
    assert_eq!(redis_cache_manager_descriptor().meta.candidate_role, CandidateRole::FALLBACK);
}

#[test]
fn backs_off_when_the_enable_property_is_unset() {
    // No leaf.redis.enabled → the OnProperty leaf fails (present-and-not-false) → the
    // whole guard is Negative → nothing registers.
    let (registered, builder, report) = run(&env_with(&[]), &ExclusionSet::new(), &[]);
    assert_eq!(registered, 0, "absent enable property → the auto-config backs off");
    assert_eq!(builder.len(), 0);
    let rec = report.lookup(ContractId::of(REDIS_CACHE_MANAGER_CONTRACT)).expect("a verdict");
    assert!(
        matches!(rec.class, ConditionReportClass::Negative(_)),
        "the back-off is recorded Negative, got {:?}",
        rec.class
    );
}

#[test]
fn explicitly_disabled_property_backs_off() {
    // leaf.redis.enabled=false → present-but-false → OnProperty fails → backs off.
    let (registered, _builder, _report) =
        run(&env_with(&[("leaf.redis.enabled", "false")]), &ExclusionSet::new(), &[]);
    assert_eq!(registered, 0, "enabled=false backs off");
}

#[test]
fn a_user_cache_manager_of_the_contributed_type_supersedes_the_auto_config() {
    // A user RedisCacheManager bean is registered, PROVIDING the dyn CacheManager view
    // (the production `component_seed_probe` lifts each bean's self_type + provides[]
    // views — here both the concrete type and the view). The guard's
    // OnMissingBean(dyn CacheManager) then sees the view and the auto-config backs off.
    let (registered, builder, report) = run(
        &env_with(&[("leaf.redis.enabled", "true")]),
        &ExclusionSet::new(),
        &[
            (TypeId::of::<RedisCacheManager>(), CandidateRole::NORMAL),
            (TypeId::of::<dyn leaf_core::CacheManager>(), CandidateRole::NORMAL),
        ],
    );
    assert_eq!(registered, 0, "OnMissingBean backs off: the user CacheManager supersedes");
    assert_eq!(builder.len(), 0);
    let rec = report.lookup(ContractId::of(REDIS_CACHE_MANAGER_CONTRACT)).expect("a verdict");
    assert!(matches!(rec.class, ConditionReportClass::Negative(_)));
}

#[test]
fn a_user_cache_manager_of_a_different_concrete_type_supersedes_via_the_dyn_view() {
    // The headline provides[]-aware back-off: a user CacheManager of a DIFFERENT
    // concrete type (NOT RedisCacheManager) — but providing the dyn CacheManager view —
    // makes the Redis default back off. The probe matches on the VIEW, not self_type.
    struct UserCacheManager;
    let (registered, builder, report) = run(
        &env_with(&[("leaf.redis.enabled", "true")]),
        &ExclusionSet::new(),
        // Only the dyn-view is seeded (the user bean's own self_type is unrelated to
        // RedisCacheManager) — yet the back-off still fires.
        &[
            (TypeId::of::<UserCacheManager>(), CandidateRole::NORMAL),
            (TypeId::of::<dyn leaf_core::CacheManager>(), CandidateRole::NORMAL),
        ],
    );
    assert_eq!(
        registered, 0,
        "a differently-typed CacheManager providing the dyn view supersedes the default"
    );
    assert_eq!(builder.len(), 0);
    let rec = report.lookup(ContractId::of(REDIS_CACHE_MANAGER_CONTRACT)).expect("a verdict");
    assert!(matches!(rec.class, ConditionReportClass::Negative(_)));
}

#[test]
fn an_exclusion_removes_the_candidate_before_back_off() {
    // Excluding the contract mints no bean and never enters back-off (records Exclusion).
    let mut excl = ExclusionSet::new();
    excl.insert(ContractId::of(REDIS_CACHE_MANAGER_CONTRACT));
    let (registered, builder, report) =
        run(&env_with(&[("leaf.redis.enabled", "true")]), &excl, &[]);
    assert_eq!(registered, 0, "an excluded auto-config mints no bean");
    assert_eq!(builder.len(), 0);
    let rec = report.lookup(ContractId::of(REDIS_CACHE_MANAGER_CONTRACT)).expect("a verdict");
    assert!(matches!(rec.class, ConditionReportClass::Exclusion(_)));
}

#[test]
fn the_env_exclude_list_also_removes_the_candidate() {
    // The relaxed leaf.autoconfigure.exclude list names the contract by string.
    let env = env_with(&[
        ("leaf.redis.enabled", "true"),
        ("leaf.autoconfigure.exclude", REDIS_CACHE_MANAGER_CONTRACT),
    ]);
    let excl = ExclusionSet::merge(&[], &[], &env);
    let (registered, _builder, _report) = run(&env, &excl, &[]);
    assert_eq!(registered, 0, "the env exclude list removes the candidate");
}

#[test]
fn the_kill_switch_skips_the_whole_batch() {
    // leaf.enable-autoconfiguration=false short-circuits before exclusions/back-off.
    let env =
        env_with(&[("leaf.redis.enabled", "true"), ("leaf.enable-autoconfiguration", "false")]);
    let (registered, builder, report) = run(&env, &ExclusionSet::new(), &[]);
    assert_eq!(registered, 0, "the kill-switch skips the whole auto-config batch");
    assert_eq!(builder.len(), 0);
    assert!(report.is_empty(), "no verdict rows recorded when the batch is killed");
}
