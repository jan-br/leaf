# gRPC Server Reflection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in gRPC Server Reflection (`grpc.reflection.v1` + `v1alpha`) to leaf as a dogfooded `#[grpc_controller]` that auto-discovers every registered service's descriptors.

**Architecture:** `leaf-grpc-build` emits each compiled proto's encoded `FileDescriptorSet` into a `leaf-grpc` `linkme` discovery slice (`REFLECTED_FILE_DESCRIPTOR_SETS`) automatically. A version-agnostic `ReflectionIndex` core decodes the collected sets and answers the reflection queries (with transitive dependency-closure resolution). Two thin `#[grpc_controller]`s (v1 + v1alpha), gated `#[conditional(on_property = "leaf.grpc.reflection.enabled")]`, serve the bidi `ServerReflectionInfo` RPC over the existing streaming machinery. The `#[grpc_controller]` codegen gains condition-propagation so a conditioned controller gates its generated routes as a unit.

**Tech Stack:** Rust, leaf DI, `prost`/`prost-types` (FDS decode), `protox`+`prost-build` (build), `linkme` (discovery channel via leaf-core), `tonic` (dev-only reflection-client test peer).

**Spec:** `docs/superpowers/specs/2026-06-22-grpc-reflection-design.md`

---

## File structure (new + modified)

**Modified**
- `crates/leaf-grpc/` — the `REFLECTED_FILE_DESCRIPTOR_SETS` slice + `linkme` re-export; the `reflection` module (the `ReflectionIndex` core); `reflection.proto` (v1 + v1alpha) + the two reflection `#[grpc_controller]`s; the opt-in auto-config gate.
- `crates/leaf-grpc-build/` — `compile()` writes each package's `.fds` + the generator emits the `FILE_DESCRIPTOR_SET` const + the `__LEAF_FDS_<PKG>` slice-registration row.
- `crates/leaf-codegen/` (`grpc_controller.rs`) — propagate the controller struct's `#[conditional]`/`#[profile]` guards onto its generated `GrpcRoute` beans.
- `examples/storefront/` — becomes reflectable (a reflection-on integration test).

**Dependency direction (unchanged):** `leaf-grpc -> leaf-web -> leaf-core`; `leaf-grpc-build` is the build-helper; `tonic`/`tonic-build` stay dev/build-only. The reflection index keys on FDS wire symbols, never Rust type names.

---

## Stage 1: FileDescriptorSet emission + discovery slice

This stage adds the runtime discovery channel and makes every proto a leaf-grpc-build compiles contribute its encoded `FileDescriptorSet` to it — automatically, inert until reflection reads it.

**Key build-pipeline fact (verified):** the leaf pipeline drives prost-build via `Config::compile_fds(fds)` (protox already produced the `FileDescriptorSet`, no `protoc`). In prost-build 0.13.5 `compile_fds` does **not** honor `Config::file_descriptor_set_path` — that field is read only on the protoc/`load_fds` path (`crates`-registry `prost-build-0.13.5/src/config.rs:815` consumes `fds` directly and never writes it). So `compile()` writes the FDS to `<OUT_DIR>/<pkg>.fds` **itself** from the protox `fds` it already holds, grouping by package so each `.fds` mirrors the `<pkg>.rs` module prost-build emits (`Module::to_file_name_or` → dotted package, e.g. `echo.v1.rs` ⇒ `echo.v1.fds`). The generator then appends the `FILE_DESCRIPTOR_SET` const + the slice-registration row to each `<pkg>.rs`. This realizes the shared contract verbatim (`FILE_DESCRIPTOR_SET`, `REFLECTED_FILE_DESCRIPTOR_SETS`, the `__LEAF_FDS_<PKG>` row) — only the write mechanism differs from a naive `file_descriptor_set_path` call, which is a no-op under `compile_fds`.

### Files

- **Modify** `crates/leaf-grpc/src/lib.rs` — add the `linkme` re-export + the `REFLECTED_FILE_DESCRIPTOR_SETS` distributed slice.
- **Modify** `crates/leaf-grpc-build/Cargo.toml` — promote `prost` to a normal + build dependency (FDS re-encode).
- **Modify** `crates/leaf-grpc-build/src/service_gen.rs` — add the pure render functions for the `FILE_DESCRIPTOR_SET` const + the `__LEAF_FDS_<PKG>` slice row (unit-testable, no compiler).
- **Modify** `crates/leaf-grpc-build/src/lib.rs` — `compile()` writes `<pkg>.fds` per package and appends the const + row to each `<pkg>.rs`.
- **Modify** `crates/leaf-grpc-build/build.rs` — re-host the same FDS write so the build-output test sees it.
- **Modify** `crates/leaf-grpc-build/tests/generated_service.rs` — assert the const exists, equals the `.fds` bytes, and decodes.
- **Create** `crates/leaf-grpc/tests/reflected_fds_slice.rs` — assert the slice is non-empty at runtime for a crate that compiled a proto.

---

### Task 1.1: `REFLECTED_FILE_DESCRIPTOR_SETS` slice + linkme re-export in leaf-grpc

**Files:** `crates/leaf-grpc/src/lib.rs`, `crates/leaf-grpc/tests/reflected_fds_slice.rs`

- [ ] **Step 1: Write the failing runtime test.** This crate's `build.rs` already compiles `tests/proto/echo.proto`, so once the generator emits a registration row this crate links one. Create `crates/leaf-grpc/tests/reflected_fds_slice.rs`:

```rust
//! Stage 1 proof: a crate that compiled a proto via `leaf_grpc_build::compile` links a
//! row into `leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS` automatically (no app wiring),
//! and the collected bytes decode as a prost `FileDescriptorSet` naming the proto's pkg.

// leaf-grpc's own build.rs compiles tests/proto/echo.proto (package echo) -> the
// generated module + its FDS const + the __LEAF_FDS_ECHO registration row.
leaf_grpc::include_proto!("echo");

#[test]
fn the_compiled_proto_contributes_a_row_to_the_discovery_slice() {
    // linkme collects every `#[distributed_slice(REFLECTED_FILE_DESCRIPTOR_SETS)]` row.
    let sets: &[&'static [u8]] = &leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS;
    assert!(
        !sets.is_empty(),
        "a crate that compiled a proto must contribute its FDS to the slice"
    );
}

#[test]
fn the_collected_fds_decode_and_name_the_proto_package() {
    use leaf_grpc::prost::Message;
    let mut packages = std::collections::BTreeSet::new();
    for bytes in leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS {
        let fds = leaf_grpc::prost::alloc::vec::Vec::new(); // touch alloc re-export path
        let _ = fds;
        let decoded = ::prost_types::FileDescriptorSet::decode(*bytes)
            .expect("each slice row is a valid encoded FileDescriptorSet");
        for file in decoded.file {
            if let Some(pkg) = file.package {
                packages.insert(pkg);
            }
        }
    }
    assert!(
        packages.contains("echo"),
        "the echo.proto package must appear in the collected descriptor sets, got {packages:?}"
    );
}
```

`prost_types` is already a transitive dep through `prost`, but assert it explicitly: add `prost-types` to leaf-grpc `[dev-dependencies]` in the same edit if `cargo test` reports it unresolved (the run step below catches this).

- [ ] **Step 2: Run it — fails to compile (no slice, no re-export).**

```
cargo test -p leaf-grpc --test reflected_fds_slice
```

Expected: `error[E0425]: cannot find value REFLECTED_FILE_DESCRIPTOR_SETS in crate leaf_grpc` (and `leaf_grpc::prost` may resolve already; the slice and `linkme` re-export are the gap).

- [ ] **Step 3: Add the linkme re-export + the slice to `crates/leaf-grpc/src/lib.rs`.** leaf-core does `pub use linkme;` (`crates/leaf-core/src/lib.rs:99`), so re-export through it — leaf-grpc names no bare `linkme` dep, exactly as `COMPONENTS`/`AUTO_CONFIGS` rows resolve `::leaf_core::linkme`. Insert after the `pub use prost;` block (around line 53):

```rust
// `linkme` re-exported THROUGH leaf-core (which does `pub use linkme;`), so the
// `leaf-grpc-build`-generated FDS registration rows resolve `::leaf_grpc::linkme`
// without leaf-grpc naming a bare `linkme` dependency — the same indirection the
// COMPONENTS/AUTO_CONFIGS rows use for `::leaf_core::linkme`.
#[doc(no_inline)]
pub use leaf_core::linkme;

/// The gRPC reflection discovery channel: every proto compiled by
/// `leaf_grpc_build::compile` contributes its encoded `prost_types::FileDescriptorSet`
/// bytes here via a generated `#[distributed_slice]` row — no app wiring. Mirrors
/// leaf-core's `COMPONENTS`/`AUTO_CONFIGS` channels (collected at link time). The bytes
/// are inert static data whether or not reflection is enabled; the reflection index
/// (a later stage) is the only reader.
#[::leaf_grpc::linkme::distributed_slice]
pub static REFLECTED_FILE_DESCRIPTOR_SETS: [&'static [u8]] = [..];
```

Note: inside leaf-grpc's own crate the `#[::leaf_grpc::...]` self-path resolves (there is an implicit `extern crate self as leaf_grpc` for 2018+; if the build flags it, use `#[linkme::distributed_slice]` here since the re-export is in scope). Prefer the self-qualified form to match the cross-crate generated row exactly.

- [ ] **Step 4: Run it — passes.**

```
cargo test -p leaf-grpc --test reflected_fds_slice
```

Expected: `the_compiled_proto_contributes_a_row_to_the_discovery_slice ... ok` and `the_collected_fds_decode_and_name_the_proto_package ... ok` — but this depends on Tasks 1.2–1.4 emitting the row. Until those land, this test still fails on an empty slice. Mark it `#[ignore]` with a `// un-ignore after Task 1.4` note, OR (preferred) land Tasks 1.2–1.4 first and run this last. The plan orders the slice decl first (this task) so the type exists; flip the ignore off in Task 1.4 Step 5.

If `leaf_grpc::prost::alloc` line errors, delete the two `fds`/`_` lines (they only exercise the re-export path defensively) — keep the decode + package assertions.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/lib.rs crates/leaf-grpc/tests/reflected_fds_slice.rs
git commit -m "leaf-grpc: REFLECTED_FILE_DESCRIPTOR_SETS discovery slice + linkme re-export

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 1.2: Pure render functions for the FDS const + the slice row

**Files:** `crates/leaf-grpc-build/src/service_gen.rs`

The generator writes Rust source into a `String` (the leaf-codegen discipline: unit-testable without a compiler). Add two pure functions + a combined emitter, keyed on the proto **package name** — never on a Rust type name (the const ident is pure case mechanics over the package string, the same discipline as `const_ident`/`module_ident` already in this file).

- [ ] **Step 1: Write the failing unit tests.** Add to the `#[cfg(test)] mod tests` block at the end of `crates/leaf-grpc-build/src/service_gen.rs`:

```rust
#[test]
fn renders_the_file_descriptor_set_const_including_the_fds_file() {
    let c = render_fds_const("echo.v1");
    let flat = c.split_whitespace().collect::<String>();
    assert!(
        flat.contains("pubconstFILE_DESCRIPTOR_SET:&[u8]"),
        "the const is the public FDS byte slice: {flat}"
    );
    assert!(
        flat.contains(r#"include_bytes!(concat!(env!("OUT_DIR"),"/echo.v1.fds"))"#),
        "the const embeds the sibling <pkg>.fds via include_bytes!: {flat}"
    );
}

#[test]
fn renders_the_distributed_slice_registration_row_for_the_package() {
    let r = render_fds_slice_row("echo.v1");
    let flat = r.split_whitespace().collect::<String>();
    // The linkme attribute routes through leaf-grpc's re-export, like declare_source!'s
    // `#[linkme(crate = ::leaf_core::linkme)]` does for leaf-core's slices.
    assert!(
        flat.contains("#[::leaf_grpc::linkme::distributed_slice(::leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS)]"),
        "the row joins the leaf-grpc discovery slice: {flat}"
    );
    assert!(
        flat.contains("#[linkme(crate=::leaf_grpc::linkme)]"),
        "the row pins linkme to the leaf-grpc re-export: {flat}"
    );
    // The static ident is the SCREAMING package, deterministic + unique per package.
    assert!(
        flat.contains("static__LEAF_FDS_ECHO_V1:&[u8]=&FILE_DESCRIPTOR_SET;"),
        "the row contributes the package's FILE_DESCRIPTOR_SET: {flat}"
    );
}

#[test]
fn fds_static_ident_is_pure_case_mechanics_over_the_package_dots() {
    // No type-name detection: the ident is the package text upper-cased with dots->_.
    assert_eq!(fds_static_ident("echo.v1"), "__LEAF_FDS_ECHO_V1");
    assert_eq!(fds_static_ident("grpc.reflection.v1alpha"), "__LEAF_FDS_GRPC_REFLECTION_V1ALPHA");
    assert_eq!(fds_static_ident(""), "__LEAF_FDS_");
}

#[test]
fn the_emitted_fds_block_parses_as_rust_items() {
    let src = render_fds_block("echo.v1");
    syn::parse_str::<syn::File>(&src).expect("the FDS const + row parse as valid Rust items");
}

#[test]
fn the_fds_block_carries_an_allow_for_the_generated_static_ident() {
    // The SCREAMING static ident + the unread const trip non_upper_case_globals/dead_code
    // only in rust-analyzer's eyes for generated code — emit the allow (MEMORY: emit
    // #[allow] on generated items; rustc skips macro-gen naming lints but RA doesn't).
    let flat = render_fds_block("echo.v1").split_whitespace().collect::<String>();
    assert!(flat.contains("#[allow(dead_code)]") || flat.contains("#![allow(dead_code)]"),
        "the const is dead unless reflection reads it: {flat}");
}
```

- [ ] **Step 2: Run them — fail (functions don't exist).**

```
cargo test -p leaf-grpc-build --lib
```

Expected: `error[E0425]: cannot find function render_fds_const` (and the other three).

- [ ] **Step 3: Add the pure functions** to `crates/leaf-grpc-build/src/service_gen.rs`, just before the `LeafServiceGenerator` struct (after `spec_from_service`):

```rust
/// The SCREAMING_SNAKE static ident for a package's FDS registration row
/// (`echo.v1` -> `__LEAF_FDS_ECHO_V1`). PURE case mechanics over the package's OWN
/// dotted text — dots become `_`, letters/digits upper-case — NEVER type-name
/// detection (no behavior is keyed on the spelling; an empty package yields
/// `__LEAF_FDS_`). Deterministic + unique per package, so a second proto in the same
/// package would collide loudly (one FDS per package module, by construction).
#[must_use]
fn fds_static_ident(package: &str) -> String {
    let mut out = String::from("__LEAF_FDS_");
    for ch in package.chars() {
        if ch == '.' {
            out.push('_');
        } else {
            out.push(ch.to_ascii_uppercase());
        }
    }
    out
}

/// The dotted `<pkg>.fds` sibling file name prost-build's module naming implies
/// (`Module::to_file_name_or` joins package components with `.`, so the module is
/// `echo.v1.rs` and its FDS sibling is `echo.v1.fds`). For the empty package prost-build
/// uses the default filename root; leaf compiles only packaged protos, but mirror the
/// default cleanly: empty package -> the default `<default>.fds` is handled by `compile`,
/// not here (this renders the dotted form).
#[must_use]
fn fds_file_name(package: &str) -> String {
    format!("{package}.fds")
}

/// `pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"),
/// "/<pkg>.fds"));` — the package's encoded `FileDescriptorSet`, embedded from the
/// sibling `.fds` `compile()` writes beside the generated `<pkg>.rs`.
#[must_use]
fn render_fds_const(package: &str) -> String {
    let file = fds_file_name(package);
    format!(
        "pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{file}\"));"
    )
}

/// The `#[distributed_slice]` row contributing this package's `FILE_DESCRIPTOR_SET` to
/// `leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS`. Routes linkme through the leaf-grpc
/// re-export (`#[linkme(crate = ::leaf_grpc::linkme)]`), exactly as `declare_source!`
/// routes through `::leaf_core::linkme`.
#[must_use]
fn render_fds_slice_row(package: &str) -> String {
    let ident = fds_static_ident(package);
    format!(
        "#[::leaf_grpc::linkme::distributed_slice(::leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS)]\n\
         #[linkme(crate = ::leaf_grpc::linkme)]\n\
         static {ident}: &[u8] = &FILE_DESCRIPTOR_SET;"
    )
}

/// The full FDS discovery block for one compiled package: the `FILE_DESCRIPTOR_SET`
/// const + its slice-registration row, under an `#[allow]` (the const is dead static
/// data unless reflection reads it; rust-analyzer lints generated idents that rustc
/// would skip). Emitted ONCE per proto package by `compile()`.
#[must_use]
pub fn render_fds_block(package: &str) -> String {
    let mut out = String::new();
    out.push_str("#[allow(dead_code, non_upper_case_globals)]\n");
    out.push_str("const _: () = ();\n"); // anchor the allow's item-position; see below
    out.push_str(&render_fds_const(package));
    out.push('\n');
    out.push_str(&render_fds_slice_row(package));
    out.push('\n');
    out
}
```

The `#[allow(...)]` must attach to the items it covers. The `const _: () = ();` anchor trick is fragile — instead wrap the two items so the allow applies. Replace the body of `render_fds_block` with the module-wrapped form that the parse test still accepts:

```rust
#[must_use]
pub fn render_fds_block(package: &str) -> String {
    // Inner `#[doc(hidden)] mod` so the inner `#![allow(...)]` covers BOTH items and the
    // SCREAMING static ident never collides with the surrounding generated module. The
    // const + row are re-exported with `pub use` so `FILE_DESCRIPTOR_SET` stays reachable
    // at the package-module path the integration test (and the reflection index) reads.
    let module = format!("__leaf_fds_{}", fds_static_ident(package).trim_start_matches("__LEAF_FDS_").to_ascii_lowercase());
    let mut out = String::new();
    out.push_str(&format!("pub mod {module} {{\n"));
    out.push_str("    #![allow(dead_code, non_upper_case_globals)]\n    ");
    out.push_str(&render_fds_const(package));
    out.push_str("\n    ");
    out.push_str(&render_fds_slice_row(package));
    out.push('\n');
    out.push_str("}\n");
    out.push_str(&format!("pub use {module}::FILE_DESCRIPTOR_SET;\n"));
    out
}
```

Then update the two block tests to match the wrapped form (re-run Step 1's tests against this shape):

```rust
#[test]
fn the_emitted_fds_block_parses_as_rust_items() {
    let src = render_fds_block("echo.v1");
    syn::parse_str::<syn::File>(&src).expect("the FDS const + row parse as valid Rust items");
}

