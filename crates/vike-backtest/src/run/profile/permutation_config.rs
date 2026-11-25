//! The `[permutation]` section: `ProfilePermutation`, a Monte Carlo permutation test of the profile.

use serde::Deserialize;

use crate::HarnessError;
use crate::run::refusals::ProfileRefusal;
use crate::search::paramscan::RankMetric;

/// The `[permutation]` section: run this profile as a Monte Carlo PERMUTATION TEST
/// (`crate::permutation`, Masters) instead of one backtest: the real history against
/// `permutations` permuted ones, each scored by `rank_by`, answered with a p-value.
///
/// ```toml
/// [permutation]
/// permutations = 1000  # how many permuted histories, >= 1
/// seed         = 7     # absent ⇒ 0; one seed is one set of histories
/// train        = true  # absent ⇒ false; true re-runs the [paramscan] grid on every history
/// rank_by      = "return" # absent ⇒ "sharpe"; also "return" | "max_dd" | "equity"
/// ```
///
/// Absent (the default) is byte-identical to every profile written before it: the profile runs
/// as it always did. Present, it is the run's program, so it is refused beside `[walkforward]` (a
/// second program) at load, and a `[paramscan]` grid beside it must be searched (`train = true`)
/// or it would configure nothing (`ProfileBacktest::validate`).
#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfilePermutation {
    /// How many permuted histories the real one is compared with (`>= 1`). The smallest p-value
    /// `N` of them can give is `1 / (N + 1)`.
    pub permutations: usize,
    /// The seed every permuted history is drawn from; one seed is one set of histories.
    #[serde(default)]
    pub seed: usize,
    /// `true` is PERMUTATION TRAINING: the profile's `[paramscan]` grid is searched on the real
    /// history and on every permuted one, and their BEST scores are compared. Absent or `false`
    /// runs the profile's own `[strategy.params]` on each.
    #[serde(default)]
    pub train: bool,
    /// How a history is SCORED: any of the four [`RankMetric`] names (`"sharpe"` | `"return"` |
    /// `"max_dd"` | `"equity"`, case-insensitive), defaulting to [`RankMetric::default`] — the
    /// `[walkforward].rank_by` vocabulary and parser.
    #[serde(default)]
    pub rank_by: Option<String>,
}

impl ProfilePermutation {
    /// Resolve [`Self::rank_by`] to a [`RankMetric`]. Absent ⇒ [`RankMetric::default`].
    pub fn rank_metric(&self) -> Result<RankMetric, HarnessError> {
        let Some(raw) = self.rank_by.as_deref() else {
            return Ok(RankMetric::default());
        };
        RankMetric::from_str_ci(raw.trim()).ok_or_else(|| {
            HarnessError::from(ProfileRefusal::PermutationRankByUnknown { raw: raw.to_string() })
        })
    }
}
