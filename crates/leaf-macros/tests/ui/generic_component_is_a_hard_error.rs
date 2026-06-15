//! A generic `#[component]` must be a Tier-0 `compile_error!` with the
//! `register_component!(Concrete)` hint (a generic type has no single concrete
//! `TypeId`/`ContractId`, so it cannot be a const registry row).

use leaf_macros::component;

#[component]
struct Repo<T> {
    inner: T,
}

fn main() {}
