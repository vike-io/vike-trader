//! The shared `egui_kittest` harness behind vike-chart's two dialog accessibility suites
//! (`crates/vike-chart/tests/chart_settings_a11y.rs` and
//! `crates/vike-chart/tests/indicator_settings_a11y.rs`): one [`Fixture`] over the REAL
//! `crates/vike-chart/src/chart/mod.rs`'s `draw`, the window-scoped tree walk both suites read
//! through, and the handful of accesskit readers they share.
//!
//! It exists for the same reason `crates/vike-chart/tests/common/mod.rs` does — an integration
//! test cannot `use` another integration test, each `tests/*.rs` being its own crate, so a law
//! spelled in both suites would be spelled twice. It is a SECOND module rather than more items in
//! `common` because `common` is the `egui::Context::run_ui` harness that
//! `crates/vike-chart/tests/draw_characterization.rs` and
//! `crates/vike-chart/tests/tessellation_goldens.rs` drive; folding a kittest `Harness` into it
//! would put `egui_kittest` in those two binaries' build for nothing. The FIXTURES stay shared:
//! this module reaches into `crate::common` for the bars, the UTC-pinned `ChartState` and the font
//! binding, so both suites must declare `mod common;` beside `mod dialog_harness;`.
//!
//! # Why the whole chart renders to test a dialog
//!
//! `settings_dialog_body` and `indicator_settings_dialog` are `pub(crate)`
//! (`crates/vike-chart/src/chart/dialogs.rs`), so `draw` is the only door to either. Posing
//! `ChartInputs`'s `settings` / `indicator_dialog` open and rendering one full chart frame is
//! therefore not overkill — it is the only way the dialog code runs at all, and it has the side
//! benefit that the dialogs are exercised exactly as `vike-app` reaches them.
//!
//! # ⚠ The first pass draws NOTHING, and that is load-bearing
//!
//! `draw` lays out labels in the custom `"light"` / `"semibold"` font families `vike-app` registers
//! at startup (`crates/vike-chart/src/chart/overlay.rs` and
//! `crates/vike-chart/src/chart/subpanes.rs` name them inline), and epaint PANICS on first use of
//! an unbound `FontFamily::Name`. `crates/vike-chart/tests/common/mod.rs`'s
//! `bind_chart_font_families` is the cure every harness in this crate already uses — but it is
//! `egui::Context::set_fonts`, which takes effect at the START OF THE NEXT PASS, and
//! `egui_kittest`'s `HarnessBuilder` runs its first pass INSIDE the constructor, before a test can
//! touch the context. Drawing on that pass would panic. So [`harness`]'s closure binds the fonts
//! and returns on its first call; the constructor's own settle loop always steps at least once
//! more, so the chart's first real frame is the one after the fonts land. Every test then calls
//! [`settle`], which steps again.
//!
//! # ⚠ Clicking a dialog control summons the chart's hover-gated overlays
//!
//! `egui_kittest`'s `Node::click` sends a `PointerMoved` to the node's centre first, and
//! `crates/vike-chart/src/chart/controls.rs`'s `nav_row` and `scale_row` gate their buttons on a
//! RAW `frame.contains(pointer)` test with no occlusion check — so a click inside a dialog
//! floating over the price pane makes the `− + ‹ › ⟳ ⚙ ⛶` and `Log / % / Auto` rows appear in the
//! tree. One of those is a `Role::Button` labelled `Auto`, and the chart-settings Symbol panel
//! carries a `Role::CheckBox` labelled `Auto`. Both hazards are answered the same way: every read
//! below is scoped to ONE dialog's subtree by [`dialog_nodes`] and pins a ROLE, never a bare label.
//! Scoping is sound rather than lucky — `egui::Window` files its title as a `Role::Window` node
//! BEFORE it builds its content `Ui`, and that content `Ui` declares the window node as its
//! accesskit parent, so the dialog's whole widget set hangs beneath it.

// Each of the two suites uses a SUBSET of this module and compiles it separately, so an item only
// one of them calls is dead code in the other — and `-D warnings` would fail on a helper that is
// used, just not by the binary being compiled. Same standard `tests/` idiom as `common`; it
// suppresses nothing about the crate under test.
#![allow(dead_code)]

use egui::accesskit::{Role, Toggled};
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use indexmap::IndexMap;
use vike_chart::indicators::Source;
use vike_chart::scale::ScaleAssign;
use vike_chart::{
    Active, ChartInputs, ChartOptions, ChartState, ChartStyle, FollowLive, IndicatorDialog,
    IndicatorEdit, PaneFractions, PaneKey, ScaleMode, SettingsDialog,
};

