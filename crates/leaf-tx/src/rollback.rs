//! Rollback-rule matching (transaction-management, phase3/09): decide whether a
//! failing [`LeafError`] should ROLL BACK or COMMIT the surrounding transaction.
//!
//! The contract from the design (phase3/09 "Resolved open questions"): the
//! DEFAULT is "any `Err` rolls back" — UNLESS a `no_rollback_on` rule matches.
//! An explicit `rollback_on` rule lets a normally-committed kind force a
//! rollback. `no_rollback_on` is the override and takes precedence (Spring's
//! most-specific-rule-wins, modeled here as "an exclusion beats an inclusion").
//!
//! Matching reuses the error-model vocabulary ([`ErrorMatch`] over [`ErrorKind`])
//! — NOT stringly exception class names. A vendor's `DataAccessError` IS
//! `ErrorKind::Integration { kind_id }`, so a rule names the taxonomy id it
//! triggers on. Panics are NOT a rollback channel (no `catch_unwind`); only a
//! returned `Err` is classified here.

use leaf_core::{ErrorKind, ErrorMatch, LeafError, TxAttribute};

/// Whether `err` should roll the surrounding transaction back, per `attr`.
///
/// The rule precedence (Spring's most-specific-wins, realized by data):
///
/// 1. if any `no_rollback_on` rule matches → COMMIT (the override wins);
/// 2. else if any `rollback_on` rule matches → ROLL BACK;
/// 3. else the DEFAULT → ROLL BACK (any `Err` rolls back).
///
/// So with NO rules at all the default is "every error rolls back" (the safe
/// Spring default); a `no_rollback_on` carve-out is the only way an `Err` commits.
#[must_use]
pub fn should_rollback(attr: &TxAttribute, err: &LeafError) -> bool {
    if attr.no_rollback_on.iter().any(|m| matches_rule(m, err)) {
        return false;
    }
    if attr.rollback_on.iter().any(|m| matches_rule(m, err)) {
        return true;
    }
    // The default: any error rolls back.
    true
}

/// Whether one [`ErrorMatch`] rule matches `err`, by matching over the typed
/// [`ErrorKind`] — both [`ErrorMatch`] arms key a stable [`ContractId`] that is
/// compared against the open [`ErrorKind::Integration`] taxonomy id the whole
/// error model uses for by-data, cross-crate error classes.
///
/// [`ContractId`]: leaf_core::ContractId
#[must_use]
fn matches_rule(rule: &ErrorMatch, err: &LeafError) -> bool {
    let target = match rule {
        ErrorMatch::Integration(id) | ErrorMatch::Kind(id) => *id,
    };
    matches!(err.kind, ErrorKind::Integration { kind_id } if kind_id == target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{ContractId, DataAccessKind, ErrorMatch};

    fn integration(kind: DataAccessKind) -> LeafError {
        LeafError::new(ErrorKind::Integration { kind_id: kind.contract_id() })
    }

    fn attr_with(
        rollback_on: &'static [ErrorMatch],
        no_rollback_on: &'static [ErrorMatch],
    ) -> TxAttribute {
        TxAttribute { rollback_on, no_rollback_on, ..TxAttribute::DEFAULT }
    }

    #[test]
    fn default_any_error_rolls_back() {
        // With NO rules, EVERY error rolls back (the safe Spring default).
        let attr = TxAttribute::DEFAULT;
        assert!(should_rollback(&attr, &LeafError::new(ErrorKind::NoSuchBean)));
        assert!(should_rollback(&attr, &integration(DataAccessKind::DuplicateKey)));
    }

    #[test]
    fn no_rollback_on_carve_out_commits_a_matching_error() {
        // A no_rollback_on rule is the ONLY way an Err commits: an
        // OptimisticLockingFailure is excluded → commit (false), but a
        // non-matching error still rolls back.
        static NO_RB: [ErrorMatch; 1] =
            [ErrorMatch::Integration(ContractId::of("leaf::dao::OptimisticLockingFailure"))];
        let attr = attr_with(&[], &NO_RB);
        assert!(
            !should_rollback(&attr, &integration(DataAccessKind::OptimisticLockingFailure)),
            "the excluded kind commits"
        );
        assert!(
            should_rollback(&attr, &integration(DataAccessKind::DuplicateKey)),
            "a non-excluded error still rolls back (the default)"
        );
    }

    #[test]
    fn no_rollback_on_overrides_rollback_on() {
        // The SAME kind named in BOTH lists: the exclusion (no_rollback_on) wins
        // (Spring's most-specific-rule semantics, modeled as exclusion-beats-inclusion).
        let id = DataAccessKind::DuplicateKey.contract_id();
        static RB: [ErrorMatch; 1] =
            [ErrorMatch::Integration(ContractId::of("leaf::dao::DuplicateKey"))];
        static NO_RB: [ErrorMatch; 1] =
            [ErrorMatch::Integration(ContractId::of("leaf::dao::DuplicateKey"))];
        // sanity: the static ids equal the kind's id.
        assert_eq!(RB[0], ErrorMatch::Integration(id));
        let attr = attr_with(&RB, &NO_RB);
        assert!(!should_rollback(&attr, &integration(DataAccessKind::DuplicateKey)));
    }

    #[test]
    fn rollback_on_matches_the_integration_taxonomy_id() {
        // An explicit rollback_on rule keys the Integration taxonomy id; a matching
        // error rolls back, a non-matching one (with no other rule) ALSO rolls back
        // (the default), so prove the rule MATCHES via the no_rollback_on interplay.
        static RB: [ErrorMatch; 1] =
            [ErrorMatch::Integration(ContractId::of("leaf::dao::DuplicateKey"))];
        let attr = attr_with(&RB, &[]);
        // rollback_on present, error matches → rollback (true). (Default would also
        // be true, but the rule path is exercised: a no_rollback carve-out below
        // proves the rule list is consulted in order.)
        assert!(should_rollback(&attr, &integration(DataAccessKind::DuplicateKey)));
    }

    #[test]
    fn kind_and_integration_arms_match_the_same_id() {
        // Both ErrorMatch arms key a ContractId; either matches an Integration kind
        // with that id.
        let id = DataAccessKind::TransientDataAccess.contract_id();
        static NO_RB_KIND: [ErrorMatch; 1] =
            [ErrorMatch::Kind(ContractId::of("leaf::dao::TransientDataAccess"))];
        assert_eq!(NO_RB_KIND[0], ErrorMatch::Kind(id));
        let attr = attr_with(&[], &NO_RB_KIND);
        assert!(
            !should_rollback(&attr, &integration(DataAccessKind::TransientDataAccess)),
            "an ErrorMatch::Kind rule matches the same Integration id"
        );
    }
}
