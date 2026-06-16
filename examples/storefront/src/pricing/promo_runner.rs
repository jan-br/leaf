use leaf::prelude::*;

use crate::pricing::discount_policy::DiscountPolicy;

/// A `#[runner]` gated by the SAME `#[conditional]` as [`DiscountPolicy`] — a feature
/// toggle: the whole runner (and the `DiscountPolicy` it injects) exists ONLY when the app
/// is run with `--pricing.discounts.enabled=true`, where it announces the active promo.
/// When the flag is unset both are absent and this never fires.
#[runner]
#[conditional(on_property("pricing.discounts.enabled", having_value = "true"))]
pub struct PromoRunner {
    discount: Ref<DiscountPolicy>,
}

#[async_impl]
impl Runner for PromoRunner {
    async fn run(&self, _args: &leaf::core::ApplicationArguments) -> Result<(), LeafError> {
        println!("promo active: discounts enabled ({}c off a 2598c order)", self.discount.discount_cents(2598));
        Ok(())
    }
}
