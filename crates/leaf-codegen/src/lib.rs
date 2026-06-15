//! `leaf-codegen` — Heavy unit-testable codegen logic the thin macros + build.rs call (annotation merge, force-link + ExpectedManifest, parsers, metadata rollup).
//!
//! Implementation lands per the design corpus in `docs/design/` (phase3 subsystem
//! docs + phase2 `TOOLKIT.md`), built kernel-first.
//!
//! `leaf-codegen` is a NORMAL library (not a proc-macro): it uses
//! `proc-macro2`/`syn`/`quote` so every codegen routine is unit-testable WITHOUT a
//! compiler/link/runtime. The thin `leaf-macros` proc-macro parses with `syn`,
//! delegates ALL logic here, and emits the resulting [`proc_macro2::TokenStream`].
//!
//! ## Modules
//!
//! - [`annotation`] — the composed-annotation merge / `@AliasFor` / distance
//!   engine that lowers `Descriptor.meta` (unit 1).
//! - [`descriptor`] — the const `Descriptor` + `ProviderSeed` + `InjectionPlan`
//!   row emitter, the heart of the four-layer pipeline (unit 2).
//! - [`stereotype`] — the `@component`/`@service`/… vocabulary as DATA + the
//!   `syn::ItemStruct` → `descriptor::BeanInput` lowering the thin stereotype
//!   macros call (component-stereotypes).
//! - [`forcelink`] — the build.rs anti-DCE emitters: the Layer-0 force-link shim
//!   (`use <crate> as _;`) + the const `ExpectedManifest` self-check anchor.
//! - [`constfold`] — the build-time `CondExpr` ConstFold folder + the deferred
//!   auto-config ordering plan (`cargo leaf prepare`).
//! - [`parsers`] — the embedded `${…}`/`#{…}` value-template parser + the
//!   message-bundle parser the value / catalog codegen call.
//! - [`rollup`] — the config-metadata rollup over `CONFIG_METADATA` + the
//!   duplicate-prefix check.
//! - [`cargo_leaf`] — the clap-free `cargo leaf` subcommand skeleton.
//! - [`advisor`] — the `#[advisable]`/`#[aspect]`/`#[advice]`/`#[pointcut]` AOP
//!   lowering: the `ADVISORS` identity row + the chain-order pairing const
//!   (declarative-advice phase3/09).
//! - [`listener`] — the `#[event_listener]`/`#[transactional_event_listener]`
//!   lowering: the `EVENT_LISTENERS` row + the defer/phase + condition dispatch
//!   metadata (events phase3/12).
//! - [`scheduling`] — the `#[scheduled]`/`#[cacheable]`/`#[resource]`/`#[catalog]`
//!   lowering into the `SCHEDULED`/`ADVISORS`/`RESOURCES`/`CATALOGS` channels.
//! - [`app`] — the `#[leaf::main]` binary-crate entry + anti-DCE seam, the
//!   `#[runner]` `dyn Runner` bean, and the `#[failure_analyzer]` SPI row.

pub mod advisor;
pub mod annotation;
pub mod app;
pub mod cargo_leaf;
pub mod conditional;
pub mod config;
pub mod constfold;
pub mod descriptor;
pub mod forcelink;
pub mod listener;
pub mod parsers;
pub mod rollup;
pub mod scheduling;
pub mod stereotype;
