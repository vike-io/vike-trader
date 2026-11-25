//! `StudioState`'s in-flight worker lifecycle: the ⟳ Refresh catalog walk's spawn and latch, "is
//! anything running" (`any_running`), Cancel (`cancel`), and the per-frame `poll` that folds each
//! worker's outcome in, with the helpers `poll` is written against (`StudioWorkerDelivery`,
//! `poll_worker`, `poll_tagged_worker`) and the two in-flight request shapes `StudioState` holds.
//!
//! Split out of `studio/mod.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use std::sync::mpsc::TryRecvError;

use super::*;

impl StudioState {
    /// ⟳ Refresh: re-read the store's catalog ON A WORKER THREAD and fold the answer in on a later
    /// frame (`poll()`), instead of walking it inline between two frames.
    ///
    /// The click used to run four full catalog walks in the frame — three `list_series` calls from
    /// `StudioSlicePicker::refresh` plus `StudioDataBrowserPane::refresh`'s `inventory()` — which on the
    /// `HistoryStoreDatahub` this shell holds whenever a datahub address resolves is four fresh TCP
    /// connects on the egui paint thread. `crate::backend::catalog`'s module doc carries the measurement and
    /// the `vike-desktop` precedent this copies (`refresh_stored` + `DataManagerInventory::loading`).
    ///
    /// A second click while a walk is in flight is a NO-OP: `catalog_rx` is the latch (see its
    /// field doc), so the button cannot pile up threads on a store that is answering slowly —
    /// which is exactly the store an impatient operator clicks Refresh on twice.
    pub(crate) fn start_catalog_load(&mut self, context: &egui::Context) {
        if self.catalog_rx.is_some() {
            return; // a walk is already owed an answer — see `catalog_rx`'s doc
        }
        self.catalog_rx =
            Some(crate::backend::catalog::spawn_catalog_load(self.store.clone(), context));
    }

    /// Whether the shell must seed [`Self::hub_keys`] (and re-arm the Import CSV dial that copies it):
    /// true the first time, and again each time `epoch`, the shell's device-key epoch, moves; it
    /// records `epoch` as seeded. A Studio opened before the desktop's Connect made the device key
    /// signs with it from the next frame on, the way the desktop's datahub dials re-resolve.
    pub fn hub_keys_due(&mut self, epoch: u64) -> bool {
        self.hub_keys_epoch.replace(epoch) != Some(epoch)
    }

    /// Point the Data pane's Import CSV at `datahub` (the remote store's address; `None` disarms it),
    /// signed with [`Self::hub_keys`], this PC's device key: the upload is an observe-scope verb, armed
    /// on the datahub's own box (decision 0272). Call it after the shell seeds the key.
    pub fn arm_csv_upload(&mut self, datahub: Option<String>) {
        self.datahub_upload = datahub.map(|address| {
            crate::backend::csv_upload::StudioCsvUploadDial { address, keys: self.hub_keys.clone() }
        });
    }

