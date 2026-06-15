//! Propagation resolution (transaction-management, phase3/09): given the declared
//! [`TxPropagation`] and whether a transaction is ALREADY active on the ambient
//! [`Cx`](leaf_core::Cx) (the [`TxResourceKey`](leaf_core::TxResourceKey) binding),
//! decide what the interceptor must DO before running the method body.
//!
//! This is the pure decision half (no `.await`, no manager); the
//! [`TransactionInterceptor`](crate::TransactionInterceptor) drives the resulting
//! [`TxAction`] (begin / join / suspend+begin / run-plain / error). REQUIRES_NEW's
//! one-directional child shadow and the never-inheritable rule are realized on the
//! [`TxResourceKey`](leaf_core::TxResourceKey) `POLICY = Isolate` (see phase3/09
//! §transaction-management).

use leaf_core::{ErrorKind, LeafError, TxPropagation};

/// What the interceptor must do for a call, resolved from the declared
/// [`TxPropagation`] + whether a tx is already active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxAction {
    /// Begin a fresh transaction, install it on the ambient `Cx`, and
    /// commit/rollback it at the end (the demarcating owner).
    BeginNew,
    /// A transaction is already active — JOIN it (do not begin/commit/rollback;
    /// the outer owner demarcates). The body runs in the existing tx.
    JoinExisting,
    /// Suspend the active transaction (shadow it off the `Cx`) and begin a NEW
    /// independent one that this call owns (`REQUIRES_NEW` with an active tx).
    SuspendAndBeginNew,
    /// Run the body NON-transactionally, with any active tx suspended off the
    /// ambient `Cx` for the duration (`NOT_SUPPORTED`, or `NEVER` with no tx).
    SuspendAndRunPlain,
    /// Run the body NON-transactionally with no tx active (`SUPPORTS`/`NEVER`/
    /// `NOT_SUPPORTED` when none is active) — nothing to demarcate, nothing to
    /// suspend.
    RunPlain,
}

impl TxAction {
    /// Whether this action makes THIS call the tx OWNER (it begins + must
    /// commit/rollback at the end). `JoinExisting`/`RunPlain`/`SuspendAndRunPlain`
    /// are NOT owners (no commit/rollback fires for them here).
    #[must_use]
    pub fn owns_transaction(self) -> bool {
        matches!(self, TxAction::BeginNew | TxAction::SuspendAndBeginNew)
    }

    /// Whether this action must SUSPEND (shadow off the `Cx`) the active tx for
    /// the body's duration, restoring it after.
    #[must_use]
    pub fn suspends_existing(self) -> bool {
        matches!(self, TxAction::SuspendAndBeginNew | TxAction::SuspendAndRunPlain)
    }
}

/// Resolve the [`TxAction`] for `propagation` given whether a tx is `active`.
///
/// `MANDATORY` with no active tx and `NEVER` with an active tx are illegal
/// states surfaced as a loud [`LeafError`] (`ErrorKind::Integration` on the tx
/// taxonomy id) — never a silent fallthrough.
///
/// # Errors
/// `MANDATORY` requires an active tx; `NEVER` forbids one.
pub fn resolve(propagation: TxPropagation, active: bool) -> Result<TxAction, LeafError> {
    use TxPropagation::{Mandatory, Nested, Never, NotSupported, Required, RequiresNew, Supports};
    Ok(match (propagation, active) {
        // REQUIRED: join if active, else begin one (the default).
        (Required, true) => TxAction::JoinExisting,
        (Required, false) => TxAction::BeginNew,
        // REQUIRES_NEW: always a NEW independent tx; suspend the outer if present.
        (RequiresNew, true) => TxAction::SuspendAndBeginNew,
        (RequiresNew, false) => TxAction::BeginNew,
        // NESTED: a savepoint within an active tx if present, else begin one. The
        // in-memory/no-op manager has no real savepoints, so NESTED degrades to
        // JOIN when active (a documented v1 simplification) and BEGIN when not.
        (Nested, true) => TxAction::JoinExisting,
        (Nested, false) => TxAction::BeginNew,
        // SUPPORTS: join if active, else run plain (no tx demarcated).
        (Supports, true) => TxAction::JoinExisting,
        (Supports, false) => TxAction::RunPlain,
        // NOT_SUPPORTED: always run plain; suspend any active tx for the duration.
        (NotSupported, true) => TxAction::SuspendAndRunPlain,
        (NotSupported, false) => TxAction::RunPlain,
        // MANDATORY: must join an active tx; ERROR if none.
        (Mandatory, true) => TxAction::JoinExisting,
        (Mandatory, false) => {
            return Err(illegal(
                "propagation MANDATORY requires an active transaction, but none is bound",
            ));
        }
        // NEVER: must run plain; ERROR if a tx is active.
        (Never, false) => TxAction::RunPlain,
        (Never, true) => {
            return Err(illegal(
                "propagation NEVER forbids an active transaction, but one is bound",
            ));
        }
    })
}

