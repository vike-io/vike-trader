//! `App`'s window-close teardown — the body of the `eframe::App::on_exit` hook, as an INHERENT
//! method (`App::run_bounded_teardown`) that `main.rs`'s trait impl calls in one line.
//!
//! ⚠ **This is deliberately an `impl App`, not an `impl eframe::App for App`, and the distinction
//! is a hard rule rather than a style choice.** Rust's coherence permits a type any number of
//! INHERENT impl blocks — which is why `app_methods.rs` can carry a second `impl App { .. }` —
//! but exactly ONE `impl Trait for Type`. A second `impl eframe::App for App` beside `main.rs`'s
//! is `E0119` (conflicting implementations) plus an `E0046` on each block for the trait items it
//! does not name, and this file HELD one until it was repaired: the split was generalised from
//! `app_methods.rs`'s legal inherent split to an illegal trait split. So a lifecycle hook's BODY
//! can live here, but the `fn` that satisfies the trait cannot.
//!
//! Only the teardown moved. `persist_egui_memory` is one `false` and a comment — forwarding to it
//! would cost more lines than it saves, so it stays inline in `main.rs` beside `ui`, and this file
//! holds the one body that is genuinely worth having out of that file.

use super::*;

impl App {
    /// UNIFIED, BOUNDED teardown — the body of [`eframe::App::on_exit`], which forwards here in
    /// one line from `main.rs` (see this module's doc for why the trait method cannot live here).
    /// Every `on_exit` citation elsewhere in this crate means this sequence.
    ///
    /// Deterministic teardown is still the goal (the gate the legacy
    /// detached-thread feed falsified) — stop + JOIN every feed thread across every venue, THEN
    /// flush + join the tick recorder, THEN shut vt-core down losslessly, in that load-bearing
    /// order (Phase 1 rule, tick-producer T5: a live feed thread must never observe a closed
    /// recorder or core ingest channel). What changed: it no longer runs INLINE on this (the
    /// eframe/winit) thread, where it could block exit for many seconds.
    ///
    /// Why it used to be slow: each venue feed's `shutdown()` raises its stop flags then JOINS its
    /// threads, and a market-feed thread only observes its stop on the next socket READ-TIMEOUT
    /// tick (`family::market_feed::READ_TIMEOUT`/`depth::READ_TIMEOUT`, 2s). Run one venue after
    /// another across a multi-venue restored workspace (a binance chart + a bybit DOM + an okx
    /// chart …), that was ~2s × N → the observed >6s. And nothing bounded the wait, so a thread
    /// parked in a blocking connect/read held exit open indefinitely.
    ///
    /// The fix has two layers. (1) COOPERATIVE SIGNAL: raise the unified `shutdown` flag + the
    /// live-event forwarder's drain-stop up front (the latter still BEFORE the core is joined — its
    /// teardown-deadlock note in `App::new` is unchanged), and fan the per-venue feed shutdowns out
    /// so they wind down in PARALLEL (~one read-timeout total, not the per-venue sum). (2) BOUNDED
    /// JOIN as the guarantee: a single overall DEADLINE bounds the whole sequence, then we return
    /// regardless. If a socket-read thread ignores its stop past the deadline we abandon it —
    /// returning here lets `run_native`/`main` return and the process exit (the OS reaps the parked
    /// thread; nothing on a market-data read path persists). The graceful path (the common case — a
    /// busy socket notices its stop in milliseconds) still completes every flush well within it.
    ///
    /// The bounded-join ORCHESTRATION itself (parallel tasks + a sequential tail under one deadline,
    /// on a throwaway thread with a one-shot-channel timed wait) is pure and renderer-agnostic, so it
    /// was lifted into `vike_app_core::shutdown::run_with_deadline` where CI covers it with a
    /// regression test (this crate is CI-excluded). This method keeps only the eframe/App glue: the
    /// cooperative signal, which real handles to tear down, and the 1500 ms policy value.
    pub(crate) fn run_bounded_teardown(&mut self) {
        // Overall deadline: the hard bound on how long window-close can take. Comfortably above the
        // graceful common case (busy sockets wind down in ms) and below the old >6s force-kill.
        const SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_millis(1500);
        use std::sync::atomic::Ordering::Relaxed;

        // (1) Unified signal — also gates `ensure_feed_on` (no NEW blocking reads are started once
        // set). Raise the forwarder drain-stop up front too: it must be set BEFORE the core is
        // joined so a live user-data pump's `blocking_send` into `live_rx` can't wedge the core's
        // exec-thread join (the teardown-deadlock note at the forwarder spawn in `App::new`).
        self.shutdown.store(true, Relaxed);
        self.forwarder_stop.store(true, Relaxed);

        // (2) Hand the real handles to the extracted, CI-tested orchestrator
        // (`vike_app_core::shutdown::run_with_deadline` — see that module for the contract). The
        // per-venue feed shutdowns run as PARALLEL tasks: each `shutdown()` raises its subs' stop
        // flags then joins its threads, so fanning them out makes the whole set cost ~one
        // READ_TIMEOUT instead of the old per-venue sum (an idle venue drains instantly). The
        // recorder/recon/materializer/core teardown is the load-bearing SEQUENTIAL tail, in the SAME
        // order as before: recorder AFTER the feeds it records (final drain + join); the reconcile
        // driver (holds only a WEAK core-ingest sender, so it can never wedge core exit — joined here
        // only to bound its thread to the app's lifetime); the journal materializer (final WAL drain
        // + join); the core LAST — lossless `Command::Shutdown` + join, which also detaches every
        // live ExecActor, closing its command channel so its recv returns. The helper bounds the
        // whole sequence by SHUTDOWN_DEADLINE and returns regardless, so the >6s window-close hang
        // cannot recur (its regression test lives with the helper in `vike-app-core`, which — unlike
        // this crate — runs in CI).
        let feed_tasks: Vec<Box<dyn FnOnce() + Send + 'static>> = std::mem::take(&mut self.feeds)
            .into_values()
            .map(|mut f| -> Box<dyn FnOnce() + Send + 'static> { Box::new(move || f.shutdown()) })
            .collect();
        let recorder = self.recorder.take();
        let recon = self.recon_driver.take();
        let materializer = self.materializer.take();
        let core = self.core.take();
        // The active remote backend (observe mode only; `None` on every other path): take + drop
        // it here so the control channel's `Drop` shuts the socket and joins its worker thread,
        // and the observe bridge's `Drop` stops-and-joins the reconnect thread (B6) — a fast,
        // self-contained teardown that needs no separate step and never outlives the app.
        let _ = self.active_backend.take();
        let tail: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
            if let Some(recorder) = recorder {
                recorder.shutdown();
            }
            if let Some(driver) = recon {
                driver.shutdown();
            }
            if let Some(mat) = materializer {
                mat.shutdown();
            }
            if let Some(core) = core {
                core.shutdown_and_join();
            }
        });
        let _ = vike_app_core::shutdown::run_with_deadline(feed_tasks, tail, SHUTDOWN_DEADLINE);
    }
}
