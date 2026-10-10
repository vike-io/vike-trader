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
use std::sync::mpsc::{Receiver, TryRecvError};
use vike_analytics::report::{DEFAULT_PERIODS_PER_YEAR, periods_per_year_for_interval};
use vike_backtest::walkforward::WalkForwardReport;
use vike_script::discover_params;

use self::workspace::{
    StudioWorkspace, load_workspace, save_workspace, workspace_read_path, workspace_write_path,
};
use crate::backend::remote::Backend;
use crate::panes::chat::{ChatApiKeys, ChatOutcome, ChatPane, summary_of};
use crate::panes::data_browser::DataBrowserPane;
use crate::panes::editor::EditorPane;
use crate::panes::indicators::IndicatorsPane;
use crate::panes::picker::SlicePicker;
use crate::panes::research::{ResearchAction, ResearchPane};
use crate::panes::results::{ResultsTab, results_ui};
use crate::panes::saved::{
    SavedAction, SavedPane, SavedStrategy, StrategySource, comparison_rows, save_strategies,
};
use vike_data::TsRange;
use vike_script::TEMPLATES;
use vike_studio_core::{
    CompareOutcome, RunError, RunOutcome, SliceKind, StoreHandle, StrategySpec, StudioParamscan,
    StudyRun, StudyRunError, native_strategies, params_from_rows, spawn_compare_all, spawn_study,
};
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::{Status, Tokens, chip, section};
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::maps::{self, MapRow};
use vike_ui_theme::type_scale::TextRole;

/// The saved-strategy list's filename, colocated with the Studio's per-store state directory
/// (`state_dir.join(...)` — the caller-supplied stand-in for the store root, now that the store
/// is a trait handle with no `root()`; vike-desktop passes its resolved local hist-store root, so a
/// local session keeps the exact old location) — per-store state alongside the data it was
/// written against.
const SAVED_STRATEGIES_FILE: &str = "studio_strategies.json";

/// The editor header's chip while the buffer is a Plugin's RUST source — the status-neutral
/// stand-in for the Rhai check's `● compiles` / `● error · line N`, which cannot judge Rust (see
/// `StudioState::buffer_is_rhai`). It names what is actually known — the language, and that the
/// verdict is the Build's — rather than a verdict nothing here has reached.
pub const PLUGIN_EDITOR_CHIP: &str = "● Rust · Build to check";

/// Run's refusal while a backtest is in flight.
const RUN_BUSY: &str = "A backtest is running — wait for it, or Cancel it.";
/// Every dispatch's refusal while no data slice is picked.
const PICK_A_SLICE: &str = "Pick a data slice first.";
/// Run Sweep's refusal while a sweep is in flight.
const SWEEP_BUSY: &str = "A sweep is running.";
/// Run Sweep's refusal while the grid parses to no value — the words its hover text used to carry,
/// where a disabled button never showed them.
const SEED_A_GRID: &str = "Seed a parameter grid first (it needs at least one numeric value).";
/// Walk-Forward's refusal while one is in flight.
const WALK_FORWARD_BUSY: &str = "A walk-forward is running.";
/// Send's refusal with no provider key — also the chat pane's own line when it has none.
const NO_PROVIDER_KEY: &str = "No provider key found — run `vike-cli secrets set ANTHROPIC_API_KEY` (or CEREBRAS_API_KEY; \
     the value on stdin), then restart.";
/// Send's refusal while the copilot answers.
const COPILOT_BUSY: &str = "The copilot is still answering.";
/// Send's refusal with nothing typed.
const NOTHING_TO_SEND: &str = "Describe a strategy first.";

/// The Study pane's central panel with no run yet — one merged sentence, since `state::view`'s
/// `Load::Empty` takes a single string where the old rendering split it across a title and a
/// caption label.
const NO_STUDY_RESULT: &str =
    "No study result yet — pick a study in the Research tool, then Run study.";

/// `empty_state`'s copy when the picker's series list is genuinely empty — not a depth-only store,
/// not a refused scan, just nothing backfilled yet. The existing sentence, verbatim.
const EMPTY_STORE: &str = "The data store is empty — backfill some history first\n\
     (vike-backfill, or the Data Manager's Backfill), then Refresh.";

/// Why every control that writes a RHAI script into the editor is disabled while the Strategy pane
/// is on `Plugin (Rust)` — the hover text of all four ([`StudioState::rhai_writer_blocked_reason`]
/// lists them and argues why they are refused rather than made to switch the source).
pub(crate) const RHAI_WRITER_BLOCKED_IN_PLUGIN: &str = "Plugin mode: the editor holds this plugin's Rust \
     source, and templates and the AI copilot write Rhai. Pick Rhai script in the Strategy tab \
     first — nothing here overwrites Rust source with Rhai.";

/// The unsaved chip's hover text in Rhai mode.
const UNSAVED_TIP: &str = "Unsaved changes — Ctrl+S saves to the Saved list";

/// The unsaved chip's hover text in Native mode: a save captures the registry name and the param
/// rows, never the parked editor buffer — see [`StudioState::native_saved`]'s doc for why the
/// dirty check itself had to change too, not just this text.
const UNSAVED_TIP_NATIVE: &str =
    "Unsaved — Ctrl+S saves this strategy's name and its param rows to the Saved list.";

/// The unsaved chip's hover text in Plugin mode while the buffer IS what the held sha was built
/// from, so a save names this code.
const UNSAVED_TIP_PLUGIN_BUILT: &str = "Unsaved — Ctrl+S saves this plugin's name and the sha \
     of its Build of this buffer to the Saved list. The Rust source itself is not saved.";