    /// The Data pane's Import CSV request, sent on a worker thread (`crate::backend::csv_upload`,
    /// decision 0272); [`Self::poll_csv_upload`] folds its progress and answer into the form. A request
    /// while one runs is dropped: the form disables its buttons, and `csv_upload_rx` is the latch.
    pub(crate) fn start_csv_upload(
        &mut self,
        ask: crate::panes::csv_upload::StudioCsvUploadRequest,
        context: &egui::Context,
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
            context,
        );
        self.csv_upload_rx = Some((ask.preview, rx));
    }

    /// Fold the upload's events in: each chunk's progress, then its answer. A disconnect is a terminal
    /// failure, never a stuck spinner; an upload that stored days re-walks the catalog so its new lane
    /// shows in the pane.
    pub(crate) fn poll_csv_upload(&mut self, context: &egui::Context) {
        use crate::backend::csv_upload::StudioCsvUploadEvent;
        let Some((preview, rx)) = &self.csv_upload_rx else { return };
        let preview = *preview;
        let answer = loop {
            match rx.try_recv() {
                Ok(StudioCsvUploadEvent::Progress { sent, total }) => {
                    self.data_browser.import.progress(sent, total);
                }
                Ok(StudioCsvUploadEvent::Done(answer)) => break Some(answer),
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
            self.start_catalog_load(context);
        }
    }

    /// True while the ⟳ Refresh walk is in flight — what the toolbar disables the button on.
    ///
    /// Separate from [`Self::any_running`] on purpose: that one drives the CANCEL button, and
    /// Cancel does not offer to abandon a catalog walk (see `catalog_rx`'s field doc).
    pub(crate) fn catalog_loading(&self) -> bool {
        self.catalog_rx.is_some()
    }

    /// True while any worker-thread task (Run, Sweep, Walk-Forward, or the Saved pane's Compare
    /// all) is in flight — what the toolbar's Cancel button shows/hides on.
    pub(crate) fn any_running(&self) -> bool {
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
        self.note_buffer_language();
        match poll_worker(&mut self.server_check_rx) {
            StudioWorkerDelivery::Ready(answer) => self.server_check = Some(answer),
            StudioWorkerDelivery::Failed => {
                self.server_check = Some(Err("the check's worker ended without answering".into()));
            }
            StudioWorkerDelivery::Pending => {}
        }
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
            StudioWorkerDelivery::Ready(outcome) => {
                self.last = Some(outcome);
                self.running = false;
                self.run_origin.answer();
            }
            StudioWorkerDelivery::Failed => {
                self.last = Some(Err(RunError::Data("run failed (worker terminated)".into())));
                self.running = false;
                self.run_origin.answer();
            }
            StudioWorkerDelivery::Pending => {}
        }
        // THE JOIN's sequencing point. A build answers with a sha and NOTHING ELSE happens: no Run
        // is dispatched from here. The operator presses Run next, and `run_blocked_reason` — which
        // this delivery is what unblocks — has already been consulted by every dispatcher. That is
        // the design's "await `Ok(sha)` from the builder, THEN send Run", expressed as a state
        // change rather than as a chained call, which is also what keeps a failed build from
        // launching anything.
        match poll_tagged_worker(&mut self.build_rx) {
            StudioWorkerDelivery::Ready((sent, Ok(sha))) => {
                // Record the source this sha was built FROM in the same step that accepts the sha:
                // the two are one fact, and setting them apart is how they drift.
                //
                // ⚠ `sent`, the bytes the dispatch captured — NEVER `editor.source` read now. The
                // build took as long as a cargo build takes, and an edit made during it is not in
                // this artifact: recording the live buffer here marked that edit as built, and the
                // staleness guard then let Run execute the old artifact under the new code.
                self.rust_built_source = Some(sent);
                self.rust_sha = Some(sha.clone());
                self.build_last = Some(Ok(sha));
            }
            StudioWorkerDelivery::Ready((_, Err(e))) => {
                // ⚠ A FAILED build must not leave a previous sha standing as if it were current.
                // The buffer that failed to compile is what the author is looking at, so an
                // unchanged "built abc123" chip would invite a Run over the LAST artifact and
                // report it as this code. Clear both halves.
                self.rust_sha = None;
                self.rust_built_source = None;
                self.build_last = Some(Err(e));
            }
            StudioWorkerDelivery::Failed => {
                self.rust_sha = None;
                self.rust_built_source = None;
                self.build_last =
                    Some(Err("build failed (worker terminated before answering)".to_string()));
            }
            StudioWorkerDelivery::Pending => {}
        }
        // The Strategy pane's served list, and an Open's text (folded as a LOAD of that file).
        match poll_worker(&mut self.strategy_files_rx) {
            StudioWorkerDelivery::Ready(answer) => self.strategy_files = Some(answer),
            StudioWorkerDelivery::Failed => {
                self.strategy_files = Some(Err("listing failed (worker terminated)".to_string()));
            }
            StudioWorkerDelivery::Pending => {}
        }
        match poll_tagged_worker(&mut self.open_rx) {
            StudioWorkerDelivery::Ready((file, Ok(text))) => {
                self.open_last = None;
                self.open_file_text(&file, text);
            }
            StudioWorkerDelivery::Ready((file, Err(e))) => {
                self.open_last = Some(format!("{file}: {e}"))
            }
            StudioWorkerDelivery::Failed => {
                self.open_last = Some("open failed (worker terminated)".to_string())
            }
            StudioWorkerDelivery::Pending => {}
        }
        match poll_tagged_worker(&mut self.save_file_rx) {
            StudioWorkerDelivery::Ready(((file, sent), Ok((sha256, entry)))) => {
                let state = if entry.broken.is_empty() {
                    entry.build
                } else {
                    format!("broken: {}", entry.broken)
                };
                self.saved_file(file, sent, &sha256, &state);
            }
            StudioWorkerDelivery::Ready((_, Err(e))) => self.save_file_last = Some(Err(e)),
            StudioWorkerDelivery::Failed => {
                self.save_file_last = Some(Err("save failed (worker terminated)".to_string()));
            }
            StudioWorkerDelivery::Pending => {}
        }
        // The strategy on screen owns `grid`: switch rows before anything below reads them.
        self.sync_grid_key();
        let answered = match poll_worker(&mut self.sweep_rx) {
            StudioWorkerDelivery::Ready(outcome) => Some(outcome),
            StudioWorkerDelivery::Failed => {
                Some(Err(RunError::Data("param scan failed (worker terminated)".into())))
            }
            StudioWorkerDelivery::Pending => None,
        };
        if let Some(outcome) = answered {
            // The Sweep pane's status line says how long it took, and scrolls the answer into view.
            self.sweep_took = self.sweep_started.take().map(|sent| sent.elapsed());
            self.sweep_reveal = true;
            self.sweep_last = Some(outcome);
            self.scan_origin.answer();
        }
        // A strategy file's text, read to re-cut the parameter rows to its header.
        match poll_worker(&mut self.seed_file_rx) {
            StudioWorkerDelivery::Ready(Ok(text)) => self.recut_grid(&text),
            StudioWorkerDelivery::Ready(Err(e)) => self.seed_failed = Some(e),
            StudioWorkerDelivery::Failed => {
                self.seed_failed = Some("reading the file failed (worker terminated)".into());
            }
            StudioWorkerDelivery::Pending => {}
        }
        match poll_worker(&mut self.walkforward_rx) {
            StudioWorkerDelivery::Ready(outcome) => {
                // A walk-forward that answered is put in front, on the results' Walk-forward tab
                // (the owner, 2026-11-20: finishing one used to change nothing visible); a refusal
                // shows on the backtest surface's failure line.
                self.center = StudioCenterView::Backtest;
                if outcome.is_ok() {
                    self.tab = StudioResultsTab::Walkforward;
                }
                self.walkforward_last = Some(outcome);
                self.walkforward_origin.answer();
            }
            StudioWorkerDelivery::Failed => {
                self.walkforward_last =
                    Some(Err(RunError::Data("walk-forward failed (worker terminated)".into())));
                self.walkforward_origin.answer();
            }
            StudioWorkerDelivery::Pending => {}
        }
        // A scan point the user opened, and the Trades tab's page: both fetched from the run the
        // backtesthub holds (decision 0078's amendment of 2026-11-16).
        match poll_worker(&mut self.point_rx) {
            StudioWorkerDelivery::Ready(outcome) => self.sweep_open = Some(outcome),
            StudioWorkerDelivery::Failed => {
                self.sweep_open = Some(Err(RunError::Data(
                    "opening the point failed (worker terminated)".into(),
                )));
            }
            StudioWorkerDelivery::Pending => {}
        }
        match poll_worker(&mut self.trades_rx) {
            StudioWorkerDelivery::Ready(page) => {
                self.trades.shown = Some(page.map_err(|e| e.to_string()))
            }
            StudioWorkerDelivery::Failed => {
                self.trades.shown = Some(Err("the trades page failed (worker terminated)".into()));
            }
            StudioWorkerDelivery::Pending => {}
        }
        // Each row is the server's summary, annualized at the slice's interval as the results pane
        // is (`saved_compare_rows`' doc), so the table takes no factor here.
        match poll_worker(&mut self.compare_rx) {
            StudioWorkerDelivery::Ready(results) => {
                // Snapshot the name -> source map first: `saved_compare_rows` borrows it while
                // `self.saved.compare_rows` is being assigned.
                let sources: Vec<(String, StudioStrategySource)> =
                    self.saved.strategies.iter().map(|s| (s.name.clone(), s.source)).collect();
                let rows = saved_compare_rows(&results, |name| {
                    sources
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, s)| *s)
                        .unwrap_or(StudioStrategySource::Rhai)
                });
                self.saved.compare_rows = Some(rows);
            }
            StudioWorkerDelivery::Failed => {
                self.saved.compare_error = Some("compare failed (worker terminated)".to_string());
            }
            StudioWorkerDelivery::Pending => {}
        }
        // The ⟳ Refresh walk (`start_catalog_load`). ONE message feeds both store-reading
        // panes, each through its own `apply` — the same fold the synchronous `refresh` uses, so
        // an off-thread answer and a blocking one cannot mean different things. A disconnect is a
        // terminal failure in BOTH panes rather than a silent keep-the-old-lists, which would
        // claim the refresh had found the store unchanged.
        match poll_worker(&mut self.catalog_rx) {
            StudioWorkerDelivery::Ready(load) => {
                self.picker.apply(load.series);
                self.data_browser.apply(load.inventory);
            }
            StudioWorkerDelivery::Failed => {
                let load = crate::backend::catalog::StudioCatalogLoad::worker_terminated();
                self.picker.apply(load.series);
                self.data_browser.apply(load.inventory);
            }
            StudioWorkerDelivery::Pending => {}
        }
        // The Research pane's two lists, each answer handed to the pane; a dead worker is a refusal
        // in the list's place, never a list that silently stays empty.
        match poll_worker(&mut self.research_studies_rx) {
            StudioWorkerDelivery::Ready(answer) => self.research.apply_studies(answer),
            StudioWorkerDelivery::Failed => {
                self.research.apply_studies(Err(RESEARCH_LIST_LOST.to_string()));
            }
            StudioWorkerDelivery::Pending => {}
        }
        match poll_worker(&mut self.research_runs_rx) {
            StudioWorkerDelivery::Ready(answer) => self.research.apply_runs(answer),
            StudioWorkerDelivery::Failed => {
                self.research.apply_runs(Err(RESEARCH_LIST_LOST.to_string()));
            }
            StudioWorkerDelivery::Pending => {}
        }
        match poll_worker(&mut self.study_rx) {
            StudioWorkerDelivery::Ready(outcome) => {
                let finished = outcome.is_ok();
                self.study_last = Some(outcome);
                // A run that just finished belongs in the list it was saved into: ask for the
                // newest page again, and only the runs, because no study can have changed by
                // running one.
                if finished {
                    self.start_research_runs_page(0);
                }
            }
            StudioWorkerDelivery::Failed => {
                self.study_last = Some(Err("study failed (worker terminated)".to_string()));
            }
            StudioWorkerDelivery::Pending => {}
        }
        match poll_worker(&mut self.study_artifact_rx) {
            StudioWorkerDelivery::Ready(answer) => self.research.apply_artifact(answer),
            StudioWorkerDelivery::Failed => {
                self.research.apply_artifact(Err(RESEARCH_LIST_LOST.to_string()));
            }
            StudioWorkerDelivery::Pending => {}
        }
        // The SDK source view's list, a folder of its tree and the file of its code view; a dead worker
        // is a refusal in that place, never a tree that silently stays unread.
        match poll_worker(&mut self.sdk_listing_rx) {
            StudioWorkerDelivery::Ready(answer) => {
                let pin = self.strategy_sdk_pin();
                self.sdk_source.apply_listing(answer, pin);
            }
            StudioWorkerDelivery::Failed => {
                let pin = self.strategy_sdk_pin();
                self.sdk_source.apply_listing(Err(RESEARCH_LIST_LOST.to_string()), pin);
            }
            StudioWorkerDelivery::Pending => {}
        }
        match poll_tagged_worker(&mut self.sdk_folder_rx) {
            StudioWorkerDelivery::Ready(((sdk, path), answer)) => {
                self.sdk_source.apply_folder(&sdk, path, answer);
            }
            // The tag went with the slot: the folder stays unread and the tree asks for it again.
            StudioWorkerDelivery::Failed | StudioWorkerDelivery::Pending => {}
        }
        match poll_worker(&mut self.sdk_file_rx) {
            StudioWorkerDelivery::Ready(answer) => self.sdk_source.apply_file(answer),
            StudioWorkerDelivery::Failed => {
                self.sdk_source.apply_file(Err(RESEARCH_LIST_LOST.to_string()));
            }
            StudioWorkerDelivery::Pending => {}
        }
        match poll_worker(&mut self.chat_rx) {
            StudioWorkerDelivery::Ready(outcome) => {
                self.chat.running = false;
                self.chat.history.push(("assistant".to_string(), summary_of(&outcome)));
                self.chat.last = outcome.ok();
            }
            StudioWorkerDelivery::Failed => {
                // Mirrors the run/sweep/wf arms above: a disconnect (worker panic, or its sender
                // dropped without sending) is a terminal failure, not silence -- surface it in the
                // transcript so `StudioChatOutcome`'s doc ("a worker panic ... surfaced by poll()'s
                // disconnect arm") is actually true.
                self.chat.running = false;
                self.chat.history.push((
                    "assistant".to_string(),
                    "AI worker terminated unexpectedly".to_string(),
                ));
            }
            StudioWorkerDelivery::Pending => {}
        }
    }
}

