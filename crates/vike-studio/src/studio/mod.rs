//! `StudioState` — the Studio shell (Run toolbar | editor | Sweep/Validate | results). Holds the
//! store handle and wires the panes to worker-thread Run/Sweep/Walk-Forward. `poll()` folds each
//! worker's outcome in; none of the three ever runs on the egui update loop.
//!
//! ...and neither does the ⟳ Refresh CATALOG walk any more — see [`crate::backend::catalog`], which carries
//! why a catalog read is the same hazard as a backtest here (the handle may be an RPC store that
//! connects per read) and what the button deliberately still does not do. ⚠ That covers the
//! CATALOG walks, not every store read: `compute_indicator_preview`'s `load_bars` and the
//! constructor's one-shot walk are still synchronous, and each says so where it happens.
//!
//! NOTE: egui 0.35 collapsed `SidePanel`/`TopBottomPanel` into one `egui::Panel` type (constructors
//! `Panel::left`/`Panel::top`/...) and renamed `show_inside` -> `show` (the old names are `#[deprecated]`
//! shims that would fail this crate's `-D warnings` clippy gate), so this uses the resolved 0.35 API.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use vike_backtesthub_client::study::{
    WireRunArtifact, WireRunPage, WireStudyListing, WireStudyReport,
};
use vike_backtesthub_client::summary::{
    ParamscanSummaryWire, TradesPageWire, WalkforwardSummaryWire,
};

use self::workers::{
    StudioWorkerRustBuildRequest, StudioWorkerSdkRead, StudioWorkerStrategyFileSaveRequest,
};
use self::workspace::{
    StudioWorkspace, StudioWorkspaceSaver, load_workspace, workspace_read_path,
    write_workspace_file,
};
use crate::backend::backtesthub::{StudioRunBackend, StudioRunEngine, StudioRunViewOutcome};
use crate::panes::chat::{StudioChatApiKeys, StudioChatOutcome, StudioChatPane, summary_of};
use crate::panes::data_browser::StudioDataBrowserPane;
use crate::panes::editor::StudioEditorPane;
use crate::panes::indicators::StudioIndicatorsPane;
use crate::panes::param_scan::{StudioParamRow, StudioTestConditions};
use crate::panes::research::{StudioResearchAction, StudioResearchPane};
use crate::panes::results::{
    StudioResultsAction, StudioResultsCode, StudioResultsOrigin, StudioResultsTab,
    StudioResultsTradesPager, code_line_ui, results_ui, run_line_ui, walkforward_view,
};
use crate::panes::saved::{
    StudioSavedAction, StudioSavedPane, StudioSavedStrategy, StudioStrategySource, save_strategies,
    saved_compare_rows,
};
use crate::panes::sdk_source::{StudioSdkSourceAction, StudioSdkSourcePane};
#[cfg(test)]
use crate::panes::slice_picker::PICK_A_SLICE;
use crate::panes::slice_picker::StudioSlicePicker;
use vike_backtesthub_client::proto::WireStrategyFile;
use vike_backtesthub_client::sdk_source::{WireSdkFile, WireSdkListing};
use vike_data::HistoryStore;
use vike_data::TimestampRange;
use vike_model::run_request::replay::ReplayKind;
use vike_rhai::TEMPLATES;
use vike_studio_core::{RunError, StrategySpec, params_from_rows};
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::input::{self, ThemeInputField};
use vike_ui_theme::components::segmented::{self, ThemeSegment};
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, chip, section};
use vike_ui_theme::icons::{self, ThemeIcon};
use vike_ui_theme::maps::{self, ThemeMapRow};
use vike_ui_theme::type_scale::ThemeTextRole;

/// The saved-strategy list's filename, in the Studio's state directory (`state_dir.join(...)`, the
/// caller's: vike-desktop passes the project's `settings/state`).
const SAVED_STRATEGIES_FILE: &str = "studio_strategies.json";

/// The editor header's chip while the buffer is a Strategy file's RUST source — the status-neutral
/// stand-in for the Rhai check's `● compiles` / `● error · line N`, which cannot judge Rust (see
/// `StudioState::buffer_is_rhai`). It names what is actually known — the language, and that the
/// verdict is the Build's — rather than a verdict nothing here has reached.
pub const RUST_EDITOR_CHIP: &str = "● Rust · Build to check";

/// Run's refusal while a backtest is in flight.
const RUN_BUSY: &str = "A backtest is running — wait for it, or Cancel it.";
/// Run Sweep's refusal while a sweep is in flight.
const SWEEP_BUSY: &str = "A param scan is running.";
/// Run Sweep's refusal while no ticked row reads to a value — the words its hover text used to carry,
/// where a disabled button never showed them.
const SEED_A_GRID: &str =
    "Tick a parameter to scan and give it a range (5..30 step 5) or a list (5, 10, 20).";
/// Run param scan's refusal above the points one scan may run
/// (`vike_backtesthub_client::summary::PARAMSCAN_POINTS_MAX`, the server's cap too).
const SCAN_TOO_LARGE: &str =
    "More combinations than one scan may run: narrow a range or widen its step.";
/// The scan's and the walk-forward's refusal while an unticked row's Value reads to more than one value: it
/// would scan more points than the size line counts.
const HELD_VALUE_IS_ONE: &str = "A Value is one number: tick the row to scan a list or a range.";
/// Why the starting capital and the funding switch reach no run on the Named backend: its request has no field
/// for either.
const NAMED_BACKEND_CAPITAL: &str = "The Named backend's request carries no starting capital and no \
     funding switch: its Run and Compare all run at the server's defaults.";
/// Walk-Forward's refusal while one is in flight.
const WALKFORWARD_BUSY: &str = "A walk-forward is running.";
/// The Walk-Forward window count a session starts at — the four splits the button always ran
/// before its count became a control.
const WALKFORWARD_SPLITS_DEFAULT: usize = 4;
/// The window counts the Sweep pane's control offers: two is the fewest a walk-forward can mean
/// anything over; the upper end bounds the control, not the walk.
const WALKFORWARD_SPLITS_RANGE: std::ops::RangeInclusive<usize> = 2..=12;
/// Send's refusal with no provider key — also the chat pane's own line when it has none.
const NO_PROVIDER_KEY: &str = "No provider key found — run `vike-cli secrets set ANTHROPIC_API_KEY` (or CEREBRAS_API_KEY; \
     the value on stdin), then restart.";
/// Send's refusal while the copilot answers.
const COPILOT_BUSY: &str = "The copilot is still answering.";
/// Send's refusal on the Named backend: the copilot backtests every candidate on the backtest
/// server at the Write scope (Source's dial), which the Named backend does not use (decision 0064),
/// and it never falls back to computing in this process.
const COPILOT_NEEDS_COMPUTE_KEY: &str = "The copilot backtests its candidates on the server, which \
     only the Source backend does — switch the backend to Source.";
/// Send's refusal with nothing typed.
const NOTHING_TO_SEND: &str = "Describe a strategy first.";

/// The Study pane's central panel with no run yet — one merged sentence, since `state::view`'s
/// `ThemeEmptyPane::Empty` takes a single string where the old rendering split it across a title and a
/// caption label.
const NO_STUDY_RESULT: &str =
    "No study result yet — pick a study in the Research tool, then Run study.";

/// `empty_state`'s copy when the picker's series list is genuinely empty — not a depth-only store,
/// not a refused scan, just nothing backfilled yet. The existing sentence, verbatim.
const EMPTY_STORE: &str = "The data store is empty — backfill some history first\n\
     (vike-backfill, or the Data Manager's Backfill), then Refresh.";