/// The unsaved chip's hover text in Plugin mode while the buffer is NOT what any held sha was built
/// from (no Build yet, an edit since, or a sha reloaded from a saved row).
const UNSAVED_TIP_PLUGIN_UNBUILT: &str = "Unsaved — in Plugin mode Ctrl+S saves the plugin's name \
     and its last Build's sha, never the editor text, so it cannot save these edits. Build first, \
     then Ctrl+S.";

// The persisted-workspace filename moved to `crate::studio::workspace` (settings-unification
// Phase 2): it is no longer colocated with the store like `SAVED_STRATEGIES_FILE` — which tab is
// open describes this USER's screen, not that store's data — so the basename now sits beside the
// two path helpers that resolve it (`workspace_read_path` / `workspace_write_path`).

/// Which tool the single right-hand panel shows. The seven tools (Sweep/Validate, Strategy,
/// Data, Indicators, Saved, Research, AI Copilot) share ONE tabbed `Panel::right` rather than each owning its own
/// always-open panel — simultaneous side panels crushed the editor + results to a sliver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RightTab {
    #[default]
    Sweep,
    /// Strategy source: the Rhai editor buffer, or a NATIVE registry strategy + its params.
    Strategy,
    Data,
    Indicators,
    Saved,
    /// The user's own STUDIES, and the runs they left behind — see `crate::panes::research`.
    Research,
    Chat,
}

impl RightTab {
    /// Tabs in display order (left→right).
    pub const ALL: [RightTab; 7] = [
        RightTab::Sweep,
        RightTab::Strategy,
        RightTab::Data,
        RightTab::Indicators,
        RightTab::Saved,
        RightTab::Research,
        RightTab::Chat,
    ];

    /// This tool's row of `ui-theme.toml`'s `studio_tab` map (`vike_ui_theme::maps::studio_tab`): its `word`
    /// is the tab's label and its `icon` the glyph on the rail, so which words and which glyph a tab wears
    /// is a line of the TOML and nothing here. Exhaustive: a tab without a row does not compile, and
    /// `every_tab_has_its_own_studio_tab_row_and_every_row_its_tab` holds the other direction.
    fn row(self) -> &'static MapRow {
        match self {
            RightTab::Sweep => &maps::studio_tab::SWEEP,
            RightTab::Strategy => &maps::studio_tab::STRATEGY,
            RightTab::Data => &maps::studio_tab::DATA,
            RightTab::Indicators => &maps::studio_tab::INDICATORS,
            RightTab::Saved => &maps::studio_tab::SAVED,
            RightTab::Research => &maps::studio_tab::RESEARCH,
            RightTab::Chat => &maps::studio_tab::CHAT,
        }
    }

    /// The tab strip's button label for this tool: its row's `word`.
    pub fn label(self) -> &'static str {
        self.row().word.expect("a studio_tab row carries the tab's label")
    }

    /// The vertical icon rail's icon for this tool — one registry icon per tool, the same one its
    /// pane header shows; the rail shows `icon()` alone and names it `label()` — its accessible
    /// name and its hover tooltip (`icons::named`, see `ui()`). It is its row's `icon`.
    pub fn icon(self) -> Icon {
        self.row().icon().expect("a studio_tab row names an icon of the registry")
    }

    /// Parse a caller-supplied QA tab name (the capture hook — see `StudioState::new_with_qa`)
    /// into a tab. Lowercase tool names, `None` for anything else — never panics on garbage input.
    pub fn from_qa_str(s: &str) -> Option<RightTab> {
        match s {
            "sweep" => Some(RightTab::Sweep),
            "strategy" => Some(RightTab::Strategy),
            "data" => Some(RightTab::Data),
            "indicators" => Some(RightTab::Indicators),
            "saved" => Some(RightTab::Saved),
            "research" => Some(RightTab::Research),
            "chat" => Some(RightTab::Chat),
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
/// [`StudioState::start_sweep`] is private and reachable from its own button alone. A capture
/// cannot click. So the `state` clause that pose is written against ("results, not an empty form")
/// was unreachable by construction, which is what its `capture_gap` records.
///
/// An EXHAUSTIVE match everywhere it is consumed, not a `matches!`: a third autorun (walk-forward
/// is the obvious next one) must fail to compile until somebody decides what it starts, rather
/// than inheriting "do nothing" from a wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QaAutorun {
    /// Nothing is started; the shell renders whatever the workspace restored. The default, and
    /// what an unset (or unrecognised) knob means.
    #[default]
    Off,
    /// One backtest — [`StudioState::start_run`]. The `"1"` spelling, kept EXACTLY as it was: the
    /// pre-existing read was `env::var(..).as_deref() == Ok("1")`, so every capture script and
    /// dev shell already setting `VIKE_STUDIO_AUTORUN=1` keeps its old behaviour to the byte.
    Run,
    /// One parameter sweep — seed the grid from the strategy's own declared params, widen each
    /// row to a ladder ([`qa_sweep_ladder`]), then [`StudioState::start_sweep`].
    Sweep,
}

impl QaAutorun {
    /// Parse the caller-supplied knob value. `None`, or anything unrecognised, is [`Self::Off`] —
    /// the same never-panic-on-garbage disposition [`RightTab::from_qa_str`] has, and the same one
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

/// Widen one seeded sweep-grid value into the candidate ladder a capture sweeps over.
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
    // Rendered through the same `{}` float formatting `seed_sweep_grid` writes its defaults with,
    // so a widened row reads like a seeded one.
    rungs.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
}

/// WHICH producer's results the central panel is showing.
///
/// Two surfaces rather than one, and the split is R6's own: a backtest's numbers come from the
/// event-driven simulator and a study's do not, so `crate::panes::results::results_ui` (which renders a
/// `vike_analytics::BacktestResult`) and `crate::panes::research::study_result_ui` (which renders a
/// `vike_studio_core::StudyRun`) each name their own producer instead of sharing a column. Every
/// dispatch sets this to its own view, so the panel shows the result of the thing that was last
/// asked for rather than obeying a precedence rule nobody can predict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CenterView {
    /// `crate::panes::results::results_ui` over the last Run / Sweep / Walk-Forward.
    #[default]
    Backtest,
    /// `crate::panes::research::study_result_ui` over the last study run.
    Study,
}

