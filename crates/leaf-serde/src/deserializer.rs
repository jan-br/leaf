//! [`ConfigDeserializer`] — a serde [`Deserializer`] over a config subtree.
//!
//! This is the "ConfigDeserializer alternate" of binding-conversion phase3/07:
//! bind a config subtree (an [`Env`] + a prefix [`CanonicalName`]) into ANY serde
//! `Deserialize` type, reusing leaf-core's relaxed key identity and the tri-state
//! [`ConfigurationPropertySource`] (CPS) view the native [`Binder`] already uses.
//! It is the STRUCTURED half of the bridge (the [`crate::SerdeConverter`] handles
//! the scalar half).
//!
//! It mirrors the [`Binder`]'s tree-descent exactly:
//! - a STRUCT/MAP node offers a [`serde::de::MapAccess`] whose values recurse into
//!   a child [`ConfigDeserializer`] at `prefix.child(Named(field))`;
//! - a SEQ node walks indexed children `prefix[0]`, `prefix[1]`, … until the CPS
//!   reports the element subtree `Absent` (the binder's stop rule);
//! - a SCALAR node reads `cps.get(&prefix)` and forwards the raw string to the
//!   shared [`ScalarDeserializer`](crate::converter::ScalarDeserializer);
//! - an OPTION is `None` when no descendant of `prefix` is present, else `Some`.
//!
//! Failures surface as `BindError` [`LeafError`] nodes via [`from_env`]/
//! [`from_source`]. The serde seam trades away the binder's per-field Origin
//! richness (the documented bridge cost), but the value still lands on leaf's one
//! diagnostic spine.
//!
//! [`Binder`]: leaf_core::Binder
//! [`Deserializer`]: serde::Deserializer

use leaf_core::bind::{
    ConfigurationPropertySource, ConfigurationPropertyState, StackCps,
};
use leaf_core::env::Env;
use leaf_core::error::{ErrorKind, LeafError, Origin};
use leaf_core::relaxed::{CanonicalName, Segment};

use crate::converter::ScalarDeserializer;
use crate::error::SerdeBridgeError;

/// Generate the numeric `deserialize_*` leaves of [`ConfigDeserializer`]: each
/// reads the scalar at the current prefix and forwards it to the shared
/// [`ScalarDeserializer`].
macro_rules! config_numeric_leaf {
    ($($method:ident),+ $(,)?) => {
        $(
            fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
            where
                V: serde::de::Visitor<'de>,
            {
                let (raw, origin) = self.scalar_de()?;
                ScalarDeserializer::new(&raw, origin).$method(visitor)
            }
        )+
    };
}

/// A serde [`Deserializer`](serde::Deserializer) positioned at a node of a config
/// subtree: a [`ConfigurationPropertySource`] view + the [`CanonicalName`] prefix
/// of the current node.
///
/// Build the root with [`ConfigDeserializer::new`] (over any CPS) or use the
/// [`from_env`]/[`from_source`] free functions for the common cases; serde drives
/// the descent from there.
pub struct ConfigDeserializer<'c> {
    cps: &'c dyn ConfigurationPropertySource,
    prefix: CanonicalName,
}

impl<'c> ConfigDeserializer<'c> {
    /// A deserializer rooted at `prefix` over `cps`.
    #[must_use]
    pub fn new(cps: &'c dyn ConfigurationPropertySource, prefix: CanonicalName) -> Self {
        ConfigDeserializer { cps, prefix }
    }

    fn child(&self, seg: Segment) -> ConfigDeserializer<'c> {
        ConfigDeserializer {
            cps: self.cps,
            prefix: self.prefix.child(seg),
        }
    }

    /// The raw scalar value at the current prefix + its origin, if present.
    fn scalar(&self) -> Option<(String, Origin)> {
        self.cps
            .get(&self.prefix)
            .map(|pv| (pv.raw.into_owned(), pv.origin))
    }

    fn missing(&self) -> SerdeBridgeError {
        SerdeBridgeError::new(
            format!("no config value at `{}`", self.prefix),
            Origin::Unknown,
        )
    }

    fn scalar_de(&self) -> Result<(String, Origin), SerdeBridgeError> {
        self.scalar().ok_or_else(|| self.missing())
    }
}

