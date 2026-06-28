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
///
/// "Self-contained" means each package's set carries not only the files declaring that
/// package but the TRANSITIVE IMPORT CLOSURE of those files (everything reachable through
/// `FileDescriptorProto::dependency`, e.g. the `google/api/*` AIP annotations a service
/// imports, or cross-package message files). gRPC reflection resolves a service by walking
/// its descriptors' imports by filename; if an imported file is absent from the reflected
/// set, the client fails with `File not found: google/api/annotations.proto` and the service
/// never resolves. The closure is emitted dependencies-first (post-order) so consumers that
/// assume topological order are satisfied; clients that resolve by filename don't care.
#[must_use]
fn group_fds_by_package(fds: &::prost_types::FileDescriptorSet) -> BTreeMap<String, Vec<u8>> {
    use ::prost::Message;
    use std::collections::HashMap;

    // Index every file by its proto filename so imports (which name files, not packages)
    // can be resolved across the whole input set.
    let by_filename: HashMap<&str, &::prost_types::FileDescriptorProto> = fds
        .file
        .iter()
        .map(|f| (f.name(), f))
        .collect();

    // Post-order DFS from `start`, appending each file AFTER its dependencies. `visited`
    // dedups across the whole closure; `out` preserves dependencies-first order.
    fn collect_closure<'a>(
        start: &'a ::prost_types::FileDescriptorProto,
        by_filename: &HashMap<&str, &'a ::prost_types::FileDescriptorProto>,
        visited: &mut std::collections::HashSet<&'a str>,
        out: &mut Vec<&'a ::prost_types::FileDescriptorProto>,
    ) {
        if !visited.insert(start.name()) {
            return;
        }
        for dep in &start.dependency {
            if let Some(dep_file) = by_filename.get(dep.as_str()) {
                collect_closure(dep_file, by_filename, visited, out);
            }
        }
        out.push(start);
    }

    // Which packages exist (each gets its own self-contained set).
    let packages: std::collections::BTreeSet<String> = fds
        .file
        .iter()
        .map(|f| f.package().to_string())
        .collect();

    packages
        .into_iter()
        .map(|pkg| {
            let mut visited = std::collections::HashSet::new();
            let mut files = Vec::new();
            // Seed from every file declaring this package, then pull in its import closure.
            for file in fds.file.iter().filter(|f| f.package() == pkg) {
                collect_closure(file, &by_filename, &mut visited, &mut files);
            }
            let set = ::prost_types::FileDescriptorSet {
                file: files.into_iter().cloned().collect(),
            };
            (pkg, set.encode_to_vec())
        })
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

    fn file_importing(pkg: &str, name: &str, deps: &[&str]) -> FileDescriptorProto {
        FileDescriptorProto {
            name: Some(name.to_string()),
            package: Some(pkg.to_string()),
            dependency: deps.iter().map(|d| (*d).to_string()).collect(),
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
    fn a_packages_set_carries_its_transitive_import_closure() {
        use ::prost::Message;
        // A service in `example.v1` imports `google/api/annotations.proto`, which in turn
        // imports `google/protobuf/descriptor.proto`. The reflected set for `example.v1` must
        // carry ALL THREE files (its own + the full closure), else a reflection client fails
        // with "File not found: google/api/annotations.proto".
        let fds = FileDescriptorSet {
            file: vec![
                file_importing("example.v1", "service.proto", &["google/api/annotations.proto"]),
                file_importing(
                    "google.api",
                    "google/api/annotations.proto",
                    &["google/protobuf/descriptor.proto"],
                ),
                file("google.protobuf", "google/protobuf/descriptor.proto"),
            ],
        };
        let groups = group_fds_by_package(&fds);
        let example = groups.get("example.v1").expect("example.v1 group present");
        let decoded =
            FileDescriptorSet::decode(example.as_slice()).expect("re-encoded set decodes");
        let names: Vec<&str> = decoded.file.iter().map(|f| f.name()).collect();
        assert!(
            names.contains(&"service.proto")
                && names.contains(&"google/api/annotations.proto")
                && names.contains(&"google/protobuf/descriptor.proto"),
            "example.v1's set must carry its full import closure, got: {names:?}",
        );
        // Dependencies-first order: a file appears only after everything it imports.
        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(pos("google/protobuf/descriptor.proto") < pos("google/api/annotations.proto"));
        assert!(pos("google/api/annotations.proto") < pos("service.proto"));
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
