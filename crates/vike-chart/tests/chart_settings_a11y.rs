//! Accessibility-tree tests for the in-chart "Chart settings" dialog
//! (`crates/vike-chart/src/chart/dialogs.rs`'s `settings_dialog_body`, reached through
//! `crates/vike-chart/src/chart/mod.rs`'s `draw`).
//!
//! Not one test in this crate carried an accesskit assertion before this file (the count of tests
//! it DID carry is deliberately not written down here — that number rots, and the claim that
//! matters is the zero). Its two render rungs stop short of this dialog in the same place:
//! `crates/vike-chart/tests/draw_characterization.rs` asserts the
//! `ChartActions` a frame RETURNS, `crates/vike-chart/tests/tessellation_goldens.rs` the shapes it
//! EMITS — and a dialog that renders every control at the right coordinate in the right colour,
//! while lying about which of them a user may touch, satisfies both. `settings_dialog_body` is
//! `pub(crate)`, so nothing could call it directly either; before this file, ZERO tests opened it
//! at all.
//!
//! What only the tree can see, one test each:
//!
//! 1. **The rail's selection and the panel on screen are the same claim.** `settings_dialog_body`
//!    writes `*tab = t` from a `selectable_label` loop and then `match *tab`es to a panel; the two
//!    are separate reads of the same value and nothing tied them together. A swapped `match` arm
//!    compiles, paints a full dialog and leaves every pure test green, while the rail highlights
//!    `Canvas` over the Symbol panel's controls. Both halves are tree-only: a `SelectableLabel`'s
//!    highlight is a `Toggled` on a `Role::Button`, and "which panel is on screen" is which
//!    controls EXIST.
//! 2. **`Auto` really disables the precision spinner.** `symbol_panel` wraps that `DragValue` in
//!    `ui.add_enabled_ui(!auto, …)`. Delete the wrapper — a two-token edit — and the field stays
//!    draggable while the box beside it reads `Auto`, so the chart pins a decimal count its own
//!    dialog denies having. `enabled` is not a colour: it is `set_disabled` on the node, and the
//!    only thing that reads it is an accessibility client.
//! 3. **The master `Grid` toggle gates the per-axis rows.** `canvas_panel` wraps them in
//!    `ui.add_enabled_ui(w.show_grid, …)`, and `ChartOptions`'s `grid_show` ANDs both with the
//!    master anyway — so an ungated row is a control that appears to work and changes nothing.
//!    The pose also pins that a DISABLED row still reports its own `Toggled`, i.e. that the gate
//!    suppresses the interaction and not the value.
//! 4. **The background mode the combo names and the swatches it offers agree.** `Solid` shows one
//!    stop, `Gradient` two, and the closed combo is the only text that says which mode is live.
//!    Inverting either half — the `selected_text` ternary or the `if w.bg_gradient` guard on the
//!    top-stop swatch — leaves the other half telling the truth, so the two are asserted together.
//!
//! ⚠ **Every pose that asserts a VALUE reads the WORKING copy against a DIFFERENT committed one.**
//! `draw` hands `settings_dialog_body` `settings.working` while `ChartInputs`'s `options` holds the
//! committed set, and the live-preview contract is that the dialog edits and displays the former.
//! Posing the two identically would let a body that read `options` pass, so tests 2, 3 and 4 pose
//! them apart — which is what makes each reading answer *which side did you read*.
//!
//! ⚠ **Two poses deliberately do NOT differ, and saying "every pose" would be a lie about them.**
//! [`the_dialog_shows_exactly_the_section_its_rail_reports_as_selected`] and the grid-ON half of
//! [`the_master_grid_toggle_gates_the_per_axis_grid_rows`] both open on `ChartOptions::default()`
//! against a committed `ChartOptions::default()`. Neither asserts provenance — the first asks
//! WHICH PANEL is on screen, a question `options` cannot answer either way, and the second is the
//! enabled half of a gate whose disabled half (which DOES differ) is what pins the read. P5's row
//! below records exactly that: the routing test survives a body pointed at the committed options,
//! and is not claimed to die.
//!
//! ⚠ **No test clicks `OK` or `Cancel`.** Both are outside this file's subject — they mutate
//! `ChartActions`'s `options_change` and close the window, which is `draw`'s contract with its
//! caller rather than the dialog's with its user, and clicking one would tear down the very tree
//! the next assertion reads.
//!
//! # Kill proof
//!
//! Two properties here cannot be satisfied by a constant answer, so they keep working with nobody
//! remembering a plant. The section matrix is nine `(posed tab, marker control)` pairs where
//! presence must EQUAL identity — a dialog that always renders one panel fails six of nine, one
//! that renders all three fails six of nine, and one that renders none fails three; the closed
//! pose adds a fourth row where all three must be absent. And every gate below is asserted in BOTH
//! directions off ONE control, so a gate stuck open fails one half and a gate stuck shut the other.
//! [`SECTIONS`]'s exhaustive coverage of `SettingsTab::ALL` is checked at the top of the matrix
//! test, so a fourth section reddens here rather than going quietly untested.
//!
//! **⚠ TRACED BY HAND — the lane run is still owed.** This repo's standard is that a gate not
//! shown to fail is decoration, and no cargo runs on the branch that wrote this file. So each
//! plant was discharged the next-honest way available, the way
//! `crates/vike-studio/tests/studio_shell_render.rs` discharged its own: the edit was made against
//! the real `crates/vike-chart/src/chart/dialogs.rs` and walked through the named test assertion
//! by assertion, IN EXECUTION ORDER, recording which assertion fires FIRST and with what message.
//! That is a claim about the code as READ, not as executed, and the table says so — a lane run
//! replaces a `traced` row one-for-one with a `measured` one (the way
//! `crates/vike-chart/tests/frame_sanity_gate.rs` records what ITS plant reddened — that page owns
//! those numbers and they are deliberately not copied here), and the recipes below
//! stay intact so that run needs no re-derivation. The trace found no dead plant — all seven fire
//! — but it found P1's recipe claiming three GREEN tests that are in fact RED, and P2's and P5's
//! over-stating which poses are reached; those are corrected in place.
//!
//! | plant | reddening test(s) | first failing assertion (traced) | status |
//! |---|---|---|---|
//! | P1 | [`the_dialog_shows_exactly_the_section_its_rail_reports_as_selected`]; collateral: ALL THREE of the others | the matrix test's FIRST pose (Symbol), inner row `other == Symbol`, presence leg: `with "Symbol" selected, "Color bars based on previous close" must be on screen` — the swap renders `canvas_panel` there. It panics before reaching the Canvas pose. The collateral is not subtle, and the first draft's "every other test stays GREEN" was simply wrong: all three others open on a tab whose panel has moved. [`one`]'s exactly-once floor panics with `expected exactly one CheckBox labelled "Auto", found 0` in the precision test and `… labelled "Grid", found 0` in the grid test, and the background test dies on its own structural floor, `the Canvas panel carries exactly one combo — the background mode selector` (0 ≠ 1), because `symbol_panel` carries none. | traced 2026-08-22 |
//! | P2 | the matrix test alone — nothing else clicks the rail (every other pose sets `settings.tab` directly) | iteration 2, BEFORE any tree read: `the "Canvas" rail entry must select its OWN section`. The Scales pose is never reached, so the redness is one pose, not two. | traced 2026-08-22 |
//! | P3 | [`the_auto_checkbox_gates_the_precision_spinner_and_the_spinner_shows_the_working_copy`], `Auto`-checked half only | `while Auto is checked the precision spinner must not be operable` — after that pose's `Toggled::True` read passed, which is what shows the gate and the checkbox are pinned separately | traced 2026-08-22 |
//! | P4 | [`the_master_grid_toggle_gates_the_per_axis_grid_rows`], one half per direction | forced TRUE: the grid-OFF half's `"Vertical grid" must not be operable while the master Grid toggle is off`. Forced FALSE: the grid-ON half's `"Vertical grid" must be operable once the master Grid toggle is on`, reached only after the OFF half passes entirely. | traced 2026-08-22 |
//! | P5 | P3's, P4's and [`the_background_combo_and_its_swatches_agree_on_the_working_copys_mode`]; the matrix test stays GREEN | P3's test first: `a working copy with no precision override must read as Auto` (the committed side pins 3 decimals, so `Auto` reads unchecked). P4's dies on the bare `Toggled::False` read of the master. The background test reaches its combo leg: `the closed combo must name the WORKING copy's background mode`. | traced 2026-08-22 |
//! | P6 | [`the_background_combo_and_its_swatches_agree_on_the_working_copys_mode`], `Solid` half only | `Solid mode must offer 1 background swatch(es) before the Grid lines section` — the `Gradient` half passes untouched, which is the whole point of counting rather than merely asserting presence | traced 2026-08-22 |
//! | P7 | all four tests here, and every other rendering suite in this crate | each dies inside `dialog_harness`'s `settle`: `frame geometry is not paintable (…)`, the bad-coordinate list naming the Circle's NaN centre. It fires in `settle` and NOT in [`harness`], because the constructor's own settle loop asserts nothing. | traced 2026-08-22 |
//!
//! The plants, verbatim — run them in a verification lane and flip the rows above to `measured`:
//!
//! - **P1 panel routing** — swap the `SettingsTab::Symbol` and `SettingsTab::Canvas` arms of
//!   `settings_dialog_body`'s `match *tab`.
//! - **P2 rail routing** — in the same function's rail loop, write `*tab = SettingsTab::Symbol`
//!   instead of `*tab = t`.
//! - **P3 the precision gate** — delete `symbol_panel`'s `ui.add_enabled_ui(!auto, …)` wrapper,
//!   keeping its body (it needs no other edit — the body already owns its `p` local).
//! - **P4 the grid gate** — change `canvas_panel`'s `ui.add_enabled_ui(w.show_grid, …)` to
//!   `add_enabled_ui(true, …)`, then to `add_enabled_ui(false, …)`.
//! - **P5 provenance** — in `draw`'s dialog block, bind `let mut o = options.clone();` and hand
//!   `settings_dialog_body` `&mut o` instead of `&mut settings.working`.
//! - **P6 the swatch pair** — delete `canvas_panel`'s `if w.bg_gradient` guard on the top-stop
//!   `color_edit_button_srgb`, so both swatches always render.
//! - **P7 geometry reach** — paint a `NaN`-centred circle at the top of `draw`.

