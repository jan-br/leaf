//! THE leaf-tx AUTO-ADVISE PROOF-GATE: a REAL annotated app whose
//! `#[component]` + `#[advisable]` service with a "transactional" method is
//! AUTO-ADVISED end-to-end through `Application::new().run()` — proving the
//! Infrastructure tx advisor leaf-tx ships AUTO-WIRES (commit on `Ok`, rollback on
//! `Err`, the advice running in `TX_ORDER`).
//!
//! What is user code (annotations + one slice row):
//! - a `#[component]` `LedgerTxManager` — a `TransactionManager` bean wrapping
//!   leaf-tx's no-op [`InMemoryTransactionManager`] (a real datastore manager would
//!   be its own ordinary bean; a local newtype is needed only because the orphan
//!   rule forbids `#[component]`-ing a foreign type from this test crate);
//! - a `#[component]` + `#[advisable]` `LedgerService` whose methods return
//!   `Result<i64, LeafError>` (`record_ok` commits, `record_err` rolls back) — the
//!   ADVISED bean;
//! - ONE const `ADVISOR_PAIRINGS` row the binary submits, exactly like `#[aspect]`
//!   emits — so `Application::run` AUTO-COLLECTS the tx advisor with NO
//!   hand-assembled `.with_advisors`.
//!
//! Everything else (the proxy plan, the chain install at R4, the `make_interceptor`
//! resolving the manager through the container) is the run pipeline's auto-wiring.
//! The test then `invoke_advised`-es each method and asserts the manager saw the
//! right begin/commit/rollback — the demarcation the auto-installed
//! `TransactionInterceptor` drove.

use std::any::TypeId;
use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{
    BoxFuture, ContractId, ErasedArgs, ErrorKind, LeafError, MethodKey, ResolveCtx,
    TransactionManager, TxDefinition, TxState, TxSyncRegistry,
};
use leaf_macros::{advisable, component, register_component};
use leaf_tx::{tx_advisor_contract, InMemoryTransactionManager, TxPointcut};

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

/// A `@Component` service whose methods are TRANSACTIONAL (advised by the tx
/// advisor). `record_ok` returns `Ok` (→ commit); `record_err` returns `Err`
/// (→ rollback). Both return `Result<i64, LeafError>` so the tx advisor's return
/// classifier can detect the business outcome.
#[component]
#[derive(Debug)]
struct LedgerService;

#[advisable]
impl LedgerService {
    fn new() -> Self {
        LedgerService
    }

    /// A successful transactional write → the tx commits.
    fn record_ok(&self, amount: i64) -> Result<i64, LeafError> {
        Ok(amount + 1)
    }

    /// A failing transactional write → the tx rolls back (the default rule: any
    /// `Err` rolls back), and the `Err` is still returned to the caller.
    fn record_err(&self, _amount: i64) -> Result<i64, LeafError> {
        Err(LeafError::new(ErrorKind::ValidationError))
    }
}

// ───────────────────── the AUTO-WIRED tx advisor row ────────────────────────

// The tx advisor matches LedgerService by its concrete TypeId (the recursion-safe
// pointcut — it never advises the manager bean itself). The const TypeId-of seam
// mints the 'static slice exactly as a `#[transactional]` macro would.
static ADVISED_TYPES: [TypeId; 1] = [const { TypeId::of::<LedgerService>() }];
static TX_POINTCUT: TxPointcut = TxPointcut::new(&ADVISED_TYPES, &[]);

// THE auto-wire row: one const `AdvisorPairingRow` in `ADVISOR_PAIRINGS` (the same
// channel `Application::run` collects `#[aspect]` rows from), binding the manager
// bean + the i64-return classifier. No `.with_advisors` in the run call. The
// `make_interceptor` is a non-capturing closure literal (const-promoted to the bare
// fn-pointer exactly like the `#[aspect]` codegen emits), deferring the generic
// `make_transaction_interceptor_for` build to call time at R4.
#[leaf_core::linkme::distributed_slice(leaf_core::ADVISOR_PAIRINGS)]
#[linkme(crate = leaf_core::linkme)]
static TX_ADVISOR_ROW: leaf_core::AdvisorPairingRow = leaf_core::AdvisorPairingRow {
    contract: ContractId::of("leaf::tx::TransactionAdvisor"),
    order: leaf_core::OrderKey {
        value: leaf_core::TX_ORDER,
        source: leaf_core::OrderSource::Interface,
    },
    role: leaf_core::Role::Infrastructure,
    pointcut: &TX_POINTCUT,
    make_interceptor: |c| {
        leaf_tx::make_transaction_interceptor_for::<LedgerTxManager, i64>()(c)
    },
};

// ─────────────────────────────── the milestone ──────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transactional_bean_auto_advises_through_run_and_commits_or_rolls_back() {
    leaf_tokio::install_ambient_store().ok();
    let module = module_path!();
    let service_contract = ContractId::of(&format!("{module}::LedgerService"));

    let spawner: Arc<dyn leaf_core::Spawner> =
        Arc::new(leaf_tokio::TokioExecutionFacility::new());

    // Drive the FULL run pipeline with NOTHING but annotations + the one slice row.
    let running = Application::new()
        .with_name("ledger-app")
        .with_spawner(spawner)
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .expect("the app auto-wires and runs to Ready");

    // The tx advisor row auto-collected (the headline: it is in ADVISOR_PAIRINGS).
    assert!(
        leaf_core::collect_slice(&leaf_core::ADVISOR_PAIRINGS)
            .iter()
            .any(|r| r.contract == tx_advisor_contract()),
        "the tx Infrastructure advisor row auto-collected from ADVISOR_PAIRINGS"
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
        "the #[advisable] bean is AUTOMATICALLY advised by the auto-collected tx advisor"
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
