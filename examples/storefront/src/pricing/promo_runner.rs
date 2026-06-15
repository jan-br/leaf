use std::sync::atomic::{AtomicUsize, Ordering};

use leaf::prelude::*;

use crate::pricing::discount_policy::DiscountPolicy;

/// Incremented when the (conditional) promo runner fires — lets the test prove the gated
/// runner activated only when enabled.
pub static PROMO_FIRED: AtomicUsize = AtomicUsize::new(0);

/// A `#[runner]` gated by the SAME `#[conditional]` as [`DiscountPolicy`] — a feature
/// toggle: the whole runner (and the `DiscountPolicy` it injects) exists ONLY when the app
/// is run with `--pricing.discounts.enabled=true`, where it announces the active promo.
/// When the flag is unset both are absent and this never fires.
#[runner]
#[conditional(on_property("pricing.discounts.enabled", having_value = "true"))]
pub struct PromoRunner {
    discount: Ref<DiscountPolicy>,
}

impl PromoRunner {
    fn new(discount: Ref<DiscountPolicy>) -> Self {
        PromoRunner { discount }
    }
}

impl Runner for PromoRunner {
    fn run<'a>(
        &'a self,
        _args: &'a leaf::core::ApplicationArguments,
    ) -> leaf::core::BoxFuture<'a, Result<(), LeafError>> {
        Box::pin(async move {
            println!("promo active: discounts enabled ({}c off a 2598c order)", self.discount.discount_cents(2598));
            PROMO_FIRED.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