#[test]
fn the_fds_block_reexports_the_const_at_the_package_path() {
    let flat = render_fds_block("echo.v1").split_whitespace().collect::<String>();
    assert!(flat.contains("pubuse"), "the const is re-exported to the package module path: {flat}");
    assert!(flat.contains("::FILE_DESCRIPTOR_SET;"), "re-exports FILE_DESCRIPTOR_SET: {flat}");
    assert!(flat.contains("#![allow(dead_code,non_upper_case_globals)]"), "inner allow covers both items: {flat}");
}
```

(Replace the earlier `the_fds_block_carries_an_allow_for_the_generated_static_ident` test with this `the_fds_block_reexports_the_const_at_the_package_path` test — the wrapped form's inner `#![allow]` is the realized shape.)

- [ ] **Step 4: Run them — pass.**

```
cargo test -p leaf-grpc-build --lib
```

Expected: all `render_fds_*` / `fds_static_ident` / block tests `... ok`, plus the existing service-gen tests stay green (`test result: ok.`).

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc-build/src/service_gen.rs
git commit -m "leaf-grpc-build: pure FILE_DESCRIPTOR_SET const + discovery-slice-row renderers

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 1.3: `compile()` writes `<pkg>.fds` per package + appends the FDS block

**Files:** `crates/leaf-grpc-build/Cargo.toml`, `crates/leaf-grpc-build/src/lib.rs`

`compile()` already holds the protox `fds` before handing it to `compile_fds`. Since `compile_fds` does NOT write the FDS (verified above), `compile()` groups `fds.file` by package, encodes one `FileDescriptorSet` per group to `<OUT_DIR>/<pkg>.fds`, runs `compile_fds`, then appends `render_fds_block(pkg)` to each generated `<pkg>.rs`. Encoding needs `prost::Message` — promote `prost` to a normal + build dep.

- [ ] **Step 1: Promote `prost` in `crates/leaf-grpc-build/Cargo.toml`.** Move it out of `[dev-dependencies]` into `[dependencies]` and add it to `[build-dependencies]` (the build.rs re-host in Task 1.4 needs it too). In `[dependencies]` add after `prost-types.workspace = true`:

```toml
# prost — re-encodes the per-package FileDescriptorSet to bytes for the <pkg>.fds the
# reflection discovery slice embeds (prost-types' descriptor types derive ::prost::Message).
prost.workspace = true
```

In `[build-dependencies]` add the same line after its `prost-types.workspace = true`. Leave the `[dev-dependencies]` `prost` entry (the generated test structs still need it at test-compile time) — a dep may appear in multiple tables.

- [ ] **Step 2: Write the failing unit test for the grouping helper.** Add to a new `#[cfg(test)] mod tests` in `crates/leaf-grpc-build/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ::prost_types::{FileDescriptorProto, FileDescriptorSet};

    fn file(pkg: &str, name: &str) -> FileDescriptorProto {
        FileDescriptorProto {
            name: Some(name.to_string()),
            package: Some(pkg.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn groups_descriptor_files_by_package_each_a_self_contained_set() {
        let fds = FileDescriptorSet {
            file: vec![
                file("echo.v1", "echo.proto"),
                file("echo.v1", "shared.proto"),
                file("other", "other.proto"),
            ],
        };
        let groups = group_fds_by_package(&fds);
        // One encoded set per package; echo.v1 carries BOTH its files.
        assert_eq!(groups.len(), 2);
        let echo = groups.get("echo.v1").expect("echo.v1 group present");
        use ::prost::Message;
        let decoded = FileDescriptorSet::decode(echo.as_slice()).expect("re-encoded set decodes");
        assert_eq!(decoded.file.len(), 2, "echo.v1's group holds both of its files");
        assert!(groups.contains_key("other"));
    }

    #[test]
    fn an_empty_package_groups_under_the_empty_key() {
        let fds = FileDescriptorSet { file: vec![file("", "root.proto")] };
        let groups = group_fds_by_package(&fds);
        assert!(groups.contains_key(""), "empty-package files group under the empty key");
    }
}
```

- [ ] **Step 3: Run it — fails (helper missing).**

```
cargo test -p leaf-grpc-build --lib groups_descriptor
```

Expected: `error[E0425]: cannot find function group_fds_by_package`.

- [ ] **Step 4: Implement the grouping helper + rewrite `compile()`** in `crates/leaf-grpc-build/src/lib.rs`. Replace the whole file body below the module doc + `pub mod service_gen;` with:

```rust
pub mod service_gen;

use std::collections::BTreeMap;

/// Group a `FileDescriptorSet`'s files by their proto package into one encoded,
/// self-contained `FileDescriptorSet` per package — the bytes embedded as that package
/// module's `FILE_DESCRIPTOR_SET`. Keyed on the package STRING the descriptor carries
/// (the gRPC wire identifier), never on a Rust type name. Returns a sorted map so the
/// emitted `.fds` set is deterministic across builds.
#[must_use]
fn group_fds_by_package(fds: &::prost_types::FileDescriptorSet) -> BTreeMap<String, Vec<u8>> {
    use ::prost::Message;
    let mut by_pkg: BTreeMap<String, ::prost_types::FileDescriptorSet> = BTreeMap::new();
    for file in &fds.file {
        let pkg = file.package.clone().unwrap_or_default();
        by_pkg.entry(pkg).or_default().file.push(file.clone());
    }
    by_pkg
        .into_iter()
        .map(|(pkg, set)| (pkg, set.encode_to_vec()))
        .collect()
}

/// Compile `protos` (resolved against `includes`) to Rust in `OUT_DIR`.
///
/// Pure-Rust pipeline: `protox` parses to a `FileDescriptorSet` (NO `protoc` binary),
/// then `prost_build::Config::compile_fds` emits the message structs while
/// [`service_gen::LeafServiceGenerator`] emits the leaf server trait + path/descriptor
/// module per service. Additionally, for reflection discovery, each proto PACKAGE's
/// encoded `FileDescriptorSet` is written to `<OUT_DIR>/<pkg>.fds` and a
/// `FILE_DESCRIPTOR_SET` const + a `REFLECTED_FILE_DESCRIPTOR_SETS` registration row are
/// appended to the generated `<pkg>.rs` (so every compiled proto is reflectable, inert
/// until reflection reads the slice). Included via `leaf_grpc::include_proto!("pkg")`.
///
/// # Errors
/// Returns an [`std::io::Error`] if `protox` parsing, prost-build codegen, or writing the
/// `.fds`/appended block fails.
pub fn compile(protos: &[&str], includes: &[&str]) -> std::io::Result<()> {
    // protox: pure-Rust .proto -> FileDescriptorSet (no protoc binary).
    let fds = protox::compile(protos, includes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    for proto in protos {
        println!("cargo:rerun-if-changed={proto}");
    }

    let out_dir = std::env::var_os("OUT_DIR")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "OUT_DIR not set"))?;
    let out_dir = std::path::PathBuf::from(out_dir);

    // Group BEFORE moving `fds` into compile_fds: one encoded set + one .fds per package.
    let groups = group_fds_by_package(&fds);

    let mut config = prost_build::Config::new();
    config.out_dir(&out_dir);
    config.service_generator(Box::new(service_gen::LeafServiceGenerator));
    // compile_fds drives prost-build off the protox FileDescriptorSet (no protoc). NOTE:
    // compile_fds does NOT honor Config::file_descriptor_set_path (that field is read only
    // on the protoc/load_fds path), so we write the .fds ourselves from `groups` below.
    config.compile_fds(fds)?;

    // Per package: write <pkg>.fds and append the FDS discovery block to <pkg>.rs (the
    // module file prost-build named by the dotted package, e.g. echo.v1.rs).
    for (package, bytes) in &groups {
        let fds_path = out_dir.join(format!("{package}.fds"));
        std::fs::write(&fds_path, bytes)?;

        let rs_path = out_dir.join(format!("{package}.rs"));
        let block = service_gen::render_fds_block(package);
        let mut existing = std::fs::read_to_string(&rs_path).unwrap_or_default();
        existing.push('\n');
        existing.push_str(&block);
        std::fs::write(&rs_path, existing)?;
    }

    Ok(())
}
```

Note on the empty package: `to_file_name_or` uses the default filename root (`default_package_filename`, default `_`), so `<pkg>.rs`/`<pkg>.fds` for the empty key would mismatch. leaf compiles only packaged protos (echo, the reflection protos all carry a package), so the empty-package row is not exercised by leaf-grpc-build's own targets; the grouping helper handles it for correctness but `compile`'s append targets the dotted filename. If a downstream ever compiles an unpackaged proto, that surfaces as a missing-`.rs` write error — acceptable for this stage (documented in the doc comment).

- [ ] **Step 5: Run it — passes.**

```
cargo test -p leaf-grpc-build --lib
```

Expected: `groups_descriptor_files_by_package_each_a_self_contained_set ... ok`, `an_empty_package_groups_under_the_empty_key ... ok`, plus all service-gen tests green.

- [ ] **Step 6: Commit.**

```
git add crates/leaf-grpc-build/Cargo.toml crates/leaf-grpc-build/src/lib.rs
git commit -m "leaf-grpc-build: compile() writes <pkg>.fds + appends the discovery block per package

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 1.4: Build-output proof — the FDS const equals the .fds and decodes

**Files:** `crates/leaf-grpc-build/build.rs`, `crates/leaf-grpc-build/tests/generated_service.rs`, `crates/leaf-grpc/tests/reflected_fds_slice.rs`

`leaf-grpc-build`'s OWN build.rs re-hosts the pipeline (it cannot path-self-dep). Mirror the new FDS write there so `tests/generated_service.rs` (which `include!`s `$OUT_DIR/echo.v1.rs`) sees `FILE_DESCRIPTOR_SET` + the `.fds`.

- [ ] **Step 1: Re-host the FDS write in `crates/leaf-grpc-build/build.rs`.** It currently `#[path]`-loads `service_gen` and inlines protox→prost-build. After `config.compile_fds(fds)?` (it moves `fds`), it needs the groups — so group before the move, then write `.fds` + append the block. Replace the `build.rs` `main` body:

```rust
fn main() -> std::io::Result<()> {
    let protos = ["tests/echo.proto"];
    let includes = ["tests"];

    // protox: pure-Rust .proto -> FileDescriptorSet (no protoc binary).
    let fds = protox::compile(protos, includes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    for proto in protos {
        println!("cargo:rerun-if-changed={proto}");
    }

    let out_dir = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "OUT_DIR not set"))?,
    );

    // Group by package BEFORE moving `fds` into compile_fds (compile_fds does not write
    // the .fds itself) — mirrors the library's `compile`.
    use ::prost::Message;
    let mut by_pkg: std::collections::BTreeMap<String, ::prost_types::FileDescriptorSet> =
        std::collections::BTreeMap::new();
    for f in &fds.file {
        let pkg = f.package.clone().unwrap_or_default();
        by_pkg.entry(pkg).or_default().file.push(f.clone());
    }

    let mut config = prost_build::Config::new();
    config.out_dir(&out_dir);
    config.service_generator(Box::new(LeafServiceGenerator));
    config.compile_fds(fds)?;

    for (package, set) in &by_pkg {
        std::fs::write(out_dir.join(format!("{package}.fds")), set.encode_to_vec())?;
        let rs_path = out_dir.join(format!("{package}.rs"));
        let mut existing = std::fs::read_to_string(&rs_path).unwrap_or_default();
        existing.push('\n');
        existing.push_str(&service_gen::render_fds_block(package));
        std::fs::write(&rs_path, existing)?;
    }

    Ok(())
}
```

(`render_fds_block` is `pub` in `service_gen`, so the `#[path]`-loaded module exposes it. `prost`/`prost-types` are now build-deps from Task 1.3 Step 1.)

- [ ] **Step 2: Add the failing build-output assertions** to `crates/leaf-grpc-build/tests/generated_service.rs`. The `include!` already brings the generated module — now it also brings `FILE_DESCRIPTOR_SET` (re-exported at the file's top level by `render_fds_block`'s `pub use`):

```rust
#[test]
fn the_generated_module_exposes_the_encoded_file_descriptor_set() {
    // The FDS const is the package's encoded FileDescriptorSet, embedded from echo.v1.fds.
    assert!(!FILE_DESCRIPTOR_SET.is_empty(), "the FDS const carries the encoded bytes");
}

#[test]
fn the_fds_const_equals_the_sibling_fds_file_and_decodes_to_the_echo_package() {
    use ::prost::Message;
    // Equals the bytes compile() wrote to OUT_DIR/echo.v1.fds.
    let on_disk = include_bytes!(concat!(env!("OUT_DIR"), "/echo.v1.fds"));
    assert_eq!(FILE_DESCRIPTOR_SET, on_disk, "the const embeds the .fds verbatim");

    let decoded = ::prost_types::FileDescriptorSet::decode(FILE_DESCRIPTOR_SET)
        .expect("the FDS const decodes as a prost FileDescriptorSet");
    let packages: Vec<_> = decoded.file.iter().filter_map(|f| f.package.clone()).collect();
    assert!(
        packages.iter().any(|p| p == "echo.v1"),
        "the decoded FDS names the echo.v1 package, got {packages:?}"
    );
    // The service the leaf trait was generated from is present in the descriptor.
    let services: Vec<_> = decoded
        .file
        .iter()
        .flat_map(|f| f.service.iter().filter_map(|s| s.name.clone()))
        .collect();
    assert!(services.iter().any(|s| s == "Echo"), "the Echo service is in the FDS, got {services:?}");
}
```

Add `prost-types` to `crates/leaf-grpc-build/[dev-dependencies]` (the test names `::prost_types`):

```toml
prost-types = { workspace = true }
```

- [ ] **Step 3: Run it — fails first, then re-run after the build.rs change takes effect.**

```
cargo test -p leaf-grpc-build --test generated_service
```

If the const is missing it fails to compile (`cannot find value FILE_DESCRIPTOR_SET`); a stale OUT_DIR can mask the build.rs change, so force-clean this crate first:

```
cargo clean -p leaf-grpc-build && cargo test -p leaf-grpc-build --test generated_service
```

Expected after the fix: `the_generated_module_exposes_the_encoded_file_descriptor_set ... ok`, `the_fds_const_equals_the_sibling_fds_file_and_decodes_to_the_echo_package ... ok`, plus the four existing trait/path/descriptor tests still green.

- [ ] **Step 4: Un-ignore + run the leaf-grpc slice proof.** If Task 1.1 Step 4 marked `reflected_fds_slice.rs` `#[ignore]`, remove it now. leaf-grpc's build.rs uses package `echo` (its `tests/proto/echo.proto` — verify its `package` line; if it is `echo` the file is `echo.rs`/`echo.fds`, matching `include_proto!("echo")`). Force-clean to pick up the regenerated module:

```
cargo clean -p leaf-grpc && cargo test -p leaf-grpc --test reflected_fds_slice
```

Expected: `the_compiled_proto_contributes_a_row_to_the_discovery_slice ... ok` and `the_collected_fds_decode_and_name_the_proto_package ... ok`. If the package assertion fails on the name, align the test's expected package with `crates/leaf-grpc/tests/proto/echo.proto`'s `package` declaration (read it; do not assume).

- [ ] **Step 5: Full force-clean regression gate** (the existing ~1733-test suite stays green; build-output tests are OUT_DIR-cached, so clean first):

```
cargo clean
cargo test --workspace
```

Expected: `test result: ok.` for every crate; the new tests appear under `leaf-grpc-build` (`generated_service`, lib) and `leaf-grpc` (`reflected_fds_slice`), with the prior total preserved + the additions. Also run the lint/doc gate:

```
cargo clippy --workspace --all-targets -- -D warnings && cargo doc --workspace --no-deps
```

