//! `seal_environment` — the 5f async fence at the `App<Define>→App<Resolve>` edge.
//!
//! environment-config (phase3/06) + bootstrap-diagnostics (phase3/14): the body of
//! `App<Define>::seal_environment().await`. ALL config IO completes here, the env
//! snapshots into the immutable [`leaf_core::Env`], and the entire
//! `App<Resolve>`/`seal()`/`App<Wired>` chain that follows reads it lock-free.
//!
//! The fence performs five jobs, in order:
//!
//! 1. **argv parse** — the raw argv is parsed ONCE into the
//!    [`leaf_core::ApplicationArguments`] (the kernel's pure `--name[=value]`
//!    grammar), and a command-line [`leaf_core::PropertySource`] is synthesized
//!    from its valued options at the highest operational precedence (so
//!    `--server.port=8080` wins over a file, and `--leaf.main.*` overrides work
//!    schema-independently).
//! 2. **config-data load** (the `spring.config.import`-analogue) — the supplied
//!    location worklist is driven through leaf-config's [`ConfigDataLoader`]s
//!    (async, on the bootstrap executor here) RECURSIVELY: a `leaf.config.import`
//!    key inside a loaded document discovers + folds in the imported location(s),
//!    with a visited-set so an import CYCLE is idempotent. This is TWO-PHASE: the
//!    documents are collected first (always-active ones seed profile resolution),
//!    then re-filtered against the resolved active profiles — a loaded document
//!    carrying `leaf.config.activate.on-profile` is DROPPED unless its profile
//!    expression is active, and a profile-activated document that sets a reserved
//!    early-binding key is a loud `IllegalActivationDocument` error.
//! 3. **profile activation** — [`leaf_core::ProfileLevers`] are harvested from the
//!    always-active stack and resolved ONCE via [`leaf_core::resolve_active`] into
//!    the canonical [`leaf_core::ActiveProfiles`], BEFORE any condition evaluates
//!    AND before the document-activation filter (phase-2) runs.
//! 4. **`bindToApplication`** — the `leaf.main.*` subtree is bound onto the live
//!    [`leaf_core::BootstrapSettings`] record (declarative-seal, NOT live-object
//!    mutation; config = last write wins over programmatic defaults).
//! 5. **snapshot** — the [`leaf_core::EnvBuilder`] is consumed into the immutable
//!    [`leaf_core::Env`]; a post-seal source push is type-unrepresentable.
//!
//! The argv parse + command-line source ordering are pure and unit-testable; the
//! config-data drive is the only `.await` (the cold IO fence).

use std::ffi::OsString;
use std::sync::Arc;

use leaf_core::{
    resolve_active, ActiveProfiles, ApplicationArguments, BannerMode, BootstrapSettings,
    EnvBuilder, MapPropertySource, Origin, PropertyResolver, PropertyValue, ProfileLevers,
    StartupValidation,
};

use leaf_config::{apply, ConfigDataError, ConfigDataLoader, LoadCtx, PrecedenceRung};
use leaf_config::{
    illegal_activation_error, illegal_activation_key, is_document_active, ConfigDataLocation,
    ConfigDataPlan, Contribution, DocControl,
};

/// The stable name of the synthesized command-line property source.
pub const COMMAND_LINE_SOURCE: &str = "commandLineArgs";
/// The stable name of the programmatic-defaults property source (`leaf.main.*`
/// set in code, the lowest operational rung).
pub const PROGRAMMATIC_SOURCE: &str = "programmaticDefaults";

/// One location to load during the config-data pass (the `spring.config.import`
/// worklist item). `inline` carries document text for the test/snapshot path
/// (no filesystem access); else the loader reads `raw_location` from disk.
#[derive(Clone, Debug)]
pub struct ImportLocation {
    /// The raw location string (`application.yaml`, `config.json`, …).
    pub raw_location: String,
    /// The precedence rung the resulting document(s) sit at.
    pub rung: PrecedenceRung,
    /// Inline document text (test path), else `None` to read from disk.
    pub inline: Option<String>,
}

