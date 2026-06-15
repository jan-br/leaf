//! THE leaf-tx AUTO-ADVISE PROOF-GATE: a REAL annotated app whose
//! `#[component]` + `#[advisable]` service with `#[transactional]` methods is
//! AUTO-ADVISED end-to-end through `Application::new().run()` — proving the
//! Infrastructure tx advisor leaf-tx ships AUTO-WIRES from the NATURAL annotation
//! (commit on `Ok`, rollback on `Err`, the advice running in `TX_ORDER`).
//!
//! What is user code (the NATURAL declarative annotation — NO `#[aspect]`, NO
//! hand-written `ADVISOR_PAIRINGS` row):
//! - a `register_component!` `LedgerTxManager` — a `TransactionManager` bean wrapping
//!   leaf-tx's no-op [`InMemoryTransactionManager`] (a real datastore manager would
//!   be its own ordinary bean; a local newtype is needed only because the orphan
//!   rule forbids `#[component]`-ing a foreign type from this test crate);
//! - a `#[component]` + `#[advisable]` `LedgerService` whose methods carry
//!   `#[transactional(manager = LedgerTxManager)]` and return `Result<i64, LeafError>`
//!   (`record_ok` commits, `record_err` rolls back) — the ADVISED bean.
//!
//! The `#[transactional]` annotation on each `#[advisable]`-impl method is what the
//! impl-block macro lowers to the const `ADVISOR_PAIRINGS` row (the tx advisor keyed
//! by the bean's `TypeId`, binding the manager + the `Result<i64,_>` return classifier)
//! — so `Application::run` AUTO-COLLECTS the tx advisor with NO `.with_advisors`. The
//! test then `invoke_advised`-es each method and asserts the manager saw the right
//! begin/commit/rollback — the demarcation the auto-installed `TransactionInterceptor`
//! drove from the declarative annotation.

use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{
    BoxFuture, ContractId, ErasedArgs, ErrorKind, LeafError, MethodKey, ResolveCtx,
    TransactionManager, TxDefinition, TxState, TxSyncRegistry,
};
// `#[transactional]` is NOT imported: it is a per-method MARKER the `#[advisable]`
// impl macro STRIPS + lowers (the impl-block macro owns the row), so — exactly like
// `#[bean]` inside `#[configuration] impl` — it is consumed before attribute-macro
// resolution and needs no import.
use leaf_macros::{advisable, component, register_component};
use leaf_tx::InMemoryTransactionManager;

// ─────────────────────── the tx manager bean ────────────────────────────────

/// A [`TransactionManager`] bean: a thin local newtype delegating to leaf-tx's
/// no-op [`InMemoryTransactionManager`] (the orphan rule forbids `#[component]`-ing
/// the foreign type directly). Registered via `register_component!` (constructed
/// via `::new()`, no field-injection) since its `inner` field is owned state, not
/// an injected dependency. Exposes the begin/commit/rollback counts so the test can
/// assert the auto-installed interceptor demarcated.
#[derive(Debug)]
struct LedgerTxManager {
    inner: InMemoryTransactionManager,
}
register_component!(LedgerTxManager);

impl LedgerTxManager {
    fn new() -> Self {
        LedgerTxManager { inner: InMemoryTransactionManager::new() }
    }
    fn begins(&self) -> usize {
        self.inner.begins()
    }
    fn commits(&self) -> usize {
        self.inner.commits()
    }
    fn rollbacks(&self) -> usize {
        self.inner.rollbacks()
    }
}

impl TransactionManager for LedgerTxManager {
    fn begin<'a>(
        &'a self,
        def: &'a TxDefinition,
        cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<TxState, LeafError>> {
        self.inner.begin(def, cx)
    }
    fn commit(&self, st: TxState) -> BoxFuture<'_, Result<(), LeafError>> {
        self.inner.commit(st)
    }
    fn rollback(&self, st: TxState) -> BoxFuture<'_, Result<(), LeafError>> {
        self.inner.rollback(st)
    }
    fn synchronizations<'a>(&'a self, st: &'a TxState) -> &'a TxSyncRegistry {
        self.inner.synchronizations(st)
    }
}

// ───────────────────────── the advised service bean ─────────────────────────

/// A `@Component` service whose methods are TRANSACTIONAL via the NATURAL
/// `#[transactional]` annotation (advised by the tx advisor the annotation auto-wires).
/// `record_ok` returns `Ok` (→ commit); `record_err` returns `Err` (→ rollback). Both
/// return `Result<i64, LeafError>` so the auto-emitted return classifier can detect the
/// business outcome.
#[component]
#[derive(Debug)]
struct LedgerService;