mod common;
mod dialog_harness;

use egui::accesskit::{Role, Toggled};
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_chart::options::SettingsTab;
use vike_chart::ChartOptions;

use dialog_harness::{
    all_nodes, combo_texts, count, count_before, dialog_nodes, dialog_titles, fixture, harness,
    one, present, settle, spin_values, toggled, Fixture,
};

/// The title `draw` gives the dialog's `egui::Window`, and therefore the label of the
/// `Role::Window` node the whole subtree hangs beneath.
const TITLE: &str = "Chart settings";

/// The three sections, each as `(tab, its rail label, the ONE control that appears in its panel
/// and in no other)`.
///
/// The markers are checkboxes rather than section headings on purpose: a heading is a
/// `Role::Label` whose text `crates/vike-chart/src/chart/dialogs.rs`'s `section` uppercases, and a
/// heading can survive a routing bug that drops every control under it. A control's presence is
/// the thing a user would notice missing.
const SECTIONS: [(SettingsTab, &str, &str); 3] = [
    (SettingsTab::Symbol, "Symbol", "Color bars based on previous close"),
    (SettingsTab::Canvas, "Canvas", "Grid"),
    (SettingsTab::Scales, "Scales and lines", "Last price line"),
];

/// The dialog open on `tab`, with `working` posed and the COMMITTED options left at their defaults
/// so the two disagree wherever a caller made them.
fn open_on(tab: SettingsTab, working: ChartOptions) -> Harness<'static, Fixture> {
    let mut f = fixture();
    f.settings.open = true;
    f.settings.tab = tab;
    f.settings.working = working;
    let mut h = harness(f);
    settle(&mut h);
    h
}

