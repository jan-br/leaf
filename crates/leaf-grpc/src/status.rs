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

    /// Render this status as the gRPC HTTP/2 trailers (`grpc-status` = the numeric
    /// [`Code`], `grpc-message` = the percent-encoded message). These are the trailing
    /// metadata a gRPC response carries — the [`Frame::Trailers`](leaf_web::Frame::Trailers)
    /// the gRPC edge appends to the response body stream. A tonic/grpc-go peer reads
    /// `grpc-status: <code as i32>` canonically.
    ///
    /// `grpc-message` is percent-encoded per the gRPC HTTP/2 spec (bytes outside
    /// `%x20-%x7E`, and `%` itself, become `%XX`) so an arbitrary message is always a
    /// valid ASCII [`HeaderValue`](http::HeaderValue). An empty message is omitted.
    #[must_use]
    pub fn to_trailers(&self) -> http::HeaderMap {
        let mut h = http::HeaderMap::new();
        // grpc-status is the numeric code (0..=16). The discriminant IS the wire number;
        // `HeaderValue: From<u16>` renders it as ASCII digits with no allocation.
        h.insert("grpc-status", http::HeaderValue::from(self.code as u16));
        // grpc-message is percent-encoded; an empty message carries no trailer.
        if !self.message.is_empty() {
            let encoded = percent_encode_grpc_message(&self.message);
            if let Ok(v) = http::HeaderValue::from_str(&encoded) {
                h.insert("grpc-message", v);
            }
        }
        h
    }
}

/// Percent-encode a grpc-message per the gRPC HTTP/2 protocol: any byte not in
/// `%x20-%x7E` (and `%` itself) becomes `%XX`. Pure, backend-free.
fn percent_encode_grpc_message(msg: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(msg.len());
    for &b in msg.as_bytes() {
        if (0x20..=0x7E).contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            // `write!` into a String is infallible; the result is discarded.
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
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
    fn status_renders_as_grpc_status_and_message_trailers() {
        let s = Status::new(Code::NotFound, "no such product");
        let trailers = s.to_trailers();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("5"),
            "grpc-status carries the numeric Code"
        );
        // Per the gRPC HTTP/2 spec, bytes 0x20..=0x7E (incl. SPACE) are NOT encoded;
        // only bytes outside that range and `%` itself become %XX. So an ASCII message
        // with spaces rides through literally — the load-bearing invariant is the
        // numeric grpc-status, and that an arbitrary message is a valid ASCII header.
        assert_eq!(
            trailers.get("grpc-message").and_then(|v| v.to_str().ok()),
            Some("no such product"),
            "grpc-message carries the (percent-encoded) message"
        );
    }

    #[test]
    fn grpc_message_percent_encodes_bytes_outside_the_printable_ascii_range() {
        // A newline (0x0A, below 0x20) and `%` (0x25) MUST be %XX-escaped so the value
        // is always a valid ASCII HeaderValue (the gRPC HTTP/2 spec's percent-encoding).
        let s = Status::new(Code::Internal, "a\nb%c");
        let trailers = s.to_trailers();
        assert_eq!(
            trailers.get("grpc-message").and_then(|v| v.to_str().ok()),
            Some("a%0Ab%25c"),
            "newline -> %0A, percent -> %25; printable ASCII passes through"
        );
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
