//! Integration test for BY-TRAIT INJECTION via the `#[injectable]` trait attribute.
//!
//! A user trait `Notifier` annotated `#[injectable]` makes `dyn Notifier` an
//! injectable VIEW: the macro emits one `impl ::leaf_core::Resolve for dyn Notifier`
//! (the per-view seam). A concrete bean `Email` provides the view through a
//! macro-shaped `provides[]` `TypeRow` upcast (the double-`Arc` view-holder). The
//! test then drives the engine to resolve `Ref<dyn Notifier>` THROUGH the same
//! `Engine::resolve_view` primitive a concrete `Ref<T>` uses — proving the whole
//! `#[injectable]` → `Resolve` impl → `resolve_view` → typed view-holder roundtrip
//! across crate boundaries (this crate has NO `linkme` dep, only leaf-core/leaf-macros).

use std::any::TypeId;
use std::sync::Arc;

use leaf_core::{
    AnnotationMetadata, Descriptor, Engine, ErasedBean, Injectable, Published, Ref, RegistryBuilder,
    ResolveCtx, Role, ScopeDef, TypeRow,
};
use leaf_macros::injectable;

/// A user service trait, made an injectable VIEW by `#[injectable]`. The attribute
/// emits `impl ::leaf_core::Resolve for dyn Notifier` once (orphan-rule-OK).
#[injectable]
trait Notifier: Send + Sync + 'static {
    fn notify(&self) -> &'static str;
}

/// A concrete bean implementing the trait (the providing bean).
struct Email;
impl leaf_core::Bean for Email {}
impl Notifier for Email {
    fn notify(&self) -> &'static str {
        "email-sent"
    }
}

/// The macro-shaped `provides[]` upcast for `Email as dyn Notifier`: downcast to the
/// concrete, unsize the `Arc` to `Arc<dyn Notifier>`, re-erase as the double-`Arc`
/// view-HOLDER `Engine::resolve_view` hands back (recovered typed by `view_from_holder`).
fn email_as_notifier(bean: ErasedBean) -> ErasedBean {
    match bean.downcast::<Email>() {
        Ok(c) => {
            let view: Arc<dyn Notifier> = c;
            Arc::new(view) as ErasedBean
        }
        Err(orig) => orig,
    }
}

struct EmailProvider {
    descriptor: Descriptor,
}
impl leaf_core::Provider for EmailProvider {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    fn provide<'a>(
        &'a self,
        _cx: &'a ResolveCtx<'a>,
    ) -> leaf_core::BoxFuture<'a, Result<Published, leaf_core::LeafError>> {
        Box::pin(async { Ok(Published::shared_value(Email)) })
    }
}

fn email_descriptor() -> Descriptor {
    // The macro-emitted shape: a provides[] row whose view is `dyn Notifier` and whose
    // upcast produces the view-holder. (A real struct stereotype emits this row; here
    // we hand-build it to keep the test to the by-trait path.)
    let provides: &'static [TypeRow] = Box::leak(Box::new([TypeRow {
        view: TypeId::of::<dyn Notifier>(),
        upcast: email_as_notifier,
    }]));
    Descriptor {
        contract: leaf_core::ContractId::of("by_trait_injection::Email"),
        self_type: TypeId::of::<Email>(),
        provides,
        declared_name: Some("email"),
        aliases: &[],
        scope: ScopeDef::SINGLETON,
        role: Role::Application,
        meta: &AnnotationMetadata::EMPTY,
        parent: None,
        origin: leaf_core::Origin::Native { crate_name: Some("by_trait_injection") },
    }
}

fn engine_with_email() -> Engine {
    let d = email_descriptor();
    let mut builder = RegistryBuilder::new();
    builder
        .register(d, Arc::new(EmailProvider { descriptor: d }))
        .expect("register Email");
    Engine::from_builder(builder).expect("freeze")
}

#[test]
fn injectable_trait_makes_ref_dyn_trait_resolvable_to_the_providing_bean() {
    // `#[injectable] trait Notifier` made `Ref<dyn Notifier>` Injectable: it resolves
    // to the `Email` bean providing the view, through the SAME path as a concrete
    // Ref<T>, and the recovered trait object dispatches correctly.
    let engine = engine_with_email();
    let cx = ResolveCtx::for_engine(&engine);

    let n: Ref<dyn Notifier> =
        futures::executor::block_on(<Ref<dyn Notifier> as Injectable>::inject(&cx))
            .expect("dyn Notifier view resolves through the #[injectable] seam");
    assert_eq!(n.notify(), "email-sent");
}

#[test]
fn the_dyn_view_resolvable_targets_the_view_type_id_not_the_concrete() {
    // The const RESOLVABLE the wave-planner reads targets the VIEW's TypeId (trait
    // dispatch via the emitted Resolve impl, never a spelled name).
    let r = <Ref<dyn Notifier> as Injectable>::RESOLVABLE;
    assert_eq!(r.produced, TypeId::of::<dyn Notifier>());
    assert_ne!(r.produced, TypeId::of::<Email>());
}