/// Click the rail entry labelled `label` and settle.
///
/// The `Vec` is dropped before the settle because it borrows the harness; `Node::click` only
/// queues events through a shared handle, so the click itself needs no mutable access.
fn click_rail(h: &mut Harness<'static, Fixture>, label: &str) {
    let nodes = dialog_nodes(h, TITLE);
    one(&nodes, Role::Button, label).click();
    drop(nodes);
    settle(h);
}

/// The panel's single `DragValue`, as a node. [`spin_values`] reads the number it holds; this
/// reads the state around it, which is the half that has no pixels.
fn only_spinner<'t>(nodes: &[Node<'t>]) -> Node<'t> {
    let spins: Vec<Node<'t>> =
        nodes.iter().filter(|n| n.accesskit_node().role() == Role::SpinButton).copied().collect();
    assert_eq!(spins.len(), 1, "expected exactly one DragValue, found {}", spins.len());
    spins[0]
}

/// The rail says which section is selected, the panel shows it, and the two never disagree.
///
/// Reddens on a swapped arm in `settings_dialog_body`'s `match *tab` (the panel moves without the
/// rail) and on its rail loop writing anything but `*tab = t` (the rail moves without the panel).
/// The closed pose is the baseline that keeps the other three from passing vacuously: it proves
/// these controls exist only because the dialog is open, not because the chart draws them anyway.
#[test]
fn the_dialog_shows_exactly_the_section_its_rail_reports_as_selected() {
    assert_eq!(
        SECTIONS.len(),
        SettingsTab::ALL.len(),
        "every SettingsTab needs a marker row here, or a new section ships untested"
    );

    // Closed: no window, and therefore none of the three markers anywhere in the tree.
    let mut shut = harness(fixture());
    settle(&mut shut);
    assert!(dialog_titles(&shut).is_empty(), "no dialog may be open before anything opens one");
    // Read the WHOLE frame, not the (necessarily empty) dialog subtree — an absence asserted
    // inside a window that is not there proves nothing about the chart behind it.
    let everything = all_nodes(&shut);
    for (_, rail, marker) in SECTIONS {
        assert!(
            !present(&everything, Role::CheckBox, marker),
            "{marker:?} ({rail}) must be nowhere on screen while the dialog is shut"
        );
    }

    // Open on the default section, then walk the rail. Each pose asserts the FULL matrix, so a
    // panel that renders too much fails as loudly as one that renders too little.
    let mut h = open_on(SettingsTab::Symbol, ChartOptions::default());
    assert_eq!(
        dialog_titles(&h),
        vec![TITLE.to_string()],
        "opening the settings dialog must put exactly one window on screen"
    );

    for (tab, rail, _) in SECTIONS {
        if tab != SettingsTab::Symbol {
            click_rail(&mut h, rail);
        }
        assert_eq!(
            h.state().settings.tab,
            tab,
            "the {rail:?} rail entry must select its OWN section"
        );

        let nodes = dialog_nodes(&h, TITLE);
        for (other, other_rail, marker) in SECTIONS {
            assert_eq!(
                toggled(one(&nodes, Role::Button, other_rail)),
                Some(if other == tab { Toggled::True } else { Toggled::False }),
                "with {rail:?} selected, the {other_rail:?} rail entry reports the wrong state"
            );
            assert_eq!(
                present(&nodes, Role::CheckBox, marker),
                other == tab,
                "with {rail:?} selected, {marker:?} must{} be on screen",
                if other == tab { "" } else { " NOT" }
            );
        }
    }
}

