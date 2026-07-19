//! The committed manifest: the pinned generator inputs plus the pinned output
//! hashes
//!
//! The manifest file is the single source of truth for how the fixtures are
//! generated

use std::collections::BTreeMap;
use std::error::Error;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

/// Bumped when the manifest shape or the generated layout changes
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Fixed seed so every run reproduces the same fixtures. Changing
/// it re-rolls every generated value
pub(crate) const DEFAULT_SEED: i64 = 42;

/// Window bounds pinned in code
pub(crate) const DEFAULT_WINDOW_START: &str = "2015-01-01";
pub(crate) const DEFAULT_WINDOW_END: &str = "2024-12-31";

/// Everything needed to regenerate the fixtures byte-for-byte and prove they
/// match
///
/// `hashes` are written only by `--pin`
/// It is a `BTreeMap` so keys serialize in sorted order
///
/// Public so callers can build a fixture in memory, but its fields stay
/// crate-visible: outside code hands [`Manifest::default`] to
/// [`generate`](crate::generate), it does not assemble one field by field
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version of this manifest and the layout it produces
    pub(crate) schema_version: u32,
    /// Seed for the deterministic generator
    pub(crate) seed: i64,
    /// Inclusive ISO-8601 window start
    pub(crate) window_start: String,
    /// Inclusive ISO-8601 window end
    pub(crate) window_end: String,
    /// sha256 of each generated file, keyed by name relative to the fixtures dir
    /// Kept last so it serializes as a trailing TOML table
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hashes: Option<BTreeMap<String, String>>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            seed: DEFAULT_SEED,
            window_start: DEFAULT_WINDOW_START.to_owned(),
            window_end: DEFAULT_WINDOW_END.to_owned(),
            hashes: None,
        }
    }
}

impl Manifest {
    /// Inclusive range of calendar years the window covers, used to lay out the
    /// per-year include skeleton
    pub(crate) fn years(&self) -> Result<RangeInclusive<i32>, Box<dyn Error>> {
        let start = year_of(&self.window_start)?;
        let end = year_of(&self.window_end)?;
        if end < start {
            return Err(format!(
                "window end {} precedes start {}",
                self.window_end, self.window_start
            )
            .into());
        }
        Ok(start..=end)
    }
}

/// Read the leading year of an ISO-8601 date
fn year_of(date: &str) -> Result<i32, Box<dyn Error>> {
    // split always yields at least one element, so a failure here is a bad year
    date.split('-')
        .next()
        .and_then(|year| year.parse::<i32>().ok())
        .ok_or_else(|| Box::<dyn Error>::from(format!("cannot read a year from date {date}")))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{DEFAULT_WINDOW_END, DEFAULT_WINDOW_START, Manifest};

    #[test]
    fn default_window_is_pinned_not_today() {
        let manifest = Manifest::default();
        assert_eq!(manifest.window_start, DEFAULT_WINDOW_START);
        assert_eq!(manifest.window_end, DEFAULT_WINDOW_END);
        assert!(manifest.hashes.is_none());
    }

    #[test]
    fn years_span_the_inclusive_window() {
        let manifest = Manifest::default();
        let years: Vec<i32> = manifest.years().unwrap().collect();
        assert_eq!(years.first(), Some(&2015));
        assert_eq!(years.last(), Some(&2024));
    }

    #[test]
    fn years_reject_a_reversed_window() {
        let manifest = Manifest {
            window_start: "2024-01-01".to_owned(),
            window_end: "2015-12-31".to_owned(),
            ..Manifest::default()
        };
        assert!(manifest.years().is_err());
    }

    #[test]
    fn years_reject_a_nonnumeric_year_at_either_end() {
        let start = Manifest {
            window_start: "not-a-date".to_owned(),
            ..Manifest::default()
        };
        let err = start.years().unwrap_err().to_string();
        assert!(err.contains("not-a-date"), "{err}");

        let end = Manifest {
            window_end: "also-not-a-date".to_owned(),
            ..Manifest::default()
        };
        let err = end.years().unwrap_err().to_string();
        assert!(err.contains("also-not-a-date"), "{err}");
    }
}
