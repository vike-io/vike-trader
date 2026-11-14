//! `StudioState`'s in-flight worker lifecycle: the ⟳ Refresh catalog walk's spawn and latch, "is
//! anything running" (`any_running`), Cancel (`cancel`), and the per-frame `poll` that folds each
//! worker's outcome in.
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). The `Delivery` / `poll_worker` / `poll_tagged_worker` helpers `poll`
//! is written against stay in the parent module. `use super::*` brings in the parent module's
//! imports and items, so nothing about resolution changes.

use super::*;

impl StudioState {
    /// ⟳ Refresh: re-read the store's catalog ON A WORKER THREAD and fold the answer in on a later
    /// frame (`poll()`), instead of walking it inline between two frames.
    ///
    /// The click used to run four full catalog walks in the frame — three `list_series` calls from
    /// `SlicePicker::refresh` plus `DataBrowserPane::refresh`'s `inventory()` — which on the
    /// `HistoryStoreDatahub` this shell holds whenever a datahub address resolves is four fresh TCP
    /// connects on the egui paint thread. `crate::backend::catalog`'s module doc carries the measurement and
    /// the `vike-desktop` precedent this copies (`refresh_stored` + `stored_loading`).
    ///
    /// A second click while a walk is in flight is a NO-OP: `catalog_rx` is the latch (see its
    /// field doc), so the button cannot pile up threads on a store that is answering slowly —
    /// which is exactly the store an impatient operator clicks Refresh on twice.
    pub fn spawn_catalog_refresh(&mut self, ctx: &egui::Context) {
        if self.catalog_rx.is_some() {
            return; // a walk is already owed an answer — see `catalog_rx`'s doc
        }
        self.catalog_rx =
            Some(crate::backend::catalog::spawn_catalog_load(self.store.clone(), ctx));
    }

    /// Point the Data pane's Import CSV at `datahub` (the remote store's address; `None` disarms it),
    /// signed with [`Self::named_run_keys`], the OBSERVE key — never [`Self::backtest_key`]: the
    /// upload is an observe-scope verb, armed on the datahub's own box (decision 0272). Call it after
    /// the shell seeds the observe key.
    pub fn arm_csv_upload(&mut self, datahub: Option<String>) {
        self.datahub_upload = datahub.map(|addr| crate::backend::csv_upload::CsvUploadDial {
            addr,
            keys: self.named_run_keys.clone(),
        });
    }

    /// The Data pane's Import CSV request, sent on a worker thread (`crate::backend::csv_upload`,
    /// decision 0272); [`Self::poll_csv_upload`] folds its progress and answer into the form. A request
    /// while one runs is dropped: the form disables its buttons, and `csv_upload_rx` is the latch.
    pub(crate) fn spawn_csv_upload(
        &mut self,
        ask: crate::panes::csv_import::CsvImportAsk,
        ctx: &egui::Context,
    ) {
        if self.csv_upload_rx.is_some() {
            return;
        }
        let Some(dial) = self.datahub_upload.clone() else {
            let why = "Studio reads no datahub, so there is nowhere to send the file.".to_string();
            self.data_browser.import.finish(ask.preview, Err(why));
            return;
        };
        self.data_browser.import.started(ask.preview);
        let rx = crate::backend::csv_upload::spawn_csv_upload(
            dial,
            ask.path,
            ask.target,
            ask.preview,
            ctx,
        );
        self.csv_upload_rx = Some((ask.preview, rx));
    }

