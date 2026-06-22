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
