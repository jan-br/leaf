use leaf::prelude::*;

use crate::pricing::pricing_rule::PricingRule;

/// A `@Component` demonstrating COLLECTION INJECTION: the `rules` field is
/// `Vec<Ref<dyn PricingRule>>`, so the container injects ALL beans providing the
/// `dyn PricingRule` view (Spring's `@Autowired List<PricingRule>`), ordered — purely
/// by trait dispatch on the field type, through the ONE general collection primitive.
/// Zero providers would be an empty Vec (never a wiring error).
#[component]
pub struct PricingService {
    rules: Vec<Ref<dyn PricingRule>>,
}

impl PricingService {
    /// The total surcharge (cents) across every registered pricing rule.
    pub fn total_surcharge_cents(&self) -> i64 {
        self.rules.iter().map(|r| r.surcharge_cents()).sum()
    }

    /// The labels of every registered rule, for the startup summary.
    pub fn labels(&self) -> Vec<&'static str> {
        self.rules.iter().map(|r| r.label()).collect()
    }

    /// How many pricing rules were collected.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}
