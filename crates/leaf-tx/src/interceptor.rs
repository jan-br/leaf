//! The [`TransactionInterceptor`] — the around-advice that demarcates a
//! transaction over an advised method call (transaction-management, phase3/09).
//!
//! The body, per phase3/09 §transaction-management:
//!
//! 1. read the resolved [`TxAttribute`](leaf_core::TxAttribute) →
//!    [`resolve`](crate::propagation::resolve) the
//!    [`TxAction`](crate::propagation::TxAction) from the propagation + whether a
//!    tx is already active on the ambient [`Cx`](leaf_core::Cx);
//! 2. if this call OWNS the tx, `manager.begin()` and INSTALL the
//!    [`TxState`](leaf_core::TxState) on the ambient `Cx` under the ONE
//!    [`TxResourceKey`](leaf_core::TxResourceKey) (`POLICY = Isolate` — never
//!    inherited across a spawn), wrapping `next.proceed()` in
//!    [`Scoped`](leaf_core::Scoped) so the binding re-installs per poll and
//!    survives work-stealing;
//! 3. `next.proceed().await`;
//! 4. on `Ok` → fire BEFORE_COMMIT (a veto here aborts the commit), `commit`, fire
//!    AFTER_COMMIT; on `Err` matching a rollback rule → `rollback`, fire
//!    AFTER_ROLLBACK; ALWAYS fire AFTER_COMPLETION;
//! 5. a JOIN / run-plain call does NOT begin/commit (the outer owner demarcates).
//!
//! Cancellation: the tx-resource binding rides [`Scoped`](leaf_core::Scoped)'s sync
//! RAII restore; the
//! explicit commit/rollback is an awaited step in THIS body (no async `Drop`). The
//! full SIGTERM-mid-request `TxFinalizeEntry` drain is leaf-boot's ledger concern;
//! this body owns the happy-path + error-path demarcation (see the NOTE in the
//! crate docs).

use std::sync::Arc;

use leaf_core::{
    AdviceError, BoxFuture, Call, Cx, CxFutureExt, Interceptor, LeafError, Next, TransactionManager,
    TxAttribute, TxDefinition, TxOutcome, TxPhase, TxResourceKey, TxState,
};

use crate::manager::TxResource;
use crate::propagation::resolve;
use crate::rollback::should_rollback;

/// Classifies an advised method's ERASED return value as a tx success or failure.
///
/// A method that returns `Result<T, LeafError>` reports a business failure THROUGH
/// its `Ok(ErasedRet)` return (the chain packs the whole `Result` into the
/// [`ErasedRet`](leaf_core::ErasedRet) — `proceed` only yields `Err(AdviceError)`
/// for a FRAMEWORK fault, not a method-returned `Result::Err`). So a tx interceptor
/// must peek INSIDE the typed return to decide commit-vs-rollback. A classifier
/// downcasts the erased return to the concrete `Result<T, LeafError>` and yields the
/// `LeafError` on the `Err` arm; [`result_classifier`] builds one per concrete `T`.
///
/// The DEFAULT (no classifier) treats every `Ok(ErasedRet)` as a tx success — only
/// a framework `AdviceError` from `proceed` rolls back. (When `#[transactional]`
/// lands it emits the per-method classifier; until then a binding site installs one
/// — a NOTE in the crate docs.)
pub type ReturnClassifier = fn(&leaf_core::ErasedRet) -> Option<LeafError>;

/// A [`ReturnClassifier`] for a method returning `Result<T, LeafError>`: downcasts
/// the erased return to `Result<T, LeafError>` and clones the `Err` payload (the
/// business failure the rollback rules classify), or `None` on `Ok` / a type
/// mismatch (which degrades to "treat as success", never a panic).
#[must_use]
pub fn result_classifier<T: std::any::Any + Send>() -> ReturnClassifier {
    |ret: &leaf_core::ErasedRet| -> Option<LeafError> {
        ret.0
            .downcast_ref::<Result<T, LeafError>>()
            .and_then(|r| r.as_ref().err().cloned())
    }
}

