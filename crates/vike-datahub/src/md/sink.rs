//! [`MdHubSink`] — the ONE [`LiveDataSink`] every venue client this hub builds is constructed with.
//!
//! One instance, shared, and that is legal because every `LiveDataSink` verb carries `venue` and
//! `symbol`: the sink routes on those rather than on which client called it.
//!
//! # Every verb is WAIT-FREE (§6.1)
//!
//! Replace a latest-wins slot, or `push_back` on a bounded deque, and set a dirty bit. **No
//! serialization, no socket, no blocking send, no unbounded queue.** This is
//! `crates/vike-data/src/live_rec.rs`'s `RecorderSink` and `crates/bridges/polymarket/src/raw_tap.rs`'s
//! `RawTap` idiom verbatim — *"losing a frame is acceptable; affecting the live feed is not"* —
//! except that here a loss is COUNTED and DISCLOSED.
//!
//! # ⚠ EVERY DEFAULTED VERB IS SPELLED OUT, INCLUDING THE ONES THAT DO NOTHING
//!
//! `crates/vike-data/src/live.rs`'s `LiveDataSink` splits into REQUIRED methods (which cannot be
//! forgotten) and DEFAULTED ones (which can). There are **four** defaults, not the three §6.1 names:
//! `bar_close_tick`, `l2_snapshot`, `book_update` and `stream_status`. Forgetting one is a one-line
//! omission that compiles, passes every type check, and produces *"a subscription that connects,
//! seeds, validates checksums and reports healthy while delivering nothing"* — §6.1's own documented
//! failure, and the exact thing `crates/vike-data/src/live.rs`'s `TeeSink` carries in-source comments
//! about after a missing override silently dropped every depth snapshot.
//!
//! So every one is written out here WITH ITS REASON, never inherited.
//!
//! # ⚠ `quote` returns BEFORE any lookup, and that is not micro-optimisation
//!
//! §4.4 refuses a quotes lane because `GuiFeedSink::quote` is itself a no-op — but polymarket's
//! `subscribe_book` **derives** L1 quotes (the reason `crates/vike-recorder/src/runtime.rs`'s
//! `VenueFeed::narrow` hook exists), and the store holds 329M polymarket quote rows over 40 days. So
//! this is a HOT no-op on the busiest venue in the store, and a map lookup inside it would be pure
//! cost on a feed thread.

use std::sync::{Arc, Weak};

use vike_data::live::{LiveDataSink, StreamStatus};
use vike_datahub_client::market::{MdLane, WireStreamStatus};
use vike_model::{Bar, BookUpdate, L2Book, Level, QuoteTick, TradeTick};

use super::hub::{BookState, MdHub, push_trade, store_book, store_status};

/// The hub's sink. Holds a [`Weak`] rather than an [`Arc`] deliberately: the hub owns the venue
/// clients, the clients hold this sink, and an `Arc` back to the hub would be a cycle that never
/// frees. It is constructed inside `Arc::new_cyclic`, so the weak handle is valid from the first
/// callback onward.
pub struct MdHubSink {
    hub: Weak<MdHub>,
}

impl std::fmt::Debug for MdHubSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MdHubSink")
    }
}

impl MdHubSink {
    pub(crate) fn new(hub: Weak<MdHub>) -> Self {
        MdHubSink { hub }
    }

    fn hub(&self) -> Option<Arc<MdHub>> {
        self.hub.upgrade()
    }
}

impl LiveDataSink for MdHubSink {
    // ---- SERVED ------------------------------------------------------------------------------

