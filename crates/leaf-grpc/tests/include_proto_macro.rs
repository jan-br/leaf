//! `include_proto!` must expand to an `include!` of `$OUT_DIR/<pkg>.rs`. We point
//! OUT_DIR-like inclusion at a fixture written here at test time, proving the macro
//! splices a generated file into a module.

// The macro builds the path from env!("OUT_DIR"); the build.rs writes `fixture.rs`
// into OUT_DIR so the include resolves.
mod fixture {
    leaf_grpc::include_proto!("fixture");
}

#[test]
fn include_proto_splices_the_generated_module() {
    // `fixture.rs` (written by build.rs) declares `pub const MARKER: u32 = 42;`.
    assert_eq!(fixture::MARKER, 42);
}
