//! `HyperServerAutoConfig` — the DEFAULT embedded-`WebServer` `#[auto_config]`
//! integration: an [`AUTO_CONFIGS`](leaf_core::AUTO_CONFIGS) row at
//! [`CandidateRole::FALLBACK`](leaf_core::CandidateRole) contributing the hyper-backed
//! [`HyperServer`] as the `Arc<dyn ::leaf_web::WebServer>` bean (Spring's embedded-server
//! auto-configuration: `@Bean WebServer` over a pluggable Tomcat/Netty), guarded by
//! `OnMissingBean(dyn WebServer)`.
//!
//! ## The back-off contract (the swappable backend)
//!
//! The whole point of the leaf-web abstraction boundary is that the backend is SWAPPABLE:
//! `leaf-web` names no HTTP server; `leaf-web-hyper` contributes ITS [`HyperServer`] as the
//! DEFAULT, and a different backend (or a user's hand-rolled `WebServer`) supersedes it by
//! providing the same `dyn ::leaf_web::WebServer` view. That is exactly the `FALLBACK`
//! candidate role + `OnMissingBean(dyn WebServer)` VIEW back-off the cache/tx defaults use:
//! the auto-config registers IFF no `dyn WebServer` bean already exists, and a user backend
//! (any concrete type providing the view) wins.
//!
//! ## The `#[auto_config] impl` form (Spring's @AutoConfiguration + @Bean methods)
//!
//! The `#[auto_config] impl HyperServerAutoConfig` block (a `#[bean(name = "webServer",
//! provides = "dyn ::leaf_web::WebServer")]` method gated by
//! `#[conditional(on_missing_bean(dyn ::leaf_web::WebServer))]` that returns a
//! `HyperServer`) emits the SAME const artifacts a hand-built auto-config would: the
//! `AUTO_CONFIGS` [`Descriptor`] at `FALLBACK` (carrying the `dyn WebServer` view plus the
//! `"webServer"` declared name), its [`ProviderSeed`] and `SEED_PAIRINGS` JOIN, and the
//! `#[conditional]` guard with its `GUARD_PAIRINGS` and `CONDITIONS` anchors — all keyed on
//! the ONE contributed contract (`module_path!()::web_server`). The holder is a managed
//! `#[component]` (the `&self` receiver the `#[bean]` method reads — singleton-correct), and
//! the [`EmbeddedWebServer`](leaf_web::EmbeddedWebServer) injects the resolved
//! `Ref<dyn WebServer>` and serves on it.

use leaf_core::{CondExpr, ContractId, Descriptor, ProviderSeed};

use crate::server::HyperServer;

/// The declared name of the contributed `Arc<dyn WebServer>` bean (Spring's embedded
/// `webServer` identity, shared with any backend that supersedes it).
pub const WEB_SERVER_BEAN: &str = "webServer";

/// The stable contract path of the web auto-config's contributed server bean — the contract
/// the `#[auto_config] impl` macro mints from `module_path!()::web_server` (the ONE contract
/// the `Descriptor`, the `SeedPairingRow`, and the `GuardPairingRow` share).
pub const WEB_SERVER_CONTRACT: &str = "leaf_web_hyper::autoconfig::web_server";

// ───────────────────────── the #[auto_config] holder ─────────────────────────

/// The auto-configuration HOLDER (a managed `#[component]` singleton). The
/// `#[auto_config] impl` block below contributes the hyper [`WebServer`](leaf_web::WebServer)
/// from a `#[bean]` method that reads this holder as its `&self` receiver.
#[leaf_macros::component]
pub struct HyperServerAutoConfig;

impl HyperServerAutoConfig {
    /// The no-collaborator constructor the `#[component]` provider calls.
    #[must_use]
    pub fn new() -> Self {
        HyperServerAutoConfig
    }
}

impl Default for HyperServerAutoConfig {
    fn default() -> Self {
        HyperServerAutoConfig::new()
    }
}

// ──────────────────── the @bean-method web-server contribution ─────────────────

