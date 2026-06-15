//! The [`ConfigDataLoader`] SPI + the concrete format loaders (JSON/YAML/
//! config-tree/env), realizing environment-config `config-data` + `extra-9`.
//!
//! A loader claims a [`ConfigDataLocation`] via the `handles` DATA predicate
//! (never link order), then `load`s it (on the bootstrap executor) into a list
//! of [`LoadedDocument`]s. Per the ADR-07 dyn-seam standard the async method
//! returns a [`BoxFuture`]; the built-in local-file loaders are also exposed via
//! a synchronous `load_sync` fast path (the local/classpath case never needs to
//! `.await`), so the planner can fold them without a runtime.
//!
//! The format loaders normalize their native parse tree into the ONE
//! [`crate::flatten::Node`] shape and run the ONE [`crate::flatten::flatten`]
//! pass, so JSON and YAML share identical segment/null-as-absent semantics.

use leaf_core::{BoxFuture, Origin, PropertyValue};

use crate::error::{ConfigDataError, ConfigDataLocation, LocationScheme};
use crate::flatten::{flatten, Node};

/// One loaded config document: its flattened properties.
///
/// `activate_on`/`imports` from the design sketch are deferred to leaf-boot's
/// `seal_environment` orchestration (the `CondExpr` document-activation filter
/// and `spring.config.import` traversal run THERE over the frozen profiles);
/// this crate's loaders produce the keyed property payload they gate.
#[derive(Clone, PartialEq, Debug)]
pub struct LoadedDocument {
    /// The flattened, stringly-typed properties (canonical key → value).
    pub props: Vec<(String, PropertyValue)>,
}

impl LoadedDocument {
    /// A document with the given flattened properties.
    #[must_use]
    pub fn new(props: Vec<(String, PropertyValue)>) -> Self {
        LoadedDocument { props }
    }
}

/// The per-load context handed to a [`ConfigDataLoader`].
///
/// Currently carries only the raw text for in-memory loads (so loaders are
/// exhaustively unit-testable without touching the filesystem — the injected
/// `ConfigFs` the design names is leaf-boot's). A loader that needs real IO
/// reads `loc.path()` directly in `load`.
#[derive(Default)]
pub struct LoadCtx<'a> {
    /// In-memory document text (test/inline path); `None` means read from disk.
    pub inline: Option<&'a str>,
}

impl<'a> LoadCtx<'a> {
    /// An empty context (real-IO path).
    #[must_use]
    pub fn new() -> Self {
        LoadCtx { inline: None }
    }

    /// A context supplying inline document text (no filesystem access).
    #[must_use]
    pub fn inline(text: &'a str) -> Self {
        LoadCtx { inline: Some(text) }
    }
}

/// THE origin-agnostic config-data loader SPI (environment-config `config-data`).
///
/// Selection is by the `handles` DATA predicate; `load` is async (the ADR-07
/// dyn-seam standard) for genuinely-remote sources, but the built-in local
/// loaders also implement [`SyncConfigDataLoader`] so the planner can fold them
/// synchronously.
pub trait ConfigDataLoader: Send + Sync {
    /// Whether this loader claims `loc` (by DATA — scheme/extension).
    fn handles(&self, loc: &ConfigDataLocation) -> bool;

    /// Load `loc` into a list of documents.
    ///
    /// # Errors
    /// A [`ConfigDataError`] on a malformed document or an IO failure.
    fn load<'a>(
        &'a self,
        loc: &'a ConfigDataLocation,
        cx: &'a LoadCtx<'a>,
    ) -> BoxFuture<'a, Result<Vec<LoadedDocument>, ConfigDataError>>;
}

/// The synchronous local-file facet (environment-config `extra-10` sync entry).
///
/// The built-in JSON/YAML/config-tree/env loaders implement this so the
/// plan/apply engine can fold local sources without a runtime. A genuinely
/// remote loader implements only the async [`ConfigDataLoader`].
pub trait SyncConfigDataLoader: ConfigDataLoader {
    /// Load `loc` synchronously (the local/classpath fast path).
    ///
    /// # Errors
    /// A [`ConfigDataError`] on a malformed document or an IO failure.
    fn load_sync(
        &self,
        loc: &ConfigDataLocation,
        cx: &LoadCtx<'_>,
    ) -> Result<Vec<LoadedDocument>, ConfigDataError>;
}

