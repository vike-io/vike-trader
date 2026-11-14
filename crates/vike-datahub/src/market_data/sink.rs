//! [`DataClientSinkMarketDataHub`] — the ONE [`DataClientSink`] every venue client this hub builds is constructed with.
//!
//! One instance, shared, and that is legal because every `DataClientSink` verb carries `venue` and
//! `symbol`: the sink routes on those rather than on which client called it.
//!
//! # Every verb is WAIT-FREE (§6.1)
//!
//! Replace a latest-wins slot, or `push_back` on a bounded deque, and set a dirty bit. **No
//! serialization, no socket, no blocking send, no unbounded queue.** This is
//! `crates/vike-store/src/live_rec.rs`'s `RecorderSink` and `crates/bridges/polymarket/src/feed/tap.rs`'s
//! `RawTap` idiom verbatim — *"losing a frame is acceptable; affecting the live feed is not"* —
//! except that here a loss is COUNTED and DISCLOSED.
//!
//! # ⚠ EVERY DEFAULTED VERB IS SPELLED OUT, INCLUDING THE ONES THAT DO NOTHING
//!
//! `crates/vike-data/src/live.rs`'s `DataClientSink` splits into REQUIRED methods (which cannot be
//! forgotten) and DEFAULTED ones (which can). There are **five** defaults, not the three §6.1 names:
//! `bar_close_tick`, `l2_snapshot`, `book_update`, `stream_status` and `data_delay` (decision 0140).
//! Forgetting one is a one-line omission that compiles, passes every type check, and produces *"a
//! subscription that connects, seeds, validates checksums and reports healthy while delivering
//! nothing"* — §6.1's own documented
//! failure, and the exact thing `crates/vike-data/src/live.rs`'s `DataClientSinkTee` carries in-source comments
//! about after a missing override silently dropped every depth snapshot.
//!
//! So every one is written out here WITH ITS REASON, never inherited.
//!
//! # ⚠ `quote` returns BEFORE any lookup unless a quote key exists, and that is not micro-optimisation
//!
//! polymarket's `subscribe_book` **derives** L1 quotes (the reason `crates/vike-recorder/src/runtime.rs`'s
//! `VenueFeed::narrow` hook exists), and the store holds 329M polymarket quote rows over 40 days. So
//! this verb is HOT on the busiest venue in the store, and a map lookup per call would be pure cost on
//! a feed thread for a box where no window holds a quote key. One atomic answers that box.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Weak};

use vike_data::DataClientSink;
use vike_marketdata::{Bar, L2Book, L2BookLevel, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamLabel, StreamStatus};

use super::hub::{
    MarketDataBookState, MarketDataHub, MarketDataQuoteState, push_trade, store_book, store_delay,
    store_quote, store_status,
};

/// The hub's sink. Holds a [`Weak`] rather than an [`Arc`] deliberately: the hub owns the venue
/// clients, the clients hold this sink, and an `Arc` back to the hub would be a cycle that never
/// frees. It is constructed inside `Arc::new_cyclic`, so the weak handle is valid from the first
/// callback onward.
pub struct DataClientSinkMarketDataHub {
    hub: Weak<MarketDataHub>,
    /// How many quote-lane entries the hub holds: `quote`'s first question (the module doc).
    quote_keys: AtomicU32,
}

impl std::fmt::Debug for DataClientSinkMarketDataHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DataClientSinkMarketDataHub")
    }
}

impl DataClientSinkMarketDataHub {
    pub(crate) fn new(hub: Weak<MarketDataHub>) -> Self {
        DataClientSinkMarketDataHub { hub, quote_keys: AtomicU32::new(0) }
    }

    pub(crate) fn quote_key_added(&self) {
        self.quote_keys.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn quote_key_removed(&self) {
        self.quote_keys.fetch_sub(1, Ordering::AcqRel);
    }

    pub(crate) fn quote_keys(&self) -> u32 {
        self.quote_keys.load(Ordering::Acquire)
    }

    fn hub(&self) -> Option<Arc<MarketDataHub>> {
        self.hub.upgrade()
    }
}

impl DataClientSink for DataClientSinkMarketDataHub {
    // ---- SERVED ------------------------------------------------------------------------------

