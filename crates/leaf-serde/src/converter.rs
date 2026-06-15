//! [`SerdeConverter<T>`] — a leaf-core [`Converter`] backed by serde `Deserialize`.
//!
//! This is the "serde-bridge converter" of binding-conversion phase3/07: an
//! erased [`Converter`] registered into the OPT-IN
//! [`ConversionService`](leaf_core::ConversionService) so the binder's erased
//! fallback can coerce a scalar [`ConfigValue`] into any serde `Deserialize`
//! target. The canonical monomorphized [`FromConfigValue`](leaf_core::FromConfigValue)
//! path is never touched — the two coexist; registering a `SerdeConverter` is a
//! deliberate opt-in, exactly like a `#[converter]` bean.
//!
//! The coercion routes the raw, trimmed scalar string through
//! [`ScalarDeserializer`], a tiny serde [`Deserializer`] that forwards a single
//! string to whichever `deserialize_*` hint the target requests (numbers/bools
//! parse the string, strings/chars pass it through, an `Option` is `Some`). It is
//! the SCALAR half of the bridge (the [`crate::ConfigDeserializer`] handles the
//! structured subtree half).

use std::any::{Any, TypeId};
use std::marker::PhantomData;
use std::sync::Arc;

use leaf_core::bind::{ConversionService, Converter};
use leaf_core::convert::{ConfigValue, ConvertCtx};
use leaf_core::error::{ErrorKind, LeafError, Origin};

use crate::error::SerdeBridgeError;

/// A scalar [`Converter`] that coerces a [`ConfigValue`] into `T` via serde.
///
/// `T` must be `serde::de::DeserializeOwned` (so the value is fully owned, no
/// borrow from the transient string) and `'static + Send + Sync` (the erased
/// [`Converter`] contract). Construct one and register it into a
/// [`ConversionService`] via [`register_serde_converter`] or
/// [`ConversionService::register`].
pub struct SerdeConverter<T> {
    _target: PhantomData<fn() -> T>,
}

impl<T> SerdeConverter<T> {
    /// A converter for target type `T`.
    #[must_use]
    pub fn new() -> Self {
        SerdeConverter {
            _target: PhantomData,
        }
    }
}

impl<T> Default for SerdeConverter<T> {
    fn default() -> Self {
        SerdeConverter::new()
    }
}

impl<T> Converter for SerdeConverter<T>
where
    T: serde::de::DeserializeOwned + Any + Send + Sync + 'static,
{
    fn convert(
        &self,
        v: &ConfigValue<'_>,
        _cx: &ConvertCtx,
    ) -> Result<Box<dyn Any + Send>, LeafError> {
        let de = ScalarDeserializer::new(v.trimmed(), v.origin);
        let value = T::deserialize(de).map_err(|e| e.into_leaf(ErrorKind::ConvertError))?;
        Ok(Box::new(value) as Box<dyn Any + Send>)
    }

    fn target(&self) -> TypeId {
        TypeId::of::<T>()
    }
}

/// Register a [`SerdeConverter<T>`] into `svc` (cold-path, pre-seal).
///
/// The ergonomic entry point: `register_serde_converter::<MyType>(&mut svc)`
/// installs the serde-backed erased converter for `MyType`, so the binder's
/// erased fallback will use serde for that target while every other target stays
/// on the canonical `FromConfigValue` path.
pub fn register_serde_converter<T>(svc: &mut ConversionService)
where
    T: serde::de::DeserializeOwned + Any + Send + Sync + 'static,
{
    svc.register(Arc::new(SerdeConverter::<T>::new()));
}

/// A minimal serde [`Deserializer`](serde::Deserializer) over a single scalar
/// string + its [`Origin`].
///
/// It forwards the string to whatever primitive the target requests: numeric and
/// boolean hints parse the string (errors keep the origin), `str`/`string`/`char`
/// pass it through, `option` is always `Some` (a blank scalar never reaches here —
/// the canonical `Option<T>` impl owns that), and `deserialize_any`/`enum` treat
/// the string as an identifier (so a plain unit enum variant works). Structured
/// hints (`struct`/`map`/`seq`) are rejected — those are the
/// [`crate::ConfigDeserializer`]'s job.
pub(crate) struct ScalarDeserializer<'a> {
    raw: &'a str,
    origin: Origin,
}

impl<'a> ScalarDeserializer<'a> {
    pub(crate) fn new(raw: &'a str, origin: Origin) -> Self {
        ScalarDeserializer { raw, origin }
    }

    fn err(&self, msg: impl Into<String>) -> SerdeBridgeError {
        SerdeBridgeError::new(msg, self.origin)
    }

    fn parse<T: std::str::FromStr>(&self, ty: &str) -> Result<T, SerdeBridgeError> {
        self.raw
            .parse::<T>()
            .map_err(|_| self.err(format!("cannot parse {:?} as {ty}", self.raw)))
    }
}

