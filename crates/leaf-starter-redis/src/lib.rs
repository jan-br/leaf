//! `leaf-starter-redis` — a **CAPABILITY starter** (aggregator crate), the
//! Spring-Boot `spring-boot-starter-data-redis` analogue.
//!
//! ## What a starter is (phase3/03 TOPOLOGY "Starters & BOM")
//!
//! A starter is an almost code-free aggregator: a `Cargo.toml` that names a
//! curated set of integration crates plus a near-empty `lib.rs` that re-exports
//! them. Its VALUE is its dependency edges — a downstream that adds
//! `leaf-starter-redis` transitively pulls in [`leaf_redis`] **and** its runtime
//! peer [`leaf_tokio`], so the Redis auto-config rows + the tokio runtime land in
//! the build together as one coherent capability.
//!
//! A CAPABILITY starter (this crate) wraps ONE integration crate plus its peers;
//! contrast the STACK starter (`leaf-starter-web` = a curated multi-crate
//! bundle).
//!
//! ## DAG: starters → integration crates, NEVER the umbrella
//!
//! This crate depends on its integration crate ([`leaf_redis`]) and runtime peer
//! ([`leaf_tokio`]) — both leaf-core-only crates — and **never on the `leaf`
//! umbrella**. The umbrella is the unique DAG sink: it depends on the starters,
//! nothing depends on it. Wiring it the other way would be a hard Cargo cycle.
//! The umbrella's `redis` capability feature is `dep:`-hidden onto THIS crate, so
//! enabling it pulls this starter (→ `leaf-redis` + `leaf-tokio`) into the
//! force-link / `ExpectedManifest` participating set.
//!
//! ## Two-gate activation (this crate handles only the first gate)
//!
//! A cargo feature gates COMPILATION, not LINKAGE — so pulling this starter is
//! the PARTICIPATION gate (the integration crate is compiled + present to be
//! force-linked + self-checked + in the candidate batch). The SECOND gate —
//! whether `RedisAutoConfig` actually WIRES — is leaf-boot's runtime decision
//! (`CondExpr` guard matches, not excluded, the `@Fallback` candidate loses to no
//! user bean). See [`leaf_redis`] for those rows and guards.
//!
//! ## Surface
//!
//! Re-exported flat so a downstream depending only on the starter reaches the
//! capability without naming the constituent crates:

#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[doc(no_inline)]
pub use leaf_redis;
#[doc(no_inline)]
pub use leaf_tokio;
