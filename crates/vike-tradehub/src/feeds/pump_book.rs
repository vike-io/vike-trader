//! `PumpBookFold` — the sink in front of the CEX feed's `CoreLaneSink` that turns the tick pump's
//! book back into the book the engine has always seen.
//!
//! # Why it exists
//!
//! The CEX pump used to push `Ingest::Book(Arc<L2Book>)` — the whole folded book, every update —
//! straight onto the core's tick lane, from inside this process. The pump now runs at the gatewayhub
//! and its book crosses two hops as an ANCHOR (`BookUpdateKind::Snapshot`, the top
//! `PUMP_BOOK_LEVELS` levels a side) plus DELTAS (`BookUpdateKind::Delta`, only the levels that
//! changed, a removed level as `qty == 0`), because a full book per update is tens of kilobytes at a
//! thousand updates a second (`crates/vike-gatewayhub/src/data/pump.rs`'s `BookFeed`). Those arrive here
//! as `LiveDataSink::book_update`, and `CoreLaneSink` leaves that verb a no-op (the raw book is a
//! GUI/recorder concern the core does not carry) — so without this fold the engine would see a book once
//! every anchor (64 updates or 2 s) and a stale one in between. The maker prices off
//! `on_order_book`; that would be a regression of the working behaviour.
//!
//! So this sink keeps one [`L2Book`] per symbol, folds each anchor with `apply_snapshot` and each delta
//! with `apply_delta`, and forwards the result as `LiveDataSink::book(Arc<L2Book>)`, which
//! `CoreLaneSink` turns into the same `Ingest::Book` as before.
//!
//! # A hole is a wait for the next anchor
//!
//! * a delta with **no book held** (the first message after a link loss; the pump's encoder resets
//!   on a disconnect, so the next update is an anchor) is dropped;
//! * a delta on **another price grid** than the held book cannot be applied (the encoder only sends a
//!   delta on the grid of the update before): the book is dropped, loudly once, and the sink waits for
//!   the next anchor;
//! * a delta whose sequence is **not newer** than the book's (a duplicate or a replay) is ignored, the
//!   book kept: `L2Book::apply_delta` says so and nothing was lost;
//! * a **link `GapStart`** for the symbol (the pump's own disconnect word, or this process's strat link
//!   word) drops the book: whatever the subscriber held is no longer the pump's. The `GapStart` itself
//!   is forwarded untouched.
//! * a **hole on the book lane** (the tube's `GapStart`/`Live` pair under the book word, which the
//!   datahub-client's `LinkSink` also answers by withholding deltas until an anchor) is told to the
//!   engine for what it is, see the next section.
//!
//! * an **anchor that restates the book already held** (same grid, same levels: the sender's periodic anchor of a
//!   quiet book) replaces the folded book but is not forwarded again, so the strategy is not made to re-price an
//!   identical book every ~2 s (`SpreadMaker::requote` amends its resting quote on every call with the refresh
//!   tolerance off). The anchor that ends a hole is always forwarded.
//!
//! This fold exists for the four CEX venues only (binance, bybit, okx, aster: `feeds/cex.rs` builds it); no other
//! venue's feed passes through it.
//!
//! Every other verb passes through unchanged.
//!
//! # `Live` for the book means a CONSISTENT book
//!
//! The tube discloses a lost frame as a `GapStart` then a `Live` back to back, so on its own the engine
//! would hear "Disconnected, Live" a microsecond apart while this fold held (and kept) a book that is
//! missing a delta, and nothing would change that until the sender's next anchor: a strategy reading
//! depth would trade on a stale book that the status stream called healthy. The fold therefore tells the
//! hole as it is. From the book-word `GapStart` until the next ANCHOR it:
//!
//! * drops the held book (no `Ingest::Book` reaches the core, so the engine's last book is the one
//!   from before the hole and is never advanced past it);
//! * forwards the `GapStart` as a **`Stale`** on the book stream, not as the `GapStart` it received: the
//!   connection-state dead-man never counts a `Stale`, so a quiet book that takes minutes to anchor can
//!   not trip a halt, while a strategy's `on_feed_status` hears a non-`Live` status (the cross-exchange
//!   maker halts and pulls on any);
//! * WITHHOLDS the paired `Live`, and sends it (the original gap stamp) right after the anchor's book.
//!
//! The sender anchors at once when it sees the loss itself, and otherwise at its next periodic anchor
//! (`crates/vike-gatewayhub/src/data/pump.rs`'s `ANCHOR_EVERY`, 2 s; a QUIET book gets one too, from the hub's
//! own timer thread, not only on a pump message).
//!
//! # The wait has a DEADLINE ([`ANCHOR_DEADLINE`])
//!
//! A wait that only an anchor can end must not be able to last for ever: a pump whose messages stopped
//! without a `Disconnected`, an anchor frame lost to the same queue that lost the delta, a sender that is
//! gone, would leave the engine `Stale` indefinitely, with nothing more said and the dead-man blind to a
//! stale book. So a hole that has had NO anchor for [`ANCHOR_DEADLINE`] (3 x the sender's 2 s anchor period =
//! 6 s: the sender has had three chances) escalates ONCE: the fold tells the engine `GapStart` on the book
//! stream (what a strategy's `on_feed_status` hears as `Disconnected`, and what the connection-state
//! dead-man DOES count, as a window of the venue's own source that the paired `Live` after the anchor closes,
//! and that trips `CancelAllAndHalt` after the grace if the anchor never comes), and says so at `error`.
//! A watcher thread per OPEN hole does it (a hole is rare; the hot verbs take no lock for it). Its decision and
//! its `GapStart` send are ONE critical section under the fold's lock: the reader sends an anchor's `Live` only
//! after its own `fold` (the same lock) removed the hole, so an anchor that lands while the escalation is in flight
//! is folded AFTER it and its `Live` follows the `GapStart` it closes (the other order, `Live` then `GapStart`, is a
//! dead-man window nothing closes). A fold that is dropped (its feed shut down) ends every open wait, so a detached
//! watcher never reports a hole for a book nobody holds. There is no
//! "resync request" because the tube has no frame for one: the sender's periodic anchor IS the resync, and
//! this is what notices when it does not arrive.
//!
//! # What a strategy sees
//!
//! A book hole: no `on_order_book` call until the anchor (the core holds no book of its own; the fold forwards
//! none), `on_feed_status(Stale)` (and, after the deadline, `Disconnected`), then the anchor's whole book and
//! `Live`. `XemmMaker` (the only in-tree `on_feed_status` hook) halts and pulls its quotes on the first
//! non-`Live` status and **latches the halt permanently** (`resume_after_halt_ms` defaults to 0: it never
//! resumes by itself), so ANY book hole halts it until an operator re-tunes it or restarts it, as the old
//! `GapStart`/`Live` pair did: this fold changed nothing for it. `SpreadMaker` has no feed-status hook: after
//! the fold drops the book it receives no `on_order_book` calls and quotes from `on_quote_tick` alone (one
//! level, `BookView::from_quote`), then gets the full book back with the anchor; depth-style and
//! own-order-filtration quoting degrade to touch-based during the hole, silently.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use vike_data::{LiveDataSink, StreamLabel};
use vike_marketdata::{Bar, BookLevel, BookUpdate, BookUpdateKind, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus, now_ms};
use vike_venues::link_deadman::STRAT_LINK_STREAM;

