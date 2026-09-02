//! Accessibility-tree tests for the in-chart "Indicator settings" dialog
//! (`crates/vike-chart/src/chart/dialogs.rs`'s `indicator_settings_dialog`, reached through
//! `crates/vike-chart/src/chart/mod.rs`'s `draw`).
//!
//! `dialogs.rs`'s three unit tests all cover `seeded_params`, the pure length-normalizer the seed
//! site delegates to. Nothing rendered the dialog, and the dialog is where that vector becomes a
//! form: it pairs `Active`'s `spec.params` names with `IndicatorEdit`'s `params` values by INDEX,
//! pairs `Active`'s `outputs` names with `IndicatorEdit`'s `lines` paint by index again, and gates
//! two nested sections on booleans. Every one of those is a fact about a widget's semantic state,
//! which `crates/vike-chart/tests/draw_characterization.rs` (the returned `ChartActions`) and
//! `crates/vike-chart/tests/tessellation_goldens.rs` (the emitted shapes) both pass right over.
//!
//! What only the tree can see, one test each:
//!
//! 1. **The dialog that opens belongs to the uid that was asked for.** `indicator_settings_dialog`
//!    resolves `dialog.open_uid` through `indicators.iter().find(...)`; every plausible slip —
//!    `first()`, an enumerate index, a stale uid — still renders a complete, plausible form. The
//!    edit it signals back is keyed to the REQUESTED uid, so a form showing indicator A while
//!    editing indicator B is silent. The window TITLE is the only place the answer is written
//!    down, and `egui::Window` files it as a `Role::Window` label. The same test pins the
//!    target-vanished branch, which is a statement about ABSENCE — no window at all — and so has
//!    no shape to assert.
//! 2. **The form shows the WORKING copy, not the committed indicator.** That is the whole T8
//!    live-preview contract: the widgets edit a scratch copy, the caller applies the deltas. A
//!    body that read the live `Active` instead would compile, render, and quietly discard the
//!    user's own keystrokes on the next frame. The pose below makes the two disagree on purpose.
//! 3. **Each plot row carries its OWN name, visibility and stroke width.** The Style grid emits
//!    five cells per row — a NAMELESS checkbox, the output's name, a colour well, a width
//!    `DragValue` (egui deliberately clears a `DragValue`'s label) and a dash combo — so nothing
//!    but tree ORDER ties them together, which is exactly the tie an off-by-one breaks. A
//!    misrouted row recolours or re-widths somebody else's plot, and `Load(0)`/`Load(1)`-style,
//!    the type system cannot see it.
//! 4. **The two band gates disable exactly what they gate, and they NEST.** `Show levels` gates
//!    the per-level rows AND the overbought/oversold toggle inside it; that toggle in turn gates
//!    only its two fill swatches. Dropping either `ui.add_enabled_ui` leaves a live control
//!    writing a field the renderer ignores; dropping the outer one also un-nests the inner, which
//!    is why the poses walk both.
//!
//! ⚠ **Poses 1, 3 and 4 open the dialog FRESH (`working` is `None`) and pose the live `Active`;
//! pose 2 pre-seeds `working` instead.** The difference is deliberate. A fresh open runs the seed
//! block, so posing the `Active` tests the seed and the render together — which is what makes the
//! row-index test cover both loops. Pre-seeding SKIPS that block, which is the only way to make
//! the working copy and the committed indicator disagree and therefore the only way to ask which
//! side a widget read.
//!
//! ⚠ **No test clicks `OK` or `Cancel`.** Both reset the dialog and signal an edit back through
//! `ChartActions`'s `indicator_edit` — `draw`'s contract with its caller, not the dialog's with its
//! user — and either would tear down the tree the next assertion reads.
//!
//! # Kill proof
//!
//! Three properties here cannot be satisfied by a constant answer. The uid test poses TWO
//! indicators and asks for the second, so an implementation that always opens the first fails it
//! (and one that opens both fails the title-count assertion); the row test poses three distinct
//! `(visible, width, dash)` triples against three distinct names, so any permutation of either
//! side fails; and the gate test asserts the same control ENABLED in one pose and DISABLED in
//! another, so a gate stuck either way fails one half. Each test also opens on a structural floor
//! — the exact window set, the exact label sequence, the exact control counts — checked BEFORE any
//! state assertion, so nothing can pass over a frame that stopped rendering.
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
//! — but it found P5 claiming a nested assertion the panic never reaches, and P3/P4 written as
//! edits that do not compile as spelled; both kinds are corrected in place.
//!
//! | plant | reddening test(s) | first failing assertion (traced) | status |
//! |---|---|---|---|
//! | P1 | [`the_dialog_opens_for_the_uid_it_was_given_and_not_at_all_for_a_vanished_one`] alone — every other test here attaches ONE indicator, so `first()` and `find()` agree | its FIRST assertion: `asking for uid 9 must open THAT indicator's settings, and only one window` — `first()` resolves the rsi attached ahead of it. The vanished-target half is equally violated (one indicator is attached, so `first()` opens a window for uid 4242) and unreached. | traced 2026-08-22 |
//! | P2 | the same test; its title/label legs still pass | `the form must seed from the RUNNING indicator's parameters, not the registry defaults` — expected `[5.0, 40.0, 3.0]`, the registry hands back MACD's 12/26/9. `dialogs.rs`'s three `seeded_params` unit tests stay GREEN as claimed: the function is untouched, only its argument moves. | traced 2026-08-22 |
//! | P3 | [`the_inputs_tab_reads_the_working_copy_rather_than_the_committed_indicator`] alone; P1/P2's test stays GREEN because a fresh open seeds `working` FROM `a`, so both sides read alike | `the parameter spinners must show the working copy's edits` — after the two committed-side preconditions passed, which is what proves the pose was still discriminating. The Source leg is equally violated and unreached, so "fail on both halves" was an over-claim. | traced 2026-08-22 |
//! | P4 | [`each_plot_row_pairs_its_own_name_visibility_and_stroke_width`] alone — RSI has ONE output, so `(0 + 1) % 1 == 0` leaves [`the_band_gates_disable_exactly_what_they_gate`] untouched | `each row must carry ITS OWN plot's name, show state and stroke width` — the rotation reads `[("macd", false, 2.5), ("signal", true, 4.0), ("hist", true, 1.0)]`. The dash-combo leg is equally violated and unreached. | traced 2026-08-22 |
//! | P5 | [`the_band_gates_disable_exactly_what_they_gate`], levels-off pose | `band row 0 must not be operable while Show levels is off` — reached after the block's text and control-count floors pass. ⚠ The panic lands HERE, so the NESTED fill-toggle assertion four lines later is violated but never reported; the first draft claimed it as part of the failure. | traced 2026-08-22 |
//! | P6 | the same test, fill-off pose only | `the overbought swatch must not be operable while the fill toggle is off` — the levels-off pose passes entirely first (the outer gate is untouched), which is what shows the two gates are pinned independently | traced 2026-08-22 |
//! | P7 | all four tests here, and every other rendering suite in this crate | each dies inside `dialog_harness`'s `settle`: `frame geometry is not paintable (…)`, the bad-coordinate list naming the Circle's NaN centre | traced 2026-08-22 |
//!
//! The plants, verbatim — run them in a verification lane and flip the rows above to `measured`:
//!
//! - **P1 target resolution** — replace `indicator_settings_dialog`'s
//!   `indicators.iter().find(|a| a.uid == uid)` with `indicators.first()`.
//! - **P2 seed provenance** — seed the dialog's `params` from `a.spec.params` defaults
//!   (`seeded_params(&[], a.spec.params)`) instead of `seeded_params(&a.params, …)`.
//! - **P3 live-preview provenance** — in the Inputs tab, read the COMMITTED side through scratch
//!   locals, because `a` is a `&Active` and neither field is assignable: `let mut p = a.params[i];`
//!   driving `DragValue::new(&mut p)`, and `.selected_text(a.source.label())` on the combo.
//! - **P4 plot-row routing** — in the Style tab's row loop, hoist `let n = working.lines.len();`
//!   ABOVE the loop (indexing `working.lines[(i + 1) % working.lines.len()]` mutably borrows the
//!   vec while the index expression still reads it) and index `working.lines[(i + 1) % n]`.
//! - **P5 the band gate** — change `ui.add_enabled_ui(working.show_bands, …)` to
//!   `add_enabled_ui(true, …)`.
//! - **P6 the fill gate** — change the inner `ui.add_enabled_ui(working.show_ob_os_fill, …)` to
//!   `add_enabled_ui(true, …)`.
//! - **P7 geometry reach** — paint a `NaN`-centred circle at the top of `draw`. This one is the
//!   reach check rather than a dialog pin: it is what proves `dialog_harness`'s `settle` carries
//!   the shared geometry invariant onto a real chart frame.

