//! The `develop_strategy_with_ledger` agentic loop: generate -> compile-validate -> OOS backtest ->
//! repair (Studio SP3 Part B, Task 5), and its N-candidate batch twin
//! `develop_strategies_with_ledger`, which deflates each ACCEPTED candidate's OOS Sharpe against
//! the whole trial set (Bailey & López de Prado 2014's deflated Sharpe ratio,
//! `vike_analytics::overfit::deflated_sharpe_ratio`) — the search-bias correction for "best of N
//! candidates". It targets the Rhai strategy seam (`vike_rhai::RhaiStrategy`).
//!
//! ## The backtester is the CALLER's (`backtest`)
//!
//! The loop compiles each candidate and builds no engine for it: its out-of-sample run is the
//! caller's `backtest` over an [`OutOfSampleSlice`]. Studio hands one that sends each candidate to the
//! backtest SERVER as the `RunSlice` a Run sends (`crates/vike-studio/src/backend/remote.rs`'s
//! `copilot_backtester`), so a candidate is computed there and charged exactly as a Run of the same
//! bars is (the venue's fee table; the owner's rulings of 2026-11-09). It is a parameter because
//! `vike-backtest` is this crate's own layer (30), the harness stays out of it
//! (`docs/decisions/0087-the-simulator-leaves-the-harness.md`), and the server is the caller's to dial.
//!
//! ## The ledger seam (`ledger: Option<&LedgerPaths>`)
//!
//! Both entry points take `Option<&LedgerPaths>` — the loop's cross-session memory (see
//! [`crate::ledger`]). `None` is the historical behavior, byte-identical:
//! no file read, no file write, no extra tool advertised, and the prompt handed to the model is
//! the caller's verbatim. `Some(paths)` buys two things:
//!
//! 1. **Prompt grounding** — [`crate::ledger::summary::prior_work_summary`] is prepended to the user
//!    prompt, so the loop stops rediscovering duds it already rejected on this exact slice.
//! 2. **Cross-session deflation** — the statistically real win. [`deflate_with_prior`] takes THIS
//!    call's accepted candidates ∪ every prior accepted trial on the same
//!    `(venue, symbol, interval)`, so a single-candidate run (one candidate — never ≥2 within
//!    one call, hence always `deflated_sharpe == 0.0` before this) finally produces a meaningful
//!    number. The multiple-testing correction is only honest if the trial count reflects every
//!    trial actually run against that slice, not just the ones that happened to share a process.
//!
//! Both are best-effort: an unavailable/corrupt ledger degrades to the `None` behavior rather than
//! failing the authoring loop.

pub mod deflate;
pub mod prompt;

mod authoring;

use serde::{Deserialize, Serialize};
use vike_analytics::BacktestResult;
use vike_data::{HistoryStore, TimestampRange};

use crate::client::LlmClient;
// Items, not the module: several functions below take a parameter NAMED `ledger`, and while a
// local can never shadow a module in path position, `ledger::load_trials(..)` sitting next to
// `ledger: Option<&LedgerPaths>` reads as if it could.
use crate::ledger::LedgerPaths;
use crate::ledger::store::{append_trial, load_trials};
use crate::ledger::trial::Trial;

use self::authoring::{refuse_foreign_quoted_symbol, refuse_inverse_symbol, run_authoring_loop};
use self::deflate::deflate_with_prior;

/// Outcome of one chat->strategy run.
///
/// `Serialize`/`Deserialize` (added with the trial ledger) is what lets a finished run be recorded
/// as a [`Trial`]; every field carries `#[serde(default)]` so an older ledger file still loads —
/// see [`crate::ledger`]'s forward-compat note.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentResult {
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
    #[serde(default)]
    pub oos_sharpe: f64,
    #[serde(default)]
    pub n_trades: usize,
    /// The equity curve over the held-out (OOS) window, captured only when `accepted` (empty
    /// otherwise). This is the raw material `develop_strategies_with_ledger`'s batch deflation needs
    /// (`metrics::returns_skewness`/`returns_kurtosis`/`returns(..).len()` for THIS candidate)
    /// without re-running the backtest a second time.
    #[serde(default)]
    pub oos_equity_curve: Vec<f64>,
    /// Bailey & López de Prado's (2014) deflated Sharpe ratio: this candidate's OOS Sharpe
    /// corrected for selection bias against the whole trial set — see
    /// [`develop_strategies_with_ledger`] and [`deflate_with_prior`]. Left at the `Default` `0.0`
    /// only when the trial set has fewer than 2 members (deflation is undefined without variance
    /// across trials): a LEDGER-LESS (`None`) single-candidate call, or a batch with fewer than 2
    /// accepted candidates and no prior trials. With a ledger, prior accepted trials on the same slice join the trial set, so even
    /// a single accepted candidate gets a real number.
    #[serde(default)]
    pub deflated_sharpe: f64,
}

/// What one candidate is backtested over: the slice the loop was asked about, cut to its held-out
/// tail (the last `holdout_fraction` of its bars). The caller's `backtest` receives it with the
/// candidate's source.
#[derive(Debug, Clone, Copy)]
pub struct OutOfSampleSlice<'a> {
    pub venue: &'a str,
    pub symbol: &'a str,
    pub interval: &'a str,
    /// The held-out bars' first and last timestamp, both inclusive.
    pub range: TimestampRange,
}