/// The tube's word for the CEX pump's link health (`LiveDataSink::stream_status`'s `stream`).
use vike_market_stream::wire::LINK_STREAM_WORD;

/// How long a hole may wait for its anchor before the fold escalates: 3 x the sender's anchor period
/// (`vike_gatewayhub::data::pump::ANCHOR_EVERY`, 2 s), so the sender has had three chances to anchor. A test in
/// `pump_book_tests.rs` pins the arithmetic against the sender's constant.
pub(super) const ANCHOR_DEADLINE: Duration = Duration::from_secs(6);

/// The end of one hole's wait, signalled by the anchor that ends it so the watcher thread leaves at once.
struct Wait {
    over: Mutex<bool>,
    cv: Condvar,
}

impl Wait {
    fn new() -> Arc<Self> {
        Arc::new(Self { over: Mutex::new(false), cv: Condvar::new() })
    }

    fn finish(&self) {
        *self.over.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_all();
    }
}

/// One open hole: when it opened, its end-of-wait signal, whether a watcher thread exists for it, and whether
/// it has been escalated.
struct Hole {
    at_ts_ms: i64,
    wait: Arc<Wait>,
    watched: bool,
    escalated: bool,
}

/// What the fold keeps per symbol. A feed serves ONE venue, so the symbol is the key.
#[derive(Default)]
struct Held {
    /// The folded book.
    books: HashMap<String, Arc<L2Book>>,
    /// Symbols whose book lane was holed (or whose deltas can no longer be applied) and have not been
    /// re-anchored since, with the stamp the gap opened at (the `Live` that ends the wait echoes it).
    holed: HashMap<String, Hole>,
}

