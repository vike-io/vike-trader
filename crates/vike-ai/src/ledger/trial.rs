//! One authoring attempt ([`Trial`]) and the persisted, append-ordered [`TrialLedger`].

use serde::{Deserialize, Serialize};
use vike_analytics::overfit;

use crate::AgentResult;

#[cfg(doc)]
use super::MAX_CURVE_ROWS;
#[cfg(doc)]
use super::store::prune;

/// One completed authoring attempt: an [`AgentResult`] plus the slice it was run against and the
/// derived statistic deflation needs.
///
/// Every field is `#[serde(default)]` — see the module doc's forward-compat note.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Trial {
    #[serde(default)]
    pub ts_ms: i64,
    #[serde(default)]
    pub venue: String,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub interval: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub explanation: String,
    #[serde(default)]
    pub accepted: bool,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub problems: Vec<String>,
    /// The ANNUALIZED display Sharpe (`metrics::sharpe`, PPY=252) — the same quantity
    /// `AgentResult::oos_sharpe` carries, for display only. It must never reach `overfit::`; that
    /// is what `sr_per_obs` below is for.
    #[serde(default)]
    pub oos_sharpe: f64,
    #[serde(default)]
    pub n_trades: usize,
    /// The OOS equity curve — retained only on the newest [`MAX_CURVE_ROWS`] accepted rows (see
    /// [`prune`]); empty on every older row and on every rejected one.
    #[serde(default)]
    pub oos_equity_curve: Vec<f64>,
    #[serde(default)]
    pub deflated_sharpe: f64,
    /// The **PER-PERIOD** Sharpe (`overfit::sharpe_moments(..).sr_per_obs`) derived from
    /// `oos_equity_curve` at RECORD time and persisted alongside it. This is the load-bearing
    /// field of the whole bounding scheme: it is the only thing a trial contributes to a later
    /// call's deflation trial set, so a curve-pruned row still counts as a trial. Without it,
    /// pruning a curve would silently shrink the multiple-testing correction.
    #[serde(default)]
    pub sr_per_obs: f64,
}

/// `x` if it is finite, else `0.0`.
///
/// **This is a never-brick guard, not cosmetics.** `serde_json` writes a non-finite f64 as JSON
/// `null` (it does not error), and `null` then FAILS to deserialize back into `f64` — so one NaN
/// (`metrics::sharpe` over a perfectly flat equity curve is `0/0`) would make the WHOLE ledger
/// unparseable on the next load, silently discarding every trial ever recorded. Non-finite
/// statistics carry no information the trial set can use anyway: `prior_sharpes_for` filters them
/// out, and `sample_variance` would be poisoned by one.
///
/// **The substitution is LOUD, because it is not neutral.** A stored `0.0` is indistinguishable on
/// the next load from a trial that genuinely scored `0.0`, and the two mean different things to
/// `prior_sharpes_for`: a substituted value is a REAL observation in the cross-session deflation
/// trial set, pulling `sample_variance` around, where "this trial had no valid Sharpe" would have
/// contributed nothing. Silently recording it is how that difference goes unnoticed for sessions.
/// So a replacement emits one `warn` — only on the replacing branch: the finite branch runs for
/// every f64 of every trial, and an event there would be noise, not signal. `#[track_caller]`
/// (which leaves the signature a drop-in at all three call sites) names WHICH statistic was
/// replaced, by its call site; WHICH trial comes from [`Trial::from_result`]'s span.
#[track_caller]
pub(super) fn finite_or_zero(x: f64) -> f64 {
    if x.is_finite() {
        return x;
    }
    // `%x` (Display), never the f64 Value: the JSON file layer cannot serialize a non-finite f64
    // — the very hazard this guard exists for would then bite the log line reporting it.
    tracing::warn!(
        value = %x,
        replaced_at = %std::panic::Location::caller(),
        "ai trial ledger: non-finite statistic recorded as 0.0 — it counts as a real 0.0 \
         observation in cross-session deflation, not as a missing one"
    );
    0.0
}

impl Trial {
    /// Record one finished [`AgentResult`] against the slice it was run on. `sr_per_obs` is
    /// derived here, once, from the curve — so it survives the curve being pruned later.
    ///
    /// Every f64 goes through [`finite_or_zero`], and a curve containing any non-finite value is
    /// dropped outright (it can produce no usable moments, and would brick the file — see that
    /// helper's doc).
    pub fn from_result(
        ts_ms: i64,
        venue: &str,
        symbol: &str,
        interval: &str,
        r: &AgentResult,
    ) -> Self {
        // Names the trial for any `finite_or_zero` warning fired below: that helper sees one f64
        // and nothing else, and "a non-finite statistic was zeroed" is unactionable without
        // knowing whose. Every call site of it in this module is inside this span.
        //
        // `warn_span!`, not the `info_span!` the venue pumps use, and for a reason: a span carries
        // its OWN level, so an `info` one is DISABLED under a `RUST_LOG=warn` console — exactly
        // the setting a long headless run is told to use — and the warning below would then land
        // with no trial attached. The span exists only to caption that warning; it matches level.
        let _span = tracing::warn_span!(
            "ai_trial",
            ts_ms,
            venue = %venue,
            symbol = %symbol,
            interval = %interval
        )
        .entered();
        let curve: Vec<f64> = if r.oos_equity_curve.iter().all(|v| v.is_finite()) {
            r.oos_equity_curve.clone()
        } else {
            Vec::new()
        };
        let sr_per_obs = if r.accepted && !curve.is_empty() {
            finite_or_zero(overfit::sharpe_moments(&curve).sr_per_obs)
        } else {
            0.0
        };
        Self {
            ts_ms,
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            code: r.code.clone(),
            explanation: r.explanation.clone(),
            accepted: r.accepted,
            attempts: r.attempts,
            problems: r.problems.clone(),
            oos_sharpe: finite_or_zero(r.oos_sharpe),
            n_trades: r.n_trades,
            oos_equity_curve: curve,
            deflated_sharpe: finite_or_zero(r.deflated_sharpe),
            sr_per_obs,
        }
    }

    /// This trial was run against the given `(venue, symbol, interval)` slice.
    pub fn is_slice(&self, venue: &str, symbol: &str, interval: &str) -> bool {
        self.venue == venue && self.symbol == symbol && self.interval == interval
    }
}

/// The persisted trial ledger. A plain struct rather than a bare `Vec` so future top-level fields
/// (a schema tag, counters) stay additive.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrialLedger {
    /// Trials in APPEND order — oldest first, newest last. "Newest" everywhere in this module
    /// means the TAIL of this vec, not a `ts_ms` sort: append order is what the writer controls
    /// and what pruning must be stable under.
    #[serde(default)]
    pub trials: Vec<Trial>,
}

impl TrialLedger {
    /// Every trial run against this slice, oldest first.
    pub fn for_slice(&self, venue: &str, symbol: &str, interval: &str) -> Vec<&Trial> {
        self.trials.iter().filter(|t| t.is_slice(venue, symbol, interval)).collect()
    }

    /// The PER-PERIOD Sharpes of prior ACCEPTED trials on this slice — the cross-session half of
    /// the deflation trial set (see `agent::deflate::deflate_with_prior`). Non-finite values are dropped:
    /// one NaN would poison `sample_variance` and hence every candidate's deflated Sharpe.
    pub fn prior_sharpes_for(&self, venue: &str, symbol: &str, interval: &str) -> Vec<f64> {
        self.trials
            .iter()
            .filter(|t| t.accepted && t.is_slice(venue, symbol, interval))
            .map(|t| t.sr_per_obs)
            .filter(|s| s.is_finite())
            .collect()
    }
}