/// Generate -> compile-validate -> OOS-backtest -> repair loop. Scores ONLY on the out-of-sample
/// tail the model never saw (`holdout_fraction`). Returns the last submission's outcome.
///
/// `ledger` is the persistent trial ledger: the module doc's "ledger seam" section says what
/// `Some(paths)` buys (prompt grounding + cross-session deflation). `None` is LEDGER-LESS, exactly
/// the pre-ledger behavior (no file read, no file write, no `record_learning` tool, the prompt
/// handed through verbatim, `deflated_sharpe` always `0.0`).
///
/// `backtest` runs one compiled candidate over its [`OutOfSampleSlice`] (the module doc's "backtester"
/// section); its `Err` is a refusal the model is handed to repair, like a compile error.
///
/// `max_repairs` caps the number of submission ATTEMPTS actually processed (compiled +
/// backtested) — not the number of tool calls the model happens to make. The first `max_repairs`
/// `submit_strategy` calls are compiled/backtested as usual; once that many attempts have been
/// processed (whether they failed or the loop already returned on acceptance), every further
/// `submit_strategy` call is refused immediately — no compile, no backtest, not accepted — with a
/// terminal "maximum repair attempts reached" message handed back to the model. This bounds the
/// loop even against a client/model that keeps calling the tool past the cap (`MAX_TURNS` inside
/// each concrete `LlmClient` is a separate, coarser bound on total model turns).
///
/// Order is load-bearing: the prior trial set is read BEFORE this run's trial is appended, so a
/// candidate is never counted twice in its own deflation.
pub fn develop_strategy_with_ledger(
    prompt: &str,
    venue: &str,
    symbol: &str,
    interval: &str,
    store: &dyn HistoryStore,
    client: &dyn LlmClient,
    backtest: &dyn Fn(&str, OutOfSampleSlice<'_>) -> Result<BacktestResult, String>,
    max_repairs: u32,
    holdout_fraction: f64,
    ledger: Option<&LedgerPaths>,
) -> AgentResult {
    // A foreign-quoted or coin-margined symbol is refused before the model is asked and before any
    // ledger write.
    if let Some(refused) =
        refuse_foreign_quoted_symbol(symbol).or_else(|| refuse_inverse_symbol(venue, symbol))
    {
        return refused;
    }
    let mut result = run_authoring_loop(
        prompt,
        venue,
        symbol,
        interval,
        store,
        client,
        backtest,
        max_repairs,
        holdout_fraction,
        ledger,
    );
    if let Some(paths) = ledger {
        let prior = load_trials(&paths.trials).prior_sharpes_for(venue, symbol, interval);
        deflate_with_prior(std::slice::from_mut(&mut result), &prior);
        append_trial(
            &paths.trials,
            Trial::from_result(vike_model::now_ms(), venue, symbol, interval, &result),
        );
    }
    result
}

/// Generate `n` candidates via the authoring loop, then deflate each ACCEPTED candidate's OOS
/// Sharpe against the whole trial set (Bailey & López de Prado's 2014 deflated Sharpe ratio,
/// `vike_analytics::overfit::deflated_sharpe_ratio` — see the module doc).
///
/// `backtest` and `ledger` as in [`develop_strategy_with_ledger`] (`None` = ledger-less). The
/// batch runs the authoring loop `n` times with NO per-candidate deflation or recording, then deflates the whole
/// batch against `batch ∪ prior` in ONE pass and appends every candidate afterwards — so candidate
/// 2's trial set is not silently widened by candidate 1 having already been written to disk
/// mid-batch.
// The single-candidate door's ten (at the lint's limit) plus `n`, each a separate knob of the loop.
#[expect(clippy::too_many_arguments)]
pub fn develop_strategies_with_ledger(
    prompt: &str,
    venue: &str,
    symbol: &str,
    interval: &str,
    store: &dyn HistoryStore,
    client: &dyn LlmClient,
    backtest: &dyn Fn(&str, OutOfSampleSlice<'_>) -> Result<BacktestResult, String>,
    n: u32,
    max_repairs: u32,
    holdout_fraction: f64,
    ledger: Option<&LedgerPaths>,
) -> Vec<AgentResult> {
    // Refused once for the whole batch, as `develop_strategy_with_ledger` refuses one: `n` copies, so
    // a caller that indexes the result per candidate still finds `n`, and no trial is recorded.
    if let Some(refused) =
        refuse_foreign_quoted_symbol(symbol).or_else(|| refuse_inverse_symbol(venue, symbol))
    {
        return (0..n).map(|_| refused.clone()).collect();
    }
    let mut results: Vec<AgentResult> = (0..n)
        .map(|_| {
            run_authoring_loop(
                prompt,
                venue,
                symbol,
                interval,
                store,
                client,
                backtest,
                max_repairs,
                holdout_fraction,
                ledger,
            )
        })
        .collect();
    let prior = ledger
        .map(|p| load_trials(&p.trials).prior_sharpes_for(venue, symbol, interval))
        .unwrap_or_default();
    deflate_with_prior(&mut results, &prior);
    if let Some(paths) = ledger {
        let now = vike_model::now_ms();
        for r in &results {
            append_trial(&paths.trials, Trial::from_result(now, venue, symbol, interval, r));
        }
    }
    results
}

#[cfg(test)]
mod tests;
