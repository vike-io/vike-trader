//! The feed-object types the daemon holds from mount to teardown.
//!
//! Every price feed this daemon holds is a dial to the datahub's STRATEGY LANE
//! (`vike_datahub_client::strat::StratFeed`, decision 0164 verdict 2): the venue sockets are the
//! gatewayhub's, and the datahub carries them to this process over the tube. What lives here is only
//! what the daemon must hold to stop them in the right order: the feed, and the subscriptions it took on
//! it.
//!
//! # Release before shutdown
//!
//! A feed that is shut down with its subscriptions still held leaves the strat-link gaps it disclosed
//! open (`StratFeed`'s link-loss rule: a `Disconnected` is always closed by a `Live` from the same source
//! or by process exit). Every feed here therefore **unsubscribes what it holds, then shuts down**
//! ([`Tracked`], [`CexFeed`]): a gap that is open when the daemon stops is closed by that last unsubscribe
//! rather than left for the dead-man to count.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, StreamLabel, SubscriptionId};
use vike_datahub_client::strat::StratFeed;

#[cfg(doc)]
use super::{FeedCtors, wire_venue_feeds};

/// A [`DataClient`] that releases every subscription it handed out before it shuts down.
///
/// It forwards every verb to the feed it wraps and remembers the ids that came back `Ok`; `unsubscribe`
/// forgets one; [`DataClient::shutdown`] unsubscribes what is left, THEN shuts the feed down. That is the
/// whole of it: it exists so the teardown order does not depend on each arm remembering its ids.
pub struct Tracked {
    inner: Box<dyn DataClient + Send>,
    ids: Vec<SubscriptionId>,
    /// Whether [`DataClient::shutdown`] ran: a feed dropped WITHOUT it (an arm that met a refusal after its first
    /// subscription and returned `Err`) is shut down in order by `Drop`; one that was shut down is not again.
    closed: bool,
}

impl Tracked {
    /// Track `inner`'s subscriptions.
    pub fn new(inner: Box<dyn DataClient + Send>) -> Self {
        Self { inner, ids: Vec::new(), closed: false }
    }

    fn keep(
        &mut self,
        r: Result<SubscriptionId, LiveDataError>,
    ) -> Result<SubscriptionId, LiveDataError> {
        if let Ok(id) = &r {
            self.ids.push(*id);
        }
        r
    }
}

impl DataClient for Tracked {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_bars(symbol, interval);
        self.keep(r)
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_quotes(symbol);
        self.keep(r)
    }
    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_trades(symbol);
        self.keep(r)
    }
    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_book(symbol);
        self.keep(r)
    }
    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        let r = self.inner.subscribe_depth(symbol);
        self.keep(r)
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.ids.retain(|held| *held != id);
        self.inner.unsubscribe(id);
    }
    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        self.inner.set_delivery(symbol, lane, class)
    }
    fn is_dead(&self, id: SubscriptionId) -> bool {
        self.inner.is_dead(id)
    }
    fn begin_shutdown(&mut self) {
        self.inner.begin_shutdown();
    }
    fn shutdown(&mut self) {
        self.closed = true;
        for id in std::mem::take(&mut self.ids) {
            self.inner.unsubscribe(id);
        }
        self.inner.shutdown();
    }
}

impl Drop for Tracked {
    /// **A partial feed is released in order too.** A feed arm that refuses on its second subscription drops the
    /// feed it built (`?` returns the `Err`); without this it was torn down by the feed's own `Drop` alone, which
    /// closes the strat-link gaps but unsubscribes nothing in the order the daemon keeps.
    fn drop(&mut self) {
        if !self.closed {
            DataClient::shutdown(self);
        }
    }
}

/// The CEX feed of one venue: ONE strategy-lane dial carrying both lanes the maker needs, and the
/// subscriptions it took on each, so they are released in the order the daemon has always stopped them.
///
/// ⚠ **Two lanes, and the pair is the whole point of the CEX arm.** binance/bybit/okx/aster declare
/// `live_data.quotes = false` and `live_data.book = false` (`vike_venues::venue_caps`), so
/// `DataClient::subscribe_quotes` is a caps-driven refusal and the hyperliquid arm's model cannot be
/// copied. The obvious substitute, `subscribe_depth`, is a SILENT NO-OP: it emits
/// `LiveDataSink::l2_snapshot`, whose body is the trait's default and which NEITHER sink this daemon owns
/// overrides (`vike_tradehub_core::CoreLaneSink` says so in its module doc). A depth-wired maker connects,
/// seeds, validates checksums, reports healthy, and posts zero orders forever, because
/// `vike_mm::SpreadMaker` reaches `requote` from exactly `on_quote_tick` and `on_order_book`.
///
/// * **BARS** (`subscribe_bars`) are the cadence of the account-wide margin-call watchdog and the
///   equity-drawdown latch (`CoreConfig::margin_call` / `max_drawdown`), which fire per CLOSED bar and
///   nowhere else. A quote-only CEX mount would run a live account with BOTH safety watchdogs dead.
/// * **TICKS** (`StratFeed::subscribe_ticks`, the tube's `SubOp::Ticks`) are the venue's quote/trade/book
///   pump: the lane the maker acts on, and this daemon's ONLY link-death signal for a CEX venue (the pump
///   discloses its own transport state, which arms the CONNECTION-state dead-man;
///   `crates/vike-tradehub/src/tradehub_cli/tests/deadman.rs`'s
///   `the_cex_arm_subscribes_the_tick_pump_that_reports_a_dead_link` is the gate that refuses to let that
///   happen quietly). Its book arrives as an anchor plus deltas and is folded back into the book the
///   engine always saw by `feeds/pump_book.rs`.
///
/// Every CEX venue here PUBLISHES a quote and drives BOTH maker verbs; what differs is only where the
/// quote comes from ([`crate::CexVenue::quote_source`]).
pub struct CexFeed {
    client: StratFeed,
    bars: Vec<SubscriptionId>,
    ticks: Vec<SubscriptionId>,
    /// Whether [`CexFeed::shutdown`] ran (see [`Tracked`]'s field of the same name).
    closed: bool,
}