impl ImportLocation {
    /// A worklist item loading `raw` at the bundled `application.yaml` rung.
    #[must_use]
    pub fn file(raw: impl Into<String>) -> Self {
        ImportLocation {
            raw_location: raw.into(),
            rung: PrecedenceRung::ConfigDataFile {
                group: 0,
                profile_specific: false,
                external: false,
            },
            inline: None,
        }
    }

    /// A worklist item with inline document text (no filesystem access).
    #[must_use]
    pub fn inline(raw: impl Into<String>, rung: PrecedenceRung, text: impl Into<String>) -> Self {
        ImportLocation {
            raw_location: raw.into(),
            rung,
            inline: Some(text.into()),
        }
    }
}

/// The frozen product of [`seal_environment`]: the sealed read view + the three
/// records every downstream `App<Resolve>` step consults.
///
/// `env` is the lock-free read handle; `args` backs the command-line source +
/// the injectable singleton + runner args; `settings` is the frozen `leaf.main.*`
/// self-binding; `profiles` is the canonical active set computed up-front so the
/// first condition reads a fully-materialized snapshot.
#[derive(Clone, Debug)]
pub struct SealedEnvironment {
    /// The sealed environment read handle.
    pub env: leaf_core::Env,
    /// The parsed command-line arguments (Arc-shared).
    pub args: ApplicationArguments,
    /// The frozen `leaf.main.*` self-binding record.
    pub settings: BootstrapSettings,
    /// The canonical active-profile set (resolved up-front).
    pub profiles: ActiveProfiles,
}

/// The inputs to [`seal_environment`] (a small owned bundle so the fence
/// signature stays stable as later units add fields).
#[derive(Default)]
pub struct SealInputs {
    /// The raw argv (excluding the program name).
    pub argv: Vec<OsString>,
    /// The config-data import worklist (the `spring.config.import`-analogue).
    pub imports: Vec<ImportLocation>,
    /// Programmatic `leaf.main.*` (and other) defaults — the lowest operational
    /// rung, overridden by config + command-line.
    pub programmatic: Vec<(String, String)>,
}

impl SealInputs {
    /// An empty input bundle.
    #[must_use]
    pub fn new() -> Self {
        SealInputs::default()
    }

    /// Set the raw argv (already as `String`s, the common in-process path).
    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.argv = args.into_iter().map(Into::into).collect();
        self
    }

    /// Add one config-data import location.
    #[must_use]
    pub fn with_import(mut self, loc: ImportLocation) -> Self {
        self.imports.push(loc);
        self
    }

    /// Add one programmatic default key/value.
    #[must_use]
    pub fn with_programmatic(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.programmatic.push((key.into(), value.into()));
        self
    }
}

/// Build a command-line [`leaf_core::PropertySource`] from the parsed arguments.
///
/// Each valued option becomes one property (`--server.port=8080` →
/// `server.port=8080`); a multi-valued option is joined CSV (Spring's default
/// list adaptation). A bare flag with no value contributes the empty string
/// (present-and-wins), so `--debug` is `debug=` (truthy under `OnProperty`'s
/// not-literally-"false" rule). Flags + the command-line origin are stamped so a
/// diagnostic can name the source.
#[must_use]
pub fn command_line_source(args: &ApplicationArguments) -> MapPropertySource {
    let origin = Origin::Native { crate_name: Some("leaf-boot::command-line") };
    let entries = args.option_names().map(|name| {
        let values = args.option_values(name);
        let raw = if values.is_empty() {
            String::new()
        } else {
            values.join(",")
        };
        (name.to_string(), PropertyValue::with_origin(raw, origin))
    });
    MapPropertySource::new(Arc::<str>::from(COMMAND_LINE_SOURCE), entries)
}

/// One config-data document collected during the recursive worklist traversal,
/// carrying everything the two-phase orchestration needs: its precedence rung +
/// deterministic discovery index, its sealed source name, its parsed control
/// subset (on-profile gate + declared imports), and its flattened props.
struct CollectedDoc {
    rung: PrecedenceRung,
    index: u32,
    source_name: String,
    control: DocControl,
    props: Vec<(String, leaf_core::PropertyValue)>,
}

