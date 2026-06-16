//! `InjectableCtor` — the "magic constructor" trait that resolves a REFERENCED
//! constructor's parameters through [`Injectable`], so a
//! stereotype macro can name a constructor by path (`construct_with(Foo::new, ctx)`)
//! WITHOUT ever parsing or counting its parameters.
//!
//! The mechanism is axum-style: one blanket impl PER ARITY over `Fn(P1, …, Pn) -> T`
//! whose every parameter is [`Injectable`], each keyed by a distinct `Args` tuple
//! (`()`, `(P1,)`, `(P1, P2)`, …) so the impls do not overlap and a fn item has
//! exactly one arity. Type inference picks the arity (and `T`) from a bare
//! `Type::new` value, so the macro emits `construct_with(Type::new, ctx)` with NO
//! turbofish. The two free drivers ([`construct_with`]/[`ctor_deps`]) exist only so
//! the macro never has to spell the unspellable `Args`/`T`.

use crate::error::LeafError;
use crate::future::BoxFuture;
use crate::injectable::Injectable;
use crate::injection::InjectionPoint;
use crate::provider::ResolveCtx;

/// A constructor whose every parameter is [`Injectable`], resolvable WITHOUT the
/// caller (or the macro) ever spelling its parameter list.
///
/// Implemented once per arity over `Fn(P1, …, Pn) -> T`. `Args` is the parameter
/// tuple (`()`, `(P1,)`, …); `T` is the produced bean. Both are inferable from a
/// bare `Type::new` value, so the stereotype macro references a constructor by path
/// without parsing it. A parameter typed as a non-[`Injectable`] (a bare bean type)
/// makes the bound fail — steered to `Ref<T>` by the diagnostic below.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a constructor whose every parameter is `Injectable`",
    label = "this constructor cannot be resolved by `leaf`",
    note = "every parameter must be an injection handle — wrap a bean type as `Ref<T>` \
            (or `Lookup<T>`/`LazyRef<T>`), never a bare bean type",
    note = "or the constructor's arity may exceed the generated maximum (12)"
)]
pub trait InjectableCtor<Args, T>: Sized {
    /// Resolve every parameter via [`Injectable::inject`], then call the constructor.
    ///
    /// # Errors
    /// Returns a [`LeafError`] if any parameter fails to resolve (a missing/ambiguous
    /// collaborator or a construction fault); the constructor body itself is infallible.
    fn construct<'a>(self, ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<T, LeafError>>;

    /// The static dependency plan — each parameter's
    /// [`Injectable::RESOLVABLE`] lowered to an
    /// [`InjectionPoint`] with a positional name (`arg0`, `arg1`, …).
    ///
    /// Read from the fn value at ASSEMBLY (before any instantiation) so the
    /// wave-planner gets the dependency graph for cycle detection / ordering /
    /// whole-graph validation. It is a runtime `Vec` (not a const) because `Args` is
    /// unspellable at the macro call site, so the points cannot be named in a const
    /// position — the design choice is to compute them once, at assembly time.
    fn deps(&self) -> Vec<InjectionPoint>;
}

/// Build the positional [`InjectionPoint`] for the `i`-th parameter from its
/// [`Injectable::RESOLVABLE`] (trait dispatch — never type-name matching).
#[allow(non_snake_case)]
fn point_from<P: Injectable>(i: usize) -> InjectionPoint {
    let name: &'static str = match i {
        0 => "arg0",
        1 => "arg1",
        2 => "arg2",
        3 => "arg3",
        4 => "arg4",
        5 => "arg5",
        6 => "arg6",
        7 => "arg7",
        8 => "arg8",
        9 => "arg9",
        10 => "arg10",
        _ => "arg11",
    };
    <P as Injectable>::RESOLVABLE.into_point(name)
}

macro_rules! impl_injectable_ctor {
    ( $( $P:ident ),* ) => {
        impl<F, T, $($P),*> InjectableCtor<($($P,)*), T> for F
        where
            F: Fn($($P),*) -> T + Send + Sync + 'static,
            T: Send + 'static,
            $( $P: $crate::injectable::Injectable, )*
        {
            #[allow(non_snake_case, unused_variables)]
            fn construct<'a>(self, ctx: &'a ResolveCtx<'a>) -> BoxFuture<'a, Result<T, LeafError>> {
                Box::pin(async move {
                    $( let $P = <$P as $crate::injectable::Injectable>::inject(ctx).await?; )*
                    Ok(self( $($P),* ))
                })
            }

            #[allow(unused_mut, unused_variables, unused_assignments)]
            fn deps(&self) -> Vec<InjectionPoint> {
                let mut i = 0usize;
                let mut v = Vec::new();
                $( { let _ = ::core::stringify!($P); v.push(point_from::<$P>(i)); i += 1; } )*
                v
            }
        }
    };
}

impl_injectable_ctor!();
impl_injectable_ctor!(P1);
impl_injectable_ctor!(P1, P2);
impl_injectable_ctor!(P1, P2, P3);
impl_injectable_ctor!(P1, P2, P3, P4);
impl_injectable_ctor!(P1, P2, P3, P4, P5);
impl_injectable_ctor!(P1, P2, P3, P4, P5, P6);
impl_injectable_ctor!(P1, P2, P3, P4, P5, P6, P7);
impl_injectable_ctor!(P1, P2, P3, P4, P5, P6, P7, P8);
impl_injectable_ctor!(P1, P2, P3, P4, P5, P6, P7, P8, P9);
impl_injectable_ctor!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10);
impl_injectable_ctor!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11);
impl_injectable_ctor!(P1, P2, P3, P4, P5, P6, P7, P8, P9, P10, P11, P12);

