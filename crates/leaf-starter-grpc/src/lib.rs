//! `leaf-starter-grpc` — a STACK starter (aggregator), the gRPC bundle.
//!
//! gRPC is a SECOND Handler family on the shared hyper WebServer (one server, one port),
//! so this bundle is the web stack (the http2-enabled hyper backend + the JSON converter
//! for same-port HTTP) PLUS the leaf-grpc engine: Status/Code, the length-prefix framing,
//! Streaming<T>, the GrpcDispatch (the dyn ProtocolDispatch the Dispatcher routes
//! `application/grpc` to), and the DefaultGrpcStatusMapper FALLBACK. Each crate's
//! auto-config participates + backs off independently.
//!
//! Like every starter, it depends only on its constituents and NEVER on the `leaf`
//! umbrella (the unique DAG sink). The umbrella's `grpc` capability feature `dep:`-pulls
//! this crate into the force-link / ExpectedManifest participating set.

#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[doc(no_inline)]
pub use leaf_grpc;
#[doc(no_inline)]
pub use leaf_starter_web;