/// Read the document text: prefer the inline ctx, else read the file from disk.
fn read_text(loc: &ConfigDataLocation, cx: &LoadCtx<'_>) -> Result<Option<String>, ConfigDataError> {
    if let Some(text) = cx.inline {
        return Ok(Some(text.to_string()));
    }
    match std::fs::read_to_string(loc.path()) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if loc.is_optional() {
                Ok(None)
            } else {
                Err(ConfigDataError::missing(loc.raw()))
            }
        }
        Err(e) => Err(ConfigDataError::new(
            crate::error::ConfigDataErrorKind::Io,
            loc.raw(),
            e.to_string(),
        )),
    }
}

// ───────────────────────────── JSON loader ──────────────────────────────────

/// The JSON format loader (serde_json) + the SAJ flatten contract (`extra-9`).
///
/// Parses a JSON object/array and runs the ONE flatten pass. Doubles as the
/// `JsonFlattenLoader` the SAJ blob path uses (flatten-to-owned semantics are
/// identical — the difference is only the precedence rung the planner tags).
pub struct JsonLoader;

impl JsonLoader {
    fn parse(text: &str, loc: &ConfigDataLocation) -> Result<Vec<LoadedDocument>, ConfigDataError> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| ConfigDataError::malformed(loc.raw(), e.to_string()))?;
        let node = json_to_node(&value);
        // The loader's coarse origin is a native source tag (the fine file:line
        // OriginId the design names needs the interned OriginStore — deferred to
        // the leaf-boot seal_environment pass; we stamp the always-available
        // coarse carrier here so provenance is never blank).
        let props = flatten(&node, Origin::Native { crate_name: Some("leaf-config::json") });
        Ok(vec![LoadedDocument::new(props)])
    }
}

fn json_to_node(v: &serde_json::Value) -> Node {
    match v {
        serde_json::Value::Null => Node::Null,
        serde_json::Value::Bool(b) => Node::Scalar(b.to_string()),
        serde_json::Value::Number(n) => Node::Scalar(n.to_string()),
        serde_json::Value::String(s) => Node::Scalar(s.clone()),
        serde_json::Value::Array(items) => Node::Seq(items.iter().map(json_to_node).collect()),
        serde_json::Value::Object(map) => {
            Node::Map(map.iter().map(|(k, v)| (k.clone(), json_to_node(v))).collect())
        }
    }
}

impl ConfigDataLoader for JsonLoader {
    fn handles(&self, loc: &ConfigDataLocation) -> bool {
        matches!(loc.scheme(), LocationScheme::File)
            && loc.extension().as_deref() == Some("json")
    }

    fn load<'a>(
        &'a self,
        loc: &'a ConfigDataLocation,
        cx: &'a LoadCtx<'a>,
    ) -> BoxFuture<'a, Result<Vec<LoadedDocument>, ConfigDataError>> {
        Box::pin(async move { self.load_sync(loc, cx) })
    }
}

impl SyncConfigDataLoader for JsonLoader {
    fn load_sync(
        &self,
        loc: &ConfigDataLocation,
        cx: &LoadCtx<'_>,
    ) -> Result<Vec<LoadedDocument>, ConfigDataError> {
        match read_text(loc, cx)? {
            Some(text) => JsonLoader::parse(&text, loc),
            None => Ok(vec![]),
        }
    }
}

// ───────────────────────────── YAML loader ──────────────────────────────────

/// The YAML format loader (yaml-rust2, the maintained YAML 1.2 parser).
///
/// A multi-document YAML stream (`---` separated) yields one [`LoadedDocument`]
/// per `---` document — the natural carrier for `spring.config.activate`-style
/// multi-doc files (the activation FILTER itself is leaf-boot's).
pub struct YamlLoader;

