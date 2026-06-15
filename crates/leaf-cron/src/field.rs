//! The allowed-value set for one cron field.
//!
//! Cron parsing is a cold path (the Tier-2 startup pass), so this favors
//! always-correct simplicity over micro-optimization: a sorted, de-duplicated
//! `Vec<u32>` of allowed values. It handles every field uniformly, including the
//! wide year field (`1970..=2099`, 130 values) that does not fit a single
//! machine word.

/// A set of allowed values for a single cron field.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct Field {
    /// Sorted, de-duplicated allowed values.
    values: Vec<u32>,
}

impl Field {
    /// An empty field (no value allowed).
    #[must_use]
    pub fn empty() -> Self {
        Field { values: Vec::new() }
    }

    /// A field with every value in the inclusive range `[lo, hi]` set.
    #[must_use]
    pub fn full(lo: u32, hi: u32) -> Self {
        Field {
            values: (lo..=hi).collect(),
        }
    }

    /// Mark `value` as allowed (keeps the set sorted + de-duplicated).
    pub fn set(&mut self, value: u32) {
        match self.values.binary_search(&value) {
            Ok(_) => {}
            Err(pos) => self.values.insert(pos, value),
        }
    }

    /// Whether `value` is in the set.
    #[must_use]
    pub fn contains(&self, value: u32) -> bool {
        self.values.binary_search(&value).is_ok()
    }

    /// The smallest allowed value `>= from`, if any.
    #[must_use]
    pub fn next_from(&self, from: u32) -> Option<u32> {
        match self.values.binary_search(&from) {
            Ok(pos) => Some(self.values[pos]),
            Err(pos) => self.values.get(pos).copied(),
        }
    }
}
