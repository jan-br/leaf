//! Integration test `[boot-live-self-check]`: the anti-DCE expected-vs-found
//! self-check runs LIVE inside the run pipeline over the binary's
//! `ExpectedManifest` (NOT the empty manifest), so a force-linked-but-zero-
//! contributing crate is a LOUD `AntiDceError::SourceVanished` naming it, while a
//! healthy app (every expected crate `declare_source!`s its tag) passes
//! (bootstrap-diagnostics phase3/14, ADR-09 Defense MANIFEST).
//!
//! PROOF GATE: this drives the REAL `Application::run` pipeline (a tokio spawner,
//! the genuine link-collected `SOURCES` slice), not a hand-called `self_check`. The
//! `Application::with_expected_sources` manifest is what the umbrella feeds from
//! `leaf::expected_manifest()` + the binary crate name.

use std::sync::Arc;

use leaf_boot::{Application, RunOverlay, SealInputs};
use leaf_core::{ErrorKind, SourceTag};

/// Drive a run with the given expected manifest; return the run result.
async fn run_with_expected(expected: Vec<SourceTag>) -> Result<(), leaf_core::LeafError> {
    leaf_tokio::install_ambient_store().ok();
    let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());
    Application::new()
        .with_name("self-check-app")
        .with_spawner(spawner)
        .with_expected_sources(expected)
        .run(SealInputs::new(), RunOverlay::none())
        .await
        .map(|_running| ())
        .map_err(|failure| failure.error)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zero_contributing_expected_crate_is_a_loud_source_vanished_through_run() {
    // The ExpectedManifest names a crate that contributed ZERO SourceTags to the
    // link-collected SOURCES (a real DCE drop / a never-force-linked crate). The
    // LIVE self-check inside `run` fails — never a silent empty registry later.
    let err = run_with_expected(vec![SourceTag("leaf-ghost-never-force-linked")])
        .await
        .expect_err("a vanished expected source faults the run pipeline");
    assert_eq!(err.kind, ErrorKind::AntiDce, "it is the AntiDce defense");
    assert!(
        err.to_string().contains("leaf-ghost-never-force-linked"),
        "the diagnostic names the vanished crate: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_present_expected_crate_passes_the_live_self_check() {
    // leaf-tokio is force-linked here (the spawner path-references it) and
    // `declare_source!`s "leaf-tokio" in its crate root, so an ExpectedManifest
    // naming it passes the LIVE self-check — the healthy-app path, NOT the empty
    // manifest. (If leaf-tokio had no declare_source! anchor this would be a false
    // SourceVanished — the exact gap this wiring closes.)
    run_with_expected(vec![SourceTag("leaf-tokio")])
        .await
        .expect("a present, contributing expected crate passes the live self-check");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bare_app_with_an_empty_manifest_still_passes() {
    // The default (no expected sources) is trivially green — nothing can vanish.
    run_with_expected(vec![]).await.expect("an empty manifest always passes");
}