impl YamlLoader {
    fn parse(text: &str, loc: &ConfigDataLocation) -> Result<Vec<LoadedDocument>, ConfigDataError> {
        let docs = yaml_rust2::YamlLoader::load_from_str(text)
            .map_err(|e| ConfigDataError::malformed(loc.raw(), e.to_string()))?;
        let origin = Origin::Native {
            crate_name: Some("leaf-config::yaml"),
        };
        let mut out = Vec::with_capacity(docs.len());
        for doc in &docs {
            let node = yaml_to_node(doc);
            let props = flatten(&node, origin);
            // Skip a wholly-empty document (e.g. a trailing `---`).
            if !props.is_empty() {
                out.push(LoadedDocument::new(props));
            }
        }
        Ok(out)
    }
}

fn yaml_to_node(y: &yaml_rust2::Yaml) -> Node {
    use yaml_rust2::Yaml;
    match y {
        Yaml::Null | Yaml::BadValue => Node::Null,
        Yaml::Boolean(b) => Node::Scalar(b.to_string()),
        Yaml::Integer(i) => Node::Scalar(i.to_string()),
        Yaml::Real(s) => Node::Scalar(s.clone()),
        Yaml::String(s) => Node::Scalar(s.clone()),
        Yaml::Array(items) => Node::Seq(items.iter().map(yaml_to_node).collect()),
        Yaml::Hash(map) => Node::Map(
            map.iter()
                .map(|(k, v)| (yaml_key_to_string(k), yaml_to_node(v)))
                .collect(),
        ),
        // An alias is a forward-ref we do not resolve (yaml-rust2 resolves
        // anchors at parse for the common case); treat as absent.
        Yaml::Alias(_) => Node::Null,
    }
}

/// Render a YAML mapping KEY to its string form (keys are stringly-typed in the
/// config stack; a non-string key is rendered via its scalar form).
fn yaml_key_to_string(y: &yaml_rust2::Yaml) -> String {
    use yaml_rust2::Yaml;
    match y {
        Yaml::String(s) => s.clone(),
        Yaml::Boolean(b) => b.to_string(),
        Yaml::Integer(i) => i.to_string(),
        Yaml::Real(s) => s.clone(),
        Yaml::Null => "null".to_string(),
        _ => String::new(),
    }
}

impl ConfigDataLoader for YamlLoader {
    fn handles(&self, loc: &ConfigDataLocation) -> bool {
        matches!(loc.scheme(), LocationScheme::File)
            && matches!(loc.extension().as_deref(), Some("yaml" | "yml"))
    }

    fn load<'a>(
        &'a self,
        loc: &'a ConfigDataLocation,
        cx: &'a LoadCtx<'a>,
    ) -> BoxFuture<'a, Result<Vec<LoadedDocument>, ConfigDataError>> {
        Box::pin(async move { self.load_sync(loc, cx) })
    }
}

impl SyncConfigDataLoader for YamlLoader {
    fn load_sync(
        &self,
        loc: &ConfigDataLocation,
        cx: &LoadCtx<'_>,
    ) -> Result<Vec<LoadedDocument>, ConfigDataError> {
        match read_text(loc, cx)? {
            Some(text) => YamlLoader::parse(&text, loc),
            None => Ok(vec![]),
        }
    }
}

// ──────────────────────────── config-tree loader ────────────────────────────

/// The config-tree loader (`configtree:<dir>`) — Kubernetes-style mounted
/// secrets/configmaps where each FILE is one property and its contents are the
/// value (environment-config `config-data`).
///
/// A directory `db/` containing files `username` and `password` yields the keys
/// `db.username` / `db.password` (a `/` path separator becomes a `.` segment).
/// Trailing whitespace on a value is trimmed (matching Spring's config-tree).
pub struct ConfigTreeLoader;