/// What a Research list or an artifact page shows when its worker died without answering.
const RESEARCH_LIST_LOST: &str = "reading from the backtest server failed (worker terminated)";

/// The result of polling one worker-thread receiver in [`StudioState::poll`]: a delivered value, a
/// terminal disconnect (the worker panicked / dropped its sender without sending), or still-pending.
/// [`poll_worker`] clears the receiver slot on either terminal arm, so each `poll()` site handles
/// only the value / the failure — never the repeated "set `*_rx = None`" bookkeeping.
enum StudioWorkerDelivery<T> {
    Ready(T),
    Failed,
    Pending,
}

/// Poll `slot` once with `try_recv`, clearing it to `None` on any terminal outcome (delivered or
/// disconnected). Centralizes the three-way `TryRecvError` match every worker arm in
/// [`StudioState::poll`] shares.
fn poll_worker<T>(slot: &mut Option<Receiver<T>>) -> StudioWorkerDelivery<T> {
    let Some(rx) = slot else {
        return StudioWorkerDelivery::Pending;
    };
    match rx.try_recv() {
        Ok(v) => {
            *slot = None;
            StudioWorkerDelivery::Ready(v)
        }
        Err(TryRecvError::Disconnected) => {
            *slot = None;
            StudioWorkerDelivery::Failed
        }
        Err(TryRecvError::Empty) => StudioWorkerDelivery::Pending,
    }
}

