//! The `develop_strategy` agentic loop: generate -> compile-validate -> OOS backtest -> repair
//! (Studio SP3 Part B, Task 5), and its N-candidate batch twin `develop_strategies`, which
//! deflates each ACCEPTED candidate's OOS Sharpe against the whole trial set (Bailey & López de
//! Prado 2014's deflated Sharpe ratio, `vike_analytics::overfit::deflated_sharpe_ratio`) — the
//! search-bias correction for "best of N candidates". Ported from vike-trader-app `ai/agent.py`
//! (including that file's `_attach_overfit`), targeting the Rhai strategy seam
//! (`vike_script::RhaiStrategy`) instead of the Python oracle.
//!
//! ## The ledger seam (`*_with_ledger`)
//!
//! Every entry point here has a `_with_ledger` twin taking `Option<&LedgerPaths>` — the loop's
//! cross-session memory (see [`crate::ledger`]). `None` is the historical behavior, byte-identical:
//! no file read, no file write, no extra tool advertised, and the prompt handed to the model is
//! the caller's verbatim. `Some(paths)` buys two things:
//!
//! 1. **Prompt grounding** — [`crate::ledger::prior_work_summary`] is prepended to the user
//!    prompt, so the loop stops rediscovering duds it already rejected on this exact slice.
//! 2. **Cross-session deflation** — the statistically real win. [`deflate_with_prior`] takes THIS
//!    call's accepted candidates ∪ every prior accepted trial on the same
//!    `(venue, symbol, interval)`, so a bare `develop_strategy` (one candidate — never ≥2 within
//!    one call, hence always `deflated_sharpe == 0.0` before this) finally produces a meaningful
//!    number. The multiple-testing correction is only honest if the trial count reflects every
//!    trial actually run against that slice, not just the ones that happened to share a process.
//!
//! Both are best-effort: an unavailable/corrupt ledger degrades to the `None` behavior rather than
//! failing the authoring loop.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use serde_json::json;
use vike_analytics::report::periods_per_year_for_interval;
use vike_analytics::{metrics, overfit};
use vike_data::{HistStore, TsRange};
use vike_script::RhaiStrategy;
use vike_sim::{EngineParams, SimBroker, StrategyEngine};

use crate::client::{LlmClient, ToolCall, ToolSpec};
// Items, not the module: several functions below take a parameter NAMED `ledger`, and while a
// local can never shadow a module in path position, `ledger::load_trials(..)` sitting next to
// `ledger: Option<&LedgerPaths>` reads as if it could.
use crate::ledger::{
    Learning, LearningScope, LedgerPaths, PROMPT_TOP_K, Trial, append_learning, append_trial,
    load_learnings, load_trials, now_ms, prior_work_summary,
};

// (`PPY` moved into the test module: the real path derives its annualization from the slice's
// interval now, and a module-level constant used only by tests is dead code under `-D warnings`.)

/// The Rhai strategy API contract handed to the model as the system prompt.
pub const STRATEGY_SYSTEM_PROMPT: &str = r#"You write trading strategies in Rhai (a Rust-like scripting language) for the vike backtester.
Contract:
- Define `fn on_bar() { ... }`, called once per bar.
- Reads (host functions): position() -> current signed position; price() -> last close;
  indicators -- sma(n), ema(n), rsi(n), atr(n), adx(n), cci(n) and most of the vike-indicators
  registry, each NaN until warmed up. Call one with no argument to take its default period
  (sma() == sma(20)). Multi-output indicators (macd, bollinger, stochastic) are NOT callable:
  a single number cannot say which line you meant. A name that is not bound raises on the first
  call rather than returning a value, so a guess fails loudly.
- Orders (host functions): market(dir, qty) where dir is 1 (buy) or -1 (sell) and qty > 0;
  buy(qty) / sell(qty) as shortcuts. There is no cross-bar mutable state in the script -- all state
  (position, indicator windows) is host-side; read it fresh each bar.