/// Deserialize `T` from the config subtree of `env` rooted at `prefix`.
///
/// The serde-shaped alternate to `leaf_core::Binder::bind::<T>`. A failure is a
/// canonical `BindError` [`LeafError`] (the serde-bridge cost is the loss of
/// per-field Origin, not the loss of the diagnostic spine).
///
/// # Errors
/// Returns a `BindError` [`LeafError`] when a value is missing/ill-typed for `T`.
pub fn from_env<T>(env: &Env, prefix: &str) -> Result<T, LeafError>
where
    T: serde::de::DeserializeOwned,
{
    let cps = StackCps::new(env.clone());
    from_source(&cps, prefix)
}

/// Deserialize `T` from the config subtree of `cps` rooted at `prefix`.
///
/// Like [`from_env`] but over any [`ConfigurationPropertySource`] (e.g. a test
/// double or a pre-built [`StackCps`]).
///
/// # Errors
/// Returns a `BindError` [`LeafError`] when a value is missing/ill-typed for `T`,
/// or when `prefix` is empty / not a valid canonical name.
pub fn from_source<T>(cps: &dyn ConfigurationPropertySource, prefix: &str) -> Result<T, LeafError>
where
    T: serde::de::DeserializeOwned,
{
    // The bind is always rooted at a non-empty prefix (the binder contract); an
    // empty/invalid prefix is a BindError, not a silent root bind.
    let name = CanonicalName::parse(prefix).map_err(|e| {
        LeafError::new(ErrorKind::BindError).caused_by(leaf_core::error::Cause::plain(
            "serde-bridge prefix",
            format!("invalid config prefix {prefix:?}: {}", e.reason),
        ))
    })?;
    let de = ConfigDeserializer::new(cps, name);
    T::deserialize(de).map_err(|e| e.into_leaf(ErrorKind::BindError))
}

impl<'de, 'c> serde::Deserializer<'de> for ConfigDeserializer<'c> {
    type Error = SerdeBridgeError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        // Untyped at a subtree node: a present scalar is a string; otherwise we
        // cannot infer structure without the target's hint.
        match self.scalar() {
            Some((raw, _)) => visitor.visit_string(raw),
            None => Err(SerdeBridgeError::new(
                format!(
                    "cannot deserialize `{}` without a type hint (no scalar value)",
                    self.prefix
                ),
                Origin::Unknown,
            )),
        }
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_map(FieldMap {
            de: self,
            fields: fields.iter(),
            pending: None,
        })
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        // A free-form map: enumerate the immediate child keys under the prefix.
        let keys = child_keys(self.cps, &self.prefix);
        visitor.visit_map(KeyMap {
            de: self,
            keys: keys.into_iter(),
            pending: None,
        })
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        // Inline comma-split scalar form first (`name=a,b,c`), else indexed
        // children (`name[0]`, `name[1]`, …) — the binder's list rule.
        if let Some((raw, origin)) = self.scalar() {
            return visitor.visit_seq(CommaSeq {
                items: split_list(&raw),
                idx: 0,
                origin,
            });
        }
        visitor.visit_seq(IndexedSeq {
            de: &self,
            idx: 0,
        })
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        // None iff no descendant (and no scalar) is present at this prefix.
        let present = self.scalar().is_some()
            || matches!(
                self.cps.contains_descendant_of(&self.prefix),
                ConfigurationPropertyState::Present | ConfigurationPropertyState::Unknown
            );
        if present {
            visitor.visit_some(self)
        } else {
            visitor.visit_none()
        }
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

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        // A scalar at this node is a unit-variant name; defer to the scalar path.
        let (raw, origin) = self.scalar_de()?;
        ScalarDeserializer::new(&raw, origin).deserialize_enum(name, variants, visitor)
    }

    // ── scalar leaves: read the value here, forward to ScalarDeserializer ──

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        let (raw, origin) = self.scalar_de()?;
        ScalarDeserializer::new(&raw, origin).deserialize_bool(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        let (raw, _) = self.scalar_de()?;
        visitor.visit_str(&raw)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        let (raw, _) = self.scalar_de()?;
        visitor.visit_string(raw)
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        let (raw, origin) = self.scalar_de()?;
        ScalarDeserializer::new(&raw, origin).deserialize_char(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        let (raw, origin) = self.scalar_de()?;
        ScalarDeserializer::new(&raw, origin).deserialize_bytes(visitor)
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        let (raw, origin) = self.scalar_de()?;
        ScalarDeserializer::new(&raw, origin).deserialize_byte_buf(visitor)
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

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_unit()
    }

    // The numeric leaves each read the scalar at the prefix and forward it to the
    // shared ScalarDeserializer (which parses it under the requested hint).
    config_numeric_leaf! {
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
    }
}

