use leaf::prelude::*;

/// `@ConfigurationProperties(prefix = "app")` — bound from `app.*` (CLI args / env /
/// config files) by the run pipeline, purely from the macro-emitted bind thunk.
#[config_properties(prefix = "app")]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AppProperties {
    /// `app.name` — the application's display name.
    pub name: String,
    /// `app.workers` — the configured worker count.
    pub workers: u16,
}
