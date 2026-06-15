//! Acceptance test (binding-conversion phase3/07 serde-bridge): a
//! `#[derive(serde::Deserialize)]` struct binds from a `PropertySource` via the
//! `leaf-serde` bridge, AND the bridge coexists with the canonical
//! `FromConfigValue` path on the SAME values.
//!
//! This is the headline contract: serde is an OPT-IN alternate, never a
//! replacement of leaf-core's canonical coercion surface.

#![cfg(feature = "serde-bridge")]

use std::sync::Arc;

use leaf_core::bind::{Binder, ConversionService, Converter, NoopBindHandler, StackCps};
use leaf_core::convert::{ConfigValue, ConvertCtx, FromConfigValue};
use leaf_core::env::{Env, EnvBuilder, MapPropertySource};
use leaf_core::relaxed::CanonicalName;

use leaf_serde::{from_env, register_serde_converter, SerdeConverter};

/// A serde-deserializable config type (the third-party / migrant shape that
/// motivates the bridge — no `#[derive(BindTarget)]`).
#[derive(serde::Deserialize, Debug, PartialEq)]
struct DbConfig {
    url: String,
    pool_size: u32,
    #[serde(default)]
    read_only: bool,
}

fn env() -> Env {
    let mut b = EnvBuilder::new();
    b.add_last(Arc::new(MapPropertySource::from_pairs(
        "application",
        [
            ("db.url", "postgres://localhost/leaf"),
            ("db.pool-size", "16"),
            ("db.read-only", "true"),
        ],
    )));
    b.seal_env()
}

#[test]
fn derive_deserialize_binds_from_a_property_source_via_the_bridge() {
    let env = env();
    let got: DbConfig = from_env(&env, "db").expect("serde-bridge binds the subtree");
    assert_eq!(
        got,
        DbConfig {
            url: "postgres://localhost/leaf".into(),
            pool_size: 16,
            read_only: true,
        }
    );
}

#[test]
fn the_bridge_coexists_with_the_canonical_fromconfigvalue_path() {
    let env = env();

    // (1) the serde-bridge ConfigDeserializer path
    let bridged: DbConfig = from_env(&env, "db").unwrap();

    // (2) the canonical leaf-native FromConfigValue path over the SAME Env. The
    // native Binder is constructed over the same sealed Env to prove the two
    // engines coexist without interfering.
    let cps = StackCps::new(env.clone());
    let conv = ConversionService::new();
    let handler = NoopBindHandler;
    let _binder = Binder::new(&cps, &conv, &handler);

    let url: String = String::from_config_value(
        &ConfigValue::scalar(cps_get(&cps, "db.url")),
        &ConvertCtx::strict(),
    )
    .unwrap();
    let pool: u32 = u32::from_config_value(
        &ConfigValue::scalar(cps_get(&cps, "db.pool-size")),
        &ConvertCtx::strict(),
    )
    .unwrap();

    assert_eq!(bridged.url, url);
    assert_eq!(bridged.pool_size, pool);
    assert_eq!(url, "postgres://localhost/leaf");
    assert_eq!(pool, 16);
}

#[test]
fn the_serde_converter_registers_into_the_optin_conversion_service() {
    // The erased serde Converter rides the SAME opt-in ConversionService the
    // binder consults for type-erased targets — coexisting with FromConfigValue.
    let mut svc = ConversionService::new();
    register_serde_converter::<u32>(&mut svc);
    assert!(svc.has(std::any::TypeId::of::<u32>()));

    let cv = ConfigValue::scalar("16");
    let boxed = svc
        .convert(std::any::TypeId::of::<u32>(), &cv, &ConvertCtx::strict())
        .unwrap()
        .unwrap();
    assert_eq!(*boxed.downcast::<u32>().unwrap(), 16);

    // Constructing the converter directly works too (the manual-registration path).
    let conv = SerdeConverter::<u32>::new();
    let n = *conv
        .convert(&cv, &ConvertCtx::strict())
        .unwrap()
        .downcast::<u32>()
        .unwrap();
    assert_eq!(n, 16);
}

fn cps_get(cps: &StackCps, key: &str) -> String {
    use leaf_core::bind::ConfigurationPropertySource;
    let name = CanonicalName::parse(key).unwrap();
    cps.get(&name).unwrap().raw.into_owned()
}