// ───────────────────────── MapAccess for structs ───────────────────────────

/// A `MapAccess` that walks a struct's STATIC field list, yielding each field's
/// value as a child [`ConfigDeserializer`]. A field whose subtree is absent is
/// SKIPPED (so `#[serde(default)]` / `Option` fields work), mirroring the
/// binder's `Unbound`-is-not-an-error rule.
struct FieldMap<'c> {
    de: ConfigDeserializer<'c>,
    fields: std::slice::Iter<'static, &'static str>,
    pending: Option<&'static str>,
}

/// Whether the field `name` has any value (scalar or subtree) under `prefix`.
fn field_present(
    cps: &dyn ConfigurationPropertySource,
    prefix: &CanonicalName,
    name: &str,
) -> bool {
    let Ok(seg) = field_segment(name) else {
        return false;
    };
    let child = prefix.child(seg);
    cps.get(&child).is_some()
        || matches!(
            cps.contains_descendant_of(&child),
            ConfigurationPropertyState::Present | ConfigurationPropertyState::Unknown
        )
}

impl<'de, 'c> serde::de::MapAccess<'de> for FieldMap<'c> {
    type Error = SerdeBridgeError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        // Advance to the next field that actually has a value present.
        let cps = self.de.cps;
        let prefix = self.de.prefix.clone();
        for &field in self.fields.by_ref() {
            if field_present(cps, &prefix, field) {
                self.pending = Some(field);
                let key = seed.deserialize(serde::de::value::StrDeserializer::<
                    SerdeBridgeError,
                >::new(field))?;
                return Ok(Some(key));
            }
        }
        Ok(None)
    }

    fn next_value_seed<S>(&mut self, seed: S) -> Result<S::Value, Self::Error>
    where
        S: serde::de::DeserializeSeed<'de>,
    {
        let field = self
            .pending
            .take()
            .ok_or_else(|| SerdeBridgeError::new("value requested before key", Origin::Unknown))?;
        let seg = field_segment(field)?;
        seed.deserialize(self.de.child(seg))
    }
}

// ───────────────────────── MapAccess for free maps ──────────────────────────

/// A `MapAccess` over the immediate child keys discovered under a prefix (the
/// `HashMap<String, V>` shape). Keys are the raw immediate segment strings.
struct KeyMap<'c> {
    de: ConfigDeserializer<'c>,
    keys: std::vec::IntoIter<String>,
    pending: Option<String>,
}

