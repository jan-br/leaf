//! Config metadata: the co-emitted [`ConfigGroup`]/[`Property`] rows + the
//! consumption of the `CONFIG_METADATA` discovery slice.
//!
//! Realizes binding-conversion `config-metadata`: the same `#[config_properties]`
//! /`#[derive(BindTarget)]` expansion that emits the bean `Descriptor` ALSO emits
//! a const [`ConfigGroup`] documenting the bound keys (descriptions from `///`,
//! types, defaults, deprecation, hints, declaration origin). A `leaf metadata`
//! rollup walks them at the final binary; a DCE-dropped group degrades only
//! IDE/tooling, never app behavior (the UNIQUE benign-DCE property), so this
//! needs no force-link self-check.
//!
//! The existing discovery [`crate::discovery::ConfigMetadataRow`] (`{contract,
//! prefix}`) is the minimal anti-DCE anchor row already on the
//! [`crate::discovery::CONFIG_METADATA`] linkme slice; this module adds the
//! richer build-time shape and the consumption helpers, and bridges the two via
//! [`group_to_row`].

use crate::discovery::ConfigMetadataRow;
use crate::identity::ContractId;

/// A deprecation note on a config [`Property`].
#[derive(Clone, Copy, Debug)]
pub struct Deprecation {
    /// Why the property is deprecated.
    pub reason: Option<&'static str>,
    /// The replacement key, if any.
    pub replacement: Option<&'static str>,
}

/// A value hint for a config [`Property`] (e.g. an enum variant the IDE offers).
#[derive(Clone, Copy, Debug)]
pub struct Hint {
    /// The suggested value.
    pub value: &'static str,
    /// A short description of the suggestion.
    pub description: Option<&'static str>,
}

/// A compile-time declaration origin (file:line:col) captured by the macro via
/// `proc_macro::Span`. A const placeholder shape until the macro unit lands.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodeSpan {
    /// The source file path.
    pub file: &'static str,
    /// The 1-based line.
    pub line: u32,
    /// The 1-based column.
    pub column: u32,
}

/// One documented config property within a [`ConfigGroup`] (config-metadata).
#[derive(Clone, Copy, Debug)]
pub struct Property {
    /// The canonical kebab name (via relaxed-binding's rule).
    pub name: &'static str,
    /// The Rust type rendered as a string (kept consistent with the converter
    /// grammar).
    pub ty: &'static str,
    /// The `///`-derived description, if any.
    pub description: Option<&'static str>,
    /// The default value rendered as a string, if any.
    pub default: Option<&'static str>,
    /// A deprecation note, if any.
    pub deprecation: Option<Deprecation>,
    /// Value hints (enum variants, suffix hints).
    pub hints: &'static [Hint],
    /// The declaration origin.
    pub origin: CodeSpan,
}

/// One `@ConfigurationProperties` metadata group (config-metadata).
///
/// Emitted as a const into the `CONFIG_METADATA`-adjacent rollup; the binary-crate
/// `leaf metadata` tool aggregates every group from the whole graph.
#[derive(Clone, Copy, Debug)]
pub struct ConfigGroup {
    /// The canonical key prefix this group documents.
    pub prefix: &'static str,
    /// The fully-qualified Rust type name.
    pub type_name: &'static str,
    /// The group `///` description, if any.
    pub description: Option<&'static str>,
    /// The documented properties.
    pub properties: &'static [Property],
    /// The stable cross-build identity of the config-properties bean.
    pub contract: ContractId,
}

impl ConfigGroup {
    /// Find a property by its canonical name.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties.iter().find(|p| p.name == name)
    }
}

/// Bridge a rich [`ConfigGroup`] to the minimal anti-DCE
/// [`ConfigMetadataRow`] the discovery slice carries.
#[must_use]
pub fn group_to_row(group: &ConfigGroup) -> ConfigMetadataRow {
    ConfigMetadataRow {
        contract: group.contract,
        prefix: group.prefix,
    }
}

/// Consume the link-collected `CONFIG_METADATA` slice into owned rows.
///
/// This is the runtime/tooling read idiom (delegates to the one
/// [`crate::discovery::collect_slice`]). The binary-crate `leaf metadata` rollup
/// uses this to aggregate every contributing crate's rows.
#[must_use]
pub fn collect_config_metadata() -> Vec<ConfigMetadataRow> {
    crate::discovery::collect_slice(&crate::discovery::CONFIG_METADATA)
}

/// Look up a config-metadata row by its documented prefix (relaxed-insensitive
/// to exact-match here; the tooling owns relaxed equivalence).
#[must_use]
pub fn find_by_prefix(rows: &[ConfigMetadataRow], prefix: &str) -> Option<ConfigMetadataRow> {
    rows.iter().find(|r| r.prefix == prefix).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    static PORT_PROP: Property = Property {
        name: "port",
        ty: "u16",
        description: Some("The listen port."),
        default: Some("8080"),
        deprecation: None,
        hints: &[],
        origin: CodeSpan {
            file: "src/config.rs",
            line: 10,
            column: 5,
        },
    };

    static SERVER_GROUP: ConfigGroup = ConfigGroup {
        prefix: "server",
        type_name: "my_app::ServerProps",
        description: Some("HTTP server config."),
        properties: &[PORT_PROP],
        contract: ContractId(0x1234),
    };

    #[test]
    fn config_group_is_const_constructible_and_queryable() {
        assert_eq!(SERVER_GROUP.prefix, "server");
        let p = SERVER_GROUP.property("port").expect("has port");
        assert_eq!(p.ty, "u16");
        assert_eq!(p.default, Some("8080"));
        assert_eq!(p.description, Some("The listen port."));
        assert!(SERVER_GROUP.property("missing").is_none());
    }

    #[test]
    fn group_bridges_to_the_discovery_row() {
        let row = group_to_row(&SERVER_GROUP);
        assert_eq!(row.prefix, "server");
        assert_eq!(row.contract, ContractId(0x1234));
    }

    #[test]
    fn find_by_prefix_locates_a_row() {
        let rows = vec![
            ConfigMetadataRow {
                contract: ContractId(0x1),
                prefix: "a",
            },
            ConfigMetadataRow {
                contract: ContractId(0x2),
                prefix: "b",
            },
        ];
        assert_eq!(find_by_prefix(&rows, "b").unwrap().contract, ContractId(0x2));
        assert!(find_by_prefix(&rows, "z").is_none());
    }

    #[test]
    fn collect_config_metadata_reads_the_slice() {
        // The discovery unit's test registers an `app`-prefixed row into the
        // slice; consumption must see at least that one (it is linked in tests).
        let rows = collect_config_metadata();
        assert!(rows.iter().any(|r| r.prefix == "app"), "rows: {rows:?}");
    }
}
