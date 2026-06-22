//! The leaf service-trait generator — a pure `prost_build::ServiceGenerator` that
//! writes Rust source into a `String`, so every call-shape lowering is unit-testable
//! WITHOUT a compiler (the leaf-codegen discipline). It emits, per gRPC service: a
//! leaf-shaped server trait, the `/pkg.Service/Method` path constants, and the
//! `#[doc(hidden)]` per-method descriptors the `#[grpc_controller]` macro reads.

/// The RPC call shape — decided ONLY from the `client_streaming`/`server_streaming`
/// booleans the FileDescriptorSet carries, NEVER from a textual type name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallShape {
    /// `async fn m(&self, req: T) -> Result<U, Status>`
    Unary,
    /// `async fn m(&self, req: T) -> Result<Streaming<U>, Status>`
    ServerStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<U, Status>`
    ClientStream,
    /// `async fn m(&self, req: Streaming<T>) -> Result<Streaming<U>, Status>`
    Bidi,
}

impl CallShape {
    /// Classify from the two streaming flags (the FileDescriptorSet's `Method`).
    #[must_use]
    pub fn from_flags(client_streaming: bool, server_streaming: bool) -> Self {
        match (client_streaming, server_streaming) {
            (false, false) => CallShape::Unary,
            (false, true) => CallShape::ServerStream,
            (true, false) => CallShape::ClientStream,
            (true, true) => CallShape::Bidi,
        }
    }
}

/// The leaf request type for a call shape: `T` (unary/server-stream) wraps to
/// `leaf_grpc::Streaming<T>` for the client-streaming side. `input`/`output` are the
/// prost-resolved message type paths (emitted VERBATIM).
fn request_ty(input: &str, shape: CallShape) -> String {
    match shape {
        CallShape::Unary | CallShape::ServerStream => input.to_string(),
        CallShape::ClientStream | CallShape::Bidi => {
            format!("::leaf_grpc::Streaming<{input}>")
        }
    }
}

/// The leaf response type for a call shape: `Result<U, Status>` (unary/client-stream)
/// or `Result<Streaming<U>, Status>` (server-stream/bidi).
fn response_ty(output: &str, shape: CallShape) -> String {
    let inner = match shape {
        CallShape::Unary | CallShape::ClientStream => output.to_string(),
        CallShape::ServerStream | CallShape::Bidi => {
            format!("::leaf_grpc::Streaming<{output}>")
        }
    };
    format!("::core::result::Result<{inner}, ::leaf_grpc::Status>")
}

/// The full `async fn` signature line for one RPC (no body/semicolon).
fn method_signature(name: &str, input: &str, output: &str, shape: CallShape) -> String {
    format!(
        "async fn {name}(&self, req: {req}) -> {resp}",
        req = request_ty(input, shape),
        resp = response_ty(output, shape),
    )
}

/// The canonical gRPC method path `/package.Service/Method`. `package` may be empty
/// (then the path is `/Service/Method`).
fn grpc_path(package: &str, service: &str, method: &str) -> String {
    if package.is_empty() {
        format!("/{service}/{method}")
    } else {
        format!("/{package}.{service}/{method}")
    }
}

/// SCREAMING_SNAKE constant base for a method name (`Get` -> `GET`, `ListAll` ->
/// `LIST_ALL`). Pure case mechanics — NOT type-name detection (it derives an IDENT
/// from the proto's own method name, no behavior keyed on the text).
fn const_ident(method: &str) -> String {
    let mut out = String::new();
    for (i, ch) in method.chars().enumerate() {
        if ch.is_ascii_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

/// The `leaf_grpc::CallShape` const path for a shape.
fn shape_const_path(shape: CallShape) -> &'static str {
    match shape {
        CallShape::Unary => "::leaf_grpc::CallShape::Unary",
        CallShape::ServerStream => "::leaf_grpc::CallShape::ServerStream",
        CallShape::ClientStream => "::leaf_grpc::CallShape::ClientStream",
        CallShape::Bidi => "::leaf_grpc::CallShape::Bidi",
    }
}

/// The `pub const <METHOD>_PATH: &str = "/pkg.Service/Method";` line.
fn path_const(service: &str, package: &str, method: &str, _svc_alias: &str) -> String {
    let path = grpc_path(package, service, method);
    let ident = const_ident(method);
    format!("pub const {ident}_PATH: &str = {path:?};")
}

/// The `#[doc(hidden)]` const `leaf_grpc::MethodDescriptor` the `#[grpc_controller]`
/// macro reads: the canonical path + the call shape. Named
/// `<METHOD>_DESCRIPTOR` beside its `<METHOD>_PATH`.
fn method_descriptor(method: &str, package: &str, service: &str, shape: CallShape) -> String {
    let path = grpc_path(package, service, method);
    let ident = const_ident(method);
    let shape_path = shape_const_path(shape);
    format!(
        "#[doc(hidden)] pub const {ident}_DESCRIPTOR: ::leaf_grpc::MethodDescriptor = \
         ::leaf_grpc::MethodDescriptor {{ path: {path:?}, shape: {shape_path} }};"
    )
}

