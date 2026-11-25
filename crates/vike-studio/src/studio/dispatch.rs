//! `StudioState`'s dispatchers: the per-button disabled reasons, and the methods that start a
//! worker (Run, Sweep, Walk-Forward, Strategy file Build, Study, the AI copilot's Send) or compute a pane's
//! answer on demand (the Indicators preview), plus the backend selector's switch.
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). The receivers these start are folded back in by `poll`, in
//! `workers.rs`. `use super::*` brings in the parent module's imports and items, so nothing about
//! resolution changes.

use super::*;
use crate::backend::backtesthub::{named_run, sdk_source, strategy_files, studies};
use crate::panes::param_scan::{
    axis_values, constants_as_params, declared_params, rows_for, scan_total,
};
use vike_backtesthub_client::summary::PARAMSCAN_POINTS_MAX;
use vike_backtesthub_client::wire_studio::{
    ParamscanWire, WalkforwardWindowSearchWire, WalkforwardWire,
};
use vike_model::params::{ParamValue, Params};

impl StudioState {
    /// Why Run is disabled — the toolbar's and the empty panel's, which render this ONE answer
    /// so their state and their reason cannot disagree — or `None` when it may go. The condition
    /// is exactly the one both buttons spelled for themselves (not running, a picked slice,
    /// [`Self::run_blocked_reason`]); what is new is that the reason reaches the pointer. egui
    /// shows `on_hover_text` only on an ENABLED widget, so the reason both buttons carried there
    /// was never seen.
    pub(crate) fn run_disabled_reason(&self) -> Option<&'static str> {
        if self.running {
            return Some(RUN_BUSY);
        }
        if let Some(why) = self.picker.refusal() {
            return Some(why);
        }
        self.run_blocked_reason()
    }

    /// Why Run Sweep is disabled, or `None`. `counts` is the caller's reading of the ticked rows: each
    /// one's value count, a row that reads to none left out. The condition is the button's own; a Strategy
    /// file without a sha is refused inside `start_sweep`, as before.
    pub(super) fn sweep_disabled_reason(&self, counts: &[usize]) -> Option<&'static str> {
        if self.running {
            return Some(RUN_BUSY);
        }
        if self.sweep_rx.is_some() {
            return Some(SWEEP_BUSY);
        }
        if let Some(why) = self.picker.refusal() {
            return Some(why);
        }
        if counts.is_empty() {
            return Some(SEED_A_GRID);
        }
        self.grid_refusal(scan_total(counts))
    }

    /// Why Walk-Forward is disabled, or `None`: no walk in flight, a picked slice, and a grid it may send
    /// ([`Self::walkforward_grid_refusal`]).
    pub(super) fn walkforward_disabled_reason(&self) -> Option<&'static str> {
        if self.walkforward_rx.is_some() {
            return Some(WALKFORWARD_BUSY);
        }
        if let Some(why) = self.picker.refusal() {
            return Some(why);
        }
        self.walkforward_grid_refusal()
    }

    /// Why the test conditions' capital and funding switch reach no run, or `None` when they do (or neither is
    /// set): a Named run's request has no field for either (`vike_backtesthub_client::named_run::NamedRunSpec`).
    pub(crate) fn capital_not_sent(&self) -> Option<&'static str> {
        let named = matches!(self.backend, StudioRunBackend::Named { .. });
        let set = self.conditions.capital().is_some() || self.conditions.funding;
        (named && set).then_some(NAMED_BACKEND_CAPITAL)
    }

    /// The engine settings every run sends (`StudioRunEngine`): the test conditions' capital and funding switch.
    pub(crate) fn run_engine(&self) -> StudioRunEngine {
        StudioRunEngine { cash: self.conditions.capital(), funding: self.conditions.funding }
    }

    /// Whether the backtest server advertises `feature`, as its last check answered (`check_server`); `None`
    /// while that is not known (not checked, or not connected). A run decides on the server it dials; this is
    /// what the panes may say before one.
    pub(crate) fn server_serves(&self, feature: &str) -> Option<bool> {
        let features = self.server_check.as_ref()?.as_ref().ok()?;
        Some(features.iter().any(|f| f == feature))
    }

    /// Why a grid of `total` points is refused before it is sent, or `None`: an unticked row's Value that
    /// reads to more than one value ([`Self::multi_value_held`]), or more points than one scan may run
    /// (`PARAMSCAN_POINTS_MAX`, the server's cap too, so the server never sees a grid it would refuse).
    pub(super) fn grid_refusal(&self, total: usize) -> Option<&'static str> {
        if self.multi_value_held().is_some() {
            return Some(HELD_VALUE_IS_ONE);
        }
        (total > PARAMSCAN_POINTS_MAX).then_some(SCAN_TOO_LARGE)
    }

    /// [`Self::grid_refusal`] over the grid a walk-forward sends: the ticked rows when each window
    /// searches them, else none.
    fn walkforward_grid_refusal(&self) -> Option<&'static str> {
        let searched = match self.walkforward_search {
            true => sweep_axes(&self.ticked_rows(), None),
            false => Vec::new(),
        };
        self.grid_refusal(points_of(&searched))
    }

    /// The first unticked row whose Value reads to more than one value (`5, 10`, `1..3`), by name: a held
    /// value joins a Rhai script's grid as an axis ([`Self::held_rows`]), so a list there would run more
    /// points than the scan's size line counts.
    pub(crate) fn multi_value_held(&self) -> Option<String> {
        let mut held = self.held_rows().into_iter();
        held.find(|(_, text)| axis_values(text, None).len() > 1).map(|(name, _)| name)
    }

    /// Why Send is disabled, or `None` — its button's own condition, one reason at a time.
    pub(super) fn send_disabled_reason(&self) -> Option<&'static str> {
        if !self.chat.has_key() {
            return Some(NO_PROVIDER_KEY);
        }
        if matches!(self.backend, StudioRunBackend::Named { .. }) {
            return Some(COPILOT_NEEDS_COMPUTE_KEY);
        }
        if self.chat.running || self.chat_rx.is_some() {
            return Some(COPILOT_BUSY);
        }
        if let Some(why) = self.picker.refusal() {
            return Some(why);
        }
        if self.chat.input.trim().is_empty() {
            return Some(NOTHING_TO_SEND);
        }
        None
    }

    /// Compute the Indicators pane's selected indicator over the currently-picked data slice's
    /// bars (loaded synchronously — the preview compute is O(bars) and cheap; unlike Run, it does
    /// not need a worker thread). A no-slice / load-failure surfaces as the pane's own error.
    ///
    /// ⚠ This is the one store read the Studio still performs ON the paint thread since the ⟳
    /// Refresh walk moved off it (`crate::backend::catalog`). The "cheap" above is true of the COMPUTE and
    /// of a LOCAL store; over a `HistoryStoreDatahub` the `load_bars` below is the same
    /// connect-per-read hazard that module describes, and it stays synchronous here because it is
    /// out of that change's scope, not because it is exempt — moving it means the Indicators pane
    /// growing a receiver and a loading state of its own, the way `start_catalog_load` did.
    pub(super) fn compute_indicator_preview(&mut self) {
        let Some(slice) = self.picker.selected() else {
            self.indicators.preview = None;
            self.indicators.error = Some("pick a data slice first".to_string());
            return;
        };
        // Indicators are bar-series maths; a tick slice has no bars to compute over.
        if slice.kind != ReplayKind::Bar {
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

    /// **Flow step 2** — send the editor buffer to the backtesthub's Build (which forwards it to the
    /// strategy builder), on a worker thread, signed with this PC's device key.
    ///
    /// No-op while one is already in flight: a second build of the same buffer would answer the
    /// same sha (the builder is content-addressed and an unchanged source is a cache hit), and a
    /// build of a DIFFERENT buffer racing the first would land whichever finished last.
    ///
    /// ⚠ **It dispatches NO run.** The sha arrives in [`Self::poll`] and unblocks the Run button;
    /// the operator presses it. That is the design's strictly-sequential ordering — and expressing
    /// it as a state change rather than a chained call is also what keeps a FAILED build from
    /// launching anything, which a chained call would have to remember not to do.
    pub(crate) fn start_rust_build(&mut self) {
        let spawn = strategy_files::spawn_backtesthub_build;
        let (address, key) = (self.backend.address().to_string(), self.hub_keys.clone());
        // The toolbar's SDK pick: a Build compiles against what a Run will.
        let sdk = self.picked_sdk();
        self.dispatch_rust_build(move |file, source| spawn(address, key, file, source, sdk));
    }

    /// [`Self::start_rust_build`] over any `spawn` of `(file, source)` — the seam a test drives
    /// the real dispatch through with a receiver it holds the sender of, since the real spawn dials the
    /// backtesthub.
    pub(super) fn dispatch_rust_build(
        &mut self,
        spawn: impl FnOnce(String, String) -> Receiver<Result<String, String>>,
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
        let rx = spawn(self.rust_file.clone(), sent.clone());
        self.build_rx = Some((sent, rx));
    }

    /// Ask the backtest server for its `strategies/` folder with each file's states, on a worker
    /// (folded by [`Self::poll`]). The dial is the OBSERVE one the named roster takes: both verbs
    /// are `VerbScope::Read`, which the server admits for a device listed at `observe`.
    pub(crate) fn start_strategy_files_fetch(&mut self) {
        if self.strategy_files_rx.is_some() {
            return;
        }
        self.strategy_files_rx = Some(strategy_files::spawn_backtesthub_strategy_files(
            self.backend.address().to_string(),
            self.hub_keys.clone(),
        ));
    }

    /// Save the editor buffer to its server file ([`Self::save_target`]) on a worker; the answer
    /// (the sha256 written and the file's states) is folded by [`Self::poll`], which refreshes the
    /// list. Refused while [`Self::save_blocked_reason`] says so. Signed with this PC's device key:
    /// a save writes code, so the server takes it from a Control connection only.
    pub(crate) fn start_save_strategy_file(&mut self) {
        if self.save_blocked_reason().is_some() {
            return;
        }
        let file = self.save_target().trim().to_string();
        let text = self.editor.source.clone();
        self.save_file_last = None;
        let rx = strategy_files::spawn_backtesthub_strategy_file_save(
            self.backend.address().to_string(),
            self.hub_keys.clone(),
            file.clone(),
            text.clone(),
        );
        self.save_file_rx = Some(((file, text), rx));
    }

    /// Open `file` of the served list: its text arrives on a worker and [`Self::poll`] puts it in
    /// the editor. Refused while [`Self::open_blocked_reason`] says so.
    pub(crate) fn start_open_strategy_file(&mut self, file: String) {
        if self.open_blocked_reason().is_some() {
            return;
        }
        self.open_last = None;
        let rx = strategy_files::spawn_backtesthub_strategy_file_read(
            self.backend.address().to_string(),
            self.hub_keys.clone(),
            file.clone(),
        );
        self.open_rx = Some((file, rx));
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
        self.center = StudioCenterView::Backtest;
        // ⚠ **THE GUARD LIVES HERE, not only at the button.** Fix-round-1 finding: the toolbar's
        // `can_run`/keyboard-shortcut checks were the ONLY thing consulting
        // `run_blocked_reason` — a caller that reaches this function any other way (QA autorun,
        // a test, a future call site) bypassed it entirely, building a `StrategySpec::StrategyFile`
        // whose `rust_sha.unwrap_or_default()` is an EMPTY STRING and dispatching it anyway.
        // Checking it INSIDE the dispatcher protects every caller at once rather than every
        // caller that remembers to ask first.
        if let Some(reason) = self.run_blocked_reason() {
            self.last = Some(Err(RunError::Data(reason.to_string())));
            return;
        }
        // ⚠ The split-plane tick refusal that stood here is GONE with the `Local` backend it
        // guarded: it fired only for a local tick replay over a remote store, and there is no
        // local path left to fire for. See `StudioRunBackend`'s `Default`.
        // The Named backend picks its strategy off the SERVER's roster, which is a different set
        // from this binary's compiled-in one — see `named_spec`. Every other backend runs what the
        // Strategy pane says, unchanged.
        let spec = if matches!(self.backend, StudioRunBackend::Named { .. }) {
            self.named_spec()
        } else {
            self.current_spec()
        };
        // Both arms dial the backtesthub and return the SAME `Receiver<StudioRunViewOutcome>`, so
        // `run_rx`/`poll` are one path. (This named a third, in-process `Local` arm and called the
        // target a vike-datahub server; both were wrong by 2026-09-20 - see 0078.)
        // A new run is on screen, not a scan point opened before it.
        self.sweep_open = None;
        self.point_rx = None;
        let rx = match &self.backend {
            // This arm dials at the Write scope (`crate::backend::backtesthub`'s `connect`), the Named arm
            // at Read; both sign with this PC's device key, and the server's list decides.
            StudioRunBackend::Source { address } => {
                crate::backend::backtesthub::spawn_backtesthub_run(
                    address.clone(),
                    self.hub_keys.clone(),
                    spec,
                    slice,
                    self.run_engine(),
                    self.recompute,
                )
            }
            // The NAMED run — one strategy this daemon already holds, one bounded window, one pass.
            // Same `Receiver<StudioRunViewOutcome>` as its sibling (the whole answer summarized in this
            // process, its trades in hand); every refusal (a script, a sweep, a symbol list, an open window, an
            // over-wide one) arrives on it as an `Err(RunError)` naming the bound it hit.
            StudioRunBackend::Named { address } => named_run::spawn_backtesthub_named_run(
                address.clone(),
                self.hub_keys.clone(),
                spec,
                slice,
            ),
        };
        self.run_rx = Some(rx);
        self.running = true;
        self.run_origin.send(self.results_code());
    }

    /// Which strategy [`Self::grid`]'s rows belong to: the picked strategy file's path, else the editor
    /// buffer of its language. A buffer keeps one set of rows per language rather than per the file it is
    /// saved as, since that name is typed a letter at a time.
    pub(crate) fn param_key(&self) -> String {
        match self.strategy_source {
            StudioStrategySource::File => self.picked_file.trim().to_string(),
            StudioStrategySource::Rhai => "editor: Rhai".to_string(),
            StudioStrategySource::Rust => "editor: Rust".to_string(),
        }
    }

    /// Put [`Self::grid`] away under its strategy and bring back the rows of the strategy now on screen,
    /// when that changed. [`Self::poll`] calls it every frame, so a Run never takes another strategy's
    /// values.
    pub(super) fn sync_grid_key(&mut self) {
        let key = self.param_key();
        if key == self.grid_key {
            return;
        }
        let next = self.param_grids.remove(&key).unwrap_or_default();
        let rows = std::mem::replace(&mut self.grid, next);
        self.param_grids.insert(std::mem::replace(&mut self.grid_key, key), rows);
        self.grid_seeded = None;
        self.grid_dropped.clear();
        self.seed_file_rx = None;
        self.seed_failed = None;
    }

    /// Re-cut [`Self::grid`] to the strategy's own parameters
    /// (`crate::panes::param_scan::declared_params`), keeping each row's value, tick and range by name:
    /// a Rhai script's `param()`s or a Rust buffer's header, from the editor, each time the buffer
    /// changed; a strategy file's header, from its text, read from the backtest server (a Read dial,
    /// folded by [`Self::poll`]) once per picked file. A text that does not read keeps the rows.
    pub(crate) fn seed_sweep_grid(&mut self) {
        if self.strategy_source == StudioStrategySource::File {
            let file = self.picked_file.trim().to_string();
            if file.is_empty() || self.grid_seeded.as_deref() == Some(file.as_str()) {
                return;
            }
            self.seed_failed = None;
            self.seed_file_rx = Some(strategy_files::spawn_backtesthub_strategy_file_read(
                self.backend.address().to_string(),
                self.hub_keys.clone(),
                file.clone(),
            ));
            self.grid_seeded = Some(file);
            return;
        }
        if self.grid_seeded.as_deref() == Some(self.editor.source.as_str()) {
            return;
        }
        let text = self.editor.source.clone();
        self.recut_grid(&text);
        self.grid_seeded = Some(text);
    }

    /// [`Self::grid`] re-cut to the parameters `text` declares, when it declares any reading.
    pub(super) fn recut_grid(&mut self, text: &str) {
        if let Some(declared) = declared_params(text) {
            self.grid = rows_for(declared, &self.grid, &mut self.grid_dropped);
        }
    }

    /// The Rhai buffer with its top-level numeric `const`s turned into `param()`s of the same names and
    /// numbers (`crate::panes::param_scan::constants_as_params`), so a scan can vary them: an EDIT the
    /// user sees (the unsaved chip shows it), refused over a Rust buffer as every Rhai writer is.
    pub(super) fn make_constants_params(&mut self) {
        if self.rhai_writer_blocked_reason().is_some() {
            return;
        }
        self.editor.source = constants_as_params(&self.editor.source);
    }

    /// The rows a scan varies, as `(name, text)`: every ticked strategy parameter and every ticked cost
    /// (the "Advanced" disclosure), over its range.
    pub(super) fn ticked_rows(&self) -> Vec<(String, String)> {
        let rows = self.grid.iter().chain(&self.conditions.costs);
        rows.filter(|r| r.scan).map(|r| (r.name.clone(), r.range.clone())).collect()
    }

    /// The values a Rhai script's scan and walk-forward hold its other rows at: every value set on an
    /// unticked parameter or cost. A script's request carries them as its parameters too
    /// ([`Self::current_entry`]); as axes they also reach a server that predates those, which runs the bare
    /// script. A strategy file's ride in its params alone, and this is empty.
    pub(super) fn held_rows(&self) -> Vec<(String, String)> {
        if self.strategy_source != StudioStrategySource::Rhai {
            return Vec::new();
        }
        let rows = self.grid.iter().chain(&self.conditions.costs);
        let held = rows.filter(|r| !r.scan && !r.value.trim().is_empty());
        held.map(|r| (r.name.clone(), r.value.clone())).collect()
    }

    /// Spend the [`StudioAutorun`] hook — the FIRST-FRAME half of `ui()`, lifted out of it so the
    /// decision can be tested without rasterizing a shell (`ui()` calls this and nothing else
    /// consults the flag).
    ///
    /// Consulted exactly once per session: the flag is disarmed FIRST and unconditionally (a
    /// `replace`, not a read), so an arm that cannot dispatch — because the picker has nothing
    /// selectable yet — still leaves it spent. That is deliberate rather than incidental: a flag
    /// left armed fires a surprise run minutes later, the moment `picker.refresh` auto-selects row
    /// 0 as data appears, which is the review finding the original boolean hook records.
    pub(super) fn take_qa_autorun(&mut self) {
        match std::mem::replace(&mut self.qa_autorun, StudioAutorun::Off) {
            StudioAutorun::Off => {}
            StudioAutorun::Run => {
                if !self.running && self.picker.selected().is_some() {
                    self.start_run();
                }
            }
            // The sweep autorun is the ▶ "Seed grid from params" button followed by ▶ "Run Sweep",
            // in that order, because `start_sweep` returns early on an empty grid and a fresh
            // workspace has one. `qa_sweep_ladder` then widens each seeded default so the ranked
            // table has rows to rank — see its doc for why a one-combination sweep is the same
            // empty-frame failure this hook exists to remove.
            StudioAutorun::Sweep => {
                if !self.running && self.sweep_rx.is_none() && self.picker.selected().is_some() {
                    // ⚠ THE SOURCE SWAP IS LOAD-BEARING, not a preference. A sweep varies what
                    // `param()` declared, and `crates/vike-studio/src/panes/editor/mod.rs`'s
                    // `DEFAULT_SCRIPT` declares its lookbacks with `const` — so seeding a grid
                    // from the shipped default yields an EMPTY grid, `start_sweep` returns early,
                    // and the capture renders the empty form this hook exists to remove. The
                    // swapped-in script is the SAME strategy with the same defaults; its own doc
                    // carries the argument. Persistence is already off for this arm (see
                    // `new_with_qa`'s `qa_workspace_readonly`), so the user's file is untouched.
                    self.strategy_source = StudioStrategySource::Rhai;
                    self.editor.source = crate::panes::editor::SWEEP_CAPTURE_SCRIPT.to_string();
                    // Re-baseline, or the editor shows an amber unsaved-changes dot in the frame.
                    self.saved_source = self.editor.source.clone();
                    self.grid_seeded = None;
                    self.seed_sweep_grid();
                    for row in &mut self.grid {
                        row.range = qa_sweep_ladder(&row.default);
                        row.scan = true;
                    }
                    self.start_sweep();
                }
            }
        }
    }

    /// Kick off a parameter sweep on a worker thread: no-op if a run or sweep is already in
    /// flight, no slice is selected, or every grid row is empty/unparseable.
    pub(super) fn start_sweep(&mut self) {
        if self.running || self.sweep_rx.is_some() {
            return;
        }
        let Some(slice) = self.picker.selected() else { return };
        let spec = self.current_spec();
        // The axes are TYPED here, against the very params table the sweep points land on, and
        // cross the wire as typed — see `sweep_axes`. A scan needs a ticked row with a value; the
        // values a Rhai script holds its other rows at join it (`held_rows`).
        if sweep_axes(&self.ticked_rows(), None).is_empty() {
            return;
        }
        let rows: Vec<(String, String)> =
            self.ticked_rows().into_iter().chain(self.held_rows()).collect();
        let grid = sweep_axes(&rows, spec.base_params());
        self.center = StudioCenterView::Backtest;
        // Whatever happens below is this scan's answer: the pane scrolls it into view.
        self.sweep_reveal = true;
        self.sweep_took = None;
        // The button's refusal, asked again inside the dispatcher (see the run guard below).
        if let Some(why) = self.grid_refusal(points_of(&grid)) {
            self.sweep_last = Some(Err(RunError::Data(why.to_string())));
            return;
        }
        // ⚠ **Fix-round-1 CRITICAL finding.** This dispatcher never consulted
        // `run_blocked_reason` — only the toolbar's Run button did — so a Strategy file spec with no sha
        // reached `spawn_backtesthub_sweep` with `rust_sha.unwrap_or_default()`'s EMPTY STRING as if
        // it named a real artifact. Checked HERE, inside the dispatcher, so no caller (a future
        // button, a keyboard shortcut, a test) can bypass it the way the button-only check could.
        if let Some(reason) = self.run_blocked_reason() {
            self.sweep_last = Some(Err(RunError::Data(reason.to_string())));
            return;
        }
        // ⚠ THE NAMED BACKEND HAS NO SEARCH, and this refusal is the bound rather than a missing
        // feature. `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 3 makes the
        // single-point shape the LARGEST of that verb's bounds, and a structural one: the request
        // type has no field a grid could occupy, so `expand_paramscan_overrides`' product over
        // client-supplied arrays (capped, but each point a backtest) is unreachable. Growing a search
        // dimension is that record's FIRST reopener, not a feature request.
        if let Some(message) =
            named_run::named_backend_search_refusal(&self.backend, "run a param scan")
        {
            self.sweep_last = Some(Err(RunError::Data(message)));
            return;
        }
        let rx = match &self.backend {
            StudioRunBackend::Source { address } => {
                crate::backend::backtesthub::spawn_backtesthub_sweep(
                    address.clone(),
                    self.hub_keys.clone(),
                    spec,
                    slice,
                    grid,
                    self.run_engine(),
                    self.recompute,
                )
            }
            // Unreachable: the refusal above returns first. Spelled out rather than left to a
            // catch-all so a third backend cannot land here silently.
            StudioRunBackend::Named { .. } => return,
        };
        // The new scan's best point is shown, not a point opened from the last one.
        self.sweep_open = None;
        self.point_rx = None;
        self.sweep_rx = Some(rx);
        self.scan_origin.send(self.results_code());
        self.sweep_started = Some(std::time::Instant::now());
    }

    /// What the results pane asked for ([`StudioResultsAction`]): a page of the trades of the run on screen,
    /// `of` (its run and point), whose trades are in hand when `in_hand`; or another point of the scan.
    pub(super) fn on_results_action(
        &mut self,
        action: StudioResultsAction,
        of: (String, Option<usize>),
        in_hand: bool,
    ) {
        match action {
            StudioResultsAction::ShowTradesPage(page) => {
                let asked = self.trades.of == of && self.trades.page == page;
                if asked && (in_hand || self.trades.shown.is_some() || self.trades_rx.is_some()) {
                    return;
                }
                self.trades = StudioResultsTradesPager { of: of.clone(), page, shown: None };
                self.trades_rx = None;
                // Trades in hand (a named run) are cut by the pane; only a backtesthub run's are fetched.
                if let (false, StudioRunBackend::Source { address }) = (in_hand, &self.backend) {
                    self.trades_rx =
                        Some(crate::backend::backtesthub::spawn_backtesthub_trades_page(
                            address.clone(),
                            self.hub_keys.clone(),
                            of.0,
                            of.1,
                            page,
                        ));
                }
            }
            StudioResultsAction::OpenScanPoint(point) => {
                let Some(Ok(sweep)) = &self.sweep_last else { return };
                if point == sweep.best_index {
                    self.sweep_open = None;
                    self.point_rx = None;
                    return;
                }
                if let StudioRunBackend::Source { address } = &self.backend {
                    self.point_rx = Some(crate::backend::backtesthub::spawn_backtesthub_point(
                        address.clone(),
                        self.hub_keys.clone(),
                        sweep.run.clone(),
                        point,
                    ));
                }
            }
        }
    }

    /// The walk-forward the Sweep pane asks for: [`Self::walkforward_splits`] windows, each FIXED or — with
    /// [`Self::walkforward_search`] on and a grid that parses — each SEARCHING the sweep grid on its own
    /// training half (ranked by the server's default metric), typed by [`sweep_axes`], the same
    /// reading a sweep sends. An empty grid walks fixed: there is nothing to search.
    ///
    /// A Rhai script's held values ([`Self::held_rows`]) join the search, and without one they are a
    /// search of ONE point: each window runs that point, which is the fixed walk at those values (on a
    /// server predating a Rhai buffer's parameters, the one way they reach it).
    fn walk_request(&self, base: Option<&Params>) -> WalkforwardWire {
        let rows: Vec<(String, String)> = match self.walkforward_search {
            true => self.ticked_rows().into_iter().chain(self.held_rows()).collect(),
            false => self.held_rows(),
        };
        let grid = sweep_axes(&rows, base);
        if grid.is_empty() {
            return WalkforwardWire::fixed(self.walkforward_splits);
        }
        WalkforwardWire::searching(
            self.walkforward_splits,
            WalkforwardWindowSearchWire {
                // The server's `WalkforwardWindowSearch::from_str_ci` token for a per-window grid sweep.
                method: "paramscan".to_string(),
                grid: ParamscanWire { axes: grid },
                rank_by: None,
            },
        )
    }

    /// Kick off a walk-forward validation run on a worker thread — [`Self::walk_request`]'s split
    /// count and search: no-op if no slice is selected or one is already in flight.
    pub(super) fn start_walkforward(&mut self) {
        if self.walkforward_rx.is_some() {
            return;
        }
        let Some(slice) = self.picker.selected() else { return };
        self.center = StudioCenterView::Backtest;
        // ⚠ **Fix-round-1 CRITICAL finding — see `start_sweep`'s twin comment.** Checked inside the
        // dispatcher, not only at a button, so no caller can reach `spawn_backtesthub_walkforward` with
        // an empty-sha Strategy file spec by going around a UI check that happened to exist.
        if let Some(reason) = self.run_blocked_reason() {
            self.walkforward_last = Some(Err(RunError::Data(reason.to_string())));
            return;
        }
        // Same bound as the sweep above: a walk-forward carries a SPLIT COUNT, which is a cost term
        // the named run's request type has no field for. See `named_backend_search_refusal`.
        if let Some(message) =
            named_run::named_backend_search_refusal(&self.backend, "walk-forward")
        {
            self.walkforward_last = Some(Err(RunError::Data(message)));
            return;
        }
        if let Some(why) = self.walkforward_grid_refusal() {
            self.walkforward_last = Some(Err(RunError::Data(why.to_string())));
            return;
        }
        let spec = self.current_spec();
        let walk = self.walk_request(spec.base_params());
        let rx = match &self.backend {
            StudioRunBackend::Source { address } => {
                crate::backend::backtesthub::spawn_backtesthub_walkforward(
                    address.clone(),
                    self.hub_keys.clone(),
                    spec,
                    slice,
                    walk,
                    self.run_engine(),
                    self.recompute,
                )
            }
            // Unreachable: the refusal above returns first — see the sweep's twin.
            StudioRunBackend::Named { .. } => return,
        };
        self.walkforward_rx = Some(rx);
        self.walkforward_origin.send_with_values(self.results_code(), self.sent_values());
    }

    /// The window a study run is ASKED about: the picked series' From/To window, or the whole store
    /// when no series is picked. A From or To that is no window is refused in the picker's words, as
    /// a Run refuses it: read as "nothing picked" it widened the study to the whole store.
    ///
    /// ⚠ Not a gate. A study reads whatever series it likes through its own context verbs, so the
    /// window is a fact the run RECORDS (the manifest's `vike_studio_core::study_run::StudyWindow`)
    /// and the default its read verbs take — never a restriction the Studio imposes. Requiring a
    /// slice before a study could run would be an invented coupling: the backtest path needs one
    /// because it REPLAYS that exact series, and a study does not.
    pub(super) fn study_window(&self) -> Result<TimestampRange, &'static str> {
        match self.picker.selected_row() {
            Some(_) => self.picker.range(),
            None => Ok(TimestampRange::all()),
        }
    }

    /// That same window as the sentence the Research pane shows, so the operator can see which of
    /// the answers above they are about to get.
    pub(super) fn study_window_label(&self) -> String {
        match (self.study_window(), self.picker.selected()) {
            (Err(why), _) => why.to_string(),
            (Ok(_), Some(s)) => format!("{} · {} · {}", s.venue, s.symbol(), s.interval),
            (Ok(_), None) => "the whole store (no data slice picked)".to_string(),
        }
    }

    /// Whether the backend dials the backtest server at the Write scope: the Source backend. The
    /// Named one dials Read only (decision 0064), so it lists studies and runs none.
    fn compute_backend(&self) -> bool {
        matches!(self.backend, StudioRunBackend::Source { .. })
    }

    /// Ask for the Research pane's lists unless it holds lists of this kind of backend already: the
    /// first frame the tool is open, and after a switch between Source and Named. An edited address
    /// is asked by Rescan, so a keystroke in the Backend field is not a dial.
    pub(super) fn ensure_research_listed(&mut self) {
        if self.research.needs_listing(self.compute_backend()) {
            self.start_research_listing();
        }
    }

    /// Ask the backend's server for its studies (a Read dial, either backend) and page 0 of its
    /// runs (a Write dial, the Source backend only), each on a worker folded by [`Self::poll`].
    pub(super) fn start_research_listing(&mut self) {
        let address = self.backend.address().to_string();
        let compute = self.compute_backend();
        self.research.begin_listing(&address, compute);
        self.research_studies_rx =
            Some(studies::spawn_backtesthub_studies(address, self.hub_keys.clone()));
        self.research_runs_rx = None;
        self.start_research_runs_page(0);
    }

    /// Ask for page `page` of the server's runs (0 is the newest), on a worker; a no-op on the Named
    /// backend, which may not read them.
    pub(super) fn start_research_runs_page(&mut self, page: usize) {
        if !self.compute_backend() {
            return;
        }
        self.research.fetching_runs(page);
        self.research_runs_rx = Some(studies::spawn_backtesthub_runs_page(
            self.backend.address().to_string(),
            self.hub_keys.clone(),
            page,
        ));
    }

    /// Run the selected study ON THE SERVER, on a worker: no-op if one is already in flight, the
    /// backend is Named (no Write dial), or no study is selected.
    ///
    /// The Research pane's twin of [`Self::start_run`], and deliberately the same shape — a
    /// `spawn_*` returning a `Receiver` that [`Self::poll`] folds. The request names the study, its
    /// tier and its recipe FILE; the server reads the recipe, picks the tier's runner and saves the
    /// run, so this function knows nothing of either tier. Signed with this PC's device key.
    pub fn start_study(&mut self) {
        if self.study_rx.is_some() || !self.compute_backend() {
            return;
        }
        let window = match self.study_window() {
            Ok(window) => window,
            Err(why) => {
                self.center = StudioCenterView::Study;
                self.study_last = Some(Err(why.to_string()));
                return;
            }
        };
        let Some(request) = self.research.request(window) else { return };
        self.center = StudioCenterView::Study;
        self.study_last = None;
        self.research.clear_artifact();
        self.study_rx = Some(studies::spawn_backtesthub_study_run(
            self.backend.address().to_string(),
            self.hub_keys.clone(),
            request,
        ));
    }

    /// Read the first page of `file`, an artifact of the last study run, on a worker; a no-op when
    /// that run was not saved (there is nothing on the server to read).
    pub(super) fn start_study_artifact_read(&mut self, file: String) {
        let Some(Ok(WireStudyReport { run_id: Some(run_id), .. })) = &self.study_last else {
            return;
        };
        let run_id = run_id.clone();
        self.research.reading_artifact(&file);
        self.study_artifact_rx = Some(studies::spawn_backtesthub_run_artifact(
            self.backend.address().to_string(),
            self.hub_keys.clone(),
            run_id,
            file,
        ));
    }

    /// Ask the backend's server for its strategy SDKs (a Read dial), folded by [`Self::poll`] into the
    /// SDK source view; a no-op while one is on its way.
    pub(super) fn start_sdk_listing(&mut self) {
        if self.sdk_listing_rx.is_some() {
            return;
        }
        self.sdk_listing_rx = Some(sdk_source::spawn_backtesthub_sdks(
            self.backend.address().to_string(),
            self.hub_keys.clone(),
        ));
    }

    /// What the SDK source view asked for: list the SDKs again, show another, read a folder of its tree
    /// (one at a time), or open a file in the central panel's code view (a newer click replaces an
    /// older one on its way).
    pub(super) fn on_sdk_source_action(&mut self, action: StudioSdkSourceAction) {
        let sdk = self.sdk_source.shown().unwrap_or_default().to_string();
        let address = self.backend.address().to_string();
        match action {
            StudioSdkSourceAction::Refresh => self.start_sdk_listing(),
            StudioSdkSourceAction::Show(name) => self.sdk_source.show(Some(name), true),
            StudioSdkSourceAction::ReadFolder(path) => {
                if self.sdk_folder_rx.is_none() {
                    let rx = sdk_source::spawn_backtesthub_sdk_read(
                        address,
                        self.hub_keys.clone(),
                        sdk.clone(),
                        path.clone(),
                    );
                    self.sdk_folder_rx = Some(((sdk, path), rx));
                }
            }
            StudioSdkSourceAction::OpenFile(path) => {
                self.center = StudioCenterView::SdkFile;
                self.sdk_file_rx = Some(sdk_source::spawn_backtesthub_sdk_read(
                    address,
                    self.hub_keys.clone(),
                    sdk,
                    path,
                ));
            }
        }
    }

    /// Kick off a chat -> strategy round trip on a worker thread: no-op if one is already running,
    /// no data slice is selected (the AI needs a (venue,symbol,interval) to backtest against — the
    /// same slice the Run toolbar uses), the input box is empty, the selected provider has no key,
    /// or the backend is Named ([`COPILOT_NEEDS_COMPUTE_KEY`]: no local fallback).
    ///
    /// Each candidate the copilot writes is backtested ON THE SERVER, one `RunSlice` signed with
    /// this PC's device key exactly as a Run is, so it is charged as a Run of the same bars is.
    pub(super) fn start_chat_send(&mut self) {
        if self.chat.running || self.chat_rx.is_some() {
            return;
        }
        if self.chat.input.trim().is_empty() || !self.chat.has_key() {
            return;
        }
        let StudioRunBackend::Source { address } = &self.backend else { return };
        let backtest =
            crate::backend::backtesthub::copilot_backtester(address.clone(), self.hub_keys.clone());
        let Some(slice) = self.picker.selected() else { return };
        let store = self.store.clone();
        let symbol = slice.symbol().to_string();
        // The AI copilot's cross-session memory (`vike_ai::ledger`), in the Studio's state directory
        // beside `studio_strategies.json`. The Studio resolves the path because the library
        // deliberately does not (see `StudioChatPane::send`).
        let ledger = vike_ai::LedgerPaths::under(&self.state_dir);
        self.chat_rx = Some(self.chat.send(
            store,
            slice.venue,
            symbol,
            slice.interval,
            Some(ledger),
            backtest,
        ));
    }

    pub(super) fn backend_kind(&self) -> StudioRunBackendKind {
        match self.backend {
            StudioRunBackend::Source { .. } => StudioRunBackendKind::Source,
            StudioRunBackend::Named { .. } => StudioRunBackendKind::Named,
        }
    }

    /// Switch backends, KEEPING the address the operator typed — both dial the same compute
    /// daemon, so there is nothing to reset it for — and dropping a roster when the switch lands
    /// on Named, because a roster belongs to the daemon that answered it.
    pub(super) fn switch_backend(&mut self, to: StudioRunBackendKind) {
        if self.backend_kind() == to {
            return;
        }
        let address = self.backend.address().to_string();
        self.backend = match to {
            StudioRunBackendKind::Source => StudioRunBackend::Source { address },
            StudioRunBackendKind::Named => StudioRunBackend::Named { address },
        };
        if to == StudioRunBackendKind::Named {
            self.named_roster = None;
            self.named_roster_rx = None;
        }
    }

    /// **Ask the backtest server whether it answers** — a Read dial with this PC's device key and a
    /// `Ping`, on a worker — for the toolbar's "connected". The Studio dials nothing by itself to ask
    /// it: the composition root asks once the device key is seeded, and the toolbar's Check and an
    /// edited address ask again. No-op while a check is in flight.
    pub fn check_server(&mut self) {
        if self.server_check_rx.is_some() {
            return;
        }
        self.server_check_rx = Some(crate::backend::backtesthub::spawn_backtesthub_check(
            self.backend.address().to_string(),
            self.hub_keys.clone(),
        ));
    }

    /// The toolbar's name for the backtest server: the desktop's server name ([`Self::desktop_server`])
    /// when its address names the same box as the backtest address, else the address itself. A box's
    /// own host names it whatever the port (the desktop's daemon and the backtest server listen on two).
    /// A LOOPBACK host does not: two `127.0.0.1` tunnels may lead to two boxes, so there the two
    /// ports must be one, or both among the ports the recorded tunnel forwards
    /// ([`Self::desktop_tunnel_ports`]: one ssh forwards the tradehub's, the datahub's and the
    /// backtest server's, so all three lead to its one box).
    pub(crate) fn server_name(&self) -> String {
        let address = self.backend.address();
        fn host_port(address: &str) -> (&str, &str) {
            let (host, port) = address.trim().rsplit_once(':').unwrap_or((address, ""));
            (host.trim_matches(['[', ']']), port)
        }
        fn loopback(host: &str) -> bool {
            host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
        }
        let tunnelled = |port: &str| {
            port.parse::<u16>().is_ok_and(|port| self.desktop_tunnel_ports.contains(&port))
        };
        let same_box = |at: &str| {
            let ((host, port), (here, here_port)) = (host_port(at), host_port(address));
            match (loopback(host), loopback(here)) {
                (true, true) => port == here_port || (tunnelled(port) && tunnelled(here_port)),
                (false, false) => host == here,
                _ => false,
            }
        };
        match &self.desktop_server {
            Some((name, at)) if !name.trim().is_empty() && same_box(at) => name.clone(),
            _ => address.to_string(),
        }
    }
}