impl<'de, 'c> serde::de::MapAccess<'de> for KeyMap<'c> {
    type Error = SerdeBridgeError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        match self.keys.next() {
            Some(k) => {
                let key = seed.deserialize(serde::de::value::StrDeserializer::<
                    SerdeBridgeError,
                >::new(&k))?;
                self.pending = Some(k);
                Ok(Some(key))
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<S>(&mut self, seed: S) -> Result<S::Value, Self::Error>
    where
        S: serde::de::DeserializeSeed<'de>,
    {
        let key = self
            .pending
            .take()
            .ok_or_else(|| SerdeBridgeError::new("value requested before key", Origin::Unknown))?;
        let seg = field_segment(&key)?;
        seed.deserialize(self.de.child(seg))
    }
}

// ───────────────────────── SeqAccess (indexed children) ─────────────────────

/// A `SeqAccess` over indexed children `prefix[0]`, `prefix[1]`, … stopping when
/// the element's subtree is certainly `Absent` (the binder's list stop rule).
struct IndexedSeq<'a, 'c> {
    de: &'a ConfigDeserializer<'c>,
    idx: u32,
}

impl<'de, 'a, 'c> serde::de::SeqAccess<'de> for IndexedSeq<'a, 'c> {
    type Error = SerdeBridgeError;

    fn next_element_seed<S>(&mut self, seed: S) -> Result<Option<S::Value>, Self::Error>
    where
        S: serde::de::DeserializeSeed<'de>,
    {
        let elem = self.de.prefix.child(Segment::Indexed(self.idx));
        let present = self.de.cps.get(&elem).is_some()
            || matches!(
                self.de.cps.contains_descendant_of(&elem),
                ConfigurationPropertyState::Present
            );
        if !present {
            return Ok(None);
        }
        self.idx += 1;
        let child = ConfigDeserializer {
            cps: self.de.cps,
            prefix: elem,
        };
        seed.deserialize(child).map(Some)
    }
}

/// A `SeqAccess` over a comma-split inline scalar list (`a,b,c`).
struct CommaSeq {
    items: Vec<String>,
    idx: usize,
    origin: Origin,
}

impl<'de> serde::de::SeqAccess<'de> for CommaSeq {
    type Error = SerdeBridgeError;

    fn next_element_seed<S>(&mut self, seed: S) -> Result<Option<S::Value>, Self::Error>
    where
        S: serde::de::DeserializeSeed<'de>,
    {
        if self.idx >= self.items.len() {
            return Ok(None);
        }
        let item = self.items[self.idx].clone();
        self.idx += 1;
        seed.deserialize(ScalarDeserializer::new(&item, self.origin))
            .map(Some)
    }
}

// ───────────────────────── helpers ──────────────────────────────────────────

/// Parse a struct/map field name into a canonical [`Segment`], canonicalizing the
/// snake_case ident to kebab the way the binder derives field names.
fn field_segment(name: &str) -> Result<Segment, SerdeBridgeError> {
    // The relaxed fold makes `pool_size`/`pool-size` equivalent at lookup; we use
    // the name verbatim as a Named segment (CanonicalName::child re-canonicalizes
    // on dotted rendering and the Env applies the uniform fold on get).
    if name.is_empty() {
        return Err(SerdeBridgeError::new("empty field name", Origin::Unknown));
    }
    Ok(Segment::Named(name.into()))
}

/// Discover the immediate child segment names under `prefix` from the enumerable
/// sources (used for the free-`map` shape). Returns the distinct first segments
/// of every key strictly under `prefix`, in first-seen order.
fn child_keys(cps: &dyn ConfigurationPropertySource, prefix: &CanonicalName) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let prefix_dotted = prefix.to_dotted();
    let Some(iter) = cps.iter() else {
        return out;
    };
    for name in iter {
        let dotted = name.to_dotted();
        let rest = if prefix.is_empty() {
            Some(dotted.as_str())
        } else if let Some(r) = dotted.strip_prefix(&prefix_dotted) {
            r.strip_prefix('.')
        } else {
            None
        };
        if let Some(rest) = rest {
            if rest.is_empty() {
                continue;
            }
            // The immediate child segment is up to the next `.` or `[`.
            let end = rest
                .find(['.', '['])
                .unwrap_or(rest.len());
            let head = &rest[..end];
            if !head.is_empty() && !out.iter().any(|k| k == head) {
                out.push(head.to_string());
            }
        }
    }
    out
}

/// Split a comma-separated scalar list, trimming and dropping empties (matching
/// leaf-core's `Vec<T>` scalar form).
fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leaf_core::env::{EnvBuilder, MapPropertySource};
    use std::sync::Arc;