mod common;
mod dialog_harness;

use egui::accesskit::{Role, Toggled};
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_chart::indicators::{LineDash, Source};
use vike_chart::options::IndicatorTab;

use dialog_harness::{
    combo_texts, count, dialog_nodes, dialog_titles, fixture, harness, label_texts,
    label_texts_after, last_before, nameless_checkboxes_after, one, settle, spin_values, toggled,
    Fixture,
};

/// The two indicators every pose draws from, and the titles `indicator_settings_dialog` builds for
/// them (`format!("{} settings", a.spec.pretty)`).
///
/// `rsi` is attached FIRST and `macd` second so "open the second one" is a question with a wrong
/// answer available; they also split the surface cleanly — macd carries three plots and one band,
/// rsi one plot and three bands, so each test poses the indicator whose shape it needs.
const RSI_UID: u64 = 7;
const MACD_UID: u64 = 9;
const RSI_TITLE: &str = "RSI settings";
const MACD_TITLE: &str = "MACD settings";

/// `Vec<String>` from literals — the shape every ordered tree read compares against.
fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// The Style tab's Plots grid, read as `(plot name, shown, stroke width)` in row order.
///
/// Read positionally because that is the only tie there is: the per-plot show `CheckBox` is
/// `ui.checkbox(vis, "")` and carries an EMPTY label, the name is the row's only `Role::Label`, and
/// egui's `DragValue` files under `Role::SpinButton` carrying no label of its own. The walk stops
/// at the `BANDS` heading (`crates/vike-chart/src/chart/dialogs.rs`'s `section` uppercases its
/// text) because the band rows below open with the SAME nameless checkbox and a `Role::Label`
/// beside it, so they keep feeding this state machine after the last plot. ⚠ Today they emit no
/// `DragValue`, and a row is only pushed on one — so the fence is what keeps a future band-side
/// spinner from reading as a fourth plot, not a repair of something already wrong.
fn plot_rows(nodes: &[Node<'_>]) -> Vec<(String, bool, f64)> {
    let mut rows = Vec::new();
    let mut shown: Option<bool> = None;
    let mut name: Option<String> = None;
    for node in nodes {
        let a = node.accesskit_node();
        match a.role() {
            Role::CheckBox if a.label().as_deref() == Some("") => {
                shown = Some(a.toggled() == Some(Toggled::True));
            }
            Role::Label => {
                let Some(text) = a.value() else { continue };
                if text == "BANDS" {
                    break;
                }
                name = Some(text);
            }
            Role::SpinButton => {
                if let (Some(s), Some(n), Some(w)) = (shown.take(), name.take(), a.numeric_value())
                {
                    rows.push((n, s, w));
                }
            }
            _ => {}
        }
    }
    rows
}

/// The RSI dialog open on its Style tab, with the two band booleans posed on the LIVE `Active` so
/// the dialog's own seed block carries them into the working copy.
fn rsi_style(show_bands: bool, show_fill: bool) -> Harness<'static, Fixture> {
    let mut f = fixture().with_indicator(RSI_UID, "rsi", &[]);
    {
        let a = f.indicator_mut(RSI_UID);
        a.show_bands = show_bands;
        a.show_ob_os_fill = show_fill;
    }
    f.indicator_dialog.open_uid = Some(RSI_UID);
    f.indicator_dialog.tab = IndicatorTab::Style;
    let mut h = harness(f);
    settle(&mut h);
    h
}

