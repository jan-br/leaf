//! [`RedisProperties`] — the relaxed-bound connection config for the Redis
//! integration (`leaf.redis.*`).
//!
//! These are the knobs the auto-config reads from the sealed [`Env`] to build the
//! [`RedisClient`](crate::RedisClient) / [`RedisCacheManager`](crate::RedisCacheManager).
//! They are PLAIN data (no codegen) so the contributing crate stays a normal lib;
//! `from_env` is the relaxed-binding projection the auto-config seed runs.

use leaf_core::{Env, PropertyResolver};

/// The canonical property prefix for every Redis knob.
pub const PREFIX: &str = "leaf.redis";

/// The opt-in enablement key (`leaf.redis.enabled`) the auto-config's
/// `OnProperty` guard reads. Spring-flavour: present-and-not-`false` enables.
pub const ENABLED_PROPERTY: &str = "leaf.redis.enabled";

/// The connection-URL key (`leaf.redis.url`).
pub const URL_PROPERTY: &str = "leaf.redis.url";

/// The key-prefix knob (`leaf.redis.key-prefix`) namespacing cache entries.
pub const KEY_PREFIX_PROPERTY: &str = "leaf.redis.key-prefix";

/// The default connection URL when none is configured (the conventional local
/// dev server).
pub const DEFAULT_URL: &str = "redis://127.0.0.1:6379/";

/// The relaxed-bound Redis connection config (`leaf.redis.*`).
///
/// A plain projection over the sealed [`Env`]; the auto-config builds it once in
/// its seed and threads it into the [`RedisClient`](crate::RedisClient).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedisProperties {
    /// The connection URL (`leaf.redis.url`), defaulting to [`DEFAULT_URL`].
    pub url: String,
    /// The optional key prefix (`leaf.redis.key-prefix`) namespacing every cache
    /// key written to the backend (empty = no prefix).
    pub key_prefix: String,
}

impl Default for RedisProperties {
    fn default() -> Self {
        RedisProperties { url: DEFAULT_URL.to_string(), key_prefix: String::new() }
    }
}

impl RedisProperties {
    /// Project the `leaf.redis.*` knobs out of the sealed [`Env`] (relaxed
    /// binding), falling back to [`DEFAULT_URL`] / no prefix when unset.
    #[must_use]
    pub fn from_env(env: &Env) -> Self {
        let url = PropertyResolver::get(env, URL_PROPERTY)
            .map(|rv| rv.raw)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_URL.to_string());
        let key_prefix = PropertyResolver::get(env, KEY_PREFIX_PROPERTY)
            .map(|rv| rv.raw)
            .unwrap_or_default();
        RedisProperties { url, key_prefix }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::{EnvBuilder, MapPropertySource};
    use std::sync::Arc;

    fn env_with(pairs: &[(&str, &str)]) -> Env {
        let src = MapPropertySource::from_pairs(
            "test",
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(src));
        b.seal_env()
    }

    #[test]
    fn defaults_to_local_url_with_no_prefix() {
        let p = RedisProperties::from_env(&env_with(&[]));
        assert_eq!(p.url, DEFAULT_URL);
        assert!(p.key_prefix.is_empty());
    }

    #[test]
    fn reads_url_and_key_prefix_from_env() {
        let p = RedisProperties::from_env(&env_with(&[
            ("leaf.redis.url", "redis://cache:6380/2"),
            ("leaf.redis.key-prefix", "app:"),
        ]));
        assert_eq!(p.url, "redis://cache:6380/2");
        assert_eq!(p.key_prefix, "app:");
    }

    #[test]
    fn relaxed_binding_reads_an_underscored_url_key() {
        // The relaxed view canonicalises LEAF_REDIS_URL → leaf.redis.url.
        let p = RedisProperties::from_env(&env_with(&[("LEAF_REDIS_URL", "redis://h/1")]));
        assert_eq!(p.url, "redis://h/1");
    }

    #[test]
    fn default_impl_matches_default_url() {
        assert_eq!(RedisProperties::default().url, DEFAULT_URL);
    }
}
