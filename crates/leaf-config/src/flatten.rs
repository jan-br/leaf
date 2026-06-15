//! The ONE canonical key-segment flattener (environment-config `extra-9`).
//!
//! Depth-first flatten of a tree document into stringly-typed `(key, value)`
//! pairs sharing ONE segment vocabulary with the binder's tree-descent and
//! relaxed-binding's index rules (charter #11):
//! - an object recurses `prefix.child` (dotted name segments);
//! - an array recurses `prefix[i]` (canonical file-style `[index]`, NOT the env
//!   underscore form);
//! - a scalar emits `PropertyValue { raw: scalar.to_string(), origin }`;
//! - a `null` EMITS NOTHING (null-as-absent baked at emit time);
//! - an EMPTY object/array emits nothing.
//!
//! Type info is erased to the raw string (the stack is stringly-typed; type
//! recovery is `FromConfigValue`'s job). Empty-string is present-and-wins.
//!
//! ## Fine `file:line` provenance
//!
//! A scalar optionally carries the 1-based SOURCE LINE it was parsed from (YAML
//! exposes it via yaml-rust2's `Marker`; JSON/others leave it `None`). The
//! flatten pass turns each value's provenance into an [`Origin`] via the
//! [`OriginSpec`]: a file source stamps a precise `Origin::File { path, line }`
//! (line `0` = "line unknown" when the parser exposed none — graceful
//! degradation, never a panic), while a programmatic/env source stamps its
//! always-available coarse [`Origin`] verbatim.

use leaf_core::{Origin, PropertyValue};

/// A single flattened entry: the canonical dotted/indexed key + its value.
pub type FlatEntry = (String, PropertyValue);

/// How [`flatten`] turns each scalar's parsed location into an [`Origin`].
///
/// Additive over the old single-`Origin` stamping: file loaders pass
/// [`OriginSpec::File`] (per-scalar `file:line`); env/config-tree/programmatic
/// sources pass [`OriginSpec::Coarse`] (the unchanged coarse carrier).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OriginSpec {
    /// Stamp this exact coarse [`Origin`] on every value (env / config-tree /
    /// programmatic) — the unchanged pre-existing behavior.
    Coarse(Origin),
    /// Stamp a fine `Origin::File { path, line }` per scalar; the line is the
    /// scalar's tracked source line, or `0` ("unknown") when the parser exposed
    /// none. `path` is the (process-`'static`) config file path.
    File {
        /// The config file path (already interned to `'static`).
        path: &'static str,
    },
}

impl OriginSpec {
    /// Resolve this spec for a scalar parsed at `line` (1-based; `None` =
    /// unknown) into the [`Origin`] stamped on that value.
    #[must_use]
    fn resolve(self, line: Option<u32>) -> Origin {
        match self {
            OriginSpec::Coarse(o) => o,
            OriginSpec::File { path } => Origin::File {
                path,
                line: line.unwrap_or(0),
            },
        }
    }
}

/// A minimal tree-document node the format loaders normalize into before the
/// ONE flatten pass — so JSON (`serde_json::Value`) and YAML (`yaml_rust2::Yaml`)
/// share the exact same flatten/segment/null-as-absent semantics.
#[derive(Clone, PartialEq, Debug)]
pub enum Node {
    /// A scalar already rendered to its canonical string form, with its optional
    /// 1-based source line (`None` when the parser exposed none — e.g. JSON).
    Scalar(String, Option<u32>),
    /// An explicit null (flattens to NOTHING — null-as-absent).
    Null,
    /// An ordered key→child map.
    Map(Vec<(String, Node)>),
    /// An ordered list of children.
    Seq(Vec<Node>),
}

impl Node {
    /// A scalar with no known source line (JSON/programmatic shape).
    #[must_use]
    pub fn scalar(s: impl Into<String>) -> Node {
        Node::Scalar(s.into(), None)
    }

    /// A scalar tagged with its 1-based source line (the YAML marked shape).
    #[must_use]
    pub fn scalar_at(s: impl Into<String>, line: u32) -> Node {
        Node::Scalar(s.into(), Some(line))
    }
}