/// The Remote segment's hover text — this shell dials the COMPUTE daemon directly over TCP.
const REMOTE_WHY: &str = "Offload the run to the COMPUTE daemon over TCP — `vike-backend backtest \
     --addr`, default 127.0.0.1:7880. NOT the datahub: it refuses every Run* verb by plane.";
/// The Named segment's hover text — this shell asks the SAME daemon to run a strategy it already
/// holds, over the read-only Observe key.
const NAMED_WHY: &str = "Run a strategy the SERVER already holds, over this shell's read-only \
     key. No script, no sweep, no walk-forward — one strategy, one param set, one bounded window.";

/// The backend selector's value. `Backend` carries the address, so it cannot be the segmented
/// control's `Copy` value; this is WHICH one, and `switch_backend` carries the address across.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendKind {
    Remote,
    Named,
}

/// The tab that follows `t` in `RightTab::ALL`'s display order, wrapping from the last tab back
/// to the first. Pure helper behind the Ctrl/Cmd+/ shortcut (`StudioState::ui`) — kept
/// free-standing so it's unit-testable without an egui context.
pub(crate) fn next_tab(t: RightTab) -> RightTab {
    let i = RightTab::ALL.iter().position(|&x| x == t).unwrap_or(0);
    RightTab::ALL[(i + 1) % RightTab::ALL.len()]
}

/// A tool pane's heading — its icon and its title at the Title role, then the kit's strip rule. It
/// is the ONE text that says which pane the shared right panel shows
/// (`crates/vike-studio/tests/studio_shell_render/panes.rs`'s `pane_title` reads it, which is why the
/// title stays a label of its own rather than joining the icon's). The icon is secondary text,
/// never the accent: the accent marks shapes only (design system spec §2), and the rail's accent
/// edge already says which pane is open.
pub(crate) fn pane_header(ui: &mut egui::Ui, icon: Icon, title: &str) {
    let tk = Tokens::of(ui.ctx());
    let px = tk.text.px(TextRole::Title);
    ui.horizontal(|ui| {
        ui.label(icon.rich().size(px).color(tk.theme.text2));
        ui.label(egui::RichText::new(title).size(px).color(tk.theme.text));
    });
    section::strip_rule(ui);
}

/// The result of polling one worker-thread receiver in [`StudioState::poll`]: a delivered value, a
/// terminal disconnect (the worker panicked / dropped its sender without sending), or still-pending.
/// [`poll_worker`] clears the receiver slot on either terminal arm, so each `poll()` site handles
/// only the value / the failure — never the repeated "set `*_rx = None`" bookkeeping.
enum Delivery<T> {
    Ready(T),
    Failed,
    Pending,
}

/// Poll `slot` once with `try_recv`, clearing it to `None` on any terminal outcome (delivered or
/// disconnected). Centralizes the three-way `TryRecvError` match every worker arm in
/// [`StudioState::poll`] shares.
fn poll_worker<T>(slot: &mut Option<Receiver<T>>) -> Delivery<T> {
    let Some(rx) = slot else {
        return Delivery::Pending;
    };
    match rx.try_recv() {
        Ok(v) => {
            *slot = None;
            Delivery::Ready(v)
        }
        Err(TryRecvError::Disconnected) => {
            *slot = None;
            Delivery::Failed
        }
        Err(TryRecvError::Empty) => Delivery::Pending,
    }
}

/// A plugin build in flight: the EXACT editor buffer that was sent to the builder, and the receiver
/// its answer arrives on. One value, so the answer can only ever be folded against the request it
/// answers — see [`StudioState::build_rx`].
type PendingBuild = (String, Receiver<Result<String, String>>);

/// [`poll_worker`] for a receiver that travels with a TAG — a fact about the REQUEST that the answer
/// must be folded against, not a fact read off the shell when the answer happens to land. The tag
/// comes back beside the value; on a disconnect it is dropped with the slot.
fn poll_tagged_worker<K, T>(slot: &mut Option<(K, Receiver<T>)>) -> Delivery<(K, T)> {
    let Some((tag, rx)) = slot.take() else {
        return Delivery::Pending;
    };
    match rx.try_recv() {
        Ok(v) => Delivery::Ready((tag, v)),
        Err(TryRecvError::Disconnected) => Delivery::Failed,
        Err(TryRecvError::Empty) => {
            *slot = Some((tag, rx));
            Delivery::Pending
        }
    }
}

