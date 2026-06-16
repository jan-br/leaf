//! `Injectable` — how a constructor parameter (or injected field) obtains itself
//! from the container. Trait dispatch, NEVER type-name matching: aliases/re-exports
//! of the handle types are irrelevant.
//!
//! Each impl exposes a const [`Resolvable`] (the static dependency the wave-planner
//! reads — `TypeId` + cardinality + strictness, known before instantiation, so the
//! dependency graph is built/validated/cycle-checked WITHOUT instantiating anything)
//! and an async [`inject`](Injectable::inject) (the runtime resolution at the one
//! [`ResolveCtx`] seam).
//!
//! The handle FAMILY impls live here ([`Ref`]/[`Lookup`]/[`LazyRef`]); coherence
//! forbids a blanket `impl<T: Bean> Injectable for T` alongside them, so a BARE bean
//! type (`db: Database`) is deliberately NOT `Injectable` — a clear compile error
//! steering to the handle currency `Ref<Database>` (no bare-type injection, no
//! name-based escape hatch).

use crate::error::{Cause, ErrorKind, LeafError};
use crate::future::BoxFuture;
use crate::handle::{Bean, ErasedBean, Ref};
use crate::injection::{
    Arity, Cardinality, InjectionPoint, LazyRef, Lookup, PointKind, Strictness,
};
use crate::provider::ResolveCtx;
use std::any::TypeId;
use std::sync::Arc;

/// The type-derived part of an injection point (the macro adds the param/field name
/// and any structural `@Qualifier`). `const`-constructible so the whole dependency
/// plan is known at compile time (the wave-planner reads it pre-instantiation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolvable {
    /// The bean `TypeId` this parameter/field resolves (the INNER target — e.g.
    /// `Ref<Svc>` resolves `Svc`), derived by trait dispatch, never by name.
    pub produced: TypeId,
    /// One bean vs. the whole candidate set.
    pub cardinality: Cardinality,
    /// How tolerant the resolution is of absence/ambiguity — the wave-planner reads
    /// this to decide whether the target MUST exist (a hard graph edge) or may be
    /// absent (a deferred/soft edge that does not force the target to exist).
    pub strictness: Strictness,
}

impl Resolvable {
    /// Lower this const descriptor to an [`InjectionPoint`] with the given point
    /// `name` (the param/field ident, or a positional `arg{i}` for a referenced
    /// constructor's nameless params).
    ///
    /// The ONE place the `Resolvable` → `InjectionPoint` mapping lives, shared by
    /// the field-default codegen and the [`InjectableCtor`](crate::InjectableCtor)
    /// driver: `cardinality` picks the [`Arity`], and `strictness` picks the
    /// [`PointKind`] (a `Strict` single is a real construction-time edge; a
    /// tolerant/deferred handle is a [`PointKind::Deferral`] that REMOVES the edge,
    /// the cycle break). Trait dispatch, never type-name matching.
    #[must_use]
    pub const fn into_point(self, name: &'static str) -> InjectionPoint {
        InjectionPoint {
            produced: self.produced,
            generics: &[],
            qualifiers: &[],
            name,
            arity: match self.cardinality {
                Cardinality::Single => Arity::Single,
                Cardinality::Multiple => Arity::Collection,
            },
            kind: match self.strictness {
                Strictness::Strict => PointKind::Bean,
                _ => PointKind::Deferral,
            },
            collection: None,
        }
    }
}

/// A type obtainable from the container as a constructor parameter (or injected
/// field). Trait dispatch decides HOW each is resolved — never type-name matching —
/// so aliases/re-exports of the handle types are irrelevant.
///
/// The impl-level `#[advisable]`/stereotype macros read [`RESOLVABLE`](Injectable::RESOLVABLE)
/// to build the static `InjectionPlan` and call [`inject`](Injectable::inject) at
/// instantiation to obtain the value the constructor consumes.
pub trait Injectable: Sized + Send + Sync + 'static {
    /// The static dependency this parameter contributes to the wave-planner: the
    /// resolvable target (`TypeId`), cardinality, and strictness. A const so the
    /// dependency graph is known before any instantiation (cycle detection,
    /// whole-graph validation, wave ordering).
    const RESOLVABLE: Resolvable;

    /// Obtain the value from the container at instantiation, through the one
    /// [`ResolveCtx`] resolution seam.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the eager resolution fails (a missing/ambiguous
    /// target, a construction fault, or a dropped container). Deferred handles
    /// ([`Lookup`]/[`LazyRef`]) build unconditionally — their resolution happens
    /// later, on first use.
    fn inject<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Self, LeafError>>;
}

