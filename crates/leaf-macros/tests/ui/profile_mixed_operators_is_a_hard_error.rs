//! Mixing `&` and `|` without parentheses in a `#[profile(...)]` expression is a
//! Tier-0 `compile_error!` (the fail-fast-on-ambiguity rule).

use leaf_macros::profile;

#[profile("a & b | c")]
struct Thing;

fn main() {}
