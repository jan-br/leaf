//! The single LIVE-I/O round-trip — gated behind the `live-redis` feature so the
//! default `cargo test` never touches the network.
//!
//! Run with a real server up:
//! ```sh
//! LEAF_REDIS_TEST_URL=redis://127.0.0.1:6379/ \
//!   cargo test -p leaf-redis --features live-redis -- --ignored
//! ```
//! The test is BOTH feature-gated AND `#[ignore]`d: the feature off-switch keeps it
//! out of the normal build entirely, and the `#[ignore]` documents that it needs an
//! external service even when the feature is on.

/// The env var naming the test server (defaults to the conventional local server).
#[cfg(feature = "live-redis")]
pub const TEST_URL_ENV: &str = "LEAF_REDIS_TEST_URL";

#[cfg(all(test, feature = "live-redis"))]
mod live_tests {
    use crate::client::RedisClient;
    use crate::properties::RedisProperties;

    fn test_url() -> String {
        std::env::var(super::TEST_URL_ENV)
            .unwrap_or_else(|_| crate::properties::DEFAULT_URL.to_string())
    }

    #[tokio::test]
    #[ignore = "requires a live Redis server at $LEAF_REDIS_TEST_URL (run with --ignored)"]
    async fn opens_a_real_async_connection_and_pings() {
        use redis::AsyncCommands;
        let client = RedisClient::open(RedisProperties {
            url: test_url(),
            key_prefix: "leaf-redis-test:".into(),
        })
        .expect("opens");
        let mut conn = client.async_connection().await.expect("connects to live Redis");
        // A real round-trip: SET then GET through the live connection.
        let _: () = conn.set("leaf-redis-test:smoke", "ok").await.expect("set");
        let got: String = conn.get("leaf-redis-test:smoke").await.expect("get");
        assert_eq!(got, "ok");
        let _: () = conn.del("leaf-redis-test:smoke").await.expect("cleanup");
    }
}