/// A single RPC, shape-classified and with its prost-resolved message paths — the
/// compiler-independent input the assembler renders (Task 3.6 builds these from a real
/// `prost_build::Service`).
#[derive(Clone, Debug)]
pub struct MethodSpec {
    /// The proto method name (`Get`) — drives the path + const idents.
    pub name: String,
    /// The Rust trait-method ident (`get`) — prost's snake_case fn name.
    pub fn_name: String,
    /// The prost-resolved request message path (emitted verbatim, e.g. `super::Ping`).
    pub input: String,
    /// The prost-resolved response message path.
    pub output: String,
    /// The call shape (from the streaming flags).
    pub shape: CallShape,
}

/// One gRPC service: its name/package + its methods.
#[derive(Clone, Debug)]
pub struct ServiceSpec {
    /// The proto service name (`Echo`) — the trait ident + path component.
    pub name: String,
    /// The proto package (`echo.v1`) — the path prefix (may be empty).
    pub package: String,
    /// The RPC methods.
    pub methods: Vec<MethodSpec>,
}

/// Snake-case the service name for its containing module (`Echo` -> `echo`,
/// `EchoService` -> `echo_service`). Pure case mechanics over the proto's OWN name.
fn module_ident(service: &str) -> String {
    let mut out = String::new();
    for (i, ch) in service.chars().enumerate() {
        if ch.is_ascii_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Render one service to Rust source: the `Send + Sync` server trait with one async
/// method per RPC, plus a `pub mod <service_snake>` holding the `<M>_PATH` constants
/// and the `#[doc(hidden)] <M>_DESCRIPTOR`s.
#[must_use]
pub fn render_service(svc: &ServiceSpec) -> String {
    let mut out = String::new();

    // ── the server trait (async-fn-in-trait so a `#[grpc_controller]` impl is plain) ──
    // `#[allow(async_fn_in_trait)]`: the trait is implemented ONLY in the app's own
    // crate (via `#[grpc_controller]`) — the lint's documented escape — so the missing
    // `Send` bound on the returned future is intentional, not a public-API hazard.
    out.push_str("#[allow(async_fn_in_trait)]\n");
    out.push_str(&format!("pub trait {}: Send + Sync {{\n", svc.name));
    for m in &svc.methods {
        out.push_str("    ");
        out.push_str(&method_signature(&m.fn_name, &m.input, &m.output, m.shape));
        out.push_str(";\n");
    }
    out.push_str("}\n\n");

    // ── the per-service module of path constants + method descriptors ──
    // `#![allow(dead_code)]`: the `<METHOD>_PATH` constants are a public-API convenience
    // (a caller may key on them, or not — the `#[grpc_controller]` macro reads only the
    // `<METHOD>_DESCRIPTOR`s). The module is `include!`d into the app crate, so rustc DOES
    // lint its unused consts; the allow keeps a generated artifact warning-free regardless
    // of which constants the downstream actually references.
    let module = module_ident(&svc.name);
    out.push_str(&format!("pub mod {module} {{\n"));
    out.push_str("    #![allow(dead_code)]\n");
    for m in &svc.methods {
        out.push_str("    ");
        out.push_str(&path_const(&svc.name, &svc.package, &m.name, &svc.name));
        out.push('\n');
        out.push_str("    ");
        out.push_str(&method_descriptor(&m.name, &svc.package, &svc.name, m.shape));
        out.push('\n');
    }
    out.push_str("}\n");

    out
}

/// Map a `prost_build::Service` to a [`ServiceSpec`]. The call shape comes ONLY from
/// each method's `client_streaming`/`server_streaming` flags (the FileDescriptorSet) —
/// never from a textual type name. `input_type`/`output_type` are prost's already-
/// resolved Rust paths, emitted verbatim.
#[must_use]
pub fn spec_from_service(svc: &::prost_build::Service) -> ServiceSpec {
    ServiceSpec {
        name: svc.name.clone(),
        package: svc.package.clone(),
        methods: svc
            .methods
            .iter()
            .map(|m| MethodSpec {
                name: m.proto_name.clone(),
                fn_name: m.name.clone(),
                input: m.input_type.clone(),
                output: m.output_type.clone(),
                shape: CallShape::from_flags(m.client_streaming, m.server_streaming),
            })
            .collect(),
    }
}

/// The SCREAMING_SNAKE static ident for a package's FDS registration row
/// (`echo.v1` -> `__LEAF_FDS_ECHO_V1`). PURE case mechanics over the package's OWN
/// dotted text — dots become `_`, letters/digits upper-case — NEVER type-name
/// detection (no behavior is keyed on the spelling; an empty package yields
/// `__LEAF_FDS_`). Deterministic + unique per package, so a second proto in the same
/// package would collide loudly (one FDS per package module, by construction).
#[must_use]
fn fds_static_ident(package: &str) -> String {
    let mut out = String::from("__LEAF_FDS_");
    for ch in package.chars() {
        if ch == '.' {
            out.push('_');
        } else {
            out.push(ch.to_ascii_uppercase());
        }
    }
    out
}

/// The dotted `<pkg>.fds` sibling file name prost-build's module naming implies
/// (`Module::to_file_name_or` joins package components with `.`, so the module is
/// `echo.v1.rs` and its FDS sibling is `echo.v1.fds`). The empty-package default-root
/// case is handled by `compile`, not here (this renders the dotted form).
#[must_use]
fn fds_file_name(package: &str) -> String {
    format!("{package}.fds")
}

/// `pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"),
/// "/<pkg>.fds"));` — the package's encoded `FileDescriptorSet`, embedded from the
/// sibling `.fds` `compile()` writes beside the generated `<pkg>.rs`.
#[must_use]
fn render_fds_const(package: &str) -> String {
    let file = fds_file_name(package);
    format!(
        "pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{file}\"));"
    )
}

/// The `#[distributed_slice]` row contributing this package's `FILE_DESCRIPTOR_SET` to
/// `leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS`. Routes linkme through the leaf-grpc
/// re-export (`#[linkme(crate = ::leaf_grpc::linkme)]`), exactly as `declare_source!`
/// routes through `::leaf_core::linkme`.
#[must_use]
fn render_fds_slice_row(package: &str) -> String {
    let ident = fds_static_ident(package);
    format!(
        "#[::leaf_grpc::linkme::distributed_slice(::leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS)]\n\
         #[linkme(crate = ::leaf_grpc::linkme)]\n\
         static {ident}: &[u8] = &FILE_DESCRIPTOR_SET;"
    )
}

/// The full FDS discovery block for one compiled package: the `FILE_DESCRIPTOR_SET`
/// const + its slice-registration row, wrapped in an inner `#[doc(hidden)] mod` whose
/// `#![allow(dead_code, non_upper_case_globals)]` covers BOTH items (the const is dead
/// static data unless reflection reads it; rust-analyzer lints the generated SCREAMING
/// static ident that rustc would skip — MEMORY: emit the allow on generated items). The
/// const is re-exported with `pub use` so `FILE_DESCRIPTOR_SET` stays reachable at the
/// package-module path the integration test (and the reflection index) reads. Emitted
/// ONCE per proto package by `compile()`.
#[must_use]
pub fn render_fds_block(package: &str) -> String {
    let module = format!(
        "__leaf_fds_{}",
        fds_static_ident(package)
            .trim_start_matches("__LEAF_FDS_")
            .to_ascii_lowercase()
    );
    let mut out = String::new();
    out.push_str(&format!("#[doc(hidden)]\npub mod {module} {{\n"));
    out.push_str("    #![allow(dead_code, non_upper_case_globals)]\n    ");
    out.push_str(&render_fds_const(package));
    out.push_str("\n    ");
    out.push_str(&render_fds_slice_row(package));
    out.push('\n');
    out.push_str("}\n");
    // The re-export is a public-API convenience (reflection + downstreams read it, or
    // not); when `echo.rs` is `include!`d into a crate that never names it, the `pub use`
    // trips `unused_imports`. The allow keeps the generated artifact warning-free
    // regardless of which consts the downstream references (same rationale as the service
    // module's `#![allow(dead_code)]` on the path constants).
    out.push_str("#[allow(unused_imports)]\n");
    out.push_str(&format!("pub use {module}::FILE_DESCRIPTOR_SET;\n"));
    out
}

/// The leaf `prost_build::ServiceGenerator`: for each gRPC service prost-build
/// encounters, append the rendered leaf server trait + path/descriptor module to the
/// output buffer (beside the message structs prost emits).
#[derive(Default)]
pub struct LeafServiceGenerator;

impl ::prost_build::ServiceGenerator for LeafServiceGenerator {
    fn generate(&mut self, service: ::prost_build::Service, buf: &mut String) {
        buf.push('\n');
        buf.push_str(&render_service(&spec_from_service(&service)));
        buf.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_all_four_shapes_from_the_streaming_flags() {
        assert_eq!(CallShape::from_flags(false, false), CallShape::Unary);
        assert_eq!(CallShape::from_flags(false, true), CallShape::ServerStream);
        assert_eq!(CallShape::from_flags(true, false), CallShape::ClientStream);
        assert_eq!(CallShape::from_flags(true, true), CallShape::Bidi);
    }

    #[test]
    fn emits_the_unary_method_signature() {
        let sig = method_signature("get", "super::Ping", "super::Pong", CallShape::Unary);
        assert_eq!(
            sig.split_whitespace().collect::<String>(),
            "asyncfnget(&self,req:super::Ping)->::core::result::Result<super::Pong,::leaf_grpc::Status>"
                .to_string()
        );
    }

    #[test]
    fn emits_the_server_stream_method_signature() {
        let sig = method_signature("list", "super::Ping", "super::Pong", CallShape::ServerStream);
        let flat = sig.split_whitespace().collect::<String>();
        assert!(
            flat.contains("req:super::Ping"),
            "server-stream request is a plain message: {flat}"
        );
        assert!(
            flat.contains("::leaf_grpc::Streaming<super::Pong>"),
            "server-stream response is Streaming<U>: {flat}"
        );
    }

    #[test]
    fn emits_the_client_stream_method_signature() {
        let sig = method_signature("upload", "super::Ping", "super::Pong", CallShape::ClientStream);
        let flat = sig.split_whitespace().collect::<String>();
        assert!(
            flat.contains("req:::leaf_grpc::Streaming<super::Ping>"),
            "client-stream request is Streaming<T>: {flat}"
        );
        assert!(
            flat.contains("->::core::result::Result<super::Pong"),
            "client-stream response is a plain message: {flat}"
        );
    }

    #[test]
    fn emits_the_bidi_method_signature() {
        let sig = method_signature("chat", "super::Ping", "super::Pong", CallShape::Bidi);
        let flat = sig.split_whitespace().collect::<String>();
        assert!(flat.contains("req:::leaf_grpc::Streaming<super::Ping>"), "got: {flat}");
        assert!(
            flat.contains("::core::result::Result<::leaf_grpc::Streaming<super::Pong>,::leaf_grpc::Status>"),
            "bidi response is Streaming<U>: {flat}"
        );
    }

    #[test]
    fn emits_the_grpc_path_constant() {
        let c = path_const("Echo", "echo.v1", "Get", "Echo");
        let flat = c.split_whitespace().collect::<String>();
        assert!(
            flat.contains(r#"pubconstGET_PATH:&str="/echo.v1.Echo/Get""#),
            "the path is the canonical /pkg.Service/Method literal: {flat}"
        );
    }

    #[test]
    fn emits_the_doc_hidden_method_descriptor() {
        let d = method_descriptor("Get", "echo.v1", "Echo", CallShape::Unary);
        let flat = d.split_whitespace().collect::<String>();
        assert!(flat.contains("#[doc(hidden)]"), "descriptor is doc-hidden: {flat}");
        assert!(
            flat.contains(r#"path:"/echo.v1.Echo/Get""#),
            "descriptor carries the path: {flat}"
        );
        assert!(
            flat.contains("shape:::leaf_grpc::CallShape::Unary"),
            "descriptor carries the call shape: {flat}"
        );
        assert!(
            flat.contains("::leaf_grpc::MethodDescriptor"),
            "descriptor is the leaf_grpc::MethodDescriptor type: {flat}"
        );
    }

    #[test]
    fn descriptor_shape_matches_the_streaming_flags_for_bidi() {
        let d = method_descriptor("Chat", "echo.v1", "Echo", CallShape::Bidi);
        assert!(
            d.split_whitespace().collect::<String>().contains("shape:::leaf_grpc::CallShape::Bidi"),
            "got: {d}"
        );
    }

    fn echo_spec() -> ServiceSpec {
        ServiceSpec {
            name: "Echo".into(),
            package: "echo.v1".into(),
            methods: vec![
                MethodSpec {
                    name: "Get".into(),
                    fn_name: "get".into(),
                    input: "super::Ping".into(),
                    output: "super::Pong".into(),
                    shape: CallShape::Unary,
                },
                MethodSpec {
                    name: "Chat".into(),
                    fn_name: "chat".into(),
                    input: "super::Msg".into(),
                    output: "super::Msg".into(),
                    shape: CallShape::Bidi,
                },
            ],
        }
    }

    #[test]
    fn renders_a_compilable_service_module() {
        let src = render_service(&echo_spec());
        // The whole rendered service must PARSE as a Rust item sequence (the
        // descriptor.rs discipline: emit data that parses without a compiler-link).
        syn::parse_str::<syn::File>(&src).expect("the rendered service is valid Rust items");
    }

    #[test]
    fn service_trait_is_send_sync_with_one_method_per_rpc() {
        let flat = render_service(&echo_spec()).split_whitespace().collect::<String>();
        assert!(flat.contains("pubtraitEcho:Send+Sync"), "trait is Send+Sync: {flat}");
        assert!(flat.contains("asyncfnget(&self,req:super::Ping)"), "unary method: {flat}");
        assert!(flat.contains("asyncfnchat(&self,req:::leaf_grpc::Streaming<super::Msg>)"), "bidi method: {flat}");
    }

    #[test]
    fn service_module_holds_paths_and_descriptors_per_method() {
        let flat = render_service(&echo_spec()).split_whitespace().collect::<String>();
        assert!(flat.contains(r#"pubconstGET_PATH:&str="/echo.v1.Echo/Get""#), "path const: {flat}");
        assert!(flat.contains(r#"pubconstCHAT_PATH:&str="/echo.v1.Echo/Chat""#), "path const: {flat}");
        assert!(flat.contains("GET_DESCRIPTOR:::leaf_grpc::MethodDescriptor"), "descriptor: {flat}");
        assert!(flat.contains("shape:::leaf_grpc::CallShape::Bidi"), "bidi descriptor: {flat}");
    }

    fn pb_method(name: &str, cs: bool, ss: bool) -> ::prost_build::Method {
        ::prost_build::Method {
            name: name.to_ascii_lowercase(),
            proto_name: name.to_string(),
            comments: ::prost_build::Comments {
                leading_detached: vec![],
                leading: vec![],
                trailing: vec![],
            },
            input_type: format!("super::{name}Req"),
            output_type: format!("super::{name}Resp"),
            input_proto_type: format!(".echo.v1.{name}Req"),
            output_proto_type: format!(".echo.v1.{name}Resp"),
            options: ::prost_types::MethodOptions::default(),
            client_streaming: cs,
            server_streaming: ss,
        }
    }

    #[test]
    fn renders_the_file_descriptor_set_const_including_the_fds_file() {
        let c = render_fds_const("echo.v1");
        let flat = c.split_whitespace().collect::<String>();
        assert!(
            flat.contains("pubconstFILE_DESCRIPTOR_SET:&[u8]"),
            "the const is the public FDS byte slice: {flat}"
        );
        assert!(
            flat.contains(r#"include_bytes!(concat!(env!("OUT_DIR"),"/echo.v1.fds"))"#),
            "the const embeds the sibling <pkg>.fds via include_bytes!: {flat}"
        );
    }

    #[test]
    fn renders_the_distributed_slice_registration_row_for_the_package() {
        let r = render_fds_slice_row("echo.v1");
        let flat = r.split_whitespace().collect::<String>();
        // The linkme attribute routes through leaf-grpc's re-export, like declare_source!'s
        // `#[linkme(crate = ::leaf_core::linkme)]` does for leaf-core's slices.
        assert!(
            flat.contains("#[::leaf_grpc::linkme::distributed_slice(::leaf_grpc::REFLECTED_FILE_DESCRIPTOR_SETS)]"),
            "the row joins the leaf-grpc discovery slice: {flat}"
        );
        assert!(
            flat.contains("#[linkme(crate=::leaf_grpc::linkme)]"),
            "the row pins linkme to the leaf-grpc re-export: {flat}"
        );
        // The static ident is the SCREAMING package, deterministic + unique per package.
        assert!(
            flat.contains("static__LEAF_FDS_ECHO_V1:&[u8]=&FILE_DESCRIPTOR_SET;"),
            "the row contributes the package's FILE_DESCRIPTOR_SET: {flat}"
        );
    }

    #[test]
    fn fds_static_ident_is_pure_case_mechanics_over_the_package_dots() {
        // No type-name detection: the ident is the package text upper-cased with dots->_.
        assert_eq!(fds_static_ident("echo.v1"), "__LEAF_FDS_ECHO_V1");
        assert_eq!(
            fds_static_ident("grpc.reflection.v1alpha"),
            "__LEAF_FDS_GRPC_REFLECTION_V1ALPHA"
        );
        assert_eq!(fds_static_ident(""), "__LEAF_FDS_");
    }

    #[test]
    fn the_emitted_fds_block_parses_as_rust_items() {
        let src = render_fds_block("echo.v1");
        syn::parse_str::<syn::File>(&src).expect("the FDS const + row parse as valid Rust items");
    }

    #[test]
    fn the_fds_block_reexports_the_const_at_the_package_path() {
        let flat = render_fds_block("echo.v1").split_whitespace().collect::<String>();
        assert!(
            flat.contains("pubuse"),
            "the const is re-exported to the package module path: {flat}"
        );
        assert!(
            flat.contains("::FILE_DESCRIPTOR_SET;"),
            "re-exports FILE_DESCRIPTOR_SET: {flat}"
        );
        assert!(
            flat.contains("#![allow(dead_code,non_upper_case_globals)]"),
            "inner allow covers both items: {flat}"
        );
    }

    #[test]
    fn maps_a_prost_service_to_a_spec_using_only_streaming_flags() {
        let svc = ::prost_build::Service {
            name: "Echo".into(),
            proto_name: "Echo".into(),
            package: "echo.v1".into(),
            comments: ::prost_build::Comments {
                leading_detached: vec![],
                leading: vec![],
                trailing: vec![],
            },
            methods: vec![pb_method("Get", false, false), pb_method("Chat", true, true)],
            options: ::prost_types::ServiceOptions::default(),
        };
        let spec = spec_from_service(&svc);
        assert_eq!(spec.name, "Echo");
        assert_eq!(spec.package, "echo.v1");
        assert_eq!(spec.methods[0].shape, CallShape::Unary);
        assert_eq!(spec.methods[0].fn_name, "get");
        assert_eq!(spec.methods[0].input, "super::GetReq");
        assert_eq!(spec.methods[1].shape, CallShape::Bidi);
        assert_eq!(spec.methods[1].name, "Chat");
    }
}
