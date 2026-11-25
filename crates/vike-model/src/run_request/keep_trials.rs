//! **What `--keep-trials` selected** — [`KeepTrials`] and its ONE roster ([`KeepTrials::WORDS`]),
//! the words the flag vocabulary's `--keep-trials` row admits and the standalone engine records in a
//! search's `search.json` and `report.json`.
//!
//! Here, below the engine that parses the flag (`crates/vike-backtesthub/src/backtest_cli/search_flags.rs`'s
//! `parse_keep_trials`, which owns the default, `scalars`, and the named refusal of `series`) and the
//! vocabulary row that renders it ([`super::flag_vocab::BACKTEST_FLAGS`]), so the roster is a read and
//! not a second list. What a mode RETAINS (the bucket capture `returns` arms) stays the engine's.

vike_utility::word_enum! {
    /// What `--keep-trials` selected.
    ///
    /// ⚠ **`returns` is the mode that unblocked the anti-overfitting statistics, and it is NOT
    /// `series` under another name.** `vike_analytics::overfit::pbo_cscv` and
    /// `deflated_sharpe_with_effective_n` need an N-trial matrix of per-observation performance,
    /// and a trial's equity curve is destroyed before a sweep row exists:
    /// `crates/vike-backtest/src/search/paramscan/grid.rs`'s `row_from_outcome` consumes the
    /// `TradingRecord` to derive `TradingResults`'s scalars and drops `equity_curve`,
    /// `equity_timestamp`, `trades` and `per_symbol_curves`. The measurement that dissolved the
    /// deadlock is that the STATISTIC does not want the curve: `pbo_cscv` splits `T` observations
    /// into `n_splits` contiguous blocks, so it needs `T >= n_splits` and nothing more. `returns`
    /// therefore retains a FIXED-SIZE bucketed return vector per trial
    /// (`vike_backtest::search::paramscan::OverfitReturnBuckets`, 512 buckets = 4 KB a trial, ~2 MB
    /// over a 500-point grid against ~80 MB of decimated curves) and writes the resulting
    /// `vike_backtest::search::trial_ledger::OverfitStats` into the search's `report.json`, where
    /// `vike-cli backtest gate --fail-if` can name `overfit.pbo` and its siblings.
    ///
    /// ⚠ **`series` IS STILL REFUSED BY NAME, and the refusal is narrower than it was.** What it
    /// asks for is the whole CURVE per trial, which is a retention question rather than a
    /// statistical one: at `vike_runs::MAX_EQUITY_SAMPLES` it is forty times `returns`' cost on a
    /// box whose sweep worker cap is already
    /// `vike_backtest::search::paramscan::threads::DEFAULT_PARAMSCAN_THREADS` because each
    /// concurrent point materialises its own data slice — and no consumer in this tree reads a
    /// per-trial curve. Re-running the winner on its own is how an operator gets one.
    ///
    /// ⚠ **`returns` writes the same LEDGER as `scalars`**, which is what makes it resumable: a
    /// resumed search keys its refusal on whether a ledger was kept ([`KeepTrials::keeps_ledger`]),
    /// never on the exact spelling. A resumed search's REUSED trials contribute no column (a
    /// warm-cache answer is a score, not a report), so the matrix covers the freshly-evaluated
    /// trials and `OverfitStats::excluded` reports the rest.
    ///
    /// The word is what `search.json` and `report.json` record, read back EXACTLY
    /// ([`KeepTrials::from_word`]): a word this build does not know is a document a NEWER build
    /// wrote, refused rather than guessed. The flag itself is read case-insensitively
    /// ([`KeepTrials::parse`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum KeepTrials {
        /// Keep nothing: the search writes no trial ledger.
        None => "none",
        /// The trial ledger of scalars, the engine's default when the flag is absent.
        Scalars => "scalars",
        /// `scalars`, PLUS the in-memory bucketed return matrix the overfit statistics are computed
        /// from. See this type's doc.
        Returns => "returns",
    }
}

impl KeepTrials {
    /// Whether this mode writes a trial ledger (`vike_backtest::search::trial_ledger::TRIALS_FILE`)
    /// at all — the ONE question `--resume` actually asks, and the reason that check is not
    /// `== Scalars`: a `returns` search keeps a byte-identical ledger, so refusing to resume it
    /// would send an operator to "run the whole search again" for a reason that is not true.
    #[must_use]
    pub fn keeps_ledger(self) -> bool {
        match self {
            KeepTrials::None => false,
            KeepTrials::Scalars | KeepTrials::Returns => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::KeepTrials;

    /// A `returns` search is resumable, a `none` search is not.
    #[test]
    fn every_mode_but_none_keeps_a_ledger() {
        assert!(!KeepTrials::None.keeps_ledger());
        assert!(KeepTrials::Scalars.keeps_ledger());
        assert!(KeepTrials::Returns.keeps_ledger(), "a returns search IS resumable");
    }
}
