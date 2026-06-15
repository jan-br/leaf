//! The in-memory / no-op [`TransactionManager`] + the concrete
//! per-Cx-thread-of-execution synchronization storage
//! (transaction-management, phase3/09).
//!
//! leaf-core's [`TxSyncRegistry`](leaf_core::TxSyncRegistry) pins the registration
//! SEAM (an object-safe `register(phase, cb)`), but its kernel storage is a no-op
//! placeholder (the live per-phase buckets need interior mutability + ordering,
//! which is leaf-tx's concern). This module owns that storage: [`TxSync`] is the
//! interior-mutable per-phase callback bucket the
//! [`TransactionInterceptor`](crate::TransactionInterceptor) fires at the
//! [`TxPhase`] boundaries; it rides INSIDE the manager-minted
//! [`TxState`](leaf_core::TxState) so the one tx-resource bundle (the
//! [`TxResourceKey`](leaf_core::TxResourceKey) value) carries both the connection
//! analogue AND the synchronization registry — exactly the seam transactional-events
//! reads via `current_tx()?` to register an AFTER_COMMIT callback.
//!
//! [`InMemoryTransactionManager`] is the no-op/in-memory manager used by tests and
//! the bare engine (a real datastore manager — leaf-sqlx-tx etc. — is an ordinary
//! bean). It records begin/commit/rollback counts so a test can assert the
//! interceptor drove the right outcome.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use leaf_core::{
    BoxFuture, Cause, ErrorKind, LeafError, ResolveCtx, TransactionManager, TxDefinition,
    TxOutcome, TxPhase, TxState, TxSyncCallback, TxSyncRegistry,
};

/// The concrete, interior-mutable per-phase synchronization bucket leaf-tx owns
/// (the live storage behind the kernel [`TxSyncRegistry`] seam).
///
/// Callbacks register via [`TxSync::register`] (the same shape as
/// [`TxSyncRegistry::register`]) and are drained-and-fired by
/// [`TxSync::fire_phase`] at each [`TxPhase`] boundary, IN REGISTRATION ORDER, on
/// the same logical task that owns the tx. A callback fires AT MOST ONCE (it is
/// moved out of the bucket when its phase fires).
#[derive(Default)]
pub struct TxSync {
    before_commit: Mutex<Vec<TxSyncCallback>>,
    after_commit: Mutex<Vec<TxSyncCallback>>,
    after_rollback: Mutex<Vec<TxSyncCallback>>,
    after_completion: Mutex<Vec<TxSyncCallback>>,
}

impl TxSync {
    /// A fresh, empty synchronization bucket.
    #[must_use]
    pub fn new() -> Self {
        TxSync::default()
    }

    /// Register `cb` to fire at `phase` (the live form of
    /// [`TxSyncRegistry::register`]).
    pub fn register(&self, phase: TxPhase, cb: TxSyncCallback) {
        self.bucket(phase).lock().expect("tx-sync bucket").push(cb);
    }

    /// Drain-and-fire every callback registered for `phase`, in registration
    /// order, handing each the final [`TxOutcome`]. Returns the FIRST error a
    /// callback yields (the BEFORE_COMMIT veto contract; AFTER_* callers isolate
    /// errors rather than propagate, per phase3/09).
    ///
    /// # Errors
    /// The first [`LeafError`] a fired callback returns.
    pub async fn fire_phase(&self, phase: TxPhase, outcome: TxOutcome) -> Result<(), LeafError> {
        let drained: Vec<TxSyncCallback> = std::mem::take(&mut *self.bucket(phase).lock().expect("tx-sync bucket"));
        let mut first_err: Option<LeafError> = None;
        for cb in drained {
            if let Err(e) = cb(outcome).await
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn bucket(&self, phase: TxPhase) -> &Mutex<Vec<TxSyncCallback>> {
        match phase {
            TxPhase::BeforeCommit => &self.before_commit,
            TxPhase::AfterCommit => &self.after_commit,
            TxPhase::AfterRollback => &self.after_rollback,
            TxPhase::AfterCompletion => &self.after_completion,
        }
    }
}

impl std::fmt::Debug for TxSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxSync").finish_non_exhaustive()
    }
}

/// The manager-specific state the [`InMemoryTransactionManager`] mints at `begin`
/// and carries on the ambient [`TxResourceKey`](leaf_core::TxResourceKey). Holds
/// the live [`TxSync`] bucket so nested data-access / transactional-events reach
/// the SAME registry through `current_tx()`.
#[derive(Clone)]
pub struct TxResource {
    /// A monotonically increasing per-manager tx id (diagnostics + identity).
    pub id: u64,
    /// Whether the tx is read-only (the resolved attribute hint).
    pub read_only: bool,
    /// The live per-phase synchronization bucket (shared so a nested lookup
    /// through the `Cx` registers on the SAME tx).
    pub sync: Arc<TxSync>,
}

