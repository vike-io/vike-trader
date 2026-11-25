//! What Strategy file mode does to the Rust buffer it holds: the Rhai writers it refuses, the save that
//! re-baselines to the built source, and the restart that restores it.

use super::super::*;
use super::support::{answered_build, seeded_store, state_new};

/// **No Rhai writer overwrites a Strategy file's Rust buffer** — the write half of the four
/// template/copilot writers, which `rhai_writer_blocked_reason` argues are REFUSED in Strategy file
/// mode rather than made to switch the source. What the buttons render is
/// `crates/vike-studio/tests/headless/studio_shell_render/editor_verdict.rs`'s
/// `every_rhai_writer_is_refused_over_a_strategy_file_buffer_and_armed_over_a_rhai_one`; this pins the
/// check INSIDE the write, which protects any caller that reaches it without a button.
///
/// The second half is the control: in Rhai and Native mode both writers DO write (and only the
/// template load re-baselines), so the refusal is keyed on the mode, not a writer that stopped
/// working.
#[test]
fn strategy_file_mode_refuses_every_rhai_writer_and_the_rhai_modes_do_not() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    let rust = "use vike_model::Strategy;\npub fn build() {}\n";
    let template = TEMPLATES[0].1;
    let copilot = vike_ai::AgentResult {
        code: "fn on_bar() {}".to_string(),
        accepted: true,
        ..Default::default()
    };

    st.strategy_source = StudioStrategySource::Rust;
    st.editor.source = rust.to_string();
    st.saved_source = rust.to_string();
    assert_eq!(st.rhai_writer_blocked_reason(), Some(RHAI_WRITER_BLOCKED_IN_RUST));
    assert!(!st.load_template(template), "a template load must be refused over Rust source");
    assert!(!st.apply_copilot_result(&copilot), "...and so must the copilot's Apply");
    assert_eq!(st.editor.source, rust, "the Rust buffer must survive both");
    assert_eq!(st.saved_source, rust, "...and so must its unsaved baseline");
    assert_eq!(st.strategy_source, StudioStrategySource::Rust, "refused, not switched");

    for source in [StudioStrategySource::Rhai, StudioStrategySource::File] {
        st.strategy_source = source;
        st.editor.source = "stale".to_string();
        // Saved: a template over unsaved changes asks first (`a_template_over_unsaved_changes_asks_first`).
        st.saved_source = "stale".to_string();
        assert_eq!(st.rhai_writer_blocked_reason(), None, "{source:?} buffers are Rhai");
        assert!(st.load_template(template), "{source:?}: a template must load");
        assert_eq!(st.editor.source, template);
        assert_eq!(st.saved_source, template, "{source:?}: a template load re-baselines");
        assert!(st.apply_copilot_result(&copilot), "{source:?}: the copilot must apply");
        assert_eq!(st.editor.source, copilot.code);
        assert_ne!(st.editor.source, st.saved_source, "{source:?}: Apply is an edit, not a load");
    }
}

/// **A template over unsaved changes asks first**: the buffer is untouched and the template waits,
/// "load it over my changes" (`write_template`) writes it, and over a saved buffer it loads at once.
#[test]
fn a_template_over_unsaved_changes_asks_first() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    let template = TEMPLATES[0].1;
    st.editor.source = "fn on_bar() { /* my edit */ }".to_string();
    st.editor_collapsed = true;
    assert_ne!(st.editor.source, st.saved_source, "the control: the buffer has unsaved changes");

    assert!(st.load_template(template), "held, not refused");
    assert_eq!(st.editor.source, "fn on_bar() { /* my edit */ }", "nothing written yet");
    assert_eq!(st.pending_template.as_deref(), Some(template));
    assert!(!st.editor_collapsed, "the editor opens to ask");

    st.write_template(template);
    assert_eq!((st.editor.source.as_str(), st.saved_source.as_str()), (template, template));
    assert_eq!(st.pending_template, None);

    let other = TEMPLATES[1].1;
    assert!(st.load_template(other));
    assert_eq!(st.editor.source, other, "a saved buffer loads at once");
    assert_eq!(st.pending_template, None);
}

