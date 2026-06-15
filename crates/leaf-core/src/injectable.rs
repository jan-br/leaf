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

use crate::error::LeafError;
use crate::future::BoxFuture;
use crate::handle::{Bean, Ref};
use crate::injection::{Cardinality, LazyRef, Lookup, Strictness};
use crate::provider::ResolveCtx;
use std::any::TypeId;

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

impl<T: Bean> Injectable for Ref<T> {
    const RESOLVABLE: Resolvable = Resolvable {
        produced: const { TypeId::of::<T>() },
        cardinality: Cardinality::Single,
        strictness: Strictness::Strict,
    };

    fn inject<'a>(ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<Self, LeafError>> {
        // Eager: resolve T (Strict, Single) through the one ResolveCtx seam and hand
        // back the shared Ref<T> handle the constructor consumes.
        Box::pin(async move { ctx.resolve_ref::<T>().await })
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
}
