//! The AI Copilot's candidates are backtested ON THE SERVER: `crate::backend::remote`'s
//! `copilot_backtester`, the backtester `StudioState::start_chat_send` hands `vike-ai`'s loop, sends
//! each candidate as the hub `RunSlice` a Run sends, so what the chat reports is a Run of the same
//! script over the same held-out bars (the owner's rulings of 2026-11-09). On the Named backend the
//! copilot refuses instead of computing in this process.

use super::super::*;
use super::support::{seeded_store, spawn_backtesthub_server};
use crate::backend::remote::{copilot_backtester, spawn_run_remote};
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
/// trade count ARE a Run's of the same bars through the same hub, and that Run paid the binance
/// table. The numbers print under `--nocapture`.
#[test]
fn a_copilot_candidate_is_a_server_run_of_the_same_held_out_bars() {
    let (_dir, store) = seeded_store();
    let addr = spawn_backtesthub_server(store.clone());
    let script = crate::panes::editor::EditorPane::default().source;
    let r = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &SubmitsOnce(script.clone()),
        &copilot_backtester(addr.clone(), None, None),
        2,
        0.3,
        None,
    );
    assert!(r.accepted, "{:?}", r.problems);

    let out_of_sample = TimestampRange::of(60_000 * 281, 60_000 * 400);
    let slice = DataSlice::bars("binance", "BTCUSDT", "1m", out_of_sample);
    let run = spawn_run_remote(addr, None, StrategySpec::rhai(&script), slice, None)
        .recv()
        .expect("the hub answers")
        .expect("a Run");
    assert_eq!(r.oos_equity_curve, run.equity_curve, "the candidate's curve IS the Run's");
    assert_eq!(r.n_trades, run.n_trades);
    let fees: f64 = run.trades.iter().map(|t| t.fees).sum();
    assert!(fees > 0.0, "the Run paid the venue's fee table: {fees}");
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
    let keys = ChatApiKeys { anthropic: Some("not-a-real-key".to_string()), cerebras: None };
    let mut st = StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        keys,
        None,
        QaAutorun::Off,
        StudioWorkspace::default(),
    );
    st.picker.select(0);
    st.chat.input = "RSI mean reversion".to_string();
    assert_eq!(st.send_disabled_reason(), None, "the control: the Remote backend may send");

    st.backend = Backend::Named { addr: "127.0.0.1:9".to_string() };
    assert_eq!(st.send_disabled_reason(), Some(COPILOT_NEEDS_COMPUTE_KEY));
    st.start_chat_send();
    assert!(st.chat_rx.is_none() && !st.chat.running, "nothing was dispatched");
    assert_eq!(st.chat.input, "RSI mean reversion", "the prompt was not consumed");
}