/// Why every control that writes a RHAI script into the editor is disabled while the Strategy pane
/// is on `My strategy (Rust)` — the hover text of all four
/// ([`StudioState::rhai_writer_blocked_reason`] lists them and argues why they are refused rather
/// than made to switch the source).
pub(crate) const RHAI_WRITER_BLOCKED_IN_RUST: &str = "My strategy (Rust): the editor holds this \
     strategy's Rust source, and templates and the AI copilot write Rhai. Pick My strategy (Rhai) \
     in the Strategy tab first — nothing here overwrites Rust source with Rhai.";

/// A Rust Run's refusal while the editor does not hold the server's file as it was opened
/// (`StudioState::run_blocked_reason`'s third reason).
pub(crate) const RUST_RUN_NEEDS_THE_FILE: &str = "Run runs the FILE on the server, and the editor \
     does not hold that file as it was opened: open it (unedited) in the Strategy files list to \
     run it. Edits run once the server's file holds them (Studio cannot save into the server's \
     folder yet).";

/// The unsaved chip's hover text in Rhai mode.
const UNSAVED_TIP: &str = "Unsaved changes — Ctrl+S saves to the Saved list";

/// The unsaved chip's hover text in Native mode: a save captures the strategy file's path and the
/// param rows, never the parked editor buffer — see [`StudioState::file_saved`]'s doc for why the
/// dirty check itself had to change too, not just this text.
const UNSAVED_TIP_FILE: &str =
    "Unsaved — Ctrl+S saves this strategy file's path and its param rows to the Saved list.";

/// The unsaved chip's hover text in Strategy file mode while the buffer IS what the held sha was built
/// from, so a save names this code.
const UNSAVED_TIP_RUST_BUILT: &str = "Unsaved — Ctrl+S saves the file this buffer is built \
     as and the sha of its Build to the Saved list. The Rust source itself is not saved.";

/// The unsaved chip's hover text in Strategy file mode while the buffer is NOT what any held sha was built
/// from (no Build yet, an edit since, or a sha reloaded from a saved row).
const UNSAVED_TIP_RUST_UNBUILT: &str = "Unsaved — in My strategy (Rust) Ctrl+S saves the \
     file the buffer is built as and its last Build's sha, never the editor text, so it cannot \
     save these edits. Build first, then Ctrl+S.";

// The persisted-workspace filename lives in `crate::studio::workspace`, beside the two path helpers
// that resolve it (`workspace_read_path` / `workspace_write_path`) and the debounced writer
// (`StudioWorkspaceSaver`); it lands in the same `settings/state` as `SAVED_STRATEGIES_FILE`.

/// Which tool the single right-hand panel shows. The seven tools (Param scan, Strategy,
/// Data, Indicators, Saved, Research, AI Chat) share ONE tabbed `Panel::right` rather than each owning its own
/// always-open panel — simultaneous side panels crushed the editor + results to a sliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum StudioTab {
    #[default]
    Sweep,
    /// Strategy source: a strategy file of the backtest server + its params, or the Rhai or Rust
    /// editor buffer.
    Strategy,
    Data,
    Indicators,
    Saved,
    /// The user's own STUDIES, and the runs they left behind — see `crate::panes::research`.
    Research,
    Chat,
}

impl StudioTab {
    /// Tabs in display order (left→right).
    pub const ALL: [StudioTab; 7] = [
        StudioTab::Sweep,
        StudioTab::Strategy,
        StudioTab::Data,
        StudioTab::Indicators,
        StudioTab::Saved,
        StudioTab::Research,
        StudioTab::Chat,
    ];

    /// This tool's row of `ui-theme.toml`'s `studio_tab` map (`vike_ui_theme::maps::studio_tab`): its `word`
    /// is the tab's label and its `icon` the glyph on the rail, so which words and which glyph a tab wears
    /// is a line of the TOML and nothing here. Exhaustive: a tab without a row does not compile, and
    /// `every_tab_has_its_own_studio_tab_row_and_every_row_its_tab` holds the other direction.
    fn row(self) -> &'static ThemeMapRow {
        match self {
            StudioTab::Sweep => &maps::studio_tab::SWEEP,
            StudioTab::Strategy => &maps::studio_tab::STRATEGY,
            StudioTab::Data => &maps::studio_tab::DATA,
            StudioTab::Indicators => &maps::studio_tab::INDICATORS,
            StudioTab::Saved => &maps::studio_tab::SAVED,
            StudioTab::Research => &maps::studio_tab::RESEARCH,
            StudioTab::Chat => &maps::studio_tab::CHAT,
        }
    }

    /// The tab strip's button label for this tool: its row's `word`.
    pub fn label(self) -> &'static str {
        self.row().word.expect("a studio_tab row carries the tab's label")
    }

    /// The vertical icon rail's icon for this tool — one registry icon per tool, the same one its
    /// pane header shows; the rail shows `icon()` alone and names it `label()` — its accessible
    /// name and its hover tooltip (`icons::named`, see `ui()`). It is its row's `icon`.
    pub(crate) fn icon(self) -> ThemeIcon {
        self.row().icon().expect("a studio_tab row names an icon of the registry")
    }

    /// Parse a caller-supplied QA tab name (the capture hook — see `StudioState::new_with_qa`)
    /// into a tab. Lowercase tool names, `None` for anything else — never panics on garbage input.
    pub fn from_qa_str(s: &str) -> Option<StudioTab> {
        match s {
            "sweep" => Some(StudioTab::Sweep),
            "strategy" => Some(StudioTab::Strategy),
            "data" => Some(StudioTab::Data),
            "indicators" => Some(StudioTab::Indicators),
            "saved" => Some(StudioTab::Saved),
            "research" => Some(StudioTab::Research),
            "chat" => Some(StudioTab::Chat),
            _ => None,
        }
    }
}

/// What a headless capture asks the Studio's FIRST FRAME to start — the parsed
/// `VIKE_STUDIO_AUTORUN` value, supplied by the caller (see [`StudioState::new_with_qa`]).
///
/// ⚠ It used to be a `bool`, and the widening is the point rather than a tidy-up.
/// `.trader/shots/manifest.json`'s `grid-studio-sweep.png` wants the Sweep tab showing RESULTS,
/// and a boolean could only ever start [`Self::Run`] — one backtest — because
/// `StudioState::start_sweep` is private and reachable from its own button alone. A capture
/// cannot click. So the `state` clause that pose is written against ("results, not an empty form")
/// was unreachable by construction, which is what its `capture_gap` records.
///
/// An EXHAUSTIVE match everywhere it is consumed, not a `matches!`: a third autorun (walk-forward
/// is the obvious next one) must fail to compile until somebody decides what it starts, rather
/// than inheriting "do nothing" from a wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StudioAutorun {
    /// Nothing is started; the shell renders whatever the workspace restored. The default, and
    /// what an unset (or unrecognised) knob means.
    #[default]
    Off,
    /// One backtest — [`StudioState::start_run`]. The `"1"` spelling, kept EXACTLY as it was: the
    /// pre-existing read was `env::var(..).as_deref() == Ok("1")`, so every capture script and
    /// dev shell already setting `VIKE_STUDIO_AUTORUN=1` keeps its old behaviour to the byte.
    Run,
    /// One parameter sweep — seed the grid from the strategy's own declared params, widen each
    /// row to a ladder (`qa_sweep_ladder`), then `StudioState::start_sweep`.
    Sweep,
}

impl StudioAutorun {
    /// Parse the caller-supplied knob value. `None`, or anything unrecognised, is [`Self::Off`] —
    /// the same never-panic-on-garbage disposition [`StudioTab::from_qa_str`] has, and the same one
    /// the boolean read it replaces had (anything but `"1"` was false).
    #[must_use]
    pub fn from_qa_str(s: Option<&str>) -> Self {
        match s {
            Some("1") => Self::Run,
            Some("sweep") => Self::Sweep,
            _ => Self::Off,
        }
    }
}

