//! The editor header's verdict per strategy source: a plugin's Rust buffer is not judged as Rhai,
//! and every control that writes Rhai is refused over one.

use egui_kittest::kittest::NodeT;
use vike_studio::{ChatApiKeys, PLUGIN_EDITOR_CHIP, RightTab, StrategySource};
use vike_ui_theme::icons;

use crate::common::{empty_store, seeded_store, state};
use crate::support::{all_text, button, buttons, harness_over, is_disabled, settle};

// ======================= the editor header's verdict, per strategy source =======================

/// A REAL plugin author's source: the Rust strategy both live smokes build — the builder's own and
/// `crates/vike-studio/tests/studio_live_path_smoke.rs`, which includes these same bytes. A strategy
/// the builder service actually compiles, not a fragment invented to fail a Rhai parse, because
/// the defect was a VALID plugin wearing a red header chip.
const PLUGIN_SOURCE: &str =
    include_str!("../../../vike-strategy-builder/tests/fixtures/live_smoke_strategy.rs.in");

/// The editor header's Rhai error chip (`● error · line N`, or a bare `● error` when rhai's
/// message names no line), found by PREFIX so the assertion does not depend on which line of
/// [`PLUGIN_SOURCE`] rhai trips on first.
fn rhai_error_chip(text: &[String]) -> bool {
    text.iter().any(|t| t.starts_with("● error"))
}

/// The inline banner under the header: `icons::FAILED`, a space, then
/// `crates/vike-studio/src/panes/editor.rs`'s `format_compile_error`. Found by that PREFIX — the icon
/// followed by words — which no other label carries (the central panel's failure mark is the icon
/// alone). The header chip's twin: both said the same wrong thing about a plugin, so both are
/// asserted.
fn rhai_error_banner(text: &[String]) -> bool {
    let banner = icons::FAILED.accessible_label("");
    text.iter().any(|t| t.starts_with(&format!("{banner} ")))
}

/// **The measured defect: a Plugin's Rust buffer was judged as RHAI.** On the real v0.1.34 GUI a
/// valid plugin read `● error · line 4` (red) in the editor header beside the `● built <sha>` of
/// its own successful Build — `crates/vike-studio/src/studio/workers.rs`'s `poll` ran the Rhai
/// compile check on the buffer whatever `strategy_source` said.
///
/// ONE harness, three poses, and every switch is a CLICK on the Strategy pane's own mode buttons
/// rather than an assignment — so it also proves the verdict follows the button the operator
/// actually presses:
///
/// 1. **Rhai mode (the session default) over the same Rust source shows the red chip and the
///    banner.** That is the control, and it is correct: Rhai mode would RUN this buffer as Rhai.
///    Without it the Plugin pose's "no error chip" could pass for the wrong reason — a source rhai
///    happens to accept, or a header that stopped rendering a chip at all.
/// 2. **Plugin mode drops both, shows no green verdict either, and says what is known instead**
///    ([`PLUGIN_EDITOR_CHIP`]). A bare "no red" would be satisfied by `● compiles`, which is the
///    same lie in the other colour — nothing judged this buffer.
/// 3. **Back to Rhai re-checks**: the red chip returns with the source unchanged, so no "no
///    verdict" state is carried across the switch.
///
/// The state-machine twin (what `poll` HOLDS, frame after frame) is
/// `crates/vike-studio/src/studio_tests/shell_state.rs`'s
/// `plugin_mode_holds_no_rhai_verdict_and_rhai_mode_rechecks_on_return`.
#[test]
fn a_plugin_buffer_is_not_judged_as_rhai_and_the_same_buffer_in_rhai_mode_is() {
    let (dir, store) = empty_store();
    let mut st = state(&store, &dir, RightTab::Strategy, ChatApiKeys::default());
    st.editor.source = PLUGIN_SOURCE.to_string();
    st.saved_source = st.editor.source.clone();
    let mut h = harness_over(st);
    settle(&mut h);

    // 1 — the control.
    assert_eq!(h.state().strategy_source, StrategySource::Rhai, "a session starts in Rhai mode");
    let text = all_text(&h);
    assert!(
        rhai_error_chip(&text),
        "the control: Rust source in RHAI mode must wear the Rhai error chip; rendered: {text:?}"
    );
    assert!(rhai_error_banner(&text), "...and its inline banner; rendered: {text:?}");
    assert!(
        !text.iter().any(|t| t == PLUGIN_EDITOR_CHIP),
        "Rhai mode is not Plugin mode; rendered: {text:?}"
    );

    // 2 — the fix.
    button(&h, "Plugin (Rust)").click();
    settle(&mut h);
    assert_eq!(h.state().strategy_source, StrategySource::Plugin, "the click must take");
    let text = all_text(&h);
    assert!(
        !rhai_error_chip(&text),
        "a Plugin's Rust buffer must not be judged as Rhai; rendered: {text:?}"
    );
    assert!(!rhai_error_banner(&text), "...nor bannered as a Rhai error; rendered: {text:?}");
    assert!(
        !text.iter().any(|t| t == "● compiles"),
        "...nor passed as Rhai either — nothing has judged this buffer; rendered: {text:?}"
    );
    assert!(
        text.iter().any(|t| t == PLUGIN_EDITOR_CHIP),
        "the header must say what IS known: the language, and that Build is the verdict; \
         rendered: {text:?}"
    );

    // 3 — the way back.
    button(&h, "Rhai script").click();
    settle(&mut h);
    assert_eq!(h.state().strategy_source, StrategySource::Rhai, "the click must take");
    let text = all_text(&h);
    assert!(
        rhai_error_chip(&text),
        "back in Rhai mode the same buffer must be re-checked and fail again; rendered: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t == PLUGIN_EDITOR_CHIP),
        "...and the Plugin chip must not outlive the switch; rendered: {text:?}"
    );
}

