//! Integration tests for the config + conditional macro surface (unit 5/6):
//! `#[derive(BindTarget)]` / `#[config_properties]` / `#[value]` / `#[converter]`
//! and `#[conditional]` / `#[profile]` / `#[auto_config]` / `#[import]`.
//!
//! Each is a real use of the thin macro on a sample type, asserting the
//! macro-emitted const artifact reached the right frozen `::leaf_core` seam — a
//! `CONFIG_METADATA` row, an `AUTO_CONFIGS` Fallback `Descriptor`, a `CONDITIONS`
//! anchor + a Runtime-tier `CondExpr`, etc. This closes the macro→leaf_core
//! boundary for the binding/conditional half of the pipeline.
//!
//! PROOF GATE (cross-crate, re-export): see `roundtrip.rs` — this crate has NO
//! `linkme` dep; the rows reach their slices through leaf-core's `pub use linkme;`
//! via `#[::leaf_core::linkme::distributed_slice(...)]` + `#[linkme(crate = ...)]`.

use leaf_macros::{
    auto_config, conditional, config_properties, converter, import, profile, value, BindTarget,
};

// ═══════════════════════ #[derive(BindTarget)] ══════════════════════════════

#[derive(Default, Debug, PartialEq, BindTarget)]
struct ServerProps {
    port: u16,
    host: String,
    max_connections: u32,
}

#[test]
fn derive_bind_target_binds_a_flat_javabean_from_the_env() {
    // The derived `BindTarget` (const NodeSchema + cursor-calling bind) binds a flat
    // struct from a property stack — the macro→leaf_core binding roundtrip.
    use leaf_core::{
        Binder, CanonicalName, ConversionService, EnvBuilder, MapPropertySource, NoopBindHandler,
        StackCps,
    };
    use std::sync::Arc;

    let mut b = EnvBuilder::new();
    b.add_last(Arc::new(MapPropertySource::from_pairs(
        "test",
        [
            ("server.port".to_string(), "8443".to_string()),
            ("server.host".to_string(), "leaf.dev".to_string()),
            ("server.max-connections".to_string(), "200".to_string()),
        ],
    )));
    let cps = StackCps::new(b.seal_env());
    let conv = ConversionService::new();
    let h = NoopBindHandler;
    let binder = Binder::new(&cps, &conv, &h);
    let prefix = CanonicalName::parse("server").unwrap();

    let bound = binder
        .bind::<ServerProps>(&prefix)
        .bound()
        .expect("the derived BindTarget binds");
    assert_eq!(
        bound,
        ServerProps {
            port: 8443,
            host: "leaf.dev".into(),
            // The snake_case field reads the kebab `max-connections` key.
            max_connections: 200,
        }
    );
}

#[test]
fn derive_bind_target_schema_is_an_object_of_the_fields() {
    // The derived const SCHEMA is an Object{JavaBean} with one Field per struct field
    // keyed on the kebab canonical names.
    let schema = <ServerProps as leaf_core::BindTarget>::SCHEMA;
    match schema {
        leaf_core::NodeSchema::Object { method, fields } => {
            assert_eq!(*method, leaf_core::BindMethod::JavaBean);
            let names: Vec<&str> = fields.iter().map(|f| f.canonical).collect();
            assert_eq!(names, vec!["port", "host", "max-connections"]);
        }
        other => panic!("expected an Object schema, got {other:?}"),
    }
}

// ═══════════════════════ #[config_properties] ═══════════════════════════════

#[derive(Default, Debug, PartialEq)]
#[config_properties(prefix = "app")]
struct AppProps {
    name: String,
    workers: u16,
}

#[test]
fn config_properties_emits_a_config_metadata_row_on_the_slice() {
    // The headline: a #[config_properties] type emits one CONFIG_METADATA row
    // carrying the prefix + the module-qualified contract id.
    let rows = leaf_core::collect_config_metadata();
    let mine = rows
        .iter()
        .find(|r| r.prefix == "app")
        .expect("the AppProps config-metadata row must reach the CONFIG_METADATA slice");
    let expected = leaf_core::ContractId::of(&format!("{}::AppProps", module_path!()));
    assert_eq!(
        mine.contract, expected,
        "the row's contract must be contract_hash(module::Ident)"
    );
}

