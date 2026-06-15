//! `#[cacheable]` with no cache name is a Tier-0 `compile_error!` — at least one
//! cache name is required.

use leaf_macros::cacheable;

#[cacheable]
fn find() {}

fn main() {}