/// What one update folded to.
struct Folded {
    /// The whole book to hand the engine, when there is one to say.
    book: Option<Arc<L2Book>>,
    /// The hole an anchor just ended: the `Live` to send after the book.
    resumed: Option<i64>,
    /// A wait for an anchor this update opened (the stamp): the `Stale` to send.
    stale: Option<i64>,
}

/// Whether two books hold the same grid and the same levels (their sequence aside).
fn same_book(a: &L2Book, b: &L2Book) -> bool {
    a.tick_size.to_bits() == b.tick_size.to_bits() && a.top_n(usize::MAX) == b.top_n(usize::MAX)
}

/// See the module doc.
pub(super) struct PumpBookFold {
    inner: Arc<dyn LiveDataSink>,
    held: Arc<Mutex<Held>>,
    /// How long a hole waits for its anchor before it escalates ([`ANCHOR_DEADLINE`]; a test narrows it).
    deadline: Duration,
}

impl PumpBookFold {
    /// A fold in front of `inner`.
    pub(super) fn new(inner: Arc<dyn LiveDataSink>) -> Self {
        Self::with_deadline(inner, ANCHOR_DEADLINE)
    }

    /// [`PumpBookFold::new`] with another deadline: a test's tool.
    pub(super) fn with_deadline(inner: Arc<dyn LiveDataSink>, deadline: Duration) -> Self {
        Self { inner, held: Arc::new(Mutex::new(Held::default())), deadline }
    }

    /// Fold `update` into the held book for `symbol`; the book to forward, or `None` when there is
    /// nothing to say (a hole, a duplicate, a marker).
    fn fold(&self, symbol: &str, update: &BookUpdate) -> Folded {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let Held { books, holed } = &mut *held;
        let nothing = Folded { book: None, resumed: None, stale: None };
        match update.kind {
            BookUpdateKind::Snapshot => {
                let mut book = L2Book::new(update.tick_size);
                book.apply_snapshot(update.seq, &update.bids, &update.asks);
                let book = Arc::new(book);
                // An anchor that restates the book the engine already holds (the sender's periodic anchor of
                // a QUIET book, every ~2 s) is not forwarded again: the strategy's `on_order_book` would
                // re-price off an identical book (`SpreadMaker::requote` re-amends its resting quote on every
                // call, with the refresh tolerance off), and the core's liveness marks would count a book
                // nobody sent. The folded book is replaced all the same (its sequence moves on).
                let unchanged = books.get(symbol).is_some_and(|held| same_book(held, &book));
                books.insert(symbol.to_string(), Arc::clone(&book));
                let resumed = holed.remove(symbol).map(|hole| {
                    hole.wait.finish();
                    hole.at_ts_ms
                });
                // The anchor that ENDS a hole is always forwarded: its `Live` follows the book.
                let forward = !unchanged || resumed.is_some();
                Folded { book: forward.then_some(book), resumed, stale: None }
            }
            BookUpdateKind::Delta => {
                let Some(book) = books.get_mut(symbol) else {
                    // The first message after a link loss is an anchor; a delta ahead of it is the
                    // tail of a book we no longer hold.
                    tracing::debug!(
                        symbol,
                        "pump book: a delta with no anchor held; waiting for one"
                    );
                    return nothing;
                };
                if book.tick_size != update.tick_size {
                    tracing::warn!(
                        symbol,
                        held = book.tick_size,
                        delta = update.tick_size,
                        "pump book: a delta on another price grid than the held book cannot be \
                         applied; the book is dropped and the next anchor re-seeds it"
                    );
                    books.remove(symbol);
                    // The engine still holds the book from before: it is stale until the anchor, and
                    // the status stream says so (nothing else would).
                    return Folded { stale: Self::begin_wait(holed, symbol, now_ms()), ..nothing };
                }
                // `make_mut` clones only while the core still holds the previous book, which is the
                // usual case: one clone per update, as the pump's own `Arc::new(book.clone())` was.
                let folded = Arc::make_mut(book);
                if !folded.apply_delta(update.seq, &update.bids, &update.asks) {
                    return nothing;
                }
                Folded { book: Some(Arc::clone(book)), resumed: None, stale: None }
            }
            BookUpdateKind::GapStart => {
                books.remove(symbol);
                nothing
            }
            BookUpdateKind::Stale | BookUpdateKind::LiveResume => nothing,
        }
    }