#[test]
fn config_properties_also_derives_bind_target() {
    // The same expansion derives BindTarget, so the config bean binds from the env.
    use leaf_core::{
        Binder, CanonicalName, ConversionService, EnvBuilder, MapPropertySource, NoopBindHandler,
        StackCps,
    };
    use std::sync::Arc;

    let mut b = EnvBuilder::new();
    b.add_last(Arc::new(MapPropertySource::from_pairs(
        "test",
        [
            ("app.name".to_string(), "leaf".to_string()),
            ("app.workers".to_string(), "8".to_string()),
        ],
    )));
    let cps = StackCps::new(b.seal_env());
    let conv = ConversionService::new();
    let h = NoopBindHandler;
    let binder = Binder::new(&cps, &conv, &h);
    let prefix = CanonicalName::parse("app").unwrap();
    let bound = binder.bind::<AppProps>(&prefix).bound().expect("binds");
    assert_eq!(bound, AppProps { name: "leaf".into(), workers: 8 });
}

#[test]
fn config_properties_emits_a_documenting_config_group() {
    // The rich ConfigGroup (the `leaf metadata` rollup input) documents each bound
    // key under the public pairing const.
    let group = __LEAF_CONFIG_GROUP_AppProps;
    assert_eq!(group.prefix, "app");
    let names: Vec<&str> = group.properties.iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["name", "workers"]);
    // The property type is rendered as a string for the metadata.
    let workers = group.property("workers").expect("workers documented");
    assert_eq!(workers.ty, "u16");
}

// ═══════════════════════════════ #[value] ═══════════════════════════════════

#[value("${app.port:8080}")]
const PORT_TEMPLATE: &[leaf_core::ValueSegment];

#[test]
fn value_template_lowers_to_const_value_segments() {
    // The #[value] template lowers to the const &[ValueSegment] the placeholder
    // engine interprets; the placeholder key + default survive the split.
    assert_eq!(PORT_TEMPLATE.len(), 1);
    match &PORT_TEMPLATE[0] {
        leaf_core::ValueSegment::Placeholder { key, default } => {
            assert_eq!(key.as_ref(), "app.port");
            assert_eq!(default.as_deref(), Some("8080"));
        }
        other => panic!("expected a placeholder segment, got {other:?}"),
    }
}

// ═══════════════════════════ #[converter] ═══════════════════════════════════

#[allow(dead_code)]
#[converter]
struct PortConverter;

impl leaf_core::Converter for PortConverter {
    fn convert(
        &self,
        _v: &leaf_core::ConfigValue<'_>,
        _cx: &leaf_core::ConvertCtx,
    ) -> Result<Box<dyn std::any::Any + Send>, leaf_core::LeafError> {
        Ok(Box::new(0u16))
    }
    fn target(&self) -> std::any::TypeId {
        std::any::TypeId::of::<u16>()
    }
}

#[test]
fn converter_reaches_the_catalogs_slice() {
    // The #[converter] macro emits one CatalogRow anti-DCE anchor keyed on the
    // converter's module-qualified contract id.
    let rows = leaf_core::collect_slice(&leaf_core::CATALOGS);
    let expected = leaf_core::ContractId::of(&format!("{}::PortConverter", module_path!()));
    assert!(
        rows.iter().any(|r| r.contract == expected),
        "the PortConverter catalog row must reach the CATALOGS slice"
    );
}

// ═══════════════════════════ #[conditional] ═════════════════════════════════

#[allow(dead_code)]
#[conditional(on_property("feature.enabled", having_value = "true"))]
struct GatedComponent;

#[test]
fn conditional_lowers_to_a_runtime_tier_condexpr_leaf() {
    // The headline: #[conditional(on_property(...))] lowers to ONE const CondExpr
    // (a public guard-pairing const) whose tier is the Runtime floor.
    let guard: leaf_core::CondExpr = __leaf_guard_GatedComponent;
    // A property leaf is the mandatory Runtime tier (decided against the sealed Env).
    assert_eq!(guard.tier(), leaf_core::EarliestTier::Runtime);
    // The OnProperty leaf id agrees with the runtime catalog's id minting.
    let on_property_id =
        leaf_core::ConditionId(leaf_core::contract_hash("leaf::condition::OnProperty") as u32);
    match guard {
        leaf_core::CondExpr::Leaf(id, _attrs) => assert_eq!(id, on_property_id),
        other => panic!("expected a Leaf, got {other:?}"),
    }
}