use crate::common;

/// Bars behind every pose. Enough that the price pane draws a real series (an empty chart is a
/// degenerate frame the geometry assertion in [`settle`] would pass for the wrong reason), and few
/// enough that the per-frame fold stays cheap across the poses these suites run.
const BARS: usize = 120;

/// 1200×1000 logical px — comfortable headroom for the tallest pose, deliberately NOT a measured
/// minimum.
///
/// The chart-settings window asks for `default_width(470)` and is `resizable(false)`, so it
/// AUTO-SIZES: its height is whatever its tallest section stacks to (Canvas — background, grid,
/// crosshair, series colours, margins), and nothing in this file knows that number. Two reasons
/// not to trim toward it. A cramped harness clips controls and turns a real assertion into a "not
/// found" panic — the trap `crates/vike-studio/tests/studio_shell_render.rs`'s `harness_over`
/// records. And a window egui had to CONSTRAIN to the screen would sit somewhere these poses did
/// not choose, which is a fixture that drifts with the panel's content rather than with the code
/// under test. (A height that merely overflows would not, on its own, hide anything: `egui::Ui`'s
/// clip rect gates PAINT, and every widget here files its accesskit node before asking whether it
/// is visible. That is a reason the failure mode is mild, not a reason to court it.)
const SCREEN: egui::Vec2 = egui::Vec2 { x: 1200.0, y: 1000.0 };

/// `Harness::run` PANICS past `max_steps`, whose default is 4. Raising it costs nothing when the
/// UI settles — the loop breaks on the first frame that requests no immediate repaint — and
/// removes a flake vector; if a chart frame ever repaints forever the failure names the causes.
const MAX_STEPS: u64 = 64;

/// Everything one posed chart frame needs, owned across frames the way `vike-app`'s `WinState`
/// owns it. The three dialog-facing fields are `pub` because posing them IS the test: a suite sets
/// `settings.open` / `settings.working` or `indicator_dialog.open_uid` / `working`, renders, and
/// reads the tree.
pub struct Fixture {
    /// `false` until the first pass has registered the chart's font families — see the module doc.
    fonts_bound: bool,
    /// The window's COMMITTED options. Deliberately separate from `settings.working` so a suite
    /// can pose the two DIFFERENTLY and ask which side a widget actually read.
    pub options: ChartOptions,
    pub settings: SettingsDialog,
    pub indicator_dialog: IndicatorDialog,
    pub indicators: Vec<Active>,
    state: ChartState,
    follow: FollowLive,
    panes: PaneFractions,
    study_pane_of: IndexMap<u64, PaneKey>,
    series_pane_of: IndexMap<String, PaneKey>,
    series_scale: IndexMap<String, ScaleAssign>,
}

/// A candle chart with both dialogs CLOSED and no indicators — the baseline every pose starts from.
///
/// A free function rather than `Fixture::new`, because a `new` with no `Default` beside it is a
/// clippy `-D warnings` failure and a `Default` here would claim a meaning this type does not have
/// (the state it wraps is a fully warmed `ChartState`, not a zero value).
pub fn fixture() -> Fixture {
    Fixture {
        fonts_bound: false,
        options: ChartOptions::default(),
        settings: SettingsDialog::default(),
        indicator_dialog: IndicatorDialog::default(),
        indicators: Vec::new(),
        state: common::make_state(common::wave_bars(BARS)),
        follow: FollowLive::default(),
        panes: PaneFractions::default(),
        study_pane_of: IndexMap::new(),
        series_pane_of: IndexMap::new(),
        series_scale: IndexMap::new(),
    }
}