    /// Fold the upload's events in: each chunk's progress, then its answer. A disconnect is a terminal
    /// failure, never a stuck spinner; an upload that stored days re-walks the catalog so its new lane
    /// shows in the pane.
    pub(crate) fn poll_csv_upload(&mut self, ctx: &egui::Context) {
        use crate::backend::csv_upload::UploadEvent;
        let Some((preview, rx)) = &self.csv_upload_rx else { return };
        let preview = *preview;
        let answer = loop {
            match rx.try_recv() {
                Ok(UploadEvent::Progress { sent, total }) => {
                    self.data_browser.import.progress(sent, total);
                }
                Ok(UploadEvent::Done(answer)) => break Some(answer),
                Err(TryRecvError::Empty) => break None,
                Err(TryRecvError::Disconnected) => {
                    break Some(Err("upload failed (worker terminated)".to_string()));
                }
            }
        };
        let Some(answer) = answer else { return };
        self.csv_upload_rx = None;
        let stored = answer.as_ref().is_ok_and(|done| done.stored_days > 0);
        self.data_browser.import.finish(preview, answer);
        if stored {
            self.spawn_catalog_refresh(ctx);
        }
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
            || self.walkforward_rx.is_some()
            || self.compare_rx.is_some()
            || self.study_rx.is_some()
            // A strategy file BUILD is in flight — the one worker here whose legitimate wait is minutes,
            // so it is the one the operator is most likely to think has hung. It belongs in the
            // same spinner + Cancel affordance as every other worker rather than only in the
            // Strategy file pane, which a user watching the toolbar is not necessarily looking at.
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
        self.walkforward_rx = None;
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
        // ⚠ ...and only while the buffer IS Rhai. In Strategy file mode the verdict is DROPPED, not kept
        // and hidden: the check never runs on Rust source (neither per frame nor per keystroke),
        // and a switch back to Rhai finds no verdict at all and so re-checks the buffer it now
        // holds — nothing judged before the switch can be shown after it. See `rhai_check`.
        if self.buffer_is_rhai() {
            let fresh =
                self.rhai_check.as_ref().is_some_and(|(checked, _)| *checked == self.editor.source);
            if !fresh {
                let verdict = crate::panes::editor::compile_status(&self.editor.source);
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
                self.strategy_file_built_source = Some(sent);
                self.rust_sha = Some(sha.clone());
                self.build_last = Some(Ok(sha));
            }
            Delivery::Ready((_, Err(e))) => {
                // ⚠ A FAILED build must not leave a previous sha standing as if it were current.
                // The buffer that failed to compile is what the author is looking at, so an
                // unchanged "built abc123" chip would invite a Run over the LAST artifact and
                // report it as this code. Clear both halves.
                self.rust_sha = None;
                self.strategy_file_built_source = None;
                self.build_last = Some(Err(e));
            }
            Delivery::Failed => {
                self.rust_sha = None;
                self.strategy_file_built_source = None;
                self.build_last =
                    Some(Err("build failed (worker terminated before answering)".to_string()));
            }
            Delivery::Pending => {}
        }
        // The Strategy pane's served list, and an Open's text (folded as a LOAD of that file).
        match poll_worker(&mut self.strategy_files_rx) {
            Delivery::Ready(answer) => self.strategy_files = Some(answer),
            Delivery::Failed => {
                self.strategy_files = Some(Err("listing failed (worker terminated)".to_string()));
            }
            Delivery::Pending => {}
        }
        match poll_tagged_worker(&mut self.open_rx) {
            Delivery::Ready((file, Ok(text))) => {
                self.open_last = None;
                self.open_file_text(&file, text);
            }
            Delivery::Ready((file, Err(e))) => self.open_last = Some(format!("{file}: {e}")),
            Delivery::Failed => {
                self.open_last = Some("open failed (worker terminated)".to_string())
            }
            Delivery::Pending => {}
        }
        match poll_tagged_worker(&mut self.save_file_rx) {
            Delivery::Ready(((file, sent), Ok((sha256, entry)))) => {
                let state = if entry.broken.is_empty() {
                    entry.build
                } else {
                    format!("broken: {}", entry.broken)
                };
                self.saved_file(file, sent, &sha256, &state);
            }
            Delivery::Ready((_, Err(e))) => self.save_file_last = Some(Err(e)),
            Delivery::Failed => {
                self.save_file_last = Some(Err("save failed (worker terminated)".to_string()));
            }
            Delivery::Pending => {}
        }
        match poll_worker(&mut self.sweep_rx) {
            Delivery::Ready(outcome) => self.sweep_last = Some(outcome),
            Delivery::Failed => {
                self.sweep_last =
                    Some(Err(RunError::Data("param scan failed (worker terminated)".into())));
            }
            Delivery::Pending => {}
        }
        match poll_worker(&mut self.walkforward_rx) {
            Delivery::Ready(outcome) => self.walkforward_last = Some(outcome),
            Delivery::Failed => {
                self.walkforward_last =
                    Some(Err(RunError::Data("walk-forward failed (worker terminated)".into())));
            }
            Delivery::Pending => {}
        }
        // Bound BEFORE the match: `poll_worker` holds `&mut self.compare_rx` across the whole
        // statement, so the `&self` read inside the arm would be a second borrow of `self`. The
        // compare table is annualized on the SAME factor the results pane uses — a row ranked in
        // one panel and read in the other must not be on two scales (`comparison_rows`' doc).
        let compare_periods_per_year = self.display_periods_per_year();
        match poll_worker(&mut self.compare_rx) {
            Delivery::Ready(results) => {
                // Snapshot the name -> source map first: `comparison_rows` borrows it while
                // `self.saved.compare_rows` is being assigned.
                let sources: Vec<(String, StrategySource)> =
                    self.saved.strategies.iter().map(|s| (s.name.clone(), s.source)).collect();
                let rows = comparison_rows(&results, compare_periods_per_year, |name| {
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
                let load = crate::backend::catalog::CatalogLoad::worker_terminated();
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
}
