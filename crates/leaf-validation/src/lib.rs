//! `leaf-validation` — the bean/method validation cross-cutting concern crate
//! (declarative-advice, phase3/09 §validation): it SHIPS the runtime
//! [`MethodValidationInterceptor`] (the OUTERMOST around-advice that validates
//! `@Valid` args before the call proceeds) AND the Infrastructure validation advisor
//! that auto-wires through the run pipeline — PLUS the binder-side
//! [`ValidationBindHandler`] adapter (the `@ConfigurationProperties` JSR validation
//! half of the leaf-config/leaf-boot C2 path).
//!
//! TWO faces over ONE engine (phase3/09: "one engine, never two"):
//!
//! - **ENGINE** — the compiled-per-type constraint checkers ([`constraints`]:
//!   `range`/`min`/`max`/`not_empty`/`email`/`pattern`) + the recursive cascade
//!   driver ([`cascade`]: [`Cascade`]/[`validate_root`]) with a [`VisitedSet`] cycle
//!   guard for the `@Valid` cascade. Constraints travel WITH the type (a
//!   [`ValidateInto`] impl — the `#[derive(Validate)]` macro is deferred, a NOTE
//!   below), never a central registry — "zero link-time collection, zero DCE hazard".
//! - **FACE 2 (method validation)** — the [`MethodValidationInterceptor`]
//!   ([`interceptor`]) + the Infrastructure [`AdvisorPairingRow`](leaf_core::AdvisorPairingRow)
//!   ([`advisor`]) at [`VALIDATE_ORDER`](leaf_core::VALIDATE_ORDER) (OUTERMOST) that
//!   validates `@Valid` args and SHORT-CIRCUITS with aggregated
//!   [`Violation`](leaf_core::Violation)s as a `ValidationError`
//!   [`LeafError`](leaf_core::LeafError) before the real method (or any inner
//!   advisor) runs.
//! - **FACE 3 (config binding)** — the binder-side [`ValidationBindHandler`] +
//!   [`validate_config`] ([`bind_handler`]) that run the SAME engine after a
//!   `@ConfigurationProperties` bind, mapping each violation path to the canonical
//!   property KEY and aggregating into the C2 bind fault.
//!
//! All three faces share the ONE leaf-core [`ValidationContext`](leaf_core::ValidationContext)
//! collect-all accumulator + the ONE [`LeafError`](leaf_core::LeafError) spine
//! (messages stay UNRESOLVED — `message_key` + params resolve LATER at error-render
//! time against messages-i18n + the ambient locale; the sync validate path never
//! touches a bean).
//!
//! ## Deferred (honest NOTEs)
//!
//! - The `#[derive(Validate)]` derive + the `#[validated]` STRUCT/METHOD attribute
//!   macro (which would emit the per-field constraint calls, the per-method
//!   [`ArgValidator`](interceptor::ArgValidator) over the `@Valid` args, and the
//!   VALIDATE marker the pointcut keys on) are NOT in leaf-macros/leaf-codegen yet.
//!   Until they land: a type writes a hand `impl ValidateInto` calling the
//!   [`constraints`] fns through [`Cascade`], and a binding site supplies the
//!   per-method validator via [`single_arg_make_interceptor`] / a const
//!   [`AdvisorPairingRow`](leaf_core::AdvisorPairingRow) row (the established leaf-tx
//!   `#[transactional]` precedent — see the integration test).
//! - The `#[config_properties]` C2 bind thunk (leaf-codegen) currently runs the stock
//!   [`NoopBindHandler`](leaf_core::NoopBindHandler); the JSR validation is THIS
//!   force-link's concern (per the leaf-codegen NOTE). [`validate_config`] is the
//!   adapter a thunk (or the integration test) calls on the bound value — the
//!   kernel binder hands a [`BindHandler`](leaf_core::BindHandler) the bound NAME but
//!   NOT the typed value, so the typed validation runs on the bound value, not
//!   through the observer alone.
//! - Multi-`@Valid`-argument method validation: [`single_arg_make_interceptor`]
//!   covers the one-`@Valid`-arg method; a multi-arg method writes a hand-written
//!   [`ArgValidator`](interceptor::ArgValidator) over its concrete arg tuple. Groups
//!   / group-sequences are deliberately dropped in v1 (phase3/09).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod advisor;
pub mod bind_handler;
pub mod cascade;
pub mod constraints;
pub mod interceptor;
pub mod violations;

pub use advisor::{
    enable_validation, single_arg_make_interceptor, validation_advisor_contract,
    validation_advisor_pairing, validation_marker, validation_order_key, ValidationPointcut,
    VALIDATED_MARKER_POINTCUT,
};
pub use bind_handler::{
    canonical_key, validate_config, validate_config_dyn, ValidationBindHandler,
};
pub use cascade::{addr_of, validate_root, AsValidate, Cascade, ValidateInto, VisitedSet};
pub use interceptor::{arg_validator, ArgValidator, MethodValidationInterceptor};
pub use violations::{aggregate, has_violations, render_violation};
