//! A BARE `#[bean]` with a `self` receiver (outside a `#[configuration] impl` block)
//! is a loud `compile_error!` steering to the impl-block form: a proc-macro attr on a
//! single method cannot emit the sibling `Descriptor` row, so a config-class @bean
//! METHOD is lowered by `#[configuration]` on the WHOLE impl block. (The working
//! impl-block form is exercised by `tests/config_impl_app.rs`.)

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
