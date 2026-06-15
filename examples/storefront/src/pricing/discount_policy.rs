use leaf::prelude::*;

/// A `#[component]` gated by `#[conditional]`: ABSENT from the registry unless the app
/// is run with `--pricing.discounts.enabled=true`. Demonstrates conditional bean gating.
#[component]
#[conditional(on_property("pricing.discounts.enabled", having_value = "true"))]
#[derive(Debug)]
pub struct DiscountPolicy;

impl DiscountPolicy {
    fn new() -> Self {
        DiscountPolicy
    }

    /// A flat 10% discount on the order total (cents).
    pub fn discount_cents(&self, total_cents: i64) -> i64 {
        total_cents / 10
    }
}
