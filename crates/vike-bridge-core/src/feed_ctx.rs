//! The per-thread context of a registry-backed venue market feed, and the registry its threads
//! spawn on.
//!
//! [`FeedCtx`] is what one feed thread of deribit's, hyperliquid's and okx's `Feeds` carries: the
//! feed's shared sink, its ONE operator status line and GUI wake nudge (every thread of a feed writes
//! the same line), and the thread's own registry-issued stop flag. A venue whose thread context
//! carries more (oanda's session config, polymarket's raw tap and properties recorder) keeps its own
//! struct and writes its status through [`set_status`]; bybit's status write classifies a journal
//! level and the binance family's context carries the venue spec, so both keep their own whole.
//! [`publish_top`] is every depth lane's DOM emission, whichever context it runs in.
//!
//! [`pinned_feed_registry`] is the `vike_data::FeedRegistry` a venue `Feeds` spawns on: each thread
//! first pins itself to the market-data role under the venue's label
//! (`vike_exec::affinity::pin_current_thread`, a no-op unless `config.pin_cores` names the role). It
//! lives here and not in `vike-data` because `vike-data` deliberately never depends on `vike-exec`.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use vike_data::{FeedRegistry, LiveDataSink};
use vike_marketdata::L2Book;

/// One feed thread's context (the module doc says whose).
pub struct FeedCtx {
    /// The feed's sink, shared by every thread it spawns.
    pub sink: Arc<dyn LiveDataSink>,
    /// The feed's ONE operator-facing status line, shared by every thread it spawns.
    pub status: Arc<Mutex<String>>,
    /// The GUI repaint nudge (`|| {}` headless).
    pub wake: Arc<dyn Fn() + Send + Sync>,
    /// This thread's own stop flag, issued by the registry.
    pub stop: Arc<AtomicBool>,
}

impl FeedCtx {
    /// Replace the feed's status line with `s` and nudge a repaint ([`set_status`]).
    pub fn set_status(&self, s: String) {
        set_status(&self.status, &*self.wake, s);
    }
}

/// Replace a feed's status line with `s`, then fire its wake nudge.
pub fn set_status(status: &Mutex<String>, wake: &(dyn Fn() + Send + Sync), s: String) {
    *status.lock().unwrap() = s;
    wake();
}

/// Publish the top `levels` of `book` per side to a DOM sink — carrying the book's tick so the GUI
/// quantizes on the SAME grid, stamped at RECEIPT (what a staleness check wants: how long since the
/// last update, not the venue event time) — then nudge a repaint. The body of the binance family's,
/// bybit's, deribit's and okx's depth-lane `publish_book`, each with its own venue and levels.
pub fn publish_top(
    sink: &dyn LiveDataSink,
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

/// A `FeedRegistry` whose every spawned thread first pins itself to the market-data role under
/// `venue` (the module doc).
pub fn pinned_feed_registry(venue: &'static str) -> FeedRegistry {
    FeedRegistry::with_spawn_hook(move || {
        vike_exec::affinity::pin_current_thread(vike_exec::affinity::Role::MarketData, venue);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        set_status(&status, &wake, "LIVE · X".into());
        assert_eq!(*status.lock().unwrap(), "LIVE · X");
        set_status(&status, &wake, "error".into());
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
