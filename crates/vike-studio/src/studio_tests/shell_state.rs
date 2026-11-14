//! A fresh `StudioState`, the Rhai verdict `poll` holds, the "why is this disabled" sentences Run,
//! Sweep, Walk-forward and Send give, and the backend switch.

use super::super::*;
use super::support::{seeded_store, state_new};
use std::assert_matches;
use std::sync::Arc;
use vike_store::HistoryStoreDataFusion;

#[test]
fn editor_and_tools_panes_start_expanded() {
    let (_dir, store) = seeded_store();
    let st = state_new(&_dir, store);
    assert!(!st.editor_collapsed);
    assert!(!st.tools_collapsed);
}

/// `new()` seeds the compile cache with the default (known-good) script, and the "unsaved"
/// baseline starts equal to the source — no red/amber dot on first paint.
#[test]
fn new_state_starts_with_a_clean_compile_and_no_unsaved_marker() {
    let (_dir, store) = seeded_store();
    let st = state_new(&_dir, store);
    assert_eq!(st.rhai_verdict(), Some(&Ok(())));
    assert_eq!(st.editor.source, st.saved_source);
}

/// `poll()` recompiles only when the source drifted, and reports the compile error for broken
/// source.
#[test]
fn poll_updates_the_rhai_verdict_when_source_changes() {
    let (_dir, store) = seeded_store();
    let mut st = state_new(&_dir, store);
    st.editor.source = "fn on_bar( {".to_string();
    st.poll();
    assert_matches!(st.rhai_verdict(), Some(Err(_)));

    st.editor.source = crate::panes::editor::EditorPane::default().source;
    st.poll();
    assert_eq!(st.rhai_verdict(), Some(&Ok(())));
}

/// **The Rhai check never runs on a Strategy file's Rust buffer, and no verdict survives a mode
/// switch in either direction.** The state-machine half of
/// `crates/vike-studio/tests/headless/studio_shell_render/editor_verdict.rs`'s
/// `a_strategy_file_buffer_is_not_judged_as_rhai_and_the_same_buffer_in_rhai_mode_is`, which asserts
/// what the header RENDERS; this one asserts what `poll` HOLDS, including across two polls in
/// a row (the per-frame case).
///
/// The Rhai leg comes FIRST and holds a real `Err` for this exact source, so the Strategy file leg's
/// `None` is a verdict being DROPPED rather than one that was never computed — and the last
/// leg flips back with the source unchanged, so only a re-check (not a drifted-source recompile)
/// can put a verdict back.
#[test]
fn strategy_file_mode_holds_no_rhai_verdict_and_rhai_mode_rechecks_on_return() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.editor.source = "use vike_model::Strategy;\npub fn build() {}\n".to_string();
    st.poll();
    assert_matches!(
        st.rhai_verdict(),
        Some(Err(_)),
        "the control: Rust source IS a Rhai error while the buffer is Rhai"
    );

    st.strategy_source = StrategySource::Rust;
    assert!(!st.buffer_is_rhai());
    st.poll();
    assert_eq!(st.rhai_verdict(), None, "Strategy file mode must hold no Rhai verdict");
    st.poll();
    assert!(st.rhai_check.is_none(), "...on every frame, not only the first after the switch");

    st.strategy_source = StrategySource::Rhai;
    st.poll();
    match &st.rhai_check {
        Some((checked, Err(_))) => assert_eq!(checked, &st.editor.source),
        other => panic!("back in Rhai mode the buffer must be re-checked: {other:?}"),
    }

    // Native keeps the check: its buffer is the parked Rhai script (`buffer_is_rhai`'s doc).
    st.strategy_source = StrategySource::Native;
    assert!(st.buffer_is_rhai());
    st.poll();
    assert_matches!(st.rhai_verdict(), Some(Err(_)));
}

/// Run says why it is disabled — one blocker at a time, the first a person can act on — and its
/// enable condition is unchanged: `None` exactly when the old `add_enabled` bool was true.
#[test]
fn run_says_why_it_is_disabled() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.picker.select(0);
    assert_eq!(st.run_disabled_reason(), None, "a picked slice over a Rhai buffer runs");
    st.running = true;
    assert_eq!(st.run_disabled_reason(), Some(RUN_BUSY));
    st.running = false;
    st.strategy_source = StrategySource::Rust;
    assert!(st.run_disabled_reason().is_some(), "an unbuilt strategy file cannot run");
    assert_eq!(st.run_disabled_reason(), st.run_blocked_reason());
}

#[test]
fn run_asks_for_a_slice_over_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let st = state_new(&dir, Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap()));
    assert_eq!(st.run_disabled_reason(), Some(PICK_A_SLICE));
}

#[test]
fn sweep_and_walk_forward_say_why_they_are_disabled() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.picker.select(0);
    assert_eq!(st.sweep_disabled_reason(true), None);
    assert_eq!(st.sweep_disabled_reason(false), Some(SEED_A_GRID));
    assert_eq!(st.walk_forward_disabled_reason(), None);
    let (_sweep_tx, sweep_rx) = std::sync::mpsc::channel();
    st.sweep_rx = Some(sweep_rx);
    assert_eq!(st.sweep_disabled_reason(true), Some(SWEEP_BUSY));
    let (_walkforward_tx, walkforward_rx) = std::sync::mpsc::channel();
    st.walkforward_rx = Some(walkforward_rx);
    assert_eq!(st.walk_forward_disabled_reason(), Some(WALK_FORWARD_BUSY));
}