/// The `@Bean`-method contribution: `web_server()` produces the concrete [`HyperServer`]
/// exposed as `dyn ::leaf_web::WebServer` (the `provides[]` view), named `"webServer"`, into
/// `AUTO_CONFIGS` at `FALLBACK`, gated by `OnMissingBean(dyn ::leaf_web::WebServer)` (the
/// `provides[]`-aware VIEW back-off — ANY `WebServer` bean supersedes it).
#[leaf_macros::auto_config]
impl HyperServerAutoConfig {
    /// Build the default hyper-backed web server (the factory body the macro calls). A
    /// different backend (or a user `WebServer`) is an ordinary bean that supersedes this
    /// `FALLBACK` default via the `dyn WebServer` view; this is the blessed default so an
    /// app linking `leaf-web-hyper` serves with NO hand-written server bean.
    #[bean(name = "webServer", provides = "dyn ::leaf_web::WebServer")]
    #[conditional(on_missing_bean(dyn ::leaf_web::WebServer))]
    fn web_server(&self) -> HyperServer {
        HyperServer::new()
    }
}

// ───────────────────────── thin compatibility aliases ────────────────────────
//
// The macro emits the contributed bean's `ProviderSeed` as `__leaf_seed_web_server` and its
// back-off guard as `__leaf_guard_web_server` (keyed off the METHOD ident), and submits the
// `Descriptor` into the `AUTO_CONFIGS` slice. These thin aliases preserve a stable public
// surface over the macro-emitted artifacts AND act as the anti-DCE anchors that
// path-reference the macro-emitted statics (so the row reaches the slice even under
// `--gc-sections`), mirroring leaf-cache's `CACHE_MANAGER_SEED` / `CACHE_AUTO_CONFIG_GUARD`.

/// The const [`ProviderSeed`] leaf-boot's `run_autoconfig` invokes ONCE to mint the hyper
/// server's `Provider` (the macro-emitted seed).
pub const WEB_SERVER_SEED: ProviderSeed = __leaf_seed_web_server;

/// The const back-off guard for the default hyper server: it registers at `FALLBACK` IFF no
/// `dyn WebServer` bean already exists (the macro-emitted `#[conditional]` guard tree — a
/// single `OnMissingBean(dyn WebServer)` VIEW leaf, no `OnProperty`).
pub static WEB_SERVER_AUTO_CONFIG_GUARD: CondExpr = __leaf_guard_web_server;

