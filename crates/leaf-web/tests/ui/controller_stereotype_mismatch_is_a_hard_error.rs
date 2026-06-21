//! A controller's `struct` and its request-mapping `impl` must use the SAME stereotype.
//! Rust splits a struct from its `impl`, so `#[controller]`/`#[rest_controller]` are
//! dual-form (written on both halves). The `ControllerKind` guard makes the halves agree:
//! the struct emits its `@ResponseBody` policy as a marker const, and the impl asserts its
//! own policy matches — so this mismatch is a hard compile error, not a silent disagreement.

use leaf_macros::{controller, rest_controller};

// The struct declares the @ResponseBody (serialize-via-converter) policy …
#[rest_controller]
struct Api;

// … but the impl declares the plain @Controller (IntoResponse) policy. Mismatch → hard error.
#[controller]
impl Api {}

fn main() {}
