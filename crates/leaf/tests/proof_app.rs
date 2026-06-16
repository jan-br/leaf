//! THE UMBRELLA PROOF APP — a real leaf application written against ONLY the
//! `leaf` umbrella crate (phase3 TOPOLOGY "Starters & BOM": "the blessed path").
//!
//! It proves the umbrella delivers a working app from its public surface ALONE:
//!
//! 1. `use leaf::prelude::*;` brings the annotation macros + the handle currency +
//!    the run engine into scope (no hyphenated `leaf-core`/`leaf-macros`/`leaf-boot`
//!    dependency named — this test crate depends on `leaf` only);
//! 2. `#[component]` / `register_component!` / `#[config_properties]` expand through
//!    the umbrella's re-exported macros, contributing their `linkme` rows;
//! 3. `leaf::bootstrap(name)` wires the DEFAULT tokio `ExecutionFacility` (the base
//!    runtime) + installs the ambient store, and `Application::run` AUTO-COLLECTS
//!    every per-bean channel from the slices — so the graph wires + the config bean
//!    binds with NO hand-assembled tables;
//! 4. `leaf::RunInputs` lowers argv into the env fence; the app runs to Ready and
//!    shuts down cleanly.
//!
//! This is the downstream "single dep a downstream app names" contract, end to end.

use leaf::prelude::*;

// ─────────────────────────── the user's app beans ───────────────────────────

/// A `@Component` repository constructed via `Repository::new()` (the
/// no-injected-collaborator `register_component!` form) — the dependency target.
#[derive(Debug)]
struct Repository {
    name: &'static str,
}

impl Repository {
    fn new() -> Self {
        Repository { name: "order" }
    }
}
register_component!(Repository);

/// A `@Component` service depending on the [`Repository`] (constructor injection
/// over the `Ref<Repository>` field) — the live dependency-graph edge.
#[component]
#[derive(Debug)]
struct OrderService {
    repo: Ref<Repository>,
}

/// A `@ConfigurationProperties` type bound from `app.*` — AUTO-REGISTERED + bound +
/// resolvable purely from the macro-emitted slices (no hand seed/descriptor/thunk).
#[config_properties(prefix = "app")]
#[derive(Debug, Default, PartialEq, Eq)]
struct AppProps {
    title: String,
    workers: u16,
}

// ─────────────────────────────── the milestone ───────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_umbrella_runs_a_real_app_from_the_prelude_alone() {
    // The blessed path: bootstrap the default-runtime Application + run it. The
    // tokio ExecutionFacility (the base runtime) is wired by `leaf::bootstrap`; the
    // per-bean wiring auto-collects from the linkme slices inside `run`.
    let running = leaf::bootstrap("order-app")
        .run(
            leaf::RunInputs::new()
                .with_args(["--app.title=Orders", "--app.workers=4"])
                .into(),
            leaf::boot::RunOverlay::none(),
        )
        .await
        .expect("the app runs to Ready");

    // (1) the GRAPH wired: the Service injected the Repository.
    let service = running.context().get::<OrderService>().await.expect("OrderService resolves");
    assert_eq!(service.repo.name, "order", "the Repository was injected into the Service");

    // (2) the @ConfigurationProperties bean bound from the command-line env.
    let props = running.context().get::<AppProps>().await.expect("AppProps resolves");
    assert_eq!(props.title, "Orders", "AppProps bound app.title");
    assert_eq!(props.workers, 4, "AppProps bound app.workers");

    // (3) the app reached Running; readiness flipped at Ready.
    assert_eq!(running.unit().run_state(), leaf::core::RunState::Running);
    assert_eq!(
        running.unit().availability().readiness(),
        leaf::core::ReadinessState::AcceptingTraffic,
    );

    // (4) shutdown drains cleanly (the LIFO teardown ledger).
    let report = running.shutdown().await;
    assert_eq!(report.run_state, leaf::core::RunState::Closed, "the context closed");
    assert!(report.shutdown.is_clean(), "the teardown ledger drained with no faults");
}

// ── the force-link + ExpectedManifest seam is reachable from the umbrella ──

// The app's main can invoke the force-link shim at MODULE scope (it emits `use …
// as _;` items). With no capability feature it expands to nothing; with `redis`/
// `web` on it path-references the enabled integration crates from THIS binary
// crate (the Layer-0 anti-DCE anchor). It must compile under every feature set.
leaf::force_link!();

#[test]
fn the_force_link_macro_and_manifest_seam_are_reachable() {
    // The ExpectedManifest seam is exposed for the binary-crate anti-DCE self-check.
    let manifest = leaf::forcelink::expected_manifest();
    let crates = leaf::forcelink::participating_crates();

    // The manifest is always a SourceTag mirror of the participating set.
    assert_eq!(manifest.len(), crates.len());

    #[cfg(not(any(feature = "redis", feature = "web")))]
    {
        // The base app's feature-gated participating set is empty (the binary adds
        // its own SourceTag; the base crates link through the umbrella's own edges).
        assert!(crates.is_empty());
    }
    #[cfg(feature = "redis")]
    {
        // The redis capability pulls leaf-redis (+ its tokio peer) into the set, and
        // the manifest mirrors it as a SourceTag for the self-check.
        assert!(crates.contains(&"leaf-redis"), "got: {crates:?}");
        assert!(manifest.iter().any(|t| t.0 == "leaf-redis"), "got: {manifest:?}");
    }
    #[cfg(all(feature = "web", not(feature = "redis")))]
    {
        // The web stack pulls its curated bundle (incl. leaf-validation) into the set.
        assert!(crates.contains(&"leaf-validation"), "got: {crates:?}");
        assert!(manifest.iter().any(|t| t.0 == "leaf-validation"), "got: {manifest:?}");
    }
}