/// PHASE 1 — drive the config-data import worklist through leaf-config's loaders
/// RECURSIVELY (the `leaf.config.import`-analogue traversal), collecting every
/// loaded document in deterministic discovery order.
///
/// A `leaf.config.import` key INSIDE a loaded document discovers + folds in the
/// imported location(s); a `visited` set keyed by the RESOLVED location raw
/// string makes an import CYCLE idempotent (each location loads at most once, no
/// infinite loop, no double-load). Imported documents sit at the next group's
/// `external` rung so they out-rank the importer (Spring's import precedence).
///
/// Async because a genuinely-remote loader's `load` is a `BoxFuture` (the cold
/// IO fence runs on the bootstrap executor); the local-file inline/snapshot path
/// resolves without touching the filesystem. Selection is by `handles` DATA,
/// never link order; a location no loader claims is a loud `ConfigDataError`.
///
/// NOTE: no document-activation filter or illegal-activation check runs here —
/// that is PHASE 2 ([`build_filtered_plan`]), which needs the resolved active
/// profiles. This phase only DISCOVERS + LOADS (so the always-active docs can
/// seed profile resolution).
async fn collect_documents(
    loaders: &[&dyn ConfigDataLoader],
    imports: &[ImportLocation],
) -> Result<Vec<CollectedDoc>, ConfigDataError> {
    let mut collected = Vec::new();
    let mut index: u32 = 0;
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();

    // The recursion worklist: (location, rung, inline-text, group). The seed
    // imports keep their declared rung + group; a discovered import descends one
    // group + flips `external` so it out-ranks its importer.
    let mut work: std::collections::VecDeque<(ConfigDataLocation, PrecedenceRung, Option<String>)> =
        imports
            .iter()
            .map(|item| {
                (
                    ConfigDataLocation::parse(&item.raw_location),
                    item.rung,
                    item.inline.clone(),
                )
            })
            .collect();

    while let Some((location, rung, inline)) = work.pop_front() {
        // Idempotent visited-set keyed by the resolved location raw string.
        if !visited.insert(location.raw().to_string()) {
            continue;
        }
        let loader = loaders
            .iter()
            .find(|l| l.handles(&location))
            .ok_or_else(|| ConfigDataError::no_loader(location.raw()))?;
        let cx = match &inline {
            Some(text) => LoadCtx::inline(text),
            None => LoadCtx::new(),
        };
        let docs = loader.load(&location, &cx).await?;
        for (doc_no, doc) in docs.into_iter().enumerate() {
            let source_name = if doc_no == 0 {
                location.raw().to_string()
            } else {
                format!("{}#{doc_no}", location.raw())
            };
            let control = DocControl::parse(&doc.props);
            // Enqueue this document's declared imports (recursive discovery).
            // Imported docs descend one group + are `external` so they out-rank
            // the importer; inline text never propagates (imports name a loader-
            // resolvable location, read fresh).
            //
            // A PROFILE-ACTIVATED document's import is NOT followed here: an
            // import inside an activated doc is a reserved early-binding key, so
            // it is either dropped (inactive doc) or a loud illegal-activation
            // error (active doc) — both decided in phase 2. Following it eagerly
            // would surface the wrong (load-IO) fault before that check.
            if !control.is_profile_activated() {
                let import_rung = next_import_rung(rung);
                for raw in &control.imports {
                    work.push_back((ConfigDataLocation::parse(raw), import_rung, None));
                }
            }
            collected.push(CollectedDoc {
                rung,
                index,
                source_name,
                control,
                props: doc.props,
            });
            index += 1;
        }
    }
    Ok(collected)
}

/// The rung a document discovered via `leaf.config.import` sits at: one group
/// above its importer, flagged `external`, so an imported file out-ranks the
/// importing file (the Spring import-precedence rule). A non-`ConfigDataFile`
/// rung (e.g. an env/cmdline-seeded import) keeps its rung verbatim.
fn next_import_rung(parent: PrecedenceRung) -> PrecedenceRung {
    match parent {
        PrecedenceRung::ConfigDataFile { group, .. } => PrecedenceRung::ConfigDataFile {
            group: group.saturating_add(1),
            profile_specific: false,
            external: true,
        },
        other => other,
    }
}