/// Flatten `root` into canonical `(key, value)` pairs, stamping every value's
/// [`Origin`] per the [`OriginSpec`]. The top-level `prefix` is empty (so a
/// top-level map key is the bare key); nested keys join with `.` and list
/// positions with `[i]`.
#[must_use]
pub fn flatten(root: &Node, origin: OriginSpec) -> Vec<FlatEntry> {
    let mut out = Vec::new();
    walk("", root, origin, &mut out);
    out
}

fn walk(prefix: &str, node: &Node, origin: OriginSpec, out: &mut Vec<FlatEntry>) {
    match node {
        // null-as-absent: emit nothing.
        Node::Null => {}
        Node::Scalar(s, line) => {
            // A scalar at the very root (prefix empty) is a degenerate document;
            // it has no key, so it is dropped (matches Spring — a doc must be a
            // map at the top level to contribute keyed properties).
            if !prefix.is_empty() {
                out.push((
                    prefix.to_string(),
                    PropertyValue::with_origin(s.clone(), origin.resolve(*line)),
                ));
            }
        }
        Node::Map(entries) => {
            for (k, child) in entries {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                walk(&key, child, origin, out);
            }
        }
        Node::Seq(items) => {
            for (i, child) in items.iter().enumerate() {
                let key = format!("{prefix}[{i}]");
                walk(&key, child, origin, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(node: &Node) -> Vec<(String, String)> {
        flatten(node, OriginSpec::Coarse(Origin::Unknown))
            .into_iter()
            .map(|(k, v)| (k, v.raw.into_owned()))
            .collect()
    }

    #[test]
    fn flattens_nested_object_with_dotted_keys() {
        let node = Node::Map(vec![(
            "server".to_string(),
            Node::Map(vec![
                ("port".to_string(), Node::scalar("8080")),
                ("host".to_string(), Node::scalar("localhost")),
            ]),
        )]);
        assert_eq!(
            entries(&node),
            vec![
                ("server.port".to_string(), "8080".to_string()),
                ("server.host".to_string(), "localhost".to_string()),
            ]
        );
    }

    #[test]
    fn flattens_arrays_with_bracket_index_segments() {
        let node = Node::Map(vec![(
            "hosts".to_string(),
            Node::Seq(vec![Node::scalar("a"), Node::scalar("b")]),
        )]);
        assert_eq!(
            entries(&node),
            vec![
                ("hosts[0]".to_string(), "a".to_string()),
                ("hosts[1]".to_string(), "b".to_string()),
            ]
        );
    }

    #[test]
    fn null_is_absent_and_empty_containers_emit_nothing() {
        let node = Node::Map(vec![
            ("a".to_string(), Node::Null),
            ("b".to_string(), Node::Map(vec![])),
            ("c".to_string(), Node::Seq(vec![])),
            ("d".to_string(), Node::scalar("kept")),
        ]);
        assert_eq!(entries(&node), vec![("d".to_string(), "kept".to_string())]);
    }

    #[test]
    fn empty_string_is_present_and_kept() {
        let node = Node::Map(vec![("k".to_string(), Node::scalar(String::new()))]);
        assert_eq!(entries(&node), vec![("k".to_string(), String::new())]);
    }

    #[test]
    fn file_spec_stamps_per_scalar_line_and_path() {
        let node = Node::Map(vec![
            ("a".to_string(), Node::scalar_at("1", 4)),
            ("b".to_string(), Node::scalar("2")), // line unknown
        ]);
        let out = flatten(&node, OriginSpec::File { path: "x.yaml" });
        assert_eq!(out[0].1.origin, Origin::File { path: "x.yaml", line: 4 });
        // Unknown line degrades to 0, never a panic.
        assert_eq!(out[1].1.origin, Origin::File { path: "x.yaml", line: 0 });
    }

    #[test]
    fn coarse_spec_stamps_the_carrier_verbatim() {
        let node = Node::Map(vec![("k".to_string(), Node::scalar_at("v", 9))]);
        let carrier = Origin::Native { crate_name: Some("leaf-config::env") };
        let out = flatten(&node, OriginSpec::Coarse(carrier));
        // A coarse spec ignores the scalar line and stamps the carrier as-is.
        assert_eq!(out[0].1.origin, carrier);
    }
}
