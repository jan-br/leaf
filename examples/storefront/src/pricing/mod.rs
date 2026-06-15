//! Pricing — a conditionally-gated feature (a feature toggle). The
//! [`discount_policy::DiscountPolicy`] bean and the [`promo_runner::PromoRunner`] that uses
//! it exist ONLY when the app is run with `--pricing.discounts.enabled=true`.
pub mod discount_policy;
pub mod promo_runner;