impl ConfigTreeLoader {
    fn load_dir(loc: &ConfigDataLocation) -> Result<Vec<LoadedDocument>, ConfigDataError> {
        let root = std::path::Path::new(loc.path());
        if !root.exists() {
            return if loc.is_optional() {
                Ok(vec![])
            } else {
                Err(ConfigDataError::missing(loc.raw()))
            };
        }
        let mut props = Vec::new();
        Self::walk(root, root, &mut props)?;
        // Deterministic key order.
        props.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(vec![LoadedDocument::new(props)])
    }

    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut Vec<(String, PropertyValue)>,
    ) -> Result<(), ConfigDataError> {
        let read = std::fs::read_dir(dir).map_err(|e| {
            ConfigDataError::new(crate::error::ConfigDataErrorKind::Io, dir.display().to_string(), e.to_string())
        })?;
        for entry in read {
            let entry = entry.map_err(|e| {
                ConfigDataError::new(crate::error::ConfigDataErrorKind::Io, dir.display().to_string(), e.to_string())
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|e| {
                ConfigDataError::new(crate::error::ConfigDataErrorKind::Io, path.display().to_string(), e.to_string())
            })?;
            // Skip hidden files (Kubernetes `..data` symlink dirs etc.).
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if file_type.is_dir() {
                Self::walk(root, &path, out)?;
            } else if file_type.is_file() {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let key = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(".");
                let value = std::fs::read_to_string(&path).map_err(|e| {
                    ConfigDataError::new(crate::error::ConfigDataErrorKind::Io, path.display().to_string(), e.to_string())
                })?;
                out.push((
                    key,
                    PropertyValue::with_origin(
                        value.trim_end().to_string(),
                        Origin::Native { crate_name: Some("leaf-config::configtree") },
                    ),
                ));
            }
        }
        Ok(())
    }
}

impl ConfigDataLoader for ConfigTreeLoader {
    fn handles(&self, loc: &ConfigDataLocation) -> bool {
        matches!(loc.scheme(), LocationScheme::ConfigTree)
    }

    fn load<'a>(
        &'a self,
        loc: &'a ConfigDataLocation,
        cx: &'a LoadCtx<'a>,
    ) -> BoxFuture<'a, Result<Vec<LoadedDocument>, ConfigDataError>> {
        Box::pin(async move { self.load_sync(loc, cx) })
    }
}

impl SyncConfigDataLoader for ConfigTreeLoader {
    fn load_sync(
        &self,
        loc: &ConfigDataLocation,
        _cx: &LoadCtx<'_>,
    ) -> Result<Vec<LoadedDocument>, ConfigDataError> {
        ConfigTreeLoader::load_dir(loc)
    }
}

// ───────────────────────────── env-var loader ───────────────────────────────

/// The OS-environment loader (`env:` / `env:<prefix>`) — reads a SNAPSHOT of the
/// process environment into an enumerable document (environment-config
/// `config-data`).
///
/// The env var names are mapped to their canonical kebab-dotted form via
/// leaf-core's [`leaf_core::env_var_to_canonical`] (`DB_POOL_SIZE` →
/// `db.pool.size`) so a relaxed lookup resolves them; the original var name is
/// kept too so an exact lookup still hits. An `env:<prefix>` location filters to
/// vars starting with `<prefix>` (the prefix is stripped from the key).
///
/// The snapshot is supplied EXPLICITLY (never read from `std::env` here — that is
/// unsound concurrent with `setenv`; leaf-boot snapshots the env once inside
/// `seal_environment` and hands it in). The inline `LoadCtx` text (newline
/// `KEY=VALUE` pairs) is the test/snapshot carrier.
pub struct EnvVarLoader {
    snapshot: Vec<(String, String)>,
}

impl EnvVarLoader {
    /// Build from an explicit `(name, value)` snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: impl IntoIterator<Item = (String, String)>) -> Self {
        EnvVarLoader {
            snapshot: snapshot.into_iter().collect(),
        }
    }

    /// Parse a newline-delimited `KEY=VALUE` blob into a snapshot (test helper /
    /// the inline `LoadCtx` carrier).
    #[must_use]
    pub fn parse_blob(blob: &str) -> Vec<(String, String)> {
        blob.lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                line.split_once('=')
                    .map(|(k, v)| (k.trim().to_string(), v.to_string()))
            })
            .collect()
    }

    fn build_docs(&self, loc: &ConfigDataLocation) -> Vec<LoadedDocument> {
        let prefix = loc.path();
        let origin = Origin::Native {
            crate_name: Some("leaf-config::env"),
        };
        let mut props = Vec::new();
        for (name, value) in &self.snapshot {
            let effective = if prefix.is_empty() {
                name.as_str()
            } else if let Some(stripped) = name.strip_prefix(prefix) {
                stripped
            } else {
                continue;
            };
            if effective.is_empty() {
                continue;
            }
            let canonical = leaf_core::env_var_to_canonical(effective).into_owned();
            props.push((
                canonical,
                PropertyValue::with_origin(value.clone(), origin),
            ));
        }
        if props.is_empty() {
            vec![]
        } else {
            vec![LoadedDocument::new(props)]
        }
    }
}

