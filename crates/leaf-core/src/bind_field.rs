//! Type-driven config-field dispatch (the alias-safe replacement for the codegen's
//! `field_shape` type-NAME classification).
//!
//! The cardinal rule (charter §2.x): a macro/codegen must NEVER decide behaviour from
//! a type's TEXTUAL NAME, because a type alias defeats it. The `#[derive(BindTarget)]`
//! codegen used to read the leading path segment of each field type
//! (`seg.ident == "Vec"`, plus an `is_scalar_ident` std-type NAME SET) to pick the
//! `cursor.scalar` / `cursor.list` / `cursor.nested` call AND the field's
//! `NodeSchema`. A re-exported scalar alias or `type Tags = Vec<String>;` silently
//! mis-bound (a list bound as a nested object, a scalar bound as nested).
//!
//! This module moves that decision to the TYPE SYSTEM via an autoref-specialization
//! ladder (the anyhow-style stable-Rust trick), so the macro emits ONE uniform call
//! site `(&&&::leaf_core::ConfigFieldTag::<#ty>::new()).leaf_bind_field(cursor, name)`
//! and ONE uniform schema reference, never spelling `Vec`/`String`/a scalar set. The
//! ladder resolves the real field type through DISJOINT trait bounds the framework
//! already owns:
//!
//! - a homogeneous list `Vec<T: FromConfigValue>` → `cursor.list` (the `&&` rung);
//! - a scalar leaf `T: FromConfigValue` → `cursor.scalar` (the `&` rung);
//! - a nested object `T: BindTarget` → `cursor.nested` (the bare-value rung).
//!
//! Method resolution on the `&&&Tag` call receiver prefers the candidate whose `&self`
//! receiver type EXACTLY matches the literal `&&&Tag`, then ever-deeper autoderefs. The
//! `&&` list rung (receiver `&&&Tag`) is that exact match, so a `Vec<T>` — which is
//! ITSELF `FromConfigValue`, hence ALSO satisfies the scalar rung — binds as a LIST.
//! A non-`Vec` scalar fails the list rung and autoderefs to the `&` scalar rung; a
//! `BindTarget` object (DISJOINT from `FromConfigValue`, so the nested and scalar rungs
//! never overlap) autoderefs to the bare-value nested rung. The decision is purely
//! structural: an alias for `Vec<String>` resolves to the SAME `Vec<T>` type, so it
//! binds identically.

use crate::bind::{BindCursor, BindResult, BindTarget, NodeSchema};
use crate::convert::FromConfigValue;

/// The zero-sized dispatch tag carrying the field's concrete type `T`. The
/// `#[derive(BindTarget)]` codegen constructs `ConfigFieldTag::<FieldTy>::new()` and
/// resolves the binding shape through the autoref ladder below — NEVER from `FieldTy`'s
/// spelled name (an alias resolves to the same `T`, so it binds identically).
pub struct ConfigFieldTag<T>(core::marker::PhantomData<T>);

impl<T> ConfigFieldTag<T> {
    /// Construct the tag (zero-sized; only the type `T` matters).
    #[must_use]
    pub const fn new() -> Self {
        ConfigFieldTag(core::marker::PhantomData)
    }
}

impl<T> Default for ConfigFieldTag<T> {
    fn default() -> Self {
        ConfigFieldTag::new()
    }
}

// ── rung 0 (`&&` — the EXACT `&&&Tag` match → highest priority): a Vec<T> list ──

/// The list rung of the config-field dispatch ladder. Implemented for a
/// `&&ConfigFieldTag<Vec<T>>`, whose `&self` receiver `&&&ConfigFieldTag<Vec<T>>`
/// EXACTLY matches the `&&&Tag` call site — so it is the highest-priority candidate, and
/// a `Vec<T>` (which ALSO satisfies the scalar rung's `Vec<T>: FromConfigValue`) always
/// binds as a LIST.
pub trait BindFieldList {
    /// The bound value type (`Self` is `&&ConfigFieldTag<Vec<T>>`; this is `Vec<T>`).
    type Out;
    /// Bind a homogeneous-list field through `cursor.list`.
    fn leaf_bind_field(
        &self,
        cursor: &mut BindCursor<'_, '_>,
        name: &'static str,
    ) -> BindResult<Self::Out>;
    /// The `NodeSchema` node documenting a scalar list.
    fn leaf_node_schema(&self) -> &'static NodeSchema;
}

impl<T: FromConfigValue> BindFieldList for &&ConfigFieldTag<Vec<T>> {
    type Out = Vec<T>;
    fn leaf_bind_field(
        &self,
        cursor: &mut BindCursor<'_, '_>,
        name: &'static str,
    ) -> BindResult<Vec<T>> {
        cursor.list::<T>(name)
    }
    fn leaf_node_schema(&self) -> &'static NodeSchema {
        &NodeSchema::List(&NodeSchema::Scalar)
    }
}

// ── rung 1 (`&` — one autoderef deeper): a scalar leaf ──

/// The scalar-leaf rung of the config-field dispatch ladder. Implemented for a
/// `&ConfigFieldTag<T>`, reached when a field is NOT a `Vec<T>` (the list rung fails) —
/// so a `Vec<T>` binds as a list, while a non-`Vec` scalar (`u16`, `String`, an aliased
/// scalar) binds here.
pub trait BindFieldScalar {
    /// The bound value type (`Self` is `&ConfigFieldTag<T>`; this is `T`).
    type Out;
    /// Bind a scalar leaf field through `cursor.scalar`.
    fn leaf_bind_field(
        &self,
        cursor: &mut BindCursor<'_, '_>,
        name: &'static str,
    ) -> BindResult<Self::Out>;
    /// The shared scalar schema node.
    fn leaf_node_schema(&self) -> &'static NodeSchema;
}

