//! The config-metadata rollup over `CONFIG_METADATA` + the duplicate-prefix
//! check (binding-conversion `config-metadata`, phase3/07).
//!
//! The `leaf metadata` tooling step aggregates every contributing crate's
//! [`leaf_core::ConfigMetadataRow`] (the anti-DCE anchor) / richer
//! [`leaf_core::ConfigGroup`] into one deterministically-ordered rollup that
//! documents every bound config key prefix in the whole binary, and FLAGS two
//! groups that document the SAME prefix (a duplicate-prefix collision — the
//! config-metadata analogue of the registry's name-collision guard).
//!
//! This is a pure, unit-testable fold over OWNED rows (the codegen side feeds it
//! the link-collected slice); the runtime/tooling read idiom
//! ([`leaf_core::collect_config_metadata`]) supplies the input at the final
//! binary. Unlike the bean self-check, a DCE-dropped config group degrades only
//! IDE/tooling — never app behavior — so this rollup is advisory, not a hard
//! anti-DCE gate (the UNIQUE benign-DCE property, per `leaf-core::metadata`).

use std::collections::BTreeMap;

use leaf_core::{ConfigGroup, ConfigMetadataRow, ContractId};

/// One duplicate-prefix collision: two (or more) config groups documenting the
/// SAME canonical key prefix — an ambiguity the rollup surfaces loudly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicatePrefix {
    /// The colliding canonical prefix.
    pub prefix: String,
    /// The contracts of every group claiming it (deterministically sorted).
    pub contracts: Vec<ContractId>,
}

/// The aggregated config-metadata rollup: every documented prefix once, in
/// deterministic prefix order, plus any duplicate-prefix collisions.
///
/// `ConfigMetadataRow` is a frozen leaf-core `Copy`-only row (no `PartialEq`), so
/// this struct is not `PartialEq` itself; compare its `rows`/`duplicates` fields.
#[derive(Clone, Debug, Default)]
pub struct MetadataRollup {
    /// One representative row per distinct prefix, sorted by prefix (NEVER link
    /// order). On a duplicate prefix the lowest-`ContractId` row represents it
    /// (deterministic), and the collision is also recorded in [`duplicates`].
    ///
    /// [`duplicates`]: MetadataRollup::duplicates
    pub rows: Vec<ConfigMetadataRow>,
    /// Every duplicate-prefix collision found, sorted by prefix.
    pub duplicates: Vec<DuplicatePrefix>,
}

impl MetadataRollup {
    /// `true` iff no duplicate-prefix collision was found.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.duplicates.is_empty()
    }

    /// Look up the representative row documenting `prefix`.
    #[must_use]
    pub fn row(&self, prefix: &str) -> Option<&ConfigMetadataRow> {
        self.rows.iter().find(|r| r.prefix == prefix)
    }
}

/// Roll up the link-collected [`ConfigMetadataRow`]s: dedup by prefix into a
/// deterministic, prefix-sorted table and flag every duplicate-prefix collision.
///
/// Determinism is load-bearing (a `leaf metadata --check` must be stable):
/// grouping is by a `BTreeMap` keyed on the prefix, the representative per prefix
/// is the LOWEST `ContractId`, and the collision contracts are sorted. A single
/// row per prefix is NOT a duplicate; two DISTINCT contracts on one prefix is.
#[must_use]
pub fn roll_up(rows: &[ConfigMetadataRow]) -> MetadataRollup {
    // prefix -> sorted-unique contracts claiming it.
    let mut by_prefix: BTreeMap<&str, Vec<ContractId>> = BTreeMap::new();
    for row in rows {
        let entry = by_prefix.entry(row.prefix).or_default();
        if !entry.contains(&row.contract) {
            entry.push(row.contract);
        }
    }

    let mut out_rows = Vec::new();
    let mut duplicates = Vec::new();
    for (prefix, mut contracts) in by_prefix {
        contracts.sort_by_key(|c| c.0);
        // The representative is the lowest-contract row (deterministic).
        out_rows.push(ConfigMetadataRow {
            contract: contracts[0],
            prefix,
        });
        if contracts.len() > 1 {
            duplicates.push(DuplicatePrefix {
                prefix: prefix.to_string(),
                contracts,
            });
        }
    }
    MetadataRollup {
        rows: out_rows,
        duplicates,
    }
}