/// The dialog names the indicator it was asked for, seeds that indicator's LIVE parameters, and
/// renders nothing at all for a uid the chart no longer carries.
///
/// Reddens on `indicator_settings_dialog` resolving `open_uid` by anything but identity (the title
/// names RSI, whose params are a single `length`), on the seed reading `spec.params` defaults
/// instead of the running `Active` (12/26/9 rather than the posed 5/40/3), and on the
/// target-vanished arm rendering a window over a target that is gone.
#[test]
fn the_dialog_opens_for_the_uid_it_was_given_and_not_at_all_for_a_vanished_one() {
    let mut f = fixture().with_indicator(RSI_UID, "rsi", &[]).with_indicator(
        MACD_UID,
        "macd",
        &[5.0, 40.0, 3.0],
    );
    f.indicator_dialog.open_uid = Some(MACD_UID);
    let mut h = harness(f);
    settle(&mut h);

    assert_eq!(
        dialog_titles(&h),
        owned(&[MACD_TITLE]),
        "asking for uid {MACD_UID} must open THAT indicator's settings, and only one window"
    );
    let nodes = dialog_nodes(&h, MACD_TITLE);
    assert!(
        dialog_nodes(&h, RSI_TITLE).is_empty(),
        "the other attached indicator's form must not be on screen"
    );
    assert_eq!(
        label_texts(&nodes),
        owned(&["Source", "fast_length", "slow_length", "signal_length"]),
        "the Inputs tab must name MACD's own parameter surface, in spec order"
    );
    assert_eq!(
        spin_values(&nodes),
        vec![5.0, 40.0, 3.0],
        "the form must seed from the RUNNING indicator's parameters, not the registry defaults"
    );

    // The target vanished while the dialog was open (removed from `indicators`): the dialog must
    // close silently rather than leave a stale form editing an order-of-magnitude-wrong uid.
    let mut gone = fixture().with_indicator(RSI_UID, "rsi", &[]);
    gone.indicator_dialog.open_uid = Some(4242);
    let mut h = harness(gone);
    settle(&mut h);
    assert!(
        dialog_titles(&h).is_empty(),
        "a uid no indicator carries must render no settings window, got {:?}",
        dialog_titles(&h)
    );
    assert!(
        h.state().indicator_dialog.open_uid.is_none(),
        "the vanished-target arm must also clear the request"
    );
}

