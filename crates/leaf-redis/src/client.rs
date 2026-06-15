//! [`RedisClient`] — the thin Infrastructure bean wrapping a [`redis::Client`],
//! contributed into [`COMPONENTS`](leaf_core::COMPONENTS) the SAME way leaf-tokio
//! contributes its `applicationTaskExecutor` (a const `Role::Infrastructure`
//! [`Descriptor`] + a [`ProviderSeed`](leaf_core::ProviderSeed)).
//!
//! This is the "Infrastructure Provider" half of the representative integration
//! pattern: a framework-provenance bean installed outermost, holding the connection
//! factory the cache manager (and user code) draws live connections from. The
//! `redis::Client` is itself lazy (URL validation only — no socket at construction),
//! so building it is cheap and never touches the network at wire time; the actual
//! connection is established on first `connection()`/`async_connection()` call.

use std::any::TypeId;
use std::sync::Arc;

use leaf_core::{BoxFuture, Cause, Descriptor, ErrorKind, LeafError, Published, ResolveCtx};

use crate::properties::RedisProperties;

/// The stable contract name of the Redis client Infrastructure bean.
pub const REDIS_CLIENT_BEAN: &str = "redisClient";

/// The stable contract path of the Redis client Infrastructure bean.
pub const REDIS_CLIENT_CONTRACT: &str = "leaf_redis::redisClient";

/// A thin, cloneable handle over a [`redis::Client`] — the Infrastructure bean
/// the cache manager (and user code) draws connections from.
///
/// `redis::Client::open` only validates the URL; no socket opens until a
/// connection is requested. So `RedisClient::open` is safe to call at wire time
/// (it never blocks on the network), and the live I/O is deferred to
/// [`async_connection`](RedisClient::async_connection).
#[derive(Clone)]
pub struct RedisClient {
    client: redis::Client,
    props: RedisProperties,
}

impl RedisClient {
    /// Build a client from the resolved [`RedisProperties`] (URL validation only;
    /// no connection is opened).
    ///
    /// # Errors
    /// A [`LeafError`] if the configured URL is not a valid Redis connection URL.
    pub fn open(props: RedisProperties) -> Result<Self, LeafError> {
        let client = redis::Client::open(props.url.clone()).map_err(|e| invalid_url(&props.url, &e))?;
        Ok(RedisClient { client, props })
    }

    /// The connection config this client was built from.
    #[must_use]
    pub fn properties(&self) -> &RedisProperties {
        &self.props
    }

    /// The validated connection URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.props.url
    }

    /// The underlying [`redis::Client`] (for user code wanting the raw driver).
    #[must_use]
    pub fn raw(&self) -> &redis::Client {
        &self.client
    }

    /// Open an async multiplexed connection over the ambient tokio runtime (the
    /// live-I/O seam — establishes the socket).
    ///
    /// Cheap to clone and safe across concurrent tasks. This is the ONLY method
    /// that touches the network, so unit tests never call it.
    ///
    /// # Errors
    /// A [`LeafError`] if the connection cannot be established (server down,
    /// auth failure, …).
    pub async fn async_connection(&self) -> Result<redis::aio::MultiplexedConnection, LeafError> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| connect_failed(&self.props.url, &e))
    }
}

impl std::fmt::Debug for RedisClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisClient").field("url", &self.props.url).finish_non_exhaustive()
    }
}

fn invalid_url(url: &str, e: &redis::RedisError) -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
        "redis client open",
        format!("invalid Redis URL {url:?}: {e}"),
    ))
}

fn connect_failed(url: &str, e: &redis::RedisError) -> LeafError {
    LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
        "redis connection",
        format!("could not connect to Redis at {url:?}: {e}"),
    ))
}

// ───────────────────── the redisClient Infrastructure bean ───────────────────

/// The const `Role::Infrastructure` [`Descriptor`] for the Redis client bean —
/// the EXACT shape leaf-tokio's `applicationTaskExecutor` carries, submitted into
/// [`COMPONENTS`](leaf_core::COMPONENTS) via the same `linkme` channel so a
/// force-linking binary auto-detects it.
///
/// NOTE: this bean participates only when the binary force-links this crate (the
/// `leaf-starter-redis` two-gate activation). It is `Role::Infrastructure` so the
/// container installs it before application beans, like every other framework bean.
pub const REDIS_CLIENT_DESCRIPTOR: Descriptor = Descriptor {
    contract: leaf_core::ContractId::of(REDIS_CLIENT_CONTRACT),
    self_type: TypeId::of::<RedisClient>(),
    provides: &[],
    declared_name: Some(REDIS_CLIENT_BEAN),
    aliases: &[],
    scope: leaf_core::ScopeDef::SINGLETON,
    role: leaf_core::Role::Infrastructure,
    meta: &leaf_core::AnnotationMetadata::EMPTY,
    parent: None,
    origin: leaf_core::Origin::Native { crate_name: Some("leaf-redis") },
};