/// Fold the always-active (NON-profile-gated) documents onto `builder`. The
/// profile levers harvested off this stack (plus the command-line source already
/// on `builder`) resolve the active profiles BEFORE the activation filter runs.
fn apply_always_active(docs: &[CollectedDoc], builder: &mut EnvBuilder) {
    let mut plan = ConfigDataPlan::new();
    for doc in docs {
        if doc.control.is_profile_activated() {
            continue;
        }
        plan.push(Contribution::new(
            doc.rung,
            doc.index,
            doc.source_name.clone(),
            doc.props.clone(),
        ));
    }
    apply(plan, builder);
}

/// PHASE 2 — build the FINAL plan over the resolved active profiles: apply the
/// document-activation filter (drop a gated doc whose on-profile expression is
/// inactive) + enforce the illegal-activation hard rule (a profile-activated
/// doc that sets a reserved early-binding key is a loud error).
///
/// A profile-specific (activated) document that survives the filter sits at its
/// `profile_specific` rung so it out-ranks the plain documents at its group.
fn build_filtered_plan(
    docs: Vec<CollectedDoc>,
    active: &leaf_core::ActiveProfiles,
) -> Result<ConfigDataPlan, ConfigDataError> {
    let mut plan = ConfigDataPlan::new();
    for doc in docs {
        if !is_document_active(&doc.control, active, &doc.source_name)? {
            continue;
        }
        if doc.control.is_profile_activated() {
            // Hard rule: an activated doc may not set a reserved early key.
            if let Some(key) = illegal_activation_key(&doc.control, &doc.props) {
                return Err(illegal_activation_error(&doc.source_name, key));
            }
        }
        let rung = if doc.control.is_profile_activated() {
            promote_profile_specific(doc.rung)
        } else {
            doc.rung
        };
        plan.push(Contribution::new(rung, doc.index, doc.source_name, doc.props));
    }
    Ok(plan)
}

/// Promote a document's rung to `profile_specific` (an activated document
/// out-ranks the plain documents at its group — the last-profile-wins ladder).
fn promote_profile_specific(rung: PrecedenceRung) -> PrecedenceRung {
    match rung {
        PrecedenceRung::ConfigDataFile { group, external, .. } => PrecedenceRung::ConfigDataFile {
            group,
            profile_specific: true,
            external,
        },
        other => other,
    }
}

/// Harvest the [`ProfileLevers`] from the (partly-)sealed stack: the relaxed
/// `leaf.profiles.active` / `leaf.profiles.include` list properties + the
/// `leaf.profiles.default` fallback.
fn harvest_profile_levers(env: &leaf_core::Env) -> ProfileLevers {
    let list = |key: &str| -> Vec<Arc<str>> {
        env.get(key)
            .map(|rv| {
                rv.raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(Arc::<str>::from)
                    .collect()
            })
            .unwrap_or_default()
    };
    let default: Arc<str> = env
        .get("leaf.profiles.default")
        .map(|rv| Arc::<str>::from(rv.raw.as_str()))
        .unwrap_or_else(|| Arc::<str>::from("default"));
    ProfileLevers {
        active: list("leaf.profiles.active"),
        include: list("leaf.profiles.include"),
        groups: std::collections::HashMap::new(),
        default,
    }
}

/// Bind the `leaf.main.*` subtree onto a [`BootstrapSettings`] record
/// (`bindToApplication`): config = last write wins over the
/// [`BootstrapSettings::DEFAULT`] programmatic baseline. Each field reads through
/// the relaxed env view (`leaf.main.banner-mode` / `LEAF_MAIN_BANNERMODE` alias
/// to one identity).
fn bind_to_application(env: &leaf_core::Env) -> BootstrapSettings {
    let mut s = BootstrapSettings::DEFAULT;

    let flag = |key: &str, default: bool| -> bool {
        env.get_as::<bool>(key).ok().flatten().unwrap_or(default)
    };

    if let Some(rv) = env.get("leaf.main.banner-mode") {
        s.banner_mode = match rv.raw.to_ascii_lowercase().as_str() {
            "off" => BannerMode::Off,
            "log" => BannerMode::Log,
            _ => BannerMode::Console,
        };
    }
    s.lazy_initialization = flag("leaf.main.lazy-initialization", s.lazy_initialization);
    s.allow_bean_definition_overriding =
        flag("leaf.main.allow-bean-definition-overriding", s.allow_bean_definition_overriding);
    s.allow_circular_references =
        flag("leaf.main.allow-circular-references", s.allow_circular_references);
    s.register_shutdown_hook = flag("leaf.main.register-shutdown-hook", s.register_shutdown_hook);
    s.keep_alive = flag("leaf.main.keep-alive", s.keep_alive);

    if let Some(rv) = env.get("leaf.main.startup-validation") {
        s.startup_validation = match rv.raw.to_ascii_lowercase().as_str() {
            "lenient" => StartupValidation::Lenient,
            "skip" => StartupValidation::Skip,
            _ => StartupValidation::Strict,
        };
    }

    s
}

