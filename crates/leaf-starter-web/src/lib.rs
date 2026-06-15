//! `leaf-starter-web` — a **STACK starter** (aggregator crate), the
//! Spring-Boot `spring-boot-starter-web` analogue.
//!
//! ## STACK starter vs CAPABILITY starter
//!
//! Where `leaf-starter-redis` wraps ONE capability (`leaf-redis` + its runtime
//! peer), a STACK starter is a CURATED ADDITIVE BUNDLE: it names the several
//! crates a typical web application wants, so a downstream adds one dependency
//! and gets the coherent set. The bundle is additive (each crate's auto-config
//! participates and backs off independently); the backend choice is a
//! runtime/profile decision, NEVER an XOR cargo feature.
//!
//! ## NOTE — representative bundle (charter non-ecosystem scope)
//!
//! The spring-boot-starter-web ideal is `leaf-router + leaf-tokio + leaf-json +
//! leaf-validation` (phase3/03 TOPOLOGY "Starters & BOM"). But `leaf-router` and
//! `leaf-json` are REAL web integration crates = **ecosystem**, which the
//! charter's non-ecosystem scope EXCLUDES. So this crate is a REPRESENTATIVE
//! stack starter over the non-ecosystem crates that DO exist — [`leaf_tokio`]
//! (the runtime), [`leaf_validation`] (request/bean validation), and
//! [`leaf_cache`] (response/method caching) — which is enough to prove the
//! stack-starter SHAPE.
//!
//! Real web bindings follow the [`leaf_redis`](https://docs.rs/leaf-redis)
//! integration pattern (an integration crate depending on leaf-core + a runtime +
//! a 3rd-party lib, contributing `AUTO_CONFIGS` rows + Infrastructure Providers,
//! never the umbrella) and slot in HERE as additional dependencies + re-exports
//! when the ecosystem ships them.
//!
//! ## DAG: starters → integration crates, NEVER the umbrella
//!
//! Like every starter, this crate depends only on its constituent (leaf-core-only)
//! crates and **never on the `leaf` umbrella** — the umbrella is the unique DAG
//! sink (it depends on the starters; nothing depends on it). The umbrella's `web`
//! capability feature is `dep:`-hidden onto this crate, pulling the whole bundle
//! into the force-link / `ExpectedManifest` participating set when enabled.
//!
//! ## Surface
//!
//! Re-exported flat so a downstream depending only on the starter reaches the
//! whole stack without naming the constituent crates:

#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[doc(no_inline)]
pub use leaf_cache;
#[doc(no_inline)]
pub use leaf_tokio;
#[doc(no_inline)]
pub use leaf_validation;