/// Resolve a referenced constructor's parameters via [`Injectable`] and call it —
/// the inference driver the stereotype macro emits (`construct_with(Foo::new, cx)`),
/// so it never has to spell the unspellable `Args`/`T`.
///
/// # Errors
/// Returns a [`LeafError`] if any parameter fails to resolve.
pub fn construct_with<'a, F, Args, T>(
    ctor: F,
    ctx: &'a ResolveCtx<'a>,
) -> BoxFuture<'a, Result<T, LeafError>>
where
    F: InjectableCtor<Args, T>,
{
    ctor.construct(ctx)
}

/// The static dependency plan of a referenced constructor (each parameter's
/// [`Injectable::RESOLVABLE`] as a positional [`InjectionPoint`]) — the inference
/// driver the stereotype macro emits (`ctor_deps(Foo::new)`) for the wave-planner.
#[must_use]
pub fn ctor_deps<F, Args, T>(ctor: F) -> Vec<InjectionPoint>
where
    F: InjectableCtor<Args, T>,
{
    ctor.deps()
}

#[cfg(test)]
mod tests {
    use crate::definition::{AnnotationMetadata, Descriptor, Role, ScopeDef};
    use crate::engine::Engine;
    use crate::error::{LeafError, Origin};
    use crate::future::BoxFuture;
    use crate::handle::{Bean, Published, Ref};
    use crate::identity::ContractId;
    use crate::provider::{Provider, ResolveCtx};
    use crate::registry::RegistryBuilder;
    use std::any::TypeId;
    use std::sync::Arc;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    // ── the test beans, exactly as a user would write them (plain `fn new`) ──

    // arity 0 — STATE only, no injected params, no engine needed.
    struct StateOnly {
        n: i64,
    }
    impl StateOnly {
        fn new() -> Self {
            StateOnly { n: 7 }
        }
    }

    // a collaborator bean the arity-1 constructor injects via `Ref<Dep>`.
    #[derive(Debug, PartialEq)]
    struct Dep;
    impl Bean for Dep {}

    // arity 1 — one injected `Ref<Dep>` param.
    struct NeedsDep {
        dep: Ref<Dep>,
    }
    impl NeedsDep {
        fn new(dep: Ref<Dep>) -> Self {
            NeedsDep { dep }
        }
        fn dep_present(&self) -> bool {
            *self.dep == Dep
        }
    }

    // ── a real Engine-backed ResolveCtx that can resolve Dep ──

    fn dep_descriptor() -> Descriptor {
        Descriptor {
            contract: ContractId::of("test::Dep"),
            self_type: TypeId::of::<Dep>(),
            provides: &[],
            declared_name: Some("dep"),
            aliases: &[],
            scope: ScopeDef::SINGLETON,
            role: Role::Application,
            meta: &AnnotationMetadata::EMPTY,
            parent: None,
            origin: Origin::Native { crate_name: Some("test") },
        }
    }

    struct DepProvider {
        descriptor: Descriptor,
    }
    impl Provider for DepProvider {
        fn descriptor(&self) -> &Descriptor {
            &self.descriptor
        }
        fn provide<'a>(
            &'a self,
            _cx: &'a ResolveCtx<'a>,
        ) -> BoxFuture<'a, Result<Published, LeafError>> {
            Box::pin(async { Ok(Published::shared_value(Dep)) })
        }
    }

    fn engine_with_dep() -> Engine {
        let d = dep_descriptor();
        let mut builder = RegistryBuilder::new();
        builder.register(d, Arc::new(DepProvider { descriptor: d })).unwrap();
        Engine::from_builder(builder).unwrap()
    }

    #[test]
    fn construct_with_picks_arity_from_a_bare_new() {
        // arity 0 — state only, no engine needed. Inference must pick the arity
        // from the bare fn value, with NO turbofish.
        let ctx = ResolveCtx::root();
        let v0 = block(crate::construct_with(StateOnly::new, &ctx)).unwrap();
        assert_eq!(v0.n, 7);

        // arity 1 — Ref<Dep> resolved via Injectable through the engine.
        let engine = engine_with_dep();
        let ctx_with_dep = ResolveCtx::for_engine(&engine);
        let v1 = block(crate::construct_with(NeedsDep::new, &ctx_with_dep)).unwrap();
        assert!(v1.dep_present());
    }

    #[test]
    fn ctor_deps_reports_each_param_resolvable() {
        let d0 = crate::ctor_deps(StateOnly::new);
        let d1 = crate::ctor_deps(NeedsDep::new);
        assert!(d0.is_empty());
        assert_eq!(d1.len(), 1);
        // The single param is a `Ref<Dep>`, so it resolves the INNER `Dep` type
        // (derived from `<Ref<Dep> as Injectable>::RESOLVABLE`, never by name).
        assert_eq!(d1[0].produced, TypeId::of::<Dep>());
        // positional name, since a referenced constructor's params have no idents.
        assert_eq!(d1[0].name, "arg0");
    }
}