/// The around-advice [`Interceptor`] that demarcates a transaction over the call.
///
/// Holds the resolved [`TransactionManager`] (resolved by the advisor's
/// `make_interceptor` bean bridge through the container) + the [`TxAttribute`] the
/// `#[transactional(..)]` macro emits for the advised method + an optional
/// [`ReturnClassifier`] that detects a `Result::Err` business failure in the typed
/// return. (Until a per-method attribute/classifier table is threaded, ONE
/// attribute + ONE classifier apply to every method the pointcut matches; the
/// design pins these as the macro's emitted consts, a NOTE in the crate docs.)
pub struct TransactionInterceptor {
    manager: Arc<dyn TransactionManager>,
    attribute: TxAttribute,
    classify_return: Option<ReturnClassifier>,
}

impl TransactionInterceptor {
    /// Build an interceptor over a resolved manager + the call's tx attribute (no
    /// return classifier — only a framework `AdviceError` rolls back).
    #[must_use]
    pub fn new(manager: Arc<dyn TransactionManager>, attribute: TxAttribute) -> Self {
        TransactionInterceptor { manager, attribute, classify_return: None }
    }

    /// Install a [`ReturnClassifier`] (builder style) so a `Result::Err` business
    /// failure in the method's typed return also drives the rollback decision.
    #[must_use]
    pub fn with_return_classifier(mut self, classify: ReturnClassifier) -> Self {
        self.classify_return = Some(classify);
        self
    }

    /// The resolved [`TxAttribute`] this interceptor applies.
    #[must_use]
    pub fn attribute(&self) -> &TxAttribute {
        &self.attribute
    }

    /// Whether a transaction is already active on the ambient `Cx` (the
    /// [`TxResourceKey`] binding).
    fn tx_active() -> bool {
        Cx::current().is_some_and(|c| c.contains::<TxResourceKey>())
    }

    /// Classify a successfully-returned [`ErasedRet`]: `Some(err)` iff a return
    /// classifier detected a `Result::Err` business failure (the rollback signal),
    /// else `None` (commit).
    fn business_failure(&self, ret: &leaf_core::ErasedRet) -> Option<LeafError> {
        self.classify_return.and_then(|c| c(ret))
    }
}

impl Interceptor for TransactionInterceptor {
    fn intercept<'a>(
        &'a self,
        call: &'a Call<'a>,
        next: Next<'a>,
    ) -> BoxFuture<'a, Result<ErasedRetAlias, AdviceError>> {
        Box::pin(async move {
            let action = resolve(self.attribute.propagation, Self::tx_active())
                .map_err(AdviceError::AroundBody)?;

            if action.owns_transaction() {
                self.run_owning(call, next).await
            } else {
                // JOIN / SUPPORTS-plain / NOT_SUPPORTED-plain: the outer owner (or
                // nobody) demarcates; just run the body. (Suspension of the outer tx
                // for the plain forms is a documented v1 simplification — the body
                // simply does not begin/commit; see the crate NOTE.)
                let _ = action; // action recorded; no owning work here
                next_proceed(next, call).await
            }
        })
    }
}

// A local alias keeps the trait signature readable without re-importing the long
// `ErasedRet` path into the public surface twice.
type ErasedRetAlias = leaf_core::ErasedRet;

