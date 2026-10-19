//! Shared fixtures for the `agent` tests: the seeded slice, scripted clients and trial builders.

use std::cell::RefCell;
use std::sync::Arc;

use serde_json::json;
use vike_analytics::metrics;
use vike_data::{HistStore, MemHistStore};
use vike_marketdata::Bar;

use crate::agent::{AgentResult, develop_strategy_with_ledger};
use crate::client::{FakeTurn, LlmClient, LlmError, ToolCall, ToolSpec};
use crate::ledger::LedgerPaths;
use crate::ledger::trial::Trial;

/// The daily anchor, kept HERE rather than on the module because the real path no longer has a
/// fixed annualization — `run_authoring_loop` derives it from the slice's interval.
///
/// These tests keep 252 deliberately: every one of them checks a SCALE-INVARIANT property (that
/// the displayed annualized Sharpe never reaches `overfit::`, and that the deflation runs on
/// per-observation values), and a scale-invariant claim is clearest when the scale is a
/// constant the reader can see.
pub(super) const PPY: f64 = 252.0;

pub(super) const CROSS: &str = r#"
const QTY = 1.0;
fn on_bar() { let f=sma(5); let s=sma(20); if s.is_nan(){return;} let tgt=if f>s{QTY}else{-QTY}; let d=tgt-position(); if abs(d)>1e-12 { market(if d>0.0{1}else{-1}, abs(d)); } }
"#;

/// An sma-cross script with the given (fast, slow) windows — used to give
/// `develop_strategies_deflates_every_accepted_candidate` a trial set with real variance
/// (different windows -> different OOS Sharpes on the same seeded data), unlike 3 identical
/// `CROSS` submissions which would give the trial set zero variance.
pub(super) fn cross_script(fast: u32, slow: u32) -> String {
    format!(
        "const QTY = 1.0;\nfn on_bar() {{ let f=sma({fast}); let s=sma({slow}); if s.is_nan(){{return;}} let tgt=if f>s{{QTY}}else{{-QTY}}; let d=tgt-position(); if abs(d)>1e-12 {{ market(if d>0.0{{1}}else{{-1}}, abs(d)); }} }}"
    )
}

/// A bar-seeded `MemHistStore` (real in-memory bar storage behind `test-support` — no
/// DataFusion in this crate's dev graph) plus the TempDir the ledger tests park their files
/// under: the store no longer owns a directory, so the dir is the tests' own.
pub(super) fn seeded() -> (tempfile::TempDir, Arc<MemHistStore>) {
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

pub(super) fn submit_call(code: &str) -> ToolCall {
    ToolCall {
        id: "t".into(),
        name: "submit_strategy".into(),
        input: json!({"code":code,"explanation":"e"}),
    }
}

pub(super) fn submit(code: &str) -> FakeTurn {
    FakeTurn::Tool(submit_call(code))
}

/// A `FakeClient` that also RECORDS what the loop handed it: the user prompt (for the
/// grounding tests) and the advertised tool names (for the `record_learning` gating test).
/// `FakeClient` deliberately ignores both, so those two claims need their own double.
#[derive(Default)]
pub(super) struct CapturingClient {
    /// The user prompt of every `run` call, in order.
    prompts: RefCell<Vec<String>>,
    /// The names of every tool advertised on every `run` call.
    pub(super) tools: RefCell<Vec<String>>,
    /// Tool calls to emit once, on the first `run`.
    script: RefCell<Vec<ToolCall>>,
}

impl CapturingClient {
    pub(super) fn scripted(calls: Vec<ToolCall>) -> Self {
        Self { script: RefCell::new(calls), ..Default::default() }
    }
    pub(super) fn prompt(&self) -> String {
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
pub(super) fn prior_accepted(i: usize, sr_per_obs: f64, oos_sharpe: f64) -> Trial {
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

/// Build an equity curve (starting at 100.0) whose per-bar simple returns are exactly `rets`.
pub(super) fn curve_from_returns(rets: &[f64]) -> Vec<f64> {
    let mut eq = vec![100.0];
    for &r in rets {
        let last = *eq.last().unwrap();
        eq.push(last * (1.0 + r));
    }
    eq
}

pub(super) fn accepted_with_curve(curve: Vec<f64>) -> AgentResult {
    AgentResult {
        accepted: true,
        // oos_sharpe is the ANNUALIZED value the field displays; deflate_batch must NOT use it.
        oos_sharpe: metrics::sharpe(&curve, PPY),
        oos_equity_curve: curve,
        ..Default::default()
    }
}

/// `develop_strategy_with_ledger` on the seeded slice (`binance` / `BTCUSDT` / `1m`, 2 repairs, a
/// 0.3 holdout): the one call every ledger test makes, which differs between them only in the prompt.
pub(super) fn run_with_ledger(
    prompt: &str,
    store: &dyn HistStore,
    client: &dyn LlmClient,
    paths: &LedgerPaths,
) -> AgentResult {
    develop_strategy_with_ledger(
        prompt,
        "binance",
        "BTCUSDT",
        "1m",
        store,
        client,
        2,
        0.3,
        Some(paths),
    )
}