/// The contributed `AUTO_CONFIGS` [`Descriptor`] for the default hyper server (looked up from
/// the macro-emitted `AUTO_CONFIGS` slice row by its contributed contract) — at `FALLBACK`,
/// on the SEPARATE auto-config channel, carrying the `dyn WebServer` view.
#[must_use]
pub fn web_server_descriptor() -> Descriptor {
    *leaf_core::AUTO_CONFIGS
        .iter()
        .find(|d| d.contract == ContractId::of(WEB_SERVER_CONTRACT))
        .expect("the #[auto_config] web_server Descriptor must reach the AUTO_CONFIGS slice")
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_boot::{run_autoconfig, AutoConfigCandidate, ExclusionSet};
    use leaf_conditions::ConditionKind;
    use leaf_core::{
        ActiveProfiles, CandidateRole, Env, EnvBuilder, MapPropertySource, RegistryBuilder,
    };
    use std::any::TypeId;
    use std::sync::Arc;

    fn empty_env() -> Env {
        let src = MapPropertySource::from_pairs("test", std::iter::empty::<(String, String)>());
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(src));
        b.seal_env()
    }

    /// The holder `#[component]` Descriptor (the `@bean` method reads it as `&self`, so the
    /// container must manage it).
    fn holder_descriptor() -> Descriptor {
        *leaf_core::COMPONENTS
            .iter()
            .find(|d| d.declared_name == Some("hyperServerAutoConfig"))
            .expect("the holder is a COMPONENTS row")
    }

    /// Build the one auto-config candidate from the macro-emitted const artifacts
    /// (Descriptor + seed + guard) — exactly the JOIN leaf-boot performs over the slices.
    fn web_server_candidate() -> AutoConfigCandidate {
        AutoConfigCandidate::new(
            web_server_descriptor(),
            __leaf_seed_web_server,
            Some(&__leaf_guard_web_server),
        )
    }

    /// Drive the real `run_autoconfig` ladder over the holder + the contributed candidate,
    /// with a `seed_probe` standing in for already-present beans (the OnMissingBean input).
    fn run(seed_probe: &[(TypeId, CandidateRole)]) -> (usize, RegistryBuilder) {
        let env = empty_env();
        let mut builder = RegistryBuilder::new();
        builder
            .register(holder_descriptor(), __leaf_seed_HyperServerAutoConfig())
            .expect("the holder component registers");
        let cands = [web_server_candidate()];
        let out = run_autoconfig(
            &cands,
            &env,
            &mut builder,
            &ExclusionSet::new(),
            &ActiveProfiles::default(),
            seed_probe,
        )
        .expect("the ladder runs");
        (out.registered, builder)
    }

    #[test]
    fn descriptor_is_a_fallback_auto_config_with_the_web_server_view() {
        let d = web_server_descriptor();
        assert_eq!(
            d.meta.candidate_role,
            CandidateRole::FALLBACK,
            "an auto-config registers at FALLBACK so a user/backend bean supersedes it"
        );
        assert_eq!(d.role, leaf_core::Role::Application);
        // The product is the CONCRETE HyperServer (the method return type).
        assert_eq!(d.self_type, TypeId::of::<HyperServer>());
        assert_eq!(d.declared_name, Some(WEB_SERVER_BEAN));
        // It provides the dyn WebServer view (the runner resolves Ref<dyn WebServer>).
        assert!(
            d.provides.iter().any(|r| r.view == TypeId::of::<dyn leaf_web::WebServer>()),
            "the auto-config must declare the dyn WebServer view"
        );
    }

    #[test]
    fn the_auto_config_rides_the_separate_auto_configs_channel_not_components() {
        let contract = ContractId::of(WEB_SERVER_CONTRACT);
        let autos = leaf_core::collect_slice(&leaf_core::AUTO_CONFIGS);
        assert!(
            autos.iter().any(|r| r.contract == contract),
            "the default web server must be an AUTO_CONFIGS row"
        );
        let comps = leaf_core::collect_slice(&leaf_core::COMPONENTS);
        assert!(
            !comps.iter().any(|r| r.contract == contract),
            "the auto-config contribution must NOT be in COMPONENTS"
        );
    }

    #[test]
    fn the_guard_is_a_bare_on_missing_bean_dyn_view_leaf() {
        // A single comma-less #[conditional(on_missing_bean(dyn ..))] lowers to a BARE leaf
        // whose `type` Attr targets the `dyn WebServer` VIEW TypeId — so the back-off fires
        // when ANY WebServer bean (any concrete type) is present.
        match &WEB_SERVER_AUTO_CONFIG_GUARD {
            CondExpr::Leaf(id, attrs) => {
                assert_eq!(*id, leaf_conditions::OnMissingBean::ID, "the lone guard leaf is OnMissingBean");
                let view = attrs
                    .iter()
                    .find_map(|a| match a {
                        leaf_core::Attr::Type("type", t) => Some(*t),
                        _ => None,
                    })
                    .expect("the OnMissingBean leaf carries a `type` Attr");
                assert_eq!(
                    view,
                    TypeId::of::<dyn leaf_web::WebServer>(),
                    "the back-off targets the dyn WebServer VIEW, not a concrete type"
                );
            }
            other => panic!("expected a bare OnMissingBean Leaf; got {other:?}"),
        }
    }

    #[test]
    fn registers_at_fallback_when_no_web_server_exists() {
        // No user/backend WebServer present → the guard matches → the hyper default wires.
        let (registered, builder) = run(&[]);
        assert_eq!(registered, 1, "unclaimed dyn WebServer → the hyper default wires");
        assert_eq!(builder.len(), 2, "the holder + the contributed hyper server");
    }

    #[test]
    fn a_user_web_server_supersedes_the_fallback_hyper_one() {
        // The swappable-backend payoff: a `dyn WebServer` bean already present (any concrete
        // type providing the view) → OnMissingBean(dyn WebServer) sees it → the hyper
        // default backs off. (The probe carries the dyn-view TypeId, the VIEW back-off key.)
        let (registered, builder) = run(&[(
            TypeId::of::<dyn leaf_web::WebServer>(),
            CandidateRole::NORMAL,
        )]);
        assert_eq!(registered, 0, "OnMissingBean backs off: a user/backend WebServer wins");
        assert_eq!(builder.len(), 1, "only the holder; the hyper default backed off");
    }
}
