//! A `#[bean]` with a `self` receiver (the method-on-`@configuration` form, which
//! threads the config instance) is a deferred form — a loud `compile_error!` in v1.

use leaf_macros::bean;

struct DataSource;

struct Config;

impl Config {
    #[bean]
    fn data_source(&self) -> DataSource {
        DataSource
    }
}

fn main() {}
