//! An unknown `#[component(...)]` argument is a loud `compile_error!` (the
//! attribute schema is closed: only `name` / `scope`).

use leaf_macros::component;

#[component(bogus = "x")]
struct Thing;

fn main() {}