#[advisable]
impl LedgerService {
    fn new() -> Self {
        LedgerService
    }

    /// A successful transactional write → the tx commits. The `#[transactional]`
    /// annotation auto-wires the tx advisor (binding the `LedgerTxManager` bean + the
    /// `Result<i64,_>` rollback classifier) keyed by this bean's `TypeId`.
    #[transactional(manager = LedgerTxManager)]
    fn record_ok(&self, amount: i64) -> Result<i64, LeafError> {
        Ok(amount + 1)
    }

    /// A failing transactional write → the tx rolls back (the default rule: any
    /// `Err` rolls back), and the `Err` is still returned to the caller.
    #[transactional(manager = LedgerTxManager)]
    fn record_err(&self, _amount: i64) -> Result<i64, LeafError> {
        Err(LeafError::new(ErrorKind::ValidationError))
    }
}

// ─────────────────────────────── the milestone ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transactional_bean_auto_advises_through_run_and_commits_or_rolls_back() {
    leaf_tokio::install_ambient_store().ok();
    let module = module_path!();
    let service_contract = ContractId::of(&format!("{module}::LedgerService"));

    let spawner: Arc<dyn leaf_core::Spawner> =
        Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // Drive the FULL run pipeline with NOTHING but the natural annotations.
    let running = Application::new()
        .with_name("ledger-app")
        .with_spawner(spawner)
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app auto-wires and runs to Ready");

    // The tx advisor rows auto-collected from the `#[transactional]` annotations: each
    // is a per-method-unique row whose contract is the tx family base @ the
    // module-qualified `Bean::method` (so two transactional beans/methods never collide
    // in the row index).
    let record_ok_contract = ContractId::of(&format!(
        "leaf::tx::TransactionAdvisor@{module}::LedgerService::record_ok"
    ));
    assert!(
        leaf_core::collect_slice(&leaf_core::ADVISOR_PAIRINGS)
            .iter()
            .any(|r| r.contract == record_ok_contract),
        "the tx Infrastructure advisor row for `record_ok` auto-collected from #[transactional]"
    );

    // The service is AUTOMATICALLY advised (the proxy installed at R4).
    let svc_id = running
        .context()
        .engine()
        .registry()
        .by_contract(service_contract)
        .expect("LedgerService in registry");
    assert!(
        running.is_advised(svc_id),
        "the #[transactional] #[advisable] bean is AUTOMATICALLY advised by the auto-wired tx advisor"
    );

    // The SAME shared manager singleton the auto-installed interceptor resolves.
    let mgr = running
        .context()
        .get::<LedgerTxManager>()
        .await
        .expect("the tx manager resolves");
    assert_eq!(mgr.begins(), 0, "no tx yet (the advised methods have not been called)");

    // ── COMMIT path: an Ok-returning transactional method commits ──
    let ok = running
        .invoke_advised(
            svc_id,
            MethodKey::of("LedgerService::record_ok"),
            ErasedArgs::pack((41_i64,)),
        )
        .await
        .expect("the advised call routes through the auto-installed tx chain");
    let ok_ret: Result<i64, LeafError> = ok.unpack().expect("the Result<i64,_> return");
    assert_eq!(ok_ret.expect("Ok"), 42, "the real method ran (41 + 1)");
    assert_eq!(mgr.begins(), 1, "a tx was begun for the advised call");
    assert_eq!(mgr.commits(), 1, "Ok → commit");
    assert_eq!(mgr.rollbacks(), 0, "no rollback on the Ok path");

    // ── ROLLBACK path: an Err-returning transactional method rolls back ──
    let err = running
        .invoke_advised(
            svc_id,
            MethodKey::of("LedgerService::record_err"),
            ErasedArgs::pack((7_i64,)),
        )
        .await
        .expect("the chain itself succeeds (the method's Err rides the return)");
    let err_ret: Result<i64, LeafError> = err.unpack().expect("the Result<i64,_> return");
    assert!(err_ret.is_err(), "the method's Result::Err passes through to the caller");
    assert_eq!(mgr.begins(), 2, "a second tx was begun");
    assert_eq!(mgr.commits(), 1, "the failing call did NOT commit");
    assert_eq!(mgr.rollbacks(), 1, "Err → rollback (the default rule)");

    // ── shutdown drains cleanly ──
    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf_core::RunState::Closed, "the context closed");
}