impl ConfigDataLoader for EnvVarLoader {
    fn handles(&self, loc: &ConfigDataLocation) -> bool {
        matches!(loc.scheme(), LocationScheme::Env)
    }

    fn load<'a>(
        &'a self,
        loc: &'a ConfigDataLocation,
        cx: &'a LoadCtx<'a>,
    ) -> BoxFuture<'a, Result<Vec<LoadedDocument>, ConfigDataError>> {
        Box::pin(async move { self.load_sync(loc, cx) })
    }
}

impl SyncConfigDataLoader for EnvVarLoader {
    fn load_sync(
        &self,
        loc: &ConfigDataLocation,
        cx: &LoadCtx<'_>,
    ) -> Result<Vec<LoadedDocument>, ConfigDataError> {
        // An inline blob overrides the stored snapshot (test/snapshot path).
        if let Some(text) = cx.inline {
            let loader = EnvVarLoader::from_snapshot(EnvVarLoader::parse_blob(text));
            Ok(loader.build_docs(loc))
        } else {
            Ok(self.build_docs(loc))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_inline(
        loader: &dyn SyncConfigDataLoader,
        raw_loc: &str,
        text: &str,
    ) -> Vec<(String, String)> {
        let loc = ConfigDataLocation::parse(raw_loc);
        let cx = LoadCtx::inline(text);
        loader
            .load_sync(&loc, &cx)
            .unwrap()
            .into_iter()
            .flat_map(|d| d.props)
            .map(|(k, v)| (k, v.raw.into_owned()))
            .collect()
    }

    // ── JSON loader ─────────────────────────────────────────────────────────

    #[test]
    fn json_loader_handles_and_parses_nested() {
        let loader = JsonLoader;
        assert!(loader.handles(&ConfigDataLocation::parse("application.json")));
        assert!(!loader.handles(&ConfigDataLocation::parse("application.yaml")));
        let out = load_inline(
            &loader,
            "application.json",
            r#"{"server":{"port":8080},"hosts":["a","b"],"nothing":null}"#,
        );
        assert!(out.contains(&("server.port".to_string(), "8080".to_string())));
        assert!(out.contains(&("hosts[0]".to_string(), "a".to_string())));
        assert!(out.contains(&("hosts[1]".to_string(), "b".to_string())));
        // null-as-absent.
        assert!(!out.iter().any(|(k, _)| k == "nothing"));
    }

    #[test]
    fn json_loader_malformed_is_a_loud_error() {
        let loader = JsonLoader;
        let loc = ConfigDataLocation::parse("bad.json");
        let cx = LoadCtx::inline("{not json");
        let err = loader.load_sync(&loc, &cx).unwrap_err();
        assert_eq!(err.kind, crate::error::ConfigDataErrorKind::Malformed);
    }

    // ── YAML loader ─────────────────────────────────────────────────────────

    #[test]
    fn yaml_loader_handles_and_parses_nested() {
        let loader = YamlLoader;
        assert!(loader.handles(&ConfigDataLocation::parse("application.yaml")));
        assert!(loader.handles(&ConfigDataLocation::parse("application.yml")));
        assert!(!loader.handles(&ConfigDataLocation::parse("application.json")));
        let out = load_inline(
            &loader,
            "application.yaml",
            "server:\n  port: 8080\nhosts:\n  - a\n  - b\nnothing: ~\n",
        );
        assert!(out.contains(&("server.port".to_string(), "8080".to_string())));
        assert!(out.contains(&("hosts[0]".to_string(), "a".to_string())));
        assert!(out.contains(&("hosts[1]".to_string(), "b".to_string())));
        assert!(!out.iter().any(|(k, _)| k == "nothing"));
    }

    #[test]
    fn yaml_multi_document_yields_one_doc_each() {
        let loader = YamlLoader;
        let loc = ConfigDataLocation::parse("application.yaml");
        let cx = LoadCtx::inline("a: 1\n---\nb: 2\n");
        let docs = loader.load_sync(&loc, &cx).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].props[0].0, "a");
        assert_eq!(docs[1].props[0].0, "b");
    }

