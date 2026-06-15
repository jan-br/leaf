//! A declarative concern annotation (`#[transactional]`) applied STANDALONE — not on
//! a method inside an `#[advisable] impl` block — is a Tier-0 `compile_error!`: a
//! method-position attribute alone cannot emit the sibling `ADVISOR_PAIRINGS` row, so
//! it steers to the impl-block form (the same constraint `#[bean]`/`#[advice]` hit).

use leaf_macros::transactional;

struct Svc;

#[transactional(manager = Mgr)]
fn record(_svc: &Svc) -> Result<i64, ()> {
    Ok(1)
}

fn main() {}
