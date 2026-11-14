//! The status writer of a registry-backed venue market feed, its DOM emission, and the registry its
//! threads spawn on.
//!
//! [`set_status`] is the ONE status writer every feed thread's context
//! (`crate::data_client::context::DataClientContext`) writes through. [`publish_top`] is every depth
//! lane's DOM emission, whichever context it runs in.
//!
//! [`pinned_feed_registry`] is the `vike_data::DataClientRegistry` a venue feed spawns on: each thread
//! first pins itself to the market-data role under the venue's label
//! (`vike_execution::affinity::pin_current_thread`, a no-op unless `config.pin_cores` names the role). It
//! lives here and not in `vike-data` because `vike-data` deliberately never depends on `vike-execution`.

use std::sync::Mutex;

use vike_data::{DataClientRegistry, DataClientSink};
use vike_marketdata::L2Book;

/// The prefix every HEALTHY status text starts with (`"LIVE · Binance"`, `"LIVE · OKX"`, …).
/// [`set_status`] classifies a transition's journal LEVEL on it, so a call site that invented another
/// healthy spelling would log its recovery as a warning; this is the one place it is spelled.
pub const HEALTHY_STATUS_PREFIX: &str = "LIVE ·";

/// The ONE status writer: replace a feed's status line with `s`, then fire its wake nudge — and, on a
/// TRANSITION only, say it in the journal, naming `venue`.
///
/// ⚠ **The mutex is a GUI channel and in a headless process it is a dead end.** `status` is read by
/// the desktop's status bar; the recorder and the datahub daemon never look at it, so without the
/// journal line every `seed error`/`ws error (reconnecting)` a feed thread produces on those boxes goes
/// nowhere (half of why a depth lane once failed for forty days in silence, and why a latched bybit
/// string suppressed a reconcile leg for 42 hours undiagnosable from the box).
///
/// **On a TRANSITION, and not on every call.** These strings are produced on reconnect loops; a line
/// per call would put a per-cycle event in a file layer that defaults to `trace` (the 341 GB shape,
/// root `CLAUDE.md`, Logging). So the emit is gated on the status TEXT CHANGING: entering an error
/// state speaks once, a venue repeating the identical error every 3 s speaks once, and coming back
/// healthy speaks once. The level splits on [`HEALTHY_STATUS_PREFIX`]: healthy at `info!`, anything
/// else at `warn!`, so `journalctl -p warning` shows a feed that stopped working. The same discipline
/// as `crate::depth::WebsocketSessionDepthFaultLog`, which speaks for the depth driver's own reconnect loop.
pub fn set_status(status: &Mutex<String>, wake: &(dyn Fn() + Send + Sync), venue: &str, s: String) {
    let changed = {
        let mut current = status.lock().unwrap();
        let changed = *current != s;
        if changed {
            current.clone_from(&s);
        }
        changed
    };
    if changed {
        if s.starts_with(HEALTHY_STATUS_PREFIX) {
            tracing::info!(target: "vike_bridge_core::feed_ctx", venue, status = %s, "feed status");
        } else {
            tracing::warn!(target: "vike_bridge_core::feed_ctx", venue, status = %s, "feed status");
        }
    }
    wake();
}

/// Publish the top `levels` of `book` per side to a DOM sink — carrying the book's tick so the GUI
/// quantizes on the SAME grid, stamped at RECEIPT (what a staleness check wants: how long since the
/// last update, not the venue event time) — then nudge a repaint. The body of the binance family's,
/// bybit's, deribit's and okx's depth-lane `publish_book`, each with its own venue and levels.
pub fn publish_top(
    sink: &dyn DataClientSink,
    wake: &(dyn Fn() + Send + Sync),
    venue: &str,
    symbol: &str,
    book: &L2Book,
    levels: usize,
) {
    let (bids, asks) = book.top_n(levels);
    sink.l2_snapshot(venue, symbol, book.tick_size, bids, asks, vike_model::now_ms());
    wake();
}

/// A `DataClientRegistry` whose every spawned thread first pins itself to the market-data role under
/// `venue` (the module doc).
pub fn pinned_feed_registry(venue: &'static str) -> DataClientRegistry {
    DataClientRegistry::with_spawn_hook(move || {
        vike_execution::affinity::pin_current_thread(
            vike_execution::affinity::Role::MarketData,
            venue,
        );
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The status write every venue feed carried: the line is replaced (not appended), and each
    /// write fires the wake nudge exactly once.
    #[test]
    fn set_status_replaces_the_line_and_wakes_once_per_write() {
        let status = Mutex::new("connecting…".to_string());
        let wakes = AtomicUsize::new(0);
        let wake = || {
            wakes.fetch_add(1, Ordering::Relaxed);
        };
        set_status(&status, &wake, "okx", "LIVE · X".into());
        assert_eq!(*status.lock().unwrap(), "LIVE · X");
        set_status(&status, &wake, "okx", "error".into());
        assert_eq!(*status.lock().unwrap(), "error");
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    /// The registry runs each body on its own thread after the spawn hook, which must neither
    /// panic nor block when no pin spec is installed (the default).
    #[test]
    fn the_pinned_registry_runs_each_body() {
        let mut registry = pinned_feed_registry("okx");
        let ran = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&ran);
        let id = registry
            .spawn("feed-okx-test".into(), move |_stop| {
                seen.fetch_add(1, Ordering::Relaxed);
            })
            .expect("a test thread spawns");
        registry.stop_join(id);
        assert_eq!(ran.load(Ordering::Relaxed), 1, "the body ran once, after the hook");
    }
}
