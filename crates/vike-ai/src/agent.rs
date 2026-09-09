//! The `develop_strategy` agentic loop: generate -> compile-validate -> OOS backtest -> repair
//! (Studio SP3 Part B, Task 5), and its N-candidate batch twin `develop_strategies`, which
//! deflates each ACCEPTED candidate's OOS Sharpe against the whole trial set (Bailey & López de
//! Prado 2014's deflated Sharpe ratio, `vike_backtest::overfit::deflated_sharpe_ratio`) — the
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
use vike_backtest::report::periods_per_year_for_interval;
use vike_backtest::{EngineParams, SimBroker, StrategyEngine, metrics, overfit};
use vike_data::{HistStore, TsRange};
use vike_script::RhaiStrategy;

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
#[allow(clippy::too_many_arguments)] // one seam per (venue,symbol,interval,store,client,repairs,holdout) knob
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
#[allow(clippy::too_many_arguments)] // develop_strategy's knobs plus the ledger seam
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
/// `vike_backtest::overfit::deflated_sharpe_ratio` — see the module doc). Ported from
/// vike-trader-app `ai/agent.py`'s `develop_strategies` + `_attach_overfit`.
///
/// LEDGER-LESS, like [`develop_strategy`]; [`develop_strategies_with_ledger`] is the twin.
#[allow(clippy::too_many_arguments)] // batch twin of develop_strategy plus `n`
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
#[allow(clippy::too_many_arguments)] // develop_strategies' knobs plus the ledger seam
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
#[allow(clippy::too_many_arguments)] // the develop_strategy knob set, unchanged
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
/// (`metrics::sharpe` at `vike_backtest::report::periods_per_year_for_interval` of the slice's own
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The daily anchor, kept HERE rather than on the module because the real path no longer has a
    /// fixed annualization — `run_authoring_loop` derives it from the slice's interval.
    ///
    /// These tests keep 252 deliberately: every one of them checks a SCALE-INVARIANT property (that
    /// the displayed annualized Sharpe never reaches `overfit::`, and that the deflation runs on
    /// per-observation values), and a scale-invariant claim is clearest when the scale is a
    /// constant the reader can see.
    const PPY: f64 = 252.0;
    use crate::client::{FakeClient, FakeTurn, LlmError, ToolCall};
    // The module itself here (no local named `ledger` in this scope) — the tests drive the ledger
    // files directly to set up prior state.
    use crate::ledger::{self, TrialLedger};
    use serde_json::json;
    use std::sync::Arc;
    use vike_data::{HistStore, MemHistStore};
    use vike_model::Bar;

    const CROSS: &str = r#"