impl TxResource {
    /// Recover the [`TxResource`] from an erased [`TxState`] (the value carried on
    /// the [`TxResourceKey`](leaf_core::TxResourceKey)).
    #[must_use]
    pub fn from_state(st: &TxState) -> Option<&TxResource> {
        st.downcast_ref::<TxResource>()
    }
}

impl std::fmt::Debug for TxResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxResource")
            .field("id", &self.id)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

/// The in-memory / no-op [`TransactionManager`] (the test + bare-engine default).
///
/// `begin` mints a fresh [`TxResource`] (a [`TxSync`] bucket + an id); `commit` /
/// `rollback` do no datastore work but RECORD the outcome so a test can assert the
/// interceptor demarcated correctly. A real datastore manager (leaf-sqlx-tx, …) is
/// an ordinary bean implementing the SAME trait; this is the safe stand-in.
pub struct InMemoryTransactionManager {
    next_id: AtomicUsize,
    begins: AtomicUsize,
    commits: AtomicUsize,
    rollbacks: AtomicUsize,
    /// A seam-shaped (no-op-storage) registry returned by `synchronizations` to
    /// honor the trait; the LIVE storage is the [`TxSync`] on each [`TxResource`].
    seam: TxSyncRegistry,
}

impl InMemoryTransactionManager {
    /// A fresh manager with zeroed counters.
    #[must_use]
    pub fn new() -> Self {
        InMemoryTransactionManager {
            next_id: AtomicUsize::new(0),
            begins: AtomicUsize::new(0),
            commits: AtomicUsize::new(0),
            rollbacks: AtomicUsize::new(0),
            seam: TxSyncRegistry::new(),
        }
    }

    /// The number of transactions begun.
    #[must_use]
    pub fn begins(&self) -> usize {
        self.begins.load(Ordering::SeqCst)
    }

    /// The number of transactions committed.
    #[must_use]
    pub fn commits(&self) -> usize {
        self.commits.load(Ordering::SeqCst)
    }

    /// The number of transactions rolled back.
    #[must_use]
    pub fn rollbacks(&self) -> usize {
        self.rollbacks.load(Ordering::SeqCst)
    }
}

impl Default for InMemoryTransactionManager {
    fn default() -> Self {
        InMemoryTransactionManager::new()
    }
}

impl TransactionManager for InMemoryTransactionManager {
    fn begin<'a>(
        &'a self,
        def: &'a TxDefinition,
        _cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<TxState, LeafError>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as u64;
        self.begins.fetch_add(1, Ordering::SeqCst);
        let read_only = def.attribute.read_only;
        Box::pin(async move {
            Ok(TxState::new(TxResource { id, read_only, sync: Arc::new(TxSync::new()) }))
        })
    }

    fn commit(&self, st: TxState) -> BoxFuture<'_, Result<(), LeafError>> {
        let ok = TxResource::from_state(&st).is_some();
        self.commits.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if ok {
                Ok(())
            } else {
                Err(LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
                    "transaction commit",
                    "the tx state did not carry an in-memory TxResource",
                )))
            }
        })
    }

    fn rollback(&self, st: TxState) -> BoxFuture<'_, Result<(), LeafError>> {
        let ok = TxResource::from_state(&st).is_some();
        self.rollbacks.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if ok {
                Ok(())
            } else {
                Err(LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
                    "transaction rollback",
                    "the tx state did not carry an in-memory TxResource",
                )))
            }
        })
    }

    fn synchronizations<'a>(&'a self, _st: &'a TxState) -> &'a TxSyncRegistry {
        // The trait returns the kernel seam type; the LIVE per-phase storage is the
        // TxSync bucket on the TxResource the interceptor reaches through the Cx.
        &self.seam
    }
}

