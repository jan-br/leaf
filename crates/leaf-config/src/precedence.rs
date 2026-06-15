//! The declarative externalized-config precedence ladder (environment-config
//! `config-data`, resolved-open-question "ONE precedence representation").
//!
//! [`PrecedenceRung`] is the total-ordered enum CO-OWNED by config-data +
//! profiles + auto-config: each contributes rungs; the planner stable-sorts a
//! flat list of [`Contribution`]s by `(rung, group, profile_specific, external,
//! discovery_index)` so profile-specific-overrides + last-active-profile-wins +
//! `.properties`-beats-`.yaml` are a COMPARATOR, not `add_*` insertion order.
//!
//! Ordering convention: a LOWER [`PrecedenceRung`] is LOWER precedence (loses);
//! `CommandLine` is the highest rung (wins). The planner emits the sealed stack
//! with the HIGHEST-precedence source FIRST (first-source-wins), so it sorts
//! DESCENDING by rung.

/// The full externalized-config precedence ladder as a total-ordered enum.
///
/// Variants are declared LOWEST-precedence first; the derived `Ord` therefore
/// ranks them so a greater value wins. The `ConfigDataFile` rung carries the
/// sub-ordering keys (`group`/`profile_specific`/`external`) Spring uses to rank
/// `application.yaml` vs `application-prod.yaml` vs an imported external file.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum PrecedenceRung {
    /// The lowest rung — `defaultProperties` (auto-config contributes this).
    DefaultProperties,
    /// `@PropertySource`-class contributed sources (bottom-but-one).
    PropertySourceClass,
    /// A config-data file (`application.yaml`, profile-specific, imported).
    ConfigDataFile {
        /// The location-group index (a later group out-ranks an earlier one).
        group: u16,
        /// Whether this is a profile-specific document (out-ranks plain).
        profile_specific: bool,
        /// Whether this is an external (imported) file (out-ranks bundled).
        external: bool,
    },
    /// The `random.*` computing source.
    Random,
    /// The flattened `SPRING_APPLICATION_JSON`-analogue blob.
    SpringApplicationJson,
    /// The OS environment.
    OsEnv,
    /// The command line (the highest rung — always wins).
    CommandLine,
}

/// One precedence-tagged contribution to the env stack (environment-config
/// `config-data`).
///
/// `discovery_index` is the deterministic tie-break (assigned by worklist order,
/// NOT IO-completion order, per the Phase-4 risk note) so two contributions at
/// the same rung keep a stable, reproducible relative order.
#[derive(Clone, PartialEq, Debug)]
pub struct Contribution {
    /// The precedence rung this contribution sits at.
    pub rung: PrecedenceRung,
    /// The deterministic tie-break index (worklist order).
    pub discovery_index: u32,
    /// The source name (the sealed source's stable identity).
    pub source_name: String,
    /// The flattened properties of this contribution.
    pub props: Vec<(String, leaf_core::PropertyValue)>,
}

impl Contribution {
    /// Build a contribution.
    #[must_use]
    pub fn new(
        rung: PrecedenceRung,
        discovery_index: u32,
        source_name: impl Into<String>,
        props: Vec<(String, leaf_core::PropertyValue)>,
    ) -> Self {
        Contribution {
            rung,
            discovery_index,
            source_name: source_name.into(),
            props,
        }
    }

    /// The total-order sort key (descending precedence in the planner's apply
    /// pass: a HIGHER `(rung, discovery_index)` ranks FIRST/highest-precedence).
    #[must_use]
    pub fn sort_key(&self) -> (PrecedenceRung, u32) {
        (self.rung, self.discovery_index)
    }
}

/// The ordered set of contributions the planner emits; the applier folds it onto
/// the [`leaf_core::EnvBuilder`] highest-precedence-first.
#[derive(Default, Clone, PartialEq, Debug)]
pub struct ConfigDataPlan {
    contributions: Vec<Contribution>,
}