- Parameters: `let x = param("x", default);` at the top level declares a sweepable knob.
- Guard indicators: `if s.is_nan() { return; }` before using sma/ema/rsi.
Call the `submit_strategy` tool with your complete script in `code` and a one-line `explanation`.
If your script fails to compile or does not trade, you'll get the error -- fix it and resubmit."#;

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
    /// otherwise). This is the raw material `develop_strategies`' batch deflation needs
    /// (`metrics::returns_skewness`/`returns_kurtosis`/`returns(..).len()` for THIS candidate)
    /// without re-running the backtest a second time.
    #[serde(default)]
    pub oos_equity_curve: Vec<f64>,
    /// Bailey & López de Prado's (2014) deflated Sharpe ratio: this candidate's OOS Sharpe
    /// corrected for selection bias against the whole trial set — see `develop_strategies` and
    /// [`deflate_with_prior`]. Left at the `Default` `0.0` only when the trial set has fewer than
    /// 2 members (deflation is undefined without variance across trials): a LEDGER-LESS bare
    /// `develop_strategy` call, or a batch with fewer than 2 accepted candidates and no prior
    /// trials. With a ledger, prior accepted trials on the same slice join the trial set, so even
    /// a single accepted candidate gets a real number.
    #[serde(default)]
    pub deflated_sharpe: f64,
}

fn submit_tool() -> ToolSpec {
    ToolSpec {
        name: "submit_strategy".into(),
        description: "Submit a complete Rhai strategy script (code) plus a one-line explanation."
            .into(),
        input_schema: json!({"type":"object","properties":{"code":{"type":"string"},"explanation":{"type":"string"}},"required":["code","explanation"]}),
    }
}

/// The `record_learning` tool — advertised ONLY when a ledger path is configured. A tool that
/// provably discards its input is worse than an absent one: it burns model turns and teaches the
/// model to trust a note that was never written. With no ledger there is nowhere to write, so the
/// tool is simply not offered.
fn record_learning_tool() -> ToolSpec {
    ToolSpec {
        name: "record_learning".into(),
        description: "Record a durable note about this market/slice for FUTURE sessions to read \
                      (e.g. \"wide-spread regimes on this symbol cluster 00:00-04:00 UTC\"). \
                      `scope` is \"global\" for a note that holds anywhere, or \"slice\" (the \
                      default) for one that only applies to the instrument being worked on. This \
                      does NOT submit a strategy."
            .into(),
        input_schema: json!({"type":"object","properties":{"scope":{"type":"string","enum":["global","slice"]},"text":{"type":"string"}},"required":["text"]}),
    }
}

/// Generate -> compile-validate -> OOS-backtest -> repair loop. Scores ONLY on the out-of-sample
/// tail the model never saw (`holdout_frac`). Returns the last submission's outcome.
///
/// LEDGER-LESS: exactly the pre-ledger behavior (no file read, no file write, no
/// `record_learning` tool, the prompt handed through verbatim, `deflated_sharpe` always `0.0`).
/// [`develop_strategy_with_ledger`] is the twin that remembers.
///
/// `max_repairs` caps the number of submission ATTEMPTS actually processed (compiled +
/// backtested) — not the number of tool calls the model happens to make. The first `max_repairs`
/// `submit_strategy` calls are compiled/backtested as usual; once that many attempts have been
/// processed (whether they failed or the loop already returned on acceptance), every further
/// `submit_strategy` call is refused immediately — no compile, no backtest, not accepted — with a
/// terminal "maximum repair attempts reached" message handed back to the model. This bounds the
/// loop even against a client/model that keeps calling the tool past the cap (`MAX_TURNS` inside
/// each concrete `LlmClient` is a separate, coarser bound on total model turns).
pub fn develop_strategy(
    prompt: &str,
    venue: &str,
    symbol: &str,
    interval: &str,
    store: &dyn HistStore,
    client: &dyn LlmClient,
    max_repairs: u32,
    holdout_frac: f64,
) -> AgentResult {
    develop_strategy_with_ledger(
        prompt,
        venue,
        symbol,
        interval,
        store,
        client,
        max_repairs,
        holdout_frac,
        None,
    )
}

