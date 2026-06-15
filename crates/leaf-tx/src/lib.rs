//! `leaf-tx` — the transaction-management cross-cutting concern crate
//! (declarative-advice, phase3/09): it SHIPS the runtime
//! [`TransactionInterceptor`] (around advice — begin/commit/rollback on a
//! container-resolved [`TransactionManager`](leaf_core::TransactionManager)) AND
//! the Infrastructure tx advisor that auto-wires through the run pipeline.
//!
//! The pieces (all resting on the leaf-core ABI — nothing minted twice):
//!
//! - **[`TransactionInterceptor`]** — the `Role::Infrastructure`, `TX_ORDER`
//!   around-advice. It reads the resolved [`TxAttribute`](leaf_core::TxAttribute),
//!   resolves the [`TxAction`](propagation::TxAction) from the propagation + the
//!   ambient [`TxResourceKey`](leaf_core::TxResourceKey), `begin`s when it owns the
//!   tx, INSTALLs the [`TxState`](leaf_core::TxState) on the ambient
//!   [`Cx`](leaf_core::Cx) (re-installed per poll via
//!   [`Scoped`](leaf_core::Scoped) so it survives work-stealing and is
//!   never inherited across a spawn — `POLICY = Isolate`), runs the body, then on
//!   `Ok` COMMITs (firing BEFORE_COMMIT then AFTER_COMMIT syncs), on a rollback-rule
//!   `Err` ROLLs BACK (firing AFTER_ROLLBACK), and ALWAYS fires AFTER_COMPLETION.
//! - **propagation** ([`propagation`]) — `REQUIRED`/`REQUIRES_NEW`/`SUPPORTS`/
//!   `MANDATORY`/`NEVER`/`NOT_SUPPORTED`/`NESTED` resolved to a [`TxAction`](propagation::TxAction).
//! - **rollback rules** ([`rollback`]) — `should_rollback` over the
//!   [`ErrorMatch`](leaf_core::ErrorMatch) rules + the typed
//!   [`ErrorKind`](leaf_core::ErrorKind) (default: any `Err` rolls back unless a
//!   `no_rollback_on` carve-out matches).
//! - **synchronizations** ([`manager::TxSync`]) — the concrete per-Cx-thread-of-
//!   execution per-phase callback storage the kernel
//!   [`TxSyncRegistry`](leaf_core::TxSyncRegistry) seam delegates to; it rides the
//!   manager-minted [`TxState`] so the SAME registry serves the interceptor AND a
//!   `#[transactional_event_listener]` (which registers AFTER_COMMIT/AFTER_ROLLBACK
//!   callbacks via [`register_synchronization`]).
//! - **manager** ([`InMemoryTransactionManager`]) — the no-op/in-memory manager for
//!   tests + the bare engine (a real datastore manager is an ordinary bean).
//! - **advisor** ([`advisor`]) — the Infrastructure [`AdvisorPairingRow`](leaf_core::AdvisorPairingRow)
//!   builders ([`tx_advisor_pairing`]/[`make_transaction_interceptor`]) +
//!   [`TxPointcut`] that auto-wire the advisor through `Application::run`'s
//!   `ADVISOR_PAIRINGS` collection.
//!
//! ## Deferred (honest NOTEs)
//!
//! - The `#[transactional]` STRUCT/METHOD attribute macro (which would emit the
//!   per-method const [`TxAttribute`](leaf_core::TxAttribute) + the TX marker the
//!   pointcut keys on) is NOT in leaf-macros; until it lands the auto-wire row
//!   applies [`TxAttribute::DEFAULT`](leaf_core::TxAttribute::DEFAULT) uniformly and
//!   a binding site supplies a non-default attribute via
//!   [`TransactionInterceptor::new`]. (`#[transactional_event_listener]` IS in
//!   leaf-macros and fires through [`register_synchronization`].)
//! - The SIGTERM-mid-request `TxFinalizeEntry` per-request-ledger drain (C7) is
//!   leaf-boot's concern; this interceptor owns the happy-path + error-path
//!   demarcation (the explicit awaited commit/rollback, no async `Drop`).
//! - `NESTED` degrades to `JOIN` on the in-memory manager (no real savepoints).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod advisor;
pub mod interceptor;
pub mod manager;
pub mod propagation;
pub mod rollback;

use leaf_core::{Cx, LeafError, TxPhase, TxResourceKey, TxState};

pub use advisor::{
    enable_transaction_management, make_transaction_interceptor, make_transaction_interceptor_for,
    tx_advisor_contract, tx_advisor_pairing, tx_advisor_pairing_for, tx_marker, tx_order_key,
    TxPointcut, TX_MARKER_POINTCUT,
};
pub use interceptor::{result_classifier, ReturnClassifier, TransactionInterceptor};
pub use manager::{InMemoryTransactionManager, TxResource, TxSync};
pub use propagation::{resolve as resolve_propagation, TxAction};
pub use rollback::should_rollback;

/// The active transaction's [`TxState`], read from the ambient
/// [`Cx`](leaf_core::Cx) under the ONE [`TxResourceKey`](leaf_core::TxResourceKey)
/// (the design's `current_tx()`). `None` when no transaction is active on the
/// current thread of execution.
///
/// This is the single place "the current transaction" lives — the interceptor,
/// nested data-access, and the deferred transactional-events dispatch all read it.
#[must_use]
pub fn current_tx() -> Option<TxState> {
    Cx::current().and_then(|c| c.get::<TxResourceKey>().cloned())
}

/// `true` iff a transaction is active on the current thread of execution.
#[must_use]
pub fn is_transaction_active() -> bool {
    Cx::current().is_some_and(|c| c.contains::<TxResourceKey>())
}

/// Register a synchronization callback on the ACTIVE transaction's per-phase
/// registry (the seam `#[transactional_event_listener]` fires through).
///
/// Returns `Ok(())` if a tx is active (the callback is registered on its
/// [`TxSync`] bucket and fired by the interceptor at the `phase` boundary), or the
/// `cb` back in `Err` if no tx is active (the caller decides the no-tx fallback:
/// fire inline now, or skip with a loud diagnostic — phase3/09 §transactional-events).
///
/// # Errors
/// Returns the callback unfired (`Err(cb)`) when no transaction is active.
pub fn register_synchronization(
    phase: TxPhase,
    cb: leaf_core::TxSyncCallback,
) -> Result<(), leaf_core::TxSyncCallback> {
    match current_tx().as_ref().and_then(TxResource::from_state) {
        Some(resource) => {
            resource.sync.register(phase, cb);
            Ok(())
        }
        None => Err(cb),
    }
}

/// Build a tx illegal-state [`LeafError`] on the open
/// [`Integration`](leaf_core::ErrorKind::Integration) arm (the public form of the
/// taxonomy id propagation faults ride).
#[must_use]
pub fn tx_state_error(detail: &'static str) -> LeafError {
    LeafError::new(propagation::tx_error_kind())
        .caused_by(leaf_core::Cause::plain("transaction", detail))
}
