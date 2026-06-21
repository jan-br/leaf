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
//! ## The bundle — the HTTP transport stack (sub-project A)
//!
//! This bundle is the leaf-web HTTP stack the web spec defines:
//!
//! - [`leaf_web`] — the backend-free web ABSTRACTIONS (`Request`/`Response`/
//!   `WebServer`/`Handler`/`Route`/`WebFilter`/`HttpMessageConverter`/`ControlAdvice` +
//!   the `EmbeddedWebServer` `#[keep_alive]` that self-assembles the dispatcher from the
//!   container + serves on the lifecycle machinery).
//! - [`leaf_web_hyper`] — the hyper BACKEND: its `#[auto_config]` FALLBACK
//!   `dyn WebServer` bean (`OnMissingBean(dyn WebServer)`), so merely pulling the
//!   bundle makes an app serve, while a user backend supersedes it.
//! - [`leaf_serde`] — the JSON `HttpMessageConverter` `#[component]` bean (content
//!   negotiation), its `web-converter` feature on by default.
//! - [`leaf_tokio`] (the runtime peer the server serves on), [`leaf_validation`]
//!   (request/bean validation), and [`leaf_cache`] (response/method caching) —
//!   the cross-cutting web concerns.
//!
//! Each crate's auto-config participates + backs off independently; the backend
//! choice is a runtime/profile decision, NEVER an XOR cargo feature. A future
//! ecosystem backend (an alternative `WebServer`) slots in HERE as an extra
//! dependency, superseding the hyper FALLBACK via the same `OnMissingBean` model.
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
pub use leaf_serde;
#[doc(no_inline)]
pub use leaf_tokio;
#[doc(no_inline)]
pub use leaf_validation;
#[doc(no_inline)]
pub use leaf_web;
#[doc(no_inline)]
pub use leaf_web_hyper;