impl Fixture {
    /// Attach one live `crates/vike-chart/src/indicators.rs`'s `Active` at `uid`, folded over this
    /// fixture's own bars. A non-empty `params` runs it through `Active::set_params`, which is how
    /// a pose gets an indicator whose live values differ from its registry defaults — the only way
    /// to tell "the dialog seeded from the running indicator" apart from "the dialog printed the
    /// catalogue defaults".
    ///
    /// The indicator is NOT given a sub-pane, and that is what keeps the chart itself out of the
    /// `Role::Label` population: `crates/vike-chart/src/chart/subpanes.rs`'s `draw_pane_overlay` is
    /// the one place a chart frame calls `ui.label` at all (everything else on the canvas is
    /// painted through `egui::Painter`, which files no accesskit node), and it is reached ONLY per
    /// pane. `ChartInputs`'s `sub_panes` stays `&[]` here, and `resolve_pane_layout` FILTERS that
    /// authored list rather than synthesizing from anything else — so an empty one yields the price
    /// pane alone.
    ///
    /// ⚠ That covers the VOLUME pane too, and it is worth saying because the obvious reading is
    /// wrong: `ChartOptions::default()`'s `show_volume` is `true`, and a reader who stops there
    /// concludes the volume legend ("Volume", through the same `draw_pane_overlay`) is on screen.
    /// It is not — `show_volume` only decides whether a `PaneKey::Volume` ALREADY IN `sub_panes`
    /// survives the filter, and there is none to survive.
    ///
    /// Even so, none of that is what makes the reads sound: every `Role::Label` either suite reads
    /// goes through [`dialog_nodes`], which is scoped to one window's subtree. This paragraph
    /// buys a clean UNSCOPED read (`all_nodes`) for a future assertion; it is not load-bearing for
    /// anything shipped.
    pub fn with_indicator(mut self, uid: u64, name: &str, params: &[f64]) -> Self {
        let spec = vike_chart::indicators::get_any(name)
            .unwrap_or_else(|| panic!("{name:?} must be a registered indicator"));
        let mut active = Active::new(uid, spec, &self.state.bars);
        if !params.is_empty() {
            active.set_params(params.to_vec(), &self.state.bars);
        }
        self.indicators.push(active);
        self
    }

    /// The `Active` this fixture attached at `uid`, for a pose that edits its paint state.
    pub fn indicator_mut(&mut self, uid: u64) -> &mut Active {
        self.indicators
            .iter_mut()
            .find(|a| a.uid == uid)
            .unwrap_or_else(|| panic!("no indicator attached at uid {uid}"))
    }

    /// The edit copy `indicator_settings_dialog` would seed for the `Active` at `uid`, with
    /// `params` and `source` REPLACED by values that indicator does not hold.
    ///
    /// Pre-seeding `IndicatorDialog`'s `working` skips the dialog's own seed block, which is the
    /// point: with the working copy and the committed `Active` deliberately disagreeing, every
    /// widget's reading answers "which side did you read" — the question the live-preview contract
    /// turns on and that no pure test can pose.
    pub fn working_over(&self, uid: u64, params: Vec<f64>, source: Source) -> IndicatorEdit {
        let a = self
            .indicators
            .iter()
            .find(|a| a.uid == uid)
            .unwrap_or_else(|| panic!("no indicator attached at uid {uid}"));
        IndicatorEdit {
            params,
            source,
            lines: a.outputs.iter().map(|o| (o.color, o.width, o.visible, o.line_style)).collect(),
            show_bands: a.show_bands,
            bands: a.bands.iter().map(|b| (b.value, b.color, b.show)).collect(),
            show_ob_os_fill: a.show_ob_os_fill,
            ob_fill: a.ob_fill,
            os_fill: a.os_fill,
            visible: a.visible,
        }
    }
}

/// Wrap a posed fixture in a harness over the real `draw`.
///
/// Every `ChartInputs` field outside the two dialogs is the byte-identical default each one
/// documents (`&[]` studies/overlays/panes, no footprint, no sync, no GPU seam), so the frame under
/// test is a plain candle chart with a dialog floating over it.
pub fn harness(posed: Fixture) -> Harness<'static, Fixture> {
    Harness::builder().with_size(SCREEN).with_max_steps(MAX_STEPS).build_ui_state(
        |ui, f: &mut Fixture| {
            if !f.fonts_bound {
                // See the module doc: this pass exists ONLY to register the font families, because
                // `set_fonts` lands at the start of the NEXT pass and drawing here would panic.
                common::bind_chart_font_families(ui.ctx());
                f.fonts_bound = true;
                return;
            }
            let _actions = vike_chart::draw(
                ui,
                ChartInputs {
                    state: &f.state,
                    style: ChartStyle::Candles,
                    nav: None,
                    indicators: &f.indicators[..],
                    studies: &[],
                    follow: &mut f.follow,
                    options: &f.options,
                    settings: &mut f.settings,
                    indicator_dialog: &mut f.indicator_dialog,
                    scale: ScaleMode::Linear,
                    invert: false,
                    panes: &mut f.panes,
                    sync: None,
                    footprint: None,
                    footprint_gen: 0,
                    cvd_on: false,
                    profile_on: false,
                    of_tick_size: 0.0,
                    sub_panes: &[],
                    study_pane_of: &f.study_pane_of,
                    overlays: &[],
                    series_panes: &[],
                    series_pane_of: &f.series_pane_of,
                    series_scale: &f.series_scale,
                    gpu_candles: None,
                },
            );
        },
        posed,
    )
}

