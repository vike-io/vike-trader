//! `StudioState` — the Studio shell (Run toolbar | editor | Sweep/Validate | results). Holds the
//! store handle and wires the panes to worker-thread Run/Sweep/Walk-Forward. `poll()` folds each
//! worker's outcome in; none of the three ever runs on the egui update loop.
//!
//! ...and neither does the ⟳ Refresh CATALOG walk any more — see [`crate::catalog`], which carries
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

use crate::chat::{ChatApiKeys, ChatOutcome, ChatPane, summary_of};
use crate::data_browser::DataBrowserPane;
use crate::editor::EditorPane;
use crate::indicators::IndicatorsPane;
use crate::picker::SlicePicker;
use crate::remote::Backend;
use crate::research::{ResearchAction, ResearchPane};
use crate::results::{ResultsTab, results_ui};
use crate::saved::{
    SavedAction, SavedPane, SavedStrategy, StrategySource, comparison_rows, save_strategies,
};
use crate::workspace::{
    StudioWorkspace, load_workspace, save_workspace, workspace_read_path, workspace_write_path,
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
const NO_PROVIDER_KEY: &str =
    "No provider key found — set ANTHROPIC_API_KEY or CEREBRAS_API_KEY in the workspace .env.";
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
pub const RHAI_WRITER_BLOCKED_IN_PLUGIN: &str = "Plugin mode: the editor holds this plugin's Rust \
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

// The persisted-workspace filename moved to `crate::workspace` (settings-unification Phase 2):
// it is no longer colocated with the store like `SAVED_STRATEGIES_FILE` — which tab is open
// describes this USER's screen, not that store's data — so the basename now sits beside the two
// path helpers that resolve it (`workspace_read_path` / `workspace_write_path`).

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
    /// The user's own STUDIES, and the runs they left behind — see `crate::research`.
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

    /// The tab strip's button label for this tool.
    pub fn label(self) -> &'static str {
        match self {
            RightTab::Sweep => "Sweep",
            RightTab::Strategy => "Strategy",
            RightTab::Data => "Data",
            RightTab::Indicators => "Indicators",
            RightTab::Saved => "Saved",
            RightTab::Research => "Research",
            RightTab::Chat => "AI Chat",
        }
    }

    /// The vertical icon rail's icon for this tool — one registry icon per tool, the same one its
    /// pane header shows; the rail shows `icon()` alone and names it `label()` — its accessible
    /// name and its hover tooltip (`icons::named`, see `ui()`).
    pub fn icon(self) -> Icon {
        match self {
            RightTab::Sweep => icons::SWEEP,
            RightTab::Strategy => icons::STRATEGY,
            RightTab::Data => icons::DATA,
            RightTab::Indicators => icons::INDICATORS,
            RightTab::Saved => icons::SAVED,
            RightTab::Research => icons::RESEARCH,
            RightTab::Chat => icons::CHAT,
        }
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
pub fn qa_sweep_ladder(value: &str) -> String {
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
/// event-driven simulator and a study's do not, so `crate::results::results_ui` (which renders a
/// `vike_analytics::BacktestResult`) and `crate::research::study_result_ui` (which renders a
/// `vike_studio_core::StudyRun`) each name their own producer instead of sharing a column. Every
/// dispatch sets this to its own view, so the panel shows the result of the thing that was last
/// asked for rather than obeying a precedence rule nobody can predict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CenterView {
    /// `crate::results::results_ui` over the last Run / Sweep / Walk-Forward.
    #[default]
    Backtest,
    /// `crate::research::study_result_ui` over the last study run.
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
pub fn next_tab(t: RightTab) -> RightTab {
    let i = RightTab::ALL.iter().position(|&x| x == t).unwrap_or(0);
    RightTab::ALL[(i + 1) % RightTab::ALL.len()]
}

/// A tool pane's heading — its icon and its title at the Title role, then the kit's strip rule. It
/// is the ONE text that says which pane the shared right panel shows
/// (`crates/vike-studio/tests/studio_shell_render.rs`'s `pane_title` reads it, which is why the
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
    catalog_rx: Option<Receiver<crate::catalog::CatalogLoad>>,
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
    /// `crate::workspace::StudioWorkspace::strategy_source` carries the format argument.
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
    /// the keys beside it, and `crate::plugin_build` refuses a build outright when it is `None`
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
    /// (`crate::research`). Its `host` — where `user_data` is and which binary is running — is
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
    /// ⚠ It is resolved OFF the frame thread by the shell (that resolution opens
    /// `<project>/settings/node.env`) and pushed in, so nothing here reads a file.
    pub named_run_keys: Option<vike_node_proto::auth::NodeKeys>,
    /// Studio's COMPUTE key — the datahub CONTROL key that [`Backend::Remote`]'s Run, Sweep and
    /// Walk-Forward sign with — LATE-BOUND by the shell from `crate::remote::COMPUTE_KEY_ENV`, and
    /// `None` in a bare `cargo run -p vike-studio` or a desktop the launcher handed no key.
    ///
    /// ⚠ **Read by the three `Backend::Remote` dispatches and nothing else**
    /// (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1: the owner's option (a),
    /// "for Studio's COMPUTE dial only"). It is a `crate::remote::ComputeKey`, whose key is private
    /// to that module and signed in one place, only toward a server whose pre-auth `Welcome` is the
    /// compute daemon's — so neither [`Self::named_run_keys`]' dial, the builder's, nor any datahub
    /// dial of the desktop's can be handed it: each takes a `NodeKeys`, and this is not one.
    pub compute_key: Option<crate::remote::ComputeKey>,
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
    /// override, which is the `Layer::Library` class `crates/vike-ops/tests/settings_registry.rs`
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
        // (`spawn_catalog_refresh`, and `crate::catalog`'s module doc for the whole argument). It
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
            let verdict = crate::editor::compile_status(&st.editor.source);
            st.rhai_check = Some((st.editor.source.clone(), verdict));
        }
        st.persisted_workspace = st.workspace_snapshot();
        st
    }

    /// The native strategy name currently selected in the Strategy pane. Clamped against the
    /// registry roster so a stale index is never an out-of-bounds panic.
    pub fn native_name(&self) -> &'static str {
        let roster = native_strategies();
        roster[self.native_idx.min(roster.len().saturating_sub(1))]
    }

    /// What [`Backend::Named`] runs: the name picked off the SERVER's roster, with this pane's
    /// params rows.
    ///
    /// ⚠ **A SCRIPT is passed through unchanged rather than substituted**, deliberately. Quietly
    /// swapping in a native name would run something the operator did not choose and report it as
    /// the answer; instead the spec reaches `crate::remote::to_named_run_spec`, which refuses it
    /// with the sentence that explains why a named run carries no source and what to do instead.
    ///
    /// With no name picked this falls back to [`Self::current_spec`], so the pane's own native
    /// selection still runs — which is right on a daemon whose roster overlaps this binary's.
    pub fn named_spec(&self) -> StrategySpec {
        match (&self.strategy_source, &self.named_strategy) {
            // A Plugin joins Rhai here for the same reason: it is not on the SERVER's compiled-in
            // roster either (`crate::remote::to_named_run_spec` refuses it by name, the same way it
            // refuses a script), so falling through to `current_spec` is the honest answer rather
            // than inventing a substitution the daemon cannot resolve.
            (StrategySource::Rhai, _)
            | (StrategySource::Plugin, _)
            | (StrategySource::Native, None) => self.current_spec(),
            (StrategySource::Native, Some(name)) => {
                StrategySpec::native(name.clone(), params_from_rows(&self.native_params))
            }
        }
    }

    /// What every Run/Sweep/Walk-Forward/Compare-from-the-toolbar executes right now — the ONE
    /// place `strategy_source` is turned into a runnable [`StrategySpec`].
    pub fn current_spec(&self) -> StrategySpec {
        match self.strategy_source {
            StrategySource::Rhai => StrategySpec::rhai(self.editor.source.clone()),
            StrategySource::Native => {
                StrategySpec::native(self.native_name(), params_from_rows(&self.native_params))
            }
            // No params editor for Plugin mode yet, so this always resolves an EMPTY params table;
            // a dedicated params pane is later work, the same way Native's grew one after its
            // spec did. An empty table is a valid empty TOML document on the plugin's side of the
            // C-ABI, so a plugin runs on its own `build` fallbacks rather than on nothing — which
            // is exactly what the template's params test pins.
            StrategySource::Plugin => StrategySpec::plugin(
                self.plugin_name.clone(),
                self.plugin_sha.clone().unwrap_or_default(),
                vike_studio_core::empty_params(),
            ),
        }
    }

    /// Why Run/Sweep/Walk-Forward are disabled right now, if they are — `None` means "go ahead".
    ///
    /// The one reason this shell's plumbing adds: [`StrategySource::Plugin`] names an artifact by
    /// sha, and running with `plugin_sha` still `None` would ask the server for a build that never
    /// happened. [`crate::remote::to_wire_spec`] would happily serialize an EMPTY sha onto the wire
    /// — nothing downstream refuses it structurally — so this refusal is what actually stops that
    /// request from being sent, not a side effect of some other check.
    /// ⚠ **The second reason is STALENESS, and it is the other half of what makes a sha honest.**
    /// A Build binds the run to the artifact that was built; an edit afterwards leaves the sha
    /// naming the previous one, so a Run would report on code the author had already replaced —
    /// with no error and entirely plausible numbers. Only a sha THIS session built is checked
    /// ([`Self::plugin_built_source`] says why a reloaded one is not).
    pub fn run_blocked_reason(&self) -> Option<&'static str> {
        if self.strategy_source != StrategySource::Plugin {
            return None;
        }
        if self.plugin_sha.is_none() {
            return Some(
                "Build the plugin first — Run needs a sha, and no Build has returned one yet.",
            );
        }
        if self.plugin_built_source.as_deref().is_some_and(|src| src != self.editor.source) {
            return Some(
                "The editor changed since the last Build — Build again, or the run would report \
                 on the previous artifact.",
            );
        }
        None
    }

    /// Why Run is disabled — the toolbar's and the empty panel's, which render this ONE answer
    /// so their state and their reason cannot disagree — or `None` when it may go. The condition
    /// is exactly the one both buttons spelled for themselves (not running, a picked slice,
    /// [`Self::run_blocked_reason`]); what is new is that the reason reaches the pointer. egui
    /// shows `on_hover_text` only on an ENABLED widget, so the reason both buttons carried there
    /// was never seen.
    pub fn run_disabled_reason(&self) -> Option<&'static str> {
        if self.running {
            return Some(RUN_BUSY);
        }
        if self.picker.selected().is_none() {
            return Some(PICK_A_SLICE);
        }
        self.run_blocked_reason()
    }

    /// Why Run Sweep is disabled, or `None`. `grid_ok` is the caller's parse of the grid. The
    /// condition is the button's own; a Plugin without a sha is refused inside `start_sweep`, as
    /// before.
    fn sweep_disabled_reason(&self, grid_ok: bool) -> Option<&'static str> {
        if self.running {
            return Some(RUN_BUSY);
        }
        if self.sweep_rx.is_some() {
            return Some(SWEEP_BUSY);
        }
        if self.picker.selected().is_none() {
            return Some(PICK_A_SLICE);
        }
        if !grid_ok {
            return Some(SEED_A_GRID);
        }
        None
    }

    /// Why Walk-Forward is disabled, or `None`: no walk in flight and a picked slice, its button's
    /// own condition.
    fn walk_forward_disabled_reason(&self) -> Option<&'static str> {
        if self.wf_rx.is_some() {
            return Some(WALK_FORWARD_BUSY);
        }
        if self.picker.selected().is_none() {
            return Some(PICK_A_SLICE);
        }
        None
    }

    /// Why Send is disabled, or `None` — its button's own condition, one reason at a time.
    fn send_disabled_reason(&self) -> Option<&'static str> {
        if !self.chat.has_key() {
            return Some(NO_PROVIDER_KEY);
        }
        if self.chat.running || self.chat_rx.is_some() {
            return Some(COPILOT_BUSY);
        }
        if self.picker.selected().is_none() {
            return Some(PICK_A_SLICE);
        }
        if self.chat.input.trim().is_empty() {
            return Some(NOTHING_TO_SEND);
        }
        None
    }

    /// Whether the editor buffer is RHAI source right now — the one question the Rhai compile
    /// check, the editor header's `● compiles` / `● error · line N` chip and the inline error
    /// banner under it all answer. `false` means none of the three may speak about the buffer.
    ///
    /// - [`StrategySource::Rhai`]: yes — it is the script every Run executes.
    /// - [`StrategySource::Plugin`]: **no.** The buffer is RUST that Build hands to the builder
    ///   service, so a Rhai parse of it is meaningless — measured on the real GUI, a valid plugin
    ///   read `● error · line 4` beside the `● built <sha>` of its own successful Build. The Build
    ///   result in the Strategy pane is the verdict on this buffer.
    /// - [`StrategySource::Native`]: yes, and deliberately. Native mode never WRITES the editor —
    ///   the registry dropdown, the param rows and a native saved row's Load all leave it alone —
    ///   so what it holds is the parked Rhai script that switching back to Rhai will run, and the
    ///   verdict is a true statement about Rhai source. The one way Rust gets there is typing it in
    ///   Plugin mode and then picking Native; the red chip that follows is then the verdict Rhai
    ///   mode WOULD return on that buffer, not a claim about what Native runs.
    ///
    /// An exhaustive `match` rather than `!= Plugin`, so a fourth source is a compile error here
    /// until somebody decides what language its buffer is.
    pub fn buffer_is_rhai(&self) -> bool {
        match self.strategy_source {
            StrategySource::Rhai | StrategySource::Native => true,
            StrategySource::Plugin => false,
        }
    }

    /// The Rhai verdict the editor header renders: `None` when no verdict is held — which is
    /// always the case while [`Self::buffer_is_rhai`] is `false` — else `compile_status`'s answer
    /// for the buffer as of the last [`Self::poll`].
    pub fn rhai_verdict(&self) -> Option<&Result<(), String>> {
        self.rhai_check.as_ref().map(|(_, verdict)| verdict)
    }

    /// Why a control that writes a RHAI script into the editor buffer is refused right now, if it
    /// is — `None` means "go ahead". There are four such writers: the Sweep pane's template
    /// `Load`, `Browse templates`' per-card `Load`, the empty results panel's `Load a template`,
    /// and the AI Copilot's `Apply to editor`. Each renders disabled on this answer with it as the
    /// hover text, and [`Self::load_template`] / [`Self::apply_copilot_result`] ask it AGAIN inside
    /// the write — the shape `start_run` gives `run_blocked_reason`: the button's state protects the
    /// button, the check inside the write protects every caller.
    ///
    /// ⚠ **Refused, rather than made to switch the source to Rhai as it loads, and the difference is
    /// the author's code.** While the buffer is not Rhai ([`Self::buffer_is_rhai`] — Plugin mode) it
    /// is RUST, and it has no other copy: a Plugin save stores the plugin's name and its Build's
    /// sha, never the source (`SavedStrategy::plugin`). A writer that flipped the mode and loaded
    /// would destroy the only copy of a plugin in one click, and one that loaded WITHOUT flipping
    /// would leave Plugin selected over a Rhai buffer that Build then hands to cargo. Refused, it
    /// costs one deliberate click — pick `Rhai script`, where the same buffer wears the Rhai verdict
    /// and a load is the ordinary Rhai-mode overwrite — and destroys nothing by itself.
    ///
    /// Tied to `buffer_is_rhai` rather than spelled as its own match, because it IS that question:
    /// a Rhai writer may write exactly when the buffer is Rhai. Native passes for the reason that
    /// function gives — its buffer is the parked Rhai script.
    pub fn rhai_writer_blocked_reason(&self) -> Option<&'static str> {
        (!self.buffer_is_rhai()).then_some(RHAI_WRITER_BLOCKED_IN_PLUGIN)
    }

    /// Put a template's `code` into the editor and re-baseline the unsaved chip (a template load is
    /// a LOAD, not an edit) — or refuse and return `false` while
    /// [`Self::rhai_writer_blocked_reason`] says so, leaving the buffer untouched.
    pub fn load_template(&mut self, code: &str) -> bool {
        if self.rhai_writer_blocked_reason().is_some() {
            return false;
        }
        self.editor.source = code.to_string();
        self.saved_source = self.editor.source.clone();
        true
    }

    /// The AI Copilot's `Apply to editor`: write the script it generated into the buffer — an EDIT,
    /// so the unsaved chip is left to show it — or refuse and return `false` while
    /// [`Self::rhai_writer_blocked_reason`] says so. The copilot writes Rhai
    /// (`vike_ai::develop_strategy_with_ledger` backtests what it wrote as a script).
    pub fn apply_copilot_result(&mut self, result: &vike_ai::AgentResult) -> bool {
        if self.rhai_writer_blocked_reason().is_some() {
            return false;
        }
        crate::chat::apply_result(&mut self.editor.source, result);
        true
    }

    /// The editor header's `● unsaved` chip hover text: what Ctrl+S would actually do about the
    /// drift the chip is reporting.
    ///
    /// ⚠ **In Plugin mode it said "Ctrl+S saves to the Saved list", and that was false twice.** A
    /// Plugin save stores the name and a sha, never the buffer, and it did not re-baseline either —
    /// so the chip never cleared, and the tooltip promised a save that could not capture the edits
    /// it was pointing at. Now a Plugin save re-baselines to what its sha was built from (see
    /// [`Self::saved_source`]), and this names which of the two situations the author is in: the
    /// buffer IS the held sha's source (a save names this code), or it is not (no Build, an edit
    /// since, or a sha reloaded from a saved row) and only a Build can make a save capture it.
    ///
    /// ⚠ **Native used to keep the Rhai sentence, and that was the same defect in the third mode.**
    /// A Native save stores the registry name and params, never the parked buffer, so "Ctrl+S saves
    /// to the Saved list" named the wrong payload — fixed the same way as Plugin, by naming what a
    /// save actually captures.
    pub fn unsaved_chip_tip(&self) -> &'static str {
        match self.strategy_source {
            StrategySource::Rhai => UNSAVED_TIP,
            StrategySource::Native => UNSAVED_TIP_NATIVE,
            StrategySource::Plugin => {
                let buffer_is_built = self.plugin_sha.is_some()
                    && self.plugin_built_source.as_deref() == Some(self.editor.source.as_str());
                if buffer_is_built { UNSAVED_TIP_PLUGIN_BUILT } else { UNSAVED_TIP_PLUGIN_UNBUILT }
            }
        }
    }

    /// [`StrategySource::Native`]'s own dirty check — see [`Self::native_saved`]'s doc for why it
    /// cannot be `editor.source != saved_source` the way the other two modes' can.
    fn native_is_dirty(&self) -> bool {
        self.native_saved.as_ref().map(|(name, params)| (*name, params.as_slice()))
            != Some((self.native_name(), self.native_params.as_slice()))
    }

    /// Path the saved-strategy list is persisted to — colocated with the per-store state
    /// directory (the caller-supplied stand-in for the store's root).
    fn saved_strategies_path(&self) -> std::path::PathBuf {
        self.state_dir.join(SAVED_STRATEGIES_FILE)
    }

    /// Persist `self.saved.strategies` to disk. A write failure is swallowed (best-effort state,
    /// like `SlicePicker::refresh`'s own `unwrap_or_default` posture) — the in-memory list stays
    /// authoritative for the rest of the session even if the disk write fails.
    fn persist_saved(&self) {
        let _ = save_strategies(&self.saved_strategies_path(), &self.saved.strategies);
    }

    /// Path the workspace snapshot is persisted to — the project state root
    /// (`<project>/settings/state/studio_workspace.json`), NOT the store, unlike
    /// `saved_strategies_path`.
    /// See `crate::workspace`'s module doc for the split and for the dual read that keeps an
    /// existing `<store_root>` file loading until this first write migrates it.
    fn workspace_path(&self) -> std::path::PathBuf {
        workspace_write_path(&self.state_dir)
    }

    /// What a workspace write would record right now — the ONE spelling of the snapshot, used both
    /// by the per-frame persist below and by the constructor (which seeds `persisted_workspace`
    /// with it), so the two cannot describe the state differently and trigger a spurious write.
    fn workspace_snapshot(&self) -> StudioWorkspace {
        StudioWorkspace {
            right_tab: self.right_tab,
            editor_collapsed: self.editor_collapsed,
            tools_collapsed: self.tools_collapsed,
            template_idx: self.template_idx,
            editor_source: self.editor.source.clone(),
            strategy_source: self.strategy_source,
            native_strategy: self.native_name().to_string(),
            native_params: self.native_params.clone(),
            plugin_name: self.plugin_name.clone(),
        }
    }

    /// Persist the current workspace snapshot iff it drifted from `persisted_workspace` since
    /// the last check — called once per frame at the end of `ui()`. The equality comparison (a
    /// handful of scalars, the buffer and the per-source selection) runs every frame, but the disk
    /// write only happens on an actual change, so typing in the editor writes once per drift rather
    /// than once per frame. A write failure is swallowed (best-effort UI state, like
    /// `persist_saved`).
    fn maybe_persist_workspace(&mut self) {
        // A `VIKE_STUDIO_TAB` capture session never writes: any drift (even one editor
        // keystroke, or a strategy-source switch) would persist the FORCED state over the user's
        // real file.
        if self.qa_workspace_readonly {
            return;
        }
        let current = self.workspace_snapshot();
        if current != self.persisted_workspace {
            let _ = save_workspace(&self.workspace_path(), &current);
            self.persisted_workspace = current;
        }
    }

    /// Fold in a `SavedAction` from `SavedPane::ui` this frame. `Load`/`Delete`/`SaveCurrent`
    /// mutate `self.saved.strategies` and persist; `CompareAll` kicks off every saved strategy's
    /// backtest over the selected slice on a worker thread (`spawn_compare_all` — the
    /// `saved.rs`-side twin of `start_run`/`start_sweep`/`start_walkforward`), so a large saved
    /// list or slice never blocks the UI thread. No-op if a compare is already in flight.
    fn handle_saved_action(&mut self, action: SavedAction) {
        match action {
            SavedAction::Load(i) => {
                if let Some(s) = self.saved.strategies.get(i) {
                    // A RHAI row's Load overwrites `editor.source` below — the fifth Rhai writer,
                    // same hazard `rhai_writer_blocked_reason` refuses for the template/copilot
                    // writers: while the buffer is Rust with no other copy (Plugin mode), that
                    // write would destroy it. The UI already disables this row's Load button for
                    // that case (`saved.rs`'s `ui`); this is the second layer, so a caller that
                    // reaches this method any other way is covered too. A Native or Plugin row
                    // never touches `editor.source`, so neither is blocked here.
                    if s.source == StrategySource::Rhai
                        && self.rhai_writer_blocked_reason().is_some()
                    {
                        return;
                    }
                    // A NATIVE entry has no script: loading it must restore the strategy SOURCE
                    // (registry name + params) instead of blanking the editor with its empty `code`.
                    self.strategy_source = s.source;
                    match s.source {
                        StrategySource::Rhai => {
                            self.editor.source = s.code.clone();
                            self.saved_source = s.code.clone();
                        }
                        StrategySource::Native => {
                            self.native_idx = native_strategies()
                                .iter()
                                .position(|n| *n == s.native)
                                .unwrap_or(self.native_idx);
                            self.native_params = s.params.clone();
                            // A Load is a baseline, exactly like Rhai's `saved_source = s.code`
                            // above — the row just loaded IS what a save would re-capture.
                            self.native_saved =
                                Some((self.native_name(), self.native_params.clone()));
                            self.right_tab = RightTab::Strategy;
                            self.tools_collapsed = false;
                        }
                        // The saved sha loads back too — a row saved AFTER a successful Build
                        // still names a real artifact.
                        //
                        // ⚠ `plugin_built_source` is cleared rather than guessed at, and the
                        // clearing is what stops this path from lying in EITHER direction. A saved
                        // row carries a name and a sha, never the source (the design: what is
                        // persisted is a SHA, not a file), so this shell genuinely does not know
                        // what that artifact was built from. Leaving the previous build's source
                        // in place would make `run_blocked_reason` compare the reloaded sha
                        // against an unrelated buffer and block a perfectly good row; inventing
                        // one would claim knowledge nothing here has. `None` means "not built by
                        // this session", which is exactly true.
                        //
                        // The residual is stated rather than hidden: a row whose artifact has
                        // since been pruned, or whose source the author edited elsewhere, still
                        // loads — and the refusal for it is the LOADER's, on the server, naming
                        // the missing artifact. That is a refusal, not a silent wrong answer.
                        StrategySource::Plugin => {
                            self.plugin_name = s.plugin_name.clone();
                            self.plugin_sha = s.plugin_sha.clone();
                            self.plugin_built_source = None;
                            self.right_tab = RightTab::Strategy;
                            self.tools_collapsed = false;
                        }
                    }
                }
            }
            SavedAction::Delete(i) => {
                if i < self.saved.strategies.len() {
                    self.saved.strategies.remove(i);
                    self.persist_saved();
                }
            }
            SavedAction::SaveCurrent => {
                let name = self.saved.save_name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                // Snapshot whatever the CURRENT strategy source is — a native entry persists its
                // registry name + param rows, a Rhai entry its script (and re-baselines the
                // unsaved-changes dot), a Plugin entry its name + sha (re-baselining to what that
                // sha was built from).
                let entry = match self.strategy_source {
                    StrategySource::Rhai => {
                        let code = self.editor.source.clone();
                        self.saved_source = code.clone();
                        SavedStrategy::rhai(name.clone(), code)
                    }
                    StrategySource::Native => {
                        self.native_saved = Some((self.native_name(), self.native_params.clone()));
                        SavedStrategy::native(
                            name.clone(),
                            self.native_name(),
                            self.native_params.clone(),
                        )
                    }
                    // A Plugin row stores a name and a sha, never the buffer — so the baseline moves
                    // to the source that sha was BUILT from, the only buffer this row can be said to
                    // have saved (`saved_source`'s doc). With no sha this session built, it does not
                    // move at all: nothing here has saved the buffer, and the chip saying so is true.
                    StrategySource::Plugin => {
                        if let Some(built) = &self.plugin_built_source {
                            self.saved_source = built.clone();
                        }
                        SavedStrategy::plugin(
                            name.clone(),
                            self.plugin_name.clone(),
                            self.plugin_sha.clone(),
                        )
                    }
                };
                match self.saved.strategies.iter_mut().find(|s| s.name == name) {
                    Some(existing) => *existing = entry,
                    None => self.saved.strategies.push(entry),
                }
                // Remember the name so an empty-name Ctrl+S re-saves HERE instead of "untitled".
                self.last_saved_name = Some(name);
                self.saved.save_name.clear();
                self.persist_saved();
            }
            SavedAction::CompareAll => {
                if self.compare_rx.is_some() {
                    return; // a compare is already in flight
                }
                self.saved.compare_error = None;
                self.saved.compare_rows = None;
                let Some(slice) = self.picker.selected() else {
                    self.saved.compare_error = Some("pick a data slice first".to_string());
                    return;
                };
                // Each saved row contributes its OWN spec, so a Rhai script and a native registry
                // strategy rank side by side in one comparison.
                let strategies: Vec<(String, StrategySpec)> =
                    self.saved.strategies.iter().map(|s| (s.name.clone(), s.spec())).collect();
                let store = self.store.clone();
                self.compare_rx = Some(spawn_compare_all(strategies, slice, store));
            }
        }
    }

    /// Compute the Indicators pane's selected indicator over the currently-picked data slice's
    /// bars (loaded synchronously — the preview compute is O(bars) and cheap; unlike Run, it does
    /// not need a worker thread). A no-slice / load-failure surfaces as the pane's own error.
    ///
    /// ⚠ This is the one store read the Studio still performs ON the paint thread since the ⟳
    /// Refresh walk moved off it (`crate::catalog`). The "cheap" above is true of the COMPUTE and
    /// of a LOCAL store; over a `RemoteHistStore` the `load_bars` below is the same
    /// connect-per-read hazard that module describes, and it stays synchronous here because it is
    /// out of that change's scope, not because it is exempt — moving it means the Indicators pane
    /// growing a receiver and a loading state of its own, the way `spawn_catalog_refresh` did.
    fn compute_indicator_preview(&mut self) {
        let Some(slice) = self.picker.selected() else {
            self.indicators.preview = None;
            self.indicators.error = Some("pick a data slice first".to_string());
            return;
        };
        // Indicators are bar-series maths; a tick slice has no bars to compute over.
        if slice.kind != SliceKind::Bars {
            self.indicators.preview = None;
            self.indicators.error =
                Some("indicators need a bar slice — pick one from the Data combo".to_string());
            return;
        }
        match self.store.load_bars(&slice.venue, slice.symbol(), &slice.interval, slice.range) {
            Ok(bars) => self.indicators.compute(&bars),
            Err(e) => {
                self.indicators.preview = None;
                self.indicators.error = Some(format!("failed to load bars: {e}"));
            }
        }
    }

    /// **Flow step 2** — send the editor buffer to the builder service, on a worker thread.
    ///
    /// No-op while one is already in flight: a second build of the same buffer would answer the
    /// same sha (the builder is content-addressed and an unchanged source is a cache hit), and a
    /// build of a DIFFERENT buffer racing the first would land whichever finished last.
    ///
    /// ⚠ **It dispatches NO run.** The sha arrives in [`Self::poll`] and unblocks the Run button;
    /// the operator presses it. That is the design's strictly-sequential ordering — and expressing
    /// it as a state change rather than a chained call is also what keeps a FAILED build from
    /// launching anything, which a chained call would have to remember not to do.
    pub fn start_plugin_build(&mut self) {
        self.dispatch_plugin_build(crate::plugin_build::spawn_build);
    }

    /// [`Self::start_plugin_build`] over any `spawn` with [`crate::plugin_build::spawn_build`]'s
    /// shape — the seam a test drives the real dispatch through with a receiver it holds the sender
    /// of, since the real spawn dials a builder service.
    fn dispatch_plugin_build(
        &mut self,
        spawn: impl FnOnce(
            String,
            Option<vike_node_proto::auth::NodeKeys>,
            String,
            String,
        ) -> Receiver<Result<String, String>>,
    ) {
        if self.build_rx.is_some() {
            return;
        }
        // Clear the previous answer before dispatching: a stale green chip beside a running build
        // is the same misreading as a stale chip beside edited source.
        self.build_last = None;
        // The bytes SENT are captured ONCE and travel with the receiver: they, not the buffer when
        // the answer lands minutes later, are what the returned sha is the build of.
        let sent = self.editor.source.clone();
        let rx = spawn(
            self.builder_addr.clone(),
            self.builder_keys.clone(),
            self.plugin_name.clone(),
            sent.clone(),
        );
        self.build_rx = Some((sent, rx));
    }

    /// Kick off a backtest on a worker thread (no-op if a slice isn't selected or one is running).
    pub fn start_run(&mut self) {
        if self.running {
            return;
        }
        let Some(slice) = self.picker.selected() else { return };
        // Claim the central panel for the BACKTEST surface: whatever happens below — a result, a
        // refusal, a worker that dies — is a backtest's answer and belongs where a backtest's
        // answers are shown. Set before the refusal arm, not after the dispatch, so a refused run
        // is VISIBLE rather than landing in `self.last` behind whichever surface was in front.
        self.center = CenterView::Backtest;
        // ⚠ **THE GUARD LIVES HERE, not only at the button.** Fix-round-1 finding: the toolbar's
        // `can_run`/keyboard-shortcut checks were the ONLY thing consulting
        // `run_blocked_reason` — a caller that reaches this function any other way (QA autorun,
        // a test, a future call site) bypassed it entirely, building a `StrategySpec::Plugin`
        // whose `plugin_sha.unwrap_or_default()` is an EMPTY STRING and dispatching it anyway.
        // Checking it INSIDE the dispatcher protects every caller at once rather than every
        // caller that remembers to ask first.
        if let Some(reason) = self.run_blocked_reason() {
            self.last = Some(Err(RunError::Data(reason.to_string())));
            return;
        }
        // ⚠ The split-plane tick refusal that stood here is GONE with the `Local` backend it
        // guarded: it fired only for a local tick replay over a remote store, and there is no
        // local path left to fire for. See `remote::Backend`'s `Default`.
        // The Named backend picks its strategy off the SERVER's roster, which is a different set
        // from this binary's compiled-in one — see `named_spec`. Every other backend runs what the
        // Strategy pane says, unchanged.
        let spec = if matches!(self.backend, Backend::Named { .. }) {
            self.named_spec()
        } else {
            self.current_spec()
        };
        // Both arms dial the COMPUTE daemon and return the SAME `Receiver<RunOutcome>`, so
        // `run_rx`/`poll` are unchanged either way. (This named a third, in-process `Local` arm
        // and called the target a vike-datahub server; both were wrong by 2026-09-20 - see 0078.)
        let rx = match &self.backend {
            // The compute key rides this arm and the two below, and no other: the Named arm signs
            // with the OBSERVE key, and its dial could not take this one if handed it.
            Backend::Remote { addr } => {
                crate::remote::spawn_run_remote(addr.clone(), self.compute_key.clone(), spec, slice)
            }
            // The NAMED run — one strategy this daemon already holds, one bounded window, one pass.
            // Same `Receiver<RunOutcome>` as both siblings, so `poll` and the results pane are
            // unchanged; every refusal (a script, a sweep, a symbol list, an open window, an
            // over-wide one) arrives on it as an `Err(RunError)` naming the bound it hit.
            Backend::Named { addr } => crate::remote::spawn_named_run_remote(
                addr.clone(),
                self.named_run_keys.clone(),
                spec,
                slice,
            ),
        };
        self.run_rx = Some(rx);
        self.running = true;
    }

    /// Fill `grid` from the script's declared `param()`s (name, default value as text) — the
    /// "Seed grid from params" button. A discovery failure (e.g. the script doesn't compile) just
    /// clears the grid rather than surfacing an error; Run/Sweep already report compile errors
    /// when the script is actually executed.
    pub fn seed_sweep_grid(&mut self) {
        self.grid = match self.strategy_source {
            StrategySource::Rhai => discover_params(&self.editor.source)
                .map(|ps| {
                    ps.into_iter().map(|(name, default)| (name, format!("{default}"))).collect()
                })
                .unwrap_or_default(),
            // No `discover_params` twin exists for native strategies (no param spec in the
            // registry — see `strategy_pane_ui`), so seed from the param rows the user typed:
            // whatever they configured is exactly the axis set worth sweeping. Non-numeric rows
            // (e.g. `symbol = BTCUSDT`) are skipped — the sweep grid is numeric by construction.
            StrategySource::Native => self
                .native_params
                .iter()
                .filter(|(k, v)| !k.trim().is_empty() && v.trim().parse::<f64>().is_ok())
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect(),
            // No params editor for Plugin mode yet (see `current_spec`'s doc), so there is no row
            // set to seed a grid from. An EMPTY grid is the honest answer, not a guess — the same
            // "seed nothing" the Native arm above would give a strategy with no param rows typed.
            StrategySource::Plugin => Vec::new(),
        };
    }

    /// Spend the [`QaAutorun`] hook — the FIRST-FRAME half of `ui()`, lifted out of it so the
    /// decision can be tested without rasterizing a shell (`ui()` calls this and nothing else
    /// consults the flag).
    ///
    /// Consulted exactly once per session: the flag is disarmed FIRST and unconditionally (a
    /// `replace`, not a read), so an arm that cannot dispatch — because the picker has nothing
    /// selectable yet — still leaves it spent. That is deliberate rather than incidental: a flag
    /// left armed fires a surprise run minutes later, the moment `picker.refresh` auto-selects row
    /// 0 as data appears, which is the review finding the original boolean hook records.
    fn take_qa_autorun(&mut self) {
        match std::mem::replace(&mut self.qa_autorun, QaAutorun::Off) {
            QaAutorun::Off => {}
            QaAutorun::Run => {
                if !self.running && self.picker.selected().is_some() {
                    self.start_run();
                }
            }
            // The sweep autorun is the ▶ "Seed grid from params" button followed by ▶ "Run Sweep",
            // in that order, because `start_sweep` returns early on an empty grid and a fresh
            // workspace has one. `qa_sweep_ladder` then widens each seeded default so the ranked
            // table has rows to rank — see its doc for why a one-combination sweep is the same
            // empty-frame failure this hook exists to remove.
            QaAutorun::Sweep => {
                if !self.running && self.sweep_rx.is_none() && self.picker.selected().is_some() {
                    // ⚠ THE SOURCE SWAP IS LOAD-BEARING, not a preference. A sweep varies what
                    // `param()` declared, and `crates/vike-studio/src/editor.rs`'s
                    // `DEFAULT_SCRIPT` declares its lookbacks with `const` — so seeding a grid
                    // from the shipped default yields an EMPTY grid, `start_sweep` returns early,
                    // and the capture renders the empty form this hook exists to remove. The
                    // swapped-in script is the SAME strategy with the same defaults; its own doc
                    // carries the argument. Persistence is already off for this arm (see
                    // `new_with_qa`'s `qa_workspace_readonly`), so the user's file is untouched.
                    self.strategy_source = StrategySource::Rhai;
                    self.editor.source = crate::editor::SWEEP_CAPTURE_SCRIPT.to_string();
                    // Re-baseline, or the editor shows an amber unsaved-changes dot in the frame.
                    self.saved_source = self.editor.source.clone();
                    self.seed_sweep_grid();
                    for (_, csv) in &mut self.grid {
                        *csv = qa_sweep_ladder(csv);
                    }
                    self.start_sweep();
                }
            }
        }
    }

    /// Kick off a parameter sweep on a worker thread: no-op if a run or sweep is already in
    /// flight, no slice is selected, or every grid row is empty/unparseable.
    fn start_sweep(&mut self) {
        if self.running || self.sweep_rx.is_some() {
            return;
        }
        let Some(slice) = self.picker.selected() else { return };
        let grid: Vec<(String, Vec<f64>)> = self
            .grid
            .iter()
            .filter_map(|(n, csv)| {
                let v: Vec<f64> = csv.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                (!v.is_empty()).then(|| (n.clone(), v))
            })
            .collect();
        if grid.is_empty() {
            return;
        }
        self.center = CenterView::Backtest;
        // ⚠ **Fix-round-1 CRITICAL finding.** This dispatcher never consulted
        // `run_blocked_reason` — only the toolbar's Run button did — so a Plugin spec with no sha
        // reached `spawn_sweep_remote` with `plugin_sha.unwrap_or_default()`'s EMPTY STRING as if
        // it named a real artifact. Checked HERE, inside the dispatcher, so no caller (a future
        // button, a keyboard shortcut, a test) can bypass it the way the button-only check could.
        if let Some(reason) = self.run_blocked_reason() {
            self.sweep_last = Some(Err(RunError::Data(reason.to_string())));
            return;
        }
        // ⚠ THE NAMED BACKEND HAS NO SEARCH, and this refusal is the bound rather than a missing
        // feature. `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 3 makes the
        // single-point shape the LARGEST of that verb's bounds, and a structural one: the request
        // type has no field a grid could occupy, so `expand_paramscan_overrides`' unchecked
        // `product()` over client-supplied arrays is unreachable. Growing a search dimension is
        // that record's FIRST reopener, not a feature request.
        if let Some(msg) = crate::remote::named_backend_search_refusal(&self.backend, "sweep") {
            self.sweep_last = Some(Err(RunError::Data(msg)));
            return;
        }
        let spec = self.current_spec();
        let rx = match &self.backend {
            Backend::Remote { addr } => crate::remote::spawn_sweep_remote(
                addr.clone(),
                self.compute_key.clone(),
                spec,
                slice,
                grid,
            ),
            // Unreachable: the refusal above returns first. Spelled out rather than left to a
            // catch-all so a third backend cannot land here silently.
            Backend::Named { .. } => return,
        };
        self.sweep_rx = Some(rx);
    }

    /// Kick off a walk-forward validation run (n_splits fixed at 4) on a worker thread: no-op if
    /// no slice is selected or one is already in flight.
    fn start_walkforward(&mut self) {
        if self.wf_rx.is_some() {
            return;
        }
        let Some(slice) = self.picker.selected() else { return };
        self.center = CenterView::Backtest;
        // ⚠ **Fix-round-1 CRITICAL finding — see `start_sweep`'s twin comment.** Checked inside the
        // dispatcher, not only at a button, so no caller can reach `spawn_walkforward_remote` with
        // an empty-sha Plugin spec by going around a UI check that happened to exist.
        if let Some(reason) = self.run_blocked_reason() {
            self.wf_last = Some(Err(RunError::Data(reason.to_string())));
            return;
        }
        // Same bound as the sweep above: a walk-forward carries a SPLIT COUNT, which is a cost term
        // the named run's request type has no field for. See `named_backend_search_refusal`.
        if let Some(msg) =
            crate::remote::named_backend_search_refusal(&self.backend, "walk-forward")
        {
            self.wf_last = Some(Err(RunError::Data(msg)));
            return;
        }
        let spec = self.current_spec();
        let rx = match &self.backend {
            Backend::Remote { addr } => crate::remote::spawn_walkforward_remote(
                addr.clone(),
                self.compute_key.clone(),
                spec,
                slice,
                4,
            ),
            // Unreachable: the refusal above returns first — see the sweep's twin.
            Backend::Named { .. } => return,
        };
        self.wf_rx = Some(rx);
    }

    /// The window a study run is ASKED about: the picked data slice's range, or the whole store
    /// when nothing is picked.
    ///
    /// ⚠ Not a gate. A study reads whatever series it likes through its own context verbs, so the
    /// window is a fact the run RECORDS (`vike_studio_core::StudyRunRequest::window` calls it *"the
    /// window the run is ASKED about"*) and the default its read verbs take — never a restriction
    /// the Studio imposes. Requiring a slice before a study could run would be an invented
    /// coupling: the backtest path needs one because it REPLAYS that exact series, and a study does
    /// not.
    fn study_window(&self) -> TsRange {
        self.picker.selected().map_or_else(TsRange::all, |s| s.range)
    }

    /// That same window as the sentence the Research pane shows, so the operator can see which of
    /// the two answers above they are about to get.
    fn study_window_label(&self) -> String {
        match self.picker.selected() {
            Some(s) => format!("{} · {} · {}", s.venue, s.symbol(), s.interval),
            None => "the whole store (no data slice picked)".to_string(),
        }
    }

    /// Kick off a STUDY on a worker thread: no-op if one is already in flight, no study is
    /// selected, or the pane has no host to mint a run into.
    ///
    /// The Research pane's twin of [`Self::start_run`], and deliberately the same shape — a
    /// `spawn_*` returning a `Receiver` that [`Self::poll`] folds, so a study never runs on the
    /// egui update loop. It reaches `vike_studio_core::run_study_plan`, which holds the only
    /// `match` on a study's TIER: this function does not know, and must not learn, whether the row
    /// the user clicked is interpreted or compiled.
    ///
    /// A recipe that will not parse is refused HERE and nothing is dispatched — the pane read it
    /// while building the plan, so the sentence can name the file the picker chose.
    pub fn start_study(&mut self) {
        if self.study_rx.is_some() {
            return;
        }
        let window = self.study_window();
        let Some(plan) = self.research.plan(self.store.clone(), window) else { return };
        self.center = CenterView::Study;
        match plan {
            Ok(plan) => {
                self.study_last = None;
                self.study_rx = Some(spawn_study(plan));
            }
            Err(why) => self.study_last = Some(Err(why)),
        }
    }

    /// Kick off a chat -> strategy round trip on a worker thread: no-op if one is already running,
    /// no data slice is selected (the AI needs a (venue,symbol,interval) to backtest against — the
    /// same slice the Run toolbar uses), the input box is empty, or the selected provider has no key.
    fn start_chat_send(&mut self) {
        if self.chat.running || self.chat_rx.is_some() {
            return;
        }
        if self.chat.input.trim().is_empty() || !self.chat.has_key() {
            return;
        }
        let Some(slice) = self.picker.selected() else { return };
        let store = self.store.clone();
        let symbol = slice.symbol().to_string();
        // The AI copilot's cross-session memory (`vike_ai::ledger`), colocated with the per-store
        // state directory exactly like `studio_strategies.json` (unlike `studio_workspace.json`,
        // which lives in the settings state directory — this one stays: the memory is earned ON a
        // store's data, so it belongs beside it). The Studio resolves the
        // path because the library deliberately does not (see `ChatPane::send`).
        let ledger = vike_ai::LedgerPaths::under(&self.state_dir);
        self.chat_rx =
            Some(self.chat.send(store, slice.venue, symbol, slice.interval, Some(ledger)));
    }

    /// ⟳ Refresh: re-read the store's catalog ON A WORKER THREAD and fold the answer in on a later
    /// frame (`poll()`), instead of walking it inline between two frames.
    ///
    /// The click used to run four full catalog walks in the frame — three `list_series` calls from
    /// `SlicePicker::refresh` plus `DataBrowserPane::refresh`'s `inventory()` — which on the
    /// `RemoteHistStore` this shell holds whenever a datahub address resolves is four fresh TCP
    /// connects on the egui paint thread. `crate::catalog`'s module doc carries the measurement and
    /// the `vike-desktop` precedent this copies (`refresh_stored` + `stored_loading`).
    ///
    /// A second click while a walk is in flight is a NO-OP: `catalog_rx` is the latch (see its
    /// field doc), so the button cannot pile up threads on a store that is answering slowly —
    /// which is exactly the store an impatient operator clicks Refresh on twice.
    pub fn spawn_catalog_refresh(&mut self, ctx: &egui::Context) {
        if self.catalog_rx.is_some() {
            return; // a walk is already owed an answer — see `catalog_rx`'s doc
        }
        self.catalog_rx = Some(crate::catalog::spawn_catalog_load(self.store.clone(), ctx));
    }

    /// True while the ⟳ Refresh walk is in flight — what the toolbar disables the button on.
    ///
    /// Separate from [`Self::any_running`] on purpose: that one drives the CANCEL button, and
    /// Cancel does not offer to abandon a catalog walk (see `catalog_rx`'s field doc).
    pub fn catalog_scanning(&self) -> bool {
        self.catalog_rx.is_some()
    }

    /// True while any worker-thread task (Run, Sweep, Walk-Forward, or the Saved pane's Compare
    /// all) is in flight — what the toolbar's Cancel button shows/hides on.
    pub fn any_running(&self) -> bool {
        self.running
            || self.sweep_rx.is_some()
            || self.wf_rx.is_some()
            || self.compare_rx.is_some()
            || self.study_rx.is_some()
            // A plugin BUILD is in flight — the one worker here whose legitimate wait is minutes,
            // so it is the one the operator is most likely to think has hung. It belongs in the
            // same spinner + Cancel affordance as every other worker rather than only in the
            // Plugin pane, which a user watching the toolbar is not necessarily looking at.
            || self.build_rx.is_some()
    }

    /// Abandon every in-flight worker-thread task: drop the receiver(s) and reset the
    /// running/rx state so the UI is immediately usable again (Run/Sweep/Compare all can be
    /// started fresh the very next frame).
    ///
    /// This is ABANDON, not kill: there is no cooperative-cancellation hook into
    /// `StrategyEngine::run` / `run_slice` today, so the spawned OS thread(s) keep executing the
    /// backtest(s)/compare to completion in the background — dropping the receiver just makes
    /// `Sender::send` on the worker side return `Err` (silently ignored, exactly like every other
    /// disconnect path in this file), so the result is discarded rather than delivered. The
    /// thread's CPU time is not reclaimed; a true kill would need a cancellation token threaded
    /// through `StrategyEngine`/`RhaiStrategy`, which is out of scope for this MVP.
    pub fn cancel(&mut self) {
        self.run_rx = None;
        self.running = false;
        self.sweep_rx = None;
        self.wf_rx = None;
        self.compare_rx = None;
        self.study_rx = None;
        // ⚠ Abandoning a BUILD abandons only the ANSWER, not the work: the builder keeps
        // compiling and still writes its artifact. That is not a leak — the artifact is
        // content-addressed, so the next Build of the same buffer is a cache hit that answers
        // instantly, and the service prunes its own directory. What is dropped is this session's
        // route to the sha, which is what `cancel` means everywhere else in this file too.
        self.build_rx = None;
    }

    /// Fold in the worker's outcome if it has arrived. Call once per frame (and it's what tests drive).
    ///
    /// A disconnected channel (the worker thread panicked, or its sender dropped without sending)
    /// is treated as a terminal `run failed` outcome rather than left silently pending forever —
    /// spec §7: a worker panic is isolated to its thread and surfaces as a UI-visible failure, not
    /// a stuck spinner with Run disabled for the rest of the process.
    pub fn poll(&mut self) {
        // Recompile only when the editor source actually drifted since the last compile — a
        // compile is ~ms, but per-keystroke-per-frame would still be wasteful.
        //
        // ⚠ ...and only while the buffer IS Rhai. In Plugin mode the verdict is DROPPED, not kept
        // and hidden: the check never runs on Rust source (neither per frame nor per keystroke),
        // and a switch back to Rhai finds no verdict at all and so re-checks the buffer it now
        // holds — nothing judged before the switch can be shown after it. See `rhai_check`.
        if self.buffer_is_rhai() {
            let fresh =
                self.rhai_check.as_ref().is_some_and(|(checked, _)| *checked == self.editor.source);
            if !fresh {
                let verdict = crate::editor::compile_status(&self.editor.source);
                self.rhai_check = Some((self.editor.source.clone(), verdict));
            }
        } else {
            self.rhai_check = None;
        }
        match poll_worker(&mut self.run_rx) {
            Delivery::Ready(outcome) => {
                self.last = Some(outcome);
                self.running = false;
            }
            Delivery::Failed => {
                self.last = Some(Err(RunError::Data("run failed (worker terminated)".into())));
                self.running = false;
            }
            Delivery::Pending => {}
        }
        // THE JOIN's sequencing point. A build answers with a sha and NOTHING ELSE happens: no Run
        // is dispatched from here. The operator presses Run next, and `run_blocked_reason` — which
        // this delivery is what unblocks — has already been consulted by every dispatcher. That is
        // the design's "await `Ok(sha)` from the builder, THEN send Run", expressed as a state
        // change rather than as a chained call, which is also what keeps a failed build from
        // launching anything.
        match poll_tagged_worker(&mut self.build_rx) {
            Delivery::Ready((sent, Ok(sha))) => {
                // Record the source this sha was built FROM in the same step that accepts the sha:
                // the two are one fact, and setting them apart is how they drift.
                //
                // ⚠ `sent`, the bytes the dispatch captured — NEVER `editor.source` read now. The
                // build took as long as a cargo build takes, and an edit made during it is not in
                // this artifact: recording the live buffer here marked that edit as built, and the
                // staleness guard then let Run execute the old artifact under the new code.
                self.plugin_built_source = Some(sent);
                self.plugin_sha = Some(sha.clone());
                self.build_last = Some(Ok(sha));
            }
            Delivery::Ready((_, Err(e))) => {
                // ⚠ A FAILED build must not leave a previous sha standing as if it were current.
                // The buffer that failed to compile is what the author is looking at, so an
                // unchanged "built abc123" chip would invite a Run over the LAST artifact and
                // report it as this code. Clear both halves.
                self.plugin_sha = None;
                self.plugin_built_source = None;
                self.build_last = Some(Err(e));
            }
            Delivery::Failed => {
                self.plugin_sha = None;
                self.plugin_built_source = None;
                self.build_last =
                    Some(Err("build failed (worker terminated before answering)".to_string()));
            }
            Delivery::Pending => {}
        }
        match poll_worker(&mut self.sweep_rx) {
            Delivery::Ready(outcome) => self.sweep_last = Some(outcome),
            Delivery::Failed => {
                self.sweep_last =
                    Some(Err(RunError::Data("sweep failed (worker terminated)".into())));
            }
            Delivery::Pending => {}
        }
        match poll_worker(&mut self.wf_rx) {
            Delivery::Ready(outcome) => self.wf_last = Some(outcome),
            Delivery::Failed => {
                self.wf_last =
                    Some(Err(RunError::Data("walk-forward failed (worker terminated)".into())));
            }
            Delivery::Pending => {}
        }
        // Bound BEFORE the match: `poll_worker` holds `&mut self.compare_rx` across the whole
        // statement, so the `&self` read inside the arm would be a second borrow of `self`. The
        // compare table is annualized on the SAME factor the results pane uses — a row ranked in
        // one panel and read in the other must not be on two scales (`comparison_rows`' doc).
        let compare_ppy = self.display_periods_per_year();
        match poll_worker(&mut self.compare_rx) {
            Delivery::Ready(results) => {
                // Snapshot the name -> source map first: `comparison_rows` borrows it while
                // `self.saved.compare_rows` is being assigned.
                let sources: Vec<(String, StrategySource)> =
                    self.saved.strategies.iter().map(|s| (s.name.clone(), s.source)).collect();
                let rows = comparison_rows(&results, compare_ppy, |name| {
                    sources
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, s)| *s)
                        .unwrap_or(StrategySource::Rhai)
                });
                self.saved.compare_rows = Some(rows);
            }
            Delivery::Failed => {
                self.saved.compare_error = Some("compare failed (worker terminated)".to_string());
            }
            Delivery::Pending => {}
        }
        // The ⟳ Refresh walk (`spawn_catalog_refresh`). ONE message feeds both store-reading
        // panes, each through its own `apply` — the same fold the synchronous `refresh` uses, so
        // an off-thread answer and a blocking one cannot mean different things. A disconnect is a
        // terminal failure in BOTH panes rather than a silent keep-the-old-lists, which would
        // claim the refresh had found the store unchanged.
        match poll_worker(&mut self.catalog_rx) {
            Delivery::Ready(load) => {
                self.picker.apply(load.series);
                self.data_browser.apply(load.inventory);
            }
            Delivery::Failed => {
                let load = crate::catalog::CatalogLoad::worker_terminated();
                self.picker.apply(load.series);
                self.data_browser.apply(load.inventory);
            }
            Delivery::Pending => {}
        }
        match poll_worker(&mut self.study_rx) {
            Delivery::Ready(outcome) => {
                // The error side is flattened to the runner's own words here, once — see
                // `study_last`'s field doc for why the shell holds a sentence rather than the type.
                self.study_last = Some(outcome.map_err(|e| format!("{e}")));
                // A run that just landed belongs in the list it was minted into: re-scan the RUNS
                // only, because no study folder can have changed by running one.
                self.research.refresh_runs();
            }
            Delivery::Failed => {
                self.study_last = Some(Err("study failed (worker terminated)".to_string()));
            }
            Delivery::Pending => {}
        }
        match poll_worker(&mut self.chat_rx) {
            Delivery::Ready(outcome) => {
                self.chat.running = false;
                self.chat.history.push(("assistant".to_string(), summary_of(&outcome)));
                self.chat.last = outcome.ok();
            }
            Delivery::Failed => {
                // Mirrors the run/sweep/wf arms above: a disconnect (worker panic, or its sender
                // dropped without sending) is a terminal failure, not silence -- surface it in the
                // transcript so `ChatOutcome`'s doc ("a worker panic ... surfaced by poll()'s
                // disconnect arm") is actually true.
                self.chat.running = false;
                self.chat.history.push((
                    "assistant".to_string(),
                    "AI worker terminated unexpectedly".to_string(),
                ));
            }
            Delivery::Pending => {}
        }
    }

    fn backend_kind(&self) -> BackendKind {
        match self.backend {
            Backend::Remote { .. } => BackendKind::Remote,
            Backend::Named { .. } => BackendKind::Named,
        }
    }

    /// Switch backends, KEEPING the address the operator typed — both dial the same compute
    /// daemon, so there is nothing to reset it for — and dropping a roster when the switch lands
    /// on Named, because a roster belongs to the daemon that answered it.
    fn switch_backend(&mut self, to: BackendKind) {
        if self.backend_kind() == to {
            return;
        }
        let addr = self.backend.addr().to_string();
        self.backend = match to {
            BackendKind::Remote => Backend::Remote { addr },
            BackendKind::Named => Backend::Named { addr },
        };
        if to == BackendKind::Named {
            self.named_roster = None;
            self.named_roster_rx = None;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();
        // QA autorun (see the field doc): consult on the FIRST frame only — disarm
        // unconditionally so a launch against an empty store can never leave the flag armed to
        // fire a surprise backtest minutes later, the moment a slice first becomes selectable
        // (review finding: `picker.refresh` auto-selects row 0 once data appears).
        self.take_qa_autorun();
        // Keyboard shortcuts (checked once per frame): Ctrl/Cmd+Enter runs the backtest,
        // Ctrl/Cmd+S saves the current editor buffer, Ctrl/Cmd+/ cycles the right-hand tool tab.
        // `modifiers.command` is Cmd on macOS / Ctrl elsewhere — the cross-platform egui idiom.
        let cmd = ui.input(|i| i.modifiers.command);
        let tk = Tokens::of(ui.ctx());
        if cmd {
            let (run_pressed, save_pressed, cycle_pressed) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::S),
                    i.key_pressed(egui::Key::Slash),
                )
            });
            if run_pressed
                && !self.running
                && self.picker.selected().is_some()
                && self.run_blocked_reason().is_none()
            {
                self.start_run();
            }
            if save_pressed {
                // Ctrl+S with an empty name box re-saves under the LAST saved name (normal
                // editor Save semantics), falling back to "untitled" only when this session has
                // never saved. The old always-"untitled" fallback silently misfiled follow-up
                // saves: save "momo-1" (name box clears), keep editing, Ctrl+S again → the edits
                // landed under "untitled" while "momo-1" kept the stale code.
                if self.saved.save_name.trim().is_empty() {
                    self.saved.save_name =
                        self.last_saved_name.clone().unwrap_or_else(|| "untitled".to_string());
                }
                self.handle_saved_action(SavedAction::SaveCurrent);
            }
            if cycle_pressed {
                self.right_tab = next_tab(self.right_tab);
                // Cycling while collapsed used to mutate an invisible tab with zero on-screen
                // feedback (the rail highlight requires an expanded panel) — expand like a rail
                // click does, so the shortcut always shows its effect.
                self.tools_collapsed = false;
            }
        }
        egui::Panel::top("studio-toolbar").show(ui, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                // Brand block: the Studio glyph + name, then the data-slice
                // picker and the ONE primary action (Run). Everything transient (spinner, Cancel)
                // is right-aligned so the left half of the bar never jumps around mid-run.
                let title_px = tk.text.px(TextRole::Title);
                ui.label(icons::STUDIO.rich().size(title_px).color(tk.theme.text2));
                ui.label(egui::RichText::new("Studio").strong().size(title_px));
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                self.picker.ui(ui);
                ui.add_space(4.0);
                // WHICH strategy Run will execute. Without this the toolbar looks identical in
                // both modes while running completely different code — clicking it jumps to the
                // Strategy tab. The STRATEGY icon, then the source's name: a kit secondary
                // button, whose words (and the icon among them) are the Strong role.
                let (chip_text, chip_tip) = match self.strategy_source {
                    StrategySource::Rhai => {
                        ("rhai".to_string(), "Running the editor buffer — click to change")
                    }
                    StrategySource::Native => (
                        self.native_name().to_string(),
                        "Running a native registry strategy — click to change",
                    ),
                    StrategySource::Plugin => (
                        if self.plugin_name.is_empty() {
                            "plugin".to_string()
                        } else {
                            self.plugin_name.clone()
                        },
                        "Running a runtime-loaded Rust plugin — click to change",
                    ),
                };
                if ui
                    .add(ActionButton::secondary((icons::STRATEGY, chip_text)))
                    .on_hover_text(chip_tip)
                    .clicked()
                {
                    self.right_tab = RightTab::Strategy;
                    self.tools_collapsed = false;
                }
                ui.add_space(4.0);
                let mut run = ActionButton::primary((icons::RUN, "Run"));
                if let Some(why) = self.run_disabled_reason() {
                    run = run.disabled_because(why);
                }
                if ui.add(run).on_hover_text("Run backtest  (Ctrl+Enter)").clicked() {
                    self.start_run();
                }
                // Refresh spawns the catalog walk on a worker thread (`spawn_catalog_refresh`)
                // rather than running it in this frame — see `crate::catalog`'s module doc. While
                // it is in flight the button is DISABLED and says so: the walk it is waiting on
                // may be a remote connect, and a button that looks clickable but silently no-ops
                // reads as a broken button rather than as a busy one.
                let scanning = self.catalog_scanning();
                let refresh = ui
                    .add_enabled(
                        !scanning,
                        egui::Button::new((
                            icons::REFRESH,
                            if scanning { "scanning…" } else { "Refresh" },
                        )),
                    )
                    .on_hover_text("Re-scan the data store")
                    .on_disabled_hover_text(
                        "Re-scanning the data store… (a remote store answers over the network)",
                    );
                if refresh.clicked() {
                    self.spawn_catalog_refresh(ui.ctx());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.any_running() {
                        if ui
                            .button((icons::CANCEL, "Cancel"))
                            .on_hover_text("Abandon in-flight runs")
                            .clicked()
                        {
                            self.cancel();
                        }
                        ui.spinner();
                        ui.label(
                            egui::RichText::new(if self.running {
                                "running backtest…"
                            } else if self.sweep_rx.is_some() {
                                "running sweep…"
                            } else if self.wf_rx.is_some() {
                                "running walk-forward…"
                            } else if self.study_rx.is_some() {
                                "running study…"
                            } else if self.build_rx.is_some() {
                                "building plugin…"
                            } else {
                                "comparing saved…"
                            })
                            .weak(),
                        );
                    }
                });
            });
            // The backend selector (second toolbar line): WHERE a Run/Sweep/Walk-Forward
            // executes. TWO choices, both of them the COMPUTE daemon - `Remote` (a script this
            // shell sends) or `Named` (a strategy that daemon already holds). Kept off the busy
            // primary row above.
            //
            // ⚠ There was a THIRD button here, "Local", and deleting `Backend::Local` without it
            // left a live defect for one commit: the button tested `!is_remote`, so once `Local`
            // was gone it lit up whenever the backend was NAMED - labelling the named-run
            // backend "Local" - and clicking it silently switched the user to Remote. A
            // boolean that USED to mean "not remote, therefore local" does not survive the
            // removal of the third state it was quietly assuming.
            // `docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Backend").weak());
                // ⚠ **THE NAMED BACKEND — the one that works with the OBSERVE key alone**
                // (`docs/decisions/0064-a-named-run-carries-no-source.md`). Remote above needs the
                // CONTROL key: every `Run*` verb is `Scope::Write` because it compiles
                // client-supplied Rhai or runs a built artifact, while `ListStrategies` plus the
                // named run are the Observe half. ⚠ Until 2026-09-26 this said this process could
                // never hold that key; the owner then ruled that it may, for Studio's COMPUTE dial
                // only (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1), as
                // `Self::compute_key` — so Remote works against a keyed daemon exactly when the
                // launcher handed that key over, and Named remains the backend that needs none.
                // (The key pair is ONE pair for both daemons, one domain separator; the daemon both
                // backends dial is the COMPUTE one.)
                let mut kind = self.backend_kind();
                let changed = segmented::segmented(
                    ui,
                    &mut kind,
                    &[
                        Segment { value: BackendKind::Remote, label: "Remote", why: REMOTE_WHY },
                        Segment { value: BackendKind::Named, label: "Named", why: NAMED_WHY },
                    ],
                );
                if changed {
                    self.switch_backend(kind);
                }
                if let Backend::Remote { addr } = &mut self.backend {
                    ui.add_sized(
                        [160.0, tk.metrics.control_h],
                        egui::TextEdit::singleline(addr)
                            .font(tk.mono(TextRole::Body))
                            .hint_text("host:port"),
                    )
                    // ⚠ NOT the datahub, and this tooltip said it was. `Backend::Remote`'s own
                    // doc records the same correction on 2026-09-20: the three verbs this
                    // backend sends are `Plane::Compute`, which the data daemon refuses BY
                    // PLANE - it only ever DEFAULTED to the datahub's port. The doc was fixed
                    // and the tooltip an operator actually reads was not.
                    .on_hover_text(
                        "COMPUTE daemon address (host:port) - `vike-backend backtest --addr`, \n                         a different process and port from the datahub",
                    );
                }
                if let Backend::Named { addr } = &mut self.backend {
                    let edited = ui
                        .add_sized(
                            [160.0, tk.metrics.control_h],
                            egui::TextEdit::singleline(addr)
                                .font(tk.mono(TextRole::Body))
                                .hint_text("host:port"),
                        )
                        .on_hover_text(
                            "COMPUTE daemon address (`vike-backend backtest --addr`) — a DIFFERENT \
                             credential and ask from the Remote backend, which dials the SAME \n                             daemon and port",
                        )
                        .changed();
                    if edited {
                        self.named_roster = None;
                        self.named_roster_rx = None;
                    }
                }
            });
            // The named backend's own row: the roster this daemon would run, and — when its
            // operator has not armed the lane — the SWITCH rather than an empty list.
            if let Backend::Named { addr } = &self.backend {
                let addr = addr.clone();
                // Fold in a finished fetch before rendering, so the answer appears on the frame it
                // lands rather than the one after.
                if let Some(rx) = &self.named_roster_rx
                    && let Ok(answer) = rx.try_recv()
                {
                    self.named_roster = Some(answer);
                    self.named_roster_rx = None;
                }
                let fetching = self.named_roster_rx.is_some();
                ui.horizontal_wrapped(|ui| {
                    // ⚠ The dial goes to a WORKER, never this thread — see `named_roster_rx`. The
                    // button is disabled while one is in flight so a click cannot start a second.
                    if ui
                        .add_enabled(!fetching, egui::Button::new("Roster").small())
                        .on_hover_text("Ask this daemon which strategies it can run")
                        .on_disabled_hover_text("Asking this daemon…")
                        .clicked()
                    {
                        self.named_roster_rx = Some(crate::remote::spawn_named_roster_remote(
                            addr.clone(),
                            self.named_run_keys.clone(),
                        ));
                    }
                    if fetching {
                        ui.label(egui::RichText::new("asking…").weak());
                        return;
                    }
                    match &self.named_roster {
                        None => {
                            ui.label(egui::RichText::new("roster not fetched").weak());
                        }
                        Some(Err(e)) => {
                            ui.label(egui::RichText::new(e.clone()).color(Status::Error.color()));
                        }
                        // ⚠ An UNARMED daemon WITHHOLDS the roster (it answers `armed: false`
                        // with an EMPTY list — arming gates the NAMES as well as the run, 0064's
                        // decision 8 leg 3). Render the SWITCH, never that empty list — the
                        // teaching-refusal rule, and the reason this outcome is a success on the
                        // wire rather than an error.
                        Some(Ok(roster)) if !roster.armed => {
                            ui.label(
                                egui::RichText::new(
                                    vike_datahub_client::named_run::NamedRoster::unarmed_note(),
                                )
                                .color(Status::Warning.color()),
                            );
                        }
                        Some(Ok(roster)) => {
                            for name in &roster.strategies {
                                if ui
                                    .selectable_label(
                                        self.named_strategy.as_deref() == Some(name.as_str()),
                                        // A selectable's words default to the Button style (the
                                        // Strong role), so the Body role is named, not implied.
                                        egui::RichText::new(name).size(tk.text.px(TextRole::Body)),
                                    )
                                    .clicked()
                                {
                                    self.named_strategy = Some(name.clone());
                                }
                            }
                        }
                    }
                });
            }
            ui.add_space(3.0);
        });
        if self.editor_collapsed {
            // NOTE: a DIFFERENT panel id than the expanded editor — egui persists panel width
            // by id, so sharing one id would store this 28px width and reopen the expanded
            // editor as a min-width sliver instead of at the user's dragged width.
            egui::Panel::left("studio-editor-min").exact_size(28.0).resizable(false).show(
                ui,
                |ui| {
                    ui.add_space(4.0);
                    ui.vertical_centered(|ui| {
                        if icons::named(ui.small_button(icons::EXPAND_PANEL), "Expand editor")
                            .clicked()
                        {
                            self.editor_collapsed = false;
                        }
                    });
                },
            );
        } else {
            egui::Panel::left("studio-editor").resizable(true).default_size(460.0).show(ui, |ui| {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if icons::named(ui.small_button(icons::COLLAPSE_PANEL), "Collapse editor")
                        .clicked()
                    {
                        self.editor_collapsed = true;
                    }
                    ui.label(
                        egui::RichText::new("Editor").strong().size(tk.text.px(TextRole::Title)),
                    );
                    ui.add_space(4.0);
                    // Compile-status chip: green "compiles" when the last-compiled source was
                    // clean, red "line N" (full message on hover) otherwise — same facts as the
                    // old glance-dots, now readable without hovering.
                    match self.rhai_verdict() {
                        Some(Ok(())) => {
                            chip::badge(ui, "● compiles", Status::Ok).on_hover_text("Compiles OK");
                        }
                        Some(Err(msg)) => {
                            let label = match crate::editor::error_line(msg) {
                                Some(line) => format!("● error · line {line}"),
                                None => "● error".to_string(),
                            };
                            chip::badge(ui, &label, Status::Error).on_hover_text(msg.as_str());
                        }
                        // No Rhai verdict is held: the buffer is a Plugin's RUST source
                        // (`buffer_is_rhai`). Say what IS known — its language, and where its
                        // verdict comes from — in the muted status, because nothing here has
                        // judged it. The Build result in the Strategy pane is the authority.
                        None => {
                            chip::badge(ui, PLUGIN_EDITOR_CHIP, Status::Muted).on_hover_text(
                                "Plugin mode: this buffer is Rust, which the Rhai check cannot \
                                 judge. Build (Strategy tab) compiles it — its result there is \
                                 the verdict.",
                            );
                        }
                    }
                    // Unsaved-changes chip: amber while the current mode's own baseline has
                    // drifted. Native compares its OWN (name, params) baseline
                    // (`native_is_dirty`) rather than the parked editor buffer every other mode
                    // uses — see `native_saved`'s doc for why the two cannot share one check. The
                    // hover says what Ctrl+S would really do about it (`unsaved_chip_tip`).
                    let dirty = match self.strategy_source {
                        StrategySource::Native => self.native_is_dirty(),
                        StrategySource::Rhai | StrategySource::Plugin => {
                            self.editor.source != self.saved_source
                        }
                    };
                    if dirty {
                        chip::badge(ui, "● unsaved", Status::Warning)
                            .on_hover_text(self.unsaved_chip_tip());
                    }
                });
                // Inline compile-error banner ABOVE the editor (under the header), not below it:
                // the chip is a glance affordance, but a failing compile deserves a message
                // visible without hovering — and above the editor it can never be pushed off the
                // panel bottom if the editor's row estimate runs long.
                if let Some(Err(msg)) = self.rhai_verdict() {
                    let words = egui::RichText::new(crate::editor::format_compile_error(msg))
                        .color(Status::Error.color());
                    ui.label(icons::FAILED.before(ui.style(), words));
                }
                ui.add_space(2.0);
                // The editor fills whatever height is left.
                self.editor.ui_sized(ui, ui.available_height().max(80.0));
            });
        }
        // The tool rail + the tools panel are two SIBLING right panels (VS Code activity-bar
        // shape), replacing the old single panel that hand-partitioned its width. Two reasons:
        // the rail can no longer be pushed off-screen by over-wide pane content (the pre-redesign
        // clipping bug — `ui.set_width` is only a minimum, so a 380px row in a 300px column shoved
        // the rail past the panel edge), and the rail stays visible while the tools panel is
        // collapsed, so the tools are always one click away. Panel::right order matters: the rail
        // is added FIRST so it hugs the window edge; the tools panel lands to its left.
        const RAIL_WIDTH: f32 = 40.0;
        egui::Panel::right("studio-rail").exact_size(RAIL_WIDTH).resizable(false).show(ui, |ui| {
            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                for tab in RightTab::ALL {
                    let active = self.right_tab == tab && !self.tools_collapsed;
                    let ink = if active { tk.theme.accent } else { tk.theme.text3 };
                    let icon = tab.icon().rich().size(tk.text.px(TextRole::Title)).color(ink);
                    let button = egui::Button::new(icon)
                        .min_size(egui::vec2(
                            RAIL_WIDTH - 8.0,
                            tk.metrics.control_h + tk.metrics.gap,
                        ))
                        .fill(if active { tk.theme.surface } else { egui::Color32::TRANSPARENT })
                        .frame(active);
                    let resp = icons::named(ui.add(button), tab.label());
                    if active {
                        let mut bar = resp.rect;
                        bar.set_right(bar.left() + 2.0);
                        ui.painter().rect_filled(bar, 0.0, tk.theme.accent);
                    }
                    if resp.clicked() {
                        // VS Code behavior: clicking the active tool's icon toggles the panel;
                        // clicking any other icon selects it (expanding if collapsed).
                        if active {
                            self.tools_collapsed = true;
                        } else {
                            self.right_tab = tab;
                            self.tools_collapsed = false;
                        }
                    }
                    ui.add_space(2.0);
                }
            });
        });
        if !self.tools_collapsed {
            egui::Panel::right("studio-tools").exact_size(340.0).resizable(false).show(ui, |ui| {
                if self.right_tab == RightTab::Data {
                    // The Data pane manages its own scrolling (the shared catalog grid brings an
                    // internal vertical ScrollArea, and the pane adds a horizontal wrap for the
                    // grid's ~690px natural width) — nesting that inside the shared vertical
                    // ScrollArea below would hand the internal scroll unbounded height.
                    ui.add_space(4.0);
                    if let Some((venue, symbol, interval)) = self.data_browser.ui(ui) {
                        self.picker.select_by_key(&venue, &symbol, &interval);
                    }
                } else {
                    // One vertical scroll for the whole pane body: content taller than the panel
                    // scrolls instead of clipping, and nothing here can affect the rail's
                    // geometry. Salted per tab so each tool keeps its OWN scroll offset —
                    // unsalted, switching tabs would open the new pane at the old pane's offset.
                    egui::ScrollArea::vertical()
                        .id_salt(("studio-tools-scroll", self.right_tab))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add_space(4.0);
                            match self.right_tab {
                                RightTab::Sweep => self.sweep_pane_ui(ui),
                                RightTab::Strategy => self.strategy_pane_ui(ui),
                                RightTab::Data => unreachable!("handled above"),
                                RightTab::Indicators => {
                                    // The pane returns true the frame "Compute" is clicked; loading
                                    // bars + compute happens here (the pane never touches the store)
                                    // so its logic stays testable.
                                    if self.indicators.ui(ui) {
                                        self.compute_indicator_preview();
                                    }
                                }
                                RightTab::Saved => {
                                    let blocked = self.rhai_writer_blocked_reason();
                                    if let Some(action) = self.saved.ui(ui, blocked) {
                                        self.handle_saved_action(action);
                                    }
                                    if self.compare_rx.is_some() {
                                        ui.horizontal(|ui| {
                                            ui.spinner();
                                            ui.label(egui::RichText::new("comparing…").weak());
                                        });
                                    }
                                }
                                RightTab::Research => {
                                    // Scanned LAZILY, on the first frame this tool is opened: a
                                    // session that never opens it reads no directory at all, and
                                    // a session that does gets a list without having to press
                                    // Rescan first.
                                    self.research.ensure_scanned();
                                    let window = self.study_window_label();
                                    let running = self.study_rx.is_some();
                                    // Bound BEFORE the match: `ui` borrows the pane mutably, and
                                    // both arms below then touch `self` again.
                                    let action = self.research.ui(ui, &window, running);
                                    match action {
                                        Some(ResearchAction::Refresh) => self.research.refresh(),
                                        Some(ResearchAction::RunStudy) => self.start_study(),
                                        None => {}
                                    }
                                }
                                RightTab::Chat => self.chat_pane_ui(ui),
                            }
                            ui.add_space(6.0);
                        });
                }
            });
        }
        self.maybe_persist_workspace();
        egui::CentralPanel::default().show(ui, |ui| match self.center {
            // TWO surfaces, each naming its own producer — see [`CenterView`] for why a study's
            // numbers and a backtest's may not share one.
            CenterView::Study => self.study_center(ui),
            CenterView::Backtest => {
                // Bound FIRST, and bound to an `f64`: `display_periods_per_year` reads the whole
                // of `self`, while `results_ui` below is handed `&mut self.tab` alongside three
                // shared field borrows. Taking the factor as a value ends that read before any of
                // them start.
                let periods_per_year = self.display_periods_per_year();
                let sweep = self.sweep_last.as_ref().and_then(|r| r.as_ref().ok());
                let wf = self.wf_last.as_ref().and_then(|r| r.as_ref().ok());
                let single = self.last.as_ref().and_then(|r| r.as_ref().ok());
                // show the single-run result if present, else the sweep's best entry (so Equity/Trades
                // have data to render even when the user only ran a sweep).
                let r = single.or_else(|| sweep.map(|s| &s.entries[s.best_index].result));
                match r {
                    Some(res) => results_ui(ui, &mut self.tab, res, sweep, wf, periods_per_year),
                    None => {
                        // Which failure to surface, with an honest per-source title (the old fixed
                        // "Run failed" header misattributed sweep failures). Walk-forward errors are
                        // included too — previously a failed Walk-Forward was completely invisible
                        // (read only via `.ok()`).
                        let err = match (&self.last, &self.sweep_last, &self.wf_last) {
                            (Some(Err(e)), _, _) => Some(("Run failed", format!("{e}"))),
                            (_, Some(Err(e)), _) => Some(("Sweep failed", format!("{e}"))),
                            (_, _, Some(Err(e))) => Some(("Walk-forward failed", format!("{e}"))),
                            _ => None,
                        };
                        match err {
                            Some((title, msg)) => Self::error_state(ui, title, &msg),
                            None => self.empty_state(ui),
                        }
                    }
                }
            }
        });
    }

    /// The annualization factor every DISPLAYED metric is scaled by: the number of RETURN
    /// OBSERVATIONS a year produces at the picked slice's bar step, which is one per bar.
    ///
    /// ONE derivation for the whole window. `crate::results`' Performance tab, its Validation
    /// Sharpe row and its sweep table all take this value, and so does the Saved pane's compare
    /// table — so two panes describing one run cannot print two different Sharpes. The arithmetic
    /// is `vike_analytics::report::periods_per_year_for_interval` (vike-analytics', re-exported),
    /// the SAME function the harness plane uses, which is what makes the Studio door and the
    /// CLI/MCP door agree about the same strategy over the same series. Before it existed this
    /// plane passed a bare `252.0` on every interval and disagreed with that door by
    /// `sqrt(24)` on 1h bars and `sqrt(1440)` on 1m.
    ///
    /// TWO fallbacks, both landing on [`DEFAULT_PERIODS_PER_YEAR`] and neither of them a panic:
    ///
    /// * **No slice picked.** The app starts there, and `SlicePicker::apply` leaves it there for
    ///   an empty store. There is no interval to derive from, so the metrics keep the daily anchor
    ///   — exactly the scale every build before this one used — rather than blanking the pane over
    ///   a display knob.
    /// * **A tick slice** (`SeriesRow::interval` is `None`). A tick stream has no fixed period, so
    ///   there is no honest observation count to derive. This MIRRORS
    ///   `vike_backtest::harness::report::periods_per_year`'s tick branch, whose doc owns the
    ///   decision, rather than inventing a second answer for the same question.
    ///
    /// ⚠ **It reads the PICKER, not the run, and the pane can therefore be re-scaled without
    /// re-running.** Nothing clears `last`/`sweep_last`/`wf_last` when the combo changes, so a
    /// result stays on screen across a re-pick and its annualized rows follow the NEW interval.
    /// That is a smaller version of the staleness the whole pane already has in that state (the
    /// equity curve and trade list are the previous run's too), but it is the one place the number
    /// itself becomes something that never happened. Closing it means the interval travelling WITH
    /// the outcome — a `vike_studio_core` run-plumbing change, not a change to this display — so
    /// it is named here rather than papered over.
    fn display_periods_per_year(&self) -> f64 {
        match self.picker.selected_row().and_then(|row| row.interval.as_deref()) {
            Some(interval) => periods_per_year_for_interval(interval),
            None => DEFAULT_PERIODS_PER_YEAR,
        }
    }

    /// The central panel while the STUDY surface is in front: the last study run, the last refusal,
    /// or the spinner in between.
    ///
    /// `Self::error_state` is REUSED rather than re-spelled — a study that refused and a backtest
    /// that refused are the same event to a reader, and the title parameter is what names which
    /// one it was. The result side is `crate::research::study_result_ui`, which is deliberately not
    /// `results_ui`: that argument lives on that function.
    fn study_center(&self, ui: &mut egui::Ui) {
        match &self.study_last {
            Some(Ok(run)) => crate::research::study_result_ui(ui, run),
            Some(Err(msg)) => Self::error_state(ui, "Study failed", msg),
            None if self.study_rx.is_some() => {
                ui.add_space((ui.available_height() * 0.26).max(16.0));
                state::view(ui, Load::Loading("Running study…"));
            }
            None => {
                ui.add_space((ui.available_height() * 0.26).max(16.0));
                state::view(ui, Load::Empty(NO_STUDY_RESULT));
            }
        }
    }

    /// The central panel before any result exists: a centered getting-started block instead of
    /// one weak sentence lost in a gray void. Offers the two next actions directly (Run when a
    /// slice is pickable, template load otherwise) and adapts its copy to an empty store.
    ///
    /// ⚠ It asks [`crate::SlicePicker::error`] BEFORE `available().is_empty()`, and the order is
    /// the whole point: an unreadable store leaves the picker's list empty too, so the emptiness
    /// test alone told an operator whose datahub had gone away to go and backfill history. This is
    /// the second surface that rendered that lie (the picker's own combo was the first).
    fn empty_state(&mut self, ui: &mut egui::Ui) {
        ui.add_space((ui.available_height() * 0.26).max(16.0));
        if let Some(err) = self.picker.error() {
            let why = format!("{}\n\n{err}", crate::picker::SERIES_SCAN_ADVICE);
            state::view(ui, Load::Unreachable(&why));
            return;
        }
        if !self.picker.depth_only().is_empty() && self.picker.available().is_empty() {
            // The store is NOT empty — it holds series no slice can replay. Sending this operator
            // to a backfill would have them re-fetch data they already have.
            state::view(ui, Load::Empty(crate::picker::DEPTH_NOT_REPLAYABLE));
            return;
        }
        if self.picker.available().is_empty() {
            state::view(ui, Load::Empty(EMPTY_STORE));
            return;
        }
        state::view(ui, Load::Empty("No results yet"));
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(
                    "1  Pick a data slice    2  Write or load a strategy    3  Run",
                )
                .weak(),
            );
            ui.add_space(12.0);
            // Clamped to the panel and wrap-enabled, NOT a fixed 280px block. The centre
            // panel can be narrower than 280 (the resizable editor + the rail + the 340px
            // tools panel squeeze it), and `vertical_centered` centres a fixed-width
            // allocation into whatever is left — below 280px the block starts LEFT of the
            // panel's clip rect and the first button rendered as "?un backtest" (the
            // 2026-08-21 GPU contact sheet; no CPU rung saw it — coordinates finite, button
            // present and enabled, click still firing). Width-aware sizing off
            // `available_width` is this file's own idiom (the combo widths below);
            // `with_main_wrap` makes the row wrap instead of overflow when even the clamped
            // width cannot hold both buttons (it also flips the Ui's default text wrap mode
            // from Extend to Wrap, so egui may wrap a label inside its button rather than
            // drop the button to a second row — both outcomes stay inside the clip). Panels
            // ≥280px wide render identically to the old fixed block.
            // The kill-proof twins — broken and fixed — live in
            // `crates/vike-chart/tests/frame_record_gate.rs` beside the opt-in check they
            // exercise (`vike_ui_theme::frame_sanity`'s `clipped_text_shapes`). Residual: a
            // panel narrower than ONE button still clips that button's right edge — the floor
            // of any layout that keeps buttons at natural size.
            let row_w = ui.available_width().min(280.0);
            ui.allocate_ui_with_layout(
                egui::vec2(row_w, 28.0),
                egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                |ui| {
                    let mut run = ActionButton::primary((icons::RUN, "Run backtest"));
                    if let Some(why) = self.run_disabled_reason() {
                        run = run.disabled_because(why);
                    }
                    if ui.add(run).on_hover_text("Ctrl+Enter").clicked() {
                        self.start_run();
                    }
                    // A template is RHAI: refused over a Plugin's Rust buffer, with the reason
                    // on hover (`rhai_writer_blocked_reason`).
                    let blocked = self.rhai_writer_blocked_reason();
                    if ui
                        .add_enabled(blocked.is_none(), egui::Button::new("Load a template"))
                        .on_disabled_hover_text(blocked.unwrap_or_default())
                        .clicked()
                    {
                        self.load_template(TEMPLATES[self.template_idx].1);
                    }
                },
            );
        });
    }

    /// The central panel when the last run/sweep/walk-forward failed: a visible failure header
    /// (`title` names WHICH action failed) + the message in a framed monospace block (was: one
    /// bare red line in the void).
    fn error_state(ui: &mut egui::Ui, title: &str, msg: &str) {
        let tk = Tokens::of(ui.ctx());
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(
                icons::FAILED
                    .rich()
                    .size(tk.text.px(TextRole::Heading))
                    .color(Status::Error.color()),
            );
            ui.label(egui::RichText::new(title).size(tk.text.px(TextRole::Title)).strong());
        });
        ui.add_space(8.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(msg).monospace().color(Status::Error.color()));
        });
    }

    /// The Strategy tool pane: pick WHAT the Run toolbar executes — the Rhai editor buffer, or a
    /// native `vike-backtest` registry strategy plus its params.
    ///
    /// The param editor is deliberately a free-form key/value table rather than a generated form:
    /// the registry has no param spec to generate from (each strategy reads a `&toml::Value` ad
    /// hoc — `vike_studio_core::spec`'s module doc), so any strategy's knobs are expressible here
    /// the day it lands, with no change to this pane. `params_from_rows` types each cell
    /// (`3` -> integer, `2.5` -> float, `true` -> bool, bare text -> string).
    fn strategy_pane_ui(&mut self, ui: &mut egui::Ui) {
        let tk = Tokens::of(ui.ctx());
        pane_header(ui, icons::STRATEGY, "Strategy");
        segmented::segmented(
            ui,
            &mut self.strategy_source,
            &[
                Segment {
                    value: StrategySource::Rhai,
                    label: "Rhai script",
                    why: "Run the editor buffer (the original Studio path)",
                },
                Segment {
                    value: StrategySource::Native,
                    label: "Native (Rust)",
                    why: "Run a compiled strategy from the vike-backtest registry",
                },
                Segment {
                    value: StrategySource::Plugin,
                    label: "Plugin (Rust)",
                    why: "Build the editor buffer as a Rust plugin and run the compiled artifact — \
                          Run is disabled until a Build returns a sha",
                },
            ],
        );
        ui.add_space(6.0);
        match self.strategy_source {
            StrategySource::Rhai => {
                ui.label(
                    egui::RichText::new(
                        "Run/Sweep/Walk-Forward execute the editor buffer on the left.",
                    )
                    .weak(),
                );
            }
            StrategySource::Native => {
                let roster = native_strategies();
                self.native_idx = self.native_idx.min(roster.len().saturating_sub(1));
                ui.label(egui::RichText::new("Registry strategy").weak());
                egui::ComboBox::from_id_salt("studio-native-strategy")
                    .width((ui.available_width() - 8.0).max(80.0))
                    .show_index(ui, &mut self.native_idx, roster.len(), |i| roster[i].to_string());
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Params").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("+ row")
                            .on_hover_text("Add a key = value param row")
                            .clicked()
                        {
                            self.native_params.push((String::new(), String::new()));
                        }
                    });
                });
                if self.native_params.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No params — the strategy's own defaults apply. \
                             Add a row for e.g. size = 2 or symbol = BTCUSDT.",
                        )
                        .weak(),
                    );
                }
                let mut remove: Option<usize> = None;
                for (i, (key, value)) in self.native_params.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if icons::named(ui.small_button(icons::REMOVE), "Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.add_sized(
                            [96.0, 18.0],
                            egui::TextEdit::singleline(key).hint_text("key"),
                        );
                        let w = (ui.available_width() - 8.0).max(40.0);
                        ui.add_sized(
                            [w, 18.0],
                            egui::TextEdit::singleline(value).hint_text("value"),
                        );
                    });
                }
                if let Some(i) = remove {
                    self.native_params.remove(i);
                }
                ui.add_space(6.0);
                // Echo the TYPED table the strategy will actually receive — the only feedback
                // available without a param spec, and it makes the string-vs-number fallback
                // visible instead of surprising.
                let typed = params_from_rows(&self.native_params);
                let rendered = typed
                    .as_table()
                    .map(|t| {
                        t.iter().map(|(k, v)| format!("{k} = {v}")).collect::<Vec<_>>().join("\n")
                    })
                    .unwrap_or_default();
                if !rendered.is_empty() {
                    ui.label(egui::RichText::new("Resolved params").weak());
                    ui.add(egui::Label::new(egui::RichText::new(rendered).monospace()).wrap());
                }
            }
            // Unlike Native, this mode does NOT replace the pane's content with a dropdown — a
            // plugin is CODE the user writes, exactly like Rhai, so the editor buffer on the left
            // stays the thing Run/Sweep/Walk-Forward act on. What differs from Rhai is what
            // happens to that buffer: it is built into a `.so` by the builder service — which
            // `crate::plugin_build` DOES call from this crate now, through the Build button below
            // — rather than compiled in-process, which is why a name and a build status live here
            // instead of a params table.
            StrategySource::Plugin => {
                // ⚠ **This label said the Run path was not connected, and that is no longer
                // true.** It read "Plumbing only so far: nothing here builds the editor buffer,
                // and no Run path loads a plugin yet", which was an honest description of
                // `docs/decisions/0082`'s state and is now the opposite of what happens: Build
                // dials the builder service below, and `vike_studio_core`'s `build_strategy`
                // `dlopen`s the artifact the returned sha names. Leaving the old sentence would
                // be the mirror of the promise IT replaced — a pane telling a user that the
                // button in front of them does nothing.
                ui.label(
                    egui::RichText::new(
                        "Build sends the editor buffer to the builder service, which compiles it \
                         to a .so and answers with its sha. Run then sends that sha, and the \
                         backtest server loads exactly that artifact. Build first — Run stays \
                         disabled until a sha comes back, and again after any edit.",
                    )
                    .weak(),
                );
                // ⚠ **The one thing a plugin author must know that is not visible anywhere else in
                // this pane.** There is no params editor for this tier yet (`current_spec` resolves
                // an EMPTY table), so a Sweep override is the ONLY way to set a knob at all — and
                // every override is an `f64`, i.e. a TOML FLOAT. A `build` that reads its knob with
                // `as_integer()` alone therefore ignores every swept value and reports one flat row
                // of identical results: no error, no warning, and a perfectly plausible answer.
                // The user is looking at this pane while writing that `build`, which is why the
                // sentence is here and not only in the tier README.
                ui.label(icons::WARNING.before(
                    ui.style(),
                    egui::RichText::new(
                        "No params editor yet — a Sweep override is the only way to set a knob, \
                         and every override arrives as a TOML float. Read params as \
                         `as_integer().or_else(|| as_float()…)` or a swept knob silently falls \
                         back to your default.",
                    )
                    .weak(),
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Name").weak());
                    input::text(ui, &mut self.plugin_name, Field::default());
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Builder").weak());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.builder_addr)
                            .font(tk.mono(TextRole::Body))
                            .min_size(egui::vec2(0.0, tk.metrics.control_h))
                            .desired_width(140.0),
                    )
                    .on_hover_text(
                        "The BUILDER service (vike-strategy-builder), not the compute daemon \
                         — a third service with its own key. Loopback only; reach a remote \
                         box through a tunnel.",
                    );
                });
                ui.add_space(4.0);
                let building = self.build_rx.is_some();
                let build_why = if building {
                    Some("A build is running.")
                } else if self.plugin_name.trim().is_empty() {
                    Some("Name the plugin first.")
                } else {
                    None
                };
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(build_why.is_none(), egui::Button::new("Build"))
                        .on_hover_text(
                            "Compile the editor buffer on the builder box (a cargo \
                                        build — seconds when cached, longer when not)",
                        )
                        .on_disabled_hover_text(build_why.unwrap_or_default())
                        .clicked()
                    {
                        self.start_plugin_build();
                    }
                    if building {
                        ui.label(egui::RichText::new("building…").weak());
                    }
                });
                ui.add_space(4.0);
                match (&self.plugin_sha, self.run_blocked_reason()) {
                    (Some(sha), None) => {
                        let built = format!("\u{25cf} built {}", &sha[..sha.len().min(8)]);
                        chip::badge(ui, &built, Status::Ok).on_hover_text(sha.as_str());
                    }
                    // A sha exists but Run is still blocked — today that means the buffer drifted
                    // since the Build. Render the REASON rather than the reassuring chip: a green
                    // "built abc123" over stale source is the exact misreading the staleness guard
                    // exists to prevent.
                    (Some(_), Some(reason)) | (None, Some(reason)) => {
                        ui.label(egui::RichText::new(reason).weak());
                    }
                    (None, None) => {}
                }
                if let Some(Err(e)) = &self.build_last {
                    ui.add_space(4.0);
                    // rustc's own diagnostics, verbatim and monospaced — the design's
                    // `Err(<rustc diagnostics as text>)` reaching the author unedited is the whole
                    // point of carrying them back across the wire.
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(e).monospace().color(Status::Error.color()),
                        )
                        .wrap(),
                    );
                }
            }
        }
    }

    /// The Sweep & Validate tool pane (extracted from the old inline match arm; behavior
    /// unchanged, layout restyled: header, full-width template row, grouped grid section, one
    /// primary action).
    fn sweep_pane_ui(&mut self, ui: &mut egui::Ui) {
        let tk = Tokens::of(ui.ctx());
        pane_header(ui, icons::SWEEP, "Sweep & Validate");
        ui.label(egui::RichText::new("Template").weak());
        ui.horizontal(|ui| {
            let combo_w = (ui.available_width() - 64.0).max(80.0);
            egui::ComboBox::from_id_salt("studio-template").width(combo_w).show_index(
                ui,
                &mut self.template_idx,
                TEMPLATES.len(),
                |i| TEMPLATES[i].0.to_string(),
            );
            // Templates are RHAI, so over a Plugin's Rust buffer both loaders below are refused —
            // disabled, with the reason on hover (`rhai_writer_blocked_reason` argues why refused
            // rather than switching the source as they load).
            let blocked = self.rhai_writer_blocked_reason();
            if ui
                .add_enabled(blocked.is_none(), egui::Button::new("Load"))
                .on_hover_text("Load this template into the editor")
                .on_disabled_hover_text(blocked.unwrap_or_default())
                .clicked()
            {
                self.load_template(TEMPLATES[self.template_idx].1);
            }
        });
        // Gallery: preview each starter script before loading it (the dropdown loads blind). The
        // gallery only reports WHICH template was picked; the write is `load_template`'s, so it
        // passes the same guard as every other Rhai writer.
        let blocked = self.rhai_writer_blocked_reason();
        let mut picked: Option<&'static str> = None;
        egui::CollapsingHeader::new("Browse templates").default_open(false).show(ui, |ui| {
            picked = crate::templates_gallery::gallery_ui(ui, blocked);
        });
        if let Some(code) = picked {
            self.load_template(code);
        }
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Parameter grid").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Seed from params")
                    .on_hover_text("Fill the grid from the script's param() declarations")
                    .clicked()
                {
                    self.seed_sweep_grid();
                }
            });
        });
        if self.grid.is_empty() {
            ui.label(
                egui::RichText::new(match self.strategy_source {
                    StrategySource::Rhai => {
                        "No grid yet — declare param(\"name\", default) in the script, then Seed."
                    }
                    StrategySource::Native => {
                        "No grid yet — add numeric param rows in the Strategy tab, then Seed."
                    }
                    StrategySource::Plugin => {
                        "No grid yet — Plugin mode has no param editor to seed from yet."
                    }
                })
                .weak(),
            );
        }
        for (name, csv) in self.grid.iter_mut() {
            ui.horizontal(|ui| {
                // Fixed, truncating name column: a long script param name must squeeze itself,
                // not push the value field past the panel edge.
                ui.add_sized(
                    [96.0, tk.metrics.control_h],
                    egui::Label::new(name.as_str()).truncate(),
                )
                .on_hover_text(name.as_str());
                let w = (ui.available_width() - 8.0).max(40.0);
                ui.scope(|ui| {
                    ui.spacing_mut().text_edit_width = w;
                    input::text(ui, csv, Field { hint: "1, 2, 3", ..Field::default() });
                });
            });
        }
        ui.add_space(6.0);
        // Enabled only when the grid actually parses to at least one candidate value — a styled
        // primary button that silently no-ops on click (start_sweep's empty-grid early return)
        // reads as broken. Mirrors start_sweep's own CSV parse.
        let grid_ok = self
            .grid
            .iter()
            .any(|(_, csv)| csv.split(',').any(|s| s.trim().parse::<f64>().is_ok()));
        ui.horizontal(|ui| {
            let mut sweep = ActionButton::primary((icons::RUN, "Run Sweep"));
            if let Some(why) = self.sweep_disabled_reason(grid_ok) {
                sweep = sweep.disabled_because(why);
            }
            if ui
                .add(sweep)
                .on_hover_text("Backtest every grid combination and rank by Sharpe")
                .clicked()
            {
                self.start_sweep();
            }
            let wf_why = self.walk_forward_disabled_reason();
            if ui
                .add_enabled(wf_why.is_none(), egui::Button::new("Walk-Forward"))
                .on_hover_text("4-split out-of-sample validation")
                .on_disabled_hover_text(wf_why.unwrap_or_default())
                .clicked()
            {
                self.start_walkforward();
            }
            if self.sweep_rx.is_some() || self.wf_rx.is_some() {
                ui.spinner();
            }
        });
        // Surface the last sweep/walk-forward failure here too: when the central panel is busy
        // showing an older successful result, a failed validation would otherwise end as a
        // spinner that just stops (review finding: invisible Walk-Forward errors).
        if let Some(Err(e)) = &self.sweep_last {
            ui.add_space(4.0);
            ui.colored_label(Status::Error.color(), format!("sweep failed: {e}"));
        }
        if let Some(Err(e)) = &self.wf_last {
            ui.add_space(4.0);
            ui.colored_label(Status::Error.color(), format!("walk-forward failed: {e}"));
        }
    }

    /// The AI Copilot tool pane (extracted from the old inline match arm; behavior unchanged,
    /// layout restyled: header, full-width provider/input, wrapped transcript, one primary Send).
    fn chat_pane_ui(&mut self, ui: &mut egui::Ui) {
        let tk = Tokens::of(ui.ctx());
        pane_header(ui, icons::CHAT, "AI Copilot");
        if self.chat.available_providers().is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(NO_PROVIDER_KEY).weak()).wrap());
        } else {
            egui::ComboBox::from_id_salt("studio-chat-provider")
                .width(ui.available_width() - 8.0)
                .selected_text(format!("{:?}", self.chat.provider))
                .show_ui(ui, |ui| {
                    for p in self.chat.available_providers().to_vec() {
                        ui.selectable_value(&mut self.chat.provider, p, format!("{p:?}"));
                    }
                });
        }
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("studio-chat-history")
            .max_height(220.0)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (role, text) in &self.chat.history {
                    let color = if role == "assistant" {
                        tk.theme.text2
                    } else {
                        ui.visuals().strong_text_color()
                    };
                    ui.label(egui::RichText::new(role).color(color).strong());
                    ui.add(egui::Label::new(egui::RichText::new(text)).wrap());
                    ui.add_space(4.0);
                }
                if self.chat.history.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "Describe a strategy — the copilot writes it, backtests it \
                             out-of-sample, and shows you the diff before it touches the editor.",
                        )
                        .weak(),
                    );
                }
            });
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.chat.input)
                .desired_width(f32::INFINITY)
                .desired_rows(3)
                .hint_text("e.g. RSI mean-reversion with a 2% stop"),
        );
        ui.horizontal(|ui| {
            let mut send = ActionButton::primary("Send");
            if let Some(why) = self.send_disabled_reason() {
                send = send.disabled_because(why);
            }
            if ui.add(send).clicked() {
                self.start_chat_send();
            }
            if self.chat.running {
                ui.spinner();
                ui.label(egui::RichText::new("developing…").weak());
            }
        });
        if let Some(last) = self.chat.last.clone() {
            ui.add_space(6.0);
            ui.separator();
            // Same sign convention as every table in the Studio: positive in the market set's up
            // colour, negative in its down colour, zero/NaN neutral (a Sharpe is money, not a
            // status — design system spec §3.2).
            let oos_color = if last.oos_sharpe > 0.0 {
                tk.market.up_text
            } else if last.oos_sharpe < 0.0 {
                tk.market.down_text
            } else {
                ui.visuals().text_color()
            };
            // A MEASUREMENT, not a status, so not a badge: a label row with the Sharpe in the
            // market colour, in JetBrains Mono like every number beside a word.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("OOS Sharpe").color(tk.theme.text2));
                ui.label(
                    egui::RichText::new(format!("{:.2}", last.oos_sharpe))
                        .monospace()
                        .color(oos_color),
                );
                ui.label(
                    egui::RichText::new(format!("· {} trades", last.n_trades))
                        .color(tk.theme.text2),
                );
            });
            // Review changes: a line-level diff of the current buffer -> the generated script,
            // so Apply is a reviewed action rather than a blind clobber.
            egui::CollapsingHeader::new("Review changes").default_open(true).show(ui, |ui| {
                let rows = crate::chat::diff_rows(&self.editor.source, &last.code);
                egui::ScrollArea::both().id_salt("studio-chat-diff").max_height(180.0).show(
                    ui,
                    |ui| {
                        for row in &rows {
                            let (prefix, color) = match row.kind {
                                crate::chat::DiffKind::Insert => ("+", Status::Ok.color()),
                                crate::chat::DiffKind::Delete => ("-", Status::Error.color()),
                                crate::chat::DiffKind::Equal => {
                                    (" ", ui.visuals().weak_text_color())
                                }
                            };
                            ui.label(
                                egui::RichText::new(format!("{prefix} {}", row.text))
                                    .monospace()
                                    .color(color),
                            );
                        }
                    },
                );
            });
            ui.horizontal(|ui| {
                // The copilot writes RHAI: refused over a Plugin's Rust buffer, with the reason on
                // hover (`rhai_writer_blocked_reason`). Discard stays available — it touches no
                // buffer.
                let mut apply = ActionButton::primary("Apply to editor");
                if let Some(why) = self.rhai_writer_blocked_reason() {
                    apply = apply.disabled_because(why);
                }
                if ui.add(apply).clicked() {
                    self.apply_copilot_result(&last);
                }
                if ui.button("Discard").clicked() {
                    self.chat.last = None;
                }
            });
        }
        ui.add_space(6.0);
        ui.separator();
        if ui
            .button("Connect to Claude")
            .on_hover_text("Generate the MCP connect command")
            .clicked()
        {
            // `vike-cli mcp`'s run/list tools need a RUNNING vike-datahub server (the retired
            // vike-mcp read a local store instead), so this used to hand over "the datahub the
            // Studio's Remote backend dials".
            //
            // ⚠ **NEITHER backend's address is the datahub's, and the match that stood here said
            // so in one arm while contradicting it in the other.** The `Named` arm passed `None`
            // with the reason written out - its address is the COMPUTE daemon's, MCP's tools dial
            // the DATA daemon, and handing one over points them at a socket that refuses their
            // verbs BY PLANE. Since ruling 7 that is equally true of `Remote`: both variants dial
            // `vike-backend backtest --addr`. So the `Remote` arm was committing the exact defect
            // the `Named` arm beside it was written to avoid, and the comment above them asserted
            // the premise that made it look correct.
            //
            // `None` lets `vike-cli mcp` fall back to its own datahub default, which is the only
            // address here that is actually a datahub's.
            self.chat.connect_to_claude(None);
        }
        if let Some(cmd) = self.chat.connect_command.clone() {
            let mut cmd_display = cmd;
            ui.add(
                egui::TextEdit::singleline(&mut cmd_display)
                    .font(tk.mono(TextRole::Body))
                    .min_size(egui::vec2(0.0, tk.metrics.control_h))
                    .desired_width(f32::INFINITY),
            );
        }
    }
}

#[path = "studio_tests.rs"]
#[cfg(test)]
mod studio_tests;