    /// Open a wait for the next anchor on `symbol`'s book, stamped `at_ts_ms`: `Some(stamp)` when this
    /// call opened it (the engine is told its book is STALE), `None` when one was open already (its first
    /// stamp stands, and nothing is said twice).
    fn begin_wait(holed: &mut HashMap<String, Hole>, symbol: &str, at_ts_ms: i64) -> Option<i64> {
        if holed.contains_key(symbol) {
            return None;
        }
        holed.insert(
            symbol.to_string(),
            Hole { at_ts_ms, wait: Wait::new(), watched: false, escalated: false },
        );
        Some(at_ts_ms)
    }

    /// Tell the engine `symbol`'s book is stale since `at_ts_ms`. A `Stale` and never a `GapStart`: the
    /// module doc says why.
    fn say_stale(&self, venue: &str, symbol: &str, at_ts_ms: i64) {
        self.inner.stream_status(
            venue,
            symbol,
            StreamLabel::Book.as_str(),
            StreamStatus::Stale { newest_data_ts_ms: at_ts_ms, now_ms: now_ms() },
        );
    }

    /// Start the watcher of `symbol`'s open hole, once: it waits [`Self::deadline`] for the anchor and, when none
    /// came, escalates (the module doc). A thread that cannot be spawned is said at `error` and the hole simply
    /// has no deadline, as it had none before.
    fn watch(&self, venue: &str, symbol: &str) {
        let wait = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            match held.holed.get_mut(symbol) {
                Some(hole) if !hole.watched => {
                    hole.watched = true;
                    Arc::clone(&hole.wait)
                }
                _ => return,
            }
        };
        let inner = Arc::clone(&self.inner);
        let held = Arc::clone(&self.held);
        let deadline = self.deadline;
        let (venue, symbol_owned) = (venue.to_string(), symbol.to_string());
        let spawned =
            std::thread::Builder::new().name("pump-book-wait".to_string()).spawn(move || {
                let guard = wait.over.lock().unwrap_or_else(PoisonError::into_inner);
                let (guard, _timed_out) = wait
                    .cv
                    .wait_timeout_while(guard, deadline, |over| !*over)
                    .unwrap_or_else(PoisonError::into_inner);
                let anchored = *guard;
                drop(guard);
                if anchored {
                    return;
                }
                // The deadline passed with the hole still the one this thread watches.
                //
                // ⚠ The decision AND the send are ONE critical section: the lock is held across
                // `stream_status`. The reader thread sends an anchor's `Live` only after its own `fold` (the
                // same lock) has removed the hole, so the two orders that can happen are "the anchor folded
                // first: no hole here, nothing is sent" and "this section first: the `GapStart` is sent, THEN
                // the anchor's fold and its `Live`". Releasing the lock between the decision and the send let
                // an anchor land in the gap, and the `GapStart` then reached the core AFTER the `Live` that
                // should have closed it: a connection-state dead-man window nothing closes, and 120 s later
                // `CancelAllAndHalt` on a healthy feed (the HALT sentinel does not self-clear).
                let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
                let escalate = match held.holed.get_mut(&symbol_owned) {
                    Some(hole) if Arc::ptr_eq(&hole.wait, &wait) && !hole.escalated => {
                        hole.escalated = true;
                        Some(hole.at_ts_ms)
                    }
                    _ => None,
                };
                if let Some(at_ts_ms) = escalate {
                    tracing::error!(
                        venue = %venue,
                        symbol = %symbol_owned,
                        waited_s = deadline.as_secs_f64(),
                        "the {symbol_owned} book on {venue} has had NO anchor since its hole opened: the \
                         book the strategies hold is stale and nothing is correcting it; telling the \
                         engine Disconnected (a connection-state dead-man window opens, closed by the \
                         anchor's Live)"
                    );
                    inner.stream_status(
                        &venue,
                        &symbol_owned,
                        StreamLabel::Book.as_str(),
                        StreamStatus::GapStart { at_ts_ms },
                    );
                }
                drop(held);
            });
        if let Err(e) = spawned {
            tracing::error!(
                error = %e,
                "the pump book's anchor-deadline watcher could not be started; this hole has no deadline"
            );
        }
    }

    /// Whether the book `status` on `stream` leaves behind is no longer the pump's.
    fn drops_book(stream: &str, status: &StreamStatus) -> bool {
        matches!(status, StreamStatus::GapStart { .. })
            && (stream == LINK_STREAM_WORD || stream == STRAT_LINK_STREAM)
    }
}