/// HOW a `Ref<X>` target obtains itself from the container — the ONE general
/// primitive every `Ref<…>` injection surface inherits (by-trait injection).
///
/// `Resolve` (the verb the handle drives) is implemented in exactly TWO shapes,
/// which do NOT overlap (a `dyn Trait` is never a [`Bean`], so the blanket and the
/// per-view impls are coherent):
///
/// 1. a BLANKET impl over every concrete [`Bean`] (`impl<T: Bean> Resolve for T`) —
///    the existing concrete `Ref<ConcreteType>` path, UNCHANGED in behavior: it
///    resolves `T` by its `TypeId` through [`ResolveCtx::resolve_ref`];
/// 2. ONE impl PER `dyn Svc` VIEW (`impl Resolve for dyn Svc`, emitted once by
///    `#[injectable]` on the trait) — the by-trait path: it resolves the view's
///    `TypeId` through [`ResolveCtx::resolve_view`] (the [`Engine::resolve_view`](crate::Engine::resolve_view)
///    primitive) and downcasts the returned view-holder to the typed `Ref<dyn Svc>`.
///
/// A bean is thus injectable by its concrete type AND by any trait it provides
/// through the SAME `by_type`/resolve path — NO per-injection-point, per-bean, or
/// per-trait special-casing, and NO type-name detection (dispatch is purely on the
/// `Resolve` trait + the const `PRODUCED`/`upcast`, never a spelled name).
///
/// Implementors supply the resolved-target `TypeId` ([`PRODUCED`](Resolve::PRODUCED))
/// and the runtime [`resolve`](Resolve::resolve) that hands back the typed `Ref`.
/// `?Sized` so a `dyn Svc` view is a valid `X`; `Send + Sync + 'static` is the
/// shared-publication contract (a `dyn Svc` view trait must be `Send + Sync`, which
/// every service trait already is) so the resolved `Arc` can ride the erased holder.
pub trait Resolve: Send + Sync + 'static {
    /// The resolved-target `TypeId` the wave-planner reads (the concrete `T`, or the
    /// `dyn Svc` view's `TypeId::of::<dyn Svc>()`). Trait dispatch, never by name.
    const PRODUCED: TypeId;

    /// Resolve this target through the one [`ResolveCtx`] seam, handing back the
    /// shared [`Ref`] the constructor consumes.
    ///
    /// # Errors
    /// Any [`LeafError`] from the eager resolution (missing/ambiguous target, a
    /// construction fault, or a dropped container).
    fn resolve<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Ref<Self>, LeafError>>;
}

// SHAPE 1 — the concrete path: every `Bean` resolves by its own `TypeId` through
// `resolve_ref` (the EXISTING behavior of `Ref<T: Bean>`, byte-for-byte).
impl<T: Bean> Resolve for T {
    const PRODUCED: TypeId = const { TypeId::of::<T>() };

    fn resolve<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Ref<Self>, LeafError>> {
        Box::pin(async move { ctx.resolve_ref::<T>().await })
    }
}

