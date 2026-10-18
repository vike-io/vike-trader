//! The generate -> compile-validate -> OOS-backtest -> repair loop and its `record_learning` dispatch.

use std::cell::RefCell;

use vike_analytics::metrics;
use vike_analytics::report::periods_per_year_for_interval;
use vike_data::{HistStore, TsRange};
use vike_script::RhaiStrategy;
use vike_sim::{EngineParams, SimBroker, StrategyEngine};

use super::AgentResult;
use super::prompt::{STRATEGY_SYSTEM_PROMPT, record_learning_tool, submit_tool};
use crate::client::{LlmClient, ToolCall};
use crate::ledger::LedgerPaths;
use crate::ledger::learning::{Learning, LearningScope};
use crate::ledger::store::{append_learning, load_learnings, load_trials};
use crate::ledger::summary::{PROMPT_TOP_K, prior_work_summary};

/// The generate -> compile-validate -> OOS-backtest -> repair loop itself, WITHOUT deflation or
/// trial recording — the shared body of [`develop_strategy_with_ledger`] and
/// [`develop_strategies_with_ledger`], which own those two steps at their own (single vs batch)
/// cadence. `ledger` here is used only for the two IN-loop concerns: prompt grounding and the
/// `record_learning` tool.
pub(super) fn run_authoring_loop(
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
    append_learning(&paths.learnings, Learning { ts_ms: vike_model::now_ms(), scope, text });
    let where_ = if global { "globally" } else { "for this slice" };
    format!("Recorded {where_}. Now submit a strategy with `submit_strategy`.")
}