/// `Auto` owns the precision spinner: checked, the spinner is DISABLED; unchecked, it is live and
/// reads the working copy's own decimal count.
///
/// Reddens on dropping `symbol_panel`'s `ui.add_enabled_ui(!auto, …)` (the checked half), on
/// inverting its condition (the unchecked half), and on the body reading the committed
/// `ChartInputs`'s `options` instead of `settings.working` — the two poses below pin precisions
/// the committed side does not hold.
#[test]
fn the_auto_checkbox_gates_the_precision_spinner_and_the_spinner_shows_the_working_copy() {
    // Auto ON in the working copy while the committed options pin 3 decimals.
    let mut auto =
        open_on(SettingsTab::Symbol, ChartOptions { precision: None, ..ChartOptions::default() });
    auto.state_mut().options.precision = Some(3);
    settle(&mut auto);
    let nodes = dialog_nodes(&auto, TITLE);
    assert_eq!(
        toggled(one(&nodes, Role::CheckBox, "Auto")),
        Some(Toggled::True),
        "a working copy with no precision override must read as Auto"
    );
    assert!(
        only_spinner(&nodes).accesskit_node().is_disabled(),
        "while Auto is checked the precision spinner must not be operable"
    );
    assert_eq!(
        spin_values(&nodes),
        vec![2.0],
        "the disabled spinner still shows the fallback the panel would seed"
    );

    // Auto OFF in the working copy while the committed options are on Auto — a body reading the
    // committed side would show a checked box over a dead spinner.
    let mut pinned = open_on(
        SettingsTab::Symbol,
        ChartOptions { precision: Some(6), ..ChartOptions::default() },
    );
    pinned.state_mut().options.precision = None;
    settle(&mut pinned);
    let nodes = dialog_nodes(&pinned, TITLE);
    assert_eq!(
        toggled(one(&nodes, Role::CheckBox, "Auto")),
        Some(Toggled::False),
        "a working copy pinning 6 decimals must not read as Auto"
    );
    assert!(
        !only_spinner(&nodes).accesskit_node().is_disabled(),
        "with Auto unchecked the precision spinner must be operable"
    );
    assert_eq!(
        spin_values(&nodes),
        vec![6.0],
        "the spinner must show the WORKING copy's precision"
    );
}

