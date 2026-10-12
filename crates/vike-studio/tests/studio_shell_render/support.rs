//! Harness plumbing and accessibility-tree queries shared by every child of this binary.
//! "The module doc" below means the root's, in `crates/vike-studio/tests/studio_shell_render.rs`.

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_studio::{ChatApiKeys, RightTab, StoreHandle, StudioState};
use vike_ui_theme::icons;

use crate::common::state;

// ============================ harness plumbing ============================

/// Wrap a posed state in a harness over the REAL `StudioState::ui`.
///
/// 1440×960 because the shell is four panels wide (a 40px rail, a 340px tools panel, a 460px
/// editor) and a cramped harness clips controls out of the tree, turning a real assertion into a
/// "not found" panic. `max_steps` is raised from `egui_kittest`'s default of 4 because
/// `Harness::run` PANICS past it: the raise costs nothing when the UI settles (the loop breaks the
/// moment a frame requests no immediate repaint) and removes a flake vector. If the shell ever
/// repaints forever the failure is loud and self-diagnosing — the error names the repaint causes.
pub(crate) fn harness_over(st: StudioState) -> Harness<'static, StudioState> {
    Harness::builder().with_size(egui::vec2(1440.0, 960.0)).with_max_steps(64).build_ui_state(
        |ui, st: &mut StudioState| {
            // The shell draws icons; their family is bound only by the app's type (module doc).
            if vike_ui_theme::harness::type_ready(ui.ctx()) {
                st.ui(ui);
            }
        },
        st,
    )
}

/// The common case: a keyless shell over `store`, posed on `tab`.
pub(crate) fn shell(
    store: &StoreHandle,
    dir: &tempfile::TempDir,
    tab: RightTab,
) -> Harness<'static, StudioState> {
    harness_over(state(store, dir, tab, ChatApiKeys::default()))
}

/// Run until repaints settle, then assert the emitted frame is paintable.
///
/// EVERY interaction in this file goes through here, which is what makes each of these scenarios a
/// geometry test as well as an accessibility one — the free upgrade
/// `crates/vike-chart/tests/common/mod.rs`'s `run_full` gives that crate's scenarios. See the
/// module doc for why no `textures_delta` clear precedes the assertion here even though
/// `assert_frame_sane`'s own doc demands one of a raw `run_ui` harness.
pub(crate) fn settle(h: &mut Harness<'static, StudioState>) {
    h.run();
    vike_ui_theme::frame_sanity::assert_frame_sane(h.output());
}

// ============================ accessibility-tree queries ============================

/// Every accessibility node matching `pred`, in tree order.
pub(crate) fn nodes<'t>(
    h: &'t Harness<'static, StudioState>,
    pred: impl Fn(&Node<'t>) -> bool,
) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

/// The accessible name of a run button: `icons::RUN`, then its words.
pub(crate) fn run_button(words: &str) -> String {
    icons::RUN.accessible_label(words)
}

/// Every BUTTON carrying exactly `label`. Equality, never containment: `Run` must not match
/// `Run Sweep` or `Run backtest`, and a rail icon must not match the toolbar's strategy chip,
/// whose label is that same icon followed by the strategy name.
pub(crate) fn buttons<'t>(h: &'t Harness<'static, StudioState>, label: &str) -> Vec<Node<'t>> {
    nodes(h, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(label)
    })
}

/// The one button labelled `label`. Panics if it is not on screen exactly once — "the control
/// vanished" must fail loudly, not read as "disabled".
pub(crate) fn button<'t>(h: &'t Harness<'static, StudioState>, label: &str) -> Node<'t> {
    let found = buttons(h, label);
    assert_eq!(found.len(), 1, "expected exactly one {label:?} button, found {}", found.len());
    found[0]
}

pub(crate) fn is_disabled(h: &Harness<'static, StudioState>, label: &str) -> bool {
    button(h, label).accesskit_node().is_disabled()
}

pub(crate) fn has_button(h: &Harness<'static, StudioState>, label: &str) -> bool {
    !buttons(h, label).is_empty()
}

/// The text of every `Role::Label` node, read off its accesskit VALUE.
///
/// ⚠ The `value()` half is load-bearing and is the trap
/// `crates/vike-studio/tests/picker_disclosure.rs` documents from having fallen into it: egui files
/// a `Label`'s (and a `ComboBox`'s) text under the node's VALUE, not its `label`, so a helper that
/// read `label()` here would return an empty list for every store and turn every negative
/// assertion green.
pub(crate) fn label_values(h: &Harness<'static, StudioState>) -> Vec<String> {
    nodes(h, |n| n.accesskit_node().role() == Role::Label)
        .into_iter()
        .filter_map(|n| n.accesskit_node().value())
        .collect()
}

/// Every piece of text in the tree, label and value alike — what a human (or a screen reader) has
/// in front of them. The `contains` assertions below run over this, because several of the strings
/// they look for are embedded in a longer sentence the shell composed.
pub(crate) fn all_text(h: &Harness<'static, StudioState>) -> Vec<String> {
    h.root()
        .children_recursive()
        .flat_map(|n| {
            let a = n.accesskit_node();
            [a.label(), a.value()].into_iter().flatten().map(|t| t.to_string()).collect::<Vec<_>>()
        })
        .collect()
}