Expected: clippy clean (no warnings on the generated block's idents — the inner `#![allow(dead_code, non_upper_case_globals)]` covers them), `Documenting ...` with no warnings.

- [ ] **Step 6: Commit.**

```
git add crates/leaf-grpc-build/build.rs crates/leaf-grpc-build/Cargo.toml crates/leaf-grpc-build/tests/generated_service.rs crates/leaf-grpc/tests/reflected_fds_slice.rs
git commit -m "leaf-grpc-build: build-output proof of the FDS const + runtime discovery-slice proof

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Stage 2: The reflection index core

The version-agnostic reflection index — plain `leaf-grpc` Rust, NOT a gRPC service. It decodes the collected encoded `FileDescriptorSet` bytes (the `REFLECTED_FILE_DESCRIPTOR_SETS` slice defined in Stage 1) into `prost_types::FileDescriptorProto`s and answers the five reflection queries, returning, for the `file_*` queries, the matched file PLUS the transitive closure of its `dependency` imports (deduped). It keys exclusively on the FDS WIRE symbol strings (`storefront.catalog.Catalog`, `storefront.catalog.GetRequest`, …) — the gRPC fully-qualified identifiers — NEVER on a Rust type name (no-type-name-detection rule). No gRPC service is built here; the two `#[grpc_controller]` adapters that consume this index are Stage 3.

Files:
- Create: `crates/leaf-grpc/src/reflection/mod.rs` (the `reflection` module root + re-export)
- Create: `crates/leaf-grpc/src/reflection/index.rs` (`ReflectionIndex` + all queries + closure)
- Modify: `crates/leaf-grpc/Cargo.toml` (add `prost-types` as a normal dep; `prost-types` dev-dep already implied via the new normal dep)
- Modify: `crates/leaf-grpc/src/lib.rs` (declare `pub mod reflection;`, re-export `ReflectionIndex`, re-export `prost_types`)

---

### Task 2.1: Add `prost-types` + the `reflection` module skeleton

`ReflectionIndex` indexes `prost_types::FileDescriptorProto`s, so `leaf-grpc` needs `prost-types` as a NORMAL (runtime) dependency — it is pure Rust, no hyper/h2, so the backend-free constraint holds. We re-export it as `prost_types` (the same facade trick as the existing `pub use prost;`) so a downstream umbrella app resolves the absolute path through the one `leaf` dep.

Files:
- Modify: `crates/leaf-grpc/Cargo.toml`
- Create: `crates/leaf-grpc/src/reflection/mod.rs`
- Create: `crates/leaf-grpc/src/reflection/index.rs`
- Modify: `crates/leaf-grpc/src/lib.rs`

- [ ] **Step 1: Add `prost-types` to `leaf-grpc`'s normal deps.** Edit `crates/leaf-grpc/Cargo.toml`, in the `[dependencies]` block right after the existing `prost.workspace = true` line:

```toml
# prost — the protobuf MESSAGE codec, confined to ProstCodec (the serde_json analogue).
prost.workspace = true
# prost-types — the well-known descriptor value types (FileDescriptorSet / FileDescriptorProto)
# the reflection INDEX decodes the discovery slice into. Pure Rust (no protoc, no hyper/h2),
# so leaf-grpc stays backend-free; re-exported as `prost_types` for the umbrella facade.
prost-types.workspace = true
```

- [ ] **Step 2: Create the empty `reflection` module + index file with a doc header and an empty `tests` mod** so the crate compiles before any type exists. Write `crates/leaf-grpc/src/reflection/mod.rs`:

```rust
//! gRPC server-reflection support (sub-project C), the version-agnostic core.
//!
//! [`ReflectionIndex`] is PLAIN Rust — NOT a gRPC service. It decodes the
//! [`crate::REFLECTED_FILE_DESCRIPTOR_SETS`] discovery slice (each row an encoded
//! `prost_types::FileDescriptorSet`) into descriptor maps and answers the five
//! reflection queries (`list_services`, `file_by_filename`, `file_containing_symbol`,
//! `file_containing_extension`, `all_extension_numbers_of_type`). The two version
//! `#[grpc_controller]` adapters (`grpc.reflection.v1` / `v1alpha`, Stage 3) drive
//! this one index.
//!
//! It keys on the FDS WIRE symbol strings (the gRPC fully-qualified identifiers,
//! e.g. `storefront.catalog.Catalog`) — NEVER on a Rust type name (the
//! no-type-name-detection rule). The `file_*` queries return the matched file PLUS
//! the transitive closure of its `dependency` imports, deduped (the reflection spec:
//! a client needs the full set to rebuild the type).

mod index;

pub use index::ReflectionIndex;
```

Write `crates/leaf-grpc/src/reflection/index.rs` with just the skeleton (no `ReflectionIndex` yet — Task 2.2 adds it test-first):

```rust
//! The reflection index: decode the FDS discovery slice → descriptor maps → queries.

#[cfg(test)]
mod tests {
    // Hand-built FileDescriptorSet fixtures + query assertions land here (Tasks 2.2–2.7).
}
```

- [ ] **Step 3: Wire the module + re-exports into `lib.rs`.** Edit `crates/leaf-grpc/src/lib.rs`, add `pub mod reflection;` to the module list (after `pub mod mapper;`):

```rust
pub mod mapper;
pub mod reflection;
pub mod status;
```

Add the `ReflectionIndex` re-export to the public re-export block (after the `pub use mapper::…;` line):

```rust
pub use mapper::{map_first, DefaultGrpcStatusMapper, GrpcStatusMapper};
pub use reflection::ReflectionIndex;
```

And re-export `prost_types` beside the existing `pub use prost;` (so generated/downstream code resolves `::leaf_grpc::prost_types::…`):

```rust
#[doc(no_inline)]
pub use prost;

// prost-types — the descriptor value types the reflection index decodes the discovery
// slice into; re-exported (the same umbrella-facade trick as `prost`) so a proto-first
// downstream resolves `::leaf_grpc::prost_types::FileDescriptorProto` through the one dep.
#[doc(no_inline)]
pub use prost_types;
```

- [ ] **Step 4: Run the build — confirm it compiles clean (empty index).**

```
cargo build -p leaf-grpc
```

Expected: `Finished` with no errors (the empty `tests` mod + new deps compile).

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/Cargo.toml crates/leaf-grpc/src/lib.rs crates/leaf-grpc/src/reflection/
git commit -m "leaf-grpc: reflection module skeleton + prost-types dep

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.2: `from_descriptor_sets` decode + `list_services`

The constructor decodes each `&[u8]` row as a `prost_types::FileDescriptorSet` (propagating `prost::DecodeError` on a corrupt row), and indexes every file. `list_services` returns the fully-qualified service names (`<package>.<Service>`, or just `<Service>` when the file has no package). This task introduces the struct, its fields, the decode, the `by_filename` map, the `services` list, and a shared test-fixture helper.

Files:
- Modify: `crates/leaf-grpc/src/reflection/index.rs`

- [ ] **Step 1: Write the failing test** — a hand-built `FileDescriptorSet` with two services across a package, encoded to bytes, fed through `from_descriptor_sets`, asserting `list_services`. Put the fixture builder + test in the `tests` mod of `index.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use prost_types::{
        DescriptorProto, FieldDescriptorProto, FileDescriptorProto, FileDescriptorSet,
        MethodDescriptorProto, ServiceDescriptorProto,
    };

    /// A minimal message descriptor (name only).
    fn message(name: &str) -> DescriptorProto {
        DescriptorProto { name: Some(name.to_string()), ..Default::default() }
    }

    /// A minimal RPC method descriptor (`name`, `input_type`, `output_type`).
    fn method(name: &str, input: &str, output: &str) -> MethodDescriptorProto {
        MethodDescriptorProto {
            name: Some(name.to_string()),
            input_type: Some(input.to_string()),
            output_type: Some(output.to_string()),
            ..Default::default()
        }
    }

    /// A service descriptor with the given methods.
    fn service(name: &str, methods: Vec<MethodDescriptorProto>) -> ServiceDescriptorProto {
        ServiceDescriptorProto {
            name: Some(name.to_string()),
            method: methods,
            ..Default::default()
        }
    }

    /// The `storefront.catalog` file: a Catalog service over Get/List + its messages.
    fn catalog_file() -> FileDescriptorProto {
        FileDescriptorProto {
            name: Some("storefront/catalog.proto".to_string()),
            package: Some("storefront.catalog".to_string()),
            message_type: vec![
                message("GetRequest"),
                message("GetResponse"),
                message("ListRequest"),
                message("ListResponse"),
            ],
            service: vec![
                service(
                    "Catalog",
                    vec![
                        method(
                            "Get",
                            ".storefront.catalog.GetRequest",
                            ".storefront.catalog.GetResponse",
                        ),
                        method(
                            "List",
                            ".storefront.catalog.ListRequest",
                            ".storefront.catalog.ListResponse",
                        ),
                    ],
                ),
                service("Admin", vec![method("Reindex", ".storefront.catalog.GetRequest", ".storefront.catalog.GetResponse")]),
            ],
            ..Default::default()
        }
    }

    /// Encode one FileDescriptorProto into a one-file FileDescriptorSet's bytes.
    fn encode_set(files: Vec<FileDescriptorProto>) -> Vec<u8> {
        FileDescriptorSet { file: files }.encode_to_vec()
    }

    #[test]
    fn list_services_returns_fully_qualified_names() {
        let bytes = encode_set(vec![catalog_file()]);
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();

        let mut services = index.list_services();
        services.sort();
        assert_eq!(
            services,
            vec![
                "storefront.catalog.Admin".to_string(),
                "storefront.catalog.Catalog".to_string(),
            ]
        );
    }

    #[test]
    fn from_descriptor_sets_propagates_a_decode_error_on_corrupt_bytes() {
        // A truncated/garbage protobuf the FileDescriptorSet decoder rejects.
        let garbage: &[u8] = &[0xff, 0xff, 0xff, 0xff];
        let err = ReflectionIndex::from_descriptor_sets(&[garbage]);
        assert!(err.is_err(), "corrupt FDS bytes must surface a DecodeError");
    }
}
```

- [ ] **Step 2: Run it — confirm it fails to compile (`ReflectionIndex` undefined).**

```
cargo test -p leaf-grpc reflection::index 2>&1 | head -20
```

Expected: `error[E0433]` / `cannot find … ReflectionIndex` (the type does not exist yet).

- [ ] **Step 3: Implement the struct, the FQN helper, the decode, and `list_services`.** Add ABOVE the `tests` mod in `index.rs`:

```rust
use std::collections::HashMap;

use prost::Message;
use prost_types::{FileDescriptorProto, FileDescriptorSet};

/// An in-memory index over every served proto's descriptors, answering the gRPC
/// server-reflection queries.
///
/// Built once (from [`crate::REFLECTED_FILE_DESCRIPTOR_SETS`]) via
/// [`from_descriptor_sets`](ReflectionIndex::from_descriptor_sets); keyed on the FDS
/// WIRE symbol strings (the gRPC fully-qualified identifiers, e.g.
/// `storefront.catalog.Catalog`), NEVER on a Rust type name.
pub struct ReflectionIndex {
    /// `file name` (e.g. `storefront/catalog.proto`) → the decoded file.
    by_filename: HashMap<String, FileDescriptorProto>,
    /// fully-qualified symbol (message / enum / service / method / enum-value) → its
    /// defining `file name`.
    by_symbol: HashMap<String, String>,
    /// `(extendee FQN, field number)` → the defining `file name` of the extension.
    by_extension: HashMap<(String, i32), String>,
    /// `extendee FQN` → every extension field `number` declared against it.
    extension_numbers: HashMap<String, Vec<i32>>,
    /// every fully-qualified service name (for `list_services`).
    services: Vec<String>,
}

/// Join a package and a local name into a gRPC FQN (`pkg.Name`), or just `Name` when
/// the file declares no package. NEVER derived from a Rust type — `local` is the proto
/// `name` field straight off the descriptor.
fn fqn(package: &str, local: &str) -> String {
    if package.is_empty() {
        local.to_string()
    } else {
        format!("{package}.{local}")
    }
}

impl ReflectionIndex {
    /// Decode each `&[u8]` row as an encoded `FileDescriptorSet` and index every file,
    /// symbol, service and extension it carries.
    ///
    /// Returns a [`prost::DecodeError`] if any row is not a valid `FileDescriptorSet`.
    pub fn from_descriptor_sets(sets: &[&[u8]]) -> Result<ReflectionIndex, prost::DecodeError> {
        let mut by_filename = HashMap::new();
        let mut by_symbol = HashMap::new();
        let mut by_extension = HashMap::new();
        let mut extension_numbers: HashMap<String, Vec<i32>> = HashMap::new();
        let mut services = Vec::new();

        for bytes in sets {
            let set = FileDescriptorSet::decode(*bytes)?;
            for file in set.file {
                let file_name = file.name.clone().unwrap_or_default();
                let package = file.package.clone().unwrap_or_default();

                // Services (and their methods) — the FQNs for list_services + by_symbol.
                for svc in &file.service {
                    let svc_name = svc.name.clone().unwrap_or_default();
                    let svc_fqn = fqn(&package, &svc_name);
                    services.push(svc_fqn.clone());
                    by_symbol.insert(svc_fqn.clone(), file_name.clone());
                    for m in &svc.method {
                        let m_name = m.name.clone().unwrap_or_default();
                        by_symbol.insert(fqn(&svc_fqn, &m_name), file_name.clone());
                    }
                }

                // Top-level messages + enums (recursing into nested types).
                for msg in &file.message_type {
                    index_message(&package, msg, &file_name, &mut by_symbol);
                }
                for en in &file.enum_type {
                    let en_name = en.name.clone().unwrap_or_default();
                    by_symbol.insert(fqn(&package, &en_name), file_name.clone());
                }

                // File-level extensions: (extendee, number) → file, and extendee → [numbers].
                for ext in &file.extension {
                    if let (Some(extendee), Some(number)) = (&ext.extendee, ext.number) {
                        let key = normalize_symbol(extendee);
                        by_extension.insert((key.clone(), number), file_name.clone());
                        extension_numbers.entry(key).or_default().push(number);
                    }
                }

                by_filename.insert(file_name, file);
            }
        }

        Ok(ReflectionIndex {
            by_filename,
            by_symbol,
            by_extension,
            extension_numbers,
            services,
        })
    }

    /// Every fully-qualified service name across all indexed files.
    pub fn list_services(&self) -> Vec<String> {
        self.services.clone()
    }
}

/// Strip a leading `.` from a wire type reference. `input_type`/`output_type`/`extendee`
/// in an FDS are often written fully-qualified WITH a leading dot (`.pkg.Type`); the
/// reflection wire symbols are the dot-LESS form (`pkg.Type`), so we normalize both ends.
fn normalize_symbol(s: &str) -> String {
    s.strip_prefix('.').unwrap_or(s).to_string()
}

/// Index a message FQN and recurse into its nested messages + enums (each a symbol in
/// its own right, scoped under the parent: `pkg.Outer.Inner`).
fn index_message(
    scope: &str,
    msg: &prost_types::DescriptorProto,
    file_name: &str,
    by_symbol: &mut HashMap<String, String>,
) {
    let name = msg.name.clone().unwrap_or_default();
    let msg_fqn = fqn(scope, &name);
    by_symbol.insert(msg_fqn.clone(), file_name.to_string());
    for nested in &msg.nested_type {
        index_message(&msg_fqn, nested, file_name, by_symbol);
    }
    for en in &msg.enum_type {
        let en_name = en.name.clone().unwrap_or_default();
        by_symbol.insert(fqn(&msg_fqn, &en_name), file_name.to_string());
    }
}
```

- [ ] **Step 4: Run the tests — confirm they pass.**

```
cargo test -p leaf-grpc reflection::index 2>&1 | tail -15
```

Expected: `test reflection::index::tests::list_services_returns_fully_qualified_names ... ok` and `test reflection::index::tests::from_descriptor_sets_propagates_a_decode_error_on_corrupt_bytes ... ok`.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/reflection/index.rs
git commit -m "leaf-grpc: ReflectionIndex decode + list_services

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.3: The transitive dependency-closure helper

`file_by_filename` and `file_containing_symbol` must return the matched file PLUS every file it transitively imports (its `dependency` list, recursively), deduped, in a stable order (the matched file first, then its imports). This task adds the private `closure_for` helper test-first via `file_by_filename` (the simplest reader), with a three-file diamond import to prove transitivity + dedup.

Files:
- Modify: `crates/leaf-grpc/src/reflection/index.rs`

- [ ] **Step 1: Write the failing test.** Add to the `tests` mod (the diamond: `app.proto` imports `a.proto` AND `b.proto`; both import `common.proto` — `common` must appear EXACTLY once):

```rust
    /// `app.proto` → depends on a.proto + b.proto; both → depend on common.proto.
    fn diamond_files() -> Vec<FileDescriptorProto> {
        let common = FileDescriptorProto {
            name: Some("common.proto".to_string()),
            package: Some("common".to_string()),
            message_type: vec![message("Shared")],
            ..Default::default()
        };
        let a = FileDescriptorProto {
            name: Some("a.proto".to_string()),
            package: Some("a".to_string()),
            dependency: vec!["common.proto".to_string()],
            message_type: vec![message("A")],
            ..Default::default()
        };
        let b = FileDescriptorProto {
            name: Some("b.proto".to_string()),
            package: Some("b".to_string()),
            dependency: vec!["common.proto".to_string()],
            message_type: vec![message("B")],
            ..Default::default()
        };
        let app = FileDescriptorProto {
            name: Some("app.proto".to_string()),
            package: Some("app".to_string()),
            dependency: vec!["a.proto".to_string(), "b.proto".to_string()],
            message_type: vec![message("App")],
            ..Default::default()
        };
        vec![common, a, b, app]
    }

    fn names_of(files: &[FileDescriptorProto]) -> Vec<String> {
        files.iter().map(|f| f.name.clone().unwrap_or_default()).collect()
    }

    #[test]
    fn file_by_filename_returns_the_file_first_then_its_transitive_closure_deduped() {
        let bytes = encode_set(diamond_files());
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();

        let files = index.file_by_filename("app.proto").expect("app.proto is indexed");
        let names = names_of(&files);

        // The matched file is first.
        assert_eq!(names[0], "app.proto");
        // The full closure is present, deduped (common.proto exactly once).
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                "a.proto".to_string(),
                "app.proto".to_string(),
                "b.proto".to_string(),
                "common.proto".to_string(),
            ]
        );
        assert_eq!(names.iter().filter(|n| *n == "common.proto").count(), 1);
    }

    #[test]
    fn file_by_filename_returns_none_for_an_unknown_file() {
        let bytes = encode_set(diamond_files());
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();
        assert!(index.file_by_filename("nope.proto").is_none());
    }
```

- [ ] **Step 2: Run it — confirm it fails (`file_by_filename` undefined).**

```
cargo test -p leaf-grpc reflection::index 2>&1 | head -20
```

Expected: `error[E0599]: no method named file_by_filename …`.

- [ ] **Step 3: Implement `file_by_filename` + the private `closure_for` helper.** Add inside `impl ReflectionIndex`:

```rust
    /// The file with this `name` PLUS the transitive closure of its `dependency`
    /// imports (deduped, the matched file first), or `None` if no such file is indexed.
    pub fn file_by_filename(&self, name: &str) -> Option<Vec<FileDescriptorProto>> {
        if !self.by_filename.contains_key(name) {
            return None;
        }
        Some(self.closure_for(name))
    }
```

And add this private helper to the `impl ReflectionIndex` block (BFS over `dependency`, deduping by file name, the root first):

```rust
    /// The transitive `dependency` closure of `root` — `root`'s file followed by every
    /// file it imports, recursively, each appearing exactly once. A `dependency` naming
    /// an un-indexed file is skipped (a partial set still answers what it can).
    fn closure_for(&self, root: &str) -> Vec<FileDescriptorProto> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(root.to_string());
        seen.insert(root.to_string());

        while let Some(name) = queue.pop_front() {
            let Some(file) = self.by_filename.get(&name) else {
                continue;
            };
            out.push(file.clone());
            for dep in &file.dependency {
                if seen.insert(dep.clone()) {
                    queue.push_back(dep.clone());
                }
            }
        }
        out
    }