/// The master `Grid` checkbox gates both per-axis rows, and gates them without touching what they
/// report.
///
/// Reddens on `canvas_panel`'s `ui.add_enabled_ui(w.show_grid, …)` being dropped or inverted. The
/// `Toggled::True` assertion in the grid-OFF pose is the half that separates a GATE from a value
/// change: `ChartOptions`'s `show_vgrid`/`show_hgrid` stay set while the master is off, and a
/// panel that cleared them instead would lose the user's per-axis choice on the way back on.
#[test]
fn the_master_grid_toggle_gates_the_per_axis_grid_rows() {
    let off =
        open_on(SettingsTab::Canvas, ChartOptions { show_grid: false, ..ChartOptions::default() });
    let nodes = dialog_nodes(&off, TITLE);
    let master = one(&nodes, Role::CheckBox, "Grid");
    assert_eq!(toggled(master), Some(Toggled::False));
    assert!(
        !master.accesskit_node().is_disabled(),
        "the master toggle is the gate — it can never be gated by itself"
    );
    for axis in ["Vertical grid", "Horizontal grid"] {
        let row = one(&nodes, Role::CheckBox, axis);
        assert!(
            row.accesskit_node().is_disabled(),
            "{axis:?} must not be operable while the master Grid toggle is off"
        );
        assert_eq!(
            toggled(row),
            Some(Toggled::True),
            "{axis:?} must still REPORT its own setting while gated — the master hides the grid, \
             it does not forget the per-axis choice"
        );
    }

    let on = open_on(SettingsTab::Canvas, ChartOptions::default());
    let nodes = dialog_nodes(&on, TITLE);
    assert_eq!(toggled(one(&nodes, Role::CheckBox, "Grid")), Some(Toggled::True));
    for axis in ["Vertical grid", "Horizontal grid"] {
        assert!(
            !one(&nodes, Role::CheckBox, axis).accesskit_node().is_disabled(),
            "{axis:?} must be operable once the master Grid toggle is on"
        );
    }
}

/// The Background row's combo and its colour swatches describe the same mode: `Gradient` names two
/// stops and offers two wells, `Solid` names one and offers one.
///
/// The two are asserted together because each alone is satisfiable by the bug in the other:
/// inverting `canvas_panel`'s `selected_text` ternary leaves the right number of wells under the
/// wrong word, and dropping its `if w.bg_gradient` guard leaves the right word over an
/// always-editable top stop that a solid background never paints. The wells are counted only up to
/// the `GRID LINES` heading, because every section below the first carries wells of its own.
#[test]
fn the_background_combo_and_its_swatches_agree_on_the_working_copys_mode() {
    for (gradient, text, wells) in [(true, "Gradient", 2), (false, "Solid", 1)] {
        let mut h = open_on(
            SettingsTab::Canvas,
            ChartOptions { bg_gradient: gradient, ..ChartOptions::default() },
        );
        // The committed options take the OPPOSITE mode, so a panel reading them fails both halves.
        h.state_mut().options.bg_gradient = !gradient;
        settle(&mut h);

        let nodes = dialog_nodes(&h, TITLE);
        assert_eq!(
            count(&nodes, Role::ComboBox),
            1,
            "the Canvas panel carries exactly one combo — the background mode selector"
        );
        assert_eq!(
            combo_texts(&nodes),
            vec![text.to_string()],
            "the closed combo must name the WORKING copy's background mode"
        );
        assert_eq!(
            count_before(&nodes, Role::ColorWell, "GRID LINES"),
            wells,
            "{text} mode must offer {wells} background swatch(es) before the Grid lines section"
        );
    }
}
