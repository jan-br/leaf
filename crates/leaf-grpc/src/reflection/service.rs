//! The two thin `#[grpc_controller]` reflection beans + the version-agnostic `Answer`
//! adapter over the shared `ReflectionIndex`. Both v1 and v1alpha have identical reflection
//! semantics but distinct generated Rust types; the adapter is written ONCE and each
//! controller assembles its own `ServerReflectionResponse` from it.

use futures::StreamExt as _;
use prost::Message as _;

use leaf_core::{BoxStream, Ref};

use super::index::ReflectionIndex;
use super::{v1, v1alpha};
// Bring BOTH generated `ServerReflection` traits into scope (aliased — they share a name)
// so the generated `GrpcRoute` handler's `self.controller.server_reflection_info(..)` call
// resolves the trait method on `Ref<ReflectionV1>` / `Ref<ReflectionV1alpha>`.
use super::v1::ServerReflection as _;
use super::v1alpha::ServerReflection as _;
use crate::{Status, Streaming};

/// One resolved reflection answer — the version-agnostic payload the controllers render into
/// their version's `ServerReflectionResponse`. The `FileDescriptors` variant carries the
/// already-ENCODED `FileDescriptorProto` bytes (the matched file + its transitive dependency
/// closure, deduped) the wire format wants; `NotFound` is the reflection-level error
/// (code 5 == NOT_FOUND) — a normal response, NOT a transport `Status`.
#[derive(Debug)]
pub enum Answer {
    /// `file_by_filename` / `file_containing_symbol` / `file_containing_extension`: the
    /// encoded FileDescriptorProto bytes (the file + its dependency closure).
    FileDescriptors(Vec<Vec<u8>>),
    /// `list_services`: the fully-qualified service names.
    Services(Vec<String>),
    /// `all_extension_numbers_of_type`: the base type name + its extension field numbers.
    ExtensionNumbers {
        /// The fully-qualified type whose extension numbers were requested.
        base_type_name: String,
        /// The extension field numbers declared against it.
        numbers: Vec<i32>,
    },
    /// A reflection-level not-found (code 5 == NOT_FOUND): rendered as an `ErrorResponse`.
    NotFound {
        /// The gRPC status code (5 == NOT_FOUND).
        error_code: i32,
        /// The human-readable reflection error message.
        error_message: String,
    },
}

/// Encode a list of `FileDescriptorProto`s to the wire `bytes` the reflection response
/// carries (one encoded message per file).
fn encode_files(files: Vec<prost_types::FileDescriptorProto>) -> Vec<Vec<u8>> {
    files.into_iter().map(|f| f.encode_to_vec()).collect()
}

impl Answer {
    /// `list_services` — every fully-qualified service name in the index.
    #[must_use]
    pub fn list_services(index: &ReflectionIndex) -> Self {
        Answer::Services(index.list_services())
    }

    /// `file_by_filename` — the file + its transitive dependency closure, or NOT_FOUND.
    #[must_use]
    pub fn for_filename(index: &ReflectionIndex, name: &str) -> Self {
        match index.file_by_filename(name) {
            Some(files) => Answer::FileDescriptors(encode_files(files)),
            None => Answer::not_found(format!("file not found: {name}")),
        }
    }

    /// `file_containing_symbol` — the defining file + closure, or NOT_FOUND.
    #[must_use]
    pub fn for_symbol(index: &ReflectionIndex, symbol: &str) -> Self {
        match index.file_containing_symbol(symbol) {
            Some(files) => Answer::FileDescriptors(encode_files(files)),
            None => Answer::not_found(format!("symbol not found: {symbol}")),
        }
    }

    /// `file_containing_extension` — the file defining `(extendee, number)`, or NOT_FOUND.
    #[must_use]
    pub fn for_extension(index: &ReflectionIndex, extendee: &str, number: i32) -> Self {
        match index.file_containing_extension(extendee, number) {
            Some(files) => Answer::FileDescriptors(encode_files(files)),
            None => Answer::not_found(format!("extension not found: {extendee} {number}")),
        }
    }

    /// `all_extension_numbers_of_type` — the extension field numbers for `type_name`, or
    /// NOT_FOUND.
    #[must_use]
    pub fn for_all_extension_numbers(index: &ReflectionIndex, type_name: &str) -> Self {
        match index.all_extension_numbers_of_type(type_name) {
            Some(numbers) => Answer::ExtensionNumbers {
                base_type_name: type_name.to_string(),
                numbers,
            },
            None => Answer::not_found(format!("type not found: {type_name}")),
        }
    }

    /// The reflection NOT_FOUND marker (code 5 in the gRPC status-code space).
    #[must_use]
    fn not_found(message: impl Into<String>) -> Self {
        Answer::NotFound {
            error_code: 5,
            error_message: message.into(),
        }
    }
}

