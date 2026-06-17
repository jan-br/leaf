//! The ONE creation seam: [`Provider`], [`ProviderSeed`], [`FactoryBean`].
//!
//! Realizes the toolkit's single creation primitive (`TOOLKIT.md`, registry-core
//! `bean-definition`/`factory-bean`): construction is **origin-agnostic** through
//! exactly one `dyn` seam. Native beans, [`FactoryBean`] products, test doubles,
//! and the WASM host-proxy ALL implement [`Provider`] and publish the identical
//! [`Published`] — "WASM-ness stops at the proxy"; the container cannot tell
//! origins apart because the stored shape is one type.
//!
//! [`Provider::provide`] returns a [`BoxFuture`] because async-fn-in-trait and
//! `-> impl Future` are not `dyn`-compatible (true regardless of nightly), so the
//! whole kernel boxes at the `dyn` boundary. The boxed-future alloc is cold for
//! singletons (once per container) and a per-resolution cost for prototypes
//! (accepted).
//!
//! The construction recipe on the const [`Descriptor`] is a [`ProviderSeed`] —
//! a **const fn-pointer that BUILDS a `Provider`**, never a live object. Keeping
//! the seed (not a live `Arc<dyn Provider>`) on the const row is what lets the
//! whole `Descriptor` be `const`; the registry calls the seed once at register/
//! freeze to mint the `Arc<dyn Provider>` it stores in the `providers` array.
//!
//! Scope note (definition-provider unit): [`ResolveCtx`] is a minimal placeholder
//! handle. The registry/engine units flesh it out (the `Engine` back-reference,
//! the ambient `Cx`/scope-store accessor, the in-creation guard, placeholder-
//! resolved `@Value` inputs); it is `#[non_exhaustive]` so those fields are added
//! without breaking `Provider` impls. The error type at this seam is the one
//! [`LeafError`] chain (later units add a `ResolveError` newtype/alias over it).

use std::any::TypeId;
use std::sync::Arc;

use crate::definition::Descriptor;
use crate::error::LeafError;
use crate::future::BoxFuture;
use crate::handle::Published;