/// Widen one seeded sweep-grid value into the candidate ladder a capture sweeps over — and the range a
/// parameter row's first tick fills in (`crate::studio::tool_panes`'s `param_row_ui`).
///
/// [`StudioState::seed_sweep_grid`] — the ▶ "Seed grid from params" button — writes each param's
/// DEFAULT and nothing else, so a grid seeded from it holds exactly one candidate per axis and
/// therefore exactly ONE combination. That is a legal sweep and a useless screenshot: a
/// single-row ranked table is indistinguishable from the backtest result already on screen, which
/// is the "results, not an empty form" failure wearing a different shirt.
///
/// So the hook types what a human would type into that box: the default, and a value either side
/// of it. Deliberately a PURE function with its own tests rather than an inline `format!` —
/// it is the one place this capture path invents a number, and inventing numbers is worth being
/// able to read and re-derive.
///
/// Returns the value UNCHANGED (a one-element ladder) when widening would produce a degenerate or
/// meaningless axis: a non-numeric row (the grid is numeric by construction, but the seed reads
/// user text), a non-finite one, or a zero — `0`'s ±50% neighbours are all `0`, which would
/// render three identical columns.
#[must_use]
pub(crate) fn qa_sweep_ladder(value: &str) -> String {
    let Ok(v) = value.trim().parse::<f64>() else { return value.to_string() };
    if !v.is_finite() || v == 0.0 {
        return value.to_string();
    }
    // ⚠ A WHOLE default stays whole, and this is not cosmetics. Almost every sweepable knob in
    // this app is a LOOKBACK, and a lookback reaches `vike_indicators` as a `usize` — so `2.5`
    // does not sweep a half-bar window, it silently becomes the same window as `2` while the grid
    // on screen claims otherwise. A fractional candidate is therefore either a lie about what ran
    // (integer knobs) or harmless (genuinely continuous ones), and rounding costs nothing in the
    // second case. `round` rather than truncation so the ladder stays centred on the default.
    let (lo, hi) =
        if v.fract() == 0.0 { ((v * 0.5).round(), (v * 1.5).round()) } else { (v * 0.5, v * 1.5) };
    // Rounding can COLLAPSE the ladder — `1` gives `1, 1, 2` — and a repeated candidate is a
    // duplicate backtest whose row appears twice in the ranked table. Dedup, then refuse to widen
    // at all if fewer than two distinct candidates survive: a one-value axis is what the caller
    // already had, so passing the value through unchanged is the honest answer.
    let mut rungs = vec![lo, v, hi];
    rungs.sort_by(f64::total_cmp);
    rungs.dedup();
    if rungs.len() < 2 {
        return value.to_string();
    }
    // Rendered as a seeded default reads (`5`, `2.5`), at nine decimals at most, so `0.1 * 1.5` is
    // `0.15` rather than `0.15000000000000002`.
    let text = |r: &f64| {
        let fixed = format!("{r:.9}");
        fixed.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    rungs.iter().map(text).collect::<Vec<_>>().join(", ")
}

/// WHICH producer's results the central panel is showing.
///
/// Two surfaces rather than one, and the split is R6's own: a backtest's numbers come from the
/// event-driven simulator and a study's do not, so `crate::panes::results::results_ui` (which renders
/// a run's summary) and `crate::panes::research::StudioResearchPane::study_result_ui` (which renders
/// the server's `vike_backtesthub_client::study::WireStudyReport`) each name their own producer
/// instead of sharing a column. Every dispatch sets this to its own view, so the panel shows the
/// result of the thing that was last asked for rather than obeying a precedence rule nobody can
/// predict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StudioCenterView {
    /// `crate::panes::results::results_ui` over the last Run / Sweep / Walk-Forward.
    #[default]
    Backtest,
    /// `crate::panes::research::StudioResearchPane::study_result_ui` over the last study run.
    Study,
    /// `crate::panes::sdk_source::StudioSdkSourcePane::file_ui`: one file of a strategy SDK, read-only. Not
    /// a producer's results: its "Back to results" returns to [`Self::Backtest`].
    SdkFile,
}

/// The "This Studio" segment's hover text (the Source backend): the Studio sends the strategy it runs to
/// the backtest server, which runs it.
const SOURCE_WHY: &str = "The Studio sends the strategy you have here (the editor's script, your \
     Rust strategy, or a server file by its path) and the backtest server runs it, with a parameter \
     scan and a walk-forward too. The server must hold this PC at the role control.";
/// The "The server" segment's hover text (the Named backend): the same server runs a strategy it already
/// holds, picked from its list by name, which read-only access may ask for.
const NAMED_WHY: &str = "The backtest server runs a strategy it already holds, picked from its \
     list by name: one strategy, one set of values, a window with both ends. No script, no \
     parameter scan, no walk-forward. Read-only access (the role observe) is enough.";

/// The backend selector's value. `StudioRunBackend` carries the address, so it cannot be the segmented
/// control's `Copy` value; this is WHICH one, and `switch_backend` carries the address across.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StudioRunBackendKind {
    Source,
    Named,
}

/// The tab that follows `t` in `StudioTab::ALL`'s display order, wrapping from the last tab back
/// to the first. Pure helper behind the Ctrl/Cmd+/ shortcut (`StudioState::ui`) — kept
/// free-standing so it's unit-testable without an egui context.
pub(crate) fn next_tab(t: StudioTab) -> StudioTab {
    let i = StudioTab::ALL.iter().position(|&x| x == t).unwrap_or(0);
    StudioTab::ALL[(i + 1) % StudioTab::ALL.len()]
}

/// A tool pane's heading — its icon and its title at the Title role, then the kit's strip rule. It
/// is the ONE text that says which pane the shared right panel shows
/// (`crates/vike-studio/tests/headless/studio_shell_render/panes.rs`'s `pane_title` reads it, which is why the
/// title stays a label of its own rather than joining the icon's). The icon is secondary text,
/// never the accent: the accent marks shapes only (design system spec §2), and the rail's accent
/// edge already says which pane is open.
pub(crate) fn pane_header(ui: &mut egui::Ui, icon: ThemeIcon, title: &str) {
    let tk = ThemeTokens::of(ui.ctx());
    let px = tk.text.px(ThemeTextRole::Title);
    ui.horizontal(|ui| {
        ui.label(icon.rich().size(px).color(tk.theme.text_secondary));
        ui.label(egui::RichText::new(title).size(px).color(tk.theme.text));
    });
    section::strip_rule(ui);
}

/// **The two cost params every strategy file has** (the owner's ruling of 2026-11-18: a run's commission and
/// slippage are strategy params, `vike_backtest::run::costs` on the server), each with the hint its field shows.
/// The Param scan pane sets them ONCE, as test conditions (`crate::panes::param_scan::StudioTestConditions`), and
/// `StudioState::current_entry` writes a value set there into a strategy file's params. Their units are params of
/// their own (`commission_unit`, `slippage_unit`, `slippage_tick`), and the server judges every word (decision
/// 0078's thin client computes nothing).
pub(crate) const STUDIO_COST_ROWS: [(&str, &str); 2] = [
    ("commission", "the strategy's own (0.05 is 0.05% of each fill)"),
    ("slippage", "the strategy's own (0.05 is 0.05% of the price)"),
];

/// The value of the row `key` in `rows` (a strategy file's params rows), blank when there is none.
pub(crate) fn cost_row_value(rows: &[(String, String)], key: &str) -> String {
    rows.iter().find(|(k, _)| k.trim() == key).map(|(_, v)| v.clone()).unwrap_or_default()
}

/// Set the row `key` of `rows` to `value`, in place: a blank value removes the row.
pub(crate) fn set_cost_row(rows: &mut Vec<(String, String)>, key: &str, value: &str) {
    let at = rows.iter().position(|(k, _)| k.trim() == key);
    match (at, value.trim().is_empty()) {
        (Some(i), true) => {
            rows.remove(i);
        }
        (Some(i), false) => rows[i].1 = value.to_string(),
        (None, false) => rows.push((key.to_string(), value.to_string())),
        (None, true) => {}
    }
}