```

- [ ] **Step 4: Run the tests — confirm they pass.**

```
cargo test -p leaf-grpc reflection::index 2>&1 | tail -15
```

Expected: `file_by_filename_returns_the_file_first_then_its_transitive_closure_deduped ... ok` and `file_by_filename_returns_none_for_an_unknown_file ... ok` (plus the Task 2.2 tests still green).

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/reflection/index.rs
git commit -m "leaf-grpc: file_by_filename + transitive dependency closure

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.4: `file_containing_symbol`

Resolve a fully-qualified WIRE symbol (a service, method, message, nested message, enum) to its defining file, then return that file + its closure. Reuses `by_symbol` (built in Task 2.2) and `closure_for` (Task 2.3). Normalizes a leading dot so `.pkg.Type` and `pkg.Type` both resolve.

Files:
- Modify: `crates/leaf-grpc/src/reflection/index.rs`

- [ ] **Step 1: Write the failing test** (using the `catalog_file` + `diamond_files` fixtures): a message symbol, a service symbol, a method symbol, a nested-resolution dot variant, and an unknown symbol → `None`. Add to the `tests` mod:

```rust
    #[test]
    fn file_containing_symbol_resolves_messages_services_and_methods() {
        let bytes = encode_set(vec![catalog_file()]);
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();

        // A message symbol → the catalog file.
        let by_msg = index
            .file_containing_symbol("storefront.catalog.GetRequest")
            .expect("message symbol resolves");
        assert_eq!(by_msg[0].name.as_deref(), Some("storefront/catalog.proto"));

        // A service symbol resolves.
        assert!(index.file_containing_symbol("storefront.catalog.Catalog").is_some());
        // A method symbol resolves (service.method FQN).
        assert!(index.file_containing_symbol("storefront.catalog.Catalog.Get").is_some());

        // A leading-dot variant resolves the same way.
        assert!(index.file_containing_symbol(".storefront.catalog.GetResponse").is_some());
    }

    #[test]
    fn file_containing_symbol_returns_the_defining_file_plus_closure() {
        // Put the catalog message symbols in a file that imports common.proto.
        let mut catalog = catalog_file();
        catalog.dependency = vec!["common.proto".to_string()];
        let common = FileDescriptorProto {
            name: Some("common.proto".to_string()),
            package: Some("common".to_string()),
            message_type: vec![message("Shared")],
            ..Default::default()
        };
        let bytes = encode_set(vec![common, catalog]);
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();

        let files = index
            .file_containing_symbol("storefront.catalog.Catalog")
            .expect("service symbol resolves");
        let mut names = names_of(&files);
        assert_eq!(names[0], "storefront/catalog.proto");
        names.sort();
        assert_eq!(
            names,
            vec!["common.proto".to_string(), "storefront/catalog.proto".to_string()]
        );
    }

    #[test]
    fn file_containing_symbol_returns_none_for_an_unknown_symbol() {
        let bytes = encode_set(vec![catalog_file()]);
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();
        assert!(index.file_containing_symbol("storefront.catalog.Nope").is_none());
    }
```

- [ ] **Step 2: Run it — confirm it fails (`file_containing_symbol` undefined).**

```
cargo test -p leaf-grpc reflection::index 2>&1 | head -20
```

Expected: `error[E0599]: no method named file_containing_symbol …`.

- [ ] **Step 3: Implement `file_containing_symbol`.** Add inside `impl ReflectionIndex`:

```rust
    /// The file DEFINING this fully-qualified WIRE symbol (a service, method, message,
    /// nested message, or enum — e.g. `storefront.catalog.Catalog`) PLUS its transitive
    /// dependency closure, or `None` if the symbol is unknown. A leading `.` is tolerated.
    pub fn file_containing_symbol(&self, symbol: &str) -> Option<Vec<FileDescriptorProto>> {
        let key = normalize_symbol(symbol);
        let file_name = self.by_symbol.get(&key)?;
        Some(self.closure_for(file_name))
    }
```

- [ ] **Step 4: Run the tests — confirm they pass.**

```
cargo test -p leaf-grpc reflection::index 2>&1 | tail -15
```

Expected: the three `file_containing_symbol_*` tests `ok`, plus all prior tests still green.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/reflection/index.rs
git commit -m "leaf-grpc: file_containing_symbol + closure

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.5: `file_containing_extension`

Resolve `(extendee FQN, field number)` to the file declaring that extension, then return that file + its closure. Uses `by_extension` (built in Task 2.2), normalizing the extendee's leading dot.

Files:
- Modify: `crates/leaf-grpc/src/reflection/index.rs`

- [ ] **Step 1: Write the failing test** — a file declaring two extensions on `pkg.Base` (numbers 100 and 101), asserting the right (extendee, number) resolves and a wrong number → `None`. Add to the `tests` mod:

```rust
    /// A file declaring two file-level extensions on `.pkg.Base` (numbers 100, 101).
    fn extensions_file() -> FileDescriptorProto {
        let ext = |name: &str, number: i32| FieldDescriptorProto {
            name: Some(name.to_string()),
            number: Some(number),
            extendee: Some(".pkg.Base".to_string()),
            ..Default::default()
        };
        FileDescriptorProto {
            name: Some("ext.proto".to_string()),
            package: Some("pkg".to_string()),
            message_type: vec![message("Base")],
            extension: vec![ext("first", 100), ext("second", 101)],
            ..Default::default()
        }
    }

    #[test]
    fn file_containing_extension_resolves_extendee_and_number() {
        let bytes = encode_set(vec![extensions_file()]);
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();

        let files = index
            .file_containing_extension("pkg.Base", 100)
            .expect("extension 100 resolves");
        assert_eq!(files[0].name.as_deref(), Some("ext.proto"));

        // A leading-dot extendee resolves the same.
        assert!(index.file_containing_extension(".pkg.Base", 101).is_some());
    }

    #[test]
    fn file_containing_extension_returns_none_for_unknown_number() {
        let bytes = encode_set(vec![extensions_file()]);
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();
        assert!(index.file_containing_extension("pkg.Base", 999).is_none());
    }
```

- [ ] **Step 2: Run it — confirm it fails (`file_containing_extension` undefined).**

```
cargo test -p leaf-grpc reflection::index 2>&1 | head -20
```

Expected: `error[E0599]: no method named file_containing_extension …`.

- [ ] **Step 3: Implement `file_containing_extension`.** Add inside `impl ReflectionIndex`:

```rust
    /// The file declaring the extension `number` on `extendee` (a fully-qualified WIRE
    /// type name, leading `.` tolerated) PLUS its transitive dependency closure, or
    /// `None` if no such extension is indexed.
    pub fn file_containing_extension(
        &self,
        extendee: &str,
        number: i32,
    ) -> Option<Vec<FileDescriptorProto>> {
        let key = (normalize_symbol(extendee), number);
        let file_name = self.by_extension.get(&key)?;
        Some(self.closure_for(file_name))
    }
```

- [ ] **Step 4: Run the tests — confirm they pass.**

```
cargo test -p leaf-grpc reflection::index 2>&1 | tail -15
```

Expected: both `file_containing_extension_*` tests `ok`, all prior green.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/reflection/index.rs
git commit -m "leaf-grpc: file_containing_extension + closure

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.6: `all_extension_numbers_of_type`

Return every extension `number` declared against a type, deduped + sorted, or `None` if the type has no indexed extensions. Uses `extension_numbers` (built in Task 2.2).

Files:
- Modify: `crates/leaf-grpc/src/reflection/index.rs`

- [ ] **Step 1: Write the failing test** (reusing `extensions_file`): the two numbers come back sorted; an unknown type → `None`. Add to the `tests` mod:

```rust
    #[test]
    fn all_extension_numbers_of_type_returns_sorted_deduped_numbers() {
        let bytes = encode_set(vec![extensions_file()]);
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();

        let numbers = index
            .all_extension_numbers_of_type("pkg.Base")
            .expect("Base has extensions");
        assert_eq!(numbers, vec![100, 101]);

        // Leading-dot variant resolves the same set.
        assert_eq!(index.all_extension_numbers_of_type(".pkg.Base"), Some(vec![100, 101]));
    }

    #[test]
    fn all_extension_numbers_of_type_returns_none_for_a_type_without_extensions() {
        let bytes = encode_set(vec![extensions_file()]);
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();
        assert!(index.all_extension_numbers_of_type("pkg.NotExtended").is_none());
    }
```

- [ ] **Step 2: Run it — confirm it fails (`all_extension_numbers_of_type` undefined).**

```
cargo test -p leaf-grpc reflection::index 2>&1 | head -20
```

Expected: `error[E0599]: no method named all_extension_numbers_of_type …`.

- [ ] **Step 3: Implement `all_extension_numbers_of_type`.** Add inside `impl ReflectionIndex`:

```rust
    /// Every extension `number` declared against `type_name` (a fully-qualified WIRE type,
    /// leading `.` tolerated), sorted ascending and deduped, or `None` if the type has no
    /// indexed extensions.
    pub fn all_extension_numbers_of_type(&self, type_name: &str) -> Option<Vec<i32>> {
        let key = normalize_symbol(type_name);
        let mut numbers = self.extension_numbers.get(&key)?.clone();
        numbers.sort_unstable();
        numbers.dedup();
        Some(numbers)
    }
```

- [ ] **Step 4: Run the tests — confirm they pass.**

```
cargo test -p leaf-grpc reflection::index 2>&1 | tail -15
```

Expected: both `all_extension_numbers_of_type_*` tests `ok`, all prior green.

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/reflection/index.rs
git commit -m "leaf-grpc: all_extension_numbers_of_type

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2.7: Multi-set merge + the full force-clean gate

The discovery slice carries one row PER compiled proto, so `from_descriptor_sets` is fed MANY sets — prove it merges them (a symbol defined in set A and a service in set B both resolve through one index, and a cross-set dependency closure spans rows). Then run the full force-clean gate to confirm the whole ~1733-test suite + clippy + doc stay green with the new module.

Files:
- Modify: `crates/leaf-grpc/src/reflection/index.rs`

- [ ] **Step 1: Write the failing test** — two SEPARATE encoded sets passed together; an `app.proto` in set B depends on `common.proto` in set A, and the closure must span both. Add to the `tests` mod:

```rust
    #[test]
    fn from_descriptor_sets_merges_multiple_sets_and_closes_across_them() {
        // Set A: common.proto (a dependency).
        let common = FileDescriptorProto {
            name: Some("common.proto".to_string()),
            package: Some("common".to_string()),
            message_type: vec![message("Shared")],
            ..Default::default()
        };
        let set_a = encode_set(vec![common]);

        // Set B: app.proto importing common.proto from the OTHER set, with a service.
        let app = FileDescriptorProto {
            name: Some("app.proto".to_string()),
            package: Some("app".to_string()),
            dependency: vec!["common.proto".to_string()],
            service: vec![service("App", vec![method("Do", ".common.Shared", ".common.Shared")])],
            ..Default::default()
        };
        let set_b = encode_set(vec![app]);

        let index = ReflectionIndex::from_descriptor_sets(&[&set_a, &set_b]).unwrap();

        // The service from set B is listed.
        assert_eq!(index.list_services(), vec!["app.App".to_string()]);

        // The symbol from set A resolves.
        assert!(index.file_containing_symbol("common.Shared").is_some());

        // The closure of app.proto (set B) reaches common.proto (set A).
        let files = index.file_containing_symbol("app.App").unwrap();
        let mut names = names_of(&files);
        names.sort();
        assert_eq!(names, vec!["app.proto".to_string(), "common.proto".to_string()]);
    }