    /// **SERVED — the [`MdLane::Depth`] lane.** The CONFLATING L2 snapshot verb.
    ///
    /// The `Arc<BookState>` is built OUTSIDE the slot lock and moved in, so the feed thread holds
    /// the entry's lock for a pointer swap rather than for a copy of two vectors.
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<Level>,
        asks: Vec<Level>,
        ts: i64,
    ) {
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, MdLane::Depth) else { return };
        store_book(&entry, BookState { tick_size, bids, asks, venue_ts: ts, venue_seq: 0 });
    }

    /// **SERVED — the [`MdLane::Book`] lane.** The FOLDED-STATE verb.
    ///
    /// ⚠ **`venue_ts` is the HUB's own receipt clock here, and it cannot be anything else.**
    /// `LiveDataSink::l2_snapshot` carries a `ts`; this verb carries `Arc<L2Book>` and nothing else,
    /// and `crates/vike-model/src/orderbook.rs`'s `L2Book` has `tick_size` and `last_seq` and **no
    /// timestamp field at all**. §4.4 declares one field for both lanes and calls it "the venue/feed
    /// stamp"; on this lane that would be false, so `BookSnapshot::venue_ts`'s own doc states the
    /// per-lane truth rather than letting the field lie. It costs nothing: §7.4 already makes the
    /// CLIENT's receipt authoritative for staleness and this field diagnostic.
    ///
    /// `top_n` is called OUTSIDE the slot lock, and it is also what fixes the level ORDER the wire
    /// promises — bids descending, asks ascending, best first.
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, MdLane::Book) else { return };
        let (bids, asks) =
            book.top_n(vike_datahub_client::market::MD_DEPTH_LEVELS_CEILING as usize);
        store_book(
            &entry,
            BookState {
                tick_size: book.tick_size,
                bids,
                asks,
                venue_ts: vike_model::now_ms(),
                venue_seq: book.last_seq,
            },
        );
    }

    /// **SERVED — the [`MdLane::Trades`] lane.** Bounded push, evict-oldest, and every eviction is
    /// COUNTED so it can be DISCLOSED as a `TapeGap` rather than silently corrupting a client's CVD.
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, MdLane::Trades) else { return };
        push_trade(&entry, trade);
    }

    /// **SERVED — the disclosure lane**, forwarded verbatim to whichever lane the feed named.
    ///
    /// ⚠ The `stream` label is a bare string with TWO independent producers
    /// (`crates/bridges/binance/src/family/market_feed.rs` passes the literal `"depth"`;
    /// `crates/bridges/polymarket/src/market_feed.rs` passes `PumpTiming`'s `stream_label`), and
    /// `crates/vike-data/src/live_rec.rs`'s `RecorderSink::stream_status` already accepts that
    /// brittleness with `if stream != "book" { return; }`. §12.5 measured what a missed lane label
    /// costs: a binance depth series that was a reconnect artefact for FORTY DAYS with no marker in
    /// the store, because the disclosure never reached it. So the mapping is
    /// `MdLane::from_feed_stream_label` — one function, pinned by a test, naming both producers in
    /// its doc — and a label this wire serves no lane for returns BEFORE any lock.
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        let Some(lane) = MdLane::from_feed_stream_label(stream) else { return };
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, lane) else { return };
        store_status(&entry, WireStreamStatus::from(status));
    }

    // ---- SPELLED NO-OPS ----------------------------------------------------------------------

    /// **SPELLED no-op.** §10 serves no bar lane, so a seed has nowhere to land.
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        let _ = (venue, symbol, interval, bars);
    }

    /// **SPELLED no-op** — no bar lane.
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        let _ = (venue, symbol, interval, bar);
    }

    /// **SPELLED no-op** — no bar lane.
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        let _ = (venue, symbol, interval, bar);
    }

    /// **SPELLED no-op** — `mark_tick` fills a `PriceBoard`, and there is no core in this process to
    /// hold one.
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        let _ = (venue, symbol, px, ts);
    }

    /// **SPELLED no-op**, and one of the four DEFAULTS — written out because a default here is
    /// exactly the omission that produces a healthy-looking feed delivering nothing. Same reason as
    /// `mark_tick`: no `PriceBoard`, no bar lane.
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        let _ = (venue, symbol, px, ts);
    }

    /// **SPELLED no-op**, a DEFAULT. §10 serves no DELTA lane — this verb is `vike-recorder`'s, the
    /// lossless twin `RecorderSink` persists. This wire transfers STATE (§7.1), so a delta has
    /// nowhere to go and forwarding it would be a second gap vocabulary competing with `Status`.
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        let _ = (venue, symbol, update);
    }

    /// **SPELLED no-op, AND IT RETURNS FIRST — see the module doc.** This is the hot one.
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        let _ = (venue, symbol, quote);
    }
}
