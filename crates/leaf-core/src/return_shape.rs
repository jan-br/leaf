//! The sealed [`ReturnShape`] trait: the alias-safe projection of an advised method's
//! WHOLE return type into its business `Ok`/`Err` shape (the tx-rollback / retry
//! classifier substrate).
//!
//! The cardinal rule (charter §2.x): a macro/codegen must NEVER decide behaviour from a
//! type's TEXTUAL NAME. The `#[transactional]` / `#[retryable]` codegen used to read the
//! advised method's return type and gate on `seg.ident == "Result"` to PEEL the `Ok`
//! type `T`, then bake `result_classifier::<T>()` into the rollback/retry decision. A
//! `type ApiResult<T> = Result<T, LeafError>;` alias defeats that: the classifier saw
//! the ident `ApiResult` (not `Result`), peeled NOTHING, and silently degraded to
//! "never a business failure" — so a business `Err` stopped rolling back / retrying with
//! NO compile error (the most dangerous class: silent at runtime).
//!
//! This trait moves the projection to the TYPE SYSTEM. The codegen stops peeling: it
//! emits the classifier keyed on the WHOLE return type `R` (the `sig.ret_type` token it
//! already holds) bounded `R: ReturnShape`, and the trait projects the `Err` internally.
//! A non-`Result` `#[transactional]` / `#[retryable]` method then fails to satisfy
//! `R: ReturnShape` — a CLEAR COMPILE ERROR (these classifiers are only meaningful for a
//! `Result`-returning method) rather than today's silent passthrough. The trait is
//! SEALED and implemented ONLY for `Result<T, LeafError>`, so an alias for it resolves
//! identically and nothing else can claim a spurious shape.

use std::any::Any;

use crate::error::LeafError;
use crate::proxy::ErasedRet;

mod sealed {
    /// Seals [`super::ReturnShape`] so only this crate's `Result<T, LeafError>` impl can
    /// claim it — a downstream type cannot fabricate a return shape.
    pub trait Sealed {}
    impl<T> Sealed for ::core::result::Result<T, crate::error::LeafError> {}
}

/// The business success/failure shape of an advised method's WHOLE return type.
///
/// Implemented ONLY for `Result<T, LeafError>` (sealed). The tx / retry advisors build
/// their return classifier keyed on `R` (the whole return type) and call
/// [`ReturnShape::classify_business_err`] to recover a business `Err` from the erased
/// return — NO `Result`-name peeling in the codegen, so a `type ApiResult<T> = …` alias
/// classifies identically and a non-`Result` return is a compile error.
pub trait ReturnShape: sealed::Sealed + Any + Send + Sized + 'static {
    /// Recover the business-`Err` [`LeafError`] from the erased return, if the method
    /// returned `Err` (the rollback / retry trigger), else `None` (an `Ok` return or a
    /// type mismatch — degrades to "treat as success", never a panic).
    fn classify_business_err(ret: &ErasedRet) -> Option<LeafError>;
}

impl<T: Any + Send + 'static> ReturnShape for Result<T, LeafError> {
    fn classify_business_err(ret: &ErasedRet) -> Option<LeafError> {
        ret.0
            .downcast_ref::<Result<T, LeafError>>()
            .and_then(|r| r.as_ref().err().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    fn err() -> LeafError {
        LeafError::new(ErrorKind::ConstructionFailed)
    }

    #[test]
    fn classifies_a_business_err_through_the_whole_result_type() {
        let ret = ErasedRet::pack::<Result<i64, LeafError>>(Err(err()));
        let got = <Result<i64, LeafError> as ReturnShape>::classify_business_err(&ret);
        assert!(got.is_some(), "a business Err is recovered from the whole Result type");
    }

    #[test]
    fn an_ok_return_is_not_a_business_err() {
        let ret = ErasedRet::pack::<Result<i64, LeafError>>(Ok(7));
        let got = <Result<i64, LeafError> as ReturnShape>::classify_business_err(&ret);
        assert!(got.is_none(), "an Ok return classifies as success");
    }

    // The cardinal-rule regression: a `Result` ALIAS projects IDENTICALLY (the trait is
    // keyed on the structural `Result<T, LeafError>`, never the spelled name `ApiResult`).
    type ApiResult<T> = Result<T, LeafError>;

    #[test]
    fn an_aliased_result_classifies_a_business_err_identically() {
        // The value is packed under the alias, recovered through the alias' `ReturnShape`
        // — which resolves to the SAME `Result<T, LeafError>` impl, so a business Err
        // still triggers the rollback/retry classification.
        let ret = ErasedRet::pack::<ApiResult<i64>>(Err(err()));
        let got = <ApiResult<i64> as ReturnShape>::classify_business_err(&ret);
        assert!(
            got.is_some(),
            "an aliased Result<T, LeafError> classifies a business Err identically to a bare Result"
        );
    }
}