/// The stable tx taxonomy id illegal-propagation faults ride on the open
/// [`ErrorKind::Integration`] arm (by-data, no core ABI bump).
#[must_use]
pub(crate) fn tx_error_kind() -> ErrorKind {
    ErrorKind::Integration { kind_id: leaf_core::ContractId::of("leaf::tx::IllegalTransactionState") }
}

fn illegal(detail: &'static str) -> LeafError {
    LeafError::new(tx_error_kind())
        .caused_by(leaf_core::Cause::plain("transaction propagation", detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::TxPropagation;

    #[test]
    fn required_begins_when_none_active_joins_when_active() {
        assert_eq!(resolve(TxPropagation::Required, false).unwrap(), TxAction::BeginNew);
        assert_eq!(resolve(TxPropagation::Required, true).unwrap(), TxAction::JoinExisting);
    }

    #[test]
    fn requires_new_always_begins_suspending_the_outer() {
        // No active → begin a fresh one (an owner).
        assert_eq!(resolve(TxPropagation::RequiresNew, false).unwrap(), TxAction::BeginNew);
        // Active → suspend the outer + begin a new INDEPENDENT one (also an owner).
        let a = resolve(TxPropagation::RequiresNew, true).unwrap();
        assert_eq!(a, TxAction::SuspendAndBeginNew);
        assert!(a.owns_transaction(), "REQUIRES_NEW owns its own tx");
        assert!(a.suspends_existing(), "REQUIRES_NEW suspends the outer");
    }

    #[test]
    fn supports_joins_or_runs_plain() {
        assert_eq!(resolve(TxPropagation::Supports, true).unwrap(), TxAction::JoinExisting);
        assert_eq!(resolve(TxPropagation::Supports, false).unwrap(), TxAction::RunPlain);
    }

    #[test]
    fn not_supported_runs_plain_suspending_an_active_tx() {
        let active = resolve(TxPropagation::NotSupported, true).unwrap();
        assert_eq!(active, TxAction::SuspendAndRunPlain);
        assert!(!active.owns_transaction(), "NOT_SUPPORTED never owns a tx");
        assert!(active.suspends_existing());
        assert_eq!(resolve(TxPropagation::NotSupported, false).unwrap(), TxAction::RunPlain);
    }

    #[test]
    fn mandatory_requires_an_active_tx_else_errors() {
        assert_eq!(resolve(TxPropagation::Mandatory, true).unwrap(), TxAction::JoinExisting);
        let err = resolve(TxPropagation::Mandatory, false).expect_err("MANDATORY must error");
        assert_eq!(err.kind, tx_error_kind());
    }

    #[test]
    fn never_forbids_an_active_tx_else_runs_plain() {
        assert_eq!(resolve(TxPropagation::Never, false).unwrap(), TxAction::RunPlain);
        let err = resolve(TxPropagation::Never, true).expect_err("NEVER must error when active");
        assert_eq!(err.kind, tx_error_kind());
    }

    #[test]
    fn nested_begins_or_joins_on_the_no_savepoint_manager() {
        // The in-memory manager has no savepoints, so NESTED degrades: begin when
        // none active, join when active (a documented v1 simplification).
        assert_eq!(resolve(TxPropagation::Nested, false).unwrap(), TxAction::BeginNew);
        assert_eq!(resolve(TxPropagation::Nested, true).unwrap(), TxAction::JoinExisting);
    }

    #[test]
    fn only_begin_actions_own_the_transaction() {
        assert!(TxAction::BeginNew.owns_transaction());
        assert!(TxAction::SuspendAndBeginNew.owns_transaction());
        assert!(!TxAction::JoinExisting.owns_transaction());
        assert!(!TxAction::RunPlain.owns_transaction());
        assert!(!TxAction::SuspendAndRunPlain.owns_transaction());
    }
}