/// How many points `axes` expand to: the product of their sizes (`scan_total`).
fn points_of(axes: &[(String, Vec<ParamValue>)]) -> usize {
    scan_total(&axes.iter().map(|(_, values)| values.len()).collect::<Vec<_>>())
}

/// The sweep grid's `(name, text)` rows as the TYPED axes that cross the wire — every row through
/// `crate::panes::param_scan::axis_values`: a range (`5..30 step 5`), else
/// `vike_config::params_text::parse_axis`, the ONE reading of a list (the CLI's
/// `--set paramscan.<k>=…` calls it too), typed against `base` (the params table the sweep points
/// land on: `vike_studio_core::StrategySpec::base_params`). A row that reads to no value is no axis.
///
/// So a knob read with `as_integer` (`rungs`, `exit_delay_ms`, `cooldown_ms`, `time_limit_ms`) and
/// typed as `4` in the editor stays an integer under the sweep instead of reading as ABSENT — the
/// defect `docs/decisions/0083-the-runtime-plugin-join-lands.md` recorded — and a knob the base does
/// not hold (every Rhai axis, a strategy file's) keeps the text's own kind, so `5, 10` sweeps integers.
pub(crate) fn sweep_axes(
    grid: &[(String, String)],
    base: Option<&Params>,
) -> Vec<(String, Vec<ParamValue>)> {
    grid.iter()
        .filter_map(|(name, text)| {
            let values = axis_values(text, base.and_then(|b| b.get(name)));
            (!values.is_empty()).then(|| (name.clone(), values))
        })
        .collect()
}
