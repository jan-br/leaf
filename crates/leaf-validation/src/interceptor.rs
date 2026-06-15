//! The [`MethodValidationInterceptor`] — the OUTERMOST around-advice that validates
//! `@Valid` arguments BEFORE the call proceeds (validation, phase3/09 §validation,
//! FACE 2).
//!
//! The body, per phase3/09 §validation:
//!
//! 1. run the per-method [`ArgValidator`] over the call's [`ErasedArgs`] — it reaches
//!    the CONCRETE `@Valid` arg types through the proxy-substrate's typed pack/unpack
//!    glue (the erasure-free path: it downcasts the arg tuple and drives
//!    [`validate_root`](crate::validate_root) on each `@Valid` position), accumulating
//!    every [`Violation`] (collect-all);
//! 2. if ANY violation → SHORT-CIRCUIT with `Err(ValidationError)` aggregating ALL of
//!    them (the substrate short-circuit: `next.proceed()` is NEVER called, so a bad
//!    arg never reaches the inner tx/cache advisors or the real method);
//! 3. otherwise `next.proceed().await` — the validation advisor sits at
//!    [`VALIDATE_ORDER`](leaf_core::VALIDATE_ORDER) (OUTERMOST), so it runs before
//!    every other concern.
//!
//! It is pure sync CPU before an `.await`, holds no resource, and short-circuits on
//! failure — so there is no cancel-safety burden (no resource to release, no async
//! `Drop`).

use leaf_core::{AdviceError, BoxFuture, Call, ErasedArgs, ErasedRet, Interceptor, Next, Violation};

use crate::violations::aggregate;

/// A per-method argument validator: reads the call's [`ErasedArgs`] (the typed
/// positional tuple) and returns EVERY [`Violation`] across the `@Valid` arguments.
///
/// Built per advised method by [`arg_validator`] (or a hand-written closure for a
/// multi-arg method), baking in the concrete arg-tuple type so the validate path is
/// erasure-free — the same typed-closure-over-real-args shape caching's `key_fn` uses
/// (phase3/09 §validation: "delivers `&ConcreteArg`, so `Validate::validate` is
/// called directly on the typed path").
pub type ArgValidator = fn(&ErasedArgs) -> Vec<Violation>;

/// Build an [`ArgValidator`] for a single-`@Valid`-argument method whose erased arg
/// tuple is `(A,)` and whose sole argument `A` is [`ValidateInto`](crate::ValidateInto).
///
/// The monomorphized fn-item coerces to the bare [`ArgValidator`] fn-pointer. A
/// type-mismatch (the erased tuple is not `(A,)`) degrades to "no violations" (the
/// arg is simply not validated — never a panic); the pointcut + the generated glue
/// guarantee the type on the real path.
#[must_use]
pub fn arg_validator<A>() -> ArgValidator
where
    A: crate::ValidateInto + 'static,
{
    |args: &ErasedArgs| -> Vec<Violation> {
        match args.0.downcast_ref::<(A,)>() {
            Some((a,)) => crate::validate_root(a).violations().to_vec(),
            None => Vec::new(),
        }
    }
}

/// The OUTERMOST around-advice [`Interceptor`] that validates `@Valid` arguments
/// before the call proceeds (validation FACE 2).
///
/// Holds the per-method [`ArgValidator`] (the `#[validated]` macro would emit it; a
/// binding site supplies it via [`MethodValidationInterceptor::new`] until the macro
/// lands — a NOTE in the crate docs). On any violation it short-circuits with an
/// aggregated `ValidationError`; otherwise it proceeds unchanged.
pub struct MethodValidationInterceptor {
    validate_args: ArgValidator,
}

impl MethodValidationInterceptor {
    /// Build an interceptor over a per-method [`ArgValidator`].
    #[must_use]
    pub fn new(validate_args: ArgValidator) -> Self {
        MethodValidationInterceptor { validate_args }
    }

    /// An interceptor that validates a single-`@Valid`-arg method of arg type `A`
    /// (the common one-arg case; `A: ValidateInto`).
    #[must_use]
    pub fn single<A>() -> Self
    where
        A: crate::ValidateInto + 'static,
    {
        MethodValidationInterceptor::new(arg_validator::<A>())
    }
}

