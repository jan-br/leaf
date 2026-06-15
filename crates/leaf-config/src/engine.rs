//! The plan/apply engine (environment-config `config-data`, R2 plan-then-apply).
//!
//! The async planner does ALL IO and emits an ordered [`ConfigDataPlan`] of
//! rung-tagged contributions; a pure-sync applier folds it onto the
//! [`leaf_core::EnvBuilder`] stack — eliminating the `&mut`-across-`.await`
//! borrow because IO holds no stack borrow and the fold holds no `.await`.
//!
//! [`apply`] is the pure sync fold (the half this crate fully owns + tests).
//! [`plan_sync`] is the local-file planner over the [`crate::SyncConfigDataLoader`]
//! facet: it picks the winning loader per location by `handles` DATA, loads, and
//! tags each document with its rung. The genuinely-async planner (remote sources,
//! `spring.config.import` traversal, `CondExpr` document activation) is leaf-boot's
//! `seal_environment` body — this crate provides the deterministic local core.

use std::sync::Arc;

use leaf_core::{EnvBuilder, MapPropertySource};

use crate::error::{ConfigDataError, ConfigDataLocation};
use crate::loader::{LoadCtx, SyncConfigDataLoader};
use crate::precedence::{ConfigDataPlan, Contribution, PrecedenceRung};

/// One entry of the location worklist the planner folds.
pub struct PlanItem<'a> {
    /// The location to load.
    pub location: ConfigDataLocation,
    /// The rung to tag the resulting contribution(s) at.
    pub rung: PrecedenceRung,
    /// Inline document text (test/snapshot path), else load from disk.
    pub inline: Option<&'a str>,
}

impl<'a> PlanItem<'a> {
    /// A worklist item loading `raw_loc` at `rung` from disk.
    #[must_use]
    pub fn new(raw_loc: &str, rung: PrecedenceRung) -> Self {
        PlanItem {
            location: ConfigDataLocation::parse(raw_loc),
            rung,
            inline: None,
        }
    }

    /// A worklist item with inline document text (no filesystem access).
    #[must_use]
    pub fn inline(raw_loc: &str, rung: PrecedenceRung, text: &'a str) -> Self {
        PlanItem {
            location: ConfigDataLocation::parse(raw_loc),
            rung,
            inline: Some(text),
        }
    }
}