#[test]
fn conditional_emits_a_conditions_anti_dce_anchor() {
    // One ConditionRow anti-DCE anchor per referenced kind reaches the CONDITIONS
    // slice (so a dropped condition family is loud, never a silent pass-all).
    let rows = leaf_core::collect_slice(&leaf_core::CONDITIONS);
    let on_property = leaf_core::ContractId::of("leaf::condition::OnProperty");
    assert!(
        rows.iter().any(|r| r.contract == on_property),
        "the OnProperty condition anchor must reach the CONDITIONS slice"
    );
}

// ═══════════════════════════════ #[profile] ═════════════════════════════════

#[allow(dead_code)]
#[profile("prod & eu")]
struct ProdOnlyComponent;

#[test]
fn profile_lowers_to_a_single_on_profile_cond_leaf() {
    // Profiles are a PRESET: the whole `prod & eu` expression rides ONE ON_PROFILE
    // CondExpr leaf (the same guard machinery as #[conditional]).
    let guard: leaf_core::CondExpr = __leaf_guard_ProdOnlyComponent;
    match guard {
        leaf_core::CondExpr::Leaf(id, attrs) => {
            assert_eq!(id, leaf_core::ON_PROFILE, "a profile rides the ON_PROFILE leaf");
            // The rendered expression rides a `profiles` attr the runtime re-parses.
            let has_profiles = attrs.iter().any(|a| a.key() == "profiles");
            assert!(has_profiles, "the profile expression rides a `profiles` attr");
        }
        other => panic!("expected an ON_PROFILE leaf, got {other:?}"),
    }
}

// ═══════════════════════════ #[auto_config] ═════════════════════════════════

#[auto_config]
struct RedisAutoConfig;
impl RedisAutoConfig {
    fn new() -> Self {
        RedisAutoConfig
    }
}

fn auto_config_named(name: &str) -> Option<leaf_core::Descriptor> {
    leaf_core::AUTO_CONFIGS
        .iter()
        .find(|d| d.declared_name == Some(name))
        .copied()
}

#[test]
fn auto_config_emits_an_auto_configs_fallback_row() {
    // The headline: #[auto_config] emits the SAME Descriptor shape into the SEPARATE
    // AUTO_CONFIGS slice at CandidateRole::FALLBACK (a user bean transparently wins).
    let d = auto_config_named("redisAutoConfig")
        .expect("the auto-config must reach the AUTO_CONFIGS slice");
    assert_eq!(
        d.meta.candidate_role,
        leaf_core::auto_config_role(),
        "an auto-config registers at CandidateRole::FALLBACK"
    );
    // It must NOT also be a COMPONENTS row (the AutoConfigurationExcludeFilter boundary).
    assert!(
        !leaf_core::COMPONENTS
            .iter()
            .any(|c| c.declared_name == Some("redisAutoConfig")),
        "an auto-config must not be a COMPONENTS row"
    );
}

// A gated auto-config: #[auto_config] stacked with #[conditional].
#[auto_config]
#[conditional(on_property("redis.enabled", having_value = "true"))]
struct GatedAutoConfig;
impl GatedAutoConfig {
    fn new() -> Self {
        GatedAutoConfig
    }
}

#[test]
fn a_gated_auto_config_emits_both_the_fallback_row_and_the_guard() {
    // Stacking #[conditional] on an #[auto_config] emits BOTH the AUTO_CONFIGS
    // Fallback row AND the guard (its CONDITIONS anchor + the pairing const).
    let d = auto_config_named("gatedAutoConfig").expect("reaches AUTO_CONFIGS");
    assert_eq!(d.meta.candidate_role, leaf_core::auto_config_role());
    // The guard pairing const is materialized at the Runtime tier.
    let guard: leaf_core::CondExpr = __leaf_guard_GatedAutoConfig;
    assert_eq!(guard.tier(), leaf_core::EarliestTier::Runtime);
}

// ═══════════════════════════════ #[import] ══════════════════════════════════

#[allow(dead_code)]
#[import(RedisAutoConfig, GatedAutoConfig)]
struct MyApplication;

#[test]
fn import_emits_an_import_edge_referencing_the_importees() {
    // The #[import] edge carries the importer's contract as `from` and the importees'
    // contracts in `to[]` (the composition currency the assembly pass reads).
    let edge: leaf_core::ImportEdge = __LEAF_IMPORT_MyApplication;
    let from = leaf_core::ContractId::of(&format!("{}::MyApplication", module_path!()));
    assert_eq!(edge.from, from);
    assert_eq!(edge.to.len(), 2, "two importees ride the edge");
    assert!(edge.to.contains(&leaf_core::ContractId::of("RedisAutoConfig")));
}
