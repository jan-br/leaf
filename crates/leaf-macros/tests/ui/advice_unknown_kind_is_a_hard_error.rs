//! `#[advice(<unknown>)]` — the advice-kind vocabulary is closed, so an unknown
//! keyword is a Tier-0 `compile_error!`.

use leaf_macros::advice;

#[advice(sideways)]
fn audit() {}

fn main() {}