/// Roll up the richer [`ConfigGroup`]s (the build-time shape) by bridging each to
/// its minimal [`ConfigMetadataRow`] first, then folding — so the group and the
/// anchor-row paths share ONE duplicate-prefix definition.
#[must_use]
pub fn roll_up_groups(groups: &[ConfigGroup]) -> MetadataRollup {
    let rows: Vec<ConfigMetadataRow> = groups.iter().map(leaf_core::group_to_row).collect();
    roll_up(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(prefix: &'static str, contract: u64) -> ConfigMetadataRow {
        ConfigMetadataRow {
            contract: ContractId(contract),
            prefix,
        }
    }

    #[test]
    fn rolls_up_distinct_prefixes_sorted() {
        // Three distinct prefixes roll up into a prefix-sorted, collision-free
        // table (NEVER link/insertion order).
        let rows = vec![row("server", 3), row("app", 1), row("redis", 2)];
        let rollup = roll_up(&rows);
        assert!(rollup.is_clean());
        let prefixes: Vec<&str> = rollup.rows.iter().map(|r| r.prefix).collect();
        assert_eq!(prefixes, vec!["app", "redis", "server"]);
    }

    #[test]
    fn the_same_prefix_from_one_contract_is_not_a_duplicate() {
        // The SAME (prefix, contract) appearing twice (e.g. two link copies) is
        // deduplicated, not flagged.
        let rows = vec![row("app", 1), row("app", 1)];
        let rollup = roll_up(&rows);
        assert!(rollup.is_clean(), "duplicates: {:?}", rollup.duplicates);
        assert_eq!(rollup.rows.len(), 1);
    }

    #[test]
    fn two_contracts_on_one_prefix_is_a_loud_duplicate() {
        // Two DISTINCT config-properties beans documenting the same `app` prefix:
        // a duplicate-prefix collision the rollup surfaces.
        let rows = vec![row("app", 7), row("app", 2)];
        let rollup = roll_up(&rows);
        assert!(!rollup.is_clean());
        assert_eq!(rollup.duplicates.len(), 1);
        let dup = &rollup.duplicates[0];
        assert_eq!(dup.prefix, "app");
        // Contracts sorted ascending (deterministic).
        assert_eq!(dup.contracts, vec![ContractId(2), ContractId(7)]);
    }

    #[test]
    fn the_representative_row_is_the_lowest_contract() {
        // On a collision the prefix is still represented exactly once, by the
        // lowest-ContractId row (deterministic).
        let rows = vec![row("app", 9), row("app", 4)];
        let rollup = roll_up(&rows);
        assert_eq!(rollup.row("app").unwrap().contract, ContractId(4));
    }

    #[test]
    fn duplicates_are_sorted_by_prefix() {
        let rows = vec![
            row("zeta", 1),
            row("zeta", 2),
            row("alpha", 3),
            row("alpha", 4),
        ];
        let rollup = roll_up(&rows);
        let dup_prefixes: Vec<&str> = rollup.duplicates.iter().map(|d| d.prefix.as_str()).collect();
        assert_eq!(dup_prefixes, vec!["alpha", "zeta"]);
    }

    #[test]
    fn an_empty_input_rolls_up_to_an_empty_clean_rollup() {
        let rollup = roll_up(&[]);
        assert!(rollup.is_clean());
        assert!(rollup.rows.is_empty());
    }

    #[test]
    fn roll_up_groups_bridges_through_the_minimal_row() {
        // The richer ConfigGroup path shares the SAME duplicate definition as the
        // anchor-row path (bridged via leaf_core::group_to_row).
        static GROUP_A: ConfigGroup = ConfigGroup {
            prefix: "app",
            type_name: "a::AppProps",
            description: None,
            properties: &[],
            contract: ContractId(5),
        };
        static GROUP_B: ConfigGroup = ConfigGroup {
            prefix: "app",
            type_name: "b::OtherProps",
            description: None,
            properties: &[],
            contract: ContractId(8),
        };
        let rollup = roll_up_groups(&[GROUP_A, GROUP_B]);
        assert!(!rollup.is_clean());
        assert_eq!(rollup.duplicates[0].prefix, "app");
        assert_eq!(rollup.duplicates[0].contracts, vec![ContractId(5), ContractId(8)]);
    }
}