macro_rules! deserialize_parsed {
    ($($method:ident => $visit:ident : $ty:ty),+ $(,)?) => {
        $(
            fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
            where
                V: serde::de::Visitor<'de>,
            {
                let parsed: $ty = self.parse(stringify!($ty))?;
                visitor.$visit(parsed)
            }
        )+
    };
}

impl<'de, 'a> serde::Deserializer<'de> for ScalarDeserializer<'a> {
    type Error = SerdeBridgeError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        // Untyped: hand serde the borrowed string (the most faithful scalar shape).
        visitor.visit_str(self.raw)
    }

    deserialize_parsed! {
        deserialize_bool => visit_bool : bool,
        deserialize_i8 => visit_i8 : i8,
        deserialize_i16 => visit_i16 : i16,
        deserialize_i32 => visit_i32 : i32,
        deserialize_i64 => visit_i64 : i64,
        deserialize_i128 => visit_i128 : i128,
        deserialize_u8 => visit_u8 : u8,
        deserialize_u16 => visit_u16 : u16,
        deserialize_u32 => visit_u32 : u32,
        deserialize_u64 => visit_u64 : u64,
        deserialize_u128 => visit_u128 : u128,
        deserialize_f32 => visit_f32 : f32,
        deserialize_f64 => visit_f64 : f64,
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        let mut chars = self.raw.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => visitor.visit_char(c),
            _ => Err(self.err(format!("expected a single char, got {:?}", self.raw))),
        }
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_str(self.raw)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_string(self.raw.to_owned())
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        // A blank scalar is owned by leaf-core's `Option<T>` impl and never
        // reaches here; a present scalar is `Some`.
        visitor.visit_some(self)
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_unit()
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_str(self.raw)
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        // A scalar enum is a unit-variant name; hand it to the visitor as a
        // string-keyed enum access with no payload.
        visitor.visit_enum(serde::de::value::StrDeserializer::new(self.raw))
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_unit()
    }

    // Structured shapes are the ConfigDeserializer's job, not the scalar bridge.
    fn deserialize_seq<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(self.err("scalar converter cannot deserialize a sequence (use ConfigDeserializer)"))
    }

    fn deserialize_map<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(self.err("scalar converter cannot deserialize a map (use ConfigDeserializer)"))
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(self.err("scalar converter cannot deserialize a struct (use ConfigDeserializer)"))
    }

    fn deserialize_tuple<V>(self, _len: usize, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(self.err("scalar converter cannot deserialize a tuple (use ConfigDeserializer)"))
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(self.err("scalar converter cannot deserialize a tuple struct (use ConfigDeserializer)"))
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_bytes(self.raw.as_bytes())
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_byte_buf(self.raw.as_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::bind::ConversionService;
    use leaf_core::convert::{ConfigValue, ConvertCtx, FromConfigValue};
    use serde::Deserialize;

    #[derive(serde::Deserialize, Debug, PartialEq)]
    enum Mode {
        Fast,
        Slow,
    }

    #[test]
    fn serde_converter_round_trips_a_scalar_via_conversion_service() {
        let mut svc = ConversionService::new();
        register_serde_converter::<u16>(&mut svc);
        assert!(svc.has(TypeId::of::<u16>()));

        let cv = ConfigValue::scalar("8443");
        let boxed = svc
            .convert(TypeId::of::<u16>(), &cv, &ConvertCtx::strict())
            .expect("converter registered")
            .expect("conversion succeeds");
        assert_eq!(*boxed.downcast::<u16>().unwrap(), 8443);
    }

    #[test]
    fn serde_converter_deserializes_a_unit_enum_variant() {
        let de = ScalarDeserializer::new("Fast", Origin::Unknown);
        assert_eq!(Mode::deserialize(de).unwrap(), Mode::Fast);
    }

    #[test]
    fn serde_converter_failure_is_a_convert_error_with_origin() {
        let cv = ConfigValue::scalar("not-a-number").with_origin(Origin::TestDouble);
        let conv = SerdeConverter::<u16>::new();
        let err = conv.convert(&cv, &ConvertCtx::strict()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::ConvertError);
        assert_eq!(err.origin, Origin::TestDouble);
    }

    #[test]
    fn bridge_coexists_with_canonical_fromconfigvalue() {
        // The canonical monomorphized path is entirely independent of the bridge.
        let cv = ConfigValue::scalar("8443");
        let canonical = u16::from_config_value(&cv, &ConvertCtx::strict()).unwrap();

        let conv = SerdeConverter::<u16>::new();
        let bridged = *conv
            .convert(&cv, &ConvertCtx::strict())
            .unwrap()
            .downcast::<u16>()
            .unwrap();

        assert_eq!(canonical, bridged);
        assert_eq!(canonical, 8443);
    }
}
