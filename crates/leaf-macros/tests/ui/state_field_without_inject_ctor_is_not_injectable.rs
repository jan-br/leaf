//! A state-holding `#[component]` with NO `#[inject]` constructor must fail to
//! compile LOUDLY: the field-default path routes every field through `Injectable`
//! (trait dispatch, never a name strip), and a plain state field (`AtomicU64`) is
//! deliberately NOT `Injectable` — so the bean does not satisfy the field-default
//! contract. The remediation is an `#[inject]` constructor (which seeds state and
//! injects only the `Ref<…>` collaborators). This is the design's "a state-holding
//! `#[component]` now fails to compile/resolve loudly" guarantee (Task 4).

use leaf_macros::component;
use std::sync::atomic::AtomicU64;

#[component]
struct CounterService {
    hits: AtomicU64,
}

fn main() {}
