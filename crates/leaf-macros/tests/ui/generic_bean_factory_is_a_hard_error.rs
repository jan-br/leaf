//! A generic `#[bean]` factory must hard-error: a generic factory has no single
//! concrete product type, so it cannot mint a const row.

use leaf_macros::bean;

struct Wrap<T> {
    inner: T,
}

#[bean]
fn make<T: Default>() -> Wrap<T> {
    Wrap { inner: T::default() }
}

fn main() {}