/// **The buffer's language survives Strategy files** (the live desktop's report): a `.rs` buffer
/// parked under Strategy files is still Rust, so no Rhai verdict is held on it; a Rhai buffer parked
/// there is still checked as Rhai.
#[test]
fn strategy_files_keeps_the_language_of_the_buffer_it_parks() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.editor.source = "use vike_model::Strategy;\npub fn build() {}\n".to_string();
    st.strategy_source = StudioStrategySource::Rust;
    st.poll();
    st.strategy_source = StudioStrategySource::File;
    st.poll();
    assert!(!st.buffer_is_rhai(), "the parked buffer is the Rust it was");
    assert_eq!(st.rhai_verdict(), None, "no Rhai verdict on Rust source");

    st.strategy_source = StudioStrategySource::Rhai;
    st.poll();
    st.strategy_source = StudioStrategySource::File;
    st.poll();
    assert!(st.buffer_is_rhai(), "...and a Rhai buffer parked there is Rhai");
    assert!(st.rhai_verdict().is_some());
}

/// **A fifth Rhai writer: loading a SAVED Rhai row over a Strategy file buffer.** Found while fixing
/// the four template/copilot writers — `handle_saved_action`'s `Load` arm overwrites
/// `editor.source` for a Rhai row exactly like the template loader does, and was reachable
/// while Strategy file held the only copy of a Rust strategy. Refused the same way: the whole action
/// no-ops rather than switching the mode or touching the buffer. A Native or Strategy file row never
/// writes `editor.source`, so loading one is unaffected by this guard.
#[test]
fn loading_a_saved_rhai_row_is_refused_over_a_strategy_file_buffer_and_armed_over_a_rhai_one() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    let rust = "use vike_model::Strategy;\npub fn build() {}\n";
    st.saved
        .strategies
        .push(StudioSavedStrategy::rhai("saved rhai".to_string(), "let x = 1;".to_string()));

    st.strategy_source = StudioStrategySource::Rust;
    st.editor.source = rust.to_string();
    st.saved_source = rust.to_string();
    st.handle_saved_action(StudioSavedAction::Load(0));
    assert_eq!(st.editor.source, rust, "the Rust buffer must survive a saved-Rhai Load");
    assert_eq!(st.saved_source, rust, "...and so must its unsaved baseline");
    assert_eq!(st.strategy_source, StudioStrategySource::Rust, "refused, not switched");

    // The control: the same Load, over a Rhai (or Native) buffer with no Rust to lose, works.
    for source in [StudioStrategySource::Rhai, StudioStrategySource::File] {
        st.strategy_source = source;
        st.editor.source = "stale".to_string();
        st.handle_saved_action(StudioSavedAction::Load(0));
        assert_eq!(
            st.strategy_source,
            StudioStrategySource::Rhai,
            "{source:?}: the row's own kind loads"
        );
        assert_eq!(st.editor.source, "let x = 1;", "{source:?}: a Rhai row's script loads");
    }
}

