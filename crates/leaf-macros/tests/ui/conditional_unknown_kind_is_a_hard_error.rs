//! An unknown `#[conditional(...)]` leaf kind is a loud `compile_error!` (the
//! condition DSL vocabulary is closed: on_property/on_bean/on_class/all/any/not).

use leaf_macros::conditional;

#[conditional(on_quux("x"))]
struct Thing;

fn main() {}