pub struct StudioState {
    /// The history store, as the TRAIT handle (split-plane B12) — a local `DataFusionHist` or an
    /// RPC-backed `RemoteHistStore`; every pane reads through `HistStore` verbs only.
    pub store: StoreHandle,
    /// Where the Studio's per-store state files live (`studio_strategies.json`, the AI copilot
    /// ledger, and the legacy-workspace read fallback). Caller-supplied because the trait has no
    /// `root()` — a filesystem concept an RPC store does not have — so the BINARY resolves the
    /// directory; vike-desktop passes its resolved local hist-store root, which keeps a local session
    /// byte-identical to the pre-trait behavior.
    state_dir: PathBuf,
    /// True when [`Self::store`] is an RPC-backed REMOTE store. Set by the binary after
    /// construction (alongside seeding [`Self::backend`]), like the other runtime toggles.
    pub store_is_remote: bool,
    pub editor: EditorPane,
    pub picker: SlicePicker,
    pub tab: ResultsTab,
    pub running: bool,
    pub last: Option<RunOutcome>,
    run_rx: Option<Receiver<RunOutcome>>,
    /// Sweep grid rows: (param name, comma-separated candidate values as typed in the panel).
    pub grid: Vec<(String, String)>,
    sweep_rx: Option<Receiver<Result<StudioParamscan, RunError>>>,
    pub sweep_last: Option<Result<StudioParamscan, RunError>>,
    wf_rx: Option<Receiver<Result<WalkForwardReport, RunError>>>,
    pub wf_last: Option<Result<WalkForwardReport, RunError>>,
    /// Index into `TEMPLATES` selected by the panel's dropdown.
    pub template_idx: usize,
    /// The in-app AI copilot (Studio SP3 Part B, Task 5).
    pub chat: ChatPane,
    chat_rx: Option<Receiver<ChatOutcome>>,
    /// The Indicators browser/builder pane over the vike-indicators registry.
    pub indicators: IndicatorsPane,
    /// The Data browser pane over the store's full series inventory (see `data_browser.rs`).
    pub data_browser: DataBrowserPane,
    /// The Saved-strategies pane (persist + compare — see `saved.rs`).
    pub saved: SavedPane,
    /// The in-flight catalog walk's receiver (`None` when idle) — the ⟳ Refresh button's
    /// worker-thread twin of `run_rx`/`sweep_rx`/`wf_rx`, folded by `poll()` the same way.
    ///
    /// It is ALSO the in-flight latch: `Some` means a walk is still owed an answer, and
    /// [`Self::spawn_catalog_refresh`] returns without spawning a second one. There is no separate
    /// boolean, because a boolean and a receiver are two facts that can disagree — the receiver is
    /// the one that decides whether an answer can still arrive.
    ///
    /// ⚠ Deliberately NOT cleared by [`Self::cancel`] and NOT counted by [`Self::any_running`]: a
    /// catalog walk is not a run, the Cancel button offers to abandon runs, and dropping this
    /// receiver would clear the latch while the thread kept walking — which is exactly how a
    /// second click would get a second walk.
    catalog_rx: Option<Receiver<crate::backend::catalog::CatalogLoad>>,
    /// The in-flight "Compare all" worker's receiver (`None` when idle) — the Saved pane's
    /// worker-thread twin of `run_rx`/`sweep_rx`/`wf_rx`. `saved.compare_rows`/`compare_error`
    /// stay the pane's own state; this just carries the pending outcome until `poll()` folds it
    /// in, same split as the other three.
    compare_rx: Option<Receiver<CompareOutcome>>,
    /// Which tool the shared right-hand panel currently shows.
    pub right_tab: RightTab,
    /// Collapsed-to-a-thin-rail state for the left editor panel.
    pub editor_collapsed: bool,
    /// Collapsed-to-a-thin-rail state for the right tools panel.
    pub tools_collapsed: bool,
    /// The RHAI compile check over the editor buffer: the source it was computed FOR, paired with
    /// `compile_status`'s verdict on it. Recomputed (in `poll()`) only when `editor.source` has
    /// drifted from that source — compiling on every frame regardless of keystrokes would be
    /// wasteful even though a single compile is cheap (~ms).
    ///
    /// ⚠ **`None` means NO verdict is held, and that is the state whenever the buffer is not Rhai**
    /// ([`Self::buffer_is_rhai`]). It used to be two fields — the source compiled and an
    /// `Option<String>` error — and in that shape the check ran on the buffer whatever
    /// `strategy_source` said, so a PLUGIN author's valid Rust source wore a red
    /// `● error · line N` header chip beside the `● built <sha>` its own Build had just answered.
    /// A `None` error could not have said "not checked": it read as `● compiles`. So the verdict
    /// is DROPPED in Plugin mode rather than merely hidden, and the pair lives as one value so the
    /// source and the verdict about it cannot drift apart.
    rhai_check: Option<(String, Result<(), String>)>,
    /// The editor source as of the last Save/Load/template-load — the "unsaved changes" baseline.
    /// `editor.source != saved_source` is the AMBER-dot dirty check.
    ///
    /// ⚠ **In Plugin mode a Save re-baselines to the source its sha was BUILT from, not to the
    /// buffer.** A Plugin row stores a name and a sha and never the source, so the only buffer that
    /// row can be said to have saved is the one the sha names — [`Self::plugin_built_source`]. An
    /// edit since that Build therefore keeps the chip lit through a save, which is true: nothing
    /// holds that edit.
    pub saved_source: String,
    /// [`StrategySource::Native`]'s own baseline — the `(name, params)` pair as of the last
    /// Save/Load, mirroring what `saved_source` is for Rhai and `plugin_built_source` is for
    /// Plugin. `None` means this session has neither saved nor loaded a Native selection.
    ///
    /// ⚠ **Native's dirty check used to be `editor.source != saved_source` like every other
    /// mode**, which compares the PARKED Rhai buffer — a Native save touches neither `editor.source`
    /// nor `saved_source`, so the chip could light on an unrelated stale buffer and never light on
    /// an actual unsaved param edit. `native_is_dirty` reads this field instead;
    /// `unsaved_chip_tip`'s `StrategySource::Native` arm names what a save actually captures.
    native_saved: Option<(&'static str, Vec<(String, String)>)>,
    /// The `StudioWorkspace` snapshot last written to disk (or loaded at startup). Compared each
    /// frame in `ui()`'s trailing `maybe_persist_workspace` call so a write only happens when
    /// something [`Self::workspace_snapshot`] captures actually drifted — not on every frame.
    persisted_workspace: StudioWorkspace,
    /// QA autorun (see [`StudioState::new_with_qa`] and [`QaAutorun`]): kick off one backtest OR
    /// one parameter sweep automatically on the first `ui()` frame IF a slice is already selected,
    /// so headless captures can show the results surface (a capture run can't click ▶ Run or
    /// ▶ Run Sweep). Consulted exactly once — the first frame disarms it unconditionally.
    /// [`QaAutorun::Off`] unless the caller asks for it.
    qa_autorun: QaAutorun,
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
    /// and the buffer was: a Plugin author's Rust source came back after a restart in RHAI mode,
    /// wearing the Rhai error chip and one ▶ Run away from being executed as a script.
    /// `crate::studio::workspace::StudioWorkspace::strategy_source` carries the format argument.
    pub strategy_source: StrategySource,
    /// Index into [`native_strategies`] — the Strategy pane's native dropdown. Clamped on read so
    /// a registry that shrinks between versions can never index out of bounds.
    pub native_idx: usize,
    /// The native strategy's free-form `(key, value-text)` param rows (`params_from_rows` types
    /// them at run time). Deliberately NOT a typed/spec-driven form: `vike-backtest` has no param
    /// spec to drive one from — see `vike_studio_core::spec`'s module doc.
    pub native_params: Vec<(String, String)>,
    /// [`StrategySource::Plugin`] only: the plugin's declared name — free text, since a
    /// runtime-loaded strategy has no server-owned roster to pick from the way Native's dropdown
    /// does.
    pub plugin_name: String,
    /// [`StrategySource::Plugin`] only: the sha256 the LAST successful Build returned. `None` means
    /// "not yet built" — [`Self::run_blocked_reason`] refuses Run while this is `None`, because
    /// running would ask the server for an artifact it never received.
    ///
    /// ⚠ **This doc said the shell "never sets this to `Some` itself: that is Task 6's builder
    /// client, which this crate does not call." That is what the join changed** — a successful
    /// [`crate::spawn_build`] is now the only thing that sets it, folded in by [`Self::poll`].
    pub plugin_sha: Option<String>,
    /// The editor buffer [`Self::plugin_sha`] was built FROM, when this session built it — the
    /// bytes [`Self::start_plugin_build`] SENT, carried with the build in `build_rx`, never
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
    /// successful Build leaves `plugin_sha` naming the OLD artifact, so Run would report on code
    /// the author had already replaced — silently, with entirely plausible numbers. The design
    /// makes the sha the answer to *"which code traded"*; that is only true while the sha and the
    /// buffer agree.
    ///
    /// `None` means "this session did not build it" — a sha reloaded from a saved row, whose
    /// source this shell never held. That case is NOT blocked: the saved row is the author's own
    /// record of a built artifact, and refusing it would break the reload path to guard against a
    /// drift nothing here can even measure.
    pub plugin_built_source: Option<String>,
    /// Where the BUILDER SERVICE listens — `vike-strategy-builder`, a THIRD daemon, and never
    /// [`Backend`]'s compute address. Editable in the Plugin pane; defaults to that service's own
    /// bind address, read from the service rather than spelled again here.
    pub builder_addr: String,
    /// The builder's WRITE key — LATE-BOUND by the shell exactly as [`Self::named_run_keys`] is,
    /// and `None` in a bare `cargo run -p vike-studio`.
    ///
    /// ⚠ **Its own key, and a strong one.** `vike_strategy_builder::builder`'s Decision 1 argues
    /// why this service signs under a domain separator disjoint from both the datahub's and the
    /// tradehub's: a key minted to observe a data plane must not also authenticate a request to
    /// run arbitrary `cargo` on that box. Decision 2 argues why the one scope it grants is
    /// `Write` — this verb hands source straight to a compiler. So this field can hold NEITHER of
    /// the keys beside it, and `crate::backend::plugin_build` refuses a build outright when it is `None`
    /// rather than dialling with empty key bytes and earning `bad mac`.
    pub builder_keys: Option<vike_node_proto::auth::NodeKeys>,
    /// The in-flight build (`None` when idle): the source that was SENT, and the receiver its answer
    /// arrives on — the worker-thread twin of `run_rx`/`sweep_rx`/`wf_rx`, folded by [`Self::poll`]
    /// the same way. A build is a CARGO BUILD, so this is the one worker in this shell whose
    /// legitimate wait is minutes — which is why the sent source rides WITH the receiver: the buffer
    /// is free to change underneath it, and [`Self::plugin_built_source`] must name what was built.
    build_rx: Option<PendingBuild>,
    /// The last build's outcome, or `None` if none has been asked for this session. The error side
    /// is one rendered sentence (the shape `study_last` uses and for its reason); for a compile
    /// failure that sentence is rustc's OWN diagnostics, verbatim.
    pub build_last: Option<Result<String, String>>,
    /// The Research pane: the user's own studies, and the runs every producer has left behind
    /// (`crate::panes::research`). Its `host` — where `user_data` is and which binary is running — is
    /// seeded by the BINARY after construction, exactly like [`Self::store_is_remote`] and
    /// [`Self::backend`], because a library resolves no project root.
    pub research: ResearchPane,
    /// The in-flight study dispatch's receiver (`None` when idle) — the Research pane's
    /// worker-thread twin of `run_rx`/`sweep_rx`/`wf_rx`, and folded by `poll()` the same way.
    study_rx: Option<Receiver<Result<StudyRun, StudyRunError>>>,
    /// The last study dispatch's outcome, or `None` if none has been asked for this session.
    ///
    /// The error side is the runner's own WORDS rather than its type: `StudyRunError` is not
    /// `Clone`, this slot is also where a RECIPE that would not parse lands (a refusal that never
    /// reached the runner at all), and nothing in the shell branches on the variant — so one
    /// flattened sentence is the honest shape. That runner's `Display` is one line per variant and
    /// every line names what to do about it.
    pub study_last: Option<Result<StudyRun, String>>,
    /// WHICH producer's results the central panel is showing — see [`CenterView`].
    pub center: CenterView,
    /// WHERE a Run/Sweep/Walk-Forward executes. EVERY variant is a DAEMON now, and that is the
    /// whole of it since `Backend::Local` was deleted
    /// (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`): nothing
    /// here simulates in this process. [`Backend::Remote`] offloads to the COMPUTE daemon over
    /// TCP (⚠ this said `vike-datahub` until 2026-09-20 and the default address AGREED with it,
    /// which is what made it a bug rather than a typo), and [`Backend::Named`] asks that same
    /// daemon to run a strategy it already holds
    /// (`docs/decisions/0064-a-named-run-carries-no-source.md`) - the one backend an OBSERVE
    /// credential can reach. A runtime toggle like `strategy_source`, held on the state struct;
    /// not disk-persisted.
    pub backend: Backend,
    /// The OBSERVE node key [`Backend::Named`] signs its dial with — LATE-BOUND by the shell, the
    /// way `vike_app_core::data::md_session::MdSession::set_key_name` is, and `None` in a bare
    /// `cargo run -p vike-studio`.
    ///
    /// ⚠ **Observe and never Control, which is the whole point of the backend it serves.** The
    /// datahub CONTROL key also compiles client-supplied Rhai, runs any artifact in the plugin
    /// directory and — on the data plane — backfills and deletes, so this field can only ever
    /// carry the weaker key, and `vike_node_proto::auth::Scope::Read` is hard-coded at the dial
    /// rather than chosen here. The Control key Studio MAY hold since the owner's 2026-09-26 ruling
    /// is [`Self::compute_key`], a different TYPE — it cannot land in this field by mistake.
    ///
    /// ⚠ It is resolved OFF the frame thread by the shell (that resolution opens the settings
    /// database's `node_key` table) and pushed in, so nothing here reads a store.
    pub named_run_keys: Option<vike_node_proto::auth::NodeKeys>,
    /// Studio's COMPUTE key — the datahub CONTROL key that [`Backend::Remote`]'s Run, Sweep and
    /// Walk-Forward sign with — LATE-BOUND by the shell from `crate::backend::remote::COMPUTE_KEY_ENV`, and
    /// `None` in a bare `cargo run -p vike-studio` or a desktop the launcher handed no key.
    ///
    /// ⚠ **Read by the three `Backend::Remote` dispatches and nothing else**
    /// (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1: the owner's option (a),
    /// "for Studio's COMPUTE dial only"). It is a `crate::backend::remote::ComputeKey`, whose key is private
    /// to that module and signed in one place, only toward a server whose pre-auth `Welcome` is the
    /// compute daemon's — so neither [`Self::named_run_keys`]' dial, the builder's, nor any datahub
    /// dial of the desktop's can be handed it: each takes a `NodeKeys`, and this is not one.
    pub compute_key: Option<crate::backend::remote::ComputeKey>,
    /// The roster [`Backend::Named`] last learned from its daemon, plus whether that daemon's lane
    /// is ARMED — `None` until the picker asks.
    ///
    /// ⚠ **An UNARMED daemon ANSWERS but names NOTHING**, so `armed: false` must render the
    /// switch's name rather than the empty list that arrives with it —
    /// `vike_datahub_client::named_run::NamedRoster::unarmed_note` is the sentence, and without it
    /// this pane would show a blank roster indistinguishable from a daemon that holds no
    /// strategies. That is 0064's decision 8: the arming gates the ROSTER too (its leg 3 — naming
    /// the operator's own compiled-in strategies is itself the disclosure), and it rides in the
    /// ANSWER rather than in the capability string so an OLD server and an unarmed one stay two
    /// different sentences.
    pub named_roster: Option<Result<vike_datahub_client::named_run::NamedRoster, String>>,
    /// The in-flight roster fetch, if one is running.
    ///
    /// ⚠ **A worker, not a frame-thread dial, and the first draft of this pane got that wrong.**
    /// The VERB is trivial — a compile-time const roster, no store read, no engine — which is
    /// exactly the reasoning that put it on the update loop. What that reasoning missed is the
    /// DIAL: `vike_datahub_client`'s `CONNECT_TIMEOUT` is ten seconds per resolved address and the
    /// name resolution above it is unbounded, so a stale tunnel or a dark IPv6 route freezes the
    /// GUI rather than slowing it. Same rule `crates/vike-app-core/src/data/backfill_wire.rs` states for
    /// its own dial.
    pub named_roster_rx: Option<
        std::sync::mpsc::Receiver<Result<vike_datahub_client::named_run::NamedRoster, String>>,
    >,
    /// Which name off that roster [`Backend::Named`] will run — `None` until one is picked.
    ///
    /// ⚠ **A `String` rather than an index into [`native_strategies`], and that is not a style
    /// choice.** The server's roster is a DIFFERENT set from this binary's compiled-in one in both
    /// directions (0064's decision 7): it drops the simulator-only arms that live beside the Rhai
    /// compiler, and it carries the operator's own compiled-in user strategies, which this shell
    /// has never had a list of. An index into the local roster could not name the second kind at
    /// all, and would silently name the WRONG strategy for the first.
    pub named_strategy: Option<String>,
}

impl StudioState {
    /// The ordinary constructor: restore the persisted workspace, no QA overrides. Reads NO
    /// environment and NO credential store — a library takes its configuration as parameters. The
    /// two QA hooks this used to read here (the forced tool tab and the autorun flag) are now
    /// [`new_with_qa`](Self::new_with_qa) arguments that `vike-desktop`'s `main.rs` resolves, and
    /// `chat_keys` is the same treatment for the AI-provider keys the ChatPane used to load out of
    /// `<project>/settings/secrets.env` itself (split-plane I8 — see [`ChatApiKeys`]).
    ///
    /// `state_dir` is where the per-store state files land (see the field doc) — the caller's
    /// stand-in for the concrete store's `root()`, which the trait deliberately does not carry.
    pub fn new(store: StoreHandle, state_dir: PathBuf, chat_keys: ChatApiKeys) -> Self {
        Self::new_with_qa(store, state_dir, chat_keys, None, QaAutorun::Off)
    }