/// The resolution-context handle threaded through every [`Provider::provide`].
///
/// Scope note: this is the minimal forward-compatible placeholder for the
/// definition-provider unit. The registry/engine units flesh it out (an `Engine`
/// back-reference for nested `get`, the ambient `Cx` + `scope_store` accessor,
/// the in-creation re-entrancy guard, and placeholder-resolved `@Value` inputs
/// the `Provider` reads — NOT definition edits). The lifetime is kept so adding
/// borrows later is not a breaking change; `#[non_exhaustive]` so adding fields
/// is not either.
#[non_exhaustive]
#[derive(Default)]
pub struct ResolveCtx<'a> {
    /// The engine back-reference for REAL nested resolution: a `Provider::provide`
    /// reads this to drive `Engine::get`/`Engine::create` for a collaborator,
    /// resolving the whole graph THROUGH the one engine (no test-side resolver
    /// mirror). Borrowed for `'a` — the engine owns `create`'s `&self`, so the
    /// child `ResolveCtx` it threads into `provide` simply re-borrows it. `None`
    /// on a `root()` cx (a bare provider drive with no nested resolution).
    engine: Option<&'a crate::engine::Engine>,
    /// The ambient scope-store accessor for `Multiplicity::PerContextKey` beans:
    /// the `ScopeKind` → `&dyn InstanceStore` binding the request/session layer
    /// installs at the scope boundary. `None` on a bare engine (so a scoped bean
    /// resolved with no store installed is a loud `ScopeMismatch`, never a silent
    /// singleton).
    scope_store: Option<&'a dyn ScopeStores>,
    /// The container back-reference threaded so a `Provider::provide` can build a
    /// DEFERRED resolution handle ([`Lookup`](crate::Lookup)/[`LazyRef`](crate::LazyRef))
    /// that re-resolves on demand. A [`Weak`](std::sync::Weak)
    /// [`Container`](crate::injection::Container) (no `Arc` cycle — the container owns
    /// the handles transitively). `None` on a bare `root()`/`for_engine` cx (no
    /// deferred-handle wiring installed), so building a deferral handle without it is
    /// a loud error, never a silent dead handle.
    container: Option<crate::injection::ContainerRef>,
    // A private marker binds the `'a` lifetime so the public signature is stable
    // before more borrowed fields are added. Zero-sized; costs nothing.
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> ResolveCtx<'a> {
    /// A root resolution context with no engine state bound yet.
    ///
    /// Used by tests and by the bare `Provider`-drive paths before context
    /// infrastructure is installed; the engine threads the richer constructors
    /// ([`for_engine`](ResolveCtx::for_engine)/[`with_scope_stores`](ResolveCtx::with_scope_stores)).
    #[must_use]
    pub fn root() -> Self {
        ResolveCtx {
            engine: None,
            scope_store: None,
            container: None,
            _marker: std::marker::PhantomData,
        }
    }

    /// A resolution context carrying the engine back-reference, so a
    /// `Provider::provide` can perform REAL nested resolution (`A`-depends-on-`B`-
    /// depends-on-`C`) THROUGH the one engine.
    #[must_use]
    pub fn for_engine(engine: &'a crate::engine::Engine) -> Self {
        ResolveCtx {
            engine: Some(engine),
            scope_store: None,
            container: None,
            _marker: std::marker::PhantomData,
        }
    }

    /// Derive a context that also carries the ambient scope-store accessor (the
    /// request/session layer's `ScopeKind` → [`InstanceStore`](crate::lifecycle_engine::InstanceStore)
    /// binding), preserving the engine back-reference.
    #[must_use]
    pub fn with_scope_stores(mut self, stores: &'a dyn ScopeStores) -> Self {
        self.scope_store = Some(stores);
        self
    }

    /// Derive a context that also carries the container back-reference a
    /// `Provider::provide` reads to build a DEFERRED resolution handle
    /// ([`Lookup`](crate::Lookup)/[`LazyRef`](crate::LazyRef)), preserving the engine
    /// and scope-store bindings. The [`Weak`](std::sync::Weak) is cheap to clone and
    /// holds no strong count (no `Arc` cycle).
    #[must_use]
    pub fn with_container(mut self, container: crate::injection::ContainerRef) -> Self {
        self.container = Some(container);
        self
    }

    /// The engine back-reference, if one is threaded (nested resolution driver).
    #[must_use]
    pub fn engine(&self) -> Option<&'a crate::engine::Engine> {
        self.engine
    }

    /// The by-type [`BeanKey`](crate::BeanKey) for `T` — the one place the
    /// `TypeId::of::<T>()` → `BeanKey::ByType` mapping lives, shared by the
    /// [`Injectable`](crate::Injectable) deferred-handle resolution and the generated
    /// providers.
    #[must_use]
    pub fn key_for<T: 'static>(&self) -> crate::identity::BeanKey {
        crate::identity::BeanKey::ByType(TypeId::of::<T>())
    }

    /// The container back-reference for building a DEFERRED resolution handle, or a
    /// loud [`LeafError`] if none is threaded.
    ///
    /// This is the seam [`Injectable::inject`](crate::Injectable::inject) reads for
    /// [`Lookup`](crate::Lookup)/[`LazyRef`](crate::LazyRef): a deferred handle holds
    /// a [`Weak`](std::sync::Weak) [`Container`](crate::injection::Container) and
    /// re-resolves on demand.
    ///
    /// # Errors
    /// [`ErrorKind::ConstructionFailed`](crate::ErrorKind::ConstructionFailed) if the
    /// cx carries no container back-reference (a deferral handle cannot be built
    /// without one — surfaced loudly, never as a silent dead handle).
    pub fn container_ref(&self) -> Result<crate::injection::ContainerRef, LeafError> {
        self.container.clone().ok_or_else(|| {
            LeafError::new(crate::error::ErrorKind::ConstructionFailed).caused_by(
                crate::error::Cause::plain(
                    "building a deferred injection handle",
                    "the resolution context carries no container back-reference",
                ),
            )
        })
    }

    /// Eagerly resolve `T` (Strict, Single) through the engine back-reference,
    /// returning the shared [`Ref<T>`](crate::Ref).
    ///
    /// The ONE eager-resolution seam shared by [`Injectable::inject`](crate::Injectable::inject)
    /// for [`Ref`](crate::Ref) and the generated providers, encapsulating the
    /// `engine().get::<T>()` round-trip so the resolution seam lives in one place.
    ///
    /// # Errors
    /// [`ErrorKind::ConstructionFailed`](crate::ErrorKind::ConstructionFailed) if no
    /// engine back-reference is threaded; otherwise any
    /// [`LeafError`] from the nested resolution (missing/ambiguous
    /// target or a construction fault).
    pub async fn resolve_ref<T: crate::handle::Bean>(
        &self,
    ) -> Result<crate::handle::Ref<T>, LeafError> {
        let engine = self.engine.ok_or_else(|| {
            LeafError::new(crate::error::ErrorKind::ConstructionFailed).caused_by(
                crate::error::Cause::plain(
                    "resolving an injected dependency",
                    "the resolution context carries no engine back-reference",
                ),
            )
        })?;
        engine.get::<T>().await
    }

    /// Resolve a `dyn Svc` VIEW `TypeId` through the engine back-reference,
    /// returning the view-HOLDER [`ErasedBean`](crate::ErasedBean) (an
    /// `Arc<Arc<dyn Svc>>`) the [`Injectable`](crate::Injectable) seam downcasts to
    /// a typed `Ref<dyn Svc>`.
    ///
    /// The view counterpart of [`resolve_ref`](ResolveCtx::resolve_ref): the SAME
    /// one resolution seam, delegating to [`Engine::resolve_view`](crate::Engine::resolve_view)
    /// (the by-trait-injection primitive). A `Ref<ConcreteType>` is unaffected — it
    /// keeps routing through `resolve_ref`.
    ///
    /// # Errors
    /// [`ErrorKind::ConstructionFailed`](crate::ErrorKind::ConstructionFailed) if no
    /// engine back-reference is threaded; otherwise any [`LeafError`] from the view
    /// resolution (missing/ambiguous provider or a construction fault).
    pub async fn resolve_view(&self, view: TypeId) -> Result<crate::handle::ErasedBean, LeafError> {
        let engine = self.engine.ok_or_else(|| {
            LeafError::new(crate::error::ErrorKind::ConstructionFailed).caused_by(
                crate::error::Cause::plain(
                    "resolving an injected dyn-view dependency",
                    "the resolution context carries no engine back-reference",
                ),
            )
        })?;
        engine.resolve_view(view).await
    }

    /// Resolve a `target` `TypeId` (concrete OR a `dyn Svc` VIEW) through the engine
    /// back-reference to ALL the beans providing it, one
    /// [`ErasedBean`](crate::ErasedBean) element per provider in
    /// [`cmp_order`](crate::cmp_order) — the [`Injectable`](crate::Injectable) seam
    /// recovers each into a typed `Ref<X>` for a `Vec<Ref<X>>` injection point.
    ///
    /// The COLLECTION counterpart of [`resolve_ref`](ResolveCtx::resolve_ref) /
    /// [`resolve_view`](ResolveCtx::resolve_view): the SAME one resolution seam,
    /// delegating to [`Engine::resolve_collection`](crate::Engine::resolve_collection)
    /// (the one general collection primitive). ZERO providers is an EMPTY `Vec`,
    /// never an error (collection semantics).
    ///
    /// # Errors
    /// [`ErrorKind::ConstructionFailed`](crate::ErrorKind::ConstructionFailed) if no
    /// engine back-reference is threaded; otherwise any [`LeafError`] from a
    /// provider's construction (never absence).
    pub async fn resolve_collection(
        &self,
        target: TypeId,
    ) -> Result<Vec<crate::handle::ErasedBean>, LeafError> {
        let engine = self.engine.ok_or_else(|| {
            LeafError::new(crate::error::ErrorKind::ConstructionFailed).caused_by(
                crate::error::Cause::plain(
                    "resolving an injected collection dependency",
                    "the resolution context carries no engine back-reference",
                ),
            )
        })?;
        engine.resolve_collection(target).await
    }

    /// The ambient [`InstanceStore`](crate::lifecycle_engine::InstanceStore) for
    /// `kind`, if a scope-store accessor is installed AND it binds `kind`.
    ///
    /// `Multiplicity::PerContextKey` resolution reads this; `None` means no
    /// ambient store is installed for the scope (a loud `ScopeMismatch`).
    #[must_use]
    pub fn scope_store(
        &self,
        kind: crate::definition::ScopeKind,
    ) -> Option<&'a dyn crate::lifecycle_engine::InstanceStore> {
        self.scope_store.and_then(|s| s.store_for(kind))
    }
}

