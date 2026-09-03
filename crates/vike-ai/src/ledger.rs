//! The persistent trial ledger + learnings store behind the `develop_strategy` authoring loop —
//! the memory that loop never had. Every trial it runs (its code, its OOS Sharpe, its equity
//! curve, why it was rejected) used to be discarded on return, so the loop restarted cold every
//! session and re-derived the same duds, and — the statistically real cost — deflation, the single
//! most important overfitting control, could only see the candidates of ONE call and therefore
//! reported `0.0` on the path production actually uses (a bare `develop_strategy`).
//!
//! Two best-effort JSON files, conventionally colocated with the store root (see [`LedgerPaths`]):
//!
//! ```text
//! <store root>/ai_trials.json      // the trial ledger (bounded, append-ordered)
//! <store root>/ai_learnings.json   // curated durable notes the model chose to record
//! ```
//!
//! **Env boundary (settings STEP 2).** This module reads NO environment variable — every entry
//! point takes a `&Path`, exactly the `vike_alerting::persist` idiom: the BINARY (or, here, the
//! Studio shell that already owns a `DataFusionHist`) resolves the location and passes it in. That
//! keeps the settings-registry `Library` count from growing and keeps the ledger testable against
//! a temp dir with no ambient state.
//!
//! **Best-effort throughout, the studio idiom.** A missing file is `Default`, a corrupt file is
//! `Default` plus one `tracing::warn!`, a failed write is one `tracing::warn!` and nothing else.
//! The authoring loop must never fail because the ledger is unavailable — a lost note is a lost
//! note; a bricked loop is a bricked feature.
//!
//! **Forward-compat** lives in the serde model: every field on every struct here (and on
//! [`crate::AgentResult`], which gained `Serialize`/`Deserialize` for this module) is
//! `#[serde(default)]`, so a file written by an older build still loads — the
//! `studio_strategies.json` precedent.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use vike_backtest::overfit;

use crate::AgentResult;

/// The trial ledger's filename under a store root.
pub const TRIALS_FILE: &str = "ai_trials.json";

/// The learnings store's filename under a store root.
pub const LEARNINGS_FILE: &str = "ai_learnings.json";

/// How many trials the ledger retains. Unbounded growth is a real failure mode here, not a
/// theoretical one: every accepted trial carries an `oos_equity_curve` `Vec<f64>` as long as the
/// held-out window. Oldest rows are dropped first (the ledger is append-ordered).
pub const MAX_TRIALS: usize = 500;

/// How many of the newest ACCEPTED trials keep their `oos_equity_curve`. Older rows keep every
/// scalar — including [`Trial::sr_per_obs`], which is exactly what the deflation trial set needs —
/// and have their curve emptied. See [`prune`].
pub const MAX_CURVE_ROWS: usize = 50;

/// Hard cap on one learning's text, in BYTES (truncated at a char boundary — never mid-sequence).
pub const MAX_LEARNING_TEXT: usize = 1024;

/// How many learnings the store retains; oldest dropped first.
pub const MAX_LEARNINGS: usize = 200;

/// Milliseconds since the Unix epoch; `0` if the clock is before the epoch (never, in practice —
/// but this must not panic inside a best-effort path).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Where the two ledger files live. Resolved by the CALLER (see the module doc's env boundary):
/// the library never derives this from the environment, only from a path it is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerPaths {
    pub trials: PathBuf,
    pub learnings: PathBuf,
}