/// [`develop_strategy`] with the persistent trial ledger wired in — see the module doc's "ledger
/// seam" section for what `Some(paths)` buys (prompt grounding + cross-session deflation) and why
/// `None` is byte-identical to the pre-ledger loop.
///
/// Order is load-bearing: the prior trial set is read BEFORE this run's trial is appended, so a
/// candidate is never counted twice in its own deflation.
pub fn develop_strategy_with_ledger(
    prompt: &str,
    venue: &str,
    symbol: &str,
    interval: &str,
    store: &dyn HistStore,
    client: &dyn LlmClient,
    max_repairs: u32,
    holdout_frac: f64,
    ledger: Option<&LedgerPaths>,
) -> AgentResult {
    let mut result = run_authoring_loop(
        prompt,
        venue,
        symbol,
        interval,
        store,
        client,
        max_repairs,
        holdout_frac,
        ledger,
    );
    if let Some(paths) = ledger {
        let prior = load_trials(&paths.trials).prior_sharpes_for(venue, symbol, interval);
        deflate_with_prior(std::slice::from_mut(&mut result), &prior);
        append_trial(&paths.trials, Trial::from_result(now_ms(), venue, symbol, interval, &result));
    }
    result
}

/// Generate `n` candidates via the authoring loop, then deflate each ACCEPTED candidate's OOS
/// Sharpe against the whole trial set (Bailey & López de Prado's 2014 deflated Sharpe ratio,
/// `vike_analytics::overfit::deflated_sharpe_ratio` — see the module doc). Ported from
/// vike-trader-app `ai/agent.py`'s `develop_strategies` + `_attach_overfit`.
///
/// LEDGER-LESS, like [`develop_strategy`]; [`develop_strategies_with_ledger`] is the twin.
pub fn develop_strategies(
    prompt: &str,
    venue: &str,
    symbol: &str,
    interval: &str,
    store: &dyn HistStore,
    client: &dyn LlmClient,
    n: u32,
    max_repairs: u32,
    holdout_frac: f64,
) -> Vec<AgentResult> {
    develop_strategies_with_ledger(
        prompt,
        venue,
        symbol,
        interval,
        store,
        client,
        n,
        max_repairs,
        holdout_frac,
        None,
    )
}

/// [`develop_strategies`] with the ledger wired in. The batch runs the authoring loop `n` times
/// with NO per-candidate deflation or recording, then deflates the whole batch against
/// `batch ∪ prior` in ONE pass and appends every candidate afterwards — so candidate 2's trial set
/// is not silently widened by candidate 1 having already been written to disk mid-batch.
pub fn develop_strategies_with_ledger(
    prompt: &str,
    venue: &str,
    symbol: &str,
    interval: &str,
    store: &dyn HistStore,
    client: &dyn LlmClient,
    n: u32,
    max_repairs: u32,
    holdout_frac: f64,
    ledger: Option<&LedgerPaths>,
) -> Vec<AgentResult> {
    let mut results: Vec<AgentResult> = (0..n)
        .map(|_| {
            run_authoring_loop(
                prompt,
                venue,
                symbol,
                interval,
                store,
                client,
                max_repairs,
                holdout_frac,
                ledger,
            )
        })
        .collect();
    let prior = ledger
        .map(|p| load_trials(&p.trials).prior_sharpes_for(venue, symbol, interval))
        .unwrap_or_default();
    deflate_with_prior(&mut results, &prior);
    if let Some(paths) = ledger {
        let now = now_ms();
        for r in &results {
            append_trial(&paths.trials, Trial::from_result(now, venue, symbol, interval, r));
        }
    }
    results
}