impl Interceptor for MethodValidationInterceptor {
    fn intercept<'a>(
        &'a self,
        call: &'a Call<'a>,
        mut next: Next<'a>,
    ) -> BoxFuture<'a, Result<ErasedRet, AdviceError>> {
        // Validate the @Valid args FIRST (pure sync CPU, before any .await).
        let violations = (self.validate_args)(&call.args);
        Box::pin(async move {
            if let Some(err) = aggregate("validating method arguments", &violations) {
                // SHORT-CIRCUIT: the bad arg never reaches the real method or the
                // inner advisors (next.proceed() is not called).
                return Err(AdviceError::AroundBody(err));
            }
            next.proceed(call).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use leaf_core::{
        AdviceChain, BeanKey, ContractId, ErrorKind, FixedTarget, MethodKey, ResolveCtx, Tail,
        ValidationContext,
    };

    use crate::cascade::Cascade;
    use crate::constraints;
    use crate::ValidateInto;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[derive(Debug)]
    struct Nop;
    impl leaf_core::Bean for Nop {}
    fn nop_target() -> FixedTarget {
        FixedTarget::new(Arc::new(Nop))
    }

    // The @Valid argument bean (Clone — the advised-arg bound: args ride Call.args +
    // are re-cloned per replay).
    #[derive(Debug, Clone)]
    struct CreateUser {
        name: String,
        age: i64,
    }
    impl ValidateInto for CreateUser {
        fn validate_into(&self, c: &mut Cascade<'_>) {
            c.check("name", constraints::not_empty(&self.name));
            c.check("age", constraints::range(self.age, 0, 150));
        }
    }

    fn drive(arg: CreateUser, ran: &Arc<AtomicBool>) -> Result<ErasedRet, AdviceError> {
        let ran = Arc::clone(ran);
        let interceptor: Arc<dyn Interceptor> =
            Arc::new(MethodValidationInterceptor::single::<CreateUser>());
        let chain = AdviceChain::new(Box::new([interceptor]));
        let target = nop_target();
        let cx = ResolveCtx::root();
        let call = Call::new(
            MethodKey::of("svc::create"),
            BeanKey::ByContract(ContractId::of("svc::Svc")),
            ErasedArgs::pack((arg,)),
            &target,
            &cx,
        );
        let tail: Box<Tail> = Box::new(move |_c: &Call<'_>| {
            ran.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(ErasedRet::pack(1_i64)) })
                as BoxFuture<'_, Result<ErasedRet, AdviceError>>
        });
        block(chain.invoke(&call, &*tail))
    }

    #[test]
    fn a_valid_arg_proceeds_to_the_real_method() {
        let ran = Arc::new(AtomicBool::new(false));
        let out = drive(CreateUser { name: "Jan".into(), age: 39 }, &ran);
        assert_eq!(out.expect("ok").unpack::<i64>().unwrap(), 1, "the method ran");
        assert!(ran.load(Ordering::SeqCst), "proceed() was called for a valid arg");
    }

    #[test]
    fn a_bad_arg_short_circuits_with_aggregated_violations() {
        let ran = Arc::new(AtomicBool::new(false));
        // BOTH fields bad → both violations aggregated.
        let out = drive(CreateUser { name: "".into(), age: 999 }, &ran);
        let err = out.expect_err("a bad arg short-circuits").into_leaf_error();
        assert_eq!(err.kind, ErrorKind::ValidationError, "an aggregated ValidationError");
        assert_eq!(err.chain.len(), 2, "BOTH violations aggregated (collect-all, not first-fail)");
        assert!(!ran.load(Ordering::SeqCst), "the real method NEVER ran (short-circuited)");
    }

    #[test]
    fn the_arg_validator_runs_validate_root_on_the_typed_arg() {
        // The typed path: the validator downcasts (CreateUser,) and validates it.
        let v = arg_validator::<CreateUser>();
        let bad = ErasedArgs::pack((CreateUser { name: "".into(), age: 5 },));
        assert_eq!(v(&bad).len(), 1, "the name violation");
        let good = ErasedArgs::pack((CreateUser { name: "ok".into(), age: 5 },));
        assert!(v(&good).is_empty());
    }

    #[test]
    fn a_type_mismatch_degrades_to_no_violations() {
        // A wrong-typed erased tuple is not validated (never a panic) — the pointcut
        // guarantees the type on the real path; this is the safe degrade.
        let v = arg_validator::<CreateUser>();
        let other = ErasedArgs::pack((7_i64,));
        assert!(v(&other).is_empty(), "an unexpected arg tuple yields no violations");
    }

    #[test]
    fn validate_root_is_the_engine_both_faces_share() {
        // Sanity: the interceptor's per-arg validation IS validate_root (the same
        // engine the config-bind face uses), proving "one engine, never two".
        let cx: ValidationContext = crate::validate_root(&CreateUser { name: "".into(), age: 5 });
        assert_eq!(cx.violations().len(), 1);
    }
}