impl<T: FromConfigValue> BindFieldScalar for &ConfigFieldTag<T> {
    type Out = T;
    fn leaf_bind_field(
        &self,
        cursor: &mut BindCursor<'_, '_>,
        name: &'static str,
    ) -> BindResult<T> {
        cursor.scalar::<T>(name)
    }
    fn leaf_node_schema(&self) -> &'static NodeSchema {
        &NodeSchema::Scalar
    }
}

// ── rung 2 (the bare-value tag — reached LAST): a nested BindTarget object ──

/// The nested-object rung of the config-field dispatch ladder. Implemented for a bare
/// `ConfigFieldTag<T>`, reached only after the list + scalar rungs are ruled out;
/// `T: BindTarget` is DISJOINT from the scalar rung's `T: FromConfigValue`, so nested
/// and scalar never overlap (a `BindTarget` object is not `FromConfigValue`).
pub trait BindFieldNested {
    /// The bound value type (`Self` is `ConfigFieldTag<T>`; this is `T`).
    type Out;
    /// Bind a nested `BindTarget` object field through `cursor.nested`.
    fn leaf_bind_field(
        &self,
        cursor: &mut BindCursor<'_, '_>,
        name: &'static str,
    ) -> BindResult<Self::Out>;
    /// The nested object's derived schema pointer.
    fn leaf_node_schema(&self) -> &'static NodeSchema;
}

impl<T: BindTarget> BindFieldNested for ConfigFieldTag<T> {
    type Out = T;
    fn leaf_bind_field(
        &self,
        cursor: &mut BindCursor<'_, '_>,
        name: &'static str,
    ) -> BindResult<T> {
        cursor.nested::<T>(name)
    }
    fn leaf_node_schema(&self) -> &'static NodeSchema {
        <T as BindTarget>::SCHEMA
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The dispatch ladder is a TYPE-LEVEL classification, so the schema-node selection
    // (which the `#[derive(BindTarget)]` codegen drives identically) proves which rung a
    // field type resolves to WITHOUT a binder. The cardinal-rule regression: an ALIAS for
    // a type resolves to the SAME rung as the un-aliased type (a spelled-name classifier
    // would mis-route the alias).

    fn node_of<F: Fn() -> &'static NodeSchema>(f: F) -> &'static NodeSchema {
        f()
    }

    // The uniform call the codegen emits, captured as a closure so a test can drive it on
    // an arbitrary field type. The `use … as _` brings every rung's trait into scope.
    macro_rules! schema_for {
        ($ty:ty) => {
            node_of(|| {
                #[allow(unused_imports)]
                use super::{BindFieldList as _, BindFieldNested as _, BindFieldScalar as _};
                (&&&ConfigFieldTag::<$ty>::new()).leaf_node_schema()
            })
        };
    }

    #[test]
    fn a_scalar_field_resolves_to_the_scalar_rung() {
        assert!(matches!(schema_for!(u16), NodeSchema::Scalar));
        assert!(matches!(schema_for!(String), NodeSchema::Scalar));
    }

    #[test]
    fn a_vec_field_resolves_to_the_list_rung() {
        // Vec<T> is ITSELF FromConfigValue (so it would also match the scalar rung), but
        // the `&&&` list rung is reached first in method resolution → a List node.
        assert!(matches!(schema_for!(Vec<String>), NodeSchema::List(_)));
        assert!(matches!(schema_for!(Vec<u32>), NodeSchema::List(_)));
    }

    // A nested BindTarget object (disjoint from FromConfigValue) → the nested rung.
    #[derive(Debug, Default, PartialEq)]
    struct Inner {
        x: u8,
    }
    static INNER_SCHEMA: NodeSchema = NodeSchema::Object {
        method: crate::bind::BindMethod::JavaBean,
        fields: &[],
    };
    impl BindTarget for Inner {
        const SCHEMA: &'static NodeSchema = &INNER_SCHEMA;
        fn bind(_c: &mut BindCursor<'_, '_>) -> BindResult<Self> {
            BindResult::Unbound
        }
    }

    #[test]
    fn a_nested_bindtarget_field_resolves_to_the_nested_rung() {
        // The nested rung returns the inner type's derived SCHEMA pointer (an Object).
        let s = schema_for!(Inner);
        assert!(matches!(s, NodeSchema::Object { .. }));
        assert!(std::ptr::eq(s, <Inner as BindTarget>::SCHEMA));
    }

    // ── the cardinal-rule regression: aliases resolve to the SAME rung ────────────

    type Tags = Vec<String>; // a list alias
    type Name = String; // a scalar alias
    type Boxed = Inner; // a nested-object alias

    #[test]
    fn an_aliased_vec_field_still_resolves_to_the_list_rung() {
        // `type Tags = Vec<String>;` — a NAME-based classifier sees `Tags` (not `Vec`)
        // and would mis-route it as a nested object. The type-driven ladder resolves the
        // real `Vec<String>` → a List node, identical to the un-aliased type.
        assert!(matches!(schema_for!(Tags), NodeSchema::List(_)));
    }

    #[test]
    fn an_aliased_scalar_field_still_resolves_to_the_scalar_rung() {
        assert!(matches!(schema_for!(Name), NodeSchema::Scalar));
    }

    #[test]
    fn an_aliased_nested_field_still_resolves_to_the_nested_rung() {
        assert!(matches!(schema_for!(Boxed), NodeSchema::Object { .. }));
    }
}
