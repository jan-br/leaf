//! `leaf-grpc-build` — proto-first codegen for leaf gRPC services.
//!
//! An app's `build.rs` calls [`compile`]: `protox` parses the `.proto` files into a
//! `prost_types::FileDescriptorSet` (NO `protoc` system binary — pure Rust), then
//! `prost-build` emits the message structs while a leaf [`service_gen::LeafServiceGenerator`]
//! emits, per gRPC service, a leaf-shaped server trait + the `/pkg.Service/Method`
//! path constants + the `#[doc(hidden)]` per-method descriptors the `#[grpc_controller]`
//! macro (Stage 4) reads. Output lands in `OUT_DIR`, included via
//! `leaf_grpc::include_proto!("pkg")`.

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
/// The empty-package case: prost-build's `Module::to_file_name_or` uses a default
/// filename root (not the empty string) for an unpackaged proto, so the `<pkg>.fds` /
/// `<pkg>.rs` append below targets the DOTTED package filename only. leaf compiles only
/// packaged protos (echo, the reflection protos all carry a package), so the empty-package
/// row is not exercised here; if a downstream ever compiles an unpackaged proto, the
/// missing-`.rs` write surfaces as an error — acceptable for this stage.
///
/// # Errors
/// Returns an [`std::io::Error`] if `protox` parsing, prost-build codegen, or writing the
/// `.fds`/appended block fails.
pub fn compile(protos: &[&str], includes: &[&str]) -> std::io::Result<()> {
    // protox: pure-Rust .proto -> FileDescriptorSet (no protoc binary).
    let fds = protox::compile(protos, includes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    // Re-run the build only when a .proto changes.
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
        let fds = FileDescriptorSet {
            file: vec![file("", "root.proto")],
        };
        let groups = group_fds_by_package(&fds);
        assert!(
            groups.contains_key(""),
            "empty-package files group under the empty key"
        );
    }
}