impl TransactionInterceptor {
    /// The owning path: begin → install on `Cx` → proceed (scoped) → commit-or-rollback.
    async fn run_owning<'a>(
        &'a self,
        call: &'a Call<'a>,
        next: Next<'a>,
    ) -> Result<ErasedRetAlias, AdviceError> {
        let def = TxDefinition { attribute: self.attribute };
        let state = self
            .manager
            .begin(&def, call.cx)
            .await
            .map_err(AdviceError::AroundBody)?;

        // The live synchronization bucket for THIS tx (so nested data-access /
        // transactional-events register on the SAME registry via current_tx()).
        let sync = TxResource::from_state(&state).map(|r| Arc::clone(&r.sync));

        // INSTALL the tx-resource on the ambient Cx for the body's duration: derive
        // a child bundle binding TxResourceKey and re-install it per poll via Scoped
        // (the work-stealing-correct, cancel-safe RAII model). The `proceed` call is
        // deferred INSIDE the scoped async block so the SYNCHRONOUS prelude of the
        // chain tail (a nested-data-access `current_tx()` read) observes the binding
        // too — `Scoped` only installs during `poll`, never at future construction.
        let body_cx = Cx::current_or_empty().with::<TxResourceKey>(state.clone());
        let result = async move { next_proceed(next, call).await }.scoped(body_cx).await;

        // Demarcate. proceed() yields Result<ErasedRet, AdviceError>; the rollback
        // rule matches over the underlying LeafError (AdviceError → LeafError).
        match result {
            Ok(ret) => {
                // A method returning Result<_, LeafError> reports a business failure
                // THROUGH its Ok(ErasedRet); a return classifier peeks inside. A
                // detected Err that the rollback rules match → ROLL BACK, but the
                // method's Result::Err is STILL returned to the caller (the tx is the
                // side effect, the value passes through).
                // (A detected business Err with NO matching rollback rule — a
                // no_rollback_on carve-out — falls through to commit anyway.)
                if let Some(err) = self.business_failure(&ret)
                    && should_rollback(&self.attribute, &err)
                {
                    self.manager.rollback(state).await.map_err(AdviceError::AroundBody)?;
                    if let Some(sync) = &sync {
                        let _ = sync.fire_phase(TxPhase::AfterRollback, TxOutcome::RolledBack).await;
                        let _ = sync.fire_phase(TxPhase::AfterCompletion, TxOutcome::RolledBack).await;
                    }
                    return Ok(ret);
                }
                // BEFORE_COMMIT (a veto here aborts the commit → rollback path).
                if let Some(sync) = &sync
                    && let Err(veto) =
                        sync.fire_phase(TxPhase::BeforeCommit, TxOutcome::Committed).await
                {
                    return self.abort_after_before_commit_veto(state, sync, veto).await;
                }
                self.manager.commit(state).await.map_err(AdviceError::AroundBody)?;
                if let Some(sync) = &sync {
                    // AFTER_* errors are isolated (cannot un-commit); swallow here.
                    let _ = sync.fire_phase(TxPhase::AfterCommit, TxOutcome::Committed).await;
                    let _ = sync.fire_phase(TxPhase::AfterCompletion, TxOutcome::Committed).await;
                }
                Ok(ret)
            }
            Err(advice_err) => {
                let leaf = advice_err.into_leaf_error();
                if should_rollback(&self.attribute, &leaf) {
                    self.manager.rollback(state).await.map_err(AdviceError::AroundBody)?;
                    if let Some(sync) = &sync {
                        let _ = sync.fire_phase(TxPhase::AfterRollback, TxOutcome::RolledBack).await;
                        let _ = sync.fire_phase(TxPhase::AfterCompletion, TxOutcome::RolledBack).await;
                    }
                } else {
                    // A no_rollback_on carve-out: COMMIT despite the error, then
                    // re-raise the original error to the caller.
                    if let Some(sync) = &sync {
                        let _ = sync.fire_phase(TxPhase::BeforeCommit, TxOutcome::Committed).await;
                    }
                    self.manager.commit(state).await.map_err(AdviceError::AroundBody)?;
                    if let Some(sync) = &sync {
                        let _ = sync.fire_phase(TxPhase::AfterCommit, TxOutcome::Committed).await;
                        let _ = sync.fire_phase(TxPhase::AfterCompletion, TxOutcome::Committed).await;
                    }
                }
                Err(AdviceError::AroundBody(leaf))
            }
        }
    }

    /// A BEFORE_COMMIT listener vetoed the commit: roll back, fire AFTER_ROLLBACK
    /// + AFTER_COMPLETION, and surface the veto error.
    async fn abort_after_before_commit_veto(
        &self,
        state: TxState,
        sync: &Arc<crate::manager::TxSync>,
        veto: LeafError,
    ) -> Result<ErasedRetAlias, AdviceError> {
        self.manager.rollback(state).await.map_err(AdviceError::AroundBody)?;
        let _ = sync.fire_phase(TxPhase::AfterRollback, TxOutcome::RolledBack).await;
        let _ = sync.fire_phase(TxPhase::AfterCompletion, TxOutcome::RolledBack).await;
        Err(AdviceError::AroundBody(veto))
    }
}

