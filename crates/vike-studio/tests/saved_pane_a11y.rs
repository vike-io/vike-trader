//! Accessibility-tree tests for the Saved-strategies pane ([`vike_studio::SavedPane::ui`]).
//!
//! When this file was written, no test in vike-studio rendered a frame at all, and it opened by
//! saying the SHELL could not be driven headlessly — "`StudioState::ui` wants a `DataFusionHist`,
//! a worker thread and a run backend".
//!
//! ⚠ **CORRECTED.** One third of that was true when it was written and is not any more; the other
//! two thirds were never true. Split-plane B12 turned the Studio's store into a trait handle
//! (`crates/vike-studio-core/src/run.rs`'s `StoreHandle`), so `crates/vike-studio/src/studio.rs`'s
//! `new_with_qa` asks only for that handle, a `PathBuf`, a `Default`-able key struct and two QA
//! flags — an empty temp-directory store and three literals satisfy the lot — and the
//! shell's four worker receivers are `Option<Receiver<..>>` that are `None` while nothing is
//! running, so RENDERING never wanted a thread or a backend at all. The crate's own
//! `crates/vike-studio/examples/studio_shot.rs` had been driving the real `ui()` through
//! `egui::Context::run_ui` the whole time, which is as close as a claim gets to being refuted by
//! the directory it lives in. `crates/vike-studio/tests/studio_shell_render.rs` is where the
//! shell's render coverage lives now.
//!
//! What survives the correction is why this file still tests `SavedPane` ALONE rather than folding
//! into that one: the pane owns no store, spawns nothing, and its `ui()` returns a plain
//! [`SavedAction`] for the caller to perform, so the fixture below is three lines of state and its
//! assertions name a row INDEX — a fact about the pane's contract with its caller, not about a
//! shell pose. Reaching the same two `add_enabled` gates through the whole shell would need a
//! Saved tab, a store and a persisted strategy list to say something smaller.
//!
//! What it covers that no pure test can:
//!
//! - **The two `add_enabled` gates are real.** `💾 Save` is disabled while the name box is blank
//!   and `Compare all` while nothing is saved. Both are one bool in `ui()`; inverting either lets
//!   a click through to `StudioState`, which then saves a strategy under the empty name or runs a
//!   comparison over an empty list.
//! - **Row → index routing.** Every row's Load/✕ pair carries the enumerate index, and the pane
//!   hands the CALLER an index into a `Vec` it does not own. An off-by-one loads (or deletes)
//!   somebody else's strategy, and the type system cannot see it — `Load(0)` and `Load(1)` are
//!   the same type.
//!
//! ⚠ `Harness::run` steps until repaints settle, so only the first frame of a `run()` sees a
//! click; `ui()` returns `None` on the frames after. [`Fixture::action`] therefore keeps the last
//! `Some` rather than the last return value — assigning the return directly would let a trailing
//! frame blank the action under test and pass every assertion vacuously.

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_studio::{SavedAction, SavedPane, SavedStrategy};

struct Fixture {
    pane: SavedPane,
    /// the most recent action the pane asked for, kept across the settling frames of one `run()`
    action: Option<SavedAction>,
}

fn harness(names: &[&str], save_name: &str) -> Harness<'static, Fixture> {
    let pane = SavedPane {
        strategies: names.iter().map(|n| SavedStrategy::rhai(*n, "fn on_bar(){}")).collect(),
        save_name: save_name.to_string(),
        ..SavedPane::default()
    };
    Harness::builder().with_size(egui::vec2(420.0, 640.0)).build_ui_state(
        |ui, f: &mut Fixture| {
            if let Some(a) = f.pane.ui(ui) {
                f.action = Some(a);
            }
        },
        Fixture { pane, action: None },
    )
}

/// Every accessibility node that is a button carrying exactly `label`, in tree order.
fn buttons<'t>(h: &'t Harness<'static, Fixture>, label: &str) -> Vec<Node<'t>> {
    h.root()
        .children_recursive()
        .filter(|n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some(label)
        })
        .collect()
}

/// Whether the one button labelled `label` is disabled. Panics if it is not on screen exactly
/// once — "the control vanished" must fail loudly, not read as "disabled".
fn is_disabled(h: &Harness<'static, Fixture>, label: &str) -> bool {
    let found = buttons(h, label);
    assert_eq!(found.len(), 1, "expected exactly one {label:?} button, found {}", found.len());
    found[0].accesskit_node().is_disabled()
}

/// `SavedAction` carries no `Debug`/`PartialEq`, and this test has no business adding derives to
/// the library to suit itself. The exhaustive match also makes a NEW variant a compile error here,
/// which is the right place to notice one.
fn describe(action: &Option<SavedAction>) -> String {
    match action {
        None => "none".to_string(),
        Some(SavedAction::Load(i)) => format!("Load({i})"),
        Some(SavedAction::Delete(i)) => format!("Delete({i})"),
        Some(SavedAction::SaveCurrent) => "SaveCurrent".to_string(),
        Some(SavedAction::CompareAll) => "CompareAll".to_string(),
    }
}

/// Both `add_enabled` gates hold, in the accessibility tree an assistive client actually reads.
///
/// Reddens on inverting (or dropping) either `can_save` / `can_compare` in `SavedPane::ui`.
#[test]
fn save_and_compare_are_disabled_until_they_have_something_to_act_on() {
    // Nothing saved, no name typed: neither button may be pressable.
    let mut empty = harness(&[], "");
    empty.run();
    assert!(is_disabled(&empty, "💾 Save"), "Save must be disabled while the name box is blank");
    assert!(
        is_disabled(&empty, "Compare all"),
        "Compare all must be disabled while nothing is saved"
    );

    // A name typed and one strategy saved: both become pressable.
    let mut ready = harness(&["mean-revert"], "my-strategy");
    ready.run();
    assert!(!is_disabled(&ready, "💾 Save"), "a non-blank name must enable Save");
    assert!(!is_disabled(&ready, "Compare all"), "a saved strategy must enable Compare all");
}

/// A blank-looking name is still blank: `can_save` trims, so spaces alone must not arm Save.
///
/// Reddens on dropping the `.trim()` from `can_save` — the edit that lets a strategy be saved
/// under a whitespace name, which then reads as unnamed everywhere downstream.
#[test]
fn a_whitespace_only_name_does_not_arm_the_save_button() {
    let mut h = harness(&[], "   ");
    h.run();
    assert!(is_disabled(&h, "💾 Save"));
}

/// The list renders one row per saved strategy, and each row's Load hands the caller ITS OWN
/// index.
///
/// Reddens on an off-by-one in the row loop (`Load(i + 1)`), on dropping a row, and on the Load /
/// Delete pair being swapped — every one of which silently acts on the wrong strategy.
#[test]
fn the_list_renders_one_row_per_strategy_and_loads_the_row_that_was_clicked() {
    let mut h = harness(&["alpha", "beta", "gamma"], "");
    h.run();

    assert_eq!(buttons(&h, "Load").len(), 3, "one Load button per saved strategy");
    assert_eq!(buttons(&h, "✕").len(), 3, "one Delete button per saved strategy");

    // Click the SECOND row's Load — rows are emitted in list order, so this must be index 1.
    let loads = buttons(&h, "Load");
    loads[1].click();
    drop(loads);
    h.run();
    assert_eq!(describe(&h.state().action), "Load(1)");

    // ...and the row's other button is the DELETE for the same index, not a second Load.
    h.state_mut().action = None;
    let deletes = buttons(&h, "✕");
    deletes[1].click();
    drop(deletes);
    h.run();
    assert_eq!(describe(&h.state().action), "Delete(1)");
}