```

- [ ] **Step 2: Run it — confirm it passes** (the merge falls out of the existing loop; this test guards the contract — it should be green immediately, proving the per-row decode + shared maps already merge).

```
cargo test -p leaf-grpc reflection::index 2>&1 | tail -15
```

Expected: `from_descriptor_sets_merges_multiple_sets_and_closes_across_them ... ok`. (If it fails, the loop is not merging — fix `from_descriptor_sets` to accumulate across ALL rows before constructing; do not reset the maps per row.)

- [ ] **Step 3: Run the full `leaf-grpc` test suite — confirm no regression** in the crate (the index module + the existing dispatch/framing/controller tests).

```
cargo test -p leaf-grpc 2>&1 | tail -20
```

Expected: `test result: ok.` for every binary (unit + `tests/*.rs`), zero failures.

- [ ] **Step 4: Force-clean workspace gate** — test + clippy + doc, fresh (cached runs re-emit no warnings, so clean first per the project's verify-with-fresh-builds rule).

```
cargo clean
cargo test --workspace 2>&1 | tail -25
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15
cargo doc --workspace --no-deps 2>&1 | tail -15
```

Expected: `cargo test` ends with all `test result: ok.` and the suite count at or above the ~1733 baseline (the new reflection-index tests ADD to it); `clippy` finishes with no warnings (no `-D warnings` failure); `doc` finishes with no warnings (the new `pub` items + module are documented under `#![warn(missing_docs)]`).

- [ ] **Step 5: Commit.**

```
git add crates/leaf-grpc/src/reflection/index.rs
git commit -m "leaf-grpc: multi-set merge test + green force-clean gate

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Stage 3: Condition propagation + the reflection services

Two deliverables, built strictly on Stage 1 (the `REFLECTED_FILE_DESCRIPTOR_SETS` slice + the per-proto FDS const/registration row from `leaf-grpc-build`) and Stage 2 (`ReflectionIndex`):

- **(a)** a general `#[grpc_controller]` **condition-propagation** codegen addition — copy the controller's `#[conditional]`/`#[profile]` guard attributes (stacked on the `impl` block) onto each generated `#[doc(hidden)]` `GrpcRoute` bean, so a conditioned controller gates struct **and** routes as one unit (a token test + a wiring test).
- **(b)** ship the upstream `reflection.proto` for `grpc.reflection.v1` **and** `grpc.reflection.v1alpha` in `leaf-grpc`, compile them through `leaf-grpc-build` to leaf server traits, and add the two thin `#[grpc_controller]` beans `ReflectionV1` / `ReflectionV1alpha` (each `#[conditional(on_property = "leaf.grpc.reflection.enabled")]`) that field-inject the shared `ReflectionIndex` and map each inbound `ServerReflectionRequest` in the bidi stream to one `ServerReflectionResponse` (not-found -> `ErrorResponse { error_code: 5 }`).

Hard constraints carried through: backend-free `leaf-grpc` (no hyper/h2 names — only `leaf-web-hyper` does); NO type-name detection (the index keys on the FDS wire symbol strings the index already holds, the propagation keys on the controller's guard attrs, never a Rust type's spelling); dogfood (reflection = `#[grpc_controller]` beans + the Stage-1 slice; no hand-rolled `GrpcRoute`/`GrpcHandler`/`Provider`); `tonic`/`tonic-build` stay dev/build-only; keep the existing ~1733-test suite green (force-clean gate).

### Files

- **Modify** `crates/leaf-codegen/src/grpc_controller.rs` — read+strip the impl's `#[conditional]`/`#[profile]` attrs, emit one `emit_guard(route_struct_ident, &expr)` per route bean.
- **Modify** `crates/leaf-macros/src/lib.rs` — the `#[grpc_controller]` impl arm strips the propagated guard attrs from the re-emitted `impl` before `async_impl::expand`.
- **Create** `crates/leaf-codegen/tests/grpc_condition_propagation.rs` — the codegen token test (a conditioned `impl` emits a guard per route, keyed on the route struct).
- **Create** `crates/leaf-grpc/proto/reflection_v1.proto` — the upstream `grpc.reflection.v1` proto.
- **Create** `crates/leaf-grpc/proto/reflection_v1alpha.proto` — the upstream `grpc.reflection.v1alpha` proto.
- **Modify** `crates/leaf-grpc/build.rs` — compile both reflection protos via `leaf_grpc_build::compile`.
- **Create** `crates/leaf-grpc/src/reflection/mod.rs` — the reflection module root: `include_proto!` both packages + re-export `ReflectionV1`/`ReflectionV1alpha`.
- **Create** `crates/leaf-grpc/src/reflection/service.rs` — the two `#[grpc_controller]` beans + the shared request->response adapter over `ReflectionIndex`.
- **Modify** `crates/leaf-grpc/src/lib.rs` — `pub mod reflection;` + re-exports.
- **Create** `crates/leaf-grpc/tests/reflection_controllers.rs` — the wiring test (a conditioned controller gates struct+routes as a unit) + the index-adapter unit (not-found -> `ErrorResponse{NOT_FOUND}`).

---

### Task 3.1: Condition-propagation codegen — copy the impl's guard attrs onto each route bean

The `#[grpc_controller] impl` macro emits a `#[doc(hidden)]` `GrpcRoute` bean per RPC, each field-injecting `Ref<Controller>`. When the controller is `#[conditional]`-gated, its struct bean de-registers — but the route beans would still register and then fail to resolve `Ref<Controller>`. So the guard must be PROPAGATED onto every route bean. The mechanism reuses the frozen guard machinery: `conditional::emit_guard(ident, expr)` emits a `GUARD_PAIRINGS` row keyed on `ContractId::of(module_path!() ++ "::" ++ ident)`, which is EXACTLY the route bean's contract (the route bean is emitted with `module_qualified = true` and `contract_path = route_struct_ident`). The guard expression is read from the `#[conditional(...)]`/`#[profile(...)]` attrs the user stacks on the `impl` block (the standalone `#[conditional]` macro only accepts a struct, so on the `impl` they ride as inert outer attrs the `#[grpc_controller]` impl macro consumes — and strips before re-emit). This is general: it also lets a conditional HTTP controller gate correctly.

**Files**: `crates/leaf-codegen/src/grpc_controller.rs`, `crates/leaf-codegen/tests/grpc_condition_propagation.rs`

- [ ] **Step 1: Write the failing unit test for `propagated_guards`.** Add to the `tests` module in `crates/leaf-codegen/src/grpc_controller.rs` (it already imports `super::*` + `quote`). The test asserts that an `impl` carrying a `#[conditional(on_property("leaf.grpc.reflection.enabled"))]` outer attr produces, per route, a `GUARD_PAIRINGS` row + a public guard const named off the route struct.

```rust
    #[test]
    fn a_conditioned_grpc_impl_propagates_the_guard_onto_every_route_bean() {
        // The condition-propagation addition: a `#[conditional(...)]` attr stacked on the
        // `#[grpc_controller] impl` block is read off the impl's OWN attrs and a guard is
        // emitted per generated route bean, KEYED on the route struct ident (the route
        // bean's contract) — so a conditioned controller gates struct + routes as one unit.
        let item: ItemImpl = syn::parse_str(
            r#"#[conditional(on_property("leaf.grpc.reflection.enabled"))]
               impl reflection_v1::ServerReflection for ReflectionV1 {
                   async fn server_reflection_info(&self, reqs: Streaming<Req>)
                       -> Result<Streaming<Resp>, Status> { todo!() }
               }"#,
        )
        .expect("a valid conditioned impl");
        let ts = expand_grpc_controller_impl(&item).expect("emits");
        syn::parse2::<syn::File>(ts.clone()).expect("the emitted artifact is valid Rust items");
        let s = flat(&ts);
        // The route bean for `server_reflection_info` carries a guard pairing row, keyed by
        // the route struct ident (NOT the controller name) — so it pairs with the route
        // bean's own contract.
        assert!(
            s.contains("#[::leaf_core::linkme::distributed_slice(::leaf_core::GUARD_PAIRINGS)]"),
            "a conditioned grpc impl emits a GUARD_PAIRINGS row per route: {s}"
        );
        assert!(
            s.contains("pubconst__leaf_guard___LeafGrpcRoute_ReflectionV1_server_reflection_info:::leaf_core::CondExpr"),
            "the guard const is named off the ROUTE STRUCT ident (the route bean's contract): {s}"
        );
        // The guard tree is the controller's on_property condition (the OnProperty leaf id).
        assert!(
            s.contains(r#"::leaf_core::contract_hash("leaf::condition::OnProperty")"#),
            "the propagated guard carries the controller's on_property condition: {s}"
        );
        assert!(
            s.contains(r#"::leaf_core::Attr::Str("name","leaf.grpc.reflection.enabled")"#),
            "the propagated guard carries the property name: {s}"
        );
    }

    #[test]
    fn an_unconditioned_grpc_impl_emits_no_guard_rows() {
        // No `#[conditional]`/`#[profile]` on the impl => no guard propagation (the existing
        // route beans register unconditionally, exactly as before this addition).
        let item = impl_item(
            r#"impl catalog::Catalog for CatalogController {
                async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
            }"#,
        );
        let s = flat(&expand_grpc_controller_impl(&item).expect("emits"));
        assert!(
            !s.contains("::leaf_core::GUARD_PAIRINGS"),
            "an unconditioned controller emits no propagated guard rows: {s}"
        );
    }

    #[test]
    fn a_profile_gated_grpc_impl_propagates_the_profile_guard() {
        // `#[profile("prod")]` on the impl propagates the ON_PROFILE leaf onto each route.
        let item: ItemImpl = syn::parse_str(
            r#"#[profile("prod")]
               impl catalog::Catalog for CatalogController {
                   async fn get(&self, req: ProductReq) -> Result<Product, Status> { todo!() }
               }"#,
        )
        .expect("a valid profile-gated impl");
        let s = flat(&expand_grpc_controller_impl(&item).expect("emits"));
        assert!(
            s.contains(r#"::leaf_core::contract_hash("leaf::condition::OnProfile")"#),
            "the propagated guard carries the profile condition: {s}"
        );
        assert!(
            s.contains(r#"::leaf_core::Attr::Str("expr","prod")"#),
            "the rendered profile expr rides the guard: {s}"
        );
    }
```

- [ ] **Step 2: Run the test — it fails to compile / fails.** `expand_grpc_controller_impl` does not yet read the impl's attrs nor emit guards.

```
cargo test -p leaf-codegen --lib grpc_controller::tests 2>&1 | tail -20
```
Expected: `a_conditioned_grpc_impl_propagates_the_guard_onto_every_route_bean ... FAILED` with `a conditioned grpc impl emits a GUARD_PAIRINGS row per route` (the `GUARD_PAIRINGS` substring is absent).

- [ ] **Step 3: Implement guard propagation in `expand_grpc_controller_impl`.** Add a helper that reads the impl's `#[conditional]`/`#[profile]` outer attrs into a `CondExpr`, and thread the route struct ident out of `emit_grpc_route_bean` so a guard can be emitted per route. Edit `crates/leaf-codegen/src/grpc_controller.rs`.

First, extend the imports at the top of the file:
```rust
use crate::conditional::{self, CondExpr};
use crate::descriptor::{self, BeanInput, Dependency, EmitError, FieldShape, Scope, ServiceView, Slice};
use crate::stereotype::Stereotype;
```

Add the attr-reading helper (after `service_trait_of`):
```rust
/// Read the controller's PROPAGATED guard from the `#[grpc_controller] impl` block's OWN
/// outer attributes: a `#[conditional(...)]` or `#[profile("...")]` the user stacked on the
/// impl (mirroring the struct's gate). Returns the combined [`CondExpr`] to copy onto every
/// generated `GrpcRoute` bean, or `None` when the impl carries no guard.
///
/// Multiple guard attrs AND together (Spring's multiple-@Conditional stacking) — a
/// `#[conditional(..)]` plus a `#[profile(..)]` on one impl gate on BOTH. The attrs are
/// matched on the path's LAST segment (so `::leaf_macros::conditional` matches too) and
/// STRIPPED from the re-emitted impl by the macro layer (a standalone `#[conditional]` only
/// accepts a struct, so on an impl it is inert until this reader consumes it). Never inspects
/// a type name — the guard is the user's explicit condition DSL.
///
/// # Errors
/// [`EmitError`] on a malformed `#[conditional]`/`#[profile]` body (propagated from the
/// condition codegen).
fn propagated_guard(item: &ItemImpl) -> Result<Option<CondExpr>, EmitError> {
    let mut nodes = Vec::new();
    for attr in &item.attrs {
        let name = attr
            .path()
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        match name.as_str() {
            "conditional" => {
                let tokens = attr.parse_args::<TokenStream>().unwrap_or_default();
                nodes.push(conditional::parse_conditional(tokens)?);
            }
            "profile" => {
                let tokens = attr.parse_args::<TokenStream>().map_err(|e| EmitError {
                    message: format!("malformed #[profile] on a #[grpc_controller] impl: {e}"),
                })?;
                let profile = conditional::parse_profile_attr(tokens)?;
                nodes.push(conditional::profile_to_cond(&profile));
            }
            _ => {}
        }
    }
    Ok(match nodes.len() {
        0 => None,
        1 => Some(nodes.into_iter().next().unwrap()),
        _ => Some(CondExpr::All(nodes)),
    })
}
```

Change `emit_grpc_route_bean` to return the route struct ident alongside the tokens so the caller can pair a guard (replace its return type + final `Ok`):
```rust
fn emit_grpc_route_bean(
    self_ty: &Type,
    service_trait: &syn::Path,
    controller_ident: &str,
    method: &ImplItemFn,
) -> Result<(TokenStream, String), EmitError> {
```
…and replace its final return line:
```rust
    Ok((quote! { #items #registration }, route_struct_ident.to_string()))
```

Then rewrite the loop body in `expand_grpc_controller_impl` (the `for inner in &item.items { ... }` block) to emit a propagated guard per route:
```rust
    let guard = propagated_guard(item)?;
    let mut rows = TokenStream::new();
    for inner in &item.items {
        let ImplItem::Fn(func) = inner else { continue };
        let (bean, route_ident) =
            emit_grpc_route_bean(&self_ty, &service_trait, &controller_ident, func)?;
        rows.extend(bean);
        // Condition propagation: copy the controller's guard onto THIS route bean, keyed by
        // the route struct ident — `emit_guard` mints the GUARD_PAIRINGS row on
        // `ContractId::of(module_path!() ++ "::" ++ route_ident)`, which is exactly the route
        // bean's contract (it is emitted `module_qualified` with `contract_path = route_ident`),
        // so the route registers/de-registers in lockstep with the conditioned struct.
        if let Some(expr) = &guard {
            rows.extend(conditional::emit_guard(&route_ident, expr));
        }
    }
```

- [ ] **Step 4: Run the codegen unit tests — they pass.**

```
cargo test -p leaf-codegen --lib grpc_controller::tests 2>&1 | tail -20
```
Expected: `test result: ok.` covering `a_conditioned_grpc_impl_propagates_the_guard_onto_every_route_bean`, `an_unconditioned_grpc_impl_emits_no_guard_rows`, `a_profile_gated_grpc_impl_propagates_the_profile_guard`, plus the unchanged existing `grpc_controller::tests` (the four shapes + the kind-marker/guard/error cases).

- [ ] **Step 5: Strip the propagated guard attrs from the re-emitted impl in the macro layer.** The `#[grpc_controller]` impl arm re-emits the impl via `async_impl::expand(&item_impl)` — that would re-emit the inert `#[conditional]`/`#[profile]` attrs, and a standalone `#[conditional]` proc-macro on an impl errors. Strip them first. Edit `crates/leaf-macros/src/lib.rs`, the `Item::Impl(item_impl)` arm of `grpc_controller`:

```rust
        Item::Impl(item_impl) => {
            // The propagated condition attrs (`#[conditional]`/`#[profile]`) ride the impl as
            // inert markers the codegen reads (condition propagation); STRIP them from the
            // re-emitted impl so the standalone `#[conditional]`/`#[profile]` macros never see
            // an impl (they only accept a struct). The codegen still reads them off the
            // ORIGINAL `item_impl` below.
            let cleaned = strip_outer_attrs(item_impl.clone(), &["conditional", "profile"]);
            let desugared = leaf_codegen::async_impl::expand(&cleaned);
            match leaf_codegen::grpc_controller::expand_grpc_controller_impl(&item_impl) {
                Ok(rows) => quote! { #desugared #rows }.into(),
                Err(err) => {
                    let error = compile_error(&err);
                    quote! { #desugared #error }.into()
                }
            }
        }
```

Add the `strip_outer_attrs` helper next to `strip_inner_attrs` (matching on the attribute path's LAST segment, the same convention `strip_inner_attrs` uses):
```rust
/// Strip the named OUTER attributes (`#[conditional]`/`#[profile]`) off an `impl` block.
///
/// The counterpart to [`strip_inner_attrs`] (which strips METHOD-position attrs): this
/// removes IMPL-position attrs the codegen has already consumed (the propagated condition
/// guards). Matching is on the attribute path's LAST segment so a fully-qualified
/// `#[::leaf_macros::conditional]` is stripped too.
fn strip_outer_attrs(mut item: ItemImpl, names: &[&str]) -> ItemImpl {
    item.attrs.retain(|a| {
        !a.path()
            .segments
            .last()
            .map(|s| names.contains(&s.ident.to_string().as_str()))
            .unwrap_or(false)
    });
    item
}
```

- [ ] **Step 6: Build the macro crate clean.**

```
cargo build -p leaf-macros 2>&1 | tail -5
```
Expected: `Finished` with no warnings.

- [ ] **Step 7: Commit.**

```
git add crates/leaf-codegen/src/grpc_controller.rs crates/leaf-macros/src/lib.rs && \
git commit -m "leaf-grpc: propagate #[grpc_controller] #[conditional]/#[profile] onto route beans

A conditioned/profiled gRPC controller now gates its struct bean AND its
generated GrpcRoute beans as one unit: the impl macro reads the guard attrs
off the impl block, emits one emit_guard(route_struct_ident, expr) per route
(keyed on the route bean's own contract), and strips the inert attrs from the
re-emitted impl.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.2: Ship reflection.proto (v1 + v1alpha) and compile via leaf-grpc-build

Ship the upstream gRPC reflection protos verbatim in `leaf-grpc/proto/` and compile both through `leaf_grpc_build::compile` in the build script. Each compiles to a leaf server trait `ServerReflection` (call shape `Streaming<ServerReflectionRequest> -> Result<Streaming<ServerReflectionResponse>, Status>`) plus the Stage-1 FDS const + auto-registration row (inert unless reflection reads it). The packages are `grpc.reflection.v1` and `grpc.reflection.v1alpha`; `leaf_grpc_build::compile` writes `$OUT_DIR/grpc.reflection.v1.rs` and `$OUT_DIR/grpc.reflection.v1alpha.rs`.

**Files**: `crates/leaf-grpc/proto/reflection_v1.proto`, `crates/leaf-grpc/proto/reflection_v1alpha.proto`, `crates/leaf-grpc/build.rs`

- [ ] **Step 1: Write the failing build-presence test.** Add to a new file `crates/leaf-grpc/tests/reflection_protos_compile.rs` a smoke test that the generated modules exist + carry the `ServerReflection` server trait and the prost request/response types. (This drives the build script + the proto files.)

```rust
//! The reflection protos compile through leaf-grpc-build to leaf server traits + the
//! Stage-1 FDS registration. A smoke proof that `grpc.reflection.v1` and
//! `grpc.reflection.v1alpha` each yield a `ServerReflection` server trait + the
//! ServerReflectionRequest/Response prost types, included from OUT_DIR.

mod gen_v1 {
    leaf_grpc::include_proto!("grpc.reflection.v1");
}
mod gen_v1alpha {
    leaf_grpc::include_proto!("grpc.reflection.v1alpha");
}

#[test]
fn the_reflection_protos_yield_server_reflection_traits_and_messages() {
    // The prost message types exist (constructed via Default) — a structural proof the
    // protos compiled.
    let _v1_req = gen_v1::ServerReflectionRequest::default();
    let _v1_resp = gen_v1::ServerReflectionResponse::default();
    let _v1a_req = gen_v1alpha::ServerReflectionRequest::default();
    let _v1a_resp = gen_v1alpha::ServerReflectionResponse::default();
    // The FDS const Stage-1 emits per proto package is present (a `&[u8]` the discovery
    // slice points at).
    let v1_fds: &[u8] = gen_v1::FILE_DESCRIPTOR_SET;
    assert!(!v1_fds.is_empty(), "the v1 reflection FDS const is non-empty");
}
```

- [ ] **Step 2: Run — it fails (no proto, no generated module).**

```
cargo test -p leaf-grpc --test reflection_protos_compile 2>&1 | tail -20
```
Expected: a build-script / include failure — `couldn't read .../grpc.reflection.v1.rs` (the proto is not compiled yet).

- [ ] **Step 3: Add `crates/leaf-grpc/proto/reflection_v1.proto`** (the upstream `grpc.reflection.v1` proto, verbatim).

```proto
// Copyright 2016 The gRPC Authors — upstream grpc.reflection.v1 (verbatim).
syntax = "proto3";

package grpc.reflection.v1;

option java_multiple_files = true;
option java_package = "io.grpc.reflection.v1";
option java_outer_classname = "ServerReflectionProto";

service ServerReflection {
  rpc ServerReflectionInfo(stream ServerReflectionRequest)
      returns (stream ServerReflectionResponse);
}

message ServerReflectionRequest {
  string host = 1;
  oneof message_request {
    string file_by_filename = 3;
    string file_containing_symbol = 4;
    ExtensionRequest file_containing_extension = 5;
    string all_extension_numbers_of_type = 6;
    string list_services = 7;
  }
}

message ExtensionRequest {
  string containing_type = 1;
  int32 extension_number = 2;
}

message ServerReflectionResponse {
  string valid_host = 1;
  ServerReflectionRequest original_request = 2;
  oneof message_response {
    FileDescriptorResponse file_descriptor_response = 4;
    ExtensionNumberResponse all_extension_numbers_response = 5;
    ListServiceResponse list_services_response = 6;
    ErrorResponse error_response = 7;
  }
}

message FileDescriptorResponse {
  repeated bytes file_descriptor_proto = 1;
}

message ExtensionNumberResponse {
  string base_type_name = 1;
  repeated int32 extension_number = 2;
}

message ListServiceResponse {
  repeated ServiceResponse service = 1;
}

message ServiceResponse {
  string name = 1;
}

message ErrorResponse {
  int32 error_code = 1;
  string error_message = 2;
}
```

- [ ] **Step 4: Add `crates/leaf-grpc/proto/reflection_v1alpha.proto`** (the upstream `grpc.reflection.v1alpha` proto — identical message/RPC shape, the `v1alpha` package).

```proto
// Copyright 2016 The gRPC Authors — upstream grpc.reflection.v1alpha (verbatim).
syntax = "proto3";

package grpc.reflection.v1alpha;

service ServerReflection {
  rpc ServerReflectionInfo(stream ServerReflectionRequest)
      returns (stream ServerReflectionResponse);
}

message ServerReflectionRequest {
  string host = 1;
  oneof message_request {
    string file_by_filename = 3;
    string file_containing_symbol = 4;
    ExtensionRequest file_containing_extension = 5;
    string all_extension_numbers_of_type = 6;
    string list_services = 7;
  }
}

message ExtensionRequest {
  string containing_type = 1;
  int32 extension_number = 2;
}

message ServerReflectionResponse {
  string valid_host = 1;
  ServerReflectionRequest original_request = 2;
  oneof message_response {
    FileDescriptorResponse file_descriptor_response = 4;
    ExtensionNumberResponse all_extension_numbers_response = 5;
    ListServiceResponse list_services_response = 6;
    ErrorResponse error_response = 7;
  }
}

message FileDescriptorResponse {
  repeated bytes file_descriptor_proto = 1;
}

message ExtensionNumberResponse {
  string base_type_name = 1;
  repeated int32 extension_number = 2;
}

message ListServiceResponse {
  repeated ServiceResponse service = 1;
}

message ServiceResponse {
  string name = 1;
}

message ErrorResponse {
  int32 error_code = 1;
  string error_message = 2;
}
```

- [ ] **Step 5: Compile both protos in the build script.** Edit `crates/leaf-grpc/build.rs` — add the reflection protos to the existing `leaf_grpc_build::compile` call (the build script already compiles `echo.proto`). Append after the `(2a)` echo compile, before the tonic block:

```rust
    // (3) leaf-grpc ships the upstream gRPC reflection protos (grpc.reflection.v1 +
    // grpc.reflection.v1alpha) -> $OUT_DIR/grpc.reflection.v1.rs / .v1alpha.rs. compile()
    // also writes each proto's encoded FileDescriptorSet const + the Stage-1
    // REFLECTED_FILE_DESCRIPTOR_SETS auto-registration row (inert unless reflection reads it).
    leaf_grpc_build::compile(
        &["proto/reflection_v1.proto", "proto/reflection_v1alpha.proto"],
        &["proto"],
    )?;
    println!("cargo:rerun-if-changed=proto/reflection_v1.proto");
    println!("cargo:rerun-if-changed=proto/reflection_v1alpha.proto");
```

- [ ] **Step 6: Run the smoke test — it passes.**

```
cargo test -p leaf-grpc --test reflection_protos_compile 2>&1 | tail -15
```
Expected: `test the_reflection_protos_yield_server_reflection_traits_and_messages ... ok`.

- [ ] **Step 7: Commit.**

```
git add crates/leaf-grpc/proto/ crates/leaf-grpc/build.rs crates/leaf-grpc/tests/reflection_protos_compile.rs && \
git commit -m "leaf-grpc: ship + compile grpc.reflection.v1 / v1alpha protos

Adds the upstream gRPC server-reflection protos and compiles both through
leaf-grpc-build to leaf ServerReflection server traits + the Stage-1
FileDescriptorSet registration rows.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.3: The shared request->response adapter over ReflectionIndex

A version-agnostic adapter maps one reflection query (already decoded from the request's `message_request` oneof) to the response payload, delegating to the Stage-2 `ReflectionIndex`. Because v1 and v1alpha have identical message shapes but DISTINCT generated Rust types (one per package module), the adapter is written ONCE generically over closures the two controllers supply — it returns the `Vec` payloads the index produces, and the controllers assemble their version's concrete `ServerReflectionResponse`. A not-found query yields the `NOT_FOUND` (code 5) marker so the controller builds an `ErrorResponse`.

**Files**: `crates/leaf-grpc/src/reflection/mod.rs`, `crates/leaf-grpc/src/reflection/service.rs` (the adapter portion), `crates/leaf-grpc/tests/reflection_controllers.rs` (the adapter unit)

- [ ] **Step 1: Write the failing adapter unit test.** Create `crates/leaf-grpc/tests/reflection_controllers.rs` with the index-adapter unit: build a `ReflectionIndex` from a real FDS (reuse the compiled reflection v1 FDS — it contains `grpc.reflection.v1.ServerReflection`), and assert the adapter answers `list_services`, `file_containing_symbol` (file + closure), and a NOT_FOUND for an unknown symbol.

```rust
//! The reflection controllers + the shared index adapter. Three proofs:
//!  (1) the version-agnostic adapter over a real ReflectionIndex (list/symbol/not-found),
//!  (2) a conditioned controller gates its struct bean AND its route beans as one unit
//!      (the condition-propagation wiring proof) via leaf-boot's lazy assembly,
//!  (3) flipping `leaf.grpc.reflection.enabled` registers the route.

use leaf_grpc::reflection::{Answer, ReflectionIndex};

/// Build the index from the v1 reflection FDS (it self-describes
/// `grpc.reflection.v1.ServerReflection`) — a real, non-trivial descriptor set.
fn index() -> ReflectionIndex {
    mod gen_v1 {
        leaf_grpc::include_proto!("grpc.reflection.v1");
    }
    ReflectionIndex::from_descriptor_sets(&[gen_v1::FILE_DESCRIPTOR_SET])
        .expect("the v1 reflection FDS decodes into an index")
}

#[test]
fn the_adapter_lists_the_reflection_service() {
    let idx = index();
    let services = idx.list_services();
    assert!(
        services.iter().any(|s| s == "grpc.reflection.v1.ServerReflection"),
        "list_services surfaces the reflection service FQN: {services:?}"
    );
}

#[test]
fn the_adapter_returns_a_file_and_its_closure_for_a_symbol() {
    let idx = index();
    let files = idx
        .file_containing_symbol("grpc.reflection.v1.ServerReflectionRequest")
        .expect("the defining file is found");
    assert!(!files.is_empty(), "file_containing_symbol returns the defining file(s)");
}

#[test]
fn an_unknown_symbol_is_a_not_found_answer() {
    // The adapter renders an unknown symbol as the NOT_FOUND marker (code 5), which the
    // controller wraps in a reflection ErrorResponse (NOT a transport Status).
    let idx = index();
    let answer = Answer::for_symbol(&idx, "does.not.Exist");
    match answer {
        Answer::NotFound { error_code, .. } => {
            assert_eq!(error_code, 5, "unknown symbol -> NOT_FOUND (code 5)");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run — it fails (no `reflection` module / `Answer` type).**

```
cargo test -p leaf-grpc --test reflection_controllers 2>&1 | tail -15
```
Expected: `error[E0432]: unresolved import leaf_grpc::reflection`.

- [ ] **Step 3: Create the `reflection` module root** `crates/leaf-grpc/src/reflection/mod.rs` — splice both generated packages into version submodules + expose `service`.

```rust
//! gRPC Server Reflection (opt-in). Ships the upstream grpc.reflection.v1 /
//! v1alpha protos (compiled by leaf-grpc-build to leaf ServerReflection traits) and the
//! two thin `#[grpc_controller]` beans serving the bidi `ServerReflectionInfo` RPC over the
//! shared `ReflectionIndex` (built once from `REFLECTED_FILE_DESCRIPTOR_SETS`). Each
//! controller is `#[conditional(on_property = "leaf.grpc.reflection.enabled")]` — OFF by
//! default; a reflection request then hits no route (`Code::Unimplemented`).
//!
//! Backend-free: pure leaf-grpc + prost/prost-types. NO type-name detection — the index
//! keys on the FDS wire symbol strings (the gRPC identifiers), never a Rust type name.

/// The generated grpc.reflection.v1 module (server trait + prost messages + FDS const).
pub mod v1 {
    leaf_grpc::include_proto!("grpc.reflection.v1");
}
/// The generated grpc.reflection.v1alpha module.
pub mod v1alpha {
    leaf_grpc::include_proto!("grpc.reflection.v1alpha");
}

mod service;

pub use crate::index::ReflectionIndex; // re-export the Stage-2 index from its home module.
pub use service::{Answer, ReflectionV1, ReflectionV1alpha};
```

(Note: if the Stage-2 `ReflectionIndex` already lives at `crate::index`, this re-export surfaces it under `reflection::`; adjust the `pub use crate::index::ReflectionIndex;` path to wherever Stage 2 placed it.)

- [ ] **Step 4: Implement the version-agnostic `Answer` adapter** in `crates/leaf-grpc/src/reflection/service.rs` (the adapter portion — the controllers come in Task 3.4). It delegates to the index and carries either the encoded `FileDescriptorProto` byte vectors, the service-name list, the extension numbers, or the NOT_FOUND marker.

```rust
//! The two thin `#[grpc_controller]` reflection beans + the version-agnostic `Answer`
//! adapter over the shared `ReflectionIndex`. Both v1 and v1alpha have identical reflection
//! semantics but distinct generated Rust types; the adapter is written ONCE and each
//! controller assembles its own `ServerReflectionResponse` from it.

use prost::Message as _;

use crate::index::ReflectionIndex;

/// One resolved reflection answer — the version-agnostic payload the controllers render into
/// their version's `ServerReflectionResponse`. The `FileDescriptors` variant carries the
/// already-ENCODED `FileDescriptorProto` bytes (the matched file + its transitive dependency
/// closure, deduped) the wire format wants; `NotFound` is the reflection-level error
/// (code 5 == NOT_FOUND) — a normal response, NOT a transport `Status`.
#[derive(Debug)]
pub enum Answer {
    /// `file_by_filename` / `file_containing_symbol` / `file_containing_extension`: the
    /// encoded FileDescriptorProto bytes (the file + its dependency closure).
    FileDescriptors(Vec<Vec<u8>>),
    /// `list_services`: the fully-qualified service names.
    Services(Vec<String>),
    /// `all_extension_numbers_of_type`: the base type name + its extension field numbers.
    ExtensionNumbers { base_type_name: String, numbers: Vec<i32> },
    /// A reflection-level not-found (code 5 == NOT_FOUND): rendered as an `ErrorResponse`.
    NotFound { error_code: i32, error_message: String },
}

/// Encode a list of `FileDescriptorProto`s to the wire `bytes` the reflection response
/// carries (one encoded message per file).
fn encode_files(files: Vec<prost_types::FileDescriptorProto>) -> Vec<Vec<u8>> {
    files.into_iter().map(|f| f.encode_to_vec()).collect()
}

impl Answer {
    /// `list_services` — every fully-qualified service name in the index.
    #[must_use]
    pub fn list_services(index: &ReflectionIndex) -> Self {
        Answer::Services(index.list_services())
    }

    /// `file_by_filename` — the file + its transitive dependency closure, or NOT_FOUND.
    #[must_use]
    pub fn for_filename(index: &ReflectionIndex, name: &str) -> Self {
        match index.file_by_filename(name) {
            Some(files) => Answer::FileDescriptors(encode_files(files)),
            None => Answer::not_found(format!("file not found: {name}")),
        }
    }

    /// `file_containing_symbol` — the defining file + closure, or NOT_FOUND.
    #[must_use]
    pub fn for_symbol(index: &ReflectionIndex, symbol: &str) -> Self {
        match index.file_containing_symbol(symbol) {
            Some(files) => Answer::FileDescriptors(encode_files(files)),
            None => Answer::not_found(format!("symbol not found: {symbol}")),
        }
    }

    /// `file_containing_extension` — the file defining `(extendee, number)`, or NOT_FOUND.
    #[must_use]
    pub fn for_extension(index: &ReflectionIndex, extendee: &str, number: i32) -> Self {
        match index.file_containing_extension(extendee, number) {
            Some(files) => Answer::FileDescriptors(encode_files(files)),
            None => {
                Answer::not_found(format!("extension not found: {extendee} {number}"))
            }
        }
    }

    /// `all_extension_numbers_of_type` — the extension field numbers for `type_name`, or
    /// NOT_FOUND.
    #[must_use]
    pub fn for_all_extension_numbers(index: &ReflectionIndex, type_name: &str) -> Self {
        match index.all_extension_numbers_of_type(type_name) {
            Some(numbers) => Answer::ExtensionNumbers {
                base_type_name: type_name.to_string(),
                numbers,
            },
            None => Answer::not_found(format!("type not found: {type_name}")),
        }
    }

    /// The reflection NOT_FOUND marker (code 5 in the gRPC status-code space).
    #[must_use]
    fn not_found(message: impl Into<String>) -> Self {
        Answer::NotFound { error_code: 5, error_message: message.into() }
    }
}
```

Add the module deps to `crates/leaf-grpc/Cargo.toml` (`prost-types` for `FileDescriptorProto`):
```toml
# prost-types — the well-known FileDescriptorProto/Set types the reflection index + the
# response encoder traffic in (the descriptors reflection answers with). A normal dep, but
# only the reflection module names it.
prost-types.workspace = true
```

- [ ] **Step 5: Wire the module into the crate.** Edit `crates/leaf-grpc/src/lib.rs` — add `pub mod reflection;` after the other `mod` declarations + re-export the controllers near the existing `pub use` block:
```rust
pub mod reflection;
pub use reflection::{Answer, ReflectionIndex, ReflectionV1, ReflectionV1alpha};
```
(Leave `Answer`/`ReflectionV1`/`ReflectionV1alpha` unresolved warnings for now — Task 3.4 adds the controllers. If `lib.rs` will not compile without them, defer this `pub use` line to Task 3.4 Step 4.)

- [ ] **Step 6: Run the adapter units — they pass.**

```
cargo test -p leaf-grpc --test reflection_controllers the_adapter 2>&1 | tail -15 && \
cargo test -p leaf-grpc --test reflection_controllers an_unknown_symbol 2>&1 | tail -15
```
Expected: `the_adapter_lists_the_reflection_service ... ok`, `the_adapter_returns_a_file_and_its_closure_for_a_symbol ... ok`, `an_unknown_symbol_is_a_not_found_answer ... ok`.

- [ ] **Step 7: Commit.**

```
git add crates/leaf-grpc/src/reflection/ crates/leaf-grpc/src/lib.rs crates/leaf-grpc/Cargo.toml crates/leaf-grpc/tests/reflection_controllers.rs && \
git commit -m "leaf-grpc: version-agnostic reflection Answer adapter over ReflectionIndex

The shared adapter maps each decoded reflection query to its payload (encoded
FileDescriptorProto closure / service list / extension numbers) or the
reflection NOT_FOUND (code 5) marker — written once for both v1 and v1alpha.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.4: The two thin #[grpc_controller] reflection beans, each #[conditional]-gated

Add `ReflectionV1` / `ReflectionV1alpha`: each a `#[grpc_controller]` struct field-injecting the shared `ReflectionIndex` (built once, by-trait/by-value injection from the Stage-1 slice — provided as a bean), with a `#[grpc_controller] impl <pkg>::server_reflection::ServerReflection` carrying `#[conditional(on_property = "leaf.grpc.reflection.enabled")]` on BOTH struct and impl. The bidi RPC maps each inbound `ServerReflectionRequest` to one `ServerReflectionResponse` via the `Answer` adapter, decoding the request's `message_request` oneof and rendering the response oneof. A corrupt FDS at index-build time is the only `Status` (`Internal`); a not-found query is a normal `ErrorResponse`.

**Files**: `crates/leaf-grpc/src/reflection/service.rs`, `crates/leaf-grpc/tests/reflection_controllers.rs`

- [ ] **Step 1: Write the failing wiring test (the condition-propagation headline).** Add to `crates/leaf-grpc/tests/reflection_controllers.rs` a leaf-boot lazy-assembly proof that the `ReflectionV1` route bean is gated as a unit: with the property unset, the route does NOT resolve into `Vec<Ref<dyn GrpcRoute>>`; with `leaf.grpc.reflection.enabled = true`, it does. (Mirror the by-collection resolve `tests/grpc_di_assembly.rs` uses.)

```rust
use leaf_boot::App;
use leaf_core::{Injectable, Ref, ResolveCtx};
use leaf_grpc::GrpcRoute;

/// Force-link leaf-grpc + the reflection beans so their COMPONENTS rows are present.
fn force_link() {
    let _ = std::any::TypeId::of::<leaf_grpc::reflection::ReflectionV1>();
    let _ = std::any::TypeId::of::<leaf_grpc::reflection::ReflectionV1alpha>();
}

/// Resolve the collected `Vec<Ref<dyn GrpcRoute>>` under the given property args, returning
/// the route `path()`s the conditioned assembly admits.
fn resolved_route_paths(args: &[&str]) -> Vec<String> {
    force_link();
    let app = App::from_slices()
        .with_args(args.iter().map(|s| s.to_string()))
        .run_autoconfig()
        .seal()
        .expect("the app seals");
    let cx = ResolveCtx::root(&app);
    let routes: Vec<Ref<dyn GrpcRoute>> = futures::executor::block_on(
        <Vec<Ref<dyn GrpcRoute>> as Injectable>::inject(&cx),
    )
    .expect("collection-injects the GrpcRoute beans");
    routes.iter().map(|r| r.path().to_string()).collect()
}

#[test]
fn reflection_routes_are_gated_by_the_enabled_property_as_a_unit() {
    // OFF by default: the ReflectionV1 controller struct bean de-registers (its
    // #[conditional]) AND — via condition propagation — so do its GrpcRoute beans, so the
    // reflection path is absent from the collected routes.
    let off = resolved_route_paths(&[]);
    assert!(
        !off.iter().any(|p| p.contains("grpc.reflection.v1.ServerReflection")),
        "reflection is OFF by default — no reflection route registers: {off:?}"
    );

    // ON: flipping leaf.grpc.reflection.enabled=true registers the struct AND the routes.
    let on = resolved_route_paths(&["--leaf.grpc.reflection.enabled=true"]);
    assert!(
        on.iter().any(|p| p.contains("grpc.reflection.v1.ServerReflection")),
        "reflection ON registers the v1 ServerReflectionInfo route: {on:?}"
    );
}
```

- [ ] **Step 2: Run — it fails (no `ReflectionV1` controller).**

```
cargo test -p leaf-grpc --test reflection_controllers reflection_routes_are_gated 2>&1 | tail -20
```
Expected: `error[E0599]`/`no associated item` for `ReflectionV1` / no `ServerReflectionInfo` route resolves.

- [ ] **Step 3: Provide the shared `ReflectionIndex` as a bean + the two controllers** in `crates/leaf-grpc/src/reflection/service.rs`. The index is built once from the Stage-1 slice via an `#[auto_config]`/`#[bean]` factory (dogfooded — no hand-rolled Provider); the controllers field-inject `Ref<ReflectionIndex>`. Append to `service.rs`:

```rust
use leaf_core::{BoxStream, Ref};
use crate::{Status, Streaming};
use crate::reflection::{v1, v1alpha};
use futures::StreamExt as _;

/// Provide the shared `ReflectionIndex` as a singleton bean, built ONCE from the Stage-1
/// `REFLECTED_FILE_DESCRIPTOR_SETS` discovery slice. A corrupt FDS that fails to decode is a
/// hard boot error (the descriptors are app-compiled, so a decode failure is internal). The
/// bean is unconditional — only the controllers that READ it are gated; the slice itself is
/// collected regardless of whether reflection is enabled.
#[leaf_macros::configuration]
pub struct ReflectionIndexConfig;

#[leaf_macros::bean]
impl ReflectionIndexConfig {
    #[bean]
    fn reflection_index(&self) -> Ref<ReflectionIndex> {
        let sets: &[&[u8]] = &crate::REFLECTED_FILE_DESCRIPTOR_SETS;
        let index = ReflectionIndex::from_descriptor_sets(sets)
            .expect("the app's compiled FileDescriptorSets decode");
        Ref::new(index)
    }
}

/// The grpc.reflection.v1 controller — gated OFF by default; field-injects the shared index.
#[leaf_macros::grpc_controller]
#[leaf_macros::conditional(on_property("leaf.grpc.reflection.enabled"))]
pub struct ReflectionV1 {
    index: Ref<ReflectionIndex>,
}

#[leaf_macros::grpc_controller]
#[leaf_macros::conditional(on_property("leaf.grpc.reflection.enabled"))]
impl v1::server_reflection::ServerReflection for ReflectionV1 {
    async fn server_reflection_info(
        &self,
        mut requests: Streaming<v1::ServerReflectionRequest>,
    ) -> Result<Streaming<v1::ServerReflectionResponse>, Status> {
        let index = self.index.clone();
        let out: BoxStream<'static, Result<v1::ServerReflectionResponse, Status>> =
            Box::pin(async_stream_v1(requests, index));
        Ok(Streaming::new(out))
    }
}

/// The grpc.reflection.v1alpha controller — identical semantics, the v1alpha types.
#[leaf_macros::grpc_controller]
#[leaf_macros::conditional(on_property("leaf.grpc.reflection.enabled"))]
pub struct ReflectionV1alpha {
    index: Ref<ReflectionIndex>,
}

#[leaf_macros::grpc_controller]
#[leaf_macros::conditional(on_property("leaf.grpc.reflection.enabled"))]
impl v1alpha::server_reflection::ServerReflection for ReflectionV1alpha {
    async fn server_reflection_info(
        &self,
        requests: Streaming<v1alpha::ServerReflectionRequest>,
    ) -> Result<Streaming<v1alpha::ServerReflectionResponse>, Status> {
        let index = self.index.clone();
        let out: BoxStream<'static, Result<v1alpha::ServerReflectionResponse, Status>> =
            Box::pin(async_stream_v1alpha(requests, index));
        Ok(Streaming::new(out))
    }
}
```

(NOTE on the trait path: Task 3.3's smoke test used the bare `ServerReflection`/messages because the Stage-3 generator may emit them top-level rather than under a `server_reflection` submodule — confirm the generated shape from `$OUT_DIR/grpc.reflection.v1.rs` after Task 3.2 and use whichever the generator emits, e.g. `v1::ServerReflection` if top-level. The `_DESCRIPTOR` path module the `#[grpc_controller]` macro reads follows the same convention `echo` did.)

- [ ] **Step 4: Add the per-version response mappers** (the bidi stream bodies) to `service.rs`. Each decodes the inbound `message_request` oneof, calls the shared `Answer` adapter, and renders the version's `message_response` oneof. Written per-version because the generated oneof enums are distinct types.

```rust
/// Render one v1 request -> one v1 response via the shared `Answer` adapter.
fn respond_v1(
    request: &v1::ServerReflectionRequest,
    index: &ReflectionIndex,
) -> v1::ServerReflectionResponse {
    use v1::server_reflection_request::MessageRequest;
    use v1::server_reflection_response::MessageResponse;

    let answer = match &request.message_request {
        Some(MessageRequest::ListServices(_)) => Answer::list_services(index),
        Some(MessageRequest::FileByFilename(name)) => Answer::for_filename(index, name),
        Some(MessageRequest::FileContainingSymbol(sym)) => Answer::for_symbol(index, sym),
        Some(MessageRequest::FileContainingExtension(ext)) => {
            Answer::for_extension(index, &ext.containing_type, ext.extension_number)
        }
        Some(MessageRequest::AllExtensionNumbersOfType(ty)) => {
            Answer::for_all_extension_numbers(index, ty)
        }
        None => Answer::NotFound {
            error_code: 3, // INVALID_ARGUMENT: a request with no message_request set.
            error_message: "empty reflection request".into(),
        },
    };

    let message_response = match answer {
        Answer::FileDescriptors(files) => {
            MessageResponse::FileDescriptorResponse(v1::FileDescriptorResponse {
                file_descriptor_proto: files,
            })
        }
        Answer::Services(names) => {
            MessageResponse::ListServicesResponse(v1::ListServiceResponse {
                service: names
                    .into_iter()
                    .map(|name| v1::ServiceResponse { name })
                    .collect(),
            })
        }
        Answer::ExtensionNumbers { base_type_name, numbers } => {
            MessageResponse::AllExtensionNumbersResponse(v1::ExtensionNumberResponse {
                base_type_name,
                extension_number: numbers,
            })
        }
        Answer::NotFound { error_code, error_message } => {
            MessageResponse::ErrorResponse(v1::ErrorResponse { error_code, error_message })
        }
    };
    v1::ServerReflectionResponse {
        valid_host: request.host.clone(),
        original_request: Some(request.clone()),
        message_response: Some(message_response),
    }
}

/// The v1 bidi body: map each inbound request to one response. A malformed inbound frame
/// (a `Status` from the de-framer) propagates as the stream's `Err` (a transport error).
fn async_stream_v1(
    mut requests: Streaming<v1::ServerReflectionRequest>,
    index: Ref<ReflectionIndex>,
) -> impl futures::Stream<Item = Result<v1::ServerReflectionResponse, Status>> {
    async_stream_helper! { requests, index, respond_v1 }
}

/// Render one v1alpha request -> one v1alpha response (identical logic, v1alpha types).
fn respond_v1alpha(
    request: &v1alpha::ServerReflectionRequest,
    index: &ReflectionIndex,
) -> v1alpha::ServerReflectionResponse {
    use v1alpha::server_reflection_request::MessageRequest;
    use v1alpha::server_reflection_response::MessageResponse;

    let answer = match &request.message_request {
        Some(MessageRequest::ListServices(_)) => Answer::list_services(index),
        Some(MessageRequest::FileByFilename(name)) => Answer::for_filename(index, name),
        Some(MessageRequest::FileContainingSymbol(sym)) => Answer::for_symbol(index, sym),
        Some(MessageRequest::FileContainingExtension(ext)) => {
            Answer::for_extension(index, &ext.containing_type, ext.extension_number)
        }
        Some(MessageRequest::AllExtensionNumbersOfType(ty)) => {
            Answer::for_all_extension_numbers(index, ty)
        }
        None => Answer::NotFound {
            error_code: 3,
            error_message: "empty reflection request".into(),
        },
    };
    let message_response = match answer {
        Answer::FileDescriptors(files) => {
            MessageResponse::FileDescriptorResponse(v1alpha::FileDescriptorResponse {
                file_descriptor_proto: files,
            })
        }
        Answer::Services(names) => {
            MessageResponse::ListServicesResponse(v1alpha::ListServiceResponse {
                service: names
                    .into_iter()
                    .map(|name| v1alpha::ServiceResponse { name })
                    .collect(),
            })
        }
        Answer::ExtensionNumbers { base_type_name, numbers } => {
            MessageResponse::AllExtensionNumbersResponse(v1alpha::ExtensionNumberResponse {
                base_type_name,
                extension_number: numbers,
            })
        }
        Answer::NotFound { error_code, error_message } => {
            MessageResponse::ErrorResponse(v1alpha::ErrorResponse { error_code, error_message })
        }
    };
    v1alpha::ServerReflectionResponse {
        valid_host: request.host.clone(),
        original_request: Some(request.clone()),
        message_response: Some(message_response),
    }
}

fn async_stream_v1alpha(
    requests: Streaming<v1alpha::ServerReflectionRequest>,
    index: Ref<ReflectionIndex>,
) -> impl futures::Stream<Item = Result<v1alpha::ServerReflectionResponse, Status>> {
    async_stream_helper! { requests, index, respond_v1alpha }
}
```

Add the shared stream helper macro (avoids `async-stream` dep — a hand-rolled `futures::stream::unfold` over the inbound `Streaming`):
```rust
/// Map each inbound reflection request to one response over the bidi stream: a small
/// `unfold` that threads the inbound `Streaming<Req>` + the shared index, applying `$render`
/// per request. A malformed inbound frame (`Err(Status)` from de-framing) ends the stream by
/// yielding that transport `Status`.
macro_rules! async_stream_helper {
    ($requests:expr, $index:expr, $render:path) => {
        futures::stream::unfold(
            ($requests, $index),
            |(mut requests, index)| async move {
                match requests.next().await {
                    Some(Ok(req)) => {
                        let resp = $render(&req, &index);
                        Some((Ok(resp), (requests, index)))
                    }
                    Some(Err(status)) => Some((Err(status), (requests, index))),
                    None => None,
                }
            },
        )
    };
}
use async_stream_helper;
```

- [ ] **Step 5: Build the crate clean.**

```
cargo build -p leaf-grpc 2>&1 | tail -15
```
Expected: `Finished`. If the generated trait/oneof module paths differ from the assumed `v1::server_reflection::ServerReflection` / `v1::server_reflection_request::MessageRequest`, fix the paths against `$OUT_DIR/grpc.reflection.v1.rs` (e.g. `target/.../out/grpc.reflection.v1.rs`) and rebuild.

- [ ] **Step 6: Run the wiring + adapter tests — they pass.**

```
cargo test -p leaf-grpc --test reflection_controllers 2>&1 | tail -20
```
Expected: `test result: ok.` for all five tests, including `reflection_routes_are_gated_by_the_enabled_property_as_a_unit` (the condition-propagation headline: OFF -> no route, ON -> the v1 route).

- [ ] **Step 7: Commit.**

```
git add crates/leaf-grpc/src/reflection/service.rs crates/leaf-grpc/src/lib.rs crates/leaf-grpc/tests/reflection_controllers.rs && \
git commit -m "leaf-grpc: ReflectionV1/V1alpha #[grpc_controller] beans, conditional-gated

Two thin reflection controllers, each #[conditional(on_property =
leaf.grpc.reflection.enabled)] on struct + impl, field-injecting the shared
ReflectionIndex (provided once from REFLECTED_FILE_DESCRIPTOR_SETS). The bidi
ServerReflectionInfo RPC maps each request to one response via the Answer
adapter; not-found -> ErrorResponse{error_code:5}. Condition propagation gates
the struct + its routes as one unit (proven by the gated-routes wiring test).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3.5: Stage gate — force-clean test + clippy + doc

Verify the whole stage holds: the new behavior plus the existing ~1733-test suite stay green, with no warnings, and `leaf-grpc` stays backend-free.

**Files**: none (verification only)

- [ ] **Step 1: Force-clean the relevant crates so cached runs cannot mask warnings.**

```
cargo clean -p leaf-codegen -p leaf-macros -p leaf-grpc -p leaf-grpc-build 2>&1 | tail -2
```

- [ ] **Step 2: Run the codegen + grpc test suites.**

```
cargo test -p leaf-codegen -p leaf-grpc 2>&1 | tail -30
```
Expected: `test result: ok.` for every binary — the `grpc_controller::tests` propagation cases, `grpc_controller_lowering`, `serves_grpc`, `reflection_protos_compile`, and `reflection_controllers` all green; zero failures.

- [ ] **Step 3: Run the full workspace test suite (the ~1733-test regression gate).**

```
cargo test --workspace 2>&1 | tail -30
```
Expected: every crate `test result: ok.`; the total stays at or above the prior ~1733 (the new reflection/propagation tests add to it), zero failures.

- [ ] **Step 4: Clippy clean (deny warnings).**

```
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```
Expected: `Finished` with no `warning:`/`error:` lines.

- [ ] **Step 5: Doc clean + assert leaf-grpc names no backend.** The backend-free constraint: only `leaf-web-hyper` may name hyper/h2.

```
cargo doc -p leaf-grpc --no-deps 2>&1 | tail -5 && \
! grep -RInE '\b(hyper|h2)\b' crates/leaf-grpc/src && echo "leaf-grpc names no backend"
```
Expected: `Documenting leaf-grpc` / `Finished`, then `leaf-grpc names no backend` (the grep finds nothing in `src/`).

- [ ] **Step 6: Commit any final touch-ups (only if Steps 2-5 required edits).**

```
git commit -am "leaf-grpc: force-clean gate green for reflection + condition propagation

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Stage 4: Integration + dogfood + gate

The headline proof. With reflection wired (Stages 1-3 land the `REFLECTED_FILE_DESCRIPTOR_SETS` slice + the per-proto FDS registration row + the `ReflectionIndex` + the two `#[conditional]`-gated `#[grpc_controller]`s), this stage proves the whole thing end-to-end over real H2 with a tonic-generated reflection client (dev-only, no external `grpcurl`):

1. A `leaf-grpc` integration test boots the shared hyper `WebServer` with `--leaf.grpc.reflection.enabled=true`, drives `ServerReflectionInfo` over real H2, asserts `list_services` includes the app's `echo.Echo`, and `file_containing_symbol("echo.EchoRequest")` returns the descriptor + its dependency closure.
2. The opt-in proof: reflection OFF (default) → the same reflection call is `Code::Unimplemented` (no route registered); flip `--leaf.grpc.reflection.enabled=true` → the same call succeeds. This is the condition-propagation payoff from Stage 3 (struct bean + its `GrpcRoute` beans gate as one unit).
3. The storefront becomes reflectable: its gRPC dogfood test boots with reflection on and asserts `storefront.catalog.Catalog` shows up via the reflection client.
4. Full force-clean gate (test + clippy + doc) across the workspace, keeping the existing ~1733-test suite green.

This stage writes NO production code beyond a build-script addition (the dev-only tonic reflection client stub) and the storefront enabling the property — all the reflection machinery is dogfooded `#[grpc_controller]` beans + the discovery slice from Stages 1-3.

### Files

**Modify**
- `crates/leaf-grpc/build.rs` — also generate the dev-only tonic reflection client stub for `grpc.reflection.v1` from the shipped `reflection.proto` (into `$OUT_DIR/tonic/`), alongside the existing echo client.
- `examples/storefront/build.rs` — generate the dev-only tonic reflection client stub for `grpc.reflection.v1` (into `$OUT_DIR/tonic/`), alongside the existing catalog client.

**Create**
- `crates/leaf-grpc/tests/reflection_over_h2.rs` — the headline integration proof (boot the shared WebServer + a tonic reflection client over real H2: opt-in OFF→`Unimplemented`, ON→`list_services` + `file_containing_symbol`).
- `examples/storefront/tests/grpc_reflection.rs` — the storefront-reflectable proof (`storefront.catalog.Catalog` visible via the reflection client when enabled).

---

### Task 4.1: Generate the dev-only tonic reflection CLIENT stub in leaf-grpc

The integration test peer is the reflection.proto's own tonic-generated client (no external grpcurl). leaf-grpc already ships `proto/reflection.proto` (Stage 3) and compiles it to the leaf server trait; here we additionally emit tonic's CLIENT stub for `grpc.reflection.v1` into `$OUT_DIR/tonic/grpc.reflection.v1.rs`, the SAME separate-dir pattern the echo client uses. tonic/tonic-build stay build/dev-only.

**Files**
- Modify: `crates/leaf-grpc/build.rs`

- [ ] **Step 1: Write a failing test that the tonic reflection client stub exists in OUT_DIR.**

  Add a throwaway include-compile check to the new integration test file (full test lands in 4.2); for now just prove the build emits the stub. Create `crates/leaf-grpc/tests/reflection_over_h2.rs` with only:

  ```rust
  //! The gRPC SERVER REFLECTION integration proof (Stage 4): the shared hyper WebServer
  //! boots in-process with H2; a tonic-generated reflection client (the reflection.proto's
  //! own client, dev-only, NO external grpcurl) drives ServerReflectionInfo over real H2.
  //! Opt-in: OFF (default) -> Code::Unimplemented; ON -> list_services + file_containing_symbol.

  // The tonic-generated CLIENT for the SHIPPED grpc.reflection.v1 reflection.proto, compiled
  // by tonic's own codegen into a SEPARATE $OUT_DIR/tonic/ dir (so it never collides with the
  // leaf-grpc-build server trait). The polyglot reflection peer; leaf names no tonic above dev.
  pub mod reflection_tonic {
      include!(concat!(env!("OUT_DIR"), "/tonic/grpc.reflection.v1.rs"));
  }

  #[test]
  fn the_tonic_reflection_client_stub_is_generated() {
      // Compiling this file at all proves the include! resolved; name the client type so the
      // module is not dead-code-eliminated before the include is type-checked.
      let _ = std::any::type_name::<
          reflection_tonic::server_reflection_client::ServerReflectionClient<()>,
      >();
  }
  ```

- [ ] **Step 2: Run it; it fails to compile (no stub yet).**

  ```
  cargo test -p leaf-grpc --test reflection_over_h2
  ```

  Expected: a build error — `error: couldn't read $OUT_DIR/tonic/grpc.reflection.v1.rs: No such file or directory` (the build script does not emit the reflection client stub yet).

- [ ] **Step 3: Add the reflection client stub to the leaf-grpc build script.**

  In `crates/leaf-grpc/build.rs`, after the existing echo `compile_fds` block (before the `cargo:rerun-if-changed` lines), add the reflection-proto client generation. The leaf server trait for reflection.proto is already produced by `leaf_grpc_build::compile` in Stage 3; this only adds the tonic CLIENT stub from the SAME shipped proto:

  ```rust
      // (2c) tonic's CLIENT stub for the SHIPPED reflection.proto -> $OUT_DIR/tonic/. protox
      // parses grpc.reflection.v1's FileDescriptorSet (pure Rust, NO protoc), fed to
      // tonic-build's `compile_fds`. Client-only: leaf serves reflection (the dogfooded
      // #[grpc_controller]s); tonic provides the dev-test reflection client peer.
      let refl_fds = protox::compile(["proto/reflection.proto"], ["proto"])
          .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
      tonic_build::configure()
          .build_server(false)
          .build_client(true)
          .out_dir(&tonic_out)
          .compile_fds(refl_fds)?;
      println!("cargo:rerun-if-changed=proto/reflection.proto");
  ```

  (`tonic_out` is already in scope from the echo block; `tonic_build`/`protox` are already build-deps.)

- [ ] **Step 4: Run it; it passes.**

  ```
  cargo test -p leaf-grpc --test reflection_over_h2
  ```

  Expected: `test the_tonic_reflection_client_stub_is_generated ... ok` and `test result: ok. 1 passed`.

- [ ] **Step 5: Commit.**

  ```
  git add crates/leaf-grpc/build.rs crates/leaf-grpc/tests/reflection_over_h2.rs
  git commit -m "leaf-grpc: dev-only tonic reflection.proto client stub for the H2 proof

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 4.2: The opt-in proof — reflection OFF (default) → Code::Unimplemented

With no `leaf.grpc.reflection.enabled` property, the `#[conditional]`-gated reflection controllers (struct bean + their propagated-condition `GrpcRoute` beans, Stage 3) do NOT register — so a reflection call hits no route and rides back the normal unknown-method `Code::Unimplemented`. This proves the safe-by-default posture.

**Files**
- Modify: `crates/leaf-grpc/tests/reflection_over_h2.rs`

- [ ] **Step 1: Write the failing OFF-by-default test.**

  Replace the placeholder test with the real test harness (the boot scaffolding mirrors `tests/serves_grpc.rs`) plus the OFF case. Note `force_link` ALSO names the two reflection controllers so their bean rows link into the test binary; the CONDITION (not linking) is what gates them off:

  ```rust
  use std::sync::Arc;

  use leaf_boot::{Application, RunOverlay, SealInputs};
  use futures::StreamExt;

  use reflection_tonic::server_reflection_client::ServerReflectionClient;
  use reflection_tonic::server_reflection_request::MessageRequest;
  use reflection_tonic::server_reflection_response::MessageResponse;
  use reflection_tonic::ServerReflectionRequest;

  fn free_port() -> u16 {
      std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
  }

  /// Pin the link rows the boot needs: the hyper FALLBACK WebServer + JSON converter, the
  /// leaf-grpc GrpcDispatch + DefaultGrpcStatusMapper FALLBACK, the echo controller (the app
  /// service to reflect), AND the two reflection controllers (so their bean rows LINK; the
  /// #[conditional] guard — not the link — is what gates them on/off).
  fn force_link() {
      let _ = std::any::TypeId::of::<leaf_web_hyper::HyperServerAutoConfig>();
      let _ = std::any::TypeId::of::<leaf_serde::JsonConverterConfig>();
      let _ = std::any::TypeId::of::<leaf_grpc::GrpcDispatch>();
      let _ = std::any::TypeId::of::<leaf_grpc::DefaultGrpcStatusMapper>();
      let _ = std::any::TypeId::of::<leaf_grpc::reflection::ReflectionV1>();
      let _ = std::any::TypeId::of::<leaf_grpc::reflection::ReflectionV1alpha>();
  }

  // The echo controller (the app service that becomes reflectable) — its build.rs FDS row
  // is in the slice, and force_link must pin its bean rows too.
  mod echo_controller;

  async fn boot(args: Vec<String>) -> (u16, leaf_boot::RunningApp) {
      force_link();
      let port = free_port();
      let spawner: Arc<dyn leaf_core::Spawner> = Arc::new(leaf_tokio::TokioExecutionFacility::new());
      let mut all = vec![format!("--leaf.web.server.port={port}")];
      all.extend(args);
      let app = Application::new()
          .with_name("grpc-reflection")
          .with_spawner(spawner)
          .with_drain_sleeper(|d| Box::pin(tokio::time::sleep(d)))
          .run(SealInputs::new().with_args(all), RunOverlay::none())
          .await
          .expect("the grpc-reflection app boots to Ready");
      (port, app)
  }

  async fn wait_until_up(port: u16) {
      for _ in 0..400 {
          if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
              return;
          }
          tokio::time::sleep(std::time::Duration::from_millis(10)).await;
      }
      panic!("the grpc-reflection server never came up");
  }

  async fn refl_client(port: u16) -> ServerReflectionClient<tonic::transport::Channel> {
      let channel = tonic::transport::Channel::from_shared(format!("http://127.0.0.1:{port}"))
          .unwrap()
          .connect()
          .await
          .expect("tonic connects to the leaf reflection server");
      ServerReflectionClient::new(channel)
  }

  /// Drive ONE ServerReflectionInfo round-trip: send one request, read one response.
  async fn one_round(
      c: &mut ServerReflectionClient<tonic::transport::Channel>,
      req: ServerReflectionRequest,
  ) -> Result<MessageResponse, tonic::Status> {
      let outbound = futures::stream::iter(vec![req]);
      let mut stream = c.server_reflection_info(tonic::Request::new(outbound)).await?.into_inner();
      let resp = stream
          .next()
          .await
          .expect("a reflection response frame")?;
      Ok(resp.message_response.expect("a message_response variant"))
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn reflection_is_unimplemented_when_disabled_by_default() {
      // No leaf.grpc.reflection.enabled -> the #[conditional] controllers (struct + routes)
      // do not register -> the ServerReflectionInfo route is absent.
      let (port, running) = boot(vec![]).await;
      wait_until_up(port).await;
      let mut c = refl_client(port).await;

      let err = one_round(
          &mut c,
          ServerReflectionRequest {
              host: String::new(),
              message_request: Some(MessageRequest::ListServices(String::new())),
          },
      )
      .await
      .expect_err("a reflection call with reflection disabled is rejected");
      assert_eq!(
          err.code(),
          tonic::Code::Unimplemented,
          "reflection OFF by default -> the unknown-method Unimplemented path, got {err:?}"
      );

      let _ = running.shutdown().await;
  }
  ```

  Reuse `tests/serves_grpc.rs`'s `echo_controller` module by copying it as `crates/leaf-grpc/tests/echo_controller.rs` is already a shared module (it exists); the `mod echo_controller;` declaration pulls it in.

- [ ] **Step 2: Run it; expect it to pass already (the gate is the Stage-3 condition).**

  ```
  cargo test -p leaf-grpc --test reflection_over_h2 reflection_is_unimplemented_when_disabled_by_default
  ```

  Expected: `test reflection_is_unimplemented_when_disabled_by_default ... ok`. If instead the call SUCCEEDS, the Stage-3 condition propagation is not gating the `GrpcRoute` beans — fix Stage 3, do not weaken this assertion.

- [ ] **Step 3: Commit.**

  ```
  git add crates/leaf-grpc/tests/reflection_over_h2.rs
  git commit -m "leaf-grpc: prove reflection is Unimplemented when disabled by default

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 4.3: The headline ON proof — list_services + file_containing_symbol over real H2

Flip `--leaf.grpc.reflection.enabled=true` and the SAME reflection client now drives `ServerReflectionInfo` successfully: `list_services` lists the app's `echo.Echo` service, and `file_containing_symbol("echo.EchoRequest")` returns the defining `echo.proto` FileDescriptorProto bytes plus its dependency closure. Symbols are the gRPC wire FQNs from the FDS, never Rust type names.

**Files**
- Modify: `crates/leaf-grpc/tests/reflection_over_h2.rs`

- [ ] **Step 1: Write the failing ON test.**

  Append to `reflection_over_h2.rs`:

  ```rust
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn reflection_lists_services_and_returns_descriptors_when_enabled() {
      let (port, running) =
          boot(vec!["--leaf.grpc.reflection.enabled=true".to_string()]).await;
      wait_until_up(port).await;
      let mut c = refl_client(port).await;

      // 1. list_services -> the app's echo.Echo service appears (the gRPC wire FQN, sourced
      //    from the FDS the echo.proto's leaf-grpc-build run contributed to the slice).
      let resp = one_round(
          &mut c,
          ServerReflectionRequest {
              host: String::new(),
              message_request: Some(MessageRequest::ListServices(String::new())),
          },
      )
      .await
      .expect("list_services succeeds with reflection enabled");
      let names: Vec<String> = match resp {
          MessageResponse::ListServicesResponse(r) => {
              r.service.into_iter().map(|s| s.name).collect()
          }
          other => panic!("expected ListServicesResponse, got {other:?}"),
      };
      assert!(
          names.iter().any(|n| n == "echo.Echo"),
          "list_services includes the app catalog service echo.Echo, got {names:?}"
      );

      // 2. file_containing_symbol("echo.EchoRequest") -> the defining echo.proto descriptor
      //    bytes (plus its dependency closure). The symbol is the FDS wire FQN, NOT a Rust name.
      let resp = one_round(
          &mut c,
          ServerReflectionRequest {
              host: String::new(),
              message_request: Some(MessageRequest::FileContainingSymbol(
                  "echo.EchoRequest".to_string(),
              )),
          },
      )
      .await
      .expect("file_containing_symbol succeeds");
      let fds_bytes: Vec<Vec<u8>> = match resp {
          MessageResponse::FileDescriptorResponse(r) => r.file_descriptor_proto,
          other => panic!("expected FileDescriptorResponse, got {other:?}"),
      };
      assert!(
          !fds_bytes.is_empty(),
          "the file + its dependency closure came back as descriptor bytes"
      );
      // Decode each returned descriptor and assert the defining file is among them: a file
      // whose message_type contains EchoRequest. (prost_types is reached via leaf_grpc's prost
      // re-export's sibling; the test names prost/prost-types directly as dev-deps.)
      let defines_echo_request = fds_bytes.iter().any(|bytes| {
          let file = <prost_types::FileDescriptorProto as prost::Message>::decode(&bytes[..])
              .expect("a returned descriptor decodes as a FileDescriptorProto");
          file.message_type.iter().any(|m| m.name() == "EchoRequest")
      });
      assert!(
          defines_echo_request,
          "the descriptor defining EchoRequest is in the returned closure"
      );

      let _ = running.shutdown().await;
  }
  ```

  Add `prost-types` to leaf-grpc's `[dev-dependencies]` in `crates/leaf-grpc/Cargo.toml` (next to the existing `prost.workspace = true`):

  ```toml
  prost-types.workspace = true
  ```

- [ ] **Step 2: Run it; it fails if anything in the ON path is unwired.**

  ```
  cargo test -p leaf-grpc --test reflection_over_h2 reflection_lists_services_and_returns_descriptors_when_enabled
  ```

  Expected on a correctly-wired Stage 1-3: `test reflection_lists_services_and_returns_descriptors_when_enabled ... ok`. A failure here surfaces a real gap (e.g. the echo FDS row missing from `REFLECTED_FILE_DESCRIPTOR_SETS`, or the index not closing over deps) — debug Stage 1-3, do not relax the assertions.

- [ ] **Step 3: Run the whole reflection_over_h2 suite together (OFF + ON, no port/state cross-talk).**

  ```
  cargo test -p leaf-grpc --test reflection_over_h2
  ```

  Expected: `test result: ok. 3 passed; 0 failed` (the stub check + OFF + ON).

- [ ] **Step 4: Commit.**

  ```
  git add crates/leaf-grpc/Cargo.toml crates/leaf-grpc/tests/reflection_over_h2.rs
  git commit -m "leaf-grpc: headline H2 reflection proof (list_services + file_containing_symbol)

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 4.4: The storefront becomes reflectable

The storefront builds a `#[grpc_controller]` (so its build.rs calls `leaf_grpc_build::compile` → its `storefront.catalog` FDS lands in the slice automatically — no app wiring). Booting it with `--leaf.grpc.reflection.enabled=true` makes `storefront.catalog.Catalog` discoverable via the reflection client. First add the dev-only tonic reflection client stub to the storefront build.

**Files**
- Modify: `examples/storefront/build.rs`
- Modify: `examples/storefront/Cargo.toml`
- Create: `examples/storefront/tests/grpc_reflection.rs`

- [ ] **Step 1: Add the reflection client stub to the storefront build script.**

  The storefront does not ship reflection.proto (leaf-grpc does); the test peer client comes from the leaf-grpc reflection proto. The simplest dogfood-faithful path: have the storefront `build.rs` generate the reflection client from the proto leaf-grpc ships. Add a `[build-dependencies]` path to reach it via a vendored copy under the storefront for the client gen only. In `examples/storefront/build.rs`, inside the existing `if CARGO_FEATURE_GRPC` block, after the catalog tonic block, add:

  ```rust
          // The dev-test reflection CLIENT stub: tonic generates grpc.reflection.v1's client
          // from leaf-grpc's SHIPPED reflection.proto (vendored beside this build.rs as
          // proto/reflection.proto so the storefront build needs no path into the crate src).
          // Client-only, into $OUT_DIR/tonic/grpc.reflection.v1.rs — the same separate-dir
          // pattern the catalog client uses. Reflection itself is SERVED by leaf-grpc's
          // dogfooded #[grpc_controller]s; the storefront adds zero reflection server code.
          let refl_fds = protox::compile(["proto/reflection.proto"], ["proto"])
              .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
          tonic_build::configure()
              .build_server(false)
              .build_client(true)
              .out_dir(&tonic_out)
              .compile_fds(refl_fds)
              .map_err(|e| std::io::Error::other(e.to_string()))?;
          println!("cargo:rerun-if-changed=proto/reflection.proto");
  ```

  Vendor leaf-grpc's `proto/reflection.proto` to `examples/storefront/proto/reflection.proto` (copy the shipped file):

  ```
  cp crates/leaf-grpc/proto/reflection.proto examples/storefront/proto/reflection.proto
  ```

- [ ] **Step 2: Write the failing storefront reflection test.**

  Create `examples/storefront/tests/grpc_reflection.rs`:

  ```rust
  //! The STOREFRONT REFLECTABLE PROOF (Stage 4): the umbrella-only storefront, with the
  //! `grpc` capability + `--leaf.grpc.reflection.enabled=true`, is discoverable over real H2
  //! by a tonic reflection client — `storefront.catalog.Catalog` appears in list_services and
  //! its descriptors come back. ZERO storefront reflection code: the catalog #[grpc_controller]
  //! contributes its FDS to leaf-grpc's discovery slice automatically; leaf-grpc serves
  //! reflection via its dogfooded #[grpc_controller]s gated on the property.
  #![cfg(feature = "grpc")]

  extern crate leaf as leaf_grpc;
  extern crate leaf as leaf_web;

  use storefront as _;

  use std::time::Duration;

  // tonic's reflection CLIENT for leaf-grpc's grpc.reflection.v1 reflection.proto, generated
  // by the storefront build.rs into $OUT_DIR/tonic/grpc.reflection.v1.rs (dev/build-only).
  pub mod reflection_tonic {
      include!(concat!(env!("OUT_DIR"), "/tonic/grpc.reflection.v1.rs"));
  }
  use futures::StreamExt;
  use reflection_tonic::server_reflection_client::ServerReflectionClient;
  use reflection_tonic::server_reflection_request::MessageRequest;
  use reflection_tonic::server_reflection_response::MessageResponse;
  use reflection_tonic::ServerReflectionRequest;

  fn free_port() -> u16 {
      std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
  }

  async fn wait_until_up(port: u16) {
      for _ in 0..400 {
          if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
              return;
          }
          tokio::time::sleep(Duration::from_millis(10)).await;
      }
      panic!("the storefront grpc-reflection server never came up");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn the_storefront_catalog_is_discoverable_via_reflection() {
      let port = free_port();
      let running = leaf::bootstrap("storefront")
          .run(
              leaf::RunInputs::new()
                  .with_args([
                      format!("--leaf.web.server.port={port}"),
                      "--app.name=storefront".to_string(),
                      "--leaf.grpc.reflection.enabled=true".to_string(),
                  ])
                  .into(),
              leaf::boot::RunOverlay::none(),
          )
          .await
          .expect("the storefront boots to Ready");

      wait_until_up(port).await;

      let channel = tonic::transport::Channel::from_shared(format!("http://127.0.0.1:{port}"))
          .unwrap()
          .connect()
          .await
          .expect("tonic connects to the storefront reflection server");
      let mut c = ServerReflectionClient::new(channel);

      // list_services over the storefront -> storefront.catalog.Catalog appears (the wire FQN
      // from the catalog.proto FDS the #[grpc_controller] build contributed automatically).
      let outbound = futures::stream::iter(vec![ServerReflectionRequest {
          host: String::new(),
          message_request: Some(MessageRequest::ListServices(String::new())),
      }]);
      let mut stream = c
          .server_reflection_info(tonic::Request::new(outbound))
          .await
          .expect("ServerReflectionInfo is served when reflection is enabled")
          .into_inner();
      let resp = stream
          .next()
          .await
          .expect("a reflection response frame")
          .expect("ok response")
          .message_response
          .expect("a message_response variant");
      let names: Vec<String> = match resp {
          MessageResponse::ListServicesResponse(r) => r.service.into_iter().map(|s| s.name).collect(),
          other => panic!("expected ListServicesResponse, got {other:?}"),
      };
      assert!(
          names.iter().any(|n| n == "storefront.catalog.Catalog"),
          "the storefront catalog service is reflectable, got {names:?}"
      );

      let report = running.shutdown().await;
      assert_eq!(report.run_state, leaf::core::RunState::Closed, "the storefront drained cleanly");
  }
  ```

- [ ] **Step 3: Run it; it fails to compile until the build emits the stub, then passes once reflection is wired.**

  ```
  cargo test -p storefront --features grpc --test grpc_reflection
  ```

  Expected: `test the_storefront_catalog_is_discoverable_via_reflection ... ok`. If `storefront.catalog.Catalog` is missing, the FDS auto-registration row (Stage 1) is not reaching the slice from the storefront's compile — debug Stage 1.

- [ ] **Step 4: Commit.**

  ```
  git add examples/storefront/build.rs examples/storefront/Cargo.toml examples/storefront/proto/reflection.proto examples/storefront/tests/grpc_reflection.rs
  git commit -m "storefront: dogfood gRPC reflection — catalog discoverable over real H2

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```

---

### Task 4.5: Full force-clean gate (test + clippy + doc)

A cached `cargo` run re-emits no warnings, so the gate force-cleans first (per the project rule). The whole workspace test suite (the existing ~1733 tests + the new reflection tests) stays green; clippy is clean with `-D warnings`; doc builds clean. leaf-grpc stays backend-free (only leaf-web-hyper names hyper/h2) and the index keys on FDS wire symbols, not Rust type names — both already enforced by the tests above.

**Files** (none — verification only)

- [ ] **Step 1: Force-clean, then run the entire workspace test suite with all features.**

  ```
  cargo clean
  cargo test --workspace --all-features
  ```

  Expected: every crate's `test result: ok` with `0 failed`; the storefront `--features grpc` reflection test and the leaf-grpc `reflection_over_h2` tests pass; the aggregate test count is the prior ~1733 plus the new reflection tests, with NO regressions.

- [ ] **Step 2: Confirm leaf-grpc names no backend (the backend-free constraint).**

  ```
  cargo tree -p leaf-grpc --no-default-features -e normal | grep -E '^\S*(hyper|h2|tonic)\b' || echo "leaf-grpc names no hyper/h2/tonic in its normal dep graph"
  ```

  Expected: `leaf-grpc names no hyper/h2/tonic in its normal dep graph` (tonic/tonic-build/protox appear only under build/dev, never normal).

- [ ] **Step 3: Force-clean clippy across the workspace with warnings denied.**

  ```
  cargo clean
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  ```

  Expected: `Finished` with no warnings or errors (any generated reflection items carry the project's `#[allow]` naming-lint convention from Stage 1-3, so rust-analyzer-visible lints stay silent too).

- [ ] **Step 4: Force-clean doc build (no broken intra-doc links).**

  ```
  cargo clean
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
  ```

  Expected: `Finished` with zero rustdoc warnings (the reflection index + controllers + the `REFLECTED_FILE_DESCRIPTOR_SETS` slice doc-comments resolve).

- [ ] **Step 5: Commit the gate-pass marker (if any doc/lint touch-ups were needed; otherwise skip).**

  Only if Steps 1-4 required a fix:

  ```
  git add -A
  git commit -m "leaf-grpc: green force-clean gate for gRPC server reflection (test + clippy + doc)

  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
  ```
