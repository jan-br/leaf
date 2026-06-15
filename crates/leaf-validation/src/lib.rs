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
//! ## The constraint derive + the config-bind validation hook (WIRED)
//!
//! - The `#[derive(Validate)]` derive (leaf-macros / `leaf_codegen::validate`) emits
//!   the per-field constraint calls into a `ValidateInto` impl — the SAME cascade a
//!   hand `impl ValidateInto` writes (a type now `#[derive(Validate)]`s its
//!   `#[validate(not_empty)]` / `#[validate(min = N)]` / … fields rather than hand-
//!   writing the [`Cascade`] calls). The derive's emitted code names this crate via
//!   ABSOLUTE `::leaf_validation::` paths (leaf-codegen depends only on leaf-core).
//! - The `#[config_properties(prefix = .., validate)]` C2 bind thunk now CALLS
//!   [`validate_config`] over the bound value (opt-in via the bare `validate` flag):
//!   on a JSR violation it short-circuits with the aggregated `ValidationError` as a
//!   bind fault, validating BOTH the bound value and the JavaBean default arm. An
//!   unflagged config bean is unaffected (the stock
//!   [`NoopBindHandler`](leaf_core::NoopBindHandler) bind, no validation). The kernel
//!   binder hands a [`BindHandler`](leaf_core::BindHandler) the bound NAME but NOT the
//!   typed value, so the typed validation runs on the bound value via
//!   [`validate_config`], not through the observer alone.
//!
//! ## Deferred (honest NOTEs)
//!
//! - The `#[validated]` METHOD attribute (the per-method validation ADVISOR) is wired
//!   separately by the `#[advisable]` impl-block macro (it lowers `#[validated]` to a
//!   const [`AdvisorPairingRow`](leaf_core::AdvisorPairingRow) binding the
//!   per-method [`ArgValidator`] over the `@Valid` arg via
//!   [`single_arg_make_interceptor`] — see the integration test). The
//!   `#[derive(Validate)]` constraint-derive (above) supplies the `ValidateInto` that
//!   validator runs.
//! - Multi-`@Valid`-argument method validation: [`single_arg_make_interceptor`]
//!   covers the one-`@Valid`-arg method; a multi-arg method writes a hand-written
//!   [`ArgValidator`] over its concrete arg tuple. Groups
//!   / group-sequences are deliberately dropped in v1 (phase3/09).

#![deny(unsafe_code)]
#![warn(missing_docs)]

// A self-alias so the `#[derive(Validate)]` macro's ABSOLUTE `::leaf_validation::`
// emitted paths (the thin-macro path rule — leaf-codegen depends only on leaf-core,
// never on this crate, so the derive cannot name `crate`) resolve INSIDE this crate's
// own `#[cfg(test)]` modules too. Without it `::leaf_validation::ValidateInto` would
// fail to resolve when the derive is used in `src/cascade.rs` unit tests.
extern crate self as leaf_validation;

pub mod advisor;
pub mod bind_handler;
pub mod cascade;
pub mod constraints;
pub mod interceptor;
pub mod violations;

// The per-crate anti-DCE SOURCE anchor (ADR-09 Defense MANIFEST): one SourceTag in
// the link-collected `SOURCES` slice so a binary that lists leaf-validation in its
// ExpectedManifest (the `web` capability bundle) can tell "linked-but-zero-rows"
// from "never-linked" — a loud `SourceVanished` rather than a silent missing
// concern. The package name (dashes) is the string the ExpectedManifest joins on.
leaf_core::declare_source!("leaf-validation");

pub use advisor::{
    enable_validation, single_arg_make_interceptor, validation_advisor_contract,
    validation_advisor_pairing, validation_order_key, ValidationPointcut,
};
pub use bind_handler::{
    canonical_key, validate_config, validate_config_dyn, ValidationBindHandler,
};
pub use cascade::{addr_of, validate_root, AsValidate, Cascade, ValidateInto, VisitedSet};
pub use interceptor::{arg_validator, ArgValidator, MethodValidationInterceptor};
pub use violations::{aggregate, has_violations, render_violation};
