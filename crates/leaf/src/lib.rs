//! `leaf` — Umbrella/facade = the BOM coordination point + the single dep a downstream app names. Re-exports core+macros+enabled integrations; owns the force-link shim + ExpectedManifest. DAG sink.
//!
//! Skeleton crate. Implementation lands per the design corpus in `docs/design/`
//! (phase3 subsystem docs + phase2 `TOOLKIT.md`), built kernel-first.