/// Reconstitute a typed `Ref<X>` from the view-HOLDER [`ErasedBean`] that
/// [`Engine::resolve_view`](crate::Engine::resolve_view) returns (an
/// `Arc<Arc<X>>`): downcast the holder to the boxed `Arc<X>` and unwrap it.
///
/// The macro-emitted per-view upcast re-erases the providing bean's `Arc` as this
/// `Arc<Arc<dyn Svc>>` double-Arc, so the typed view is recovered with NO `unsafe`
/// and NO per-bean dispatch — the consumer only needs to know `X` (the view), which
/// the per-view [`Resolve`] impl does.
///
/// # Errors
/// [`ErrorKind::ConstructionFailed`] if the holder's payload is not the expected
/// `Arc<X>` view (a malformed upcast row — surfaced loudly).
pub fn view_from_holder<X: ?Sized + Send + Sync + 'static>(
    holder: ErasedBean,
) -> Result<Ref<X>, LeafError> {
    match holder.downcast::<Arc<X>>() {
        Ok(boxed) => Ok(Ref::from_arc(Arc::unwrap_or_clone(boxed))),
        Err(_) => Err(LeafError::new(ErrorKind::ConstructionFailed).caused_by(Cause::plain(
            "resolving by-trait view",
            "the resolved view-holder did not carry the expected `Arc<dyn Svc>` payload",
        ))),
    }
}

/// Emit the per-view [`Resolve`] impl for a `dyn Svc` trait ONCE (the by-trait-
/// injection seam): `impl_resolve_view!(dyn CacheManager)` makes `Ref<dyn CacheManager>`
/// injectable, resolving the view's `TypeId` through [`ResolveCtx::resolve_view`]
/// and downcasting the returned holder to the typed `Ref<dyn CacheManager>`.
///
/// Emitted exactly once per trait (orphan-rule-OK — `dyn Svc` is local to the
/// trait's crate). `#[injectable]` on a user `trait Foo` emits the same shape; this
/// macro is the hand-written equivalent for the framework's own traits.
#[macro_export]
macro_rules! impl_resolve_view {
    ($dyn_ty:ty) => {
        impl $crate::Resolve for $dyn_ty {
            const PRODUCED: ::core::any::TypeId =
                const { ::core::any::TypeId::of::<$dyn_ty>() };

            fn resolve<'__a>(
                ctx: &'__a $crate::ResolveCtx<'__a>,
            ) -> $crate::BoxFuture<'__a, ::core::result::Result<$crate::Ref<$dyn_ty>, $crate::LeafError>>
            {
                ::std::boxed::Box::pin(async move {
                    let __holder = ctx
                        .resolve_view(const { ::core::any::TypeId::of::<$dyn_ty>() })
                        .await?;
                    $crate::view_from_holder::<$dyn_ty>(__holder)
                })
            }
        }
    };
}

// SHAPE 2 (the blanket Injectable) — ONE impl over any `Resolve` target. This
// REPLACES the old `impl<T: Bean> Injectable for Ref<T>`: it still covers every
// concrete `Ref<T: Bean>` (via SHAPE 1) identically, and now ALSO covers every
// `Ref<dyn Svc>` whose trait carries a per-view `Resolve` impl. The two `Resolve`
// shapes do not conflict (a `dyn Trait` is not `Bean`), so this is coherent.
impl<X: ?Sized + Resolve> Injectable for Ref<X> {
    const RESOLVABLE: Resolvable = Resolvable {
        produced: <X as Resolve>::PRODUCED,
        cardinality: Cardinality::Single,
        strictness: Strictness::Strict,
    };

    fn inject<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Self, LeafError>> {
        // Eager: drive the target's own `resolve` through the one ResolveCtx seam.
        <X as Resolve>::resolve(ctx)
    }
}

impl<T: Bean> Injectable for Lookup<T> {
    const RESOLVABLE: Resolvable = Resolvable {
        produced: const { TypeId::of::<T>() },
        cardinality: Cardinality::Single,
        // Deferred/optional: the planner must NOT force T to exist — resolution
        // happens later via get_if_available/get_if_unique.
        strictness: Strictness::FullyTolerant,
    };

    fn inject<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Self, LeafError>> {
        // Deferred: build the handle from the ctx's container back-ref + the by-type
        // key. Always Ok (a missing T surfaces later, at the call site).
        Box::pin(async move {
            let container = ctx.container_ref()?;
            Ok(Lookup::new(ctx.key_for::<T>(), container))
        })
    }
}

