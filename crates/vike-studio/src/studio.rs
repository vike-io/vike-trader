//! `StudioState` — the Studio shell (Run toolbar | editor | Sweep/Validate | results). Holds the
//! store handle and wires the panes to worker-thread Run/Sweep/Walk-Forward. `poll()` folds each
//! worker's outcome in; none of the three ever runs on the egui update loop.
//!
//! NOTE: egui 0.35 collapsed `SidePanel`/`TopBottomPanel` into one `egui::Panel` type (constructors
//! `Panel::left`/`Panel::top`/...) and renamed `show_inside` -> `show` (the old names are `#[deprecated]`
//! shims that would fail this crate's `-D warnings` clippy gate), so this uses the resolved 0.35 API.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use vike_backtest::walkforward::WalkForwardReport;
use vike_backtest::EngineParams;
use vike_script::discover_params;

use crate::chat::{summary_of, ChatApiKeys, ChatOutcome, ChatPane};
use crate::data_browser::DataBrowserPane;
use crate::editor::EditorPane;
use crate::indicators::IndicatorsPane;
use crate::picker::SlicePicker;
use crate::remote::Backend;
use crate::research::{ResearchAction, ResearchPane};
use crate::results::{results_ui, ResultsTab};
use crate::saved::{
    comparison_rows, save_strategies, SavedAction, SavedPane, SavedStrategy, StrategySource,
};
use crate::workspace::{
    load_workspace, save_workspace, workspace_read_path, workspace_write_path, StudioWorkspace,
};
use vike_data::TsRange;
use vike_studio_core::templates::templates;
use vike_studio_core::{
    native_strategies, params_from_rows, spawn_compare_all, spawn_run, spawn_study, spawn_sweep,
    spawn_walkforward, CompareOutcome, RunError, RunOutcome, SliceKind, StoreHandle, StrategySpec,
    StudioSweep, StudyRun, StudyRunError,
};

