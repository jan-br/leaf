//! `#[scheduled]` with no trigger key is a Tier-0 `compile_error!` — exactly one of
//! `cron`/`fixed_rate`/`fixed_delay` is required.

use leaf_macros::scheduled;

#[scheduled]
fn cleanup() {}

fn main() {}