impl<T: Bean> Injectable for LazyRef<T> {
    const RESOLVABLE: Resolvable = Resolvable {
        produced: const { TypeId::of::<T>() },
        cardinality: Cardinality::Single,
        // Deferred eager-single, resolved (and cached, for a singleton target) on
        // first use — like Lookup, the planner does not force eager presence here.
        strictness: Strictness::FullyTolerant,
    };

    fn inject<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Self, LeafError>> {
        // Deferred: mirror Lookup — build the handle now, resolve on first get().
        Box::pin(async move {
            let container = ctx.container_ref()?;
            Ok(LazyRef::new(ctx.key_for::<T>(), container))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{AnnotationMetadata, Descriptor, Role, ScopeDef};
    use crate::engine::Engine;
    use crate::error::{ErrorKind, Origin};
    use crate::handle::{Bean, Published};
    use crate::identity::{BeanKey, ContractId};
    use crate::injection::{Container, ContainerRef, DescriptorFilter, StreamOrder};
    use crate::provider::{Provider, ResolveCtx};
    use crate::registry::RegistryBuilder;
    use smallvec::SmallVec;
    use std::any::TypeId;
    use std::sync::Arc;

    #[derive(Debug, PartialEq)]
    struct Svc;
    impl Bean for Svc {}

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    // ── RESOLVABLE (the const descriptor the wave-planner reads) ─────────────

    #[test]
    fn ref_resolvable_targets_the_inner_bean_type_single_required() {
        let r = <Ref<Svc> as Injectable>::RESOLVABLE;
        assert_eq!(r.produced, TypeId::of::<Svc>());
        assert_eq!(r.cardinality, Cardinality::Single);
        assert_eq!(r.strictness, Strictness::Strict);
    }

    #[test]
    fn lookup_resolvable_is_a_soft_single_dependency() {
        let r = <Lookup<Svc> as Injectable>::RESOLVABLE;
        assert_eq!(r.produced, TypeId::of::<Svc>());
        // Lookup is deferred/optional: the planner must NOT force Svc to exist.
        assert_eq!(r.strictness, Strictness::FullyTolerant);
    }

    #[test]
    fn lazyref_resolvable_is_a_soft_single_dependency() {
        let r = <LazyRef<Svc> as Injectable>::RESOLVABLE;
        assert_eq!(r.produced, TypeId::of::<Svc>());
        assert_eq!(r.cardinality, Cardinality::Single);
        assert_eq!(r.strictness, Strictness::FullyTolerant);
    }

    // ── inject (runtime resolution through the one ResolveCtx seam) ──────────

    fn svc_descriptor() -> Descriptor {
        Descriptor {
            contract: ContractId::of("test::Svc"),
            self_type: TypeId::of::<Svc>(),
            provides: &[],
            declared_name: Some("svc"),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("test") },
        }
    }

    struct SvcProvider {
        descriptor: Descriptor,
    }
    impl Provider for SvcProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(Svc)) })
        }
    }

    #[test]
    fn ref_inject_resolves_a_registered_bean_through_the_ctx() {
        // The EAGER path: <Ref<Svc>>::inject resolves the registered Svc through the
        // engine back-reference the ResolveCtx carries, handing back the Ref<Svc>.
        let d = svc_descriptor();
        let mut builder = RegistryBuilder::new();
        builder.register(d, Arc::new(SvcProvider { descriptor: d })).unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        let cx = ResolveCtx::for_engine(&engine);

        let r: Ref<Svc> = block(<Ref<Svc> as Injectable>::inject(&cx)).expect("Ref resolves");
        assert_eq!(&*r, &Svc);
    }

    #[test]
    fn ref_inject_is_a_loud_error_with_no_engine_back_reference() {
        // No engine threaded → the eager Ref path fails loudly (never a silent unit).
        let cx = ResolveCtx::root();
        let err = block(<Ref<Svc> as Injectable>::inject(&cx)).expect_err("no engine");
        assert_eq!(err.kind, ErrorKind::ConstructionFailed);
    }

    // A container stub that reports Svc ABSENT — the deferred-guarantee witness:
    // a Lookup<Svc> must still BUILD even though Svc cannot resolve.
    struct AbsentContainer;
    impl Container for AbsentContainer {
        fn resolve(
            &self,
            _key: BeanKey,
            _strictness: Strictness,
            _cardinality: Cardinality,
        ) -> BoxFuture<'_, Result<Published, LeafError>> {
            Box::pin(async { Err(LeafError::new(ErrorKind::NoSuchBean)) })
        }
        fn resolve_many<'a>(
            &'a self,
            _key: BeanKey,
            _mode: StreamOrder,
            _filter: Option<DescriptorFilter<'a>>,
        ) -> BoxFuture<'a, Result<SmallVec<[Published; 4]>, LeafError>> {
            Box::pin(async { Ok(SmallVec::new()) })
        }
    }

    #[test]
    fn lookup_inject_builds_even_when_target_is_absent() {
        // The DEFERRED guarantee: <Lookup<Svc>>::inject is Ok even though Svc is
        // absent — resolution is deferred to first use (get_if_available → None).
        let arc: Arc<dyn Container> = Arc::new(AbsentContainer);
        let weak: ContainerRef = Arc::downgrade(&arc);
        let cx = ResolveCtx::root().with_container(weak);

        let handle: Lookup<Svc> =
            block(<Lookup<Svc> as Injectable>::inject(&cx)).expect("Lookup builds unconditionally");
        // The handle keys the by-type target the trait derived.
        assert_eq!(handle.key(), &BeanKey::ByType(TypeId::of::<Svc>()));
        // Deferred resolution: a missing Svc is tolerated as None, never an early fail.
        let resolved = block(handle.get_if_available()).expect("absence tolerated");
        assert!(resolved.is_none());
    }

    #[test]
    fn deferred_inject_is_a_loud_error_with_no_container_back_reference() {
        // A deferral handle cannot be built without a container back-ref — surfaced
        // loudly (never a silent dead handle).
        let cx = ResolveCtx::root();
        let err = block(<Lookup<Svc> as Injectable>::inject(&cx))
            .expect_err("no container back-reference");
        assert_eq!(err.kind, ErrorKind::ConstructionFailed);
    }

    // ── BY-TRAIT INJECTION: Ref<dyn UserTrait> resolves the providing bean ───────
    //
    // A user trait + a concrete bean providing it. The bean's descriptor declares a
    // `provides[]` TypeRow whose upcast coerces the concrete Arc into the `dyn Greet`
    // VIEW-HOLDER (Arc<Arc<dyn Greet>>) — exactly the macro-emitted shape. The trait
    // carries the per-view `Resolve` impl via `impl_resolve_view!`, so `Ref<dyn Greet>`
    // is `Injectable` and resolves through `Engine::resolve_view` (the ONE primitive).

    trait Greet: Send + Sync + 'static {
        fn greet(&self) -> &'static str;
    }

    // The per-view Resolve impl — one trait, emitted ONCE (orphan-rule-OK).
    crate::impl_resolve_view!(dyn Greet);

    #[derive(Debug)]
    struct English;
    impl Bean for English {}
    impl Greet for English {
        fn greet(&self) -> &'static str {
            "hello"
        }
    }

    #[derive(Debug)]
    struct French;
    impl Bean for French {}
    impl Greet for French {
        fn greet(&self) -> &'static str {
            "bonjour"
        }
    }

    // The macro-shaped `provides[]` upcast for `<Concrete> as dyn Greet`: downcast to
    // the concrete, unsize the Arc to `Arc<dyn Greet>`, re-erase as the double-Arc
    // view-holder. `TypeId::of` is not a stable const fn, so the row is built at
    // runtime + leaked to `&'static` (a real macro emits a const via the inline block).
    fn english_as_greet(bean: ErasedBean) -> ErasedBean {
        match bean.downcast::<English>() {
            Ok(c) => {
                let view: Arc<dyn Greet> = c;
                Arc::new(view) as ErasedBean
            }
            Err(orig) => orig,
        }
    }
    fn french_as_greet(bean: ErasedBean) -> ErasedBean {
        match bean.downcast::<French>() {
            Ok(c) => {
                let view: Arc<dyn Greet> = c;
                Arc::new(view) as ErasedBean
            }
            Err(orig) => orig,
        }
    }

    fn greet_provides(upcast: crate::definition::UpcastFn) -> &'static [crate::definition::TypeRow] {
        Box::leak(Box::new([crate::definition::TypeRow {
            view: TypeId::of::<dyn Greet>(),
            upcast,
        }]))
    }

    fn greeter_descriptor(
        contract: &str,
        name: &'static str,
        self_type: TypeId,
        provides: &'static [crate::definition::TypeRow],
        role: crate::definition::CandidateRole,
    ) -> Descriptor {
        // The candidate_role rides a leaked AnnotationMetadata so the registry's
        // FallbackDemote precedence (resolve_view_id) reads it.
        let meta: &'static AnnotationMetadata = Box::leak(Box::new(AnnotationMetadata {
            candidate_role: role,
            ..AnnotationMetadata::EMPTY
        }));
        Descriptor {
            contract: ContractId::of(contract),
            self_type,
            provides,
            declared_name: Some(name),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta,
            parent: None,
            origin: Origin::Native { crate_name: Some("test") },
        }
    }

    struct GreeterProvider<T: Bean + Default> {
        descriptor: Descriptor,
        _m: std::marker::PhantomData<fn() -> T>,
    }
    impl<T: Bean + Default> Provider for GreeterProvider<T> {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(T::default())) })
        }
    }
    impl Default for English {
        fn default() -> Self {
            English
        }
    }
    impl Default for French {
        fn default() -> Self {
            French
        }
    }

    #[test]
    fn ref_dyn_user_trait_is_injectable_with_a_view_typeid_resolvable() {
        // The RESOLVABLE of a `Ref<dyn Greet>` targets the VIEW's TypeId (the dyn
        // Greet TypeId), Single + Strict — the by-trait counterpart of the concrete
        // Ref<T> resolvable. Trait dispatch, never a spelled name.
        let r = <Ref<dyn Greet> as Injectable>::RESOLVABLE;
        assert_eq!(r.produced, TypeId::of::<dyn Greet>());
        assert_ne!(r.produced, TypeId::of::<English>());
        assert_eq!(r.cardinality, Cardinality::Single);
        assert_eq!(r.strictness, Strictness::Strict);
    }

    #[test]
    fn ref_dyn_user_trait_resolves_to_the_bean_providing_it_via_upcast() {
        // ONE bean providing `dyn Greet`: `Ref<dyn Greet>` resolves to it through the
        // SAME path as a concrete Ref<T> (Engine::resolve_view → the providing bean's
        // upcast → the typed view-holder), and the upcast preserves the concrete so
        // the trait method dispatches correctly.
        let d = greeter_descriptor(
            "test::English",
            "english",
            TypeId::of::<English>(),
            greet_provides(english_as_greet),
            crate::definition::CandidateRole::NORMAL,
        );
        let mut builder = RegistryBuilder::new();
        builder
            .register(
                d,
                Arc::new(GreeterProvider::<English> { descriptor: d, _m: std::marker::PhantomData }),
            )
            .unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        let cx = ResolveCtx::for_engine(&engine);

        let r: Ref<dyn Greet> =
            block(<Ref<dyn Greet> as Injectable>::inject(&cx)).expect("dyn view resolves");
        assert_eq!(r.greet(), "hello");
    }

    #[test]
    fn concrete_ref_is_unchanged_alongside_the_dyn_view_path() {
        // The concrete `Ref<English>` STILL resolves identically (the blanket
        // Resolve-over-Bean path is unchanged in behavior) even though `dyn Greet` is
        // also an injectable view — both inherit the one general primitive.
        let d = greeter_descriptor(
            "test::English",
            "english",
            TypeId::of::<English>(),
            greet_provides(english_as_greet),
            crate::definition::CandidateRole::NORMAL,
        );
        let mut builder = RegistryBuilder::new();
        builder
            .register(
                d,
                Arc::new(GreeterProvider::<English> { descriptor: d, _m: std::marker::PhantomData }),
            )
            .unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        let cx = ResolveCtx::for_engine(&engine);

        // Concrete path: unchanged.
        let concrete: Ref<English> =
            block(<Ref<English> as Injectable>::inject(&cx)).expect("concrete resolves");
        assert_eq!(concrete.greet(), "hello");
        // View path: same bean, via the view.
        let view: Ref<dyn Greet> =
            block(<Ref<dyn Greet> as Injectable>::inject(&cx)).expect("view resolves");
        assert_eq!(view.greet(), "hello");
    }

    #[test]
    fn two_beans_providing_one_view_disambiguate_by_fallback_precedence() {
        // English is a soft @Fallback, French is Normal — both provide `dyn Greet`.
        // The non-FALLBACK (French) wins, so `Ref<dyn Greet>` resolves to French (the
        // existing precedence: a soft fallback loses to a non-fallback of the view).
        let de = greeter_descriptor(
            "test::English",
            "english",
            TypeId::of::<English>(),
            greet_provides(english_as_greet),
            crate::definition::CandidateRole::FALLBACK,
        );
        let df = greeter_descriptor(
            "test::French",
            "french",
            TypeId::of::<French>(),
            greet_provides(french_as_greet),
            crate::definition::CandidateRole::NORMAL,
        );
        let mut builder = RegistryBuilder::new();
        builder
            .register(de, Arc::new(GreeterProvider::<English> { descriptor: de, _m: std::marker::PhantomData }))
            .unwrap();
        builder
            .register(df, Arc::new(GreeterProvider::<French> { descriptor: df, _m: std::marker::PhantomData }))
            .unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        let cx = ResolveCtx::for_engine(&engine);

        let r: Ref<dyn Greet> =
            block(<Ref<dyn Greet> as Injectable>::inject(&cx)).expect("non-fallback wins");
        assert_eq!(r.greet(), "bonjour");
    }

    #[test]
    fn two_non_fallback_beans_providing_one_view_are_no_unique_bean() {
        // Both English + French are Normal providers of `dyn Greet`: an unresolvable
        // ambiguity → NoUniqueBean (the registry refuses to guess; no name-match here).
        let de = greeter_descriptor(
            "test::English",
            "english",
            TypeId::of::<English>(),
            greet_provides(english_as_greet),
            crate::definition::CandidateRole::NORMAL,
        );
        let df = greeter_descriptor(
            "test::French",
            "french",
            TypeId::of::<French>(),
            greet_provides(french_as_greet),
            crate::definition::CandidateRole::NORMAL,
        );
        let mut builder = RegistryBuilder::new();
        builder
            .register(de, Arc::new(GreeterProvider::<English> { descriptor: de, _m: std::marker::PhantomData }))
            .unwrap();
        builder
            .register(df, Arc::new(GreeterProvider::<French> { descriptor: df, _m: std::marker::PhantomData }))
            .unwrap();
        let engine = Engine::from_builder(builder).unwrap();
        let cx = ResolveCtx::for_engine(&engine);

        // `Ref<dyn Greet>` is not Debug, so map the Ok arm away before expect_err.
        let err = block(<Ref<dyn Greet> as Injectable>::inject(&cx))
            .map(|_| ())
            .expect_err("ambiguous view is NoUniqueBean");
        assert_eq!(err.kind, ErrorKind::NoUniqueBean);
    }

    #[test]
    fn ref_dyn_view_inject_is_a_loud_error_with_no_engine_back_reference() {
        // No engine threaded → the view path fails loudly (mirrors the concrete path).
        let cx = ResolveCtx::root();
        let err = block(<Ref<dyn Greet> as Injectable>::inject(&cx))
            .map(|_| ())
            .expect_err("no engine");
        assert_eq!(err.kind, ErrorKind::ConstructionFailed);
    }
}