pub struct StudioState {
    /// The history store, as the TRAIT handle (split-plane B12) — a local `HistoryStoreDataFusion` or an
    /// RPC-backed `HistoryStoreDatahub`; every pane reads through `HistoryStore` verbs only.
    pub store: Arc<dyn HistoryStore + Send + Sync>,
    /// Where the Studio's state files live (`studio_strategies.json` and the AI copilot ledger).
    /// Caller-supplied, so the BINARY resolves the directory: vike-desktop passes the project's
    /// `settings/state` (its local store root, which a thin client does not have, lost every Save).
    state_dir: PathBuf,
    /// True when [`Self::store`] is an RPC-backed REMOTE store. Set by the binary after
    /// construction (alongside seeding [`Self::backend`]), like the other runtime toggles.
    pub store_is_datahub: bool,
    pub editor: StudioEditorPane,
    pub(crate) picker: StudioSlicePicker,
    pub tab: StudioResultsTab,
    pub running: bool,
    /// The toolbar's Recompute: every Run, Sweep and Walk-Forward is RUN, even when the server saved an identical one it
    /// would answer from (decision 0078's amendment of 2026-11-20). Off when the Studio opens.
    pub recompute: bool,
    /// The last Run's summary (decision 0078's amendment of 2026-11-16: the whole answer stays on the
    /// server), or why it did not run.
    pub last: Option<StudioRunViewOutcome>,
    run_rx: Option<Receiver<StudioRunViewOutcome>>,
    /// The strategy parameters of the strategy on screen ([`Self::param_key`]'s): each row's value for a
    /// Run and its range for a scan (`crate::panes::param_scan`). Re-cut to what the strategy declares by
    /// [`Self::seed_sweep_grid`].
    pub grid: Vec<StudioParamRow>,
    /// Every other strategy's rows, by [`Self::param_key`], and the key [`Self::grid`] is of: a switch of
    /// strategy puts the rows away and brings the next one's back. Persisted with `grid`.
    param_grids: std::collections::BTreeMap<String, Vec<StudioParamRow>>,
    grid_key: String,
    /// The text [`Self::grid`] was last re-cut from: the editor buffer, or the strategy file's path.
    grid_seeded: Option<String>,
    /// The rows a re-cut dropped while a name was being edited, kept until the names settle
    /// (`crate::panes::param_scan::rows_for`). Not persisted.
    grid_dropped: Vec<StudioParamRow>,
    /// A strategy file's text on its way to re-cut [`Self::grid`] to its header, and why the last read
    /// failed.
    seed_file_rx: Option<Receiver<Result<String, String>>>,
    seed_failed: Option<String>,
    /// The test conditions a Run, a scan and a walk-forward share: starting capital and costs. Persisted.
    pub(crate) conditions: StudioTestConditions,
    /// When the scan in flight was sent, how long the last one took, and whether its answer (or
    /// refusal) is still to be scrolled into view in the pane.
    sweep_started: Option<std::time::Instant>,
    sweep_took: Option<std::time::Duration>,
    sweep_reveal: bool,
    sweep_rx: Option<Receiver<Result<ParamscanSummaryWire, RunError>>>,
    pub sweep_last: Option<Result<ParamscanSummaryWire, RunError>>,
    /// The param-scan point the user opened from the scan table (its summary, fetched on demand), or
    /// why it did not open; `None` shows the scan's best point.
    pub(crate) sweep_open: Option<StudioRunViewOutcome>,
    point_rx: Option<Receiver<StudioRunViewOutcome>>,
    walkforward_rx: Option<Receiver<Result<WalkforwardSummaryWire, RunError>>>,
    pub walkforward_last: Option<Result<WalkforwardSummaryWire, RunError>>,
    /// The Trades tab's page of the run on screen, fetched from the backtesthub on demand.
    pub(crate) trades: StudioResultsTradesPager,
    trades_rx: Option<Receiver<Result<TradesPageWire, RunError>>>,
    /// The last Run's, param scan's and walk-forward's code and timing, recorded at the dispatch and folded at
    /// the answer: what the results pane's run line and code line read.
    pub(crate) run_origin: StudioResultsOrigin,
    pub(crate) scan_origin: StudioResultsOrigin,
    pub(crate) walkforward_origin: StudioResultsOrigin,
    /// The Walk-Forward's out-of-sample window count, the Sweep pane's own control
    /// (`WALKFORWARD_SPLITS_DEFAULT` until moved; the profile door has no default of its own).
    pub walkforward_splits: usize,
    /// Whether each walk-forward window SEARCHES the Sweep pane's grid on its own training half
    /// (the wire's searching walk) instead of walking the strategy's fixed parameters.
    pub(crate) walkforward_search: bool,
    /// Index into `TEMPLATES` selected by the panel's dropdown.
    pub template_index: usize,
    /// The in-app AI copilot (Studio SP3 Part B, Task 5).
    pub chat: StudioChatPane,
    chat_rx: Option<Receiver<StudioChatOutcome>>,
    /// The Indicators browser/builder pane over the vike-indicators registry.
    pub indicators: StudioIndicatorsPane,
    /// The Data browser pane over the store's full series inventory (see `data_browser.rs`).
    pub data_browser: StudioDataBrowserPane,
    /// The Saved-strategies pane (persist + compare — see `panes/saved/mod.rs`).
    pub saved: StudioSavedPane,
    /// The in-flight catalog walk's receiver (`None` when idle) — the ⟳ Refresh button's
    /// worker-thread twin of `run_rx`/`sweep_rx`/`walkforward_rx`, folded by `poll()` the same way.
    ///
    /// It is ALSO the in-flight latch: `Some` means a walk is still owed an answer, and
    /// [`Self::start_catalog_load`] returns without spawning a second one. There is no separate
    /// boolean, because a boolean and a receiver are two facts that can disagree — the receiver is
    /// the one that decides whether an answer can still arrive.
    ///
    /// ⚠ Deliberately NOT cleared by [`Self::cancel`] and NOT counted by [`Self::any_running`]: a
    /// catalog walk is not a run, the Cancel button offers to abandon runs, and dropping this
    /// receiver would clear the latch while the thread kept walking — which is exactly how a
    /// second click would get a second walk.
    catalog_rx: Option<Receiver<crate::backend::catalog::StudioCatalogLoad>>,
    /// The in-flight "Compare all" worker's receiver (`None` when idle) — the Saved pane's
    /// worker-thread twin of `run_rx`/`sweep_rx`/`walkforward_rx`. `saved.compare_rows`/`compare_error`
    /// stay the pane's own state; this just carries the pending outcome until `poll()` folds it
    /// in, same split as the other three.
    compare_rx: Option<Receiver<crate::backend::backtesthub::StudioSavedCompareOutcome>>,
    /// Which tool the shared right-hand panel currently shows.
    pub right_tab: StudioTab,
    /// Collapsed-to-a-thin-rail state for the left editor panel.
    pub editor_collapsed: bool,
    /// Collapsed-to-a-thin-rail state for the right tools panel.
    pub tools_collapsed: bool,
    /// The editor panel's width: where the splitter between it and the results was left, kept with
    /// the workspace; the theme's `EDITOR_DEFAULT_WIDTH` until it is dragged.
    pub editor_width: f32,
    /// Whether the toolbar's Advanced line is open: the backtest server's address and where the
    /// strategy comes from (the Studio, or the server's own list).
    pub(crate) server_advanced: bool,
    /// The last answer of the backtest server's check (`check_server`: a Read dial and a `Ping`), the
    /// toolbar's "connected", with the features the server advertises ([`Self::server_serves`]); `None`
    /// until asked, and again after the address is edited.
    pub(crate) server_check: Option<Result<Vec<String>, String>>,
    /// The check in flight (a worker: a dial may wait out its connect timeout).
    server_check_rx: Option<Receiver<Result<Vec<String>, String>>>,
    /// The desktop's active server as its Backend list names it, `(name, address)`, set by the
    /// composition root: the toolbar names the backtest server by it when the two hosts agree.
    pub desktop_server: Option<(String, String)>,
    /// The local ports the tunnel `vike-cli backend connect` recorded forwards (empty: none recorded),
    /// set by the composition root: every `127.0.0.1` address at one of them leads to that tunnel's one
    /// box, so the toolbar names a backtest server there by the desktop's server through the same tunnel.
    pub desktop_tunnel_ports: Vec<u16>,
    /// Whether the last frame drew the editor narrower than [`Self::editor_width`] so the results kept
    /// their width (`fit_side_panels`): the frame after, the panel is set back to the user's width.
    pub(crate) editor_fitted: bool,
    /// A template a load is holding while the editor has unsaved changes: the editor asks first
    /// (`load_template`), and this is what "load it over my changes" writes.
    pub(crate) pending_template: Option<String>,
    /// A server file Open has read while the buffer it would replace has unsaved changes, as
    /// `(file, text)`: the editor asks first, as for a template (`Self::open_file_text`).
    pub(crate) pending_open: Option<(String, String)>,
    /// Whether the editor buffer is RUST: set by the last mode that edits the buffer (Rhai or my Rust
    /// strategy), and kept through Strategy files, which parks the buffer as it was
    /// ([`Self::buffer_is_rhai`]).
    buffer_is_rust: bool,
    /// The OTHER language's editor buffer and its unsaved baseline, `(source, saved_source)`: Rhai and
    /// Rust keep a text each, and a switch between them parks one and brings the other back
    /// (`Self::switch_source`). Not persisted: a restart keeps the buffer on screen.
    other_buffer: Option<(String, String)>,
    /// The RHAI compile check over the editor buffer: the source it was computed FOR, paired with
    /// `compile_status`'s verdict on it. Recomputed (in `poll()`) only when `editor.source` has
    /// drifted from that source — compiling on every frame regardless of keystrokes would be
    /// wasteful even though a single compile is cheap (~ms).
    ///
    /// ⚠ **`None` means NO verdict is held, and that is the state whenever the buffer is not Rhai**
    /// ([`Self::buffer_is_rhai`]). It used to be two fields — the source compiled and an
    /// `Option<String>` error — and in that shape the check ran on the buffer whatever
    /// `strategy_source` said, so a STRATEGY FILE author's valid Rust source wore a red
    /// `● error · line N` header chip beside the `● built <sha>` its own Build had just answered.
    /// A `None` error could not have said "not checked": it read as `● compiles`. So the verdict
    /// is DROPPED in Strategy file mode rather than merely hidden, and the pair lives as one value so the
    /// source and the verdict about it cannot drift apart.
    rhai_check: Option<(String, Result<(), vike_rhai::ScriptError>)>,
    /// The editor source as of the last Save/Load/template-load — the "unsaved changes" baseline.
    /// `editor.source != saved_source` is the AMBER-dot dirty check.
    ///
    /// ⚠ **In Strategy file mode a Save re-baselines to the source its sha was BUILT from, not to the
    /// buffer.** A Strategy file row stores a name and a sha and never the source, so the only buffer that
    /// row can be said to have saved is the one the sha names — `Self::rust_built_source`. An
    /// edit since that Build therefore keeps the chip lit through a save, which is true: nothing
    /// holds that edit.
    pub saved_source: String,
    /// [`StudioStrategySource::File`]'s own baseline — the `(file, params)` pair as of the last
    /// Save/Load, mirroring what `saved_source` is for Rhai and `rust_built_source` is for
    /// Strategy file. `None` means this session has neither saved nor loaded a Native selection.
    ///
    /// ⚠ **Native's dirty check used to be `editor.source != saved_source` like every other
    /// mode**, which compares the PARKED Rhai buffer — a Native save touches neither `editor.source`
    /// nor `saved_source`, so the chip could light on an unrelated stale buffer and never light on
    /// an actual unsaved param edit. `file_is_dirty` reads this field instead;
    /// `unsaved_chip_tip`'s `StudioStrategySource::File` arm names what a save actually captures.
    file_saved: Option<(String, Vec<(String, String)>)>,
    /// The workspace file's writer, holding the snapshot last written (or loaded at startup). Fed
    /// each frame by `ui()`'s trailing `maybe_persist_workspace` call: a drift in what
    /// [`Self::workspace_snapshot`] captures is written once it has held still, off this thread, and
    /// [`Self::flush_workspace`] writes what is left at the window's close and the app's exit.
    workspace_saver: StudioWorkspaceSaver,
    /// QA autorun (see [`StudioState::new_with_qa`] and [`StudioAutorun`]): kick off one backtest OR
    /// one parameter sweep automatically on the first `ui()` frame IF a slice is already selected,
    /// so headless captures can show the results surface (a capture run can't click ▶ Run or
    /// ▶ Run Sweep). Consulted exactly once — the first frame disarms it unconditionally.
    /// [`StudioAutorun::Off`] unless the caller asks for it.
    qa_autorun: StudioAutorun,
    /// True while the caller's QA tab override is active: workspace persistence is
    /// suppressed for the whole session so a capture run (or a lingering env var in a dev shell)
    /// can never write the FORCED tab/collapse state over the user's real
    /// `studio_workspace.json`.
    qa_workspace_readonly: bool,
    /// The name of the most recent successful Save — what an empty-name Ctrl+S re-saves under
    /// (normal editor Save semantics) instead of misfiling edits as "untitled".
    last_saved_name: Option<String>,
    /// Which strategy every Run/Sweep/Walk-Forward executes: the Rhai editor buffer (the default,
    /// and the only option before native support) or the selected native registry strategy.
    ///
    /// ⚠ **Persisted with the workspace, beside the buffer it says the language of.** It was not,
    /// and the buffer was: a Strategy file author's Rust source came back after a restart in RHAI mode,
    /// wearing the Rhai error chip and one ▶ Run away from being executed as a script.
    /// `crate::studio::workspace::StudioWorkspace::strategy_source` carries the format argument.
    pub strategy_source: StudioStrategySource,
    /// The strategy FILE the Strategy pane's list has selected, by its path under the backtest
    /// server's `strategies/` folder (`trend/momentum.rs`); empty while none is picked.
    pub picked_file: String,
    /// The toolbar's SDK pick: the SDK number a Run and a Build build a `.rs` file against (decision
    /// 0296), picked off the server's SDK list, `None` for "as the file says" (the default). Read by
    /// [`Self::picked_sdk`]; not persisted, so every session starts on the file's own pin.
    pub(crate) sdk_pick: Option<u32>,
    /// The served list of that folder ([`WireStrategyFile`]: each file's path and states), or the
    /// sentence its fetch failed with — `None` until the pane's Refresh asks.
    pub strategy_files: Option<Result<Vec<WireStrategyFile>, String>>,
    /// The in-flight list fetch (a WORKER, never a frame-thread dial: [`Self::named_roster_rx`]'s
    /// argument holds for this dial too).
    pub(crate) strategy_files_rx: Option<Receiver<Result<Vec<WireStrategyFile>, String>>>,
    /// The in-flight Open of a file: its path, and the receiver its text arrives on.
    pub(crate) open_rx: Option<(String, Receiver<Result<String, String>>)>,
    /// The last Open's refusal, if it failed.
    pub(crate) open_last: Option<String>,
    /// Why the persisted workspace was not read (`crate::studio::workspace::load_workspace`'s
    /// reason), `None` when it was. While `Some` the session writes nothing over it.
    pub workspace_refused: Option<String>,
    /// The strategy file's free-form `(key, value-text)` param rows (`params_from_rows` types them
    /// at run time): the file's header declares its keys, and the server refuses one it does not.
    pub file_params: Vec<(String, String)>,
    /// [`StudioStrategySource::Rust`] only: the path the buffer is built as (`trend/momentum.rs`) — the
    /// file it was opened from, or one typed in.
    pub rust_file: String,
    /// The `.rs` file last opened from the server and the text it was served with. A Rust Run runs
    /// the FILE on the server (a hub loads no artifact by sha), so it is refused while the editor
    /// does not hold this text for [`Self::rust_file`]: the server's file would not be these edits.
    pub(crate) opened_rust_file: Option<(String, String)>,
    /// [`StudioStrategySource::Rhai`]'s server file: the `.rhai` path a Save writes the buffer to — the
    /// file it was opened from, or one typed in. (Rust mode saves to [`Self::rust_file`].)
    pub rhai_file: String,
    /// The in-flight Save: the file, the text SENT (what the server now holds once it answers), and
    /// the receiver of its answer (the sha256 and the file's states).
    save_file_rx: Option<StudioWorkerStrategyFileSaveRequest>,
    /// The last Save's outcome: what was saved, in one line, or the server's refusal.
    pub save_file_last: Option<Result<String, String>>,
    /// [`StudioStrategySource::Rust`] only: the sha256 the LAST successful Build returned. `None` means
    /// "not yet built" — [`Self::run_blocked_reason`] refuses Run while this is `None`, because
    /// running would ask the server for an artifact it never received. A successful
    /// [`crate::spawn_backtesthub_build`] is the only thing that sets it, folded in by [`Self::poll`].
    pub(crate) rust_sha: Option<String>,
    /// The editor buffer [`Self::rust_sha`] was built FROM, when this session built it — the
    /// bytes [`Self::start_rust_build`] SENT, carried with the build in `build_rx`, never
    /// the buffer as it stands when the answer lands.
    ///
    /// ⚠ **That distinction is the guard's whole reach, and it was missing.** `poll` used to record
    /// `editor.source` at DELIVERY, and a build is a cargo build — minutes, during which the author
    /// keeps typing. Every edit made while it ran was then recorded as built, the comparison below
    /// could never see it, and Run dispatched the OLD artifact under the edited code with no error
    /// and plausible numbers: the exact trap this field exists to close, reopened for the one window
    /// in which an edit is most likely.
    ///
    /// ⚠ **This is the staleness guard, and without it the sha is a trap.** An edit after a
    /// successful Build leaves `rust_sha` naming the OLD artifact, so Run would report on code
    /// the author had already replaced — silently, with entirely plausible numbers. The design
    /// makes the sha the answer to *"which code traded"*; that is only true while the sha and the
    /// buffer agree.
    ///
    /// `None` means "this session did not build it" — a sha reloaded from a saved row, whose
    /// source this shell never held. That case is NOT blocked: the saved row is the author's own
    /// record of a built artifact, and refusing it would break the reload path to guard against a
    /// drift nothing here can even measure.
    pub(crate) rust_built_source: Option<String>,
    /// The in-flight build (`None` when idle): the source that was SENT, and the receiver its answer
    /// arrives on — the worker-thread twin of `run_rx`/`sweep_rx`/`walkforward_rx`, folded by [`Self::poll`]
    /// the same way. A build is a CARGO BUILD, so this is the one worker in this shell whose
    /// legitimate wait is minutes — which is why the sent source rides WITH the receiver: the buffer
    /// is free to change underneath it, and [`Self::rust_built_source`] must name what was built.
    build_rx: Option<StudioWorkerRustBuildRequest>,
    /// The last build's outcome, or `None` if none has been asked for this session. The error side
    /// is one rendered sentence (the shape `study_last` uses and for its reason); for a compile
    /// failure that sentence is rustc's OWN diagnostics, verbatim.
    pub(crate) build_last: Option<Result<String, String>>,
    /// The Research pane: the backtest server's studies, and the runs it has kept
    /// (`crate::panes::research`). A thin client of [`Self::backend`]'s server: studies run ONLY there
    /// (the owner's ruling of 2026-11-19), and nothing reads this PC's own study or runs folders.
    pub research: StudioResearchPane,
    /// The in-flight study-list fetch (`None` when idle), folded by `poll()` into [`Self::research`].
    research_studies_rx: Option<Receiver<Result<WireStudyListing, String>>>,
    /// The in-flight runs-page fetch (`None` when idle), folded the same way.
    research_runs_rx: Option<Receiver<Result<WireRunPage, String>>>,
    /// The in-flight study run on the server (`None` when idle) — the Research pane's worker-thread
    /// twin of `run_rx`/`sweep_rx`/`walkforward_rx`, and folded by `poll()` the same way.
    study_rx: Option<Receiver<Result<WireStudyReport, String>>>,
    /// The last study run's report (saved or not, `WireStudyReport::not_saved` says which), its
    /// refusal in the server's words, or `None` if none has been asked for this session.
    pub study_last: Option<Result<WireStudyReport, String>>,
    /// The in-flight read of an artifact's first page the result screen asked for (`None` when idle).
    study_artifact_rx: Option<Receiver<Result<WireRunArtifact, String>>>,
    /// The SDK source view (`crate::panes::sdk_source`): the backtest server's strategy SDKs, the one the
    /// open strategy builds against as a tree, and one of its files in the central panel. Read-only, over
    /// Read dials (decision 0296, verdict 3).
    pub(crate) sdk_source: StudioSdkSourcePane,
    /// The in-flight SDK list (`None` when idle), folded by `poll()` into [`Self::sdk_source`].
    sdk_listing_rx: Option<Receiver<Result<WireSdkListing, String>>>,
    /// The in-flight folder read of the SDK tree, tagged with its SDK and path (one at a time).
    sdk_folder_rx: Option<StudioWorkerSdkRead>,
    /// The in-flight file read for the code view (a newer click replaces it).
    sdk_file_rx: Option<Receiver<Result<WireSdkFile, String>>>,
    /// WHICH producer's results the central panel is showing — see [`StudioCenterView`].
    pub(crate) center: StudioCenterView,
    /// WHERE a Run/Sweep/Walk-Forward executes. EVERY variant is a DAEMON now, and that is the
    /// whole of it since `StudioRunBackend::Local` was deleted
    /// (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`): nothing
    /// here simulates in this process. [`StudioRunBackend::Source`] offloads to the backtesthub over
    /// TCP (⚠ this said `vike-datahub` until 2026-09-20 and the default address AGREED with it,
    /// which is what made it a bug rather than a typo), and [`StudioRunBackend::Named`] asks that same
    /// daemon to run a strategy it already holds
    /// (`docs/decisions/0064-a-named-run-carries-no-source.md`) - the one backend an OBSERVE
    /// credential can reach. A runtime toggle like `strategy_source`, held on the state struct;
    /// not disk-persisted.
    pub backend: StudioRunBackend,
    /// **This PC's device key** (decision 0288), as the `HubKeys` every hub dial of Studio signs with:
    /// the backtesthub's Read dials ([`StudioRunBackend::Named`], the strategy-file list and read), its Write
    /// dials ([`StudioRunBackend::Source`]'s Run, Sweep and Walk-Forward, Compare all, the copilot's candidates,
    /// a strategy file's Save and Build) and the Data pane's CSV upload to the datahub. LATE-BOUND by
    /// the shell from the desktop's device key (`vike_app_core::backend::registry::datahub_dial_keys`),
    /// `None` in a bare `cargo run -p vike-studio` or on a PC that holds no device key.
    ///
    /// ⚠ **What it may do is the SERVER's list, not this field's**: one key, and the device's role on
    /// the backtesthub's access list admits `Read` (`observe`) or also `Write` (`control`). Until the
    /// cutover a Run signed with a shared datahub CONTROL key held in a second field of an opaque type
    /// (decision 0083, question 1); no hub reads that key any more. A Write dial still signs only
    /// toward the backtest PLANE (`crate::backend::backtesthub`'s `connect`).
    ///
    /// ⚠ It is resolved OFF the frame thread by the shell (that resolution opens the settings
    /// database's `device_key` table) and pushed in, so nothing here reads a store.
    pub hub_keys: Option<vike_hub_protocol::auth::HubKeys>,
    /// The shell's device-key epoch [`Self::hub_keys`] was last seeded at, `None` before the first
    /// seed: what [`Self::hub_keys_due`] compares.
    hub_keys_epoch: Option<u64>,
    /// Where the Data pane's Import CSV form sends a file (decision 0272): the datahub the Studio's
    /// store reads and this PC's device key, LATE-BOUND by the shell as [`Self::hub_keys`] is, and
    /// `None` when the Studio reads no remote datahub. The upload is an observe-scope verb.
    pub datahub_upload: Option<crate::backend::csv_upload::StudioCsvUploadDial>,
    /// The upload in flight (whether it is a preview, and its event stream), `None` when idle — the
    /// Import form's worker-thread twin of `catalog_rx`, folded by `poll()`.
    csv_upload_rx: Option<(bool, Receiver<crate::backend::csv_upload::StudioCsvUploadEvent>)>,
    /// The roster [`StudioRunBackend::Named`] last learned from its daemon, plus whether that daemon's lane
    /// is ARMED — `None` until the picker asks.
    ///
    /// ⚠ **An UNARMED daemon ANSWERS but names NOTHING**, so `armed: false` must render the
    /// switch's name rather than the empty list that arrives with it —
    /// `vike_backtesthub_client::named_run::NamedRunRoster::unarmed_note` is the sentence, and without it
    /// this pane would show a blank roster indistinguishable from a daemon that holds no
    /// strategies. That is 0064's decision 8: the arming gates the ROSTER too (its leg 3 — naming
    /// the operator's own compiled-in strategies is itself the disclosure), and it rides in the
    /// ANSWER rather than in the capability string so an OLD server and an unarmed one stay two
    /// different sentences.
    pub(crate) named_roster:
        Option<Result<vike_backtesthub_client::named_run::NamedRunRoster, String>>,
    /// The in-flight roster fetch, if one is running.
    ///
    /// ⚠ **A worker, not a frame-thread dial, and the first draft of this pane got that wrong.**
    /// The VERB is trivial — a compile-time const roster, no store read, no engine — which is
    /// exactly the reasoning that put it on the update loop. What that reasoning missed is the
    /// DIAL: `vike_hub_protocol::limits::CLIENT_CONNECT_TIMEOUT` (which `vike_datahub_client` dials with) is ten seconds per resolved address and the
    /// name resolution above it is unbounded, so a stale tunnel or a dark IPv6 route freezes the
    /// GUI rather than slowing it. Same rule `crates/vike-app-core/src/data/backfill/mod.rs` states for
    /// its own dial.
    pub(crate) named_roster_rx: Option<
        std::sync::mpsc::Receiver<
            Result<vike_backtesthub_client::named_run::NamedRunRoster, String>,
        >,
    >,
    /// Which name off that roster [`StudioRunBackend::Named`] will run — `None` until one is picked.
    ///
    /// ⚠ **A `String` rather than an index into [`Self::strategy_files`], and that is not a style
    /// choice.** The named roster is a DIFFERENT set from the folder's files (0064's decision 7):
    /// it carries the operator's own compiled-in user strategies, which no file names. An index
    /// into the file list could not name them at all.
    pub(crate) named_strategy: Option<String>,
}