impl LedgerPaths {
    /// The CONVENTIONAL pair under a store root — `<root>/ai_trials.json` +
    /// `<root>/ai_learnings.json`, colocated with `studio_strategies.json` /
    /// `studio_workspace.json` so the authoring memory sits next to the data it was earned on.
    pub fn under(store_root: &Path) -> Self {
        Self { trials: store_root.join(TRIALS_FILE), learnings: store_root.join(LEARNINGS_FILE) }
    }
}

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
fn finite_or_zero(x: f64) -> f64 {
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
    /// the deflation trial set (see `agent::deflate_with_prior`). Non-finite values are dropped:
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

/// Truncate to at most `max_bytes` BYTES at a char boundary — never splitting a UTF-8 sequence.
/// (A byte cap, not a char cap: the file size is what the bound exists to control.)
pub fn truncate_text(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Enforce both bounds on a ledger, in place: keep the newest [`MAX_TRIALS`] rows, and keep an
/// `oos_equity_curve` only on the newest [`MAX_CURVE_ROWS`] ACCEPTED rows (every other row's curve
/// is emptied — its scalars, including the load-bearing `sr_per_obs`, are untouched).
pub fn prune(ledger: &mut TrialLedger) {
    let n = ledger.trials.len();
    if n > MAX_TRIALS {
        ledger.trials.drain(..n - MAX_TRIALS);
    }
    let mut curves_kept = 0usize;
    for t in ledger.trials.iter_mut().rev() {
        if t.accepted && curves_kept < MAX_CURVE_ROWS {
            curves_kept += 1;
        } else {
            t.oos_equity_curve = Vec::new();
        }
    }
}

/// Read + parse the trial ledger at a CALLER-SUPPLIED path. Missing ⇒ `Default`; corrupt ⇒
/// `Default` plus one warning. Never an error — see the module doc.
pub fn load_trials(path: &Path) -> TrialLedger {
    load_json(path, "ai trial ledger")
}

/// Read + parse the learnings store at a CALLER-SUPPLIED path — the [`load_trials`] twin.
pub fn load_learnings(path: &Path) -> LearningStore {
    load_json(path, "ai learnings store")
}

/// Write the trial ledger to a CALLER-SUPPLIED path. A failure is one warning and nothing else.
pub fn save_trials(ledger: &TrialLedger, path: &Path) {
    save_json(ledger, path, "ai trial ledger");
}

/// Write the learnings store to a CALLER-SUPPLIED path — the [`save_trials`] twin.
pub fn save_learnings(store: &LearningStore, path: &Path) {
    save_json(store, path, "ai learnings store");
}

/// Load → append → [`prune`] → save, the one write path the authoring loop uses. Best-effort at
/// every step: a corrupt existing file is replaced by a fresh ledger holding just this trial
/// rather than aborting the append.
pub fn append_trial(path: &Path, trial: Trial) {
    let mut ledger = load_trials(path);
    ledger.trials.push(trial);
    prune(&mut ledger);
    save_trials(&ledger, path);
}

/// Load → truncate the text → append → cap → save. The [`append_trial`] twin for the
/// `record_learning` tool: `text` is capped at [`MAX_LEARNING_TEXT`] bytes and the file at
/// [`MAX_LEARNINGS`] rows (oldest dropped first).
pub fn append_learning(path: &Path, mut learning: Learning) {
    learning.text = truncate_text(&learning.text, MAX_LEARNING_TEXT);
    let mut store = load_learnings(path);
    store.learnings.push(learning);
    let n = store.learnings.len();
    if n > MAX_LEARNINGS {
        store.learnings.drain(..n - MAX_LEARNINGS);
    }
    save_learnings(&store, path);
}

/// How many prior trials [`prior_work_summary`] names in the prompt. Small on purpose: the point
/// is grounding, not a data dump — a long tail of near-identical rejected attempts would crowd out
/// the request itself.
pub const PROMPT_TOP_K: usize = 6;

/// The prompt-grounding block: what has already been tried on THIS slice and what the model chose
/// to remember about it. Prepended to the user prompt by the authoring loop (payoff (a); the
/// cross-session deflation is payoff (b) and lives in `agent::deflate_with_prior`).
///
/// Ranking is ONE comparator, documented rather than clever: accepted before rejected, then by
/// annualized OOS Sharpe descending, then newest first. Rejected rows all carry `oos_sharpe ==
/// 0.0`, so within that group the ordering degenerates to recency — which is what you want from
/// them ("here is what just failed"), while the accepted group leads with the current best.
///
/// Returns an EMPTY string when there is nothing to say (no matching trial, no matching learning),
/// so the caller can leave the prompt byte-identical to the pre-ledger one.
pub fn prior_work_summary(
    ledger: &TrialLedger,
    learnings: &LearningStore,
    venue: &str,
    symbol: &str,
    interval: &str,
    top_k: usize,
) -> String {
    let trials = ledger.for_slice(venue, symbol, interval);
    let notes = learnings.matching(venue, symbol, interval);
    if trials.is_empty() && notes.is_empty() {
        return String::new();
    }
    let accepted = trials.iter().filter(|t| t.accepted).count();
    let mut out = format!(
        "# Prior work on {symbol} @ {venue} {interval}\n\
         {} previous trial(s) on this exact slice, {accepted} accepted. Do not resubmit a \
         previously rejected approach unchanged.\n",
        trials.len()
    );
    if !notes.is_empty() {
        out.push_str("\n## Notes recorded on earlier runs\n");
        for n in &notes {
            let tag = match n.scope {
                LearningScope::Global => "global",
                LearningScope::Slice { .. } => "this slice",
            };
            out.push_str(&format!("- ({tag}) {}\n", n.text));
        }
    }
    if !trials.is_empty() {
        let mut ranked = trials;
        ranked.sort_by(|a, b| {
            b.accepted
                .cmp(&a.accepted)
                .then(b.oos_sharpe.total_cmp(&a.oos_sharpe))
                .then(b.ts_ms.cmp(&a.ts_ms))
        });
        out.push_str("\n## Previous attempts (best first)\n");
        for t in ranked.iter().take(top_k) {
            if t.accepted {
                out.push_str(&format!(
                    "- ACCEPTED: OOS Sharpe {:.2} over {} trades — {}\n",
                    t.oos_sharpe, t.n_trades, t.explanation
                ));
            } else {
                let why = if t.problems.is_empty() {
                    "no reason recorded".to_string()
                } else {
                    t.problems.join("; ")
                };
                out.push_str(&format!(
                    "- REJECTED after {} attempt(s): {why} — {}\n",
                    t.attempts, t.explanation
                ));
            }
        }
    }
    out
}

fn load_json<T: DeserializeOwned + Default>(path: &Path, what: &str) -> T {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return T::default(); // absent (or unreadable) ⇒ "nothing recorded yet", the OFF state
    };
    match serde_json::from_str::<T>(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("{what} at {} is unparseable, starting empty: {e}", path.display());
            T::default()
        }
    }
}

