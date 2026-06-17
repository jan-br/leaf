//! Pricing — a conditionally-gated feature (a feature toggle). The
//! [`discount_policy::DiscountPolicy`] bean and the [`promo_runner::PromoRunner`] that uses
//! it exist ONLY when the app is run with `--pricing.discounts.enabled=true`.
//!
//! [`pricing_rule`] additionally demonstrates COLLECTION INJECTION: an `#[injectable]`
//! `dyn PricingRule` view with TWO provider beans, and a [`pricing_service::PricingService`]
//! that injects `Vec<Ref<dyn PricingRule>>` — ALL providers, ordered (Spring's
//! `List<PricingRule>`).
pub mod discount_policy;
pub mod pricing_rule;
pub mod pricing_service;
pub mod promo_runner;