impl StudioState {
    /// The constructor: restore the persisted workspace. Reads NO environment and NO credential
    /// store — a library takes its configuration as parameters: `chat_keys` are the AI-provider keys
    /// the StudioChatPane used to load out of `<project>/settings/secrets.env` itself (split-plane
    /// I8 — see [`StudioChatApiKeys`]).
    ///
    /// `state_dir` is where the Studio's state files land (see the field doc).
    ///
    /// The two headless-capture QA overrides, supplied by the CALLER (`None` and
    /// [`StudioAutorun::Off`] for none):
    ///
    /// - `qa_tab` — a raw tab name (`sweep|strategy|data|indicators|saved|chat`, parsed by
    ///   [`StudioTab::from_qa_str`]; garbage is ignored, never a panic). `Some` forces that tool tab,
    ///   expands the tools panel, and — load-bearing — puts the session in
    ///   `qa_workspace_readonly` so a capture run can never write the FORCED state over the user's
    ///   real `studio_workspace.json`.
    /// - `qa_autorun` — [`StudioAutorun`]: kick off one backtest ([`StudioAutorun::Run`]) or one parameter
    ///   sweep ([`StudioAutorun::Sweep`]) on the first `ui()` frame IF a slice is selected, so a
    ///   headless capture can show the results surface (it cannot click ▶ Run or ▶ Run Sweep).
    ///
    /// The values used to be read straight from `VIKE_STUDIO_TAB` / `VIKE_STUDIO_AUTORUN` inside
    /// this constructor — a LIBRARY reading process environment its caller can neither see nor
    /// override, which is the `Layer::Library` class `crates/vike-ops/tests/settings_secrets/settings_registry.rs`
    /// ratchets down. `vike-desktop`'s `main.rs` does the two reads now; nothing else in the workspace
    /// wants them (the `studio_shot` capture example poses `right_tab` on the struct directly).
    pub fn new_with_qa(
        store: Arc<dyn HistoryStore + Send + Sync>,
        state_dir: PathBuf,
        chat_keys: StudioChatApiKeys,
        qa_tab: Option<&str>,
        qa_autorun: StudioAutorun,
    ) -> Self {
        let read =
            workspace_read_path().map(|p| load_workspace(&p)).unwrap_or(Ok(Default::default()));
        let refused = read.as_ref().err().cloned();
        let mut st = Self::with_workspace(
            store,
            state_dir,
            chat_keys,
            qa_tab,
            qa_autorun,
            read.unwrap_or_default(),
        );
        // A snapshot that would not parse is kept as it is: this session writes nothing over it.
        if refused.is_some() {
            st.qa_workspace_readonly = true;
        }
        st.workspace_refused = refused;
        st
    }