/// `next.proceed(call)` as an owned future (the `Next` borrows `call` per-proceed;
/// this single non-replaying proceed is the common tx path — a retry advisor sits
/// OUTSIDE tx so each attempt is its own tx).
fn next_proceed<'a>(
    mut next: Next<'a>,
    call: &'a Call<'a>,
) -> BoxFuture<'a, Result<ErasedRetAlias, AdviceError>> {
    next.proceed(call)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use leaf_core::{
        AdviceChain, BeanKey, ContractId, DataAccessKind, ErasedArgs, ErasedRet, ErrorKind,
        ErrorMatch, FixedTarget, MethodKey, ResolveCtx, Tail, TxOutcome, TxPhase, TxPropagation,
        TxSyncCallback,
    };

    use crate::manager::{InMemoryTransactionManager, TxResource};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    // A nop bean the FixedTarget points at (the tail does its own work).
    #[derive(Debug)]
    struct Nop;
    impl leaf_core::Bean for Nop {}

    fn nop_target() -> FixedTarget {
        let bean: leaf_core::ErasedBean = std::sync::Arc::new(Nop);
        FixedTarget::new(bean)
    }

    fn call_key() -> (MethodKey, BeanKey) {
        (MethodKey::of("svc::tx_method"), BeanKey::ByContract(ContractId::of("svc::Svc")))
    }

    /// Build a one-interceptor chain (the tx interceptor) + drive a tail returning
    /// `tail_result`, recording whether the tx was visible on the ambient Cx in the
    /// body.
    fn drive(
        attr: TxAttribute,
        tail_result: Result<i64, LeafError>,
    ) -> (Arc<InMemoryTransactionManager>, Result<ErasedRet, AdviceError>, bool) {
        let mgr = Arc::new(InMemoryTransactionManager::new());
        let interceptor: Arc<dyn Interceptor> =
            Arc::new(TransactionInterceptor::new(mgr.clone(), attr));
        let chain = AdviceChain::new(Box::new([interceptor]));

        let target = nop_target();
        let cx = ResolveCtx::root();
        let (method, bean) = call_key();
        let call = Call::new(method, bean, ErasedArgs::none(), &target, &cx);

        let saw_tx = Arc::new(AtomicBool::new(false));
        let saw_tx_in = Arc::clone(&saw_tx);
        let tail_result = Mutex::new(Some(tail_result));
        let tail: Box<Tail> = Box::new(move |_call: &Call<'_>| {
            // Read the ambient tx INSIDE the body (the nested-data-access view).
            let active = crate::current_tx().is_some();
            saw_tx_in.store(active, Ordering::SeqCst);
            let r = tail_result.lock().unwrap().take().expect("tail run once");
            Box::pin(async move {
                match r {
                    Ok(v) => Ok(ErasedRet::pack(v)),
                    Err(e) => Err(AdviceError::AroundBody(e)),
                }
            }) as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });

        let out = block(chain.invoke(&call, &*tail));
        (mgr, out, saw_tx.load(Ordering::SeqCst))
    }

    #[test]
    fn commit_on_ok_and_the_tx_is_visible_in_the_body() {
        let (mgr, out, saw_tx) = drive(TxAttribute::DEFAULT, Ok(42));
        assert_eq!(out.expect("ok").unpack::<i64>().unwrap(), 42, "the body ran, result passes through");
        assert_eq!(mgr.begins(), 1, "a tx was begun");
        assert_eq!(mgr.commits(), 1, "Ok → commit");
        assert_eq!(mgr.rollbacks(), 0, "no rollback on Ok");
        assert!(saw_tx, "the tx was visible on the ambient Cx inside the body");
    }

    #[test]
    fn rollback_on_err() {
        let (mgr, out, _) = drive(TxAttribute::DEFAULT, Err(LeafError::new(ErrorKind::ValidationError)));
        assert!(out.is_err(), "the error propagates to the caller");
        assert_eq!(mgr.begins(), 1);
        assert_eq!(mgr.rollbacks(), 1, "Err (default rule) → rollback");
        assert_eq!(mgr.commits(), 0, "no commit on a rolled-back error");
    }

    #[test]
    fn no_rollback_on_carve_out_commits_despite_the_error() {
        // A no_rollback_on rule for the error's kind → COMMIT, then re-raise the error.
        static NO_RB: [ErrorMatch; 1] =
            [ErrorMatch::Integration(ContractId::of("leaf::dao::OptimisticLockingFailure"))];
        let attr = TxAttribute { no_rollback_on: &NO_RB, ..TxAttribute::DEFAULT };
        let err = LeafError::new(ErrorKind::Integration {
            kind_id: DataAccessKind::OptimisticLockingFailure.contract_id(),
        });
        let (mgr, out, _) = drive(attr, Err(err));
        assert!(out.is_err(), "the original error is still re-raised");
        assert_eq!(mgr.commits(), 1, "the carve-out committed despite the error");
        assert_eq!(mgr.rollbacks(), 0);
    }

    #[test]
    fn mandatory_with_no_active_tx_errors_without_beginning() {
        let attr = TxAttribute { propagation: TxPropagation::Mandatory, ..TxAttribute::DEFAULT };
        let (mgr, out, _) = drive(attr, Ok(1));
        assert!(out.is_err(), "MANDATORY with no active tx is an illegal-state error");
        assert_eq!(mgr.begins(), 0, "no tx begun on the illegal-state path");
    }

    #[test]
    fn after_commit_sync_fires_on_ok() {
        // Register an AFTER_COMMIT callback from INSIDE the body (the deferred
        // transactional-events seam), then assert the interceptor fired it on commit.
        let mgr = Arc::new(InMemoryTransactionManager::new());
        let interceptor: Arc<dyn Interceptor> =
            Arc::new(TransactionInterceptor::new(mgr.clone(), TxAttribute::DEFAULT));
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let (method, bean) = call_key();
        let call = Call::new(method, bean, ErasedArgs::none(), &target, &cx);

        let fired = Arc::new(Mutex::new(Vec::<TxOutcome>::new()));
        let fired_in = Arc::clone(&fired);
        let tail: Box<Tail> = Box::new(move |_call: &Call<'_>| {
            // Register an AFTER_COMMIT sync on the active tx (current_tx()).
            let fired_in = Arc::clone(&fired_in);
            let cb: TxSyncCallback = Box::new(move |outcome: TxOutcome| {
                let fired_in = Arc::clone(&fired_in);
                Box::pin(async move {
                    fired_in.lock().unwrap().push(outcome);
                    Ok(())
                }) as BoxFuture<'static, Result<(), LeafError>>
            });
            let registered = crate::register_synchronization(TxPhase::AfterCommit, cb).is_ok();
            Box::pin(async move {
                assert!(registered, "the body could register on the active tx");
                Ok(ErasedRet::pack(7_i64))
            }) as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });

        block(chain.invoke(&call, &*tail)).expect("ok");
        assert_eq!(mgr.commits(), 1);
        assert_eq!(*fired.lock().unwrap(), vec![TxOutcome::Committed], "AFTER_COMMIT fired once on commit");
    }

    #[test]
    fn after_rollback_sync_fires_on_err() {
        let mgr = Arc::new(InMemoryTransactionManager::new());
        let interceptor: Arc<dyn Interceptor> =
            Arc::new(TransactionInterceptor::new(mgr.clone(), TxAttribute::DEFAULT));
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let (method, bean) = call_key();
        let call = Call::new(method, bean, ErasedArgs::none(), &target, &cx);

        let fired = Arc::new(Mutex::new(Vec::<TxOutcome>::new()));
        let fired_in = Arc::clone(&fired);
        let tail: Box<Tail> = Box::new(move |_call: &Call<'_>| {
            let fired_in = Arc::clone(&fired_in);
            let cb: TxSyncCallback = Box::new(move |outcome: TxOutcome| {
                let fired_in = Arc::clone(&fired_in);
                Box::pin(async move {
                    fired_in.lock().unwrap().push(outcome);
                    Ok(())
                }) as BoxFuture<'static, Result<(), LeafError>>
            });
            let _ = crate::register_synchronization(TxPhase::AfterRollback, cb);
            Box::pin(async move {
                Err(AdviceError::AroundBody(LeafError::new(ErrorKind::ValidationError)))
            }) as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });

        block(chain.invoke(&call, &*tail)).expect_err("err");
        assert_eq!(mgr.rollbacks(), 1);
        assert_eq!(*fired.lock().unwrap(), vec![TxOutcome::RolledBack], "AFTER_ROLLBACK fired on rollback");
    }

    #[test]
    fn a_result_err_return_rolls_back_via_the_classifier() {
        // A method returning Result<i64, LeafError> reports failure THROUGH Ok(ErasedRet)
        // (the chain packs the whole Result). The return classifier detects the Err
        // and rolls back, but the Result::Err value still passes through to the caller.
        let mgr = Arc::new(InMemoryTransactionManager::new());
        let interceptor: Arc<dyn Interceptor> = Arc::new(
            TransactionInterceptor::new(mgr.clone(), TxAttribute::DEFAULT)
                .with_return_classifier(result_classifier::<i64>()),
        );
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let (method, bean) = call_key();
        let call = Call::new(method, bean, ErasedArgs::none(), &target, &cx);

        // The method returns Err (a business failure), packed into the ErasedRet.
        let tail: Box<Tail> = Box::new(|_call: &Call<'_>| {
            let r: Result<i64, LeafError> = Err(LeafError::new(ErrorKind::ValidationError));
            Box::pin(async move { Ok(ErasedRet::pack(r)) })
                as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        let out = block(chain.invoke(&call, &*tail)).expect("the chain itself succeeds");
        // The method's Result::Err is returned to the caller unchanged.
        let returned: Result<i64, LeafError> = out.unpack().unwrap();
        assert!(returned.is_err(), "the method's Result::Err passes through");
        // …but the tx ROLLED BACK because of the business failure.
        assert_eq!(mgr.rollbacks(), 1, "a Result::Err return rolls the tx back");
        assert_eq!(mgr.commits(), 0);
    }

    #[test]
    fn a_result_ok_return_commits_via_the_classifier() {
        let mgr = Arc::new(InMemoryTransactionManager::new());
        let interceptor: Arc<dyn Interceptor> = Arc::new(
            TransactionInterceptor::new(mgr.clone(), TxAttribute::DEFAULT)
                .with_return_classifier(result_classifier::<i64>()),
        );
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let (method, bean) = call_key();
        let call = Call::new(method, bean, ErasedArgs::none(), &target, &cx);
        let tail: Box<Tail> = Box::new(|_call: &Call<'_>| {
            let r: Result<i64, LeafError> = Ok(99);
            Box::pin(async move { Ok(ErasedRet::pack(r)) })
                as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        let out = block(chain.invoke(&call, &*tail)).expect("ok");
        let returned: Result<i64, LeafError> = out.unpack().unwrap();
        assert_eq!(returned.unwrap(), 99);
        assert_eq!(mgr.commits(), 1, "a Result::Ok return commits");
        assert_eq!(mgr.rollbacks(), 0);
    }

    #[test]
    fn nested_join_does_not_begin_a_second_tx() {
        // An OUTER tx interceptor wraps an INNER one (both REQUIRED). The inner JOINs
        // (no second begin/commit); only the outer demarcates.
        let mgr = Arc::new(InMemoryTransactionManager::new());
        let outer: Arc<dyn Interceptor> =
            Arc::new(TransactionInterceptor::new(mgr.clone(), TxAttribute::DEFAULT));
        let inner: Arc<dyn Interceptor> =
            Arc::new(TransactionInterceptor::new(mgr.clone(), TxAttribute::DEFAULT));
        let chain = AdviceChain::new(Box::new([outer, inner]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let (method, bean) = call_key();
        let call = Call::new(method, bean, ErasedArgs::none(), &target, &cx);

        let tail: Box<Tail> = Box::new(|_call: &Call<'_>| {
            Box::pin(async { Ok(ErasedRet::pack(1_i64)) })
                as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        block(chain.invoke(&call, &*tail)).expect("ok");
        assert_eq!(mgr.begins(), 1, "only the OUTER tx began (the inner joined)");
        assert_eq!(mgr.commits(), 1, "only the outer committed");
    }

    #[test]
    fn the_resource_id_is_stable_within_one_demarcation() {
        // The same TxResource (same id) is visible across the whole body.
        let mgr = Arc::new(InMemoryTransactionManager::new());
        let interceptor: Arc<dyn Interceptor> =
            Arc::new(TransactionInterceptor::new(mgr.clone(), TxAttribute::DEFAULT));
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let (method, bean) = call_key();
        let call = Call::new(method, bean, ErasedArgs::none(), &target, &cx);
        let seen = Arc::new(Mutex::new(None::<u64>));
        let seen_in = Arc::clone(&seen);
        let tail: Box<Tail> = Box::new(move |_call: &Call<'_>| {
            if let Some(st) = crate::current_tx()
                && let Some(r) = TxResource::from_state(&st)
            {
                *seen_in.lock().unwrap() = Some(r.id);
            }
            Box::pin(async { Ok(ErasedRet::pack(0_i64)) })
                as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        block(chain.invoke(&call, &*tail)).unwrap();
        assert_eq!(*seen.lock().unwrap(), Some(0), "the body saw the begun tx resource");
        let _ = TypeId::of::<Nop>();
    }
}
