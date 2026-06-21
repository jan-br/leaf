//! [`GrpcStatusMapper`] SPI + the [`DefaultGrpcStatusMapper`] `#[auto_config]` FALLBACK.

use leaf_core::LeafError;

use crate::status::{Code, Status};

/// The gRPC domain-error SPI (the [`leaf_web::ControlAdvice`] analogue): map a
/// [`LeafError`] (raised by a handler / filter / codec) into a [`Status`], or decline
/// (`None`) and let a later mapper — or the default FALLBACK — claim it. Collection-
/// injected (`Vec<Ref<dyn GrpcStatusMapper>>`), first-match-wins, the SAME DI way as
/// `ControlAdvice`. Reuses the `ErrorKind::Integration { kind_id }` domain-error
/// channel for app-specific kinds (e.g. unknown-SKU → `NotFound`).
pub trait GrpcStatusMapper: Send + Sync {
    /// Map `err` to a [`Status`], or `None` to decline.
    fn map(&self, err: &LeafError) -> Option<Status>;
}

// The by-trait-injection seam (emitted ONCE — orphan-rule-OK, `dyn GrpcStatusMapper`
// is local). A user mapper bean publishes the view; the gRPC edge collects every
// provider as `Vec<Ref<dyn GrpcStatusMapper>>` for its ordered first-match chain.
leaf_core::impl_resolve_view!(dyn GrpcStatusMapper);

/// The default gRPC status mapping — the FALLBACK floor (Spring's
/// `DefaultHandlerExceptionResolver`): `NoSuchBean`→[`Code::Unimplemented`],
/// `ConvertError`→[`Code::Internal`], everything else→[`Code::Unknown`]. A user
/// `GrpcStatusMapper` bean overrides it by claiming an error first. Dispatch is on the
/// typed [`ErrorKind`](leaf_core::ErrorKind), NEVER a textual name.
#[derive(Clone, Copy, Default)]
pub struct DefaultGrpcStatusMapper;

impl DefaultGrpcStatusMapper {
    /// A fresh default mapper (stateless).
    #[must_use]
    pub fn new() -> Self {
        DefaultGrpcStatusMapper
    }
}

impl GrpcStatusMapper for DefaultGrpcStatusMapper {
    fn map(&self, err: &LeafError) -> Option<Status> {
        // The FALLBACK claims EVERY error (it is the floor): a user mapper, ordered
        // earlier, gets first refusal; this always produces a valid Status.
        let status = match err.kind {
            leaf_core::ErrorKind::NoSuchBean => {
                Status::new(Code::Unimplemented, "no such method / resource")
            }
            leaf_core::ErrorKind::ConvertError => {
                Status::new(Code::Internal, "message decode/convert failed")
            }
            _ => Status::new(Code::Unknown, err.kind.slug()),
        };
        Some(status)
    }
}

/// The `#[auto_config]` HOLDER (a managed `#[component]` singleton). The
/// `#[auto_config] impl` below contributes the [`DefaultGrpcStatusMapper`] as the
/// FALLBACK `dyn GrpcStatusMapper`, gated by `OnMissingBean` so ANY user mapper
/// supersedes it — exactly like leaf-cache's `CacheAutoConfig` and
/// leaf-web-hyper's `HyperServerAutoConfig`.
#[leaf_macros::component]
pub struct GrpcStatusMapperAutoConfig;

impl GrpcStatusMapperAutoConfig {
    /// The no-collaborator constructor the `#[component]` provider calls.
    #[must_use]
    pub fn new() -> Self {
        GrpcStatusMapperAutoConfig
    }
}

impl Default for GrpcStatusMapperAutoConfig {
    fn default() -> Self {
        GrpcStatusMapperAutoConfig::new()
    }
}

#[leaf_macros::auto_config]
impl GrpcStatusMapperAutoConfig {
    /// Contribute the default mapper as the FALLBACK `dyn GrpcStatusMapper`. A user
    /// mapper (an ordinary bean providing the view) supersedes this default; this is
    /// the blessed floor so an app gets sane domain-error → Status mapping with NO
    /// hand-written mapper bean.
    #[bean(name = "defaultGrpcStatusMapper", provides = "dyn crate::GrpcStatusMapper")]
    #[conditional(on_missing_bean(dyn crate::GrpcStatusMapper))]
    fn default_grpc_status_mapper(&self) -> DefaultGrpcStatusMapper {
        DefaultGrpcStatusMapper::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Code;
    use leaf_core::{ErrorKind, LeafError};

    #[test]
    fn default_mapper_maps_the_framework_kinds() {
        let m = DefaultGrpcStatusMapper::new();
        // NoSuchBean (an unmatched-resource shape) → Unimplemented.
        let s = m.map(&LeafError::new(ErrorKind::NoSuchBean)).expect("claims NoSuchBean");
        assert_eq!(s.code, Code::Unimplemented);
        // ConvertError (a malformed body / decode fault) → Internal.
        let s = m.map(&LeafError::new(ErrorKind::ConvertError)).expect("claims ConvertError");
        assert_eq!(s.code, Code::Internal);
        // Anything else → Unknown (the floor, still a valid Status).
        let s = m.map(&LeafError::new(ErrorKind::ConstructionFailed)).expect("claims else→Unknown");
        assert_eq!(s.code, Code::Unknown);
    }

    #[test]
    fn default_mapper_is_a_fallback_auto_config_with_the_view() {
        use leaf_core::CandidateRole;
        // The dogfood claim: the default mapper reaches the SEPARATE auto-config channel
        // (NOT a hand-rolled Provider), at FALLBACK, carrying the dyn GrpcStatusMapper
        // view (so a user mapper supersedes it via OnMissingBean). The macro mints the
        // contract at the IMPL's module (`leaf_grpc::mapper`), not this nested `tests`.
        let impl_mod = module_path!().trim_end_matches("::tests");
        let contract = leaf_core::ContractId::of(&format!("{impl_mod}::default_grpc_status_mapper"));
        let bean = leaf_core::AUTO_CONFIGS
            .iter()
            .find(|d| d.contract == contract)
            .copied()
            .expect("the #[auto_config] default mapper reaches AUTO_CONFIGS");
        assert_eq!(
            bean.meta.candidate_role,
            CandidateRole::FALLBACK,
            "an auto-config registers at FALLBACK so a user mapper supersedes it"
        );
        assert!(
            bean.provides
                .iter()
                .any(|r| r.view == std::any::TypeId::of::<dyn GrpcStatusMapper>()),
            "the default mapper bean must declare the dyn GrpcStatusMapper view"
        );
    }
}