/// Plan a worklist of locations synchronously over the local-file loaders.
///
/// For each item: pick the FIRST loader whose `handles` claims the location
/// (selection by DATA, never link order), load it, and emit one rung-tagged
/// [`Contribution`] per resulting document. A location no loader claims is a
/// loud [`ConfigDataError`] (`no_loader`) — never a silent empty stack.
///
/// `discovery_index` is assigned by WORKLIST order (deterministic), so the
/// plan is reproducible regardless of IO timing.
///
/// # Errors
/// A [`ConfigDataError`] from any loader (malformed doc, missing required
/// location), or `no_loader` if a location is unclaimed.
pub fn plan_sync(
    loaders: &[&dyn SyncConfigDataLoader],
    worklist: &[PlanItem<'_>],
) -> Result<ConfigDataPlan, ConfigDataError> {
    let mut plan = ConfigDataPlan::new();
    let mut index: u32 = 0;
    for item in worklist {
        let loader = loaders
            .iter()
            .find(|l| l.handles(&item.location))
            .ok_or_else(|| ConfigDataError::no_loader(item.location.raw()))?;
        let cx = match item.inline {
            Some(text) => LoadCtx::inline(text),
            None => LoadCtx::new(),
        };
        let docs = loader.load_sync(&item.location, &cx)?;
        for (doc_no, doc) in docs.into_iter().enumerate() {
            let source_name = if doc_no == 0 {
                item.location.raw().to_string()
            } else {
                format!("{}#{doc_no}", item.location.raw())
            };
            plan.push(Contribution::new(item.rung, index, source_name, doc.props));
            index += 1;
        }
    }
    Ok(plan)
}

/// The pure, sync applier: fold a [`ConfigDataPlan`] onto the builder stack.
///
/// Each contribution becomes one [`MapPropertySource`]; they are added in
/// HIGHEST-precedence-FIRST order (the plan's `sorted` order), so the builder's
/// `add_last` chain yields a first-source-wins stack matching the rung ladder.
/// Holds NO `.await` — the async/sync bisection the design mandates.
pub fn apply(plan: ConfigDataPlan, builder: &mut EnvBuilder) {
    for contribution in plan.sorted() {
        let source = MapPropertySource::new(
            Arc::<str>::from(contribution.source_name.as_str()),
            contribution.props,
        );
        builder.add_last(Arc::new(source));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{EnvVarLoader, JsonLoader, YamlLoader};
    use leaf_core::PropertyResolver;

    fn loaders() -> (JsonLoader, YamlLoader, EnvVarLoader) {
        (JsonLoader, YamlLoader, EnvVarLoader::from_snapshot([]))
    }

    #[test]
    fn plan_sync_no_loader_is_loud() {
        let json = JsonLoader;
        let ls: Vec<&dyn SyncConfigDataLoader> = vec![&json];
        let work = vec![PlanItem::inline(
            "config.toml",
            PrecedenceRung::ConfigDataFile {
                group: 0,
                profile_specific: false,
                external: false,
            },
            "x=1",
        )];
        let err = plan_sync(&ls, &work).unwrap_err();
        assert_eq!(err.kind, crate::error::ConfigDataErrorKind::NoLoader);
    }

    #[test]
    fn end_to_end_layered_precedence_env_overrides_file() {
        // A YAML file sets server.port=8080; the OS env sets SERVER_PORT=9090.
        // OsEnv is a higher rung, so it must win.
        let (json, yaml, env) = loaders();
        let _ = json;
        let ls: Vec<&dyn SyncConfigDataLoader> = vec![&yaml, &env];
        let group0 = PrecedenceRung::ConfigDataFile {
            group: 0,
            profile_specific: false,
            external: false,
        };
        let work = vec![
            PlanItem::inline("application.yaml", group0, "server:\n  port: 8080\n  host: file-host\n"),
            PlanItem::inline("env:", PrecedenceRung::OsEnv, "SERVER_PORT=9090\n"),
        ];
        let plan = plan_sync(&ls, &work).unwrap();
        let mut builder = EnvBuilder::new();
        apply(plan, &mut builder);
        let env = builder.seal_env();

        // env overrides file for the port…
        assert_eq!(env.get_required("server.port").unwrap().raw, "9090");
        // …but the file-only key still resolves.
        assert_eq!(env.get_required("server.host").unwrap().raw, "file-host");
    }

    #[test]
    fn json_and_yaml_both_parse_into_the_stack() {
        let json = JsonLoader;
        let yaml = YamlLoader;
        let ls: Vec<&dyn SyncConfigDataLoader> = vec![&json, &yaml];
        let group0 = PrecedenceRung::ConfigDataFile {
            group: 0,
            profile_specific: false,
            external: false,
        };
        let group1 = PrecedenceRung::ConfigDataFile {
            group: 1,
            profile_specific: false,
            external: false,
        };
        let work = vec![
            PlanItem::inline("base.yaml", group0, "a: from-yaml\nshared: yaml\n"),
            PlanItem::inline("over.json", group1, r#"{"b":"from-json","shared":"json"}"#),
        ];
        let plan = plan_sync(&ls, &work).unwrap();
        let mut builder = EnvBuilder::new();
        apply(plan, &mut builder);
        let env = builder.seal_env();
        assert_eq!(env.get_required("a").unwrap().raw, "from-yaml");
        assert_eq!(env.get_required("b").unwrap().raw, "from-json");
        // group1 (json) out-ranks group0 (yaml).
        assert_eq!(env.get_required("shared").unwrap().raw, "json");
    }

    #[test]
    fn relaxed_binding_integration_env_key_resolves_kebab() {
        // The env loader maps DB_POOL_SIZE -> db.pool.size; a relaxed kebab
        // lookup (db.pool-size) must resolve it through leaf-core's uniform fold.
        let env_loader = EnvVarLoader::from_snapshot([]);
        let ls: Vec<&dyn SyncConfigDataLoader> = vec![&env_loader];
        let work = vec![PlanItem::inline("env:", PrecedenceRung::OsEnv, "DB_POOL_SIZE=25\n")];
        let plan = plan_sync(&ls, &work).unwrap();
        let mut builder = EnvBuilder::new();
        apply(plan, &mut builder);
        let env = builder.seal_env();
        // Typed relaxed read.
        let n: Option<u32> = env.get_as("db.pool-size").unwrap();
        assert_eq!(n, Some(25));
    }
}