    /// [`new`](Self::new) plus the two headless-capture QA overrides, supplied by the CALLER:
    ///
    /// - `qa_tab` — a raw tab name (`sweep|strategy|data|indicators|saved|chat`, parsed by
    ///   [`RightTab::from_qa_str`]; garbage is ignored, never a panic). `Some` forces that tool tab,
    ///   expands the tools panel, and — load-bearing — puts the session in
    ///   `qa_workspace_readonly` so a capture run can never write the FORCED state over the user's
    ///   real `studio_workspace.json`.
    /// - `qa_autorun` — [`QaAutorun`]: kick off one backtest ([`QaAutorun::Run`]) or one parameter
    ///   sweep ([`QaAutorun::Sweep`]) on the first `ui()` frame IF a slice is selected, so a
    ///   headless capture can show the results surface (it cannot click ▶ Run or ▶ Run Sweep).
    ///
    /// The values used to be read straight from `VIKE_STUDIO_TAB` / `VIKE_STUDIO_AUTORUN` inside
    /// this constructor — a LIBRARY reading process environment its caller can neither see nor
    /// override, which is the `Layer::Library` class `crates/vike-ops/tests/settings_secrets/settings_registry.rs`
    /// ratchets down. `vike-desktop`'s `main.rs` does the two reads now; nothing else in the workspace
    /// wants them (the `studio_shot` capture example poses `right_tab` on the struct directly).
    pub fn new_with_qa(
        store: StoreHandle,
        state_dir: PathBuf,
        chat_keys: ChatApiKeys,
        qa_tab: Option<&str>,
        qa_autorun: QaAutorun,
    ) -> Self {
        let ws = load_workspace(&workspace_read_path(&state_dir));
        Self::with_workspace(store, state_dir, chat_keys, qa_tab, qa_autorun, ws)
    }