// ─────────────────── the shared ReflectionIndex bean (built once) ───────────────────

/// Provide the shared [`ReflectionIndex`] as a singleton bean, built ONCE from the Stage-1
/// `REFLECTED_FILE_DESCRIPTOR_SETS` discovery slice. A corrupt FDS that fails to decode is a
/// hard boot error (the descriptors are app-compiled, so a decode failure is internal). The
/// bean is UNCONDITIONAL — only the controllers that READ it are gated; the slice itself is
/// collected regardless of whether reflection is enabled. The dogfooded `#[component]` holder
/// + `#[configuration]`/`#[bean]` factory idiom (no hand-rolled `Provider`), the same shape
/// [`crate::dispatch::GrpcDispatchConfig`] uses.
#[leaf_macros::component]
pub struct ReflectionIndexConfig;

impl ReflectionIndexConfig {
    /// The no-collaborator constructor the `#[component]` provider calls.
    #[must_use]
    pub fn new() -> Self {
        ReflectionIndexConfig
    }
}

impl Default for ReflectionIndexConfig {
    fn default() -> Self {
        ReflectionIndexConfig::new()
    }
}

#[leaf_macros::configuration]
impl ReflectionIndexConfig {
    /// Build the shared [`ReflectionIndex`] from the link-collected discovery slice once.
    #[bean(name = "reflectionIndex")]
    fn reflection_index(&self) -> ReflectionIndex {
        let sets: &[&[u8]] = &crate::REFLECTED_FILE_DESCRIPTOR_SETS;
        ReflectionIndex::from_descriptor_sets(sets)
            .expect("the app's compiled FileDescriptorSets decode")
    }
}

// ─────────────────────────── the two thin controllers ───────────────────────────

/// The grpc.reflection.v1 controller — gated OFF by default; field-injects the shared index.
#[leaf_macros::grpc_controller]
#[leaf_macros::conditional(on_property("leaf.grpc.reflection.enabled", having_value = "true"))]
pub struct ReflectionV1 {
    index: Ref<ReflectionIndex>,
}

#[leaf_macros::grpc_controller]
#[leaf_macros::conditional(on_property("leaf.grpc.reflection.enabled", having_value = "true"))]
impl v1::ServerReflection for ReflectionV1 {
    async fn server_reflection_info(
        &self,
        requests: Streaming<v1::ServerReflectionRequest>,
    ) -> Result<Streaming<v1::ServerReflectionResponse>, Status> {
        let index = self.index.clone();
        let out: BoxStream<'static, Result<v1::ServerReflectionResponse, Status>> =
            Box::pin(async_stream_v1(requests, index));
        Ok(Streaming::new(out))
    }
}

/// The grpc.reflection.v1alpha controller — identical semantics, the v1alpha types.
#[leaf_macros::grpc_controller]
#[leaf_macros::conditional(on_property("leaf.grpc.reflection.enabled", having_value = "true"))]
pub struct ReflectionV1alpha {
    index: Ref<ReflectionIndex>,
}

#[leaf_macros::grpc_controller]
#[leaf_macros::conditional(on_property("leaf.grpc.reflection.enabled", having_value = "true"))]
impl v1alpha::ServerReflection for ReflectionV1alpha {
    async fn server_reflection_info(
        &self,
        requests: Streaming<v1alpha::ServerReflectionRequest>,
    ) -> Result<Streaming<v1alpha::ServerReflectionResponse>, Status> {
        let index = self.index.clone();
        let out: BoxStream<'static, Result<v1alpha::ServerReflectionResponse, Status>> =
            Box::pin(async_stream_v1alpha(requests, index));
        Ok(Streaming::new(out))
    }
}

// ─────────────────── the per-version request->response mappers ───────────────────