/// The ambient scope-store accessor seam threaded onto [`ResolveCtx`]: maps a
/// [`ScopeKind`](crate::definition::ScopeKind) to the per-context
/// [`InstanceStore`](crate::lifecycle_engine::InstanceStore) the request/session
/// layer installs at the scope boundary (reached through the async-context `Cx`
/// binding). Object-safe so it rides the `ResolveCtx` as a `&dyn` borrow.
pub trait ScopeStores: Send + Sync {
    /// The ambient store backing `kind`, if this accessor binds it.
    fn store_for(
        &self,
        kind: crate::definition::ScopeKind,
    ) -> Option<&dyn crate::lifecycle_engine::InstanceStore>;
}

impl std::fmt::Debug for ResolveCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolveCtx").finish_non_exhaustive()
    }
}

/// THE one origin-agnostic creation seam.
///
/// Every way a bean can come into existence — a native constructor, a
/// [`FactoryBean`] product, a test double, the WASM host-proxy — is a `Provider`
/// the engine drives through one `provide` call. The engine cannot tell origins
/// apart; the `Origin` on the [`Descriptor`] is diagnostic-only and never read on
/// a resolution path.
///
/// `Send + Sync` because providers live in the frozen registry's
/// `Box<[Arc<dyn Provider>]>` and are driven from the (multi-threaded) executor.
pub trait Provider: Send + Sync {
    /// The const metamodel row this provider constructs.
    fn descriptor(&self) -> &Descriptor;