    /// [`new_with_qa`](Self::new_with_qa) over an already-loaded workspace snapshot — the restore
    /// half, split from the READ so a test can hand it a snapshot. `new_with_qa`'s read resolves
    /// `<project>/settings/state` by walking up from the working directory, which under
    /// `cargo test` is the checkout's own settings root: a test constructing through it starts from
    /// whatever workspace that box last wrote.
    fn with_workspace(
        store: Arc<dyn HistoryStore + Send + Sync>,
        state_dir: PathBuf,
        chat_keys: StudioChatApiKeys,
        qa_tab: Option<&str>,
        qa_autorun: StudioAutorun,
        ws: StudioWorkspace,
    ) -> Self {
        // ⚠ The CONSTRUCTION walk is deliberately still synchronous, unlike the ⟳ Refresh one
        // (`start_catalog_load`, and `crate::backend::catalog`'s module doc for the whole argument). It
        // runs once, before this shell has drawn a frame, and both the QA autorun hook and the
        // posed-state fixtures in `crates/vike-studio/tests/headless/common/mod.rs` read a POPULATED picker
        // the instant the constructor returns. Moving it off-thread is a separate change with its
        // own first-frame consequences; it is not the paint-thread stall this pair of calls was
        // measured in.
        let mut picker = StudioSlicePicker::default();
        picker.refresh(store.as_ref());
        let mut data_browser = StudioDataBrowserPane::default();
        data_browser.refresh(store.as_ref());
        let saved = StudioSavedPane::load(&state_dir.join(SAVED_STRATEGIES_FILE));
        let mut editor = StudioEditorPane::default();
        // Restore the last editor buffer only if the workspace actually captured one — an empty
        // `editor_source` (the field's `Default`, e.g. first launch / no file yet) keeps the
        // known-good default starter script instead of blanking the editor.
        if !ws.editor_source.is_empty() {
            editor.source = ws.editor_source.clone();
        }
        let saved_source = editor.source.clone();
        // Clamp against `TEMPLATES` shrinking/growing across versions — an out-of-range
        // stored index must never panic the `ComboBox::show_index` call in `ui()`.
        let template_index = ws.template_index.min(TEMPLATES.len().saturating_sub(1));
        // QA: a caller-supplied tab name (<sweep|strategy|data|indicators|saved|chat>) forces the
        // initial tool tab and expands the tools panel for headless per-tab captures — the
        // vike-studio twin of the desktop's style/scale capture hooks. Absent/garbage restores the
        // workspace. While the override is active, workspace persistence is disabled for the
        // session (`qa_workspace_readonly`) so the forced values can never clobber the user's file.
        let qa_tab = qa_tab.and_then(StudioTab::from_qa_str);
        // ⚠ The SWEEP autorun arms this too, and it MUST: unlike [`StudioAutorun::Run`] — which touches
        // only `center`, a field no workspace persists — the sweep arm loads
        // [`SWEEP_CAPTURE_SCRIPT`] into the editor, and `editor.source` IS persisted. Deriving the
        // guard from `qa_tab` alone was safe while every autorun was read-only; it stopped being
        // safe the moment one of them wrote, and a capture (or a stale `VIKE_STUDIO_AUTORUN=sweep`
        // in a dev shell) overwriting somebody's saved strategy is exactly the failure this flag
        // was invented to prevent.
        let qa_workspace_readonly = qa_tab.is_some() || qa_autorun == StudioAutorun::Sweep;
        let (right_tab, tools_collapsed) = match qa_tab {
            Some(t) => (t, false),
            None => (ws.right_tab, ws.tools_collapsed),
        };
        let editor_collapsed = ws.editor_collapsed;
        let mut st = Self {
            store,
            state_dir,
            store_is_datahub: false,
            editor,
            picker,
            tab: StudioResultsTab::default(),
            running: false,
            recompute: false,
            last: None,
            run_rx: None,
            grid: Vec::new(),
            param_grids: ws.param_rows.clone(),
            grid_key: String::new(),
            grid_seeded: None,
            grid_dropped: Vec::new(),
            seed_file_rx: None,
            seed_failed: None,
            conditions: ws.test_conditions.clone(),
            sweep_started: None,
            sweep_took: None,
            sweep_reveal: false,
            sweep_rx: None,
            sweep_last: None,
            sweep_open: None,
            point_rx: None,
            walkforward_rx: None,
            walkforward_last: None,
            trades: StudioResultsTradesPager::default(),
            trades_rx: None,
            run_origin: StudioResultsOrigin::default(),
            scan_origin: StudioResultsOrigin::default(),
            walkforward_origin: StudioResultsOrigin::default(),
            walkforward_splits: WALKFORWARD_SPLITS_DEFAULT,
            walkforward_search: false,
            template_index,
            chat: StudioChatPane::new(chat_keys),
            chat_rx: None,
            indicators: StudioIndicatorsPane::default(),
            data_browser,
            saved,
            catalog_rx: None,
            right_tab,
            editor_collapsed,
            tools_collapsed,
            editor_width: ws
                .editor_width
                .unwrap_or(vike_ui_theme::value::studio::EDITOR_DEFAULT_WIDTH),
            server_advanced: false,
            server_check: None,
            server_check_rx: None,
            desktop_server: None,
            desktop_tunnel_ports: Vec::new(),
            editor_fitted: false,
            pending_template: None,
            pending_open: None,
            other_buffer: ws.parked_source.clone().map(|source| (source.clone(), source)),
            buffer_is_rust: match ws.strategy_source {
                StudioStrategySource::Rhai => false,
                StudioStrategySource::Rust => true,
                StudioStrategySource::File => ws.buffer_is_rust,
            },
            compare_rx: None,
            // Seeded just below, once the restored source says whether the buffer is Rhai.
            rhai_check: None,
            saved_source,
            // The restored (file, params) counts as saved, exactly like `saved_source` starting
            // equal to the restored `editor.source` — a freshly opened session shows no unsaved
            // chip until something actually changes.
            file_saved: Some((ws.picked_file.clone(), ws.file_params.clone())),
            // Replaced just below by a saver over the snapshot of the state being built, so the
            // first frame's `maybe_persist_workspace` finds nothing drifted and writes nothing.
            workspace_saver: StudioWorkspaceSaver::new(
                StudioWorkspace::default(),
                write_workspace_file,
            ),
            qa_autorun,
            qa_workspace_readonly,
            last_saved_name: None,
            strategy_source: ws.strategy_source,
            picked_file: ws.picked_file,
            sdk_pick: None,
            strategy_files: None,
            strategy_files_rx: None,
            open_rx: None,
            open_last: None,
            workspace_refused: None,
            file_params: ws.file_params,
            rust_file: ws.rust_file,
            opened_rust_file: None,
            rhai_file: String::new(),
            save_file_rx: None,
            save_file_last: None,
            // ⚠ NOT restored, and deliberately never persisted: a sha from a previous session has
            // no recorded source (`rust_built_source` would be `None`), so the staleness guard
            // could not see an edit made after that Build and before the shutdown, and Run would
            // dispatch an artifact of unknown provenance under the restored buffer. A restored
            // Rust session is "not built yet" until Build answers — and the builder is
            // content-addressed, so rebuilding an unchanged buffer is a cache hit.
            rust_sha: None,
            rust_built_source: None,
            build_rx: None,
            build_last: None,
            backend: StudioRunBackend::default(),
            hub_keys: None,
            hub_keys_epoch: None,
            datahub_upload: None,
            csv_upload_rx: None,
            named_roster: None,
            named_roster_rx: None,
            named_strategy: None,
            // Nothing asked yet: the pane asks the backend's server the first time it is opened.
            research: StudioResearchPane::default(),
            research_studies_rx: None,
            research_runs_rx: None,
            study_rx: None,
            study_last: None,
            study_artifact_rx: None,
            // Nothing asked yet: the SDK list is asked the first time its section is opened.
            sdk_source: StudioSdkSourcePane::default(),
            sdk_listing_rx: None,
            sdk_folder_rx: None,
            sdk_file_rx: None,
            center: StudioCenterView::default(),
        };
        // Seed the compile cache eagerly rather than report a stale/blank status for one frame
        // before `poll()` first runs — but only when the RESTORED buffer is Rhai. A session that
        // comes back in Strategy file mode holds Rust, and the same rule `poll` applies from then on
        // applies here: no Rhai verdict is held about it, not even for the first frame.
        if st.buffer_is_rhai() {
            let verdict = crate::panes::editor::compile_status(&st.editor.source);
            st.rhai_check = Some((st.editor.source.clone(), verdict));
        }
        // The restored strategy's rows on screen, the others put away.
        st.grid_key = st.param_key();
        st.grid = st.param_grids.remove(&st.grid_key).unwrap_or_default();
        st.workspace_saver =
            StudioWorkspaceSaver::new(st.workspace_snapshot(), write_workspace_file);
        st
    }
}

// `StudioState`'s one `impl` is split by concern into the child modules below; this file keeps the
// types, the struct, the constructor and the worker-poll helpers they share.
mod center;
mod dispatch;
mod persistence;
pub(crate) mod shell;
mod strategy;
mod tool_panes;
mod workers;
// The persisted-workspace snapshot (`StudioWorkspace`) and the path helpers that resolve its file:
// a type module rather than a slice of the `impl`. `pub(crate)` because `lib.rs` re-exports its
// public half (`pub use studio::workspace::{...}`).
pub(crate) mod workspace;

#[cfg(test)]
mod tests;
