//! `leaf::prelude` — the one glob a leaf application imports.
//!
//! `use leaf::prelude::*;` brings into scope every annotation macro PLUS the core
//! types a user needs to write beans, run the app, and handle faults — so the user
//! never names a hyphenated crate (`leaf-core`/`leaf-macros`/`leaf-boot`) directly.
//!
//! ## What's in scope
//!
//! - **Stereotype + bean macros:** [`component`], [`service`], [`repository`],
//!   [`controller`], [`configuration`], [`bean`], [`register_component`].
//! - **Config + value binding:** [`config_properties`], [`value`], [`BindTarget`],
//!   [`converter`].
//! - **Conditions + auto-config:** [`conditional`], [`profile`], [`auto_config`],
//!   [`import`].
//! - **Declarative advice / AOP + cross-cutting concerns:** [`advisable`],
//!   [`aspect`], [`advice`], [`pointcut`], [`transactional`],
//!   [`cacheable`], [`cache_put`], [`cache_evict`], [`validated`], [`retryable`],
//!   [`concurrency_limit`].
//! - **Events + scheduling + resources:** [`event_listener`],
//!   [`transactional_event_listener`], [`scheduled`], [`resource`], [`catalog`].
//! - **The application entry:** [`main`] (so `#[leaf::main]` works under the glob,
//!   though the canonical spelling stays `#[leaf::main]`), [`runner`],
//!   [`failure_analyzer`].
//! - **The handle currency + deferral family:** [`Ref`], [`Lookup`], [`LazyRef`],
//!   [`Inject`].
//! - **The run engine:** [`Application`], [`Runner`].
//! - **The error spine:** [`LeafError`].
//! - **The advice-manager traits** (the SPIs a user implements to back a concern):
//!   [`CacheManager`], [`TransactionManager`].
//! - **The bootstrap bridge:** [`bootstrap`] (the default-runtime run entry).

// ── every annotation macro (the maximal-magic surface, charter §2.10) ──
#[doc(no_inline)]
pub use leaf_macros::{
    advice, advisable, aspect, async_impl, auto_config, bean, cache_evict, cache_put, cacheable, catalog,
    component, concurrency_limit, conditional, config_properties, configuration, controller,
    converter, event_listener, failure_analyzer, import, main, pointcut, profile,
    register_component, repository, resource, retryable, runner, scheduled, service, transactional,
    transactional_event_listener, validated, value, BindTarget,
};

// ── the handle currency + the honest-visible deferral family ──
//
// `Ref<T>` is the ONE shared-handle sugar; `Lookup`/`LazyRef`/`Inject` are the
// resolve-on-demand deferral handles (the only construction-cycle break).
#[doc(no_inline)]
pub use leaf_core::{Inject, LazyRef, Lookup, Ref};

// ── the error spine ──
#[doc(no_inline)]
pub use leaf_core::LeafError;

// ── the run engine + the runner SPI ──
#[doc(no_inline)]
pub use leaf_boot::Application;
#[doc(no_inline)]
pub use leaf_core::Runner;

// ── the advice-manager traits (the concern-backing SPIs a user implements) ──
#[doc(no_inline)]
pub use leaf_core::{CacheManager, TransactionManager};

// ── the default-runtime bootstrap bridge ──
#[doc(no_inline)]
pub use crate::runtime::bootstrap;
