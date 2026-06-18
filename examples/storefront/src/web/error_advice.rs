use leaf::http::StatusCode;
use leaf::prelude::*;

/// A `#[control_advice]` (Spring's `@ControllerAdvice` + `@ExceptionHandler`): the global
/// error-mapping bean. The server COLLECTS it by the `dyn ControlAdvice` view and consults
/// it (ordered, first-`Some`-wins) when a handler returns `Err`.
///
/// It maps the storefront's unknown-product error to `404`: `CatalogService::price_of`
/// raises a `ConstructionFailed` `LeafError` for an unknown SKU, which the framework
/// default would map to `500` — this advice claims it first and maps it to the
/// conventional `404 Not Found` (so the mapping is provably the advice's, not the floor).
#[control_advice]
#[derive(Debug)]
pub struct StorefrontErrors;

#[control_advice]
impl StorefrontErrors {
    /// Map an unknown-product lookup failure to `404`. The unknown-SKU path raises a
    /// `ConstructionFailed` (the `price_of` miss); everything else is declined (`None`) so
    /// the framework default floor (or another advice) handles it.
    #[exception_handler]
    fn unknown_product(&self, err: &LeafError, _req: &Request) -> Option<Response> {
        match err.kind {
            leaf::core::ErrorKind::ConstructionFailed => Some(Response::new(StatusCode::NOT_FOUND)),
            _ => None,
        }
    }
}