impl ConfigDataPlan {
    /// An empty plan.
    #[must_use]
    pub fn new() -> Self {
        ConfigDataPlan::default()
    }

    /// Add a contribution (planner-side; order is fixed at `sorted`).
    pub fn push(&mut self, c: Contribution) {
        self.contributions.push(c);
    }

    /// Number of contributions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.contributions.len()
    }

    /// Whether the plan is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.contributions.is_empty()
    }

    /// The contributions in HIGHEST-precedence-FIRST order (the applier's order).
    ///
    /// Stable-sort by `sort_key` DESCENDING: a higher rung (and, within a rung,
    /// a higher `discovery_index`) comes first so it wins first-source-wins. The
    /// sort is STABLE so equal keys keep insertion order (determinism).
    #[must_use]
    pub fn sorted(mut self) -> Vec<Contribution> {
        self.contributions
            .sort_by_key(|c| std::cmp::Reverse(c.sort_key()));
        self.contributions
    }

    /// Borrow the raw (unsorted) contributions.
    #[must_use]
    pub fn contributions(&self) -> &[Contribution] {
        &self.contributions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rung_total_order_ranks_command_line_highest() {
        assert!(PrecedenceRung::CommandLine > PrecedenceRung::OsEnv);
        assert!(PrecedenceRung::OsEnv > PrecedenceRung::SpringApplicationJson);
        assert!(PrecedenceRung::SpringApplicationJson > PrecedenceRung::Random);
        assert!(
            PrecedenceRung::Random
                > PrecedenceRung::ConfigDataFile {
                    group: 0,
                    profile_specific: false,
                    external: false
                }
        );
        assert!(
            PrecedenceRung::ConfigDataFile {
                group: 0,
                profile_specific: false,
                external: false
            } > PrecedenceRung::PropertySourceClass
        );
        assert!(PrecedenceRung::PropertySourceClass > PrecedenceRung::DefaultProperties);
    }

    #[test]
    fn config_data_file_profile_specific_outranks_plain() {
        let plain = PrecedenceRung::ConfigDataFile {
            group: 0,
            profile_specific: false,
            external: false,
        };
        let profile = PrecedenceRung::ConfigDataFile {
            group: 0,
            profile_specific: true,
            external: false,
        };
        assert!(profile > plain);
    }

    #[test]
    fn config_data_file_later_group_outranks_earlier() {
        let g0 = PrecedenceRung::ConfigDataFile {
            group: 0,
            profile_specific: true,
            external: true,
        };
        let g1 = PrecedenceRung::ConfigDataFile {
            group: 1,
            profile_specific: false,
            external: false,
        };
        assert!(g1 > g0);
    }

    #[test]
    fn sorted_emits_highest_precedence_first_stable() {
        let mut plan = ConfigDataPlan::new();
        plan.push(Contribution::new(
            PrecedenceRung::DefaultProperties,
            0,
            "defaults",
            vec![],
        ));
        plan.push(Contribution::new(PrecedenceRung::CommandLine, 1, "cli", vec![]));
        plan.push(Contribution::new(PrecedenceRung::OsEnv, 2, "env", vec![]));
        let order: Vec<_> = plan
            .sorted()
            .into_iter()
            .map(|c| c.source_name)
            .collect();
        assert_eq!(order, vec!["cli", "env", "defaults"]);
    }

    #[test]
    fn sorted_is_stable_within_a_rung_by_discovery_index() {
        let rung = PrecedenceRung::ConfigDataFile {
            group: 0,
            profile_specific: false,
            external: false,
        };
        let mut plan = ConfigDataPlan::new();
        plan.push(Contribution::new(rung, 0, "first", vec![]));
        plan.push(Contribution::new(rung, 1, "second", vec![]));
        // Higher discovery_index wins (later worklist item is higher precedence).
        let order: Vec<_> = plan.sorted().into_iter().map(|c| c.source_name).collect();
        assert_eq!(order, vec!["second", "first"]);
    }
}