/// While the dialog is open the working copy is the truth: the spinners and the Source combo show
/// what the user has edited, not what the indicator is still computing.
///
/// Reddens on the Inputs tab reading the immutable `Active` — `params` 5/40/3 and `Source::Close`
/// — instead of `IndicatorEdit`. Both readings are tree-only: a `DragValue` publishes its number
/// as a node's numeric value, and a closed `egui::ComboBox` publishes `selected_text` as the node's
/// value, which is literally the text a screen reader speaks.
#[test]
fn the_inputs_tab_reads_the_working_copy_rather_than_the_committed_indicator() {
    let mut f = fixture().with_indicator(MACD_UID, "macd", &[5.0, 40.0, 3.0]);
    let working = f.working_over(MACD_UID, vec![7.0, 60.0, 4.0], Source::High);
    f.indicator_dialog.open_uid = Some(MACD_UID);
    f.indicator_dialog.snapshot = Some(working.clone());
    f.indicator_dialog.working = Some(working);
    let mut h = harness(f);
    settle(&mut h);

    // The pose is only discriminating while the two sides genuinely disagree — assert that first,
    // so a fixture that silently stopped applying either side cannot make this test vacuous.
    let committed = &h.state().indicators[0];
    assert_eq!(committed.params, vec![5.0, 40.0, 3.0]);
    assert_eq!(committed.source, Source::Close);

    let nodes = dialog_nodes(&h, MACD_TITLE);
    assert_eq!(
        spin_values(&nodes),
        vec![7.0, 60.0, 4.0],
        "the parameter spinners must show the working copy's edits"
    );
    assert_eq!(
        count(&nodes, Role::ComboBox),
        1,
        "the Inputs tab carries exactly one combo — the Source selector"
    );
    assert_eq!(
        combo_texts(&nodes),
        owned(&["High"]),
        "the Source combo must name the working copy's source, not the running indicator's"
    );
}

/// Every plot row in the Style tab pairs its own output name with its own visibility, stroke width
/// and dash — the three columns an off-by-one would silently redirect onto a neighbouring plot.
///
/// Reddens on any index slip between `a.outputs` (the names) and `working.lines` (the paint), in
/// the render loop or in the seed that fills `lines` from `outputs`. The three rows are posed with
/// three DISTINCT triples, so a rotation, a swap and a dropped row all fail rather than one of them
/// landing on a lucky match.
#[test]
fn each_plot_row_pairs_its_own_name_visibility_and_stroke_width() {
    let mut f = fixture().with_indicator(MACD_UID, "macd", &[]);
    {
        let a = f.indicator_mut(MACD_UID);
        assert_eq!(a.outputs.len(), 3, "MACD's three plots are what make this test discriminating");
        for (i, (width, visible, dash)) in [
            (1.0_f32, true, LineDash::Solid),
            (2.5, false, LineDash::Dashed),
            (4.0, true, LineDash::Dotted),
        ]
        .into_iter()
        .enumerate()
        {
            a.outputs[i].width = width;
            a.outputs[i].visible = visible;
            a.outputs[i].line_style = dash;
        }
    }
    f.indicator_dialog.open_uid = Some(MACD_UID);
    f.indicator_dialog.tab = IndicatorTab::Style;
    let mut h = harness(f);
    settle(&mut h);

    let nodes = dialog_nodes(&h, MACD_TITLE);
    assert_eq!(
        plot_rows(&nodes),
        vec![
            ("macd".to_string(), true, 1.0),
            ("signal".to_string(), false, 2.5),
            ("hist".to_string(), true, 4.0),
        ],
        "each row must carry ITS OWN plot's name, show state and stroke width"
    );
    assert_eq!(
        combo_texts(&nodes),
        owned(&["Solid", "Dashed", "Dotted"]),
        "the dash pickers are a fourth column of the same rows and must route the same way"
    );
}

