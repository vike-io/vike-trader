//! **A rank word read off a run's REPORT**: the key each [`RANK_METRICS`] word is under in the report
//! and which way is better ([`RANK_METRIC_REPORTS`]).
//!
//! The readers sit on both sides of layers that cannot name each other: the engine's trial ledger
//! sorts a search's trials by these keys (`--sort`), and the CLI's gate compares two runs' reports by
//! them (`--fail-if`). Each spelled the word-to-key table itself; this is the one table, beside the
//! roster its words come from.

// Named by the doc links and the test only.
#[cfg(any(doc, test))]
use super::flag_vocab::RANK_METRICS;

/// One [`RANK_METRICS`] word read off a run's report: the key it is under and which way is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankMetricReport {
    /// The rank word, as [`RANK_METRICS`] spells it.
    pub metric: &'static str,
    /// The key the report carries it under: `vike_analytics::results::TradingResults`' SERIALIZED
    /// field name, which a trial ledger's `metrics` and a run's `report.json` both hold.
    pub report_key: &'static str,
    /// Whether a bigger value ranks higher (a drawdown is the one that ranks lower).
    pub higher_is_better: bool,
}

/// **Every rank word that names one report key, with its direction**, in [`RANK_METRICS`]' order.
/// `multi` names no key (a blend of several) and has no row.
pub const RANK_METRIC_REPORTS: [RankMetricReport; 4] = [
    RankMetricReport { metric: "sharpe", report_key: "sharpe", higher_is_better: true },
    RankMetricReport { metric: "return", report_key: "total_return", higher_is_better: true },
    RankMetricReport { metric: "max_dd", report_key: "max_drawdown", higher_is_better: false },
    RankMetricReport { metric: "equity", report_key: "final_equity", higher_is_better: true },
];

/// The [`RANK_METRIC_REPORTS`] row of `metric`, an EXACT match (a caller that admits another case
/// folds it first), or `None` for `multi` and for a word that is no rank word.
#[must_use]
pub fn rank_metric_report(metric: &str) -> Option<&'static RankMetricReport> {
    RANK_METRIC_REPORTS.iter().find(|r| r.metric == metric)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every [`RANK_METRICS`] word but `multi` has exactly one row, in the roster's order, and the keys
    /// and directions are the ones the `--sort` table and the `--fail-if` aliases spelled (`sharpe`,
    /// `total_return`, `max_drawdown` lower is better, `final_equity`).
    #[test]
    fn every_rank_word_but_multi_names_one_report_key() {
        let words: Vec<&str> = RANK_METRIC_REPORTS.iter().map(|r| r.metric).collect();
        let roster: Vec<&str> = RANK_METRICS.iter().copied().filter(|m| *m != "multi").collect();
        assert_eq!(words, roster);
        let keys: Vec<(&str, bool)> =
            RANK_METRIC_REPORTS.iter().map(|r| (r.report_key, r.higher_is_better)).collect();
        assert_eq!(
            keys,
            [
                ("sharpe", true),
                ("total_return", true),
                ("max_drawdown", false),
                ("final_equity", true)
            ]
        );
        assert_eq!(rank_metric_report("max_dd").map(|r| r.report_key), Some("max_drawdown"));
        assert_eq!(rank_metric_report("multi"), None, "a blend names no one key");
        assert_eq!(rank_metric_report("SHARPE"), None, "exact: the caller folds the case");
    }
}