/// The generate -> compile-validate -> OOS-backtest -> repair loop itself, WITHOUT deflation or
/// trial recording — the shared body of [`develop_strategy_with_ledger`] and
/// [`develop_strategies_with_ledger`], which own those two steps at their own (single vs batch)
/// cadence. `ledger` here is used only for the two IN-loop concerns: prompt grounding and the
/// `record_learning` tool.
fn run_authoring_loop(
    prompt: &str,
    venue: &str,
    symbol: &str,
    interval: &str,
    store: &dyn HistStore,
    client: &dyn LlmClient,
    max_repairs: u32,
    holdout_frac: f64,
    ledger: Option<&LedgerPaths>,
) -> AgentResult {
    let bars = store.load_bars(venue, symbol, interval, TsRange::all()).unwrap_or_default();
    let split = ((bars.len() as f64) * (1.0 - holdout_frac)) as usize;
    let oos: Vec<_> = bars.get(split..).map(|s| s.to_vec()).unwrap_or_default();

    // Payoff (a): ground the prompt in what this slice already taught us. Empty (and therefore
    // byte-identical to the pre-ledger prompt) with no ledger, or with nothing recorded yet.
    let grounding = ledger
        .map(|p| {
            prior_work_summary(
                &load_trials(&p.trials),
                &load_learnings(&p.learnings),
                venue,
                symbol,
                interval,
                PROMPT_TOP_K,
            )
        })
        .unwrap_or_default();
    let user_prompt =
        if grounding.is_empty() { prompt.to_string() } else { format!("{grounding}\n{prompt}") };

    let result = RefCell::new(AgentResult::default());
    let mut tools = vec![submit_tool()];
    if ledger.is_some() {
        tools.push(record_learning_tool());
    }
    // dispatch: compile+backtest each submitted script; return the error text (repair) or "accepted".
    // Once `max_repairs` attempts have been PROCESSED, refuse further submissions outright (see the
    // doc comment above) instead of compiling/backtesting them.
    //
    // `record_learning` is routed by NAME; every other name (including `submit_strategy`) falls
    // through to the submission path — deliberately preserving the pre-ledger leniency, where the
    // loop never checked the tool name at all.
    let mut dispatch = |call: ToolCall| -> String {
        if call.name == "record_learning" {
            return dispatch_record_learning(&call, venue, symbol, interval, ledger);
        }
        let mut r = result.borrow_mut();
        if r.attempts >= max_repairs {
            return "Maximum repair attempts reached; no further submissions will be processed."
                .to_string();
        }
        let code = call.input.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string();
        let explanation =
            call.input.get("explanation").and_then(|e| e.as_str()).unwrap_or("").to_string();
        r.attempts += 1;
        r.code = code.clone();
        r.explanation = explanation;
        match RhaiStrategy::<SimBroker>::compile(&code) {
            Err(e) => {
                let msg = format!("compile error: {e}");
                r.problems.push(msg.clone());
                r.accepted = false;
                format!("Your script failed to compile: {e}. Fix and resubmit.")
            }
            Ok(strat) => {
                if oos.is_empty() {
                    return "No out-of-sample data available to backtest.".into();
                }
                let res = StrategyEngine::new(
                    vec![(symbol.to_string(), oos.clone())],
                    strat,
                    EngineParams::default(),
                )
                .run();
                if res.n_trades == 0 {
                    let msg = "strategy did not trade over the OOS window".to_string();
                    r.problems.push(msg.clone());
                    r.accepted = false;
                    format!("{msg}. Adjust the logic and resubmit.")
                } else {
                    r.accepted = true;
                    r.oos_sharpe =
                        metrics::sharpe(&res.equity_curve, periods_per_year_for_interval(interval));
                    r.n_trades = res.n_trades;
                    r.oos_equity_curve = res.equity_curve.clone();
                    format!(
                        "Accepted. OOS Sharpe {:.2} over {} trades.",
                        r.oos_sharpe, res.n_trades
                    )
                }
            }
        }
    };
    let _ = client.run(STRATEGY_SYSTEM_PROMPT, &user_prompt, &tools, &mut dispatch);
    result.into_inner()
}

/// Handle one `record_learning` tool call: append the (truncated) note under the requested scope
/// and hand the model a one-line acknowledgement. Best-effort like everything else in the ledger —
/// an unwritable file costs a note, never the run.
///
/// `scope` defaults to `slice` (the narrower blast radius) for anything that is not the exact
/// string `"global"`, including an absent argument.
fn dispatch_record_learning(
    call: &ToolCall,
    venue: &str,
    symbol: &str,
    interval: &str,
    ledger: Option<&LedgerPaths>,
) -> String {
    let Some(paths) = ledger else {
        // Unreachable in practice — the tool is only advertised when a ledger exists — but a model
        // may call a tool it was never offered, and that must be an honest refusal, not a
        // silently-dropped note.
        return "No learnings store is configured; nothing was recorded.".to_string();
    };
    let text = call.input.get("text").and_then(|t| t.as_str()).unwrap_or("").trim().to_string();
    if text.is_empty() {
        return "Nothing recorded: `text` was empty.".to_string();
    }
    let global = call.input.get("scope").and_then(|s| s.as_str()) == Some("global");
    let scope = if global {
        LearningScope::Global
    } else {
        LearningScope::Slice {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
        }
    };
    append_learning(&paths.learnings, Learning { ts_ms: now_ms(), scope, text });
    let where_ = if global { "globally" } else { "for this slice" };
    format!("Recorded {where_}. Now submit a strategy with `submit_strategy`.")
}