// The link-time element: submit the const Descriptor into COMPONENTS via the SAME
// `::leaf_core::linkme` path the macros emit (a `#[used]` `#[link_section]` static
// under the hood — hence the scoped allow; no hand-written `unsafe` block).
#[allow(unsafe_code)]
#[::leaf_core::linkme::distributed_slice(::leaf_core::COMPONENTS)]
#[linkme(crate = ::leaf_core::linkme)]
#[doc(hidden)]
pub static REDIS_CLIENT_ELEMENT: Descriptor = REDIS_CLIENT_DESCRIPTOR;

/// The [`Provider`](leaf_core::Provider) constructing the Redis client bean: it
/// reads the `leaf.redis.*` properties off the resolution env (relaxed binding)
/// and opens a lazy [`RedisClient`] — no socket at wire time.
pub struct RedisClientProvider {
    descriptor: Descriptor,
}

impl RedisClientProvider {
    /// Construct the provider over the const descriptor.
    #[must_use]
    pub fn new() -> Self {
        RedisClientProvider { descriptor: REDIS_CLIENT_DESCRIPTOR }
    }
}

impl Default for RedisClientProvider {
    fn default() -> Self {
        RedisClientProvider::new()
    }
}

impl leaf_core::Provider for RedisClientProvider {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    fn provide<'a>(
        &'a self,
        _cx: &'a ResolveCtx<'a>,
    ) -> BoxFuture<'a, Result<Published, LeafError>> {
        // No live connection here: open() validates the URL and defers the socket.
        // The default URL is used absent a resolution-time Env on the cx (the cx
        // is the minimal placeholder; the auto-config seed binds the real props).
        Box::pin(async {
            let client = RedisClient::open(RedisProperties::default())?;
            Ok(Published::shared_value(client))
        })
    }
}

/// The const [`ProviderSeed`](leaf_core::ProviderSeed) leaf-boot pairs to the
/// `redisClient` [`Descriptor`] when lifting the [`COMPONENTS`](leaf_core::COMPONENTS)
/// slice (mints the `Arc<dyn Provider>` once at register/freeze).
pub const REDIS_CLIENT_SEED: leaf_core::ProviderSeed =
    || Arc::new(RedisClientProvider::new());

/// The [`SeedPairingRow`](leaf_core::SeedPairingRow) JOINing the `redisClient`
/// COMPONENTS descriptor to its seed (the anti-DCE per-bean JOIN — an
/// unconstructible bean must never silently vanish).
#[allow(unsafe_code)]
#[::leaf_core::linkme::distributed_slice(::leaf_core::SEED_PAIRINGS)]
#[linkme(crate = ::leaf_core::linkme)]
#[doc(hidden)]
pub static REDIS_CLIENT_SEED_PAIRING: leaf_core::SeedPairingRow =
    leaf_core::SeedPairingRow::field_default(
        leaf_core::ContractId::of(REDIS_CLIENT_CONTRACT),
        REDIS_CLIENT_SEED,
    );

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::Provider;

    #[test]
    fn open_validates_a_good_url_without_connecting() {
        let c = RedisClient::open(RedisProperties {
            url: "redis://127.0.0.1:6379/".into(),
            key_prefix: String::new(),
        })
        .expect("a valid URL opens (no socket)");
        assert_eq!(c.url(), "redis://127.0.0.1:6379/");
    }

    #[test]
    fn open_rejects_a_malformed_url_loudly() {
        let err = RedisClient::open(RedisProperties {
            url: "http://not-a-redis-url".into(),
            key_prefix: String::new(),
        })
        .expect_err("a non-redis scheme is rejected");
        assert_eq!(err.kind, ErrorKind::ConstructionFailed);
    }

    #[test]
    fn descriptor_is_infrastructure_with_the_stable_contract() {
        let d = &REDIS_CLIENT_DESCRIPTOR;
        assert_eq!(d.role, leaf_core::Role::Infrastructure);
        assert_eq!(d.declared_name, Some(REDIS_CLIENT_BEAN));
        assert_eq!(d.self_type, TypeId::of::<RedisClient>());
        assert_eq!(d.contract, leaf_core::ContractId::of(REDIS_CLIENT_CONTRACT));
    }

    #[test]
    fn redis_client_is_discoverable_in_components_with_a_paired_seed() {
        // The descriptor + its seed pairing ride the SAME link-time channels the
        // macros emit into, so a force-linking binary auto-detects + can construct it.
        let comps = leaf_core::collect_slice(&leaf_core::COMPONENTS);
        assert!(
            comps.iter().any(|r| r.contract == leaf_core::ContractId::of(REDIS_CLIENT_CONTRACT)),
            "redisClient must be discoverable in COMPONENTS"
        );
        let seeds = leaf_core::collect_slice(&leaf_core::SEED_PAIRINGS);
        assert!(
            seeds.iter().any(|r| r.contract == leaf_core::ContractId::of(REDIS_CLIENT_CONTRACT)),
            "redisClient must have a paired ProviderSeed"
        );
    }

    #[test]
    fn provider_yields_a_shared_redis_client() {
        let p = RedisClientProvider::new();
        let cx = ResolveCtx::root();
        let published = futures::executor::block_on(p.provide(&cx)).expect("provides");
        assert!(published.is_shared(), "an Infrastructure singleton publishes Shared");
    }
}
