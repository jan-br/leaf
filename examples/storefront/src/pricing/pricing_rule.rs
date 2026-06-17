use leaf::prelude::*;

/// `#[injectable]` makes `dyn PricingRule` an injectable VIEW: a bean providing it is
/// resolvable as `Ref<dyn PricingRule>` AND collectible as `Vec<Ref<dyn PricingRule>>`
/// (Spring's `List<PricingRule>` — ALL providers, ordered). A NON-manager application
/// trait, proving collection injection is a general primitive, not a manager special-case.
#[injectable]
pub trait PricingRule: Send + Sync + 'static {
    /// The surcharge (cents) this rule adds to an order total.
    fn surcharge_cents(&self) -> i64;

    /// A short label for the startup summary.
    fn label(&self) -> &'static str;
}

/// A flat weekend surcharge rule.
#[derive(Debug, Default)]
pub struct WeekendSurcharge;
impl PricingRule for WeekendSurcharge {
    fn surcharge_cents(&self) -> i64 {
        150
    }
    fn label(&self) -> &'static str {
        "weekend"
    }
}

/// A small handling-fee rule.
#[derive(Debug, Default)]
pub struct HandlingFee;
impl PricingRule for HandlingFee {
    fn surcharge_cents(&self) -> i64 {
        75
    }
    fn label(&self) -> &'static str {
        "handling"
    }
}

/// A `#[configuration]` registering TWO beans that BOTH provide the `dyn PricingRule`
/// view (a struct `#[component]` cannot declare a `provides` view, so the views are
/// contributed via `#[bean(provides = "dyn PricingRule")]` methods — the idiomatic way
/// to expose multiple providers of one view). Collection injection then gathers BOTH.
#[configuration]
pub struct PricingRules;

#[configuration]
impl PricingRules {
    /// The weekend-surcharge rule, exposed as `dyn PricingRule`.
    #[bean(name = "weekendSurcharge", provides = "dyn crate::pricing::pricing_rule::PricingRule")]
    fn weekend_surcharge(&self) -> WeekendSurcharge {
        WeekendSurcharge
    }

    /// The handling-fee rule, exposed as `dyn PricingRule`.
    #[bean(name = "handlingFee", provides = "dyn crate::pricing::pricing_rule::PricingRule")]
    fn handling_fee(&self) -> HandlingFee {
        HandlingFee
    }
}