    /// Construct (and, fused, populate) one instance, publishing it as the
    /// origin-agnostic [`Published`].
    ///
    /// Boxed because async-fn-in-trait / RPITIT are not `dyn`-compatible. The
    /// returned future borrows `self` and `cx` for `'a`. `populate` is FUSED into
    /// `provide` (the macro-emitted factory's typed params ARE the injection
    /// points) — there is no separate populate step and no early-exposure cache.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if construction fails (a constructor-body fault, a
    /// failed nested resolution, or a cancelled task).
    fn provide<'a>(&'a self, cx: &'a ResolveCtx<'a>)
        -> BoxFuture<'a, Result<Published, LeafError>>;
}

/// A const fn-pointer that BUILDS a [`Provider`] — never a live object.
///
/// This is the construction recipe carried on the const [`Descriptor`] path: the
/// macro emits a `const SEED: ProviderSeed = || Arc::new(MyProvider::new());`.
/// Keeping a *seed* (not an `Arc<dyn Provider>`) is what makes the whole
/// `Descriptor` const-constructible; the registry invokes the seed exactly once
/// (at register/freeze) to mint the stored `Arc<dyn Provider>`. The typed factory
/// closure inside the built provider is opaque and fixed at the declaration site
/// — a BFPP-analogue rewrites METADATA, never this.
pub type ProviderSeed = fn() -> Arc<dyn Provider>;

/// A user-authored factory bean (registry-core `factory-bean`).
///
/// Realized as ONE [`Provider`] impl (a `FactoryBeanProvider`, owned by the
/// registry unit), never a second registry slot. The factory is itself a FULL
/// bean (its own `BeanId`, full lifecycle, resolved through the one creation
/// driver); its product's `TypeId` is emitted as a `provides[]`
/// [`TypeRow`](crate::definition::TypeRow) so candidate resolution finds the
/// product pre-construction (the getObjectType-without-getObject contract).
///
/// `create` is `async`, expressed here as a [`BoxFuture`] at the trait boundary
/// (AFIT is not `dyn`-compatible). A `null` product → the canonical `NULL_BEAN`
/// (extra-4, a later unit).
pub trait FactoryBean: Send + Sync {
    /// The concrete product type this factory yields.
    type Product: 'static;