    #[test]
    fn yaml_malformed_is_a_loud_error() {
        let loader = YamlLoader;
        let loc = ConfigDataLocation::parse("bad.yaml");
        let cx = LoadCtx::inline("a:\n  - b\n - c\n");
        let err = loader.load_sync(&loc, &cx).unwrap_err();
        assert_eq!(err.kind, crate::error::ConfigDataErrorKind::Malformed);
    }

    // ── env-var loader ──────────────────────────────────────────────────────

    #[test]
    fn env_loader_maps_to_canonical_keys() {
        let loader = EnvVarLoader::from_snapshot([]);
        let out = load_inline(&loader, "env:", "DB_POOL_SIZE=10\nSERVER_PORT=9090\n");
        // env_var_to_canonical maps DB_POOL_SIZE -> db.pool.size
        assert!(out.contains(&("db.pool.size".to_string(), "10".to_string())));
        assert!(out.contains(&("server.port".to_string(), "9090".to_string())));
    }

    #[test]
    fn env_loader_prefix_filters_and_strips() {
        let loader = EnvVarLoader::from_snapshot([]);
        let out = load_inline(
            &loader,
            "env:APP_",
            "APP_NAME=leaf\nOTHER_VAR=ignored\n",
        );
        // APP_ prefix stripped -> NAME -> name; OTHER_VAR filtered out.
        assert_eq!(out, vec![("name".to_string(), "leaf".to_string())]);
    }

    #[test]
    fn env_loader_from_stored_snapshot() {
        let loader = EnvVarLoader::from_snapshot([("FOO_BAR".to_string(), "baz".to_string())]);
        let loc = ConfigDataLocation::parse("env:");
        let cx = LoadCtx::new();
        let out: Vec<_> = loader
            .load_sync(&loc, &cx)
            .unwrap()
            .into_iter()
            .flat_map(|d| d.props)
            .map(|(k, v)| (k, v.raw.into_owned()))
            .collect();
        assert_eq!(out, vec![("foo.bar".to_string(), "baz".to_string())]);
    }

    // ── the async BoxFuture `load` path ─────────────────────────────────────

    #[test]
    fn async_load_path_matches_sync() {
        let loader = JsonLoader;
        let loc = ConfigDataLocation::parse("application.json");
        let cx = LoadCtx::inline(r#"{"k":"v"}"#);
        let docs = futures::executor::block_on(loader.load(&loc, &cx)).unwrap();
        assert_eq!(docs[0].props[0].0, "k");
        assert_eq!(docs[0].props[0].1.raw, "v");
    }

    // ── config-tree loader (real filesystem) ────────────────────────────────

    #[test]
    fn configtree_loader_reads_files_as_properties() {
        let dir = std::env::temp_dir().join(format!("leaf-cfgtree-{}", std::process::id()));
        let db = dir.join("db");
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(db.join("username"), "admin\n").unwrap();
        std::fs::write(db.join("password"), "s3cr3t").unwrap();

        let loader = ConfigTreeLoader;
        let loc = ConfigDataLocation::parse(&format!("configtree:{}", dir.display()));
        let cx = LoadCtx::new();
        let out: Vec<_> = loader
            .load_sync(&loc, &cx)
            .unwrap()
            .into_iter()
            .flat_map(|d| d.props)
            .map(|(k, v)| (k, v.raw.into_owned()))
            .collect();
        // Nested file path -> dotted key; trailing whitespace trimmed.
        assert!(out.contains(&("db.username".to_string(), "admin".to_string())));
        assert!(out.contains(&("db.password".to_string(), "s3cr3t".to_string())));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn configtree_missing_required_is_loud_optional_is_empty() {
        let loader = ConfigTreeLoader;
        let missing = ConfigDataLocation::parse("configtree:/nonexistent/leaf/path/xyz");
        let cx = LoadCtx::new();
        let err = loader.load_sync(&missing, &cx).unwrap_err();
        assert_eq!(err.kind, crate::error::ConfigDataErrorKind::MissingLocation);

        let opt = ConfigDataLocation::parse("optional:configtree:/nonexistent/leaf/path/xyz");
        assert!(loader.load_sync(&opt, &cx).unwrap().is_empty());
    }
}