impl std::fmt::Debug for InMemoryTransactionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryTransactionManager")
            .field("begins", &self.begins())
            .field("commits", &self.commits())
            .field("rollbacks", &self.rollbacks())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{TxAttribute, TxDefinition};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[test]
    fn begin_commit_round_trip_records_counts_and_carries_a_resource() {
        let mgr = InMemoryTransactionManager::new();
        let def = TxDefinition { attribute: TxAttribute::DEFAULT };
        let cx = ResolveCtx::root();
        let st = block(mgr.begin(&def, &cx)).expect("begin");
        assert_eq!(mgr.begins(), 1);
        // The state carries a live TxResource (id + a sync bucket).
        let resource = TxResource::from_state(&st).expect("a TxResource rides the state");
        assert_eq!(resource.id, 0);
        assert!(!resource.read_only);
        block(mgr.commit(st)).expect("commit");
        assert_eq!(mgr.commits(), 1);
        assert_eq!(mgr.rollbacks(), 0);
    }

    #[test]
    fn read_only_attribute_rides_the_resource() {
        let mgr = InMemoryTransactionManager::new();
        let def = TxDefinition {
            attribute: TxAttribute { read_only: true, ..TxAttribute::DEFAULT },
        };
        let cx = ResolveCtx::root();
        let st = block(mgr.begin(&def, &cx)).unwrap();
        assert!(TxResource::from_state(&st).unwrap().read_only);
    }

    #[test]
    fn rollback_records_a_rollback_not_a_commit() {
        let mgr = InMemoryTransactionManager::new();
        let def = TxDefinition { attribute: TxAttribute::DEFAULT };
        let cx = ResolveCtx::root();
        let st = block(mgr.begin(&def, &cx)).unwrap();
        block(mgr.rollback(st)).expect("rollback");
        assert_eq!(mgr.rollbacks(), 1);
        assert_eq!(mgr.commits(), 0);
    }

    #[test]
    fn tx_ids_are_distinct_per_begin() {
        let mgr = InMemoryTransactionManager::new();
        let def = TxDefinition { attribute: TxAttribute::DEFAULT };
        let cx = ResolveCtx::root();
        let a = block(mgr.begin(&def, &cx)).unwrap();
        let b = block(mgr.begin(&def, &cx)).unwrap();
        assert_ne!(
            TxResource::from_state(&a).unwrap().id,
            TxResource::from_state(&b).unwrap().id
        );
    }

    #[test]
    fn tx_sync_fires_callbacks_in_registration_order_at_their_phase() {
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let sync = TxSync::new();
        for name in ["a", "b", "c"] {
            let log = Arc::clone(&log);
            sync.register(
                TxPhase::AfterCommit,
                Box::new(move |outcome: TxOutcome| {
                    let log = Arc::clone(&log);
                    Box::pin(async move {
                        log.lock().unwrap().push(format!("{name}:{outcome:?}"));
                        Ok(())
                    }) as BoxFuture<'static, Result<(), LeafError>>
                }),
            );
        }
        // A callback for a DIFFERENT phase must NOT fire on AfterCommit.
        let other = Arc::clone(&log);
        sync.register(
            TxPhase::AfterRollback,
            Box::new(move |_| {
                let other = Arc::clone(&other);
                Box::pin(async move {
                    other.lock().unwrap().push("rollback-cb".into());
                    Ok(())
                }) as BoxFuture<'static, Result<(), LeafError>>
            }),
        );

        block(sync.fire_phase(TxPhase::AfterCommit, TxOutcome::Committed)).expect("fire");
        assert_eq!(
            *log.lock().unwrap(),
            vec!["a:Committed", "b:Committed", "c:Committed"],
            "AfterCommit callbacks fired in registration order; the rollback cb did NOT fire"
        );
    }

    #[test]
    fn tx_sync_before_commit_error_is_returned_as_a_veto() {
        let sync = TxSync::new();
        sync.register(
            TxPhase::BeforeCommit,
            Box::new(|_| {
                Box::pin(async {
                    Err(LeafError::new(ErrorKind::ValidationError))
                }) as BoxFuture<'static, Result<(), LeafError>>
            }),
        );
        let veto = block(sync.fire_phase(TxPhase::BeforeCommit, TxOutcome::Committed));
        assert!(veto.is_err(), "a BEFORE_COMMIT error is surfaced as a veto");
    }

    #[test]
    fn tx_sync_fires_each_callback_at_most_once() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sync = TxSync::new();
        let c = Arc::clone(&count);
        sync.register(
            TxPhase::AfterCompletion,
            Box::new(move |_| {
                let c = Arc::clone(&c);
                Box::pin(async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }) as BoxFuture<'static, Result<(), LeafError>>
            }),
        );
        block(sync.fire_phase(TxPhase::AfterCompletion, TxOutcome::Committed)).unwrap();
        // A second fire of the same phase drains an EMPTY bucket (fire-at-most-once).
        block(sync.fire_phase(TxPhase::AfterCompletion, TxOutcome::Committed)).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