    fn env_from(pairs: &[(&str, &str)]) -> Env {
        let mut b = EnvBuilder::new();
        b.add_last(Arc::new(MapPropertySource::from_pairs(
            "test",
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())),
        )));
        b.seal_env()
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Server {
        port: u16,
        host: String,
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct AppConfig {
        name: String,
        server: Server,
    }

    #[test]
    fn binds_a_flat_struct_from_a_property_source() {
        let env = env_from(&[("server.port", "8443"), ("server.host", "leaf.dev")]);
        let got: Server = from_env(&env, "server").unwrap();
        assert_eq!(
            got,
            Server {
                port: 8443,
                host: "leaf.dev".into()
            }
        );
    }

    #[test]
    fn binds_nested_structs_via_descent() {
        let env = env_from(&[
            ("app.name", "leaf"),
            ("app.server.port", "8080"),
            ("app.server.host", "0.0.0.0"),
        ]);
        let got: AppConfig = from_env(&env, "app").unwrap();
        assert_eq!(got.name, "leaf");
        assert_eq!(got.server.port, 8080);
        assert_eq!(got.server.host, "0.0.0.0");
    }

    #[test]
    fn relaxed_keys_bind_via_uniform_fold() {
        // SERVER_PORT (env shape) binds server.port through leaf-core's relaxed fold.
        let env = env_from(&[("SERVER_PORT", "9000"), ("server.host", "h")]);
        let got: Server = from_env(&env, "server").unwrap();
        assert_eq!(got.port, 9000);
    }

    #[test]
    fn missing_required_field_is_a_bind_error() {
        let env = env_from(&[("server.host", "h")]); // no port
        let err = from_env::<Server>(&env, "server").unwrap_err();
        assert_eq!(err.kind, ErrorKind::BindError);
    }

    #[test]
    fn bad_value_is_a_bind_error() {
        let env = env_from(&[("server.port", "not-a-number"), ("server.host", "h")]);
        let err = from_env::<Server>(&env, "server").unwrap_err();
        assert_eq!(err.kind, ErrorKind::BindError);
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct WithOption {
        port: u16,
        #[serde(default)]
        note: Option<String>,
    }

    #[test]
    fn absent_option_field_is_none() {
        let env = env_from(&[("svc.port", "1")]);
        let got: WithOption = from_env(&env, "svc").unwrap();
        assert_eq!(got.port, 1);
        assert_eq!(got.note, None);
    }

    #[test]
    fn present_option_field_is_some() {
        let env = env_from(&[("svc.port", "1"), ("svc.note", "hi")]);
        let got: WithOption = from_env(&env, "svc").unwrap();
        assert_eq!(got.note, Some("hi".into()));
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct WithList {
        names: Vec<String>,
    }

    #[test]
    fn binds_an_inline_comma_list() {
        let env = env_from(&[("pools.names", "x,y,z")]);
        let got: WithList = from_env(&env, "pools").unwrap();
        assert_eq!(got.names, vec!["x", "y", "z"]);
    }

    #[test]
    fn binds_an_indexed_list() {
        let env = env_from(&[
            ("pools.names[0]", "a"),
            ("pools.names[1]", "b"),
            ("pools.names[2]", "c"),
        ]);
        let got: WithList = from_env(&env, "pools").unwrap();
        assert_eq!(got.names, vec!["a", "b", "c"]);
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Endpoint {
        host: String,
        port: u16,
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct WithObjList {
        servers: Vec<Endpoint>,
    }

    #[test]
    fn binds_a_list_of_nested_objects() {
        let env = env_from(&[
            ("cluster.servers[0].host", "h0"),
            ("cluster.servers[0].port", "1"),
            ("cluster.servers[1].host", "h1"),
            ("cluster.servers[1].port", "2"),
        ]);
        let got: WithObjList = from_env(&env, "cluster").unwrap();
        assert_eq!(got.servers.len(), 2);
        assert_eq!(got.servers[0].host, "h0");
        assert_eq!(got.servers[1].port, 2);
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct WithMap {
        limits: std::collections::HashMap<String, u32>,
    }

    #[test]
    fn binds_a_free_map_from_child_keys() {
        let env = env_from(&[("q.limits.read", "10"), ("q.limits.write", "20")]);
        let got: WithMap = from_env(&env, "q").unwrap();
        assert_eq!(got.limits.get("read"), Some(&10));
        assert_eq!(got.limits.get("write"), Some(&20));
    }

    #[test]
    fn empty_prefix_is_a_bind_error_not_a_silent_root() {
        let env = env_from(&[("server.port", "1"), ("server.host", "h")]);
        let err = from_env::<Server>(&env, "").unwrap_err();
        assert_eq!(err.kind, ErrorKind::BindError);
    }
}