/// Run until repaints settle, then assert the frame the chart painted is geometrically paintable.
///
/// Every interaction in both suites goes through here, which makes each dialog pose a geometry
/// test as well as an accessibility one — the same free upgrade
/// `crates/vike-chart/tests/common/mod.rs`'s `run_full` gives that crate's other scenarios.
///
/// ⚠ No `textures_delta` clear precedes the assertion, reversing what
/// `crates/vike-ui-theme/src/frame_sanity.rs`'s `assert_frame_sane` demands of a raw `run_ui`
/// harness. It is safe here for a reason rather than by luck: `egui_kittest`'s `LazyRenderer` takes
/// each frame's deltas BEFORE the harness stores the output, so `Harness::output` already holds an
/// empty-delta frame and no unapplied delta can double-panic while this assertion unwinds.
pub fn settle(h: &mut Harness<'static, Fixture>) {
    h.run();
    vike_ui_theme::frame_sanity::assert_frame_sane(h.output());
}

/// Every dialog title currently on screen, in tree order.
///
/// `egui::Window` files its title as the `label` of a `Role::Window` node, and `Window` is the only
/// caller of the `Area` hook that produces one — so this is the whole answer to "which dialogs are
/// open", and an EMPTY result is a positive statement that none are. The accesskit root is itself a
/// `Role::Window`, but it carries no label and is not among its own descendants.
pub fn dialog_titles(h: &Harness<'static, Fixture>) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Window)
        .filter_map(|n| n.accesskit_node().label())
        .collect()
}

/// EVERY node the frame produced, in tree order — the chart's own widgets included.
///
/// The scoped [`dialog_nodes`] is what almost every assertion wants; this one exists for the
/// opposite claim, "that control is nowhere on screen". Asserting an absence inside a dialog's
/// subtree while no dialog is open is circular — the subtree is empty for a different reason.
pub fn all_nodes<'t>(h: &'t Harness<'static, Fixture>) -> Vec<Node<'t>> {
    h.root().children_recursive().collect()
}

/// One dialog's whole widget set, in tree order, or an empty vec when no window carries `title`.
///
/// Tree order is creation order — egui pushes a node onto its parent the first time the widget is
/// built — so the returned slice reads a grid ROW BY ROW, which is what lets the readers below pair
/// a label with the control beside it.
pub fn dialog_nodes<'t>(h: &'t Harness<'static, Fixture>, title: &str) -> Vec<Node<'t>> {
    h.root()
        .children_recursive()
        .find(|n| {
            let a = n.accesskit_node();
            a.role() == Role::Window && a.label().as_deref() == Some(title)
        })
        .map(|window| window.children_recursive().collect())
        .unwrap_or_default()
}

/// The one node with this role and label. Panics unless there is EXACTLY one — a control that
/// vanished must fail loudly, not read as "disabled" or "unchecked".
pub fn one<'t>(nodes: &[Node<'t>], role: Role, label: &str) -> Node<'t> {
    let found: Vec<Node<'t>> = nodes
        .iter()
        .filter(|n| {
            let a = n.accesskit_node();
            a.role() == role && a.label().as_deref() == Some(label)
        })
        .copied()
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one {role:?} labelled {label:?}, found {}",
        found.len()
    );
    found[0]
}

