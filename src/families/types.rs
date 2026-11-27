//! Serde types for the `evidence_families` contract (design section 3.1),
//! the capture mode carrier and the injectable clock (section 6.1).
//! Every wire type rejects unknown keys.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FamilyKey {
    #[serde(rename = "s02_indexing")]
    S02,
    #[serde(rename = "s03_integrity")]
    S03,
    #[serde(rename = "s04_structured_data")]
    S04,
    #[serde(rename = "s05_link_graph")]
    S05,
    #[serde(rename = "s06_duplicates")]
    S06,
    #[serde(rename = "s08_resources")]
    S08,
}

impl FamilyKey {
    pub const ALL: [FamilyKey; 6] = [
        FamilyKey::S02,
        FamilyKey::S03,
        FamilyKey::S04,
        FamilyKey::S05,
        FamilyKey::S06,
        FamilyKey::S08,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            FamilyKey::S02 => "s02_indexing",
            FamilyKey::S03 => "s03_integrity",
            FamilyKey::S04 => "s04_structured_data",
            FamilyKey::S05 => "s05_link_graph",
            FamilyKey::S06 => "s06_duplicates",
            FamilyKey::S08 => "s08_resources",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionState {
    Complete,
    Partial,
    Unavailable,
}

/// Declared in sort order: issue < unknown < observed < pass (invariant 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Issue,
    Unknown,
    Observed,
    Pass,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyItem {
    pub entity_id: Option<String>,
    pub locator: Option<String>,
    pub kind: String,
    pub outcome: Outcome,
    pub reason: Option<String>,
    pub facts: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub entities: u64,
    pub eligible: u64,
    pub attempted: u64,
    pub observed: u64,
    pub unknown: u64,
    pub unknown_reasons: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilySection {
    pub section_version: u32,
    pub semantics: String,
    pub state: SectionState,
    pub reason: Option<String>,
    pub coverage: Coverage,
    pub summary: Value,
    pub items: Vec<FamilyItem>,
    pub items_total: u64,
    pub omitted_items: u64,
    pub omitted_reason: Option<String>,
    pub truncated: bool,
    pub max_items: u64,
    pub max_bytes: u64,
}

impl FamilySection {
    /// A section that was not produced: `state: unavailable`, no items, a reason.
    pub fn unavailable(key: FamilyKey, semantics: &str, reason: &str) -> Self {
        let limits = limits_for(key);
        Self {
            section_version: 1,
            semantics: semantics.to_owned(),
            state: SectionState::Unavailable,
            reason: Some(reason.to_owned()),
            coverage: Coverage::default(),
            summary: Value::Object(Map::new()),
            items: Vec::new(),
            items_total: 0,
            omitted_items: 0,
            omitted_reason: None,
            truncated: false,
            max_items: limits.max_items as u64,
            max_bytes: limits.max_bytes as u64,
        }
    }
}

/// Always holds all six keys once built by `families::build_all`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FamilySet(pub BTreeMap<FamilyKey, FamilySection>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FamilyLimits {
    pub max_items: usize,
    pub max_bytes: usize,
}

/// Design section 3.3. The byte caps sum to 786,432.
pub const FAMILY_LIMITS: [(FamilyKey, FamilyLimits); 6] = [
    (
        FamilyKey::S02,
        FamilyLimits {
            max_items: 256,
            max_bytes: 98_304,
        },
    ),
    (
        FamilyKey::S03,
        FamilyLimits {
            max_items: 256,
            max_bytes: 163_840,
        },
    ),
    (
        FamilyKey::S04,
        FamilyLimits {
            max_items: 256,
            max_bytes: 131_072,
        },
    ),
    (
        FamilyKey::S05,
        FamilyLimits {
            max_items: 256,
            max_bytes: 131_072,
        },
    ),
    (
        FamilyKey::S06,
        FamilyLimits {
            max_items: 128,
            max_bytes: 98_304,
        },
    ),
    (
        FamilyKey::S08,
        FamilyLimits {
            max_items: 256,
            max_bytes: 163_840,
        },
    ),
];

pub fn limits_for(key: FamilyKey) -> FamilyLimits {
    FAMILY_LIMITS[match key {
        FamilyKey::S02 => 0,
        FamilyKey::S03 => 1,
        FamilyKey::S04 => 2,
        FamilyKey::S05 => 3,
        FamilyKey::S06 => 4,
        FamilyKey::S08 => 5,
    }]
    .1
}

/// How a standard job captures evidence. Stored in `jobs.capture_mode`
/// as `'none'` or `'wire2'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CaptureMode {
    #[default]
    None,
    Wire2,
}

impl CaptureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureMode::None => "none",
            CaptureMode::Wire2 => "wire2",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "none" => Some(CaptureMode::None),
            "wire2" => Some(CaptureMode::Wire2),
            _ => None,
        }
    }
}

/// Injected so deadline tests need no sleeping.
pub trait Clock: Send + Sync {
    fn now(&self) -> std::time::Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_bytes_sum_to_the_families_cap() {
        assert_eq!(
            FAMILY_LIMITS
                .iter()
                .map(|(_, l)| l.max_bytes)
                .sum::<usize>(),
            786_432
        );
    }

    #[test]
    fn limits_for_matches_the_table() {
        for (key, limits) in FAMILY_LIMITS {
            assert_eq!(limits_for(key), limits);
        }
    }

    #[test]
    fn keys_serialize_to_the_wire_names() {
        for key in FamilyKey::ALL {
            assert_eq!(serde_json::to_value(key).unwrap(), key.as_str());
        }
    }

    #[test]
    fn unavailable_section_round_trips_and_rejects_unknown_keys() {
        let s = FamilySection::unavailable(FamilyKey::S05, "s05-1", "capture_mode_changed");
        let mut v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            serde_json::from_value::<FamilySection>(v.clone()).unwrap(),
            s
        );
        v["extra"] = Value::Null;
        assert!(serde_json::from_value::<FamilySection>(v).is_err());
    }

    #[test]
    fn outcome_order_is_issue_unknown_observed_pass() {
        assert!(Outcome::Issue < Outcome::Unknown);
        assert!(Outcome::Unknown < Outcome::Observed);
        assert!(Outcome::Observed < Outcome::Pass);
    }

    #[test]
    fn capture_mode_text_round_trips() {
        for m in [CaptureMode::None, CaptureMode::Wire2] {
            assert_eq!(CaptureMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(CaptureMode::parse("other"), None);
    }
}