impl CexFeed {
    /// A CEX feed over `client`, holding nothing yet.
    pub fn new(client: StratFeed) -> Self {
        Self { client, bars: Vec::new(), ticks: Vec::new(), closed: false }
    }

    /// Subscribe the BAR lane of `symbol` at `interval`.
    pub(super) fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<(), LiveDataError> {
        self.bars.push(self.client.subscribe_bars(symbol, interval)?);
        Ok(())
    }

    /// Subscribe the tick pump of `symbol`, its book on a `tick_size` grid.
    pub(super) fn subscribe_ticks(
        &mut self,
        symbol: &str,
        tick_size: f64,
    ) -> Result<(), LiveDataError> {
        self.ticks.push(self.client.subscribe_ticks(symbol, tick_size)?);
        Ok(())
    }

    /// Whether the strategy-lane session is open and acknowledged right now.
    pub fn is_up(&self) -> bool {
        self.client.is_up()
    }

    /// Release the pump FIRST (the lane the strategy acts on, so no quote can arrive for a bar window
    /// that is already closing), then the bars, then close the dial.
    pub fn shutdown(&mut self) {
        self.closed = true;
        for id in std::mem::take(&mut self.ticks) {
            self.client.unsubscribe(id);
        }
        for id in std::mem::take(&mut self.bars) {
            self.client.unsubscribe(id);
        }
        self.client.shutdown();
    }
}

impl Drop for CexFeed {
    /// A CEX feed whose pump subscription was refused after its kline subscription succeeded is released in
    /// the order a shutdown keeps (pump first, then bars, then the dial), not just dropped.
    fn drop(&mut self) {
        if !self.closed {
            self.shutdown();
        }
    }
}

/// ONE live venue's market feed — one variant per live-wired venue, so the daemon's teardown stays
/// venue-agnostic. Every variant is a dial to the datahub's strategy lane, and each releases its
/// subscriptions before it closes ([`Tracked`], [`CexFeed`]).
///
/// The daemon holds a COLLECTION of these — one entry per DISTINCT mounted venue, in mount order
/// (`live_runtime`'s `feeds` vec; a single-mount profile holds one) — and each venue's feed arm is wired
/// exactly once however many mounts share the venue.
pub enum LiveFeeds {
    /// Hyperliquid: bars + quotes, on the network the venue's armed accounts' exec dials.
    Hyperliquid(Tracked),
    /// One feed (its own dial + `MakerSink` bar synth) per DISTINCT mounted token: a `MakerSink` folds
    /// EVERY price it sees into ITS token's synth bars, so two tokens through one sink would corrupt both
    /// mounts' paper-fill bars — the per-token split is correctness, not tidiness. (Two mounts on ONE token
    /// share one entry; a same-token interval mismatch is refused before the core spawns — see
    /// `live_runtime`'s planning pass.)
    Polymarket(Vec<Tracked>),
    /// binance/bybit/okx/aster: the tick pump AND the kline lane, both required (see [`CexFeed`] for why
    /// neither alone is a working mount).
    Cex(CexFeed),
    /// alpaca (split-plane I9): the ONE multiplexed, OAuth-authenticated market-data client — real 1m bar
    /// closes + quotes + trades through `CoreLaneSink`. A credentialed stream at the gatewayhub.
    Alpaca(Tracked),
    /// ctrader (split-plane I9): quotes over the gatewayhub's dedicated data socket. Closed bars are
    /// SYNTHESIZED from quote mids by the [`vike_tradehub_core::MakerSink`] the feed delivers into — the
    /// socket itself never emits a bar close.
    Ctrader(Tracked),
    /// oanda (split-plane I9): quotes off the chunked-HTTP `/pricing/stream` line stream, bars POLLED off
    /// the candles REST endpoint, both Bearer-authed against one account. Held behind the [`DataClient`]
    /// seam since the arm was routed through [`FeedCtors`]: production stores the strategy-lane dial, the
    /// deterministic splice test stores its scripted double.
    Oanda(Tracked),
    /// deribit (split-plane I9): the widest feed this daemon mounts — FOUR live verbs (bars, quotes,
    /// trades, book) off the venue's KEYLESS public MAINNET host. The book verb here is the LOSSLESS
    /// `LiveDataSink::book` lane (which `vike_tradehub_core::CoreLaneSink` really forwards) rather than the
    /// conflating `l2_snapshot` one the CEX arm has to route around. Held behind the [`DataClient`] seam
    /// like oanda's.
    Deribit(Tracked),
    /// ig (split-plane I9): the NARROWEST live feed this daemon mounts — TWO verbs (`subscribe_quotes` +
    /// `subscribe_bars`) and no more: trades and book are caps refusals, because a DEALER venue publishes
    /// its own two-sided price and no public tape or ladder exists behind them.
    Ig(Tracked),
}

