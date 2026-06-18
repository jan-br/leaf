use std::sync::atomic::{AtomicI64, Ordering};

use leaf::prelude::*;

/// The process-wide access counter the [`AccessLogFilter`] advances on every request and
/// the catalog controller's `/_access_count` probe reads. A plain atomic (the filter is a
/// stateless bean; the count is observable demo state, like the repository's saved-count)
/// — so the integration test can prove the around-advice ran.
static ACCESS_COUNT: AtomicI64 = AtomicI64::new(0);

/// The current request count (read by the `/_access_count` probe).
#[must_use]
pub fn access_count() -> i64 {
    ACCESS_COUNT.load(Ordering::SeqCst)
}

/// The access-log [`WebFilter`] — the around-advice seam (Spring's servlet `Filter` /
/// `HandlerInterceptor`). It logs + counts each request, then continues the chain via
/// `Next::run` (a pass-through filter — it never short-circuits). Written with
/// `#[async_impl]` (no hand-rolled `BoxFuture`).
#[derive(Debug, Default)]
pub struct AccessLogFilter;

#[async_impl]
impl WebFilter for AccessLogFilter {
    async fn filter(&self, req: Request, next: Next<'_>) -> Result<Response, LeafError> {
        let n = ACCESS_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        println!("[access-log] #{n} {} {}", req.method(), req.path());
        next.run(req).await
    }
}

/// The `#[configuration]` that PUBLISHES the filter as the `dyn ::leaf_web::WebFilter` view
/// the server's `Vec<Ref<dyn WebFilter>>` collection injection gathers. A struct
/// `#[component]` cannot itself declare a `provides` dyn-view, so the view is contributed
/// via a `#[bean(provides = "dyn …")]` factory — the SAME idiom `leaf-serde`'s JSON
/// converter and the storefront's `PricingRules` use (the dogfooded registration, NOT a
/// hand-rolled `Provider`).
#[configuration]
pub struct WebFilters;

#[configuration]
impl WebFilters {
    /// Contribute the access-log filter as `dyn WebFilter`.
    #[bean(name = "accessLogFilter", provides = "dyn ::leaf_web::WebFilter")]
    fn access_log_filter(&self) -> AccessLogFilter {
        AccessLogFilter
    }
}