const QTY = 1.0;
fn on_bar() { let f=sma(5); let s=sma(20); if s.is_nan(){return;} let tgt=if f>s{QTY}else{-QTY}; let d=tgt-position(); if abs(d)>1e-12 { market(if d>0.0{1}else{-1}, abs(d)); } }
"#;

    /// An sma-cross script with the given (fast, slow) windows — used to give
    /// `develop_strategies_deflates_every_accepted_candidate` a trial set with real variance
    /// (different windows -> different OOS Sharpes on the same seeded data), unlike 3 identical
    /// `CROSS` submissions which would give the trial set zero variance.
    fn cross_script(fast: u32, slow: u32) -> String {
        format!(
            "const QTY = 1.0;\nfn on_bar() {{ let f=sma({fast}); let s=sma({slow}); if s.is_nan(){{return;}} let tgt=if f>s{{QTY}}else{{-QTY}}; let d=tgt-position(); if abs(d)>1e-12 {{ market(if d>0.0{{1}}else{{-1}}, abs(d)); }} }}"
        )
    }

    /// A bar-seeded `MemHistStore` (real in-memory bar storage behind `test-support` — no
    /// DataFusion in this crate's dev graph) plus the TempDir the ledger tests park their files
    /// under: the store no longer owns a directory, so the dir is the tests' own.
    fn seeded() -> (tempfile::TempDir, Arc<MemHistStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = MemHistStore::new();
        let mut px = 100.0f64;
        let mut seed = 0x1234_5678u64;
        let bars: Vec<Bar> = (0..400)
            .map(|i| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                px = (px + ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0).max(1.0);
                Bar {
                    ts: 60_000 * (i as i64 + 1),
                    open: px,
                    high: px,
                    low: px,
                    close: px,
                    volume: 0.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some("BTCUSDT".into()),
                }
            })
            .collect();
        store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
        (dir, Arc::new(store))
    }

    fn submit_call(code: &str) -> ToolCall {
        ToolCall {
            id: "t".into(),
            name: "submit_strategy".into(),
            input: json!({"code":code,"explanation":"e"}),
        }
    }

    fn submit(code: &str) -> FakeTurn {
        FakeTurn::Tool(submit_call(code))
    }

    /// A `FakeClient` that also RECORDS what the loop handed it: the user prompt (for the
    /// grounding tests) and the advertised tool names (for the `record_learning` gating test).
    /// `FakeClient` deliberately ignores both, so those two claims need their own double.
    #[derive(Default)]
    struct CapturingClient {
        /// The user prompt of every `run` call, in order.
        prompts: RefCell<Vec<String>>,
        /// The names of every tool advertised on every `run` call.
        tools: RefCell<Vec<String>>,
        /// Tool calls to emit once, on the first `run`.
        script: RefCell<Vec<ToolCall>>,
    }

    impl CapturingClient {
        fn scripted(calls: Vec<ToolCall>) -> Self {
            Self { script: RefCell::new(calls), ..Default::default() }
        }
        fn prompt(&self) -> String {
            self.prompts.borrow().first().cloned().expect("the loop calls the client exactly once")
        }
    }

    impl LlmClient for CapturingClient {
        fn run(
            &self,
            _system: &str,
            user: &str,
            tools: &[ToolSpec],
            dispatch: &mut dyn FnMut(ToolCall) -> String,
        ) -> Result<String, LlmError> {
            self.prompts.borrow_mut().push(user.to_string());
            self.tools.borrow_mut().extend(tools.iter().map(|t| t.name.clone()));
            let calls: Vec<ToolCall> = self.script.borrow_mut().drain(..).collect();
            for c in calls {
                let _ = dispatch(c);
            }
            Ok(String::new())
        }
    }

    /// A prior ACCEPTED trial on the seeded slice, carrying only the scalar the deflation trial
    /// set actually reads (`sr_per_obs`) — the curve-pruned shape a ledger row eventually takes.
    fn prior_accepted(i: usize, sr_per_obs: f64, oos_sharpe: f64) -> Trial {
        Trial {
            ts_ms: 1_000 + i as i64,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            explanation: format!("prior attempt {i}"),
            accepted: true,
            attempts: 1,
            oos_sharpe,
            n_trades: 5 + i,
            sr_per_obs,
            ..Default::default()
        }
    }

    #[test]
    fn happy_path_accepts_a_valid_strategy() {
        let (_d, store) = seeded();
        let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
        let r = develop_strategy(
            "make an sma cross",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &client,
            2,
            0.3,
        );
        assert!(r.accepted);
        assert_eq!(r.attempts, 1);
        assert!(r.n_trades > 0);
    }

    #[test]
    fn repair_path_recovers_after_a_compile_error() {
        let (_d, store) = seeded();
        let client = FakeClient::new(vec![
            submit("fn on_bar( {"),
            submit(CROSS),
            FakeTurn::Text("done".into()),
        ]);
        let r = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 0.3);
        assert!(r.accepted);
        assert_eq!(r.attempts, 2);
        assert!(!r.problems.is_empty());
    }

    #[test]
    fn give_up_after_max_repairs() {
        let (_d, store) = seeded();
        // Script MORE broken submissions than max_repairs allows -- if the cap were removed (or
        // merely decorative, as `_max_repairs` was pre-fix), every one of these 5 would be
        // compiled and `attempts` would end up at 5, not the capped 2.
        let max_repairs = 2;
        let client = FakeClient::new(vec![
            submit("fn on_bar( {"),
            submit("also broken ("),
            submit("still broken ("),
            submit("broken again ("),
            submit("broken once more ("),
            FakeTurn::Text("gave up".into()),
        ]);
        let r = develop_strategy(
            "x",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &client,
            max_repairs,
            0.3,
        );
        assert!(!r.accepted);
        assert_eq!(
            r.attempts, max_repairs,
            "attempts must be bounded by max_repairs, not by the 5-submission script"
        );
        assert!(!r.problems.is_empty());
    }

    #[test]
    fn repair_path_still_accepts_on_the_last_allowed_attempt() {
        // max_repairs=2, broken-then-valid: the 2nd (valid) submission must still land inside the
        // cap and be accepted -- the cap must not be off-by-one against the happy/repair paths.
        let (_d, store) = seeded();
        let client = FakeClient::new(vec![
            submit("fn on_bar( {"),
            submit(CROSS),
            FakeTurn::Text("done".into()),
        ]);
        let r = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 0.3);
        assert!(r.accepted);
        assert_eq!(r.attempts, 2);
    }

    /// The Copilot pane's displayed Sharpe follows the SLICE's interval, not a hardcoded 252.
    ///
    /// This field is rendered as "OOS Sharpe" by `crates/vike-studio/src/chat.rs`'s `summary_of`,
    /// beside a Performance tab that annualizes off the picked slice. While this read 252 at every
    /// interval, the two panes printed numbers `sqrt(1440) ≈ 37.9x` apart for one strategy on one
    /// 1m slice, inside one window.
    ///
    /// ⚠ The `assert_ne!` is what makes this bite. The equality alone compares two expressions that
    /// would both move if the derivation regressed; only the inequality with the daily anchor fails
    /// when the literal comes back. The precondition guards vacuity — `metrics::sharpe` returns
    /// `0.0` at EVERY factor for a dispersion-free curve, which would satisfy the inequality for
    /// the wrong reason.
    #[test]
    fn the_displayed_sharpe_annualizes_off_the_slice_interval_not_a_bare_252() {
        let (_d, store) = seeded();
        let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
        let r = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 0.3);
        assert!(r.accepted, "the fixture must produce an accepted candidate to have a Sharpe");

        let at_daily = metrics::sharpe(&r.oos_equity_curve, PPY);
        assert!(
            at_daily != 0.0 && at_daily.is_finite(),
            "the fixture curve must have dispersion, else every factor yields 0.0: {at_daily}"
        );

        assert_eq!(
            r.oos_sharpe.to_bits(),
            metrics::sharpe(&r.oos_equity_curve, periods_per_year_for_interval("1m")).to_bits(),
            "the displayed Sharpe IS the OOS curve annualized at the 1m factor"
        );
        assert_ne!(
            r.oos_sharpe, at_daily,
            "...and NOT at the daily anchor — the regression this test exists to catch"
        );
    }

    #[test]
    fn develop_strategies_deflates_every_accepted_candidate() {
        let (_d, store) = seeded();
        // Three DIFFERENT sma-cross variants over the same seeded data -> different OOS Sharpes,
        // giving the trial set real variance (not a degenerate all-identical case).
        let variants = [(5u32, 20u32), (8, 25), (3, 15)];
        let mut turns = Vec::new();
        for (i, (fast, slow)) in variants.iter().enumerate() {
            turns.push(submit(&cross_script(*fast, *slow)));
            turns.push(FakeTurn::Text(format!("done{i}")));
        }
        let client = FakeClient::new(turns);

        let results =
            develop_strategies("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 3, 2, 0.3);
        assert_eq!(results.len(), 3);

        let accepted: Vec<&AgentResult> = results.iter().filter(|r| r.accepted).collect();
        assert!(
            accepted.len() >= 2,
            "test setup must produce >=2 accepted trials for deflation to run at all"
        );

        let trial_sharpes: Vec<f64> = accepted.iter().map(|r| r.oos_sharpe).collect();
        assert!(
            trial_sharpes.iter().any(|&s| (s - trial_sharpes[0]).abs() > 1e-9),
            "test setup must give the trial set real variance, not all-identical sharpes"
        );

        let best = accepted.iter().max_by(|a, b| a.oos_sharpe.total_cmp(&b.oos_sharpe)).unwrap();
        assert!(best.deflated_sharpe.is_finite());
        assert!((0.0..=1.0).contains(&best.deflated_sharpe));
        assert_ne!(
            best.deflated_sharpe, 0.0,
            "deflated_sharpe must actually be computed, not left at the Default"
        );

        // overfit.rs's guarantee: benchmarking PSR against the trials' expected-max-Sharpe can only
        // match or REDUCE significance vs. a plain zero-benchmark PSR (deflated_sharpe_below_psr_
        // when_many_trials in overfit.rs). Recompute that same zero-benchmark PSR here and check
        // the direction holds for the winner. Both must use the PER-PERIOD Sharpe
        // (risk_return_ratio), same units DSR is computed in — feeding the annualized oos_sharpe
        // here would make raw_psr saturate to ~1.0 and the direction check vacuous.
        let observed_pp = metrics::risk_return_ratio(&best.oos_equity_curve);
        let n_obs = metrics::returns(&best.oos_equity_curve).len().max(2);
        let skew = metrics::returns_skewness(&best.oos_equity_curve);
        let kurt = 3.0 + metrics::returns_kurtosis(&best.oos_equity_curve);
        let raw_psr = overfit::probabilistic_sharpe_ratio(observed_pp, n_obs, 0.0, skew, kurt);
        assert!(
            best.deflated_sharpe <= raw_psr + 1e-9,
            "deflated {} should not exceed the plain zero-benchmark PSR {raw_psr}",
            best.deflated_sharpe
        );
    }

    /// Build an equity curve (starting at 100.0) whose per-bar simple returns are exactly `rets`.
    fn curve_from_returns(rets: &[f64]) -> Vec<f64> {
        let mut eq = vec![100.0];
        for &r in rets {
            let last = *eq.last().unwrap();
            eq.push(last * (1.0 + r));
        }
        eq
    }

    fn accepted_with_curve(curve: Vec<f64>) -> AgentResult {
        AgentResult {
            accepted: true,
            // oos_sharpe is the ANNUALIZED value the field displays; deflate_batch must NOT use it.
            oos_sharpe: metrics::sharpe(&curve, PPY),
            oos_equity_curve: curve,
            ..Default::default()
        }
    }

    /// MAGNITUDE pin (unit test on `deflate_batch` directly, overfit.rs's own test style): a fixed,
    /// hand-reasonable equity curve whose deflated Sharpe is pinned to a known per-period value.
    /// This is the regression guard the reviewer asked for — it FAILS if DSR is computed from the
    /// annualized Sharpe (which saturates to ~1.0) instead of the per-period one.
    #[test]
    fn deflate_batch_pins_per_period_dsr_magnitude() {
        // Two deterministic candidates with different, modest per-period Sharpes (positive drift +
        // deterministic zig-zag noise). 48 returns each -> n_obs=48, enough for skew/kurt.
        let rets_a: Vec<f64> = (0..48).map(|i| 0.002 + 0.01 * ((i % 4) as f64 - 1.5)).collect();
        let rets_b: Vec<f64> = (0..48).map(|i| 0.001 + 0.008 * ((i % 3) as f64 - 1.0)).collect();
        let curve_a = curve_from_returns(&rets_a);
        let curve_b = curve_from_returns(&rets_b);

        // The correct per-period inputs the fixed implementation must feed overfit:: with.
        let pp_a = metrics::risk_return_ratio(&curve_a);
        let pp_b = metrics::risk_return_ratio(&curve_b);
        let n_obs = metrics::returns(&curve_a).len().max(2); // = 48
        let skew = metrics::returns_skewness(&curve_a);
        let kurt = 3.0 + metrics::returns_kurtosis(&curve_a);
        let expected = overfit::deflated_sharpe_ratio(pp_a, &[pp_a, pp_b], n_obs, skew, kurt);
        // What the OLD (buggy) wiring computed: annualized sr fed where per-period is required.
        let ann_a = metrics::sharpe(&curve_a, PPY);
        let ann_b = metrics::sharpe(&curve_b, PPY);
        let buggy = overfit::deflated_sharpe_ratio(ann_a, &[ann_a, ann_b], n_obs, skew, kurt);

        let mut results = vec![accepted_with_curve(curve_a), accepted_with_curve(curve_b)];
        deflate_batch(&mut results);

        // (1) The implementation must match the per-period computation exactly.
        assert!(
            (results[0].deflated_sharpe - expected).abs() < 1e-12,
            "deflated {} must equal the per-period DSR {expected}",
            results[0].deflated_sharpe
        );
        // (2) Pinned magnitude: this curve's per-period DSR sits well inside the unit interval and
        //     is NOT saturated. Value pinned from the fixed wiring; the buggy annualized wiring
        //     saturates to ~1.0, so this bound is what makes the test a real regression catcher.
        assert!(
            (results[0].deflated_sharpe - 0.874_192).abs() < 1e-5,
            "pinned per-period DSR magnitude drifted: {}",
            results[0].deflated_sharpe
        );
        // (3) The buggy annualized wiring would have saturated near 1.0 -> materially different, so
        //     assertions (1)/(2) genuinely distinguish the two unit conventions.
        assert!(
            buggy > 0.999 && (buggy - results[0].deflated_sharpe).abs() > 0.1,
            "annualized wiring should saturate (~1.0); got buggy={buggy}, actual={}",
            results[0].deflated_sharpe
        );
    }

    #[test]
    fn develop_strategies_leaves_deflated_sharpe_zero_below_two_accepted() {
        let (_d, store) = seeded();
        // Only 1 candidate ever accepts (the other gives up) -> deflation is undefined, must stay
        // at the Default 0.0 rather than compute something from a single-element trial set.
        let client = FakeClient::new(vec![
            submit(CROSS),
            FakeTurn::Text("done0".into()),
            submit("fn on_bar( {"),
            submit("still broken ("),
            FakeTurn::Text("gave up".into()),
        ]);
        let results =
            develop_strategies("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 2, 0.3);
        assert_eq!(results.iter().filter(|r| r.accepted).count(), 1);
        for r in &results {
            assert_eq!(r.deflated_sharpe, 0.0);
        }
    }

    /// **The acceptance test for the whole trial-ledger item.** One accepted candidate — the
    /// production shape, which before this could never reach the ≥2 trials deflation needs — plus
    /// 4 prior accepted trials recorded on the same slice, must produce a real `deflated_sharpe`.
    /// The bare (ledger-less) control run over the same data still reports `0.0`, which is exactly
    /// the bug: the single most important overfitting control was inert on the production path.
    #[test]
    fn deflation_uses_prior_trials() {
        let (d, store) = seeded();
        let paths = LedgerPaths::under(d.path());
        // Four prior trials with REAL spread — a zero-variance trial set yields sr_star == 0 and
        // would make this test pass without the prior half being consulted at all.
        let prior = TrialLedger {
            trials: vec![
                prior_accepted(0, 0.02, 0.31),
                prior_accepted(1, 0.05, 0.79),
                prior_accepted(2, 0.03, 0.47),
                prior_accepted(3, 0.09, 1.42),
            ],
        };
        ledger::save_trials(&prior, &paths.trials);

        let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
        let r = develop_strategy_with_ledger(
            "make an sma cross",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &client,
            2,
            0.3,
            Some(&paths),
        );
        assert!(r.accepted);
        assert!(r.deflated_sharpe.is_finite());
        assert!((0.0..=1.0).contains(&r.deflated_sharpe));
        assert_ne!(
            r.deflated_sharpe, 0.0,
            "a single accepted candidate + prior trials must deflate — this is the whole item"
        );

        // Exact: the trial set is {this candidate} ∪ {the 4 prior}, in that order.
        let m = overfit::sharpe_moments(&r.oos_equity_curve);
        let mut set = vec![m.sr_per_obs];
        set.extend(prior.trials.iter().map(|t| t.sr_per_obs));
        let expected = overfit::deflated_sharpe_ratio(m.sr_per_obs, &set, m.n_obs, m.skew, m.kurt);
        assert_eq!(r.deflated_sharpe, expected, "prior trials must join the trial set verbatim");

        // The run is appended (with its deflated value) — the NEXT session's prior set.
        let after = ledger::load_trials(&paths.trials);
        assert_eq!(after.trials.len(), 5, "the run's own trial is recorded");
        let last = after.trials.last().unwrap();
        assert!(last.accepted);
        assert_eq!(last.deflated_sharpe, expected);
        assert_eq!(last.sr_per_obs, m.sr_per_obs, "the deflation scalar survives curve pruning");
        assert_eq!(after.prior_sharpes_for("binance", "BTCUSDT", "1m").len(), 5);

        // Control — the pre-ledger path over the SAME data reports nothing.
        let bare_client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
        let bare = develop_strategy(
            "make an sma cross",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &bare_client,
            2,
            0.3,
        );
        assert!(bare.accepted);
        assert_eq!(bare.deflated_sharpe, 0.0, "ledger-less is byte-identical to pre-ledger");
    }

    /// A prior trial on a DIFFERENT slice must not widen this slice's multiple-testing correction
    /// — the trial set is per-`(venue, symbol, interval)`, not global.
    #[test]
    fn deflation_ignores_other_slices() {
        let (d, store) = seeded();
        let paths = LedgerPaths::under(d.path());
        let mut t = prior_accepted(0, 0.05, 0.79);
        t.symbol = "ETHUSDT".into();
        ledger::save_trials(&TrialLedger { trials: vec![t] }, &paths.trials);

        let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
        let r = develop_strategy_with_ledger(
            "x",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &client,
            2,
            0.3,
            Some(&paths),
        );
        assert!(r.accepted);
        assert_eq!(r.deflated_sharpe, 0.0, "1 candidate + 0 same-slice priors -> still undefined");
    }

    /// Payoff (a): the grounding block naming a prior trial's Sharpe, its explanation, and the
    /// matching learnings reaches the model — and the user's own request is preserved beneath it.
    #[test]
    fn prompt_includes_prior_trials() {
        let (d, store) = seeded();
        let paths = LedgerPaths::under(d.path());
        let mut t = prior_accepted(0, 0.05, 1.23);
        t.explanation = "ema pullback".into();
        ledger::save_trials(&TrialLedger { trials: vec![t] }, &paths.trials);
        ledger::append_learning(
            &paths.learnings,
            Learning {
                ts_ms: 1,
                scope: LearningScope::Global,
                text: "fees dominate below 20 trades".into(),
            },
        );

        let client = CapturingClient::default();
        let _ = develop_strategy_with_ledger(
            "make an sma cross",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &client,
            2,
            0.3,
            Some(&paths),
        );
        let prompt = client.prompt();
        assert!(prompt.contains("1.23"), "the prior trial's OOS Sharpe must be named: {prompt}");
        assert!(prompt.contains("ema pullback"), "…and what it was: {prompt}");
        assert!(prompt.contains("fees dominate below 20 trades"), "the learning: {prompt}");
        assert!(prompt.contains("make an sma cross"), "the user's request survives: {prompt}");

        // Control — no ledger means the prompt is the caller's, verbatim.
        let bare = CapturingClient::default();
        let _ = develop_strategy(
            "make an sma cross",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &bare,
            2,
            0.3,
        );
        assert_eq!(bare.prompt(), "make an sma cross");
    }

    /// A fresh ledger has nothing to say, so the prompt stays byte-identical to the ledger-less
    /// one — grounding must not inject an empty header on a slice with no history.
    #[test]
    fn empty_ledger_leaves_the_prompt_untouched() {
        let (d, store) = seeded();
        let paths = LedgerPaths::under(d.path());
        let client = CapturingClient::default();
        let _ = develop_strategy_with_ledger(
            "hello",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &client,
            2,
            0.3,
            Some(&paths),
        );
        assert_eq!(client.prompt(), "hello");
    }

    /// The `record_learning` tool: advertised only WITH a ledger, writes a slice-scoped note, and
    /// does not consume a repair attempt.
    #[test]
    fn record_learning_tool_is_gated_on_the_ledger_and_writes_the_note() {
        let (d, store) = seeded();
        let paths = LedgerPaths::under(d.path());
        let client = CapturingClient::scripted(vec![
            ToolCall {
                id: "l".into(),
                name: "record_learning".into(),
                input: json!({"scope":"slice","text":"rsi mean-reversion overtrades here"}),
            },
            submit_call(CROSS),
        ]);
        let r = develop_strategy_with_ledger(
            "x",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &client,
            2,
            0.3,
            Some(&paths),
        );
        assert!(client.tools.borrow().iter().any(|t| t == "record_learning"));
        assert!(r.accepted, "the submission after the note still runs");
        assert_eq!(r.attempts, 1, "a learning call must not burn a repair attempt");

        let notes = ledger::load_learnings(&paths.learnings);
        assert_eq!(notes.learnings.len(), 1);
        assert_eq!(notes.learnings[0].text, "rsi mean-reversion overtrades here");
        assert_eq!(
            notes.learnings[0].scope,
            LearningScope::Slice {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
            }
        );

        // Ledger-less: the tool is not offered at all.
        let bare = CapturingClient::default();
        let _ = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &bare, 2, 0.3);
        assert_eq!(*bare.tools.borrow(), vec!["submit_strategy".to_string()]);
    }

    /// An unwritable ledger directory must cost the note/trial, never the run — the best-effort
    /// contract the whole module is built on.
    #[test]
    fn an_unwritable_ledger_never_fails_the_loop() {
        let (d, store) = seeded();
        // A path whose PARENT does not exist: every read misses and every write errors.
        let missing = d.path().join("no").join("such").join("dir");
        let paths = LedgerPaths::under(&missing);
        let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
        let r = develop_strategy_with_ledger(
            "x",
            "binance",
            "BTCUSDT",
            "1m",
            store.as_ref(),
            &client,
            2,
            0.3,
            Some(&paths),
        );
        assert!(r.accepted, "the authoring loop must survive an unavailable ledger");
        assert!(!paths.trials.exists());
    }
}
