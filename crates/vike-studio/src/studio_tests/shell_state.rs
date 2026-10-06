//! A fresh `StudioState`, the Rhai verdict `poll` holds, the "why is this disabled" sentences Run,
//! Sweep, Walk-forward and Send give, and the backend switch.

use super::super::*;
use super::support::{seeded_store, state_new};
use std::sync::Arc;
use vike_data::DataFusionHist;

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
    assert!(matches!(st.rhai_verdict(), Some(Err(_))));

    st.editor.source = crate::panes::editor::EditorPane::default().source;
    st.poll();
    assert_eq!(st.rhai_verdict(), Some(&Ok(())));
}

/// **The Rhai check never runs on a Plugin's Rust buffer, and no verdict survives a mode
/// switch in either direction.** The state-machine half of
/// `crates/vike-studio/tests/studio_shell_render/editor_verdict.rs`'s
/// `a_plugin_buffer_is_not_judged_as_rhai_and_the_same_buffer_in_rhai_mode_is`, which asserts
/// what the header RENDERS; this one asserts what `poll` HOLDS, including across two polls in
/// a row (the per-frame case).
///
/// The Rhai leg comes FIRST and holds a real `Err` for this exact source, so the Plugin leg's
/// `None` is a verdict being DROPPED rather than one that was never computed — and the last
/// leg flips back with the source unchanged, so only a re-check (not a drifted-source recompile)
/// can put a verdict back.
#[test]
fn plugin_mode_holds_no_rhai_verdict_and_rhai_mode_rechecks_on_return() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.editor.source = "use vike_model::Strategy;\npub fn build() {}\n".to_string();
    st.poll();
    assert!(
        matches!(st.rhai_verdict(), Some(Err(_))),
        "the control: Rust source IS a Rhai error while the buffer is Rhai"
    );

    st.strategy_source = StrategySource::Plugin;
    assert!(!st.buffer_is_rhai());
    st.poll();
    assert_eq!(st.rhai_verdict(), None, "Plugin mode must hold no Rhai verdict");
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
    assert!(matches!(st.rhai_verdict(), Some(Err(_))));
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
    st.strategy_source = StrategySource::Plugin;
    assert!(st.run_disabled_reason().is_some(), "an unbuilt plugin cannot run");
    assert_eq!(st.run_disabled_reason(), st.run_blocked_reason());
}

#[test]
fn run_asks_for_a_slice_over_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let st = state_new(&dir, Arc::new(DataFusionHist::open(dir.path()).unwrap()));
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
    let (_wf_tx, wf_rx) = std::sync::mpsc::channel();
    st.wf_rx = Some(wf_rx);
    assert_eq!(st.walk_forward_disabled_reason(), Some(WALK_FORWARD_BUSY));
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

/// Switching backends keeps the address the operator typed — both dial the same compute daemon —
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