    /// **SERVED — the [`StreamLabel::Depth`] lane.** The CONFLATING L2 snapshot verb.
    ///
    /// The `Arc<BookState>` is built OUTSIDE the slot lock and moved in, so the feed thread holds
    /// the entry's lock for a pointer swap rather than for a copy of two vectors.
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<L2BookLevel>,
        asks: Vec<L2BookLevel>,
        ts: i64,
    ) {
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, StreamLabel::Depth) else { return };
        store_book(
            &entry,
            MarketDataBookState { tick_size, bids, asks, venue_timestamp: ts, venue_sequence: 0 },
        );
    }

    /// **SERVED — the [`StreamLabel::Book`] lane.** The FOLDED-STATE verb.
    ///
    /// ⚠ **`venue_timestamp` is the HUB's own receipt clock here, and it cannot be anything else.**
    /// `DataClientSink::l2_snapshot` carries a `ts`; this verb carries `Arc<L2Book>` and nothing else,
    /// and `crates/vike-marketdata/src/orderbook.rs`'s `L2Book` has `tick_size` and `last_sequence` and **no
    /// timestamp field at all**. §4.4 declares one field for both lanes and calls it "the venue/feed
    /// stamp"; on this lane that would be false, so `BookSnapshot::venue_timestamp`'s own doc states the
    /// per-lane truth rather than letting the field lie. It costs nothing: §7.4 already makes the
    /// CLIENT's receipt authoritative for staleness and this field diagnostic.
    ///
    /// `top_n` is called OUTSIDE the slot lock, and it is also what fixes the level ORDER the wire
    /// promises — bids descending, asks ascending, best first.
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, StreamLabel::Book) else { return };
        let (bids, asks) =
            book.top_n(vike_datahub_client::market::MARKET_DATA_DEPTH_LEVELS_CEILING as usize);
        store_book(
            &entry,
            MarketDataBookState {
                tick_size: book.tick_size,
                bids,
                asks,
                venue_timestamp: vike_model::now_ms(),
                venue_sequence: book.last_sequence,
            },
        );
    }

    /// **SERVED — the [`StreamLabel::Trades`] lane.** Bounded push, evict-oldest, and every eviction is
    /// COUNTED so it can be DISCLOSED as a `TapeGap` rather than silently corrupting a client's CVD.
    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, StreamLabel::Trades) else { return };
        push_trade(&entry, trade);
    }

    /// **SERVED — the disclosure lane**, forwarded verbatim to whichever lane the feed named.
    ///
    /// ⚠ The `stream` label is a bare string, and the producers that fed it by hand-typed literal
    /// (`crates/bridges/binance/src/family/feed/mod.rs` passed `"depth"`;
    /// `crates/bridges/polymarket/src/feed/mod.rs` passed `FeedTiming`'s `stream_label`) are why
    /// §12.5 measured what a missed lane label costs: a binance depth series that was a reconnect
    /// artefact for FORTY DAYS with no marker in the store, because the disclosure never reached it.
    /// Every producer now spells the word through `vike_model::StreamLabel::as_str`, and this sink
    /// maps it with `StreamLabel::from_label` — one function over that same vocabulary, whose
    /// exhaustive `match` fails to compile when a new label appears — so a label this wire serves
    /// no lane for (a bar interval) returns BEFORE any lock.
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        let Some(lane) = StreamLabel::from_label(stream) else { return };
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, lane) else { return };
        store_status(&entry, status);
    }

    /// **SERVED — the [`StreamLabel::Quotes`] lane**, latest-wins. It returns first on a box whose hub holds
    /// no quote key (the module doc), stamped with the hub's clock otherwise.
    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        if self.quote_keys() == 0 {
            return;
        }
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, StreamLabel::Quotes) else { return };
        store_quote(&entry, MarketDataQuoteState::of(&quote, vike_model::now_ms()));
    }

    /// **SERVED, a DEFAULT — the delay label (decision 0140)**, which rides the key's next quote
    /// frame. A feed labels its `quotes` lane today (IBKR's socket feed); on a lane of this wire that
    /// has no quote frame the label lands in a field nothing reads.
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        let Some(lane) = StreamLabel::from_label(stream) else { return };
        let Some(hub) = self.hub() else { return };
        let Some(entry) = hub.lookup(venue, symbol, lane) else { return };
        store_delay(&entry, delay);
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

    /// **SPELLED no-op**, and one of the five DEFAULTS — written out because a default here is
    /// exactly the omission that produces a healthy-looking feed delivering nothing. Same reason as
    /// `mark_tick`: no `PriceBoard`, no bar lane.
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        let _ = (venue, symbol, px, ts);
    }

    /// **SPELLED no-op**, a DEFAULT. §10 serves no DELTA lane — this verb is `vike-recorder`'s, the
    /// lossless twin `RecorderSink` persists. This wire transfers STATE (§7.1), so a delta has
    /// nowhere to go and forwarding it would be a second gap vocabulary competing with `Status`.
    fn book_update(&self, venue: &str, symbol: &str, update: L2BookUpdate) {
        let _ = (venue, symbol, update);
    }
}
