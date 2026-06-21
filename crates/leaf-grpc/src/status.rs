//! gRPC status: the grpc-status code space ([`Code`]) + a carried [`Status`].

/// The gRPC status code space (the `grpc-status` trailer integers 0–16). The
/// discriminants ARE the wire numbers — `Code::NotFound as i32 == 5` — so the edge
/// renders `grpc-status: <code as i32>` with no lookup table, and a tonic/grpc-go
/// peer reads them canonically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Code {
    /// Not an error; returned on success (`grpc-status: 0`).
    Ok = 0,
    /// The operation was cancelled (typically by the caller).
    Cancelled = 1,
    /// Unknown error (e.g. a `Status` from another address space with an unknown code).
    Unknown = 2,
    /// The client specified an invalid argument (irrespective of system state).
    InvalidArgument = 3,
    /// The deadline expired before the operation could complete.
    DeadlineExceeded = 4,
    /// A requested entity was not found.
    NotFound = 5,
    /// An entity a client attempted to create already exists.
    AlreadyExists = 6,
    /// The caller lacks permission to execute the operation.
    PermissionDenied = 7,
    /// A resource has been exhausted (quota, disk, …).
    ResourceExhausted = 8,
    /// The operation was rejected because the system is not in the required state.
    FailedPrecondition = 9,
    /// The operation was aborted (e.g. a concurrency conflict).
    Aborted = 10,
    /// The operation was attempted past the valid range.
    OutOfRange = 11,
    /// The operation is not implemented / not supported.
    Unimplemented = 12,
    /// An internal error (an invariant expected by the system was broken).
    Internal = 13,
    /// The service is currently unavailable (a transient condition).
    Unavailable = 14,
    /// Unrecoverable data loss or corruption.
    DataLoss = 15,
    /// The request does not have valid authentication credentials.
    Unauthenticated = 16,
}

/// A gRPC status carried out of a handler: a [`Code`] + a human message, rendered
/// at the edge as the `grpc-status` / `grpc-message` trailers. The error currency
/// of the gRPC layer (handlers return `Result<_, Status>`; the
/// [`GrpcStatusMapper`](crate::GrpcStatusMapper) SPI maps a
/// [`LeafError`](leaf_core::LeafError) into one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// The grpc-status code.
    pub code: Code,
    /// The grpc-message (a human-readable diagnostic; may be empty).
    pub message: String,
}

impl Status {
    /// A status with an explicit [`Code`] and message.
    #[must_use]
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Status { code, message: message.into() }
    }

    /// A [`Code::NotFound`] status — a requested entity was not found.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Status::new(Code::NotFound, message)
    }

    /// A [`Code::InvalidArgument`] status — the caller passed an invalid argument.
    #[must_use]
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Status::new(Code::InvalidArgument, message)
    }

    /// A [`Code::Internal`] status — an internal invariant was broken.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Status::new(Code::Internal, message)
    }

    /// A [`Code::Unimplemented`] status — the RPC is not implemented/supported.
    #[must_use]
    pub fn unimplemented(message: impl Into<String>) -> Self {
        Status::new(Code::Unimplemented, message)
    }
}

#[cfg(test)]
mod code_tests {
    use super::*;

    #[test]
    fn code_discriminants_match_the_grpc_status_wire_numbers() {
        // The grpc-status header carries these exact integers (the canonical
        // gRPC code space): a tonic/grpc-go peer reads `grpc-status: 5` as NotFound.
        assert_eq!(Code::Ok as i32, 0);
        assert_eq!(Code::Cancelled as i32, 1);
        assert_eq!(Code::Unknown as i32, 2);
        assert_eq!(Code::InvalidArgument as i32, 3);
        assert_eq!(Code::DeadlineExceeded as i32, 4);
        assert_eq!(Code::NotFound as i32, 5);
        assert_eq!(Code::AlreadyExists as i32, 6);
        assert_eq!(Code::PermissionDenied as i32, 7);
        assert_eq!(Code::ResourceExhausted as i32, 8);
        assert_eq!(Code::FailedPrecondition as i32, 9);
        assert_eq!(Code::Aborted as i32, 10);
        assert_eq!(Code::OutOfRange as i32, 11);
        assert_eq!(Code::Unimplemented as i32, 12);
        assert_eq!(Code::Internal as i32, 13);
        assert_eq!(Code::Unavailable as i32, 14);
        assert_eq!(Code::DataLoss as i32, 15);
        assert_eq!(Code::Unauthenticated as i32, 16);
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn status_new_carries_code_and_message() {
        let s = Status::new(Code::NotFound, "no such product");
        assert_eq!(s.code, Code::NotFound);
        assert_eq!(s.message, "no such product");
    }

    #[test]
    fn named_helpers_select_the_right_code() {
        // The ergonomic ctors used by handlers and the default mapper.
        assert_eq!(Status::not_found("x").code, Code::NotFound);
        assert_eq!(Status::invalid_argument("x").code, Code::InvalidArgument);
        assert_eq!(Status::internal("x").code, Code::Internal);
        assert_eq!(Status::unimplemented("x").code, Code::Unimplemented);
        // The message is carried through verbatim.
        assert_eq!(Status::internal("boom").message, "boom");
    }
}
