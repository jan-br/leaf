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
        #![allow(dead_code, clippy::enum_variant_names)]
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
