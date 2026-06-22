//! The reflection index: decode the FDS discovery slice → descriptor maps → queries.

use std::collections::HashMap;

use prost::Message;
use prost_types::{FileDescriptorProto, FileDescriptorSet};

/// An in-memory index over every served proto's descriptors, answering the gRPC
/// server-reflection queries.
///
/// Built once (from [`crate::REFLECTED_FILE_DESCRIPTOR_SETS`]) via
/// [`from_descriptor_sets`](ReflectionIndex::from_descriptor_sets); keyed on the FDS
/// WIRE symbol strings (the gRPC fully-qualified identifiers, e.g.
/// `storefront.catalog.Catalog`), NEVER on a Rust type name.
// The query maps are populated now and read by the `file_*`/`all_extension_numbers_of_type`
// queries landing in Tasks 2.3–2.6; the allow is removed once every field has a reader.
#[allow(dead_code)]
pub struct ReflectionIndex {
    /// `file name` (e.g. `storefront/catalog.proto`) → the decoded file.
    by_filename: HashMap<String, FileDescriptorProto>,
    /// fully-qualified symbol (message / enum / service / method / enum-value) → its
    /// defining `file name`.
    by_symbol: HashMap<String, String>,
    /// `(extendee FQN, field number)` → the defining `file name` of the extension.
    by_extension: HashMap<(String, i32), String>,
    /// `extendee FQN` → every extension field `number` declared against it.
    extension_numbers: HashMap<String, Vec<i32>>,
    /// every fully-qualified service name (for `list_services`).
    services: Vec<String>,
}

/// Join a package and a local name into a gRPC FQN (`pkg.Name`), or just `Name` when
/// the file declares no package. NEVER derived from a Rust type — `local` is the proto
/// `name` field straight off the descriptor.
fn fqn(package: &str, local: &str) -> String {
    if package.is_empty() {
        local.to_string()
    } else {
        format!("{package}.{local}")
    }
}

impl ReflectionIndex {
    /// Decode each `&[u8]` row as an encoded `FileDescriptorSet` and index every file,
    /// symbol, service and extension it carries.
    ///
    /// # Errors
    /// Returns a [`prost::DecodeError`] if any row is not a valid `FileDescriptorSet`.
    pub fn from_descriptor_sets(sets: &[&[u8]]) -> Result<ReflectionIndex, prost::DecodeError> {
        let mut by_filename = HashMap::new();
        let mut by_symbol = HashMap::new();
        let mut by_extension = HashMap::new();
        let mut extension_numbers: HashMap<String, Vec<i32>> = HashMap::new();
        let mut services = Vec::new();

        for bytes in sets {
            let set = FileDescriptorSet::decode(*bytes)?;
            for file in set.file {
                let file_name = file.name.clone().unwrap_or_default();
                let package = file.package.clone().unwrap_or_default();

                // Services (and their methods) — the FQNs for list_services + by_symbol.
                for svc in &file.service {
                    let svc_name = svc.name.clone().unwrap_or_default();
                    let svc_fqn = fqn(&package, &svc_name);
                    services.push(svc_fqn.clone());
                    by_symbol.insert(svc_fqn.clone(), file_name.clone());
                    for m in &svc.method {
                        let m_name = m.name.clone().unwrap_or_default();
                        by_symbol.insert(fqn(&svc_fqn, &m_name), file_name.clone());
                    }
                }

                // Top-level messages + enums (recursing into nested types).
                for msg in &file.message_type {
                    index_message(&package, msg, &file_name, &mut by_symbol);
                }
                for en in &file.enum_type {
                    let en_name = en.name.clone().unwrap_or_default();
                    by_symbol.insert(fqn(&package, &en_name), file_name.clone());
                }

                // File-level extensions: (extendee, number) → file, and extendee → [numbers].
                for ext in &file.extension {
                    if let (Some(extendee), Some(number)) = (&ext.extendee, ext.number) {
                        let key = normalize_symbol(extendee);
                        by_extension.insert((key.clone(), number), file_name.clone());
                        extension_numbers.entry(key).or_default().push(number);
                    }
                }

                by_filename.insert(file_name, file);
            }
        }

        Ok(ReflectionIndex {
            by_filename,
            by_symbol,
            by_extension,
            extension_numbers,
            services,
        })
    }

    /// Every fully-qualified service name across all indexed files.
    #[must_use]
    pub fn list_services(&self) -> Vec<String> {
        self.services.clone()
    }
}