/// Run the 5f environment fence with the default leaf-config format loaders.
///
/// The common entry point: drives JSON/YAML/config-tree/env imports. See
/// [`seal_environment_with`] to supply a custom loader set.
///
/// # Errors
/// A [`ConfigDataError`] (config-data fault) or a [`leaf_core::ProfileError`]
/// (profile-activation cycle / invalid name), both lifted into the one
/// [`leaf_core::LeafError`] spine.
pub async fn seal_environment(inputs: SealInputs) -> Result<SealedEnvironment, leaf_core::LeafError> {
    let json = leaf_config::JsonLoader;
    let yaml = leaf_config::YamlLoader;
    let env = leaf_config::EnvVarLoader::from_snapshot([]);
    let loaders: [&dyn ConfigDataLoader; 3] = [&json, &yaml, &env];
    seal_environment_with(inputs, &loaders).await
}

/// Run the 5f environment fence over an explicit loader set (the test/embedder
/// seam).
///
/// # Errors
/// A [`ConfigDataError`] or [`leaf_core::ProfileError`], lifted into
/// [`leaf_core::LeafError`].
pub async fn seal_environment_with(
    inputs: SealInputs,
    loaders: &[&dyn ConfigDataLoader],
) -> Result<SealedEnvironment, leaf_core::LeafError> {
    // ── 1. argv → ApplicationArguments + the highest-precedence cmdline source ──
    let args = ApplicationArguments::parse(inputs.argv)?;

    let cmdline: Arc<dyn leaf_core::PropertySource> = Arc::new(command_line_source(&args));

    // Programmatic defaults are the LOWEST operational rung — built once here so
    // both the provisional (phase-1) and final (phase-2) stacks share it.
    let programmatic: Option<Arc<dyn leaf_core::PropertySource>> = (!inputs.programmatic.is_empty())
        .then(|| {
            let origin = Origin::Native { crate_name: Some("leaf-boot::programmatic") };
            let prog = MapPropertySource::new(
                Arc::<str>::from(PROGRAMMATIC_SOURCE),
                inputs
                    .programmatic
                    .into_iter()
                    .map(|(k, v)| (k, PropertyValue::with_origin(v, origin))),
            );
            Arc::new(prog) as Arc<dyn leaf_core::PropertySource>
        });

    // ── 2. config-data load PHASE 1 — recursive import discovery + collect ─────
    // The `leaf.config.import` worklist is driven RECURSIVELY (visited-set keyed
    // by resolved location → cycle-idempotent); no activation filter yet (it
    // needs the active profiles, which the always-active docs below resolve).
    let docs = collect_documents(loaders, &inputs.imports).await?;

    // ── 3. profile activation — resolve ONCE over (cmdline + always-active docs +
    //       programmatic), the inputs that can carry profile levers BEFORE the
    //       document-activation filter runs (Spring's two-phase resolve).
    let mut provisional_builder = EnvBuilder::new();
    provisional_builder.add_first(cmdline.clone());
    apply_always_active(&docs, &mut provisional_builder);
    if let Some(prog) = &programmatic {
        provisional_builder.add_last(prog.clone());
    }
    let provisional = provisional_builder.seal_env();
    let levers = harvest_profile_levers(&provisional);
    let profiles = resolve_active(levers, false)?;

    // ── 4. config-data PHASE 2 — apply the document-activation filter + the
    //       illegal-activation hard rule against the now-resolved profiles, then
    //       fold the FINAL plan onto the real stack.
    let plan = build_filtered_plan(docs, &profiles)?;
    let mut builder = EnvBuilder::new();
    builder.add_first(cmdline);
    apply(plan, &mut builder);
    if let Some(prog) = programmatic {
        builder.add_last(prog);
    }
    let env = builder.seal_env();

    // ── 5. bindToApplication (leaf.main.* → frozen BootstrapSettings) ──────────
    let settings = bind_to_application(&env);

    // ── 6. snapshot the Env (the seal fence) ───────────────────────────────────
    // The view IS the sealed env (the builder is consumed); no source push is
    // representable after this point.
    Ok(SealedEnvironment {
        env,
        args,
        settings,
        profiles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_config::JsonLoader;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        // The inline loaders resolve without pending; futures' bare executor
        // (a dev-dependency) drives the cold IO fence in the unit tests.
        futures::executor::block_on(f)
    }

    #[test]
    fn argv_is_parsed_into_application_arguments_and_a_cmdline_source() {
        let sealed = block_on(seal_environment(
            SealInputs::new().with_args(["--server.port=8080", "--name=leaf"]),
        ))
        .expect("seals");
        assert_eq!(sealed.args.option_values("server.port"), &["8080".to_string()]);
        // The command-line source resolves through the env read seam.
        assert_eq!(sealed.env.get("server.port").unwrap().raw, "8080");
    }

    #[test]
    fn command_line_beats_a_config_file_value() {
        // The file sets server.port=1111; the command line overrides to 8080.
        let json = JsonLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&json];
        let inputs = SealInputs::new()
            .with_args(["--server.port=8080"])
            .with_import(ImportLocation::inline(
                "application.json",
                PrecedenceRung::ConfigDataFile { group: 0, profile_specific: false, external: false },
                r#"{"server":{"port":"1111"}}"#,
            ));
        let sealed = block_on(seal_environment_with(inputs, &loaders)).expect("seals");
        assert_eq!(
            sealed.env.get("server.port").unwrap().raw,
            "8080",
            "command-line precedence beats the config file"
        );
    }

    #[test]
    fn config_file_value_is_read_when_no_cmdline_override() {
        let json = JsonLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&json];
        let inputs = SealInputs::new().with_import(ImportLocation::inline(
            "application.json",
            PrecedenceRung::ConfigDataFile { group: 0, profile_specific: false, external: false },
            r#"{"db":{"url":"postgres://x"}}"#,
        ));
        let sealed = block_on(seal_environment_with(inputs, &loaders)).expect("seals");
        assert_eq!(sealed.env.get("db.url").unwrap().raw, "postgres://x");
    }

    #[test]
    fn profiles_are_activated_from_the_command_line() {
        let sealed = block_on(seal_environment(
            SealInputs::new().with_args(["--leaf.profiles.active=prod,eu"]),
        ))
        .expect("seals");
        assert!(sealed.profiles.contains("prod"));
        assert!(sealed.profiles.contains("eu"));
        // No default when explicit profiles activate.
        assert!(!sealed.profiles.contains("default"));
    }

    #[test]
    fn default_profile_activates_when_nothing_explicit() {
        let sealed = block_on(seal_environment(SealInputs::new())).expect("seals");
        assert!(sealed.profiles.contains("default"));
    }

    #[test]
    fn leaf_main_subtree_binds_onto_bootstrap_settings() {
        let sealed = block_on(seal_environment(SealInputs::new().with_args([
            "--leaf.main.banner-mode=off",
            "--leaf.main.lazy-initialization=true",
            "--leaf.main.startup-validation=lenient",
        ])))
        .expect("seals");
        assert_eq!(sealed.settings.banner_mode, BannerMode::Off);
        assert!(sealed.settings.lazy_initialization);
        assert_eq!(sealed.settings.startup_validation, StartupValidation::Lenient);
        // Unset fields keep the default.
        assert!(sealed.settings.register_shutdown_hook);
    }

    #[test]
    fn config_beats_programmatic_for_leaf_main() {
        // Programmatic default says console; config (command line) says off — config wins.
        let sealed = block_on(seal_environment(
            SealInputs::new()
                .with_programmatic("leaf.main.banner-mode", "console")
                .with_args(["--leaf.main.banner-mode=off"]),
        ))
        .expect("seals");
        assert_eq!(sealed.settings.banner_mode, BannerMode::Off);
    }

    #[test]
    fn an_unclaimed_import_location_is_loud() {
        let json = JsonLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&json];
        // A `.toml` location no JSON loader claims.
        let inputs = SealInputs::new().with_import(ImportLocation::file("config.toml"));
        let err = block_on(seal_environment_with(inputs, &loaders))
            .expect_err("an unclaimed location is loud, never a silent empty stack");
        let _ = err;
    }

    #[test]
    fn flag_option_is_present_and_truthy() {
        let sealed = block_on(seal_environment(SealInputs::new().with_args(["--debug"])))
            .expect("seals");
        // A bare flag is present (empty string), which is truthy by OnProperty's rule.
        assert_eq!(sealed.env.get("debug").unwrap().raw, "");
    }

    // ── config-data orchestration: on-profile activation filter ───────────────

    fn group0() -> PrecedenceRung {
        PrecedenceRung::ConfigDataFile {
            group: 0,
            profile_specific: false,
            external: false,
        }
    }

    #[test]
    fn on_profile_document_is_dropped_when_profile_inactive() {
        // A multi-doc YAML: the base doc is always-active; the second doc gates
        // on `prod`, which is NOT active (no profile activated). The gated doc's
        // key must NOT be present.
        let yaml = leaf_config::YamlLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&yaml];
        let inputs = SealInputs::new().with_import(ImportLocation::inline(
            "application.yaml",
            group0(),
            "base.key: base-val\n---\nleaf.config.activate.on-profile: prod\nprod.key: prod-val\n",
        ));
        let sealed = block_on(seal_environment_with(inputs, &loaders)).expect("seals");
        assert_eq!(sealed.env.get("base.key").unwrap().raw, "base-val");
        assert!(
            sealed.env.get("prod.key").is_none(),
            "an inactive on-profile document must be dropped"
        );
    }

    #[test]
    fn on_profile_document_is_kept_when_profile_active() {
        // Same file, but `--leaf.profiles.active=prod` makes the gated doc active.
        let yaml = leaf_config::YamlLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&yaml];
        let inputs = SealInputs::new()
            .with_args(["--leaf.profiles.active=prod"])
            .with_import(ImportLocation::inline(
                "application.yaml",
                group0(),
                "base.key: base-val\n---\nleaf.config.activate.on-profile: prod\nprod.key: prod-val\n",
            ));
        let sealed = block_on(seal_environment_with(inputs, &loaders)).expect("seals");
        assert_eq!(sealed.env.get("base.key").unwrap().raw, "base-val");
        assert_eq!(
            sealed.env.get("prod.key").unwrap().raw,
            "prod-val",
            "an active on-profile document must be kept"
        );
    }

    #[test]
    fn on_profile_document_uses_full_profile_algebra() {
        // Expression `prod & !legacy`: active under {prod}, inactive under {prod,legacy}.
        let yaml = leaf_config::YamlLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&yaml];
        let text = "base.key: base\n---\nleaf.config.activate.on-profile: prod & !legacy\ngated.key: on\n";
        let active = block_on(seal_environment_with(
            SealInputs::new()
                .with_args(["--leaf.profiles.active=prod"])
                .with_import(ImportLocation::inline("application.yaml", group0(), text)),
            &loaders,
        ))
        .expect("seals");
        assert_eq!(active.env.get("gated.key").unwrap().raw, "on");

        let inactive = block_on(seal_environment_with(
            SealInputs::new()
                .with_args(["--leaf.profiles.active=prod,legacy"])
                .with_import(ImportLocation::inline("application.yaml", group0(), text)),
            &loaders,
        ))
        .expect("seals");
        assert!(inactive.env.get("gated.key").is_none());
    }

    // ── config-data orchestration: recursive import traversal ─────────────────

    fn write_temp(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "leaf-cfgimport-{}-{}",
            std::process::id(),
            name.replace(['/', '.'], "_")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn recursive_import_folds_in_imported_locations() {
        // base.json imports child.json (by file path); child.json's key must
        // appear in the sealed env.
        let json = JsonLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&json];
        let child = write_temp("child.json", r#"{"child":{"key":"from-child"}}"#);
        let base_text = format!(
            r#"{{"base":{{"key":"from-base"}},"leaf":{{"config":{{"import":"{}"}}}}}}"#,
            child.display()
        );
        let base = write_temp("base.json", &base_text);
        let inputs =
            SealInputs::new().with_import(ImportLocation::file(base.display().to_string()));
        let sealed = block_on(seal_environment_with(inputs, &loaders)).expect("seals");
        assert_eq!(sealed.env.get("base.key").unwrap().raw, "from-base");
        assert_eq!(
            sealed.env.get("child.key").unwrap().raw,
            "from-child",
            "the recursively-imported document must be folded in"
        );
    }

    #[test]
    fn import_cycle_is_idempotent_no_infinite_loop() {
        // a.json imports b.json; b.json imports a.json. The load must terminate
        // and each location loads at most once (the visited-set keyed by resolved
        // location). We assert both keys resolve and the call returns.
        let json = JsonLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&json];
        // Pre-declare the paths so each file can name the other.
        let dir = std::env::temp_dir().join(format!("leaf-cfgcycle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.json");
        let b = dir.join("b.json");
        std::fs::write(
            &a,
            format!(
                r#"{{"a":{{"key":"a-val"}},"leaf":{{"config":{{"import":"{}"}}}}}}"#,
                b.display()
            ),
        )
        .unwrap();
        std::fs::write(
            &b,
            format!(
                r#"{{"b":{{"key":"b-val"}},"leaf":{{"config":{{"import":"{}"}}}}}}"#,
                a.display()
            ),
        )
        .unwrap();
        let inputs =
            SealInputs::new().with_import(ImportLocation::file(a.display().to_string()));
        let sealed = block_on(seal_environment_with(inputs, &loaders))
            .expect("an import cycle terminates idempotently");
        assert_eq!(sealed.env.get("a.key").unwrap().raw, "a-val");
        assert_eq!(sealed.env.get("b.key").unwrap().raw, "b-val");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── config-data orchestration: illegal-activation hard rule ───────────────

    #[test]
    fn profile_activated_document_setting_active_profiles_is_loud() {
        let yaml = leaf_config::YamlLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&yaml];
        let inputs = SealInputs::new()
            .with_args(["--leaf.profiles.active=prod"])
            .with_import(ImportLocation::inline(
                "application.yaml",
                group0(),
                "---\nleaf.config.activate.on-profile: prod\nleaf.profiles.active: sneaky\n",
            ));
        let err = block_on(seal_environment_with(inputs, &loaders))
            .expect_err("a profile-activated doc setting active profiles is illegal");
        assert_eq!(err.kind, leaf_core::ErrorKind::BindError);
    }

    #[test]
    fn profile_activated_document_setting_import_is_loud() {
        let yaml = leaf_config::YamlLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&yaml];
        let inputs = SealInputs::new()
            .with_args(["--leaf.profiles.active=prod"])
            .with_import(ImportLocation::inline(
                "application.yaml",
                group0(),
                "---\nleaf.config.activate.on-profile: prod\nleaf.config.import: more.json\n",
            ));
        let err = block_on(seal_environment_with(inputs, &loaders))
            .expect_err("a profile-activated doc setting an import is illegal");
        assert_eq!(err.kind, leaf_core::ErrorKind::BindError);
    }

    // ── config-data orchestration: precedence of imported/profile docs ────────

    #[test]
    fn profile_activated_document_outranks_base_document() {
        // base sets server.port=1111 (always active); a prod-gated doc sets it to
        // 2222. With prod active, the profile-specific doc must WIN (it sits at a
        // higher rung).
        let yaml = leaf_config::YamlLoader;
        let loaders: [&dyn ConfigDataLoader; 1] = [&yaml];
        let inputs = SealInputs::new()
            .with_args(["--leaf.profiles.active=prod"])
            .with_import(ImportLocation::inline(
                "application.yaml",
                group0(),
                "server.port: 1111\n---\nleaf.config.activate.on-profile: prod\nserver.port: 2222\n",
            ));
        let sealed = block_on(seal_environment_with(inputs, &loaders)).expect("seals");
        assert_eq!(
            sealed.env.get("server.port").unwrap().raw,
            "2222",
            "the profile-activated document out-ranks the base document"
        );
    }
}
