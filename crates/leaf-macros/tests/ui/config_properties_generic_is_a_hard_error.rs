//! A generic `#[config_properties]` target is a Tier-0 `compile_error!` (a generic
//! config target has no single concrete bind schema).

use leaf_macros::config_properties;

#[derive(Default)]
#[config_properties(prefix = "app")]
struct Props<T> {
    inner: T,
}

fn main() {}