/// Strip a leading `.` from a wire type reference. `input_type`/`output_type`/`extendee`
/// in an FDS are often written fully-qualified WITH a leading dot (`.pkg.Type`); the
/// reflection wire symbols are the dot-LESS form (`pkg.Type`), so we normalize both ends.
fn normalize_symbol(s: &str) -> String {
    s.strip_prefix('.').unwrap_or(s).to_string()
}

/// Index a message FQN and recurse into its nested messages + enums (each a symbol in
/// its own right, scoped under the parent: `pkg.Outer.Inner`).
fn index_message(
    scope: &str,
    msg: &prost_types::DescriptorProto,
    file_name: &str,
    by_symbol: &mut HashMap<String, String>,
) {
    let name = msg.name.clone().unwrap_or_default();
    let msg_fqn = fqn(scope, &name);
    by_symbol.insert(msg_fqn.clone(), file_name.to_string());
    for nested in &msg.nested_type {
        index_message(&msg_fqn, nested, file_name, by_symbol);
    }
    for en in &msg.enum_type {
        let en_name = en.name.clone().unwrap_or_default();
        by_symbol.insert(fqn(&msg_fqn, &en_name), file_name.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use prost_types::{
        DescriptorProto, FileDescriptorProto, FileDescriptorSet, MethodDescriptorProto,
        ServiceDescriptorProto,
    };

    /// A minimal message descriptor (name only).
    fn message(name: &str) -> DescriptorProto {
        DescriptorProto {
            name: Some(name.to_string()),
            ..Default::default()
        }
    }

    /// A minimal RPC method descriptor (`name`, `input_type`, `output_type`).
    fn method(name: &str, input: &str, output: &str) -> MethodDescriptorProto {
        MethodDescriptorProto {
            name: Some(name.to_string()),
            input_type: Some(input.to_string()),
            output_type: Some(output.to_string()),
            ..Default::default()
        }
    }

    /// A service descriptor with the given methods.
    fn service(name: &str, methods: Vec<MethodDescriptorProto>) -> ServiceDescriptorProto {
        ServiceDescriptorProto {
            name: Some(name.to_string()),
            method: methods,
            ..Default::default()
        }
    }

    /// The `storefront.catalog` file: a Catalog service over Get/List + its messages.
    fn catalog_file() -> FileDescriptorProto {
        FileDescriptorProto {
            name: Some("storefront/catalog.proto".to_string()),
            package: Some("storefront.catalog".to_string()),
            message_type: vec![
                message("GetRequest"),
                message("GetResponse"),
                message("ListRequest"),
                message("ListResponse"),
            ],
            service: vec![
                service(
                    "Catalog",
                    vec![
                        method(
                            "Get",
                            ".storefront.catalog.GetRequest",
                            ".storefront.catalog.GetResponse",
                        ),
                        method(
                            "List",
                            ".storefront.catalog.ListRequest",
                            ".storefront.catalog.ListResponse",
                        ),
                    ],
                ),
                service(
                    "Admin",
                    vec![method(
                        "Reindex",
                        ".storefront.catalog.GetRequest",
                        ".storefront.catalog.GetResponse",
                    )],
                ),
            ],
            ..Default::default()
        }
    }

    /// Encode one FileDescriptorProto into a one-file FileDescriptorSet's bytes.
    fn encode_set(files: Vec<FileDescriptorProto>) -> Vec<u8> {
        FileDescriptorSet { file: files }.encode_to_vec()
    }

    #[test]
    fn list_services_returns_fully_qualified_names() {
        let bytes = encode_set(vec![catalog_file()]);
        let index = ReflectionIndex::from_descriptor_sets(&[&bytes]).unwrap();

        let mut services = index.list_services();
        services.sort();
        assert_eq!(
            services,
            vec![
                "storefront.catalog.Admin".to_string(),
                "storefront.catalog.Catalog".to_string(),
            ]
        );
    }

    #[test]
    fn from_descriptor_sets_propagates_a_decode_error_on_corrupt_bytes() {
        // A truncated/garbage protobuf the FileDescriptorSet decoder rejects.
        let garbage: &[u8] = &[0xff, 0xff, 0xff, 0xff];
        let err = ReflectionIndex::from_descriptor_sets(&[garbage]);
        assert!(err.is_err(), "corrupt FDS bytes must surface a DecodeError");
    }
}
