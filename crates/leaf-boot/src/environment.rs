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
//!    (async, on the bootstrap executor here), each document folded onto the
//!    env-builder stack at its [`PrecedenceRung`].
//! 3. **profile activation** — [`leaf_core::ProfileLevers`] are harvested from the
//!    sealed-so-far stack and resolved ONCE via [`leaf_core::resolve_active`] into
//!    the canonical [`leaf_core::ActiveProfiles`], BEFORE any condition evaluates.
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
use leaf_config::{ConfigDataLocation, Contribution, ConfigDataPlan};

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

/// Drive the config-data import worklist through leaf-config's loaders, folding
/// each resulting document onto `builder` at its rung.
///
/// Async because a genuinely-remote loader's `load` is a `BoxFuture` (the cold
/// IO fence runs on the bootstrap executor); the local-file inline/snapshot path
/// resolves without touching the filesystem. Selection is by `handles` DATA,
/// never link order; a location no loader claims is a loud `ConfigDataError`.
async fn load_config_data(
    loaders: &[&dyn ConfigDataLoader],
    imports: &[ImportLocation],
    builder: &mut EnvBuilder,
) -> Result<(), ConfigDataError> {
    let mut plan = ConfigDataPlan::new();
    let mut index: u32 = 0;
    for item in imports {
        let location = ConfigDataLocation::parse(&item.raw_location);
        let loader = loaders
            .iter()
            .find(|l| l.handles(&location))
            .ok_or_else(|| ConfigDataError::no_loader(location.raw()))?;
        let cx = match &item.inline {
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
            plan.push(Contribution::new(item.rung, index, source_name, doc.props));
            index += 1;
        }
    }
    apply(plan, builder);
    Ok(())
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

    let mut builder = EnvBuilder::new();
    // Command-line is the highest operational rung (add_first = wins).
    builder.add_first(Arc::new(command_line_source(&args)));

    // ── 2. config-data load (the spring.config.import-analogue) ────────────────
    // Loaded documents fold onto the stack BELOW the command-line source (the
    // applier `add_last`s them, so the cmdline source stays first/highest).
    load_config_data(loaders, &inputs.imports, &mut builder).await?;

    // Programmatic defaults are the LOWEST operational rung (added last).
    if !inputs.programmatic.is_empty() {
        let origin = Origin::Native { crate_name: Some("leaf-boot::programmatic") };
        let prog = MapPropertySource::new(
            Arc::<str>::from(PROGRAMMATIC_SOURCE),
            inputs
                .programmatic
                .into_iter()
                .map(|(k, v)| (k, PropertyValue::with_origin(v, origin))),
        );
        builder.add_last(Arc::new(prog));
    }

    // The stack is now assembled; snapshot a provisional read view for the levers
    // + binding (both pure reads over the sealed-so-far stack).
    let provisional = builder.seal_env();

    // ── 3. profile activation (resolve_active runs ONCE, before any condition) ──
    let levers = harvest_profile_levers(&provisional);
    let profiles = resolve_active(levers, false)?;

    // ── 4. bindToApplication (leaf.main.* → frozen BootstrapSettings) ──────────
    let settings = bind_to_application(&provisional);

    // ── 5. snapshot the Env (the seal fence) ───────────────────────────────────
    // The provisional view IS the sealed env (the builder is consumed); no source
    // push is representable after this point.
    Ok(SealedEnvironment {
        env: provisional,
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
}