    /// Build one product instance.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if the product cannot be constructed.
    fn create<'a>(&'a self, cx: &'a ResolveCtx<'a>)
        -> BoxFuture<'a, Result<Self::Product, LeafError>>;

    /// The product's `TypeId` (the type-match-without-realize contract; emitted
    /// as a `provides[]` row by the macro, also answerable at runtime here).
    fn product_type(&self) -> TypeId {
        TypeId::of::<Self::Product>()
    }

    /// Whether the product is a singleton (memoized) — the common case.
    ///
    /// `true` → the product memo lives in `registry.singletons[product_id]`;
    /// `false` → a prototype/scoped product (`Published::Owned`, re-created per
    /// resolve, no memo).
    fn is_singleton(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{Role, ScopeDef};
    use crate::handle::{downcast_owned, downcast_ref};
    use crate::identity::ContractId;
    use crate::error::{ErrorKind, Origin};

    // A concrete bean a test Provider builds.
    #[derive(Debug, PartialEq)]
    struct Widget {
        id: u32,
    }

    // A minimal const Descriptor row backing the test providers.
    fn widget_descriptor() -> Descriptor {
        Descriptor {
            contract: ContractId::of("test::Widget"),
            self_type: TypeId::of::<Widget>(),
            provides: &[],
            declared_name: Some("widget"),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &crate::definition::AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("leaf-core") },
        }
    }

    /// A test Provider that publishes a SHARED widget (the native-bean path).
    struct SharedWidgetProvider {
        descriptor: Descriptor,
    }

    impl Provider for SharedWidgetProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(Widget { id: 7 })) })
        }
    }

    /// A test Provider that publishes an OWNED widget (the prototype path).
    struct OwnedWidgetProvider {
        descriptor: Descriptor,
    }

    impl Provider for OwnedWidgetProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::owned(Widget { id: 99 })) })
        }
    }

    /// A test Provider that fails construction (the error path at the seam).
    struct FailingProvider {
        descriptor: Descriptor,
    }

    impl Provider for FailingProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Err(LeafError::new(ErrorKind::ConstructionFailed)) })
        }
    }

    #[test]
    fn provider_provides_a_shared_published() {
        let p = SharedWidgetProvider { descriptor: widget_descriptor() };
        let cx = ResolveCtx::root();
        let published = futures::executor::block_on(p.provide(&cx)).expect("provided");
        assert!(published.is_shared());
        let bean = published.into_shared().expect("shared handle");
        let w = downcast_ref::<Widget>(bean).expect("downcast");
        assert_eq!(w.id, 7);
    }

    #[test]
    fn provider_provides_an_owned_published() {
        let p = OwnedWidgetProvider { descriptor: widget_descriptor() };
        let cx = ResolveCtx::root();
        let published = futures::executor::block_on(p.provide(&cx)).expect("provided");
        assert!(published.is_owned());
        let boxed = published.into_owned().expect("owned box");
        let w = downcast_owned::<Widget>(boxed).expect("downcast owned");
        assert_eq!(w, Widget { id: 99 });
    }

    #[test]
    fn provider_error_is_a_leaf_error_at_the_seam() {
        let p = FailingProvider { descriptor: widget_descriptor() };
        let cx = ResolveCtx::root();
        let err = futures::executor::block_on(p.provide(&cx)).expect_err("must fail");
        assert_eq!(err.kind, ErrorKind::ConstructionFailed);
    }

    #[test]
    fn provider_is_object_safe_behind_arc_dyn() {
        // The whole point of the boxed-future seam: Provider is dyn-compatible.
        let p: Arc<dyn Provider> = Arc::new(SharedWidgetProvider { descriptor: widget_descriptor() });
        assert_eq!(p.descriptor().declared_name, Some("widget"));
        let cx = ResolveCtx::root();
        let published = futures::executor::block_on(p.provide(&cx)).expect("provided");
        assert!(published.is_shared());
    }

    #[test]
    fn provider_seed_builds_a_provider_lazily_not_a_live_object() {
        // A ProviderSeed is a const fn-pointer that BUILDS the Provider.
        const SEED: ProviderSeed =
            || Arc::new(SharedWidgetProvider { descriptor: widget_descriptor() });
        // Invoking the seed (once, at register/freeze) mints the Arc<dyn Provider>.
        let p: Arc<dyn Provider> = SEED();
        let cx = ResolveCtx::root();
        let published = futures::executor::block_on(p.provide(&cx)).expect("provided");
        assert!(published.is_shared());
        // The seed is callable repeatedly, each time minting a fresh provider.
        let p2 = SEED();
        assert!(!Arc::ptr_eq(&p, &p2));
    }

    // ── FactoryBean ────────────────────────────────────────────────────────────

    struct WidgetFactory;
    impl FactoryBean for WidgetFactory {
        type Product = Widget;
        fn create<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Self::Product, LeafError>> {
            Box::pin(async { Ok(Widget { id: 42 }) })
        }
    }

    #[test]
    fn factory_bean_creates_its_product_type() {
        let f = WidgetFactory;
        assert_eq!(f.product_type(), TypeId::of::<Widget>());
        assert!(f.is_singleton());
        let cx = ResolveCtx::root();
        let product = futures::executor::block_on(f.create(&cx)).expect("created");
        assert_eq!(product, Widget { id: 42 });
    }

    #[test]
    fn resolve_ctx_root_is_constructible_and_default() {
        let _cx = ResolveCtx::root();
        let _default = ResolveCtx::default();
        // Debug renders without panicking.
        assert!(format!("{:?}", ResolveCtx::root()).contains("ResolveCtx"));
    }
}