impl LiveFeeds {
    /// The per-venue feed-status handles for the reconcile HEALTH GATE — EMPTY for every venue, since
    /// wave 2.
    ///
    /// The CEX row was the one row this map ever carried: it keyed on the bridge's `Feeds::status` string
    /// (`vike_bybit::market_feed`'s `LIVE_STATUS` and its siblings), a handle this process no longer
    /// has — the venue sockets are the gatewayhub's. The health gate's evidence for a CEX venue is the
    /// gatewayhub's own (`ReconNotReady{Degraded}`, the recon task), and an absent row reads `Healthy`
    /// here, the fail-soft direction the gate always argued for: it can only ever SUPPRESS a pass.
    ///
    /// ⚠ Until that lands, a CEX venue's reconcile pass is no longer suppressed by a degraded feed
    /// string. The function stays, returning the map's type, only so `live_runtime`'s one caller and
    /// `reconcile_config::build_recon_config`'s parameter keep compiling; they leave with the recon task.
    pub fn recon_feed_statuses(&self) -> HashMap<String, Arc<std::sync::Mutex<String>>> {
        HashMap::new()
    }

    pub fn shutdown(&mut self) {
        match self {
            LiveFeeds::Hyperliquid(f)
            | LiveFeeds::Alpaca(f)
            | LiveFeeds::Ctrader(f)
            | LiveFeeds::Oanda(f)
            | LiveFeeds::Deribit(f)
            | LiveFeeds::Ig(f) => f.shutdown(),
            LiveFeeds::Polymarket(list) => {
                for f in list {
                    f.shutdown();
                }
            }
            LiveFeeds::Cex(f) => f.shutdown(),
        }
    }
}

/// The MERGED per-venue feed-status map across every mounted venue's feed (split-plane I10) — what the
/// reconcile health gate takes now that `live_runtime` wires N venues. EMPTY since wave 2: see
/// [`LiveFeeds::recon_feed_statuses`].
pub fn recon_feed_statuses_of(
    feeds: &[LiveFeeds],
) -> HashMap<String, Arc<std::sync::Mutex<String>>> {
    let mut merged = HashMap::new();
    for f in feeds {
        merged.extend(f.recon_feed_statuses());
    }
    merged
}

/// The LIVE arm's extra teardown handles (there are none in the paper arm). Held from mount to the
/// bounded shutdown so the venue feed threads are stopped+joined and the live-event forwarder is
/// quiesced in the load-bearing order (forwarder-stop BEFORE the core shutdown+join — see
/// `crates/vike-tradehub-core/src/node.rs`'s forwarder teardown-safety note).
/// Extra teardown run AFTER `feeds.shutdown()` and BEFORE the core join — the recorder's bounded flush
/// when `record-feeds` + the `flags.tradehub_record` row, else `None` (byte-identical to no recording).
/// ⚠ Always `None` since #2093 deleted `record-feeds` (2026-09-22): the slot stays so the teardown
/// tail keeps its shape (`crates/vike-tradehub/src/tradehub_cli.rs`'s `post_feeds`). Boxed
/// (and aliased) so the struct never names the feature-gated `RecorderHandle` type, and to keep the
/// recorder-open tuple under clippy's `type_complexity` bar.
pub type PostFeeds = Option<Box<dyn FnOnce() + Send>>;

pub struct LiveTeardown {
    /// One entry per mounted venue's feed (split-plane I10) — a single-mount daemon holds one.
    /// The teardown fans these out as one bounded task EACH, so every venue's feed quiesces
    /// inside the ONE shutdown deadline and a wedged venue cannot serialize its siblings.
    pub feeds: Vec<LiveFeeds>,
    pub forwarder_stop: Arc<AtomicBool>,
    pub post_feeds: PostFeeds,
    /// The reconciliation driver, `Some` when the S2 reconcile gate said yes AND ≥1 live venue
    /// produced a ReconClient (`None` on a paper mount and on any run the operator refused). Shut
    /// down in the SEQUENTIAL tail BEFORE the core join (the teardown order vike-app's local core
    /// used): it holds only a WEAK core-ingest sender so it can never wedge the core, but joining it
    /// there bounds its `vt-core-recon` thread to the daemon's lifetime rather than leaking it.
    pub recon_driver: Option<vike_tradehub_core::ReconDriver>,
}