/// The saved-strategy list's filename, colocated with the Studio's per-store state directory
/// (`state_dir.join(...)` — the caller-supplied stand-in for the store root, now that the store
/// is a trait handle with no `root()`; vike-app passes its resolved local hist-store root, so a
/// local session keeps the exact old location) — per-store state alongside the data it was
/// written against.
const SAVED_STRATEGIES_FILE: &str = "studio_strategies.json";

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

    /// The vertical icon rail's glyph for this tool (egui renders unicode directly — no icon
    /// font needed). Picked to be legible at rail size and visually distinct from one another;
    /// the rail shows `icon()` with `label()` as the hover tooltip (see `ui()`).
    pub fn icon(self) -> &'static str {
        match self {
            RightTab::Sweep => "\u{2697}",       // ⚗ (alembic) — sweep/validate
            RightTab::Strategy => "\u{1F9E9}",   // 🧩 (puzzle piece) — strategy source/params
            RightTab::Data => "\u{1F5C4}",       // 🗄 (file cabinet) — data browser
            RightTab::Indicators => "\u{1F4C8}", // 📈 (chart increasing) — indicators
            RightTab::Saved => "\u{2605}",       // ★ (star) — saved strategies
            RightTab::Research => "\u{1F52C}",   // 🔬 (microscope) — studies + their runs
            RightTab::Chat => "\u{1F4AC}",       // 💬 (speech balloon) — AI chat
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

/// WHICH producer's results the central panel is showing.
///
/// Two surfaces rather than one, and the split is R6's own: a backtest's numbers come from the
/// event-driven simulator and a study's do not, so `crate::results::results_ui` (which renders a
/// `vike_backtest::BacktestResult`) and `crate::research::study_result_ui` (which renders a
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

/// The tab that follows `t` in `RightTab::ALL`'s display order, wrapping from the last tab back
/// to the first. Pure helper behind the Ctrl/Cmd+/ shortcut (`StudioState::ui`) — kept
/// free-standing so it's unit-testable without an egui context.
pub fn next_tab(t: RightTab) -> RightTab {
    let i = RightTab::ALL.iter().position(|&x| x == t).unwrap_or(0);
    RightTab::ALL[(i + 1) % RightTab::ALL.len()]
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

pub struct StudioState {
    /// The history store, as the TRAIT handle (split-plane B12) — a local `DataFusionHist` or an
    /// RPC-backed `RemoteHistStore`; every pane reads through `HistStore` verbs only.
    pub store: StoreHandle,
    /// Where the Studio's per-store state files live (`studio_strategies.json`, the AI copilot
    /// ledger, and the legacy-workspace read fallback). Caller-supplied because the trait has no
    /// `root()` — a filesystem concept an RPC store does not have — so the BINARY resolves the
    /// directory; vike-app passes its resolved local hist-store root, which keeps a local session
    /// byte-identical to the pre-trait behavior.
    state_dir: PathBuf,
    /// True when [`Self::store`] is an RPC-backed REMOTE store. Set by the binary after
    /// construction (alongside seeding [`Self::backend`]), like the other runtime toggles.
    /// Drives the split-plane tick rule — see [`crate::remote::remote_store_tick_refusal`].
    pub store_is_remote: bool,
    pub editor: EditorPane,
    pub picker: SlicePicker,
    pub tab: ResultsTab,
    pub running: bool,
    pub last: Option<RunOutcome>,
    run_rx: Option<Receiver<RunOutcome>>,
    /// Sweep grid rows: (param name, comma-separated candidate values as typed in the panel).
    pub grid: Vec<(String, String)>,
    sweep_rx: Option<Receiver<Result<StudioSweep, RunError>>>,
    pub sweep_last: Option<Result<StudioSweep, RunError>>,
    wf_rx: Option<Receiver<Result<WalkForwardReport, RunError>>>,
    pub wf_last: Option<Result<WalkForwardReport, RunError>>,
    /// Index into `templates()` selected by the panel's dropdown.
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
    /// The editor source `compile_status` was last computed for. Recompute (in `poll()`) only
    /// when `editor.source` has drifted from this — compiling on every frame regardless of
    /// keystrokes would be wasteful even though a single compile is cheap (~ms).
    compiled_for: String,
    /// `Some(message)` when `compile_status(&compiled_for)` last came back `Err`; `None` on `Ok`.
    pub compile_err: Option<String>,
    /// The editor source as of the last Save/Load/template-load — the "unsaved changes" baseline.
    /// `editor.source != saved_source` is the AMBER-dot dirty check.
    pub saved_source: String,
    /// The `StudioWorkspace` snapshot last written to disk (or loaded at startup). Compared each
    /// frame in `ui()`'s trailing `maybe_persist_workspace` call so a write only happens when
    /// `right_tab`/`editor_collapsed`/`tools_collapsed`/`template_idx`/`editor.source` actually
    /// drifted — not on every frame.
    persisted_workspace: StudioWorkspace,
    /// QA autorun (see [`StudioState::new_with_qa`]): kick off one backtest automatically on the
    /// first `ui()` frame IF a slice is already selected, so headless captures can show the
    /// results surface (a capture run can't click ▶ Run). Consulted exactly once — the first frame
    /// disarms it unconditionally. Off (false) unless the caller asks for it.
    qa_autorun: bool,
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
    pub strategy_source: StrategySource,
    /// Index into [`native_strategies`] — the Strategy pane's native dropdown. Clamped on read so
    /// a registry that shrinks between versions can never index out of bounds.
    pub native_idx: usize,
    /// The native strategy's free-form `(key, value-text)` param rows (`params_from_rows` types
    /// them at run time). Deliberately NOT a typed/spec-driven form: `vike-backtest` has no param
    /// spec to drive one from — see `vike_studio_core::spec`'s module doc.
    pub native_params: Vec<(String, String)>,
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
    /// PR-5 — WHERE a Run/Sweep/Walk-Forward executes: [`Backend::Local`] (in-process over the local
    /// store, the default and byte-identical to the pre-PR-5 behavior) or [`Backend::Remote`]
    /// (offloaded to a `vike-datahub` server over TCP). A runtime toggle like `strategy_source`, held
    /// on the state struct; not disk-persisted.
    pub backend: Backend,
}

impl StudioState {
    /// The ordinary constructor: restore the persisted workspace, no QA overrides. Reads NO
    /// environment and NO credential store — a library takes its configuration as parameters. The
    /// two QA hooks this used to read here (the forced tool tab and the autorun flag) are now
    /// [`new_with_qa`](Self::new_with_qa) arguments that `vike-app`'s `main.rs` resolves, and
    /// `chat_keys` is the same treatment for the AI-provider keys the ChatPane used to load out of
    /// `<project>/settings/secrets.env` itself (split-plane I8 — see [`ChatApiKeys`]).
    ///
    /// `state_dir` is where the per-store state files land (see the field doc) — the caller's
    /// stand-in for the concrete store's `root()`, which the trait deliberately does not carry.
    pub fn new(store: StoreHandle, state_dir: PathBuf, chat_keys: ChatApiKeys) -> Self {
        Self::new_with_qa(store, state_dir, chat_keys, None, false)
    }

    /// [`new`](Self::new) plus the two headless-capture QA overrides, supplied by the CALLER:
    ///
    /// - `qa_tab` — a raw tab name (`sweep|strategy|data|indicators|saved|chat`, parsed by
    ///   [`RightTab::from_qa_str`]; garbage is ignored, never a panic). `Some` forces that tool tab,
    ///   expands the tools panel, and — load-bearing — puts the session in
    ///   `qa_workspace_readonly` so a capture run can never write the FORCED state over the user's
    ///   real `studio_workspace.json`.
    /// - `qa_autorun` — kick off one backtest on the first `ui()` frame IF a slice is selected, so
    ///   a headless capture can show the results surface (it cannot click ▶ Run).
    ///
    /// The values used to be read straight from `VIKE_STUDIO_TAB` / `VIKE_STUDIO_AUTORUN` inside
    /// this constructor — a LIBRARY reading process environment its caller can neither see nor
    /// override, which is the `Layer::Library` class `crates/vike-ops/tests/settings_registry.rs`
    /// ratchets down. `vike-app`'s `main.rs` does the two reads now; nothing else in the workspace
    /// wants them (the `studio_shot` capture example poses `right_tab` on the struct directly).
    pub fn new_with_qa(
        store: StoreHandle,
        state_dir: PathBuf,
        chat_keys: ChatApiKeys,
        qa_tab: Option<&str>,
        qa_autorun: bool,
    ) -> Self {
        let mut picker = SlicePicker::default();
        picker.refresh(store.as_ref());
        let mut data_browser = DataBrowserPane::default();
        data_browser.refresh(store.as_ref());
        let saved = SavedPane::load(&state_dir.join(SAVED_STRATEGIES_FILE));
        let ws = load_workspace(&workspace_read_path(&state_dir));
        let mut editor = EditorPane::default();
        // Restore the last editor buffer only if the workspace actually captured one — an empty
        // `editor_source` (the field's `Default`, e.g. first launch / no file yet) keeps the
        // known-good default starter script instead of blanking the editor.
        if !ws.editor_source.is_empty() {
            editor.source = ws.editor_source.clone();
        }
        // The restored (or default) script is assumed known-good enough to seed the compile
        // cache eagerly, rather than reporting a stale/blank status for one frame before
        // `poll()` first runs.
        let compile_err = crate::editor::compile_status(&editor.source).err();
        let compiled_for = editor.source.clone();
        let saved_source = editor.source.clone();
        // Clamp against `templates()` shrinking/growing across versions — an out-of-range
        // stored index must never panic the `ComboBox::show_index` call in `ui()`.
        let template_idx = ws.template_idx.min(templates().len().saturating_sub(1));
        // QA: a caller-supplied tab name (<sweep|strategy|data|indicators|saved|chat>) forces the
        // initial tool tab and expands the tools panel for headless per-tab captures — the
        // vike-studio twin of vike-app's style/scale capture hooks. Absent/garbage restores the
        // workspace. While the override is active, workspace persistence is disabled for the
        // session (`qa_workspace_readonly`) so the forced values can never clobber the user's file.
        let qa_tab = qa_tab.and_then(RightTab::from_qa_str);
        let qa_workspace_readonly = qa_tab.is_some();
        let (right_tab, tools_collapsed) = match qa_tab {
            Some(t) => (t, false),
            None => (ws.right_tab, ws.tools_collapsed),
        };
        let editor_collapsed = ws.editor_collapsed;
        let persisted_workspace = StudioWorkspace {
            right_tab,
            editor_collapsed,
            tools_collapsed,
            template_idx,
            editor_source: editor.source.clone(),
        };
        Self {
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
            right_tab,
            editor_collapsed,
            tools_collapsed,
            compare_rx: None,
            compiled_for,
            compile_err,
            saved_source,
            persisted_workspace,
            qa_autorun,
            qa_workspace_readonly,
            last_saved_name: None,
            strategy_source: StrategySource::default(),
            native_idx: 0,
            native_params: Vec::new(),
            backend: Backend::default(),
            // No `host`: the BINARY seeds it after construction (see the field doc), so a Studio
            // built by a test or by a headless capture lists nothing and arms nothing rather than
            // walking for a project directory a library has no business resolving.
            research: ResearchPane::default(),
            study_rx: None,
            study_last: None,
            center: CenterView::default(),
        }
    }

    /// The native strategy name currently selected in the Strategy pane. Clamped against the
    /// registry roster so a stale index is never an out-of-bounds panic.
    pub fn native_name(&self) -> &'static str {
        let roster = native_strategies();
        roster[self.native_idx.min(roster.len().saturating_sub(1))]
    }

    /// What every Run/Sweep/Walk-Forward/Compare-from-the-toolbar executes right now — the ONE
    /// place `strategy_source` is turned into a runnable [`StrategySpec`].
    pub fn current_spec(&self) -> StrategySpec {
        match self.strategy_source {
            StrategySource::Rhai => StrategySpec::rhai(self.editor.source.clone()),
            StrategySource::Native => {
                StrategySpec::native(self.native_name(), params_from_rows(&self.native_params))
            }
        }
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

    /// Persist the current workspace snapshot iff it drifted from `persisted_workspace` since
    /// the last check — called once per frame at the end of `ui()`. The equality comparison (four
    /// scalars and one string) runs every frame, but the disk write only happens on an actual
    /// change, so typing in the editor writes once per drift rather than once per frame. A write
    /// failure is swallowed (best-effort UI state, like `persist_saved`).
    fn maybe_persist_workspace(&mut self) {
        // A `VIKE_STUDIO_TAB` capture session never writes: any drift (even one editor
        // keystroke) would persist the FORCED tab + expanded state over the user's real file.
        if self.qa_workspace_readonly {
            return;
        }
        let current = StudioWorkspace {
            right_tab: self.right_tab,
            editor_collapsed: self.editor_collapsed,
            tools_collapsed: self.tools_collapsed,
            template_idx: self.template_idx,
            editor_source: self.editor.source.clone(),
        };
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
                // unsaved-changes dot, which is a Rhai-editor-only concept).
                let entry = match self.strategy_source {
                    StrategySource::Rhai => {
                        let code = self.editor.source.clone();
                        self.saved_source = code.clone();
                        SavedStrategy::rhai(name.clone(), code)
                    }
                    StrategySource::Native => SavedStrategy::native(
                        name.clone(),
                        self.native_name(),
                        self.native_params.clone(),
                    ),
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
        // The split-plane tick rule: a tick slice over a remote store must run on the Remote
        // backend (see `remote_store_tick_refusal`'s doc). Refuse with the one-liner instead of
        // dispatching a run that could only fail after dialling.
        if let Some(msg) = crate::remote::remote_store_tick_refusal(
            self.store_is_remote,
            slice.kind,
            &self.backend,
        ) {
            self.last = Some(Err(RunError::Data(msg.to_string())));
            return;
        }
        let spec = self.current_spec();
        // Local runs in-process (byte-identical to before); Remote dials a vike-datahub server and
        // returns the SAME `Receiver<RunOutcome>`, so `run_rx`/`poll` are unchanged either way.
        let rx = match &self.backend {
            Backend::Local => {
                let store = self.store.clone();
                spawn_run(spec, slice, store, EngineParams::default())
            }
            Backend::Remote { addr } => crate::remote::spawn_run_remote(addr.clone(), spec, slice),
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
        };
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
        // Same split-plane tick rule as `start_run` — every Backend dispatch carries it.
        if let Some(msg) = crate::remote::remote_store_tick_refusal(
            self.store_is_remote,
            slice.kind,
            &self.backend,
        ) {
            self.sweep_last = Some(Err(RunError::Data(msg.to_string())));
            return;
        }
        let spec = self.current_spec();
        let rx = match &self.backend {
            Backend::Local => {
                let store = self.store.clone();
                spawn_sweep(spec, slice, store, grid)
            }
            Backend::Remote { addr } => {
                crate::remote::spawn_sweep_remote(addr.clone(), spec, slice, grid)
            }
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
        // Same split-plane tick rule as `start_run` — every Backend dispatch carries it.
        if let Some(msg) = crate::remote::remote_store_tick_refusal(
            self.store_is_remote,
            slice.kind,
            &self.backend,
        ) {
            self.wf_last = Some(Err(RunError::Data(msg.to_string())));
            return;
        }
        let spec = self.current_spec();
        let rx = match &self.backend {
            Backend::Local => {
                let store = self.store.clone();
                spawn_walkforward(spec, slice, store, 4)
            }
            Backend::Remote { addr } => {
                crate::remote::spawn_walkforward_remote(addr.clone(), spec, slice, 4)
            }
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

    /// True while any worker-thread task (Run, Sweep, Walk-Forward, or the Saved pane's Compare
    /// all) is in flight — what the toolbar's Cancel button shows/hides on.
    pub fn any_running(&self) -> bool {
        self.running
            || self.sweep_rx.is_some()
            || self.wf_rx.is_some()
            || self.compare_rx.is_some()
            || self.study_rx.is_some()
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
        if self.editor.source != self.compiled_for {
            self.compile_err = crate::editor::compile_status(&self.editor.source).err();
            self.compiled_for = self.editor.source.clone();
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
        match poll_worker(&mut self.compare_rx) {
            Delivery::Ready(results) => {
                // Snapshot the name -> source map first: `comparison_rows` borrows it while
                // `self.saved.compare_rows` is being assigned.
                let sources: Vec<(String, StrategySource)> =
                    self.saved.strategies.iter().map(|s| (s.name.clone(), s.source)).collect();
                let rows = comparison_rows(&results, |name| {
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

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();
        // QA autorun (see the field doc): consult on the FIRST frame only — disarm
        // unconditionally so a launch against an empty store can never leave the flag armed to
        // fire a surprise backtest minutes later, the moment a slice first becomes selectable
        // (review finding: `picker.refresh` auto-selects row 0 once data appears).
        if self.qa_autorun {
            self.qa_autorun = false;
            if !self.running && self.picker.selected().is_some() {
                self.start_run();
            }
        }
        // Keyboard shortcuts (checked once per frame): Ctrl/Cmd+Enter runs the backtest,
        // Ctrl/Cmd+S saves the current editor buffer, Ctrl/Cmd+/ cycles the right-hand tool tab.
        // `modifiers.command` is Cmd on macOS / Ctrl elsewhere — the cross-platform egui idiom.
        let cmd = ui.input(|i| i.modifiers.command);
        if cmd {
            let (run_pressed, save_pressed, cycle_pressed) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::S),
                    i.key_pressed(egui::Key::Slash),
                )
            });
            if run_pressed && !self.running && self.picker.selected().is_some() {
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
                // Brand block: the Studio glyph + name in the accent violet, then the data-slice
                // picker and the ONE primary action (Run). Everything transient (spinner, Cancel)
                // is right-aligned so the left half of the bar never jumps around mid-run.
                ui.label(egui::RichText::new("⚗").size(16.0).color(crate::theme::ACCENT));
                ui.label(egui::RichText::new("Studio").strong().size(15.0));
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                self.picker.ui(ui);
                ui.add_space(4.0);
                // WHICH strategy ▶ Run will execute. Without this the toolbar looks identical in
                // both modes while running completely different code — clicking it jumps to the
                // Strategy tab.
                let (chip_color, chip_text, chip_tip) = match self.strategy_source {
                    StrategySource::Rhai => (
                        crate::theme::ACCENT,
                        "\u{1F9E9} rhai".to_string(),
                        "Running the editor buffer — click to change",
                    ),
                    StrategySource::Native => (
                        crate::theme::OK,
                        format!("\u{1F9E9} {}", self.native_name()),
                        "Running a native registry strategy — click to change",
                    ),
                };
                if ui
                    .button(egui::RichText::new(chip_text).size(11.0).color(chip_color))
                    .on_hover_text(chip_tip)
                    .clicked()
                {
                    self.right_tab = RightTab::Strategy;
                    self.tools_collapsed = false;
                }
                ui.add_space(4.0);
                let can_run = !self.running && self.picker.selected().is_some();
                if ui
                    .add_enabled(can_run, crate::theme::primary("▶ Run"))
                    .on_hover_text("Run backtest  (Ctrl+Enter)")
                    .clicked()
                {
                    self.start_run();
                }
                if ui.button("⟳ Refresh").on_hover_text("Re-scan the data store").clicked() {
                    self.picker.refresh(self.store.as_ref());
                    self.data_browser.refresh(self.store.as_ref());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.any_running() {
                        if ui.button("✖ Cancel").on_hover_text("Abandon in-flight runs").clicked()
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
                            } else {
                                "comparing saved…"
                            })
                            .weak(),
                        );
                    }
                });
            });
            // PR-5 backend selector (second toolbar line): WHERE a Run/Sweep/Walk-Forward executes —
            // Local (in-process, the default, byte-identical to pre-PR-5) or a remote vike-datahub
            // server dialed over TCP. Kept off the busy primary row above.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Backend").weak().size(11.0));
                let is_remote = matches!(self.backend, Backend::Remote { .. });
                if ui
                    .selectable_label(!is_remote, "Local")
                    .on_hover_text("Run in-process over the local store (default)")
                    .clicked()
                {
                    self.backend = Backend::Local;
                }
                if ui
                    .selectable_label(is_remote, "Remote")
                    .on_hover_text("Offload the run to a vike-datahub server over TCP")
                    .clicked()
                    && !is_remote
                {
                    self.backend =
                        Backend::Remote { addr: crate::remote::DEFAULT_REMOTE_ADDR.to_string() };
                }
                if let Backend::Remote { addr } = &mut self.backend {
                    ui.add_sized(
                        [160.0, 20.0],
                        egui::TextEdit::singleline(addr).hint_text("host:port"),
                    )
                    .on_hover_text("datahub server address (host:port)");
                }
            });
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
                        if ui.small_button("▶").on_hover_text("Expand editor").clicked() {
                            self.editor_collapsed = false;
                        }
                    });
                },
            );
        } else {
            egui::Panel::left("studio-editor").resizable(true).default_size(460.0).show(ui, |ui| {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui.small_button("◀").on_hover_text("Collapse editor").clicked() {
                        self.editor_collapsed = true;
                    }
                    ui.label(egui::RichText::new("Editor").strong().size(15.0));
                    ui.add_space(4.0);
                    // Compile-status chip: green "compiles" when the last-compiled source was
                    // clean, red "line N" (full message on hover) otherwise — same facts as the
                    // old glance-dots, now readable without hovering.
                    match &self.compile_err {
                        None => {
                            crate::theme::chip(ui, crate::theme::OK, "● compiles")
                                .on_hover_text("Compiles OK");
                        }
                        Some(msg) => {
                            let label = match crate::editor::error_line(msg) {
                                Some(line) => format!("● error · line {line}"),
                                None => "● error".to_string(),
                            };
                            crate::theme::chip(ui, crate::theme::ERR, &label)
                                .on_hover_text(msg.as_str());
                        }
                    }
                    // Unsaved-changes chip: amber while the buffer has drifted from the last
                    // Save/Load/template-load baseline.
                    if self.editor.source != self.saved_source {
                        crate::theme::chip(ui, crate::theme::WARN, "● unsaved")
                            .on_hover_text("Unsaved changes — Ctrl+S saves to the Saved list");
                    }
                });
                // Inline compile-error banner ABOVE the editor (under the header), not below it:
                // the chip is a glance affordance, but a failing compile deserves a message
                // visible without hovering — and above the editor it can never be pushed off the
                // panel bottom if the editor's row estimate runs long.
                if let Some(msg) = &self.compile_err {
                    ui.colored_label(crate::theme::ERR, crate::editor::format_compile_error(msg));
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
                    let button = egui::Button::new(egui::RichText::new(tab.icon()).size(16.0))
                        .min_size(egui::vec2(RAIL_WIDTH - 8.0, 30.0))
                        .fill(if active {
                            crate::theme::ACCENT.linear_multiply(0.18)
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .frame(active);
                    let resp = ui.add(button).on_hover_text(tab.label());
                    if active {
                        let mut bar = resp.rect;
                        bar.set_right(bar.left() + 2.5);
                        ui.painter().rect_filled(bar, 0.0, crate::theme::ACCENT);
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
                                    if let Some(action) = self.saved.ui(ui) {
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
                let sweep = self.sweep_last.as_ref().and_then(|r| r.as_ref().ok());
                let wf = self.wf_last.as_ref().and_then(|r| r.as_ref().ok());
                let single = self.last.as_ref().and_then(|r| r.as_ref().ok());
                // show the single-run result if present, else the sweep's best entry (so Equity/Trades
                // have data to render even when the user only ran a sweep).
                let r = single.or_else(|| sweep.map(|s| &s.entries[s.best_index].result));
                match r {
                    Some(res) => results_ui(ui, &mut self.tab, res, sweep, wf),
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
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("running study…").weak());
                });
            }
            None => {
                ui.add_space((ui.available_height() * 0.26).max(16.0));
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("\u{1F52C}")
                            .size(42.0)
                            .color(crate::theme::ACCENT.linear_multiply(0.45)),
                    );
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("No study result yet").strong().size(17.0));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Pick a study in the Research tool, then ▶ Run study.")
                            .weak(),
                    );
                });
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
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("⚗")
                    .size(42.0)
                    .color(crate::theme::ACCENT.linear_multiply(0.45)),
            );
            ui.add_space(6.0);
            ui.label(egui::RichText::new("No results yet").strong().size(17.0));
            ui.add_space(4.0);
            if let Some(err) = self.picker.error() {
                ui.label(
                    egui::RichText::new(format!("{}\n\n{err}", crate::picker::SERIES_SCAN_ADVICE))
                        .color(crate::theme::ERR),
                );
            } else if !self.picker.depth_only().is_empty() && self.picker.available().is_empty() {
                // The store is NOT empty — it holds series no slice can replay. Sending this
                // operator to a backfill would have them re-fetch data they already have.
                ui.label(
                    egui::RichText::new(crate::picker::DEPTH_NOT_REPLAYABLE)
                        .weak()
                        .color(crate::theme::WARN),
                );
            } else if self.picker.available().is_empty() {
                ui.label(
                    egui::RichText::new(
                        "The data store is empty — backfill some history first\n\
                         (vike-backfill, or the Data Manager's Backfill), then ⟳ Refresh.",
                    )
                    .weak(),
                );
            } else {
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
                        let can_run = !self.running && self.picker.selected().is_some();
                        if ui
                            .add_enabled(can_run, crate::theme::primary("▶ Run backtest"))
                            .on_hover_text("Ctrl+Enter")
                            .clicked()
                        {
                            self.start_run();
                        }
                        if ui.button("Load a template").clicked() {
                            self.editor.source = templates()[self.template_idx].1.to_string();
                            self.saved_source = self.editor.source.clone();
                        }
                    },
                );
            }
        });
    }

    /// The central panel when the last run/sweep/walk-forward failed: a visible failure header
    /// (`title` names WHICH action failed) + the message in a framed monospace block (was: one
    /// bare red line in the void).
    fn error_state(ui: &mut egui::Ui, title: &str, msg: &str) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("✗").size(30.0).color(crate::theme::ERR));
            ui.label(egui::RichText::new(title).strong().size(16.0));
        });
        ui.add_space(8.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(msg).monospace().color(crate::theme::ERR));
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
        crate::theme::section_header(ui, "\u{1F9E9}", "Strategy");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.strategy_source, StrategySource::Rhai, "Rhai script")
                .on_hover_text("Run the editor buffer (the original Studio path)");
            ui.selectable_value(&mut self.strategy_source, StrategySource::Native, "Native (Rust)")
                .on_hover_text("Run a compiled strategy from the vike-backtest registry");
        });
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
                ui.label(egui::RichText::new("Registry strategy").weak().size(11.0));
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
                        if ui.small_button("\u{2715}").on_hover_text("Remove").clicked() {
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
                    ui.label(egui::RichText::new("Resolved params").weak().size(11.0));
                    ui.add(
                        egui::Label::new(egui::RichText::new(rendered).monospace().size(11.0))
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
        crate::theme::section_header(ui, "⚗", "Sweep & Validate");
        ui.label(egui::RichText::new("Template").weak().size(11.0));
        ui.horizontal(|ui| {
            let combo_w = (ui.available_width() - 64.0).max(80.0);
            egui::ComboBox::from_id_salt("studio-template").width(combo_w).show_index(
                ui,
                &mut self.template_idx,
                templates().len(),
                |i| templates()[i].0.to_string(),
            );
            if ui.button("Load").on_hover_text("Load this template into the editor").clicked() {
                self.editor.source = templates()[self.template_idx].1.to_string();
                self.saved_source = self.editor.source.clone();
            }
        });
        // Gallery: preview each starter script before loading it (the dropdown loads blind).
        let mut gallery_loaded = false;
        egui::CollapsingHeader::new("Browse templates").default_open(false).show(ui, |ui| {
            gallery_loaded = crate::templates_gallery::gallery_ui(ui, &mut self.editor.source);
        });
        if gallery_loaded {
            self.saved_source = self.editor.source.clone();
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
                })
                .weak(),
            );
        }
        for (name, csv) in self.grid.iter_mut() {
            ui.horizontal(|ui| {
                // Fixed, truncating name column: a long script param name must squeeze itself,
                // not push the value field past the panel edge.
                ui.add_sized([96.0, 18.0], egui::Label::new(name.as_str()).truncate())
                    .on_hover_text(name.as_str());
                let w = (ui.available_width() - 8.0).max(40.0);
                ui.add_sized([w, 18.0], egui::TextEdit::singleline(csv).hint_text("1, 2, 3"));
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
        let can = !self.running && self.picker.selected().is_some();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    can && grid_ok && self.sweep_rx.is_none(),
                    crate::theme::primary("▶ Run Sweep"),
                )
                .on_hover_text(if grid_ok {
                    "Backtest every grid combination and rank by Sharpe"
                } else {
                    "Seed a parameter grid first (needs at least one numeric value)"
                })
                .clicked()
            {
                self.start_sweep();
            }
            if ui
                .add_enabled(
                    self.wf_rx.is_none() && self.picker.selected().is_some(),
                    egui::Button::new("Walk-Forward"),
                )
                .on_hover_text("4-split out-of-sample validation")
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
            ui.colored_label(crate::theme::ERR, format!("sweep failed: {e}"));
        }
        if let Some(Err(e)) = &self.wf_last {
            ui.add_space(4.0);
            ui.colored_label(crate::theme::ERR, format!("walk-forward failed: {e}"));
        }
    }

    /// The AI Copilot tool pane (extracted from the old inline match arm; behavior unchanged,
    /// layout restyled: header, full-width provider/input, wrapped transcript, one primary Send).
    fn chat_pane_ui(&mut self, ui: &mut egui::Ui) {
        crate::theme::section_header(ui, "💬", "AI Copilot");
        if self.chat.available_providers().is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "No provider key found — set ANTHROPIC_API_KEY or CEREBRAS_API_KEY \
                         in the workspace .env.",
                    )
                    .weak(),
                )
                .wrap(),
            );
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
                        crate::theme::ACCENT
                    } else {
                        ui.visuals().strong_text_color()
                    };
                    ui.label(egui::RichText::new(role).color(color).strong().size(11.0));
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
        let can_send = !self.chat.running
            && self.chat_rx.is_none()
            && self.chat.has_key()
            && !self.chat.input.trim().is_empty()
            && self.picker.selected().is_some();
        ui.horizontal(|ui| {
            if ui.add_enabled(can_send, crate::theme::primary("Send")).clicked() {
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
            // Same sign convention as every table in the Studio: positive OK, negative ERR,
            // zero/NaN neutral (WARN here previously disagreed with the compare/sweep tables).
            let oos_color = if last.oos_sharpe > 0.0 {
                crate::theme::OK
            } else if last.oos_sharpe < 0.0 {
                crate::theme::ERR
            } else {
                ui.visuals().text_color()
            };
            crate::theme::chip(
                ui,
                oos_color,
                &format!("OOS Sharpe {:.2} · {} trades", last.oos_sharpe, last.n_trades),
            );
            // Review changes: a line-level diff of the current buffer -> the generated script,
            // so Apply is a reviewed action rather than a blind clobber.
            egui::CollapsingHeader::new("Review changes").default_open(true).show(ui, |ui| {
                let rows = crate::chat::diff_rows(&self.editor.source, &last.code);
                egui::ScrollArea::both().id_salt("studio-chat-diff").max_height(180.0).show(
                    ui,
                    |ui| {
                        for row in &rows {
                            let (prefix, color) = match row.kind {
                                crate::chat::DiffKind::Insert => ("+", crate::theme::OK),
                                crate::chat::DiffKind::Delete => ("-", crate::theme::ERR),
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
                if ui.add(crate::theme::primary("Apply to editor")).clicked() {
                    crate::chat::apply_result(&mut self.editor.source, &last);
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
            // Point the spawned `vike-cli mcp` at the datahub the Studio's Remote backend dials
            // (its run/list tools need a RUNNING vike-datahub server — the retired vike-mcp read a
            // local store instead); Local backend passes None and the server uses its own default.
            let addr = match &self.backend {
                Backend::Remote { addr } => Some(addr.as_str()),
                Backend::Local => None,
            };
            self.chat.connect_to_claude(addr);
        }
        if let Some(cmd) = self.chat.connect_command.clone() {
            let mut cmd_display = cmd;
            ui.add(egui::TextEdit::singleline(&mut cmd_display).desired_width(f32::INFINITY));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vike_data::{DataFusionHist, HistStore};
    use vike_model::Bar;

    /// `StudioState::new` with the pairing vike-app passes for a LOCAL store: `state_dir` = the
    /// store's own root (here the test's temp dir), keeping the old `store.root()` colocations
    /// byte-identical. The `Arc<DataFusionHist>` → `StoreHandle` coercion happens at the call.
    fn state_new(dir: &tempfile::TempDir, store: Arc<DataFusionHist>) -> StudioState {
        StudioState::new(store, dir.path().to_path_buf(), ChatApiKeys::default())
    }

    /// `next_tab` cycles through every `RightTab::ALL` entry in display order and wraps from
    /// the last tab back to the first — the pure helper behind the Ctrl/Cmd+/ shortcut.
    #[test]
    fn next_tab_cycles_through_every_tab_and_wraps() {
        let start = RightTab::ALL[0];
        let mut t = start;
        let mut seen = vec![t];
        for _ in 0..RightTab::ALL.len() - 1 {
            t = next_tab(t);
            seen.push(t);
        }
        assert_eq!(seen, RightTab::ALL.to_vec(), "should visit every tab in display order");
        assert_eq!(next_tab(t), start, "the last tab should wrap back to the first");
    }

    #[test]
    fn right_tab_default_is_sweep_and_every_tab_has_a_label() {
        assert_eq!(RightTab::default(), RightTab::Sweep);
        assert_eq!(RightTab::ALL.len(), 7);
        for tab in RightTab::ALL {
            assert!(!tab.label().is_empty());
        }
        // Labels are distinct (no two tabs share a caption).
        let labels: Vec<&str> = RightTab::ALL.iter().map(|t| t.label()).collect();
        let mut uniq = labels.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), labels.len());
    }

    /// `from_qa_str` (the `VIKE_STUDIO_TAB` capture hook) round-trips every tab by its lowercase
    /// name and rejects garbage rather than panicking.
    #[test]
    fn from_qa_str_parses_every_tab_and_rejects_garbage() {
        let names = ["sweep", "strategy", "data", "indicators", "saved", "research", "chat"];
        for (name, want) in names.iter().zip(RightTab::ALL) {
            assert_eq!(RightTab::from_qa_str(name), Some(want));
        }
        assert_eq!(RightTab::from_qa_str(""), None);
        assert_eq!(RightTab::from_qa_str("Sweep"), None, "exact lowercase only");
        assert_eq!(RightTab::from_qa_str("nonsense"), None);
    }

    /// A temp-dir-backed store seeded with ~400 oscillating 1m bars for `binance/BTCUSDT`, so the
    /// default SMA-crossover editor script has something to cross on. Returns the `TempDir` so the
    /// caller binds it and keeps the backing directory alive for the test's lifetime (the fragile
    /// `std::mem::forget` + shared-temp-path approach from the original plan is deliberately not
    /// used here — see Task 2's `run.rs::tests::seeded_store` for the pattern this mirrors).
    fn seeded_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let bars: Vec<Bar> = (0..400)
            .map(|i| {
                let c = 100.0 + (i % 7) as f64;
                Bar {
                    ts: 60_000 * (i as i64 + 1),
                    open: c,
                    high: c,
                    low: c,
                    close: c,
                    volume: 0.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some("BTCUSDT".into()),
                }
            })
            .collect();
        store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
        (dir, Arc::new(store))
    }

    #[test]
    fn editor_and_tools_panes_start_expanded() {
        let (_dir, store) = seeded_store();
        let st = state_new(&_dir, store);
        assert!(!st.editor_collapsed);
        assert!(!st.tools_collapsed);
    }

    /// `new()` seeds the compile cache with the default (known-good) script, and the "unsaved"
    /// baseline starts equal to the source — no red/amber dot on first paint.
    #[test]
    fn new_state_starts_with_a_clean_compile_and_no_unsaved_marker() {
        let (_dir, store) = seeded_store();
        let st = state_new(&_dir, store);
        assert!(st.compile_err.is_none());
        assert_eq!(st.editor.source, st.saved_source);
    }

    /// `poll()` recompiles only when the source drifted, and reports the compile error for broken
    /// source.
    #[test]
    fn poll_updates_compile_err_when_source_changes() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.editor.source = "fn on_bar( {".to_string();
        st.poll();
        assert!(st.compile_err.is_some());

        st.editor.source = crate::editor::EditorPane::default().source;
        st.poll();
        assert!(st.compile_err.is_none());
    }

    /// Saving or loading a strategy re-baselines `saved_source` so the unsaved-changes dot clears.
    #[test]
    fn save_and_load_rebaseline_saved_source() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.editor.source = "fn on_bar() { market(1, 1.0); }".to_string();
        assert_ne!(st.editor.source, st.saved_source);

        st.saved.save_name = "dirty-then-clean".to_string();
        st.handle_saved_action(SavedAction::SaveCurrent);
        assert_eq!(st.editor.source, st.saved_source, "Save should re-baseline");

        st.editor.source = "stale-edit".to_string();
        assert_ne!(st.editor.source, st.saved_source);
        st.handle_saved_action(SavedAction::Load(0));
        assert_eq!(st.editor.source, st.saved_source, "Load should re-baseline");
    }

    #[test]
    fn start_run_then_poll_reaches_a_result() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.picker.refresh(st.store.as_ref());
        st.picker.select(0);
        st.start_run();
        assert!(st.running);
        // block until the worker delivers, then poll folds it in
        for _ in 0..200 {
            st.poll();
            if !st.running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!st.running, "run should complete");
        assert!(matches!(st.last, Some(Ok(_)) | Some(Err(_))));
    }

    /// Spec §7 regression: if the worker's sender is dropped without ever sending (the
    /// worker-thread-panic case, simulated here directly), `poll()` must observe the disconnect
    /// and clear `running` rather than treat `Disconnected` the same as `Empty` forever — the
    /// bug that would leave the spinner stuck and Run disabled for the process lifetime.
    #[test]
    fn poll_clears_running_when_worker_disconnects_without_sending() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        let (tx, rx) = std::sync::mpsc::channel::<RunOutcome>();
        drop(tx);
        st.run_rx = Some(rx);
        st.running = true;

        st.poll();

        assert!(!st.running, "poll() must clear running on a disconnected channel");
        assert!(matches!(st.last, Some(Err(_))), "disconnect should surface as a run failure");
    }

    /// Mirrors `poll_clears_running_when_worker_disconnects_without_sending` for the `chat_rx`
    /// arm: `ChatOutcome`'s doc claims a worker panic surfaces as a human-readable failure, same
    /// as the run/sweep/walk-forward arms do. Before this fix the disconnect arm cleared
    /// `chat.running`/`chat_rx` but never pushed anything to the transcript, silently swallowing
    /// the failure -- making the doc false.
    #[test]
    fn poll_surfaces_chat_worker_disconnect_in_the_transcript() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        let (tx, rx) = std::sync::mpsc::channel::<ChatOutcome>();
        drop(tx);
        st.chat_rx = Some(rx);
        st.chat.running = true;

        st.poll();

        assert!(!st.chat.running, "poll() must clear chat.running on a disconnected channel");
        let (role, text) = st.chat.history.last().expect("a transcript line must be pushed");
        assert_eq!(role, "assistant");
        assert!(
            text.to_lowercase().contains("terminated") || text.to_lowercase().contains("failed"),
            "transcript line should read as a human-readable worker failure, got: {text}"
        );
    }

    #[test]
    fn seed_grid_from_discovered_params() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.editor.source = "let fast = param(\"fast\", 5.0);\nfn on_bar() {}".to_string();
        st.seed_sweep_grid(); // discover_params -> grid rows
        assert_eq!(st.grid.len(), 1);
        assert_eq!(st.grid[0].0, "fast");
    }

    /// `SaveCurrent` snapshots `editor.source` under `saved.save_name`, appends it to
    /// `saved.strategies`, and persists to `state_dir/studio_strategies.json` — a fresh
    /// `StudioState::new` over the same store must reload it (the round-trip the whole feature
    /// exists for).
    #[test]
    fn save_current_persists_and_reloads_across_a_new_studio_state() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store.clone());
        st.editor.source = "fn on_bar() {}".to_string();
        st.saved.save_name = "my-strategy".to_string();
        st.handle_saved_action(SavedAction::SaveCurrent);

        assert_eq!(st.saved.strategies.len(), 1);
        assert_eq!(st.saved.strategies[0].name, "my-strategy");
        assert!(st.saved.save_name.is_empty(), "the name box should clear after saving");

        // a fresh StudioState over the SAME store root reloads the file SaveCurrent wrote.
        let reloaded = state_new(&_dir, store);
        assert_eq!(reloaded.saved.strategies.len(), 1);
        assert_eq!(reloaded.saved.strategies[0].name, "my-strategy");
        assert_eq!(reloaded.saved.strategies[0].code, "fn on_bar() {}");
    }

    /// Saving again under the same name overwrites the code in place rather than appending a
    /// duplicate row.
    #[test]
    fn save_current_with_an_existing_name_overwrites_in_place() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.editor.source = "fn on_bar() {}".to_string();
        st.saved.save_name = "v1".to_string();
        st.handle_saved_action(SavedAction::SaveCurrent);

        st.editor.source = "fn on_bar() { market(1, 1.0); }".to_string();
        st.saved.save_name = "v1".to_string();
        st.handle_saved_action(SavedAction::SaveCurrent);

        assert_eq!(st.saved.strategies.len(), 1, "same name updates, doesn't duplicate");
        assert_eq!(st.saved.strategies[0].code, "fn on_bar() { market(1, 1.0); }");
    }

    #[test]
    fn load_action_copies_the_saved_code_into_the_editor() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.saved.strategies.push(SavedStrategy::rhai("flat", "fn on_bar() {}"));
        st.editor.source = "stale".to_string();

        st.handle_saved_action(SavedAction::Load(0));

        assert_eq!(st.editor.source, "fn on_bar() {}");
    }

    #[test]
    fn delete_action_removes_and_persists() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store.clone());
        st.saved.strategies.push(SavedStrategy::rhai("a", "1"));
        st.saved.strategies.push(SavedStrategy::rhai("b", "2"));
        st.persist_saved();

        st.handle_saved_action(SavedAction::Delete(0));

        assert_eq!(st.saved.strategies.len(), 1);
        assert_eq!(st.saved.strategies[0].name, "b");
        let reloaded = state_new(&_dir, store);
        assert_eq!(reloaded.saved.strategies.len(), 1);
        assert_eq!(reloaded.saved.strategies[0].name, "b");
    }

    /// `CompareAll` now runs on a worker thread (`compare_rx`), so this drives `poll()` until it
    /// lands — the `start_run_then_poll_reaches_a_result` pattern. Every saved strategy over the
    /// currently-selected slice gets ranked: the SMA-cross script (the editor's own default)
    /// should out-trade / out-rank the no-op.
    #[test]
    fn compare_all_ranks_saved_strategies_over_the_selected_slice() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.picker.select(0);
        st.saved.strategies.push(SavedStrategy::rhai("no-op", "fn on_bar() {}"));
        st.saved.strategies.push(SavedStrategy::rhai("sma-cross", EditorPane::default().source));

        st.handle_saved_action(SavedAction::CompareAll);
        assert!(st.compare_rx.is_some(), "compare should be running on a worker thread");
        for _ in 0..200 {
            st.poll();
            if st.compare_rx.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(st.compare_rx.is_none(), "compare should complete");

        let rows = st.saved.compare_rows.expect("compare should populate rows");
        assert_eq!(rows.len(), 2);
        assert!(st.saved.compare_error.is_none());
        let noop = rows.iter().find(|r| r.name == "no-op").unwrap();
        assert_eq!(noop.n_trades, 0, "the no-op strategy never trades");
        let cross = rows.iter().find(|r| r.name == "sma-cross").unwrap();
        assert!(cross.n_trades > 0, "the SMA-cross strategy should trade over 400 bars");
    }

    /// A second `CompareAll` while one is already running is a no-op (mirrors `start_sweep`'s
    /// "already running" guard) — it must not spawn a second worker or clobber `compare_rx`.
    #[test]
    fn compare_all_is_a_no_op_while_already_running() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.picker.select(0);
        st.saved.strategies.push(SavedStrategy::rhai("no-op", "fn on_bar() {}"));

        st.handle_saved_action(SavedAction::CompareAll);
        assert!(st.compare_rx.is_some());

        st.handle_saved_action(SavedAction::CompareAll);
        assert!(st.compare_rx.is_some(), "still running, unchanged");

        for _ in 0..200 {
            st.poll();
            if st.compare_rx.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(st.compare_rx.is_none());
        assert!(st.saved.compare_rows.is_some());
    }

    /// `cancel()` drops the in-flight receiver(s) and resets `running` so a fresh Run/Sweep/
    /// Compare can start immediately — the MVP "abandon" contract (the spawned worker thread
    /// keeps running to completion, but its result is discarded because nothing polls the
    /// receiver anymore, matching every other disconnect path in this file).
    #[test]
    fn cancel_abandons_every_in_flight_worker_and_resets_state() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.picker.select(0);
        st.start_run();
        assert!(st.running);
        assert!(st.any_running());

        st.cancel();

        assert!(!st.running, "cancel should clear the running flag");
        assert!(st.run_rx.is_none(), "cancel should drop the receiver");
        assert!(!st.any_running());
        assert!(st.last.is_none(), "cancel doesn't fabricate a result");

        // the editor/picker/store are all still usable — a fresh Run can start right away.
        st.start_run();
        assert!(st.running, "a new run should be startable immediately after cancel");
    }

    #[test]
    fn any_running_is_false_when_idle_and_true_while_a_sweep_is_in_flight() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        assert!(!st.any_running());
        st.picker.select(0);
        st.grid = vec![("fast".to_string(), "3,5".to_string())];
        st.start_sweep();
        assert!(st.any_running(), "a running sweep should register as any_running");
        st.cancel();
        assert!(!st.any_running());
    }

    // ---- native strategies -------------------------------------------------------------------

    /// The default source is Rhai (byte-identical to the pre-native Studio), and flipping to
    /// Native makes `current_spec` resolve a registry strategy + its typed params instead.
    #[test]
    fn current_spec_follows_the_strategy_source_toggle() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        assert_eq!(st.strategy_source, StrategySource::Rhai);
        assert_eq!(st.current_spec(), StrategySpec::rhai(st.editor.source.clone()));

        st.strategy_source = StrategySource::Native;
        st.native_idx = native_strategies().iter().position(|n| *n == "buy_hold").unwrap();
        st.native_params = vec![("size".into(), "2".into())];
        match st.current_spec() {
            StrategySpec::Native { name, params } => {
                assert_eq!(name, "buy_hold");
                assert_eq!(params.get("size").and_then(|v| v.as_integer()), Some(2));
            }
            other => panic!("expected a native spec, got {other:?}"),
        }
    }

    /// `native_name` clamps a stale/oversized index instead of indexing out of bounds.
    #[test]
    fn native_name_clamps_an_out_of_range_index() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.native_idx = usize::MAX;
        assert_eq!(st.native_name(), *native_strategies().last().unwrap());
    }

    /// A NATIVE strategy runs end-to-end through the Studio's own Run path (no Rhai anywhere):
    /// start_run -> worker -> poll folds in a real `BacktestResult`.
    #[test]
    fn native_run_reaches_a_result_through_the_studio_run_path() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.picker.select(0);
        st.strategy_source = StrategySource::Native;
        st.native_idx = native_strategies().iter().position(|n| *n == "buy_hold").unwrap();
        st.native_params = vec![("symbol".into(), "BTCUSDT".into()), ("size".into(), "2".into())];
        st.start_run();
        assert!(st.running);
        for _ in 0..200 {
            st.poll();
            if !st.running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!st.running, "the native run should complete");
        let res = st.last.expect("an outcome").expect("buy_hold should run");
        assert!(!res.equity_curve.is_empty());
    }

    /// Saving while in Native mode persists the registry name + param rows (not the editor
    /// buffer), and Load restores the mode — the round trip the Strategy tab exists for.
    #[test]
    fn save_and_load_round_trip_a_native_strategy() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store.clone());
        st.strategy_source = StrategySource::Native;
        st.native_idx = native_strategies().iter().position(|n| *n == "buy_hold").unwrap();
        st.native_params = vec![("size".into(), "3".into())];
        st.saved.save_name = "hold-3".to_string();
        st.handle_saved_action(SavedAction::SaveCurrent);

        let mut reloaded = state_new(&_dir, store);
        assert_eq!(reloaded.saved.strategies.len(), 1);
        let entry = &reloaded.saved.strategies[0];
        assert_eq!(entry.source, StrategySource::Native);
        assert_eq!(entry.native, "buy_hold");
        assert_eq!(entry.params, vec![("size".to_string(), "3".to_string())]);
        // ...and a fresh session starts in Rhai mode until the entry is loaded.
        assert_eq!(reloaded.strategy_source, StrategySource::Rhai);
        reloaded.handle_saved_action(SavedAction::Load(0));
        assert_eq!(reloaded.strategy_source, StrategySource::Native);
        assert_eq!(reloaded.native_name(), "buy_hold");
        assert_eq!(reloaded.native_params, vec![("size".to_string(), "3".to_string())]);
        assert_eq!(reloaded.right_tab, RightTab::Strategy, "Load jumps to the Strategy tab");
    }

    /// The sweep grid seeds from the NATIVE param rows when native is active (there is no
    /// `discover_params` twin for the registry), numeric rows only.
    #[test]
    fn seed_grid_from_native_param_rows_skips_non_numeric() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.strategy_source = StrategySource::Native;
        st.native_params = vec![
            ("size".into(), "2".into()),
            ("symbol".into(), "BTCUSDT".into()),
            ("".into(), "9".into()),
        ];
        st.seed_sweep_grid();
        assert_eq!(st.grid, vec![("size".to_string(), "2".to_string())]);
    }

    /// A mixed Saved list (Rhai + native) compares in ONE pass and each row carries its kind.
    #[test]
    fn compare_all_ranks_a_mixed_rhai_and_native_list() {
        let (_dir, store) = seeded_store();
        let mut st = state_new(&_dir, store);
        st.picker.select(0);
        st.saved.strategies.push(SavedStrategy::rhai("no-op", "fn on_bar() {}"));
        st.saved.strategies.push(SavedStrategy::native(
            "hold",
            "buy_hold",
            vec![("symbol".into(), "BTCUSDT".into())],
        ));

        st.handle_saved_action(SavedAction::CompareAll);
        for _ in 0..200 {
            st.poll();
            if st.compare_rx.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let rows = st.saved.compare_rows.expect("compare should populate rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().find(|r| r.name == "no-op").unwrap().source, StrategySource::Rhai);
        let native_row = rows.iter().find(|r| r.name == "hold").unwrap();
        assert_eq!(native_row.source, StrategySource::Native);
        assert!(native_row.error.is_none(), "the native row ran: {:?}", native_row.error);
    }

    #[test]
    fn compare_all_with_no_slice_selected_sets_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap()); // no series -> picker has nothing selected
        let mut st = state_new(&dir, store);
        st.saved.strategies.push(SavedStrategy::rhai("a", "1"));

        st.handle_saved_action(SavedAction::CompareAll);

        assert!(st.saved.compare_rows.is_none());
        assert!(st.saved.compare_error.is_some());
    }
}