/// **A fold that is dropped (its feed shut down) ends every open wait**: the detached watcher threads wake at
/// once, find no hole, and leave without saying anything, instead of sleeping out the rest of the deadline and
/// then telling the engine a book nobody holds any more is `Disconnected` (a dead-man window for a key that is
/// unsubscribed has no closing `Live`).
impl Drop for PumpBookFold {
    fn drop(&mut self) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        for (_, hole) in held.holed.drain() {
            hole.wait.finish();
        }
    }
}

impl LiveDataSink for PumpBookFold {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.inner.seed_bars(venue, symbol, interval, bars);
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.close_bar(venue, symbol, interval, bar);
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.inner.forming_bar(venue, symbol, interval, bar);
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.mark_tick(venue, symbol, px, ts);
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.inner.bar_close_tick(venue, symbol, px, ts);
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.inner.quote(venue, symbol, quote);
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.inner.trade(venue, symbol, trade);
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.inner.book(venue, symbol, book);
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        let Folded { book, resumed, stale } = self.fold(symbol, &update);
        if let Some(at) = stale {
            self.say_stale(venue, symbol, at);
            self.watch(venue, symbol);
        }
        if let Some(book) = book {
            self.inner.book(venue, symbol, book);
        }
        // The anchor is in the engine: only now is the book the engine holds a consistent one, so only
        // now does the lane read `Live` again.
        if let Some(at) = resumed {
            self.inner.stream_status(
                venue,
                symbol,
                StreamLabel::Book.as_str(),
                StreamStatus::Live { gap_started_ts_ms: Some(at) },
            );
        }
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.inner.l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        if Self::drops_book(stream, &status) {
            self.held.lock().unwrap_or_else(PoisonError::into_inner).books.remove(symbol);
        } else if stream == StreamLabel::Book.as_str() {
            // The book lane's own hole disclosure (the module doc): a `GapStart` is the hole, and the
            // `Live` after it is withheld until an anchor has landed.
            match status {
                StreamStatus::GapStart { at_ts_ms } => {
                    let began = {
                        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
                        held.books.remove(symbol);
                        Self::begin_wait(&mut held.holed, symbol, at_ts_ms)
                    };
                    if let Some(at) = began {
                        self.say_stale(venue, symbol, at);
                        self.watch(venue, symbol);
                    }
                    return;
                }
                StreamStatus::Live { .. }
                    if self
                        .held
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .holed
                        .contains_key(symbol) =>
                {
                    return;
                }
                _ => {}
            }
        }
        self.inner.stream_status(venue, symbol, stream, status);
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        self.inner.data_delay(venue, symbol, stream, delay);
    }
}

#[cfg(test)]
#[path = "pump_book_tests.rs"]
mod pump_book_tests;
