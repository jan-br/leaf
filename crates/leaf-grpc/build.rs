//! Writes a tiny generated-style fixture into OUT_DIR so the `include_proto!` macro
//! test (`tests/include_proto_macro.rs`) has a file to splice.
use std::io::Write;

fn main() {
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let path = std::path::Path::new(&out).join("fixture.rs");
    let mut f = std::fs::File::create(path).expect("create fixture.rs");
    writeln!(f, "pub const MARKER: u32 = 42;").expect("write fixture");
    println!("cargo:rerun-if-changed=build.rs");
}
