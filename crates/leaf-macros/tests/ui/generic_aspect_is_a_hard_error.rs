//! A generic `#[aspect]` must be a Tier-0 `compile_error!` (a generic aspect has no
//! single concrete `ContractId`). The aspect bean itself can't be registered, so
//! the `#[component]` generic guard fires first with the `register_component!`
//! remediation (which `register_proxy!` aliases for the proxyable case).

use leaf_macros::aspect;

#[aspect]
struct Generic<T> {
    inner: T,
}

fn main() {}
