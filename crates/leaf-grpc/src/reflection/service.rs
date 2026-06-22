//! The two thin `#[grpc_controller]` reflection beans + the version-agnostic `Answer`
//! adapter over the shared `ReflectionIndex`. Both v1 and v1alpha have identical reflection
//! semantics but distinct generated Rust types; the adapter is written ONCE and each
//! controller assembles its own `ServerReflectionResponse` from it.

use prost::Message as _;

use super::index::ReflectionIndex;

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