/// A strategy file build in flight: the EXACT editor buffer that was sent to the builder, and the receiver
/// its answer arrives on. One value, so the answer can only ever be folded against the request it
/// answers — see [`StudioState::build_rx`].
pub(super) type StudioWorkerRustBuildRequest = (String, Receiver<Result<String, String>>);

/// An in-flight Save of the editor buffer to a server strategy file: `(file, the text sent)` as the
/// tag, and the receiver of the server's answer (the sha256 written and the file's states).
pub(super) type StudioWorkerStrategyFileSaveRequest =
    ((String, String), Receiver<Result<(String, WireStrategyFile), String>>);

/// An in-flight folder read of the SDK tree: `(sdk, path)` as the tag, so the answer lands in the folder
/// of the SDK it was asked of, and the receiver of the server's answer.
pub(super) type StudioWorkerSdkRead = ((String, String), Receiver<Result<WireSdkFile, String>>);

/// [`poll_worker`] for a receiver that travels with a TAG — a fact about the REQUEST that the answer
/// must be folded against, not a fact read off the shell when the answer happens to land. The tag
/// comes back beside the value; on a disconnect it is dropped with the slot.
fn poll_tagged_worker<K, T>(slot: &mut Option<(K, Receiver<T>)>) -> StudioWorkerDelivery<(K, T)> {
    let Some((tag, rx)) = slot.take() else {
        return StudioWorkerDelivery::Pending;
    };
    match rx.try_recv() {
        Ok(v) => StudioWorkerDelivery::Ready((tag, v)),
        Err(TryRecvError::Disconnected) => StudioWorkerDelivery::Failed,
        Err(TryRecvError::Empty) => {
            *slot = Some((tag, rx));
            StudioWorkerDelivery::Pending
        }
    }
}