/// The keyless copy names the verb that writes the key and no longer a `.env` file: the credential
/// FILE plane was removed on 2026-10-07, so a user who followed the old sentence edited a file
/// nothing reads. Both provider names stay in it, and the value is told to travel on stdin.
#[test]
fn the_keyless_copy_points_at_secrets_set_not_an_env_file() {
    let copy = NO_PROVIDER_KEY;
    assert!(copy.contains("vike-cli secrets set ANTHROPIC_API_KEY"), "{copy}");
    assert!(copy.contains("(or CEREBRAS_API_KEY;"), "{copy}");
    assert!(copy.contains("stdin"), "{copy}");
    assert!(!copy.contains(".env"), "{copy}");
}

#[test]
fn send_says_why_it_is_disabled() {
    let (dir, store) = seeded_store();
    let keyless = state_new(&dir, store.clone());
    assert_eq!(keyless.send_disabled_reason(), Some(NO_PROVIDER_KEY));
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
    assert_eq!(st.send_disabled_reason(), Some(NOTHING_TO_SEND));
    st.chat.input = "RSI mean reversion".to_string();
    assert_eq!(st.send_disabled_reason(), None);
}

/// Switching backends keeps the address the operator typed — both dial the same backtesthub —
/// and a switch onto Named drops the roster, which belongs to the daemon that answered it. The
/// segmented control's one action; the two buttons it replaces each spelled half of this.
#[test]
fn switching_backends_keeps_the_address_and_drops_a_stale_roster() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.backend = Backend::Remote { addr: "127.0.0.2:7999".to_string() };
    st.switch_backend(BackendKind::Named);
    assert_eq!(st.backend, Backend::Named { addr: "127.0.0.2:7999".to_string() });
    st.named_roster = Some(Err("stale".to_string()));
    st.switch_backend(BackendKind::Remote);
    assert_eq!(st.backend, Backend::Remote { addr: "127.0.0.2:7999".to_string() });
    st.switch_backend(BackendKind::Named);
    assert!(st.named_roster.is_none(), "a roster belongs to the daemon that answered it");
}

/// The slippage switch only says what to SEND: off sends nothing (the run before the switch),
/// percent the bare value (the wire field's own meaning, which any daemon serves), and ticks and
/// market impact name their model, the value under it and the tick. Nothing is computed here.
#[test]
fn the_slippage_switch_sends_the_model_and_its_value_untouched() {
    use vike_backtesthub_client::wire_studio::StrategyEngineConfigWire;
    let switch = |model, value, tick| SlippageSwitch { model, value, tick }.wire();
    assert_eq!(SlippageSwitch::default().wire(), None, "off by default, and off sends nothing");
    assert_eq!(switch(SlippageModel::Off, 0.5, 0.5), None);
    let want = |slippage, model: Option<&str>, tick| StrategyEngineConfigWire {
        slippage,
        slippage_model: model.map(str::to_string),
        slippage_tick: tick,
        ..StrategyEngineConfigWire::default()
    };
    assert_eq!(switch(SlippageModel::Percent, 0.0005, 0.0), Some(want(Some(0.0005), None, None)));
    assert_eq!(
        switch(SlippageModel::Ticks, 2.0, 0.01),
        Some(want(Some(2.0), Some("ticks"), Some(0.01)))
    );
    assert_eq!(
        switch(SlippageModel::MarketImpact, 0.0, 0.01),
        Some(want(None, Some("market_impact"), None)),
        "no constant part, and the tick is not the model's"
    );
    assert_eq!(
        switch(SlippageModel::MarketImpact, 0.0002, 0.0),
        Some(want(Some(0.0002), Some("market_impact"), None))
    );
}

/// The Named verb carries no cost term, so a Run there with the switch on is refused in words
/// rather than run without the slippage the user set.
#[test]
fn a_named_run_refuses_a_slippage_it_would_drop() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.picker.select(0);
    st.backend = Backend::Named { addr: "127.0.0.1:9".to_string() };
    st.slippage = SlippageSwitch { model: SlippageModel::Percent, value: 0.001, tick: 0.0 };
    st.start_run();
    let Some(Err(RunError::Data(why))) = &st.last else {
        panic!("refused, not dispatched: {:?}", st.last.as_ref().map(|r| r.is_ok()));
    };
    assert_eq!(why, crate::backend::remote::NAMED_RUN_SLIPPAGE);
    assert!(!st.running, "nothing was dispatched");
}

/// The slippage switch is remembered across a restart: the workspace snapshot carries it, the file
/// round-trips it, and a Studio built from that file sends the same slippage and finds nothing
/// drifted on its first frame.
#[test]
fn the_slippage_switch_is_remembered_across_a_restart() {
    let (dir, store) = seeded_store();
    let mut before = state_new(&dir, store.clone());
    before.slippage = SlippageSwitch { model: SlippageModel::Ticks, value: 2.0, tick: 0.5 };
    let ws = before.workspace_snapshot();
    assert_eq!(ws.slippage, before.slippage);
    let path = dir.path().join("restart_workspace.json");
    save_workspace(&path, &ws).unwrap();
    let after = StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        ChatApiKeys::default(),
        None,
        QaAutorun::Off,
        load_workspace(&path).expect("the file reads"),
    );
    assert_eq!(after.slippage, before.slippage);
    assert_eq!(after.slippage.wire(), before.slippage.wire(), "it sends what it was left on");
    assert_eq!(after.workspace_snapshot(), ws, "no spurious write on the first frame");
}