fn save_json<T: Serialize>(value: &T, path: &Path, what: &str) {
    match serde_json::to_string_pretty(value) {
        Err(e) => tracing::warn!("{what} could not be serialized, not written: {e}"),
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
                tracing::warn!("{what} could not be written to {}: {e}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial(i: usize, accepted: bool) -> Trial {
        Trial {
            ts_ms: 1_000 + i as i64,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            code: format!("code{i}"),
            explanation: format!("try {i}"),
            accepted,
            attempts: 1,
            problems: if accepted { vec![] } else { vec!["did not trade".into()] },
            oos_sharpe: i as f64 / 10.0,
            n_trades: i,
            oos_equity_curve: vec![100.0, 101.0, 100.5],
            deflated_sharpe: 0.0,
            // Exact-division form on purpose: `i as f64 / 100.0` rounds once to the nearest f64,
            // so the fixture's values are the same bits as the `0.01`/`0.03` literals the
            // assertions below compare against (`0.01 * 3.0` is NOT bit-equal to `0.03`).
            sr_per_obs: i as f64 / 100.0,
        }
    }

    #[test]
    fn trial_ledger_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(TRIALS_FILE);
        let ledger = TrialLedger { trials: vec![trial(1, true), trial(2, false)] };
        save_trials(&ledger, &p);
        assert_eq!(load_trials(&p), ledger, "disk round-trip is field-for-field lossless");
    }

    #[test]
    fn learnings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(LEARNINGS_FILE);
        let store = LearningStore {
            learnings: vec![
                Learning { ts_ms: 1, scope: LearningScope::Global, text: "g".into() },
                Learning {
                    ts_ms: 2,
                    scope: LearningScope::Slice {
                        venue: "binance".into(),
                        symbol: "BTCUSDT".into(),
                        interval: "1m".into(),
                    },
                    text: "s".into(),
                },
            ],
        };
        save_learnings(&store, &p);
        assert_eq!(load_learnings(&p), store);
    }

    #[test]
    fn trial_ledger_missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.json");
        assert_eq!(load_trials(&p), TrialLedger::default());
        assert_eq!(load_learnings(&p), LearningStore::default());
    }

    #[test]
    fn trial_ledger_corrupt_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(TRIALS_FILE);
        std::fs::write(&p, "{ not valid json ]").unwrap();
        assert_eq!(load_trials(&p), TrialLedger::default(), "corrupt file must not panic or error");
        // …and the loop must still be able to append over it.
        append_trial(&p, trial(7, true));
        assert_eq!(load_trials(&p).trials.len(), 1);
    }

    #[test]
    fn trial_ledger_prunes_to_cap() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(TRIALS_FILE);
        let mut ledger = TrialLedger { trials: (0..600).map(|i| trial(i, false)).collect() };
        prune(&mut ledger);
        assert_eq!(ledger.trials.len(), MAX_TRIALS);
        // Newest kept, oldest dropped: 600 in, the surviving window is [100, 600).
        assert_eq!(ledger.trials.first().unwrap().code, "code100");
        assert_eq!(ledger.trials.last().unwrap().code, "code599");
        save_trials(&ledger, &p);
        assert_eq!(load_trials(&p).trials.len(), MAX_TRIALS);
    }

    #[test]
    fn trial_ledger_prunes_curves() {
        // 120 accepted trials, each with a curve -> only the newest MAX_CURVE_ROWS keep one.
        let mut ledger = TrialLedger { trials: (0..120).map(|i| trial(i, true)).collect() };
        prune(&mut ledger);
        let with_curve: Vec<&Trial> =
            ledger.trials.iter().filter(|t| !t.oos_equity_curve.is_empty()).collect();
        assert_eq!(with_curve.len(), MAX_CURVE_ROWS);
        assert_eq!(with_curve.first().unwrap().code, format!("code{}", 120 - MAX_CURVE_ROWS));
        assert_eq!(with_curve.last().unwrap().code, "code119");
        // The curve-pruned rows keep every scalar — crucially `sr_per_obs`, so they still count
        // toward a later call's deflation trial set.
        let pruned = &ledger.trials[3]; // i=3: non-zero scalars, so this is not a vacuous check
        assert!(pruned.oos_equity_curve.is_empty());
        assert_eq!(pruned.sr_per_obs, 0.03);
        assert_eq!(pruned.n_trades, 3);
        assert_eq!(ledger.prior_sharpes_for("binance", "BTCUSDT", "1m").len(), 120);
    }

    #[test]
    fn prune_counts_only_accepted_rows_toward_the_curve_budget() {
        // Interleaved accept/reject: the curve budget is spent on ACCEPTED rows only, so a run of
        // rejected trials at the tail must not consume it.
        let mut ledger = TrialLedger { trials: (0..200).map(|i| trial(i, i % 2 == 0)).collect() };
        prune(&mut ledger);
        assert_eq!(
            ledger.trials.iter().filter(|t| !t.oos_equity_curve.is_empty()).count(),
            MAX_CURVE_ROWS
        );
        assert!(ledger.trials.iter().all(|t| t.oos_equity_curve.is_empty() || t.accepted));
    }

    #[test]
    fn learnings_cap() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(LEARNINGS_FILE);
        for i in 0..(MAX_LEARNINGS + 25) {
            append_learning(
                &p,
                Learning { ts_ms: i as i64, scope: LearningScope::Global, text: format!("n{i}") },
            );
        }
        let store = load_learnings(&p);
        assert_eq!(store.learnings.len(), MAX_LEARNINGS);
        assert_eq!(store.learnings.first().unwrap().text, "n25", "oldest dropped first");
        assert_eq!(store.learnings.last().unwrap().text, format!("n{}", MAX_LEARNINGS + 24));
    }

    #[test]
    fn learning_text_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(LEARNINGS_FILE);
        // A multibyte string longer than the cap: truncation must land on a char boundary, so the
        // stored text is still valid UTF-8 and strictly shorter than the input.
        let text = "é".repeat(MAX_LEARNING_TEXT); // 2 bytes each -> 2 KiB
        append_learning(&p, Learning { ts_ms: 1, scope: LearningScope::Global, text });
        let stored = &load_learnings(&p).learnings[0].text;
        assert!(stored.len() <= MAX_LEARNING_TEXT);
        assert_eq!(stored.chars().count(), MAX_LEARNING_TEXT / 2, "cut on a char boundary");
        assert!(stored.chars().all(|c| c == 'é'));
        // A short note is stored verbatim.
        assert_eq!(truncate_text("short", MAX_LEARNING_TEXT), "short");
    }

    #[test]
    fn prior_sharpes_are_slice_scoped_and_accepted_only() {
        let mut ledger = TrialLedger::default();
        ledger.trials.push(trial(1, true));
        ledger.trials.push(trial(2, false)); // rejected -> excluded
        let mut other = trial(3, true);
        other.symbol = "ETHUSDT".into(); // different slice -> excluded
        ledger.trials.push(other);
        let mut nan = trial(4, true);
        nan.sr_per_obs = f64::NAN; // non-finite -> excluded (would poison sample_variance)
        ledger.trials.push(nan);
        assert_eq!(ledger.prior_sharpes_for("binance", "BTCUSDT", "1m"), vec![0.01]);
    }

    /// The guard's own contract, pinned directly: adding the `warn` must not have turned it into a
    /// filter that lets a NaN through (which would brick the file) or that perturbs a finite value
    /// (`sr_per_obs` feeds deflation, so a rounded pass-through is a silent statistical change).
    #[test]
    fn finite_or_zero_replaces_non_finite_and_passes_finite_through_unchanged() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(finite_or_zero(bad), 0.0, "{bad} must still be substituted, not stored");
        }
        for ok in [0.0, -0.0, 1.25, -3.5, f64::MIN, f64::MAX, f64::MIN_POSITIVE] {
            // Bit equality, not `==`: `-0.0 == 0.0` would make the pass-through check vacuous.
            assert_eq!(finite_or_zero(ok).to_bits(), ok.to_bits(), "{ok} passes through verbatim");
        }
    }

    /// The never-brick guard: a run whose statistics came back non-finite must still produce a
    /// ledger that RELOADS. `serde_json` writes NaN as `null`, and `null` does not deserialize
    /// back into an `f64` — so without `finite_or_zero` one such run would silently discard every
    /// trial ever recorded on the next load.
    #[test]
    fn non_finite_statistics_do_not_brick_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(TRIALS_FILE);
        let r = AgentResult {
            accepted: true,
            attempts: 1,
            oos_sharpe: f64::NAN,
            deflated_sharpe: f64::INFINITY,
            n_trades: 3,
            oos_equity_curve: vec![100.0, f64::NAN, 101.0],
            ..Default::default()
        };
        append_trial(&p, Trial::from_result(42, "binance", "BTCUSDT", "1m", &r));
        let back = load_trials(&p);
        assert_eq!(back.trials.len(), 1, "the file must reload, not parse-fail into Default");
        let t = &back.trials[0];
        assert_eq!(t.oos_sharpe, 0.0);
        assert_eq!(t.deflated_sharpe, 0.0);
        assert_eq!(t.sr_per_obs, 0.0);
        assert!(t.oos_equity_curve.is_empty(), "a curve with a NaN carries no usable moments");
        assert_eq!(t.n_trades, 3, "the non-float scalars are untouched");
    }

    #[test]
    fn summary_is_empty_without_matching_trials_or_learnings() {
        let s = prior_work_summary(
            &TrialLedger::default(),
            &LearningStore::default(),
            "binance",
            "BTCUSDT",
            "1m",
            PROMPT_TOP_K,
        );
        assert!(s.is_empty(), "nothing to say -> the prompt stays byte-identical");
    }

    #[test]
    fn summary_names_sharpes_notes_and_rejection_reasons() {
        let mut ledger = TrialLedger::default();
        let mut good = trial(1, true);
        good.oos_sharpe = 1.25;
        good.n_trades = 7;
        good.explanation = "sma cross 5/20".into();
        ledger.trials.push(good);
        ledger.trials.push(trial(2, false));
        let store = LearningStore {
            learnings: vec![Learning {
                ts_ms: 1,
                scope: LearningScope::Global,
                text: "spreads widen 00:00-04:00 UTC".into(),
            }],
        };
        let s = prior_work_summary(&ledger, &store, "binance", "BTCUSDT", "1m", PROMPT_TOP_K);
        assert!(s.contains("1.25"), "the accepted trial's OOS Sharpe: {s}");
        assert!(s.contains("sma cross 5/20"));
        assert!(s.contains("did not trade"), "the rejection reason: {s}");
        assert!(s.contains("spreads widen 00:00-04:00 UTC"), "the global learning: {s}");
        // Accepted ranks above rejected.
        assert!(s.find("ACCEPTED").unwrap() < s.find("REJECTED").unwrap());
    }

    #[test]
    fn summary_hides_another_slices_learning() {
        let store = LearningStore {
            learnings: vec![Learning {
                ts_ms: 1,
                scope: LearningScope::Slice {
                    venue: "binance".into(),
                    symbol: "ETHUSDT".into(),
                    interval: "1m".into(),
                },
                text: "eth-only note".into(),
            }],
        };
        let mut ledger = TrialLedger::default();
        ledger.trials.push(trial(1, true));
        let s = prior_work_summary(&ledger, &store, "binance", "BTCUSDT", "1m", PROMPT_TOP_K);
        assert!(!s.contains("eth-only note"));
    }

    #[test]
    fn summary_caps_at_top_k() {
        let ledger = TrialLedger { trials: (0..20).map(|i| trial(i, true)).collect() };
        let s = prior_work_summary(
            &ledger,
            &LearningStore::default(),
            "binance",
            "BTCUSDT",
            "1m",
            PROMPT_TOP_K,
        );
        assert_eq!(s.matches("- ACCEPTED").count(), PROMPT_TOP_K);
    }
}
