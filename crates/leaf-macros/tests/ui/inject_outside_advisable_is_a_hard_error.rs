//! `#[inject]` applied STANDALONE — not on the constructor of an `#[advisable] impl`
//! block — is a Tier-0 `compile_error!`: a method-position attribute alone cannot emit
//! the sibling `SEED_PAIRINGS`/`INJECTION_PLAN_PAIRINGS` rows the constructor wiring
//! needs, so it steers to the impl-block form (the same constraint
//! `#[transactional]`/`#[bean]` hit). `#[inject]` marks the constructor of an
//! `#[advisable]` impl.

use leaf_macros::inject;

struct Svc;

#[inject]
fn new() -> Svc {
    Svc
}

fn main() {}