    /// [`new_with_qa`](Self::new_with_qa) over an already-loaded workspace snapshot — the restore
    /// half, split from the READ so a test can hand it a snapshot. `new_with_qa`'s read resolves
    /// `<project>/settings/state` by walking up from the working directory, which under
    /// `cargo test` is the checkout's own settings root: a test constructing through it starts from
    /// whatever workspace that box last wrote.
    fn with_workspace(
        store: StoreHandle,
        state_dir: PathBuf,
        chat_keys: ChatApiKeys,
        qa_tab: Option<&str>,
        qa_autorun: QaAutorun,
        ws: StudioWorkspace,
    ) -> Self {
        // ⚠ The CONSTRUCTION walk is deliberately still synchronous, unlike the ⟳ Refresh one
        // (`spawn_catalog_refresh`, and `crate::backend::catalog`'s module doc for the whole argument). It
        // runs once, before this shell has drawn a frame, and both the QA autorun hook and the
        // posed-state fixtures in `crates/vike-studio/tests/common/mod.rs` read a POPULATED picker
        // the instant the constructor returns. Moving it off-thread is a separate change with its
        // own first-frame consequences; it is not the paint-thread stall this pair of calls was
        // measured in.
        let mut picker = SlicePicker::default();
        picker.refresh(store.as_ref());
        let mut data_browser = DataBrowserPane::default();
        data_browser.refresh(store.as_ref());
        let saved = SavedPane::load(&state_dir.join(SAVED_STRATEGIES_FILE));
        let mut editor = EditorPane::default();
        // Restore the last editor buffer only if the workspace actually captured one — an empty
        // `editor_source` (the field's `Default`, e.g. first launch / no file yet) keeps the
        // known-good default starter script instead of blanking the editor.
        if !ws.editor_source.is_empty() {
            editor.source = ws.editor_source.clone();
        }
        let saved_source = editor.source.clone();
        // The Native dropdown is restored by NAME, like a Native saved row's Load: an index is only
        // meaningful against the roster that wrote it, and a registry that gained or lost a strategy
        // since would silently restore a DIFFERENT one. A name nothing matches keeps row 0.
        let native_idx =
            native_strategies().iter().position(|n| *n == ws.native_strategy).unwrap_or(0);
        // Clamp against `TEMPLATES` shrinking/growing across versions — an out-of-range
        // stored index must never panic the `ComboBox::show_index` call in `ui()`.
        let template_idx = ws.template_idx.min(TEMPLATES.len().saturating_sub(1));
        // QA: a caller-supplied tab name (<sweep|strategy|data|indicators|saved|chat>) forces the
        // initial tool tab and expands the tools panel for headless per-tab captures — the
        // vike-studio twin of the desktop's style/scale capture hooks. Absent/garbage restores the
        // workspace. While the override is active, workspace persistence is disabled for the
        // session (`qa_workspace_readonly`) so the forced values can never clobber the user's file.
        let qa_tab = qa_tab.and_then(RightTab::from_qa_str);
        // ⚠ The SWEEP autorun arms this too, and it MUST: unlike [`QaAutorun::Run`] — which touches
        // only `center`, a field no workspace persists — the sweep arm loads
        // [`SWEEP_CAPTURE_SCRIPT`] into the editor, and `editor.source` IS persisted. Deriving the
        // guard from `qa_tab` alone was safe while every autorun was read-only; it stopped being
        // safe the moment one of them wrote, and a capture (or a stale `VIKE_STUDIO_AUTORUN=sweep`
        // in a dev shell) overwriting somebody's saved strategy is exactly the failure this flag
        // was invented to prevent.
        let qa_workspace_readonly = qa_tab.is_some() || qa_autorun == QaAutorun::Sweep;
        let (right_tab, tools_collapsed) = match qa_tab {
            Some(t) => (t, false),
            None => (ws.right_tab, ws.tools_collapsed),
        };
        let editor_collapsed = ws.editor_collapsed;
        let mut st = Self {
            store,
            state_dir,
            store_is_remote: false,
            editor,
            picker,
            tab: ResultsTab::default(),
            running: false,
            last: None,
            run_rx: None,
            grid: Vec::new(),
            sweep_rx: None,
            sweep_last: None,
            wf_rx: None,
            wf_last: None,
            template_idx,
            chat: ChatPane::new(chat_keys),
            chat_rx: None,
            indicators: IndicatorsPane::default(),
            data_browser,
            saved,
            catalog_rx: None,
            right_tab,
            editor_collapsed,
            tools_collapsed,
            compare_rx: None,
            // Seeded just below, once the restored source says whether the buffer is Rhai.
            rhai_check: None,
            saved_source,
            // The restored (name, params) counts as saved, exactly like `saved_source` starting
            // equal to the restored `editor.source` — a freshly opened session shows no unsaved
            // chip until something actually changes.
            native_saved: Some((
                native_strategies()
                    .get(native_idx.min(native_strategies().len().saturating_sub(1)))
                    .copied()
                    .unwrap_or(""),
                ws.native_params.clone(),
            )),
            // Replaced just below by the snapshot of the state being built, so the first frame's
            // `maybe_persist_workspace` finds nothing drifted and writes nothing.
            persisted_workspace: StudioWorkspace::default(),
            qa_autorun,
            qa_workspace_readonly,
            last_saved_name: None,
            strategy_source: ws.strategy_source,
            native_idx,
            native_params: ws.native_params,
            plugin_name: ws.plugin_name,
            // ⚠ NOT restored, and deliberately never persisted: a sha from a previous session has
            // no recorded source (`plugin_built_source` would be `None`), so the staleness guard
            // could not see an edit made after that Build and before the shutdown, and Run would
            // dispatch an artifact of unknown provenance under the restored buffer. A restored
            // Plugin session is "not built yet" until Build answers — and the builder is
            // content-addressed, so rebuilding an unchanged buffer is a cache hit.
            plugin_sha: None,
            plugin_built_source: None,
            builder_addr: vike_strategy_builder::client::default_addr(),
            builder_keys: None,
            build_rx: None,
            build_last: None,
            backend: Backend::default(),
            named_run_keys: None,
            compute_key: None,
            named_roster: None,
            named_roster_rx: None,
            named_strategy: None,
            // No `host`: the BINARY seeds it after construction (see the field doc), so a Studio
            // built by a test or by a headless capture lists nothing and arms nothing rather than
            // walking for a project directory a library has no business resolving.
            research: ResearchPane::default(),
            study_rx: None,
            study_last: None,
            center: CenterView::default(),
        };
        // Seed the compile cache eagerly rather than report a stale/blank status for one frame
        // before `poll()` first runs — but only when the RESTORED buffer is Rhai. A session that
        // comes back in Plugin mode holds Rust, and the same rule `poll` applies from then on
        // applies here: no Rhai verdict is held about it, not even for the first frame.
        if st.buffer_is_rhai() {
            let verdict = crate::panes::editor::compile_status(&st.editor.source);
            st.rhai_check = Some((st.editor.source.clone(), verdict));
        }
        st.persisted_workspace = st.workspace_snapshot();
        st
    }
}

// `StudioState`'s one `impl` is split by concern into the child modules below; this file keeps the
// types, the struct, the constructor and the worker-poll helpers they share.
mod center;
mod dispatch;
mod persistence;
mod shell;
mod strategy;
mod tool_panes;
mod workers;
// The persisted-workspace snapshot (`StudioWorkspace`) and the path helpers that resolve its file:
// a type module rather than a slice of the `impl`. `pub(crate)` because `lib.rs` re-exports its
// public half (`pub use studio::workspace::{...}`).
pub(crate) mod workspace;

#[path = "studio_tests.rs"]
#[cfg(test)]
mod studio_tests;
