//! `leaf-serde` — the OPTIONAL serde bridge for leaf's binding/conversion world.
//!
//! Design authority: binding-conversion phase3/07 (`type-conversion`), which
//! resolves the *serde-adoption* open question as:
//!
//! > serde adoption: NOT adopted into leaf-core's mandatory surface; a
//! > feature-gated serde-bridge converter is offered but `FromConfigValue` is
//! > the canonical trait so diagnostics keep Origin/provenance richness.
//!
//! and the binder open question as:
//!
//! > Target self-description: a leaf-owned `#[derive(BindTarget)]` … ;
//! > serde-bridge is an opt-in alternate, not the canonical path.
//!
//! So this crate sits ENTIRELY behind the `serde-bridge` cargo feature (on by
//! default here, since the bridge is the crate's reason to exist) and offers two
//! coexisting, opt-in mechanisms — never replacing [`leaf_core::FromConfigValue`]:
//!
//! 1. [`SerdeConverter<T>`] — a leaf-core [`Converter`](leaf_core::Converter) that
//!    coerces a scalar [`ConfigValue`](leaf_core::ConfigValue) into any serde
//!    `Deserialize` target by routing the raw string through a tiny scalar
//!    [`Deserializer`](serde::Deserializer). It registers into the OPT-IN erased
//!    [`ConversionService`](leaf_core::ConversionService) exactly like a
//!    `#[converter]`, so it rides the same erased fallback the binder already
//!    consults — and the canonical monomorphized `FromConfigValue` path is
//!    untouched (the two coexist).
//!
//! 2. [`ConfigDeserializer`] — a serde [`Deserializer`](serde::Deserializer) over
//!    a config subtree (an [`Env`](leaf_core::Env) + a prefix
//!    [`CanonicalName`](leaf_core::CanonicalName)). It lets a plain
//!    `#[derive(serde::Deserialize)]` type be bound straight from a
//!    [`PropertySource`](leaf_core::PropertySource) stack — the serde-shaped
//!    alternate to the leaf-native [`Binder`](leaf_core::Binder)/`BindTarget`
//!    path, reusing leaf-core's relaxed key identity and tri-state CPS view.
//!
//! Both paths surface failures as leaf-core [`LeafError`](leaf_core::LeafError)
//! nodes (`ConvertError`/`BindError`) so a serde-bound value still lands on the
//! one diagnostic spine; only the field-by-field Origin richness of the native
//! path is traded away (the documented serde-bridge cost).

#![cfg_attr(not(feature = "serde-bridge"), allow(unused))]

#[cfg(feature = "serde-bridge")]
mod converter;
#[cfg(feature = "serde-bridge")]
mod deserializer;
#[cfg(feature = "serde-bridge")]
mod error;
#[cfg(feature = "web-converter")]
mod http_converter;

#[cfg(feature = "serde-bridge")]
pub use converter::{register_serde_converter, SerdeConverter};
#[cfg(feature = "serde-bridge")]
pub use deserializer::{from_env, from_source, ConfigDeserializer};
#[cfg(feature = "serde-bridge")]
pub use error::SerdeBridgeError;
#[cfg(feature = "web-converter")]
pub use http_converter::{HttpMessageConverterExt, JsonConverter, JsonConverterConfig};