/// **A Strategy file save re-baselines to what its sha was BUILT from, and the unsaved chip says what
/// Ctrl+S actually saves.** A Strategy file row stores a name and a sha, never the buffer, and this
/// save used to leave the baseline alone — so the amber `● unsaved` chip could never clear in
/// Strategy file mode, under a tooltip promising that Ctrl+S would save it.
///
/// Walked through every state the chip can be in: unbuilt (the tip says a save cannot capture
/// this), built (the tip says a save names this code), saved (the chip clears), edited since
/// (the chip comes back and a second save does NOT clear it — the row still names v1's
/// artifact, so claiming v2 was saved would be the lie), and a reloaded sha (no recorded source,
/// nothing to re-baseline to).
#[test]
fn a_strategy_file_save_rebaselines_to_its_built_source_and_the_chip_says_what_ctrl_s_saves() {
    let (dir, store) = seeded_store();
    let mut st = state_new(&dir, store);
    st.strategy_source = StudioStrategySource::Rust;
    st.rust_file = "my/strat.rs".to_string();
    st.editor.source = "// v1".to_string();
    assert_ne!(st.editor.source, st.saved_source, "the premise: the unsaved chip is lit");
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_RUST_UNBUILT, "no Build: nothing to save");

    st.build_rx = answered_build("// v1", Ok("c".repeat(64)));
    st.poll();
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_RUST_BUILT, "built: a save names this code");

    st.saved.save_name = "p1".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);
    assert_eq!(st.saved.strategies[0].rust_sha.as_deref(), Some("c".repeat(64).as_str()));
    assert_eq!(st.saved_source, "// v1", "the baseline moves to the source the sha was built from");
    assert_eq!(st.editor.source, st.saved_source, "...so the chip clears");

    st.editor.source = "// v2".to_string();
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_RUST_UNBUILT, "an edit since the Build");
    st.saved.save_name = "p1".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);
    assert_eq!(st.saved_source, "// v1", "a save cannot capture an edit its sha does not hold");
    assert_ne!(st.editor.source, st.saved_source, "...so the chip stays lit, truthfully");

    // A sha reloaded from a saved row: this session never held its source.
    st.rust_built_source = None;
    st.saved_source = "baseline".to_string();
    st.saved.save_name = "p1".to_string();
    st.handle_saved_action(StudioSavedAction::SaveCurrent);
    assert_eq!(st.saved_source, "baseline", "no recorded source, nothing to re-baseline to");
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP_RUST_UNBUILT);

    // Rhai mode keeps its sentence — there Ctrl+S does save the buffer.
    st.strategy_source = StudioStrategySource::Rhai;
    assert_eq!(st.unsaved_chip_tip(), UNSAVED_TIP);
}

/// **A restart restores the strategy source WITH the buffer** — the defect: the buffer was
/// persisted and its language was not, so a Strategy file author's Rust source came back in Rhai mode.
///
/// Round-tripped through the real snapshot a workspace write records (`workspace_snapshot`)
/// and the real restore (`with_workspace`), minus only the disk in between, which
/// `crate::studio::workspace`'s own tests cover — including the old-format file. What comes back:
/// Strategy file mode, the buffer, the strategy file's name, NO Rhai verdict even before the first `poll`,
/// and no sha — a restored session is unbuilt until Build answers (the constructor says why).
#[test]
fn a_strategy_file_session_comes_back_in_strategy_file_mode_after_a_restart() {
    let (dir, store) = seeded_store();
    let rust = "use vike_model::Strategy;\npub fn build() {}\n";
    let mut before = state_new(&dir, store.clone());
    before.strategy_source = StudioStrategySource::Rust;
    before.rust_file = "my/strat.rs".to_string();
    before.editor.source = rust.to_string();
    before.build_rx = answered_build(rust, Ok("c".repeat(64)));
    before.poll();
    assert!(before.rust_sha.is_some(), "the premise: the previous session had built it");

    let ws = before.workspace_snapshot();
    assert_eq!(ws.strategy_source, StudioStrategySource::Rust, "the write must record the source");

    let after = StudioState::with_workspace(
        store,
        dir.path().to_path_buf(),
        StudioChatApiKeys::default(),
        None,
        StudioAutorun::Off,
        ws,
    );
    assert_eq!(
        after.strategy_source,
        StudioStrategySource::Rust,
        "Rust must come back as Strategy file"
    );
    assert_eq!(after.editor.source, rust);
    assert_eq!(after.rust_file, "my/strat.rs");
    assert_eq!(after.rhai_verdict(), None, "not judged as Rhai, not even before the first poll");
    assert!(after.rust_sha.is_none(), "a sha is never restored");
    assert!(after.run_blocked_reason().is_some(), "Run waits for a Build of the restored buffer");
}