/// Whether a control is present at all, without asserting how many.
pub fn present(nodes: &[Node<'_>], role: Role, label: &str) -> bool {
    nodes.iter().any(|n| {
        let a = n.accesskit_node();
        a.role() == role && a.label().as_deref() == Some(label)
    })
}

/// The selection state a control reports. `Role::CheckBox` and the `Role::Button` egui files a
/// `SelectableLabel` under both carry it, which is what makes a tab rail's highlight readable.
pub fn toggled(n: Node<'_>) -> Option<Toggled> {
    n.accesskit_node().toggled()
}

/// Every `Role::Label`'s text, in tree order.
///
/// ⚠ Read from `value`, not `label`: `Role::Label` is the one role whose text egui files under the
/// node's VALUE (`fill_accesskit_node_from_widget_info` special-cases it), so a `label`-based read
/// of a plain label silently answers `None`.
pub fn label_texts(nodes: &[Node<'_>]) -> Vec<String> {
    nodes
        .iter()
        .filter(|n| n.accesskit_node().role() == Role::Label)
        .filter_map(|n| n.accesskit_node().value())
        .collect()
}

/// Every `Role::SpinButton`'s numeric value, in tree order — egui's `DragValue` files here
/// carrying NO label at all (`WidgetInfo::drag_value` sets none and `dialogs.rs` adds no
/// `labelled_by`), so position is the only thing tying one to the row it sits in.
pub fn spin_values(nodes: &[Node<'_>]) -> Vec<f64> {
    nodes
        .iter()
        .filter(|n| n.accesskit_node().role() == Role::SpinButton)
        .filter_map(|n| n.accesskit_node().numeric_value())
        .collect()
}

/// Every `Role::ComboBox`'s closed-state text, in tree order — egui files a combo's `selected_text`
/// as the node's value, so this is literally what the control would read aloud.
pub fn combo_texts(nodes: &[Node<'_>]) -> Vec<String> {
    nodes
        .iter()
        .filter(|n| n.accesskit_node().role() == Role::ComboBox)
        .filter_map(|n| n.accesskit_node().value())
        .collect()
}

/// How many nodes carry `role`.
pub fn count(nodes: &[Node<'_>], role: Role) -> usize {
    nodes.iter().filter(|n| n.accesskit_node().role() == role).count()
}

/// How many `role` nodes appear BEFORE the `Role::Label` reading `marker`.
///
/// `crates/vike-chart/src/chart/dialogs.rs`'s `section` renders its heading uppercased, so a
/// section header is a stable, human-visible fence between two groups of otherwise identical
/// controls — the only way to count one group's colour wells without counting the next group's.
pub fn count_before(nodes: &[Node<'_>], role: Role, marker: &str) -> usize {
    let mut n = 0;
    for node in nodes {
        let a = node.accesskit_node();
        if a.role() == Role::Label && a.value().as_deref() == Some(marker) {
            break;
        }
        if a.role() == role {
            n += 1;
        }
    }
    n
}

/// The last `role` node emitted before the `Role::Label` reading `marker`.
///
/// The overbought/oversold fill swatches are laid out `[well]["overbought"][well]["oversold"]` and
/// carry no label of their own, so the word beside a well is the only thing that names it — which
/// is exactly how a user reads that row.
pub fn last_before<'t>(nodes: &[Node<'t>], role: Role, marker: &str) -> Node<'t> {
    let mut last: Option<Node<'t>> = None;
    for node in nodes {
        let a = node.accesskit_node();
        if a.role() == Role::Label && a.value().as_deref() == Some(marker) {
            return last.unwrap_or_else(|| panic!("no {role:?} precedes the label {marker:?}"));
        }
        if a.role() == role {
            last = Some(*node);
        }
    }
    panic!("no label reading {marker:?} is on screen");
}

/// Every `Role::CheckBox` carrying an EMPTY label that appears after the `Role::Label` reading
/// `marker`, in tree order.
///
/// The per-row show/hide toggles in both `dialogs.rs` grids are `ui.checkbox(flag, "")` — nameless
/// by design, since the row's own label names it — so they can only be addressed positionally, and
/// a section header is what separates one grid's from the next one's.
pub fn nameless_checkboxes_after<'t>(nodes: &[Node<'t>], marker: &str) -> Vec<Node<'t>> {
    let mut seen = false;
    let mut out = Vec::new();
    for node in nodes {
        let a = node.accesskit_node();
        if a.role() == Role::Label && a.value().as_deref() == Some(marker) {
            seen = true;
            continue;
        }
        if seen && a.role() == Role::CheckBox && a.label().as_deref() == Some("") {
            out.push(*node);
        }
    }
    out
}

/// Every `Role::Label`'s text after the one reading `marker`, in tree order.
pub fn label_texts_after(nodes: &[Node<'_>], marker: &str) -> Vec<String> {
    let mut seen = false;
    let mut out = Vec::new();
    for node in nodes {
        let a = node.accesskit_node();
        if a.role() != Role::Label {
            continue;
        }
        let Some(text) = a.value() else { continue };
        if seen {
            out.push(text);
        } else if text == marker {
            seen = true;
        }
    }
    out
}
