//! Fine `file:line` provenance: a value loaded from a YAML fixture must carry an
//! [`Origin::File`] whose line points at the exact source line, and that origin
//! must round-trip through an [`OriginStore`]. JSON degrades gracefully to a
//! path-only file origin (serde_json exposes no per-value line).
//!
//! This proves the deferred-NOTE gap (leaf-config/src/lib.rs ~39): the loaders
//! now stamp the precise `file:line` instead of only the coarse `Origin::Native`
//! carrier.

use leaf_config::{ConfigDataLocation, JsonLoader, LoadCtx, SyncConfigDataLoader, YamlLoader};
use leaf_core::{Origin, OriginStore};

/// Find the value + origin for `key` in a loaded document.
fn value_origin(
    loader: &dyn SyncConfigDataLoader,
    raw_loc: &str,
    text: &str,
    key: &str,
) -> (String, Origin) {
    let loc = ConfigDataLocation::parse(raw_loc);
    let cx = LoadCtx::inline(text);
    let docs = loader.load_sync(&loc, &cx).unwrap();
    for doc in docs {
        for (k, v) in doc.props {
            if k == key {
                return (v.raw.into_owned(), v.origin);
            }
        }
    }
    panic!("key {key:?} not found");
}

#[test]
fn yaml_value_carries_file_line_origin_resolvable_through_origin_store() {
    // Line 1: server:
    // Line 2:   port: 8080   <-- the value lives here
    // Line 3: name: leaf
    let yaml = "server:\n  port: 8080\nname: leaf\n";
    let (raw, origin) = value_origin(&YamlLoader, "application.yaml", yaml, "server.port");
    assert_eq!(raw, "8080");

    // The value carries a fine file:line origin (not the coarse Native carrier).
    match origin {
        Origin::File { path, line } => {
            assert_eq!(path, "application.yaml");
            assert_eq!(line, 2, "the scalar `8080` is on source line 2");
        }
        other => panic!("expected Origin::File, got {other:?}"),
    }

    // …and it round-trips through an OriginStore (intern → resolve).
    let mut store = OriginStore::new();
    let id = store.intern(origin);
    assert_eq!(*store.resolve(id), origin);
}

#[test]
fn yaml_top_level_scalar_line_is_tracked_too() {
    let yaml = "server:\n  port: 8080\nname: leaf\n";
    let (_raw, origin) = value_origin(&YamlLoader, "application.yaml", yaml, "name");
    match origin {
        Origin::File { line, .. } => assert_eq!(line, 3, "`name: leaf` is on line 3"),
        other => panic!("expected Origin::File, got {other:?}"),
    }
}

#[test]
fn yaml_array_elements_carry_their_own_lines() {
    // hosts:        line 1
    //   - a         line 2
    //   - b         line 3
    let yaml = "hosts:\n  - a\n  - b\n";
    let (_, o0) = value_origin(&YamlLoader, "application.yaml", yaml, "hosts[0]");
    let (_, o1) = value_origin(&YamlLoader, "application.yaml", yaml, "hosts[1]");
    let (Origin::File { line: l0, .. }, Origin::File { line: l1, .. }) = (o0, o1) else {
        panic!("expected File origins");
    };
    assert_eq!(l0, 2);
    assert_eq!(l1, 3);
}

#[test]
fn json_degrades_gracefully_to_a_path_only_file_origin() {
    // serde_json exposes no per-value line, so JSON stamps a path-only file
    // origin (line 0 = "line unknown") — graceful, never a panic, never blank.
    let json = r#"{"server":{"port":8080}}"#;
    let (raw, origin) = value_origin(&JsonLoader, "application.json", json, "server.port");
    assert_eq!(raw, "8080");
    match origin {
        Origin::File { path, line } => {
            assert_eq!(path, "application.json");
            assert_eq!(line, 0, "JSON has no per-value line; 0 means unknown");
        }
        other => panic!("expected Origin::File (path-only), got {other:?}"),
    }
}