/// **Every control that writes RHAI into the editor is refused over a Plugin's Rust buffer, and
/// armed over a Rhai one.** There are four: the Sweep pane's template `Load`, each `Browse
/// templates` card's `Load`, the empty results panel's `Load a template`, and the AI Copilot's
/// `Apply to editor`. Before this, all four wrote Rhai straight over the plugin's Rust source while
/// `Plugin (Rust)` stayed selected — and a Plugin save keeps no copy of the source, so that was the
/// only copy. `crates/vike-studio/src/studio/strategy.rs`'s `rhai_writer_blocked_reason` argues why
/// they are refused rather than made to switch the source; its unit twin
/// `plugin_mode_refuses_every_rhai_writer_and_the_rhai_modes_do_not` pins the check INSIDE the
/// write.
///
/// Each pose flips ONLY the strategy source over one harness, so the one thing that moves the
/// controls is the mode. Rhai first, as the control — every loader armed, so the Plugin pose's
/// "disabled" cannot be a control that is always off — then Plugin, where a click on a refused
/// control must leave the buffer and the mode exactly as they were, then Rhai again, where the same
/// click writes. The gallery is OPENED (its header is clicked), so its cards are really in the tree:
/// the count assertion is what stops a gallery that rendered no cards passing "all disabled"
/// vacuously.
///
/// ⚠ The clicks are safe: a template load and an Apply are pure buffer writes — no worker, no
/// network. `Send` is never pressed.
#[test]
fn every_rhai_writer_is_refused_over_a_plugin_buffer_and_armed_over_a_rhai_one() {
    // SEEDED, so a slice is pickable and the empty results panel offers `Load a template`.
    let (dir, store) = seeded_store();
    let mut st = state(&store, &dir, RightTab::Sweep, ChatApiKeys::default());
    st.editor.source = PLUGIN_SOURCE.to_string();
    st.saved_source = st.editor.source.clone();
    let mut h = harness_over(st);
    settle(&mut h);
    button(&h, "Browse templates").click();
    settle(&mut h);
    let loaders = 1 + vike_script::TEMPLATES.len();

    // Rhai — the control.
    let loads = buttons(&h, "Load");
    assert_eq!(loads.len(), loaders, "the pane's Load plus one per gallery card must be on screen");
    assert!(loads.iter().all(|n| !n.accesskit_node().is_disabled()), "Rhai mode arms every Load");
    assert!(!is_disabled(&h, "Load a template"), "...and the results panel's loader");

    // Plugin — refused, all of them, and a click writes nothing.
    h.state_mut().strategy_source = StrategySource::Plugin;
    settle(&mut h);
    let loads = buttons(&h, "Load");
    assert_eq!(loads.len(), loaders, "Plugin mode must still SHOW the loaders, disabled");
    assert!(
        loads.iter().all(|n| n.accesskit_node().is_disabled()),
        "no template Load may write Rhai over a Plugin's Rust buffer"
    );
    assert!(is_disabled(&h, "Load a template"), "...nor the results panel's loader");
    buttons(&h, "Load")[0].click();
    settle(&mut h);
    button(&h, "Load a template").click();
    settle(&mut h);
    assert_eq!(
        h.state().editor.source,
        PLUGIN_SOURCE,
        "a refused loader must not touch the buffer"
    );
    assert_eq!(h.state().strategy_source, StrategySource::Plugin, "...nor switch the mode");

    // Rhai again — the same click now writes, so the refusal above was the mode's doing.
    h.state_mut().strategy_source = StrategySource::Rhai;
    settle(&mut h);
    buttons(&h, "Load")[0].click();
    settle(&mut h);
    assert_eq!(
        h.state().editor.source,
        vike_script::TEMPLATES[h.state().template_idx].1,
        "in Rhai mode the pane's Load writes the selected template"
    );

    // The copilot's Apply, the fourth writer, on its own pane.
    let generated = "fn on_bar() {}";
    let mut st = state(&store, &dir, RightTab::Chat, ChatApiKeys::default());
    st.editor.source = PLUGIN_SOURCE.to_string();
    st.saved_source = st.editor.source.clone();
    st.chat.last = Some(vike_ai::AgentResult {
        code: generated.to_string(),
        accepted: true,
        ..Default::default()
    });
    let mut h = harness_over(st);
    settle(&mut h);
    assert!(!is_disabled(&h, "Apply to editor"), "the control: Rhai mode arms Apply");
    h.state_mut().strategy_source = StrategySource::Plugin;
    settle(&mut h);
    assert!(is_disabled(&h, "Apply to editor"), "Apply must not write Rhai over Rust source");
    button(&h, "Apply to editor").click();
    settle(&mut h);
    assert_eq!(h.state().editor.source, PLUGIN_SOURCE, "a refused Apply must not touch the buffer");
    h.state_mut().strategy_source = StrategySource::Rhai;
    settle(&mut h);
    button(&h, "Apply to editor").click();
    settle(&mut h);
    assert_eq!(h.state().editor.source, generated, "in Rhai mode the same click applies");
}
