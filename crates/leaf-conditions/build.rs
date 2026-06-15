//! Capture the compiling toolchain version so `OnRustVersion` (a ConstFold-tier
//! member) can be decided at BUILD and lowered to `CondExpr::Const(bool)`.
//!
//! Emits `LEAF_RUSTC_VERSION` (read via `option_env!` in `src/rustversion.rs`).
//! Falls back silently if `rustc -V` cannot be invoked — the condition then
//! fails open, exactly as documented.

use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    if let Ok(out) = Command::new(rustc).arg("-V").output()
        && out.status.success()
        && let Ok(v) = String::from_utf8(out.stdout)
    {
        println!("cargo:rustc-env=LEAF_RUSTC_VERSION={}", v.trim());
    }
    // Re-run only if the build script itself changes (version is toolchain-bound).
    println!("cargo:rerun-if-changed=build.rs");
}