/// `Show levels` gates the per-level band rows AND the overbought/oversold toggle nested inside it;
/// that toggle gates only its own two fill swatches.
///
/// Reddens on either `ui.add_enabled_ui` in the Style tab's Bands block being dropped or inverted.
/// The three poses walk the nesting: with levels off, the inner toggle is unreachable even though
/// its own flag is set — the fact that separates a nested gate from two independent ones.
///
/// The fill swatches carry no label of their own (`ui.color_edit_button_srgb` files a bare
/// `Role::ColorWell`), so each is addressed by the `ui.weak` word printed beside it, which is how a
/// user reads that row too.
#[test]
fn the_band_gates_disable_exactly_what_they_gate() {
    // Levels OFF, fill flag ON: the fill toggle must be unreachable through the outer gate.
    let closed = rsi_style(false, true);
    let nodes = dialog_nodes(&closed, RSI_TITLE);
    assert_eq!(
        label_texts_after(&nodes, "BANDS"),
        owned(&["30", "50", "70", "overbought", "oversold"]),
        "RSI's three reference levels and the two fill words are the Bands block's whole text"
    );
    assert_eq!(
        count(&nodes, Role::CheckBox),
        6,
        "one plot row + Show levels + three band rows + the fill toggle"
    );

    let master = one(&nodes, Role::CheckBox, "Show levels");
    assert_eq!(toggled(master), Some(Toggled::False));
    assert!(
        !master.accesskit_node().is_disabled(),
        "the master toggle is the gate — it can never be gated by itself"
    );
    for (i, level) in nameless_checkboxes_after(&nodes, "BANDS").iter().enumerate() {
        assert!(
            level.accesskit_node().is_disabled(),
            "band row {i} must not be operable while Show levels is off"
        );
    }
    let fill = one(&nodes, Role::CheckBox, "Overbought/oversold fill");
    assert_eq!(toggled(fill), Some(Toggled::True), "the pose sets this flag, so it must read set");
    assert!(
        fill.accesskit_node().is_disabled(),
        "the fill toggle sits INSIDE the levels gate — a set flag must not make it reachable"
    );

    // Levels ON, fill OFF: the band rows and the fill toggle come alive, its swatches do not.
    let inner = rsi_style(true, false);
    let nodes = dialog_nodes(&inner, RSI_TITLE);
    let levels = nameless_checkboxes_after(&nodes, "BANDS");
    assert_eq!(levels.len(), 3, "RSI has three reference levels");
    for (i, level) in levels.iter().enumerate() {
        assert!(
            !level.accesskit_node().is_disabled(),
            "band row {i} must be operable once Show levels is on"
        );
    }
    let fill = one(&nodes, Role::CheckBox, "Overbought/oversold fill");
    assert_eq!(toggled(fill), Some(Toggled::False));
    assert!(
        !fill.accesskit_node().is_disabled(),
        "the fill toggle is reachable once levels are on"
    );
    for word in ["overbought", "oversold"] {
        assert!(
            last_before(&nodes, Role::ColorWell, word).accesskit_node().is_disabled(),
            "the {word} swatch must not be operable while the fill toggle is off"
        );
    }

    // Levels ON, fill ON: the two swatches are finally editable.
    let open = rsi_style(true, true);
    let nodes = dialog_nodes(&open, RSI_TITLE);
    for word in ["overbought", "oversold"] {
        assert!(
            !last_before(&nodes, Role::ColorWell, word).accesskit_node().is_disabled(),
            "the {word} swatch must be operable once the fill toggle is on"
        );
    }
}
