//! The AI Copilot's candidates are backtested ON THE SERVER: `crate::backend::backtesthub`'s
//! `copilot_backtester`, the backtester `StudioState::start_chat_send` hands `vike-ai`'s loop, sends
//! each candidate as the summarized hub `RunSlice` a Run sends and reads a kept candidate's whole
//! out-of-sample curve back in pages (decision 0078's amendment of 2026-11-18), so what the chat reports
//! is a Run of the same script over the same held-out bars (the owner's rulings of 2026-11-09). On the
//! Named backend the copilot refuses instead of computing in this process.

use super::super::*;
use super::support::{seeded_store, spawn_backtesthub_server};
use crate::backend::backtesthub::{copilot_backtester, spawn_backtesthub_run};
use vike_ai::{LlmClient, LlmError, ToolCall, ToolSpec, develop_strategy_with_ledger};
use vike_studio_core::DataSlice;

/// A model that submits one script and stops.
struct SubmitsOnce(String);

impl LlmClient for SubmitsOnce {
    fn run(
        &self,
        _system: &str,
        _user: &str,
        _tools: &[ToolSpec],
        dispatch: &mut dyn FnMut(ToolCall) -> String,
    ) -> Result<String, LlmError> {
        let input = serde_json::json!({"code": self.0, "explanation": "e"});
        dispatch(ToolCall { id: "t".into(), name: "submit_strategy".into(), input });
        Ok(String::new())
    }
}

/// The editor's default SMA cross on the seeded slice (binance BTCUSDT 1m, 400 bars, the chat's
/// 0.3 holdout, so the last 120 bars), through a loopback backtesthub: the candidate's curve and
/// trade count ARE a Run's of the same bars through the same hub, and that Run, whose script states no
/// commission, paid none: no venue table (the owner's ruling of 2026-11-18). The numbers print under
/// `--nocapture`.
#[test]
fn a_copilot_candidate_is_a_server_run_of_the_same_held_out_bars() {
    let (_dir, store) = seeded_store();
    let address = spawn_backtesthub_server(store.clone());
    let script = crate::panes::editor::StudioEditorPane::default().source;
    let r = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &SubmitsOnce(script.clone()),
        &copilot_backtester(address.clone(), None),
        2,
        0.3,
        None,
    );
    assert!(r.accepted, "{:?}", r.problems);

    let out_of_sample = TimestampRange::of(60_000 * 281, 60_000 * 400);
    let slice = DataSlice::bars("binance", "BTCUSDT", "1m", out_of_sample);
    let run = spawn_backtesthub_run(
        address.clone(),
        None,
        StrategySpec::rhai(&script),
        slice,
        crate::backend::backtesthub::StudioRunEngine::default(),
        false,
    )
    .recv()
    .expect("the hub answers")
    .expect("a Run")
    .summary;
    // 120 bars: under the thinning's two thousand, so the Run's summary curve is every point, and the
    // candidate's curve, read back whole by `ReadCurve`, is every one of them.
    let curve: Vec<f64> = run.curve.iter().map(|&(_, v)| v).collect();
    assert_eq!(r.oos_equity_curve, curve, "the candidate's curve IS the Run's");
    assert_eq!(r.oos_equity_curve.len(), run.curve_len, "the whole curve, read on request");
    assert_eq!(r.n_trades, run.n_trades);
    let trades = vike_backtesthub_client::BacktesthubClient::connect(address.as_str())
        .expect("connect")
        .read_trades(&run.run, None, 0, 200)
        .expect("the Run's trades");
    let fees: f64 = trades.trades.iter().map(|t| t.fees).sum();
    assert_eq!(fees, 0.0, "no commission is named or stated, and no venue table applies: {fees}");
    eprintln!(
        "copilot candidate on the server: final {:.6}, trades {}, fees on closed trades {fees:.6}, \
         OOS Sharpe {:.6}",
        run.final_equity, r.n_trades, r.oos_sharpe
    );
}

/// On the observe-only Named backend the copilot has no compute key to sign a candidate's run with,
/// so Send is refused in words and nothing is dispatched: there is no local fallback.
#[test]
fn on_the_named_backend_the_copilot_refuses_instead_of_computing_here() {
    let (dir, store) = seeded_store();
    let keys = StudioChatApiKeys { anthropic: Some("not-a-real-key".to_string()), cerebras: None };
    let mut st = StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        keys,
        None,
        StudioAutorun::Off,
        StudioWorkspace::default(),
    );
    st.picker.select(0);
    st.chat.input = "RSI mean reversion".to_string();
    assert_eq!(st.send_disabled_reason(), None, "the control: the Source backend may send");

    st.backend = StudioRunBackend::Named { address: "127.0.0.1:9".to_string() };
    assert_eq!(st.send_disabled_reason(), Some(COPILOT_NEEDS_COMPUTE_KEY));
    st.start_chat_send();
    assert!(st.chat_rx.is_none() && !st.chat.running, "nothing was dispatched");
    assert_eq!(st.chat.input, "RSI mean reversion", "the prompt was not consumed");
}
