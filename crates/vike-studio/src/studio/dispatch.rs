//! `StudioState`'s dispatchers: the per-button disabled reasons, and the methods that start a
//! worker (Run, Sweep, Walk-Forward, Plugin Build, Study, the AI copilot's Send) or compute a pane's
//! answer on demand (the Indicators preview), plus the backend selector's switch.
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). The receivers these start are folded back in by `poll`, in
//! `workers.rs`. `use super::*` brings in the parent module's imports and items, so nothing about
//! resolution changes.

use super::*;

impl StudioState {
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
    pub(super) fn sweep_disabled_reason(&self, grid_ok: bool) -> Option<&'static str> {
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
    pub(super) fn walk_forward_disabled_reason(&self) -> Option<&'static str> {
        if self.wf_rx.is_some() {
            return Some(WALK_FORWARD_BUSY);
        }
        if self.picker.selected().is_none() {
            return Some(PICK_A_SLICE);
        }
        None
    }

    /// Why Send is disabled, or `None` — its button's own condition, one reason at a time.
    pub(super) fn send_disabled_reason(&self) -> Option<&'static str> {
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

    /// Compute the Indicators pane's selected indicator over the currently-picked data slice's
    /// bars (loaded synchronously — the preview compute is O(bars) and cheap; unlike Run, it does
    /// not need a worker thread). A no-slice / load-failure surfaces as the pane's own error.
    ///
    /// ⚠ This is the one store read the Studio still performs ON the paint thread since the ⟳
    /// Refresh walk moved off it (`crate::backend::catalog`). The "cheap" above is true of the COMPUTE and
    /// of a LOCAL store; over a `RemoteHistStore` the `load_bars` below is the same
    /// connect-per-read hazard that module describes, and it stays synchronous here because it is
    /// out of that change's scope, not because it is exempt — moving it means the Indicators pane
    /// growing a receiver and a loading state of its own, the way `spawn_catalog_refresh` did.
    pub(super) fn compute_indicator_preview(&mut self) {
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
        self.dispatch_plugin_build(crate::backend::plugin_build::spawn_build);
    }

    /// [`Self::start_plugin_build`] over any `spawn` with [`crate::backend::plugin_build::spawn_build`]'s
    /// shape — the seam a test drives the real dispatch through with a receiver it holds the sender
    /// of, since the real spawn dials a builder service.
    pub(super) fn dispatch_plugin_build(
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
            Backend::Remote { addr } => crate::backend::remote::spawn_run_remote(
                addr.clone(),
                self.compute_key.clone(),
                spec,
                slice,
            ),
            // The NAMED run — one strategy this daemon already holds, one bounded window, one pass.
            // Same `Receiver<RunOutcome>` as both siblings, so `poll` and the results pane are
            // unchanged; every refusal (a script, a sweep, a symbol list, an open window, an
            // over-wide one) arrives on it as an `Err(RunError)` naming the bound it hit.
            Backend::Named { addr } => crate::backend::remote::spawn_named_run_remote(
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
    pub(super) fn take_qa_autorun(&mut self) {
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
                    // `param()` declared, and `crates/vike-studio/src/panes/editor.rs`'s
                    // `DEFAULT_SCRIPT` declares its lookbacks with `const` — so seeding a grid
                    // from the shipped default yields an EMPTY grid, `start_sweep` returns early,
                    // and the capture renders the empty form this hook exists to remove. The
                    // swapped-in script is the SAME strategy with the same defaults; its own doc
                    // carries the argument. Persistence is already off for this arm (see
                    // `new_with_qa`'s `qa_workspace_readonly`), so the user's file is untouched.
                    self.strategy_source = StrategySource::Rhai;
                    self.editor.source = crate::panes::editor::SWEEP_CAPTURE_SCRIPT.to_string();
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
    pub(super) fn start_sweep(&mut self) {
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
        if let Some(msg) =
            crate::backend::remote::named_backend_search_refusal(&self.backend, "sweep")
        {
            self.sweep_last = Some(Err(RunError::Data(msg)));
            return;
        }
        let spec = self.current_spec();
        let rx = match &self.backend {
            Backend::Remote { addr } => crate::backend::remote::spawn_sweep_remote(
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
    pub(super) fn start_walkforward(&mut self) {
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
            crate::backend::remote::named_backend_search_refusal(&self.backend, "walk-forward")
        {
            self.wf_last = Some(Err(RunError::Data(msg)));
            return;
        }
        let spec = self.current_spec();
        let rx = match &self.backend {
            Backend::Remote { addr } => crate::backend::remote::spawn_walkforward_remote(
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
    pub(super) fn study_window_label(&self) -> String {
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
    pub(super) fn start_chat_send(&mut self) {
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

    pub(super) fn backend_kind(&self) -> BackendKind {
        match self.backend {
            Backend::Remote { .. } => BackendKind::Remote,
            Backend::Named { .. } => BackendKind::Named,
        }
    }

    /// Switch backends, KEEPING the address the operator typed — both dial the same compute
    /// daemon, so there is nothing to reset it for — and dropping a roster when the switch lands
    /// on Named, because a roster belongs to the daemon that answered it.
    pub(super) fn switch_backend(&mut self, to: BackendKind) {
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
}
