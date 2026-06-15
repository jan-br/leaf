//! trybuild compile-fail tests for the thin macros' Tier-0 hard errors.
//!
//! These assert the COMPILE-FAIL diagnostics (the `compile_error!`s the macros emit
//! on a malformed/unsupported target) — the half of the macro surface an in-process
//! integration test cannot reach.

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    // A generic component is the headline hard error: a generic type has no single
    // concrete TypeId/ContractId, so it cannot be a const row. The macro emits a
    // `compile_error!` carrying the `register_component!(Concrete)` remediation.
    t.compile_fail("tests/ui/generic_component_is_a_hard_error.rs");
    // A generic @bean factory has no single concrete product type — same hard error.
    t.compile_fail("tests/ui/generic_bean_factory_is_a_hard_error.rs");
    // The @bean-on-a-method (`self` receiver) form is deferred in v1 — a loud error.
    t.compile_fail("tests/ui/bean_with_self_receiver_is_deferred.rs");
    // The stereotype attribute schema is closed (`name`/`scope` only).
    t.compile_fail("tests/ui/unknown_stereotype_arg_is_a_hard_error.rs");
    // The condition DSL vocabulary is closed — an unknown leaf kind hard-errors.
    t.compile_fail("tests/ui/conditional_unknown_kind_is_a_hard_error.rs");
    // Mixing `&`/`|` without parens in a #[profile] expr is a Tier-0 hard error.
    t.compile_fail("tests/ui/profile_mixed_operators_is_a_hard_error.rs");
    // A generic #[config_properties] target has no single concrete bind schema.
    t.compile_fail("tests/ui/config_properties_generic_is_a_hard_error.rs");
    // A generic #[aspect] has no single concrete ContractId — register_proxy! hint.
    t.compile_fail("tests/ui/generic_aspect_is_a_hard_error.rs");
    // #[scheduled] requires exactly one of cron/fixed_rate/fixed_delay.
    t.compile_fail("tests/ui/scheduled_requires_a_trigger.rs");
    // #[cacheable] requires at least one cache name.
    t.compile_fail("tests/ui/cacheable_requires_a_name.rs");
    // The #[advice] kind vocabulary is closed — an unknown keyword hard-errors.
    t.compile_fail("tests/ui/advice_unknown_kind_is_a_hard_error.rs");
}