/// Render one v1 request -> one v1 response via the shared `Answer` adapter.
fn respond_v1(
    request: &v1::ServerReflectionRequest,
    index: &ReflectionIndex,
) -> v1::ServerReflectionResponse {
    use v1::server_reflection_request::MessageRequest;
    use v1::server_reflection_response::MessageResponse;

    let answer = match &request.message_request {
        Some(MessageRequest::ListServices(_)) => Answer::list_services(index),
        Some(MessageRequest::FileByFilename(name)) => Answer::for_filename(index, name),
        Some(MessageRequest::FileContainingSymbol(sym)) => Answer::for_symbol(index, sym),
        Some(MessageRequest::FileContainingExtension(ext)) => {
            Answer::for_extension(index, &ext.containing_type, ext.extension_number)
        }
        Some(MessageRequest::AllExtensionNumbersOfType(ty)) => {
            Answer::for_all_extension_numbers(index, ty)
        }
        None => Answer::NotFound {
            error_code: 3, // INVALID_ARGUMENT: a request with no message_request set.
            error_message: "empty reflection request".into(),
        },
    };

    let message_response = match answer {
        Answer::FileDescriptors(files) => {
            MessageResponse::FileDescriptorResponse(v1::FileDescriptorResponse {
                file_descriptor_proto: files,
            })
        }
        Answer::Services(names) => {
            MessageResponse::ListServicesResponse(v1::ListServiceResponse {
                service: names
                    .into_iter()
                    .map(|name| v1::ServiceResponse { name })
                    .collect(),
            })
        }
        Answer::ExtensionNumbers { base_type_name, numbers } => {
            MessageResponse::AllExtensionNumbersResponse(v1::ExtensionNumberResponse {
                base_type_name,
                extension_number: numbers,
            })
        }
        Answer::NotFound { error_code, error_message } => {
            MessageResponse::ErrorResponse(v1::ErrorResponse { error_code, error_message })
        }
    };
    v1::ServerReflectionResponse {
        valid_host: request.host.clone(),
        original_request: Some(request.clone()),
        message_response: Some(message_response),
    }
}

/// The v1 bidi body: map each inbound request to one response. A malformed inbound frame
/// (a `Status` from the de-framer) propagates as the stream's `Err` (a transport error).
fn async_stream_v1(
    requests: Streaming<v1::ServerReflectionRequest>,
    index: Ref<ReflectionIndex>,
) -> impl futures::Stream<Item = Result<v1::ServerReflectionResponse, Status>> {
    async_stream_helper! { requests, index, respond_v1 }
}

/// Render one v1alpha request -> one v1alpha response (identical logic, v1alpha types).
fn respond_v1alpha(
    request: &v1alpha::ServerReflectionRequest,
    index: &ReflectionIndex,
) -> v1alpha::ServerReflectionResponse {
    use v1alpha::server_reflection_request::MessageRequest;
    use v1alpha::server_reflection_response::MessageResponse;

    let answer = match &request.message_request {
        Some(MessageRequest::ListServices(_)) => Answer::list_services(index),
        Some(MessageRequest::FileByFilename(name)) => Answer::for_filename(index, name),
        Some(MessageRequest::FileContainingSymbol(sym)) => Answer::for_symbol(index, sym),
        Some(MessageRequest::FileContainingExtension(ext)) => {
            Answer::for_extension(index, &ext.containing_type, ext.extension_number)
        }
        Some(MessageRequest::AllExtensionNumbersOfType(ty)) => {
            Answer::for_all_extension_numbers(index, ty)
        }
        None => Answer::NotFound {
            error_code: 3,
            error_message: "empty reflection request".into(),
        },
    };
    let message_response = match answer {
        Answer::FileDescriptors(files) => {
            MessageResponse::FileDescriptorResponse(v1alpha::FileDescriptorResponse {
                file_descriptor_proto: files,
            })
        }
        Answer::Services(names) => {
            MessageResponse::ListServicesResponse(v1alpha::ListServiceResponse {
                service: names
                    .into_iter()
                    .map(|name| v1alpha::ServiceResponse { name })
                    .collect(),
            })
        }
        Answer::ExtensionNumbers { base_type_name, numbers } => {
            MessageResponse::AllExtensionNumbersResponse(v1alpha::ExtensionNumberResponse {
                base_type_name,
                extension_number: numbers,
            })
        }
        Answer::NotFound { error_code, error_message } => {
            MessageResponse::ErrorResponse(v1alpha::ErrorResponse { error_code, error_message })
        }
    };
    v1alpha::ServerReflectionResponse {
        valid_host: request.host.clone(),
        original_request: Some(request.clone()),
        message_response: Some(message_response),
    }
}

fn async_stream_v1alpha(
    requests: Streaming<v1alpha::ServerReflectionRequest>,
    index: Ref<ReflectionIndex>,
) -> impl futures::Stream<Item = Result<v1alpha::ServerReflectionResponse, Status>> {
    async_stream_helper! { requests, index, respond_v1alpha }
}

/// Map each inbound reflection request to one response over the bidi stream: a small
/// `unfold` that threads the inbound `Streaming<Req>` + the shared index, applying `$render`
/// per request. A malformed inbound frame (`Err(Status)` from de-framing) ends the stream by
/// yielding that transport `Status`.
macro_rules! async_stream_helper {
    ($requests:expr, $index:expr, $render:path) => {
        futures::stream::unfold(
            ($requests, $index),
            |(mut requests, index)| async move {
                match requests.next().await {
                    Some(Ok(req)) => {
                        let resp = $render(&req, &index);
                        Some((Ok(resp), (requests, index)))
                    }
                    Some(Err(status)) => Some((Err(status), (requests, index))),
                    None => None,
                }
            },
        )
    };
}
use async_stream_helper;
