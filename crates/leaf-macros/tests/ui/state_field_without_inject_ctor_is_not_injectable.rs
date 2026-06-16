//! A state-holding `#[component]` with NO `constructor = <path>` arg must fail to
//! compile LOUDLY: the field-default path routes every field through `Injectable`
//! (trait dispatch, never a name strip), and a plain state field (`AtomicU64`) is
//! deliberately NOT `Injectable` — so the bean does not satisfy the field-default
//! contract. The remediation is `#[component(constructor = CounterService::new)]` (a
//! referenced `new()` that seeds state and injects only `Ref<…>` collaborators). The
//! design's "a state-holding `#[component]` fails to resolve loudly" guarantee (T4).

use leaf_macros::component;
use std::sync::atomic::AtomicU64;

#[component]
struct CounterService {
    hits: AtomicU64,
}

fn main() {}
