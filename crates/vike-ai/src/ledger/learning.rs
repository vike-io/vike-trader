//! The model's durable notes: [`Learning`], its [`LearningScope`], the persisted [`LearningStore`].

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use super::trial::TrialLedger;

/// A durable note the model chose to record via the `record_learning` tool.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Learning {
    #[serde(default)]
    pub ts_ms: i64,
    #[serde(default)]
    pub scope: LearningScope,
    #[serde(default)]
    pub text: String,
}

/// What a [`Learning`] applies to. `Slice` is the DEFAULT for an unrecognized/absent tool
/// argument — the narrower blast radius: a slice-scoped note only ever re-surfaces on that slice,
/// whereas a mis-scoped `Global` note pollutes every future prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum LearningScope {
    #[default]
    Global,
    Slice {
        venue: String,
        symbol: String,
        interval: String,
    },
}

impl LearningScope {
    /// This scope surfaces on the given slice: `Global` always, `Slice` only on its own triple.
    pub fn matches(&self, venue: &str, symbol: &str, interval: &str) -> bool {
        match self {
            LearningScope::Global => true,
            LearningScope::Slice { venue: v, symbol: s, interval: i } => {
                v == venue && s == symbol && i == interval
            }
        }
    }
}

/// The persisted learnings store — the [`TrialLedger`] twin.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LearningStore {
    /// Learnings in APPEND order — oldest first, newest last.
    #[serde(default)]
    pub learnings: Vec<Learning>,
}

impl LearningStore {
    /// Every learning that surfaces on this slice (every `Global` plus the matching `Slice` ones),
    /// oldest first.
    pub fn matching(&self, venue: &str, symbol: &str, interval: &str) -> Vec<&Learning> {
        self.learnings.iter().filter(|l| l.scope.matches(venue, symbol, interval)).collect()
    }
}