/// Deflate every ACCEPTED candidate's `oos_sharpe` in place against the shared trial set — the
/// search that was ACTUALLY performed against this slice: every accepted candidate in THIS call,
/// plus `prior_sharpes`, the per-period Sharpes of accepted trials recorded on the same slice in
/// earlier calls/sessions ([`crate::ledger::TrialLedger::prior_sharpes_for`]).
///
/// **Why the prior half matters.** Bailey & López de Prado's correction is a multiple-testing
/// correction: `sr_star` is the expected MAXIMUM Sharpe across `N` trials, so it is only honest if
/// `N` counts every trial run against that slice. Before the ledger, `N` was "candidates that
/// happened to share one process", which for the production path (`develop_strategy`, one
/// candidate) was 1 — deflation undefined, `deflated_sharpe` permanently `0.0`. A slice you have
/// hammered with 200 attempts across 40 sessions deserves a far higher bar than a fresh one, and
/// only the ledger knows that.
///
/// Trial set: per-period Sharpes only. Per-candidate skew/kurtosis/observation-count are derived
/// from THAT candidate's own `oos_equity_curve`; a prior trial contributes ONLY its scalar
/// `sr_per_obs`, which is exactly why [`crate::ledger::Trial`] persists that scalar and can
/// therefore prune the curve without shrinking the correction.
///
/// ORDER: this call's candidates first (in slice order), then `prior_sharpes` in ledger append
/// order. `sample_variance` folds f64s in that order, so it is fixed rather than incidental — and
/// an empty `prior_sharpes` reproduces the pre-ledger fold bit-for-bit.
///
/// UNITS: every statistical input comes from `overfit::sharpe_moments`, the shared derivation
/// helper that owns both load-bearing conventions (per-period — NOT annualized — Sharpe, and
/// non-excess kurtosis) and documents why each is a footgun; the annualized-Sharpe one shipped as
/// a real bug here once, pinned below by `deflate_batch_pins_per_period_dsr_magnitude`. Read that
/// helper's doc before touching this. The displayed `oos_sharpe` field stays annualized
/// (`metrics::sharpe` at `vike_analytics::report::periods_per_year_for_interval` of the slice's own
/// interval — it used to be a hardcoded 252 at every interval, which understated an intraday
/// Copilot Sharpe by `sqrt(24)` on 1h and `sqrt(1440)` on 1m) — a different quantity, for display
/// only, that must never reach `overfit::`. Using `sharpe_moments` for BOTH the observed value AND
/// every trial keeps the
/// observed value, the trial set, and the derived `sr_star` internally consistent.
///
/// Fewer than 2 trials in TOTAL -> deflation is undefined (no variance across trials to estimate
/// the expected-max-Sharpe benchmark from): every result's `deflated_sharpe` is left at its
/// `Default` `0.0` rather than computed, NaN, or panicking.
pub fn deflate_with_prior(results: &mut [AgentResult], prior_sharpes: &[f64]) {
    let mut trial_sharpes: Vec<f64> = results
        .iter()
        .filter(|r| r.accepted)
        .map(|r| overfit::sharpe_moments(&r.oos_equity_curve).sr_per_obs)
        .collect();
    // Prior rows are already finiteness-filtered by `prior_sharpes_for`; this call's candidates
    // are not, but a non-finite one here would equally have poisoned the pre-ledger fold, so the
    // behavior is deliberately left unchanged.
    trial_sharpes.extend_from_slice(prior_sharpes);
    if trial_sharpes.len() < 2 {
        return;
    }
    for r in results.iter_mut().filter(|r| r.accepted) {
        let m = overfit::sharpe_moments(&r.oos_equity_curve);
        r.deflated_sharpe =
            overfit::deflated_sharpe_ratio(m.sr_per_obs, &trial_sharpes, m.n_obs, m.skew, m.kurt);
    }
}

/// [`deflate_with_prior`] with no prior trials — the pre-ledger, within-one-batch deflation, kept
/// as a named entry point because that is the contract every existing test pins.
#[cfg(test)]
fn deflate_batch(results: &mut [AgentResult]) {
    deflate_with_prior(results, &[]);
}

#[path = "agent_tests.rs"]
#[cfg(test)]
mod agent_tests;
