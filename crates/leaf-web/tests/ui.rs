//! Compile-fail proofs for the leaf-web macro guards (the half of the macro surface an
//! in-process integration test cannot reach — the hard-error diagnostics themselves).

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    // A `#[rest_controller] struct` + `#[controller] impl` stereotype mismatch must be a
    // HARD compile error via the `ControllerKind` guard: the struct declares the
    // @ResponseBody (serialize) policy while the impl declares the plain @Controller
    // (IntoResponse) policy. Without the guard this is a SILENT policy disagreement (the
    // impl quietly wins); with it, the dual-form halves must agree or it does not compile.
    t.compile_fail("tests/ui/controller_stereotype_mismatch_is_a_hard_error.rs");
}
