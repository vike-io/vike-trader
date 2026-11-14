//! The live Bybit V5 market data → the vike-data live seam: [`DataClientSpecBybit`], bybit's half of the
//! shared data-client engine (`vike_bridge_core::data_client::ws::DataClientWebsocket`, which implements
//! [`vike_data::DataClient`]) — klines, mark, trades and DOM depth over the venue's three public sockets
//! (spot, linear, inverse) — as distinct from [`crate::feed::tick`], the L2/tick HFT feed.
//!
//! One file per lane beside this root, each holding its decoder and its plan: `klines` (the REST warmup
//! seed, then the public `kline.<code>.<SYMBOL>` WS), `mark` (a `.P` perp bars subscription ALSO opens the
//! linear `tickers` stream feeding the REAL-mark verb `mark_tick`, default ON, the venue's `mark_streams`
//! row), `trades` and `depth`. The engine owns the rest: one STOPPABLE thread per subscription, the
//! session driver with this venue's `pump_spec` row (connect + subscribe replay, subscribe-ack watchdog,
//! read-timeout stop poll, stop-aware reconnect backoff), the status words (`LIVE · Bybit`), and the one
//! place a decoded event reaches the sink. Only the PROTOCOL stays here: the pure frame classifiers
//! ([`route_frame`]/`route_trades_frame`), the `.P` perp split + the three-way host pick, the REST warmup
//! seed, and the decoders.
//!
//! ## ⚠ THE HOST IS A ROUTING DECISION, AND IT IS THE SAME ONE THE HISTORY PATH MAKES
//!
//! `docs/decisions/0061-an-instrument-names-its-kind.md` phase 1 PINNED a contradiction inside this
//! one crate: `crates/bridges/bybit/src/history.rs`'s `route_target` REFUSED a bare symbol the venue
//! lists under both spot and inverse, while every lane in THIS file read the same string through a
//! `bool` (`.P` or not) and reached a host by construction. Phase 4 — this change — ends it.
//! [`feed_route`] IS `history.rs`'s `range_target`, so a symbol the store refuses to write is a symbol
//! this refuses to stream, and [`websocket_host`] turns the resolved `Category` into one of the venue's
//! THREE public sockets.
//!
//! Two consequences worth carrying before editing a lane here:
//!
//!   * **The socket is STRICT where the REST endpoint is lenient.** MEASURED 2026-09-16 — see
//!     [`websocket_host`]'s table for the probes. Every wrong-host lane is a `handler not found`
//!     reconnect loop, except DEPTH, where it is silent.
//!   * **`depth_main`'s route CHANGED user-visibly** (a bare symbol's DOM moves from the perp book
//!     to the spot book). Its own ⚠ carries what that was and why.
//!   * **A client of ENGINE names reads a bar symbol as the engine does**
//!     ([`DataClientSpecBybit::with_engine_named_bars`], [`bars_route`]): the bar lane and its paired mark
//!     lane take a coin-margined name's book and a `X.S` pair's from the name, as the tick pump does, and
//!     only there; every other name, and every lane of any other client, is routed as above.
//!
//! **Closed-bar detection: the `confirm` flag.** A Bybit kline datum carries `confirm` — `true` once
//! the candle is FINAL, `false` while it is still forming. This is the exact analogue of Binance's
//! kline `x`: `confirm=true` → emit a CLOSED bar via `close_bar`; `confirm=false` → a forming bar
//! via `forming_bar`.
//!
//! Endpoint: `<base>/v5/public/spot`, `<base>` the venue's `live` `data` `ws` row (`wss://stream.bybit.com`
//! in the seed), looked up ONCE by [`DataClientSpecBybit::new`] with the two `live` `rest` bases the lanes
//! read (the listings the route asks, the kline seed). Subscribe:
//! `{"op":"subscribe","args":["kline.<code>.<SYMBOL>"]}` where `<code>` is Bybit's interval code
//! ("1","60","D",…) from [`crate::history::interval_code`] — the SAME map the REST kline fetcher uses.
//! A kline datum is a JSON OBJECT `{start,open,high,low,close,volume,confirm,…}` (named fields, so
//! JSON order is irrelevant); `start` is the bar-open ms.
//!
//! [`route_frame`]: bar::route_frame

use vike_bridge_core::data_client::ws::{DataClientLane, DataClientLanePlan, DataClientSpec};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::history::Category;

mod depth;
pub mod mark;

pub mod bar;
// The typed fast path in front of `tick::route_frame` (`market_scan`); its scanning blocks are
// `vike_bridge_core::frame_scan`.
pub(crate) mod decode;
pub mod tick;

#[cfg(test)]
use depth::{fold_depth_frame, frame_ts_ms};

const VENUE: &str = "bybit";
const SEED_LIMIT: usize = 1000; // newest N klines for the REST warmup (Bybit's per-request cap)
/// The public streams' path on the `live` `data` `ws` base: one STRICT socket per book,
/// `/v5/public/spot`, `/v5/public/linear` and `/v5/public/inverse` ([`websocket_host`]).
const PATH_PUBLIC_WEBSOCKET: &str = "/v5/public/";

/// **The ONE host decision**, and the twin of `crates/bridges/bybit/src/history.rs`'s `rest_category`:
/// the same [`Category`] value picks the REST category and this socket (`<base>/v5/public/<category>`,
/// the category's own wire word), so a lane's warmup seed and its stream can never reach different
/// books. `websocket_base` is the venue's `live` `data` `ws` row. The spot socket matches `history::CATEGORY =
/// "spot"`; the linear one serves `BTCUSDT` linear (what `VenueRestBybit` trades), the same
/// `orderbook.200.<SYM>`/`kline.<code>.<SYM>` topic shapes as spot; a symbol the venue lists on
/// `category=linear` routes there, never by a `.P` suffix alone — see [`feed_route`].
///
/// ⚠ **The inverse (coin-settled) socket is the reason the live feed had to change at all, and it is
/// the half no prose in this crate had measured.** `/v5/market/kline` resolves `category` LENIENTLY —
/// MEASURED 2026-09-16, `category=linear&symbol=BTCUSD` returns the inverse tape byte-identically — so
/// the REST side was quietly correct. **The socket is STRICT, in both directions**, MEASURED the same
/// day against the real hosts with the real topics this file builds:
///
/// | host | topic | answer |
/// |---|---|---|
/// | `…/public/linear` | `orderbook.200.BTCUSD`, `orderbook.50.BTCUSD`, `kline.1.BTCUSD`, `publicTrade.BTCUSD`, `tickers.BTCUSD` | `success=false`, `error:handler not found` |
/// | `…/public/inverse` | all five of the above | `type=snapshot` DATA |
/// | `…/public/inverse` | `kline.1.BTCUSDT` | `success=false`, `error:handler not found` |
///
/// So an inverse symbol on the linear socket is a `WebsocketSessionFrameOutcome::Fatal` and a reconnect loop on the
/// kline/trades/mark lanes — and on the DEPTH lane it is SILENT: a `success=false` ack folds to
/// `BookOp::Ignored`, the book never seeds, and the driver's idle watchdog reconnects forever with no
/// error text anywhere. That is why widening the catalog without the inverse socket would have been
/// worse than leaving the instruments unreachable.
///
/// The category's wire word is a `match` with no catch-all on purpose (`rest_category`) — a fourth
/// category (bybit serves `option` too) must fail to COMPILE rather than default onto one of these three.
pub(crate) fn websocket_host(websocket_base: &str, book: Category) -> String {
    format!("{websocket_base}{PATH_PUBLIC_WEBSOCKET}{}", book.wire())
}

/// **The public bases one [`DataClientSpecBybit`] dials, resolved ONCE by
/// [`DataClientSpecBybit::new`]** from the venue's `live` rows: every account reads MAINNET market
/// data, a demo one included, as it always has.
#[derive(Debug)]
pub(crate) struct BybitUrlsFeed {
    /// The public sockets' base (`live` `data` `ws`), [`websocket_host`] appends the book's path.
    pub(crate) ws: String,
    /// The keyless instrument listings [`feed_route`] reads (`live` `data` `rest`).
    pub(crate) listings: String,
    /// The kline warmup seed's base (`live` `history` `rest`).
    pub(crate) history: String,
}

impl BybitUrlsFeed {
    fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        let live =
            |channel, protocol| urls.url(UrlType::Live, channel, protocol, "").map(str::to_string);
        Ok(BybitUrlsFeed {
            ws: live(UrlChannel::Data, UrlProtocol::Websocket)?,
            listings: live(UrlChannel::Data, UrlProtocol::Rest)?,
            history: live(UrlChannel::History, UrlProtocol::Rest)?,
        })
    }
}

/// Split an incoming feed symbol into `(api_symbol, is_perp)`: a trailing `.P` marks a perpetual
/// (the catalog's distinct-symbol tag, matching `BYBIT:BTCUSDT.P`) and is STRIPPED to the exchange
/// symbol that drives the WS subscribe topic + REST seed, while the ORIGINAL `.P`-suffixed symbol
/// stays the sink/core series label (so a perp's series key never collides with its spot twin). An
/// owning wrapper over [`vike_catalog::split_perp_at`], which is where that split is defined for
/// every venue.
///
/// ⚠ **It answers "is this a perpetual", NOT "which book".** That was the whole defect: every lane
/// in this file read this one `bool` as a host, so `.P` MEANT linear. [`feed_route`] is the function
/// that answers the host question now, and this one survives only where a plain perp/not-perp
/// predicate is genuinely what is wanted ([`should_pair_mark`] — spot has no mark price, and that is
/// true of both derivative books).
fn perp_split(symbol: &str) -> (String, bool) {
    let (api, perp) = vike_catalog::split_perp_at(VENUE, symbol);
    (api.to_string(), perp)
}

/// **Resolve a feed symbol to `(api_symbol, category)`** — the live twin of
/// `crates/bridges/bybit/src/history.rs`'s `range_target`, and the end of the contradiction this file
/// used to PIN.
///
/// `docs/decisions/0061-an-instrument-names-its-kind.md` phase 1 recorded that *"bybit's own crate
/// reads one string two ways … Two live, opposite readings, one venue, one crate"* and said to PIN
/// it rather than fix it, because changing a live subscription's route was phase 4. This is phase 4:
/// the history path and the live path now call the same `route_target` through the same venue
/// listings, so a symbol that the store refuses to write is a symbol this refuses to stream.
///
/// ⚠ **It can REFUSE, and a caller must surface the refusal rather than fall back.** A bare symbol
/// the venue lists under both spot and inverse has no route (the collision refusal), and a listing
/// read that FAILED is not permission to guess. Every lane below turns an `Err` into the
/// subscription's status string, which is what an operator sees.
///
/// Network I/O on the first call per process only — the derivation is cached in
/// [`crate::instrument::listing`] (read at [`BybitUrlsFeed::listings`]), and a plain spot symbol never reaches it.
fn feed_route(hosts: &BybitUrlsFeed, symbol: &str) -> Result<(String, Category), String> {
    let (wire, book) = crate::history::live_route(&hosts.listings, symbol)?;
    Ok((wire.to_string(), book))
}

/// **The bar lane's and its paired mark lane's route**: [`feed_route`], except on a client that reads a bar
/// symbol as an ENGINE name ([`DataClientSpecBybit::with_engine_named_bars`]), where [`engine_route`] answers first.
fn bars_route(
    hosts: &BybitUrlsFeed,
    symbol: &str,
    engine_names: bool,
) -> Result<(String, Category), String> {
    route_bars_with(symbol, engine_names, |s| feed_route(hosts, s))
}

/// [`bars_route`] over an injected data-plane route, so the choice between the two is assertable offline.
fn route_bars_with(
    symbol: &str,
    engine_names: bool,
    data_route: impl FnOnce(&str) -> Result<(String, Category), String>,
) -> Result<(String, Category), String> {
    match engine_names.then(|| engine_route(symbol)).flatten() {
        Some(route) => Ok(route),
        None => data_route(symbol),
    }
}

/// **The book an engine symbol's KEPT row names** (`crate::history::Category::of_kept`, the rule
/// `crates/bridges/bybit/src/feed/tick/mod.rs`'s `pump_route` reads for the tick pump): a coin-margined contract
/// (`BTCUSD`, `BTCUSDZ26`) is the inverse book under its own name, a pair `X.S` the spot book under `X`. No
/// network. `None` sends the name the data plane's way ([`feed_route`]): a name the kept list carries no row of, a
/// `.P` spelling, and a LINEAR name, whose bare spelling has always been served the spot pair's candles here (the
/// linear mounts run on them; `crates/vike-tradehub/src/feeds.rs`'s `bybit_arm` says why) and still is.
fn engine_route(symbol: &str) -> Option<(String, Category)> {
    match Category::of_kept(symbol)? {
        Category::Linear => None,
        book => Some((crate::instrument::class::split_spot(symbol).0.to_string(), book)),
    }
}

/// Whether a bars subscription on `symbol` also opens the `tickers` mark stream (the engine adds the
/// venue's `mark_streams` row): a perpetual, which is the `.P` perp tag or, on a client of engine names, a
/// coin-margined name ([`engine_route`]). Pure — the ONE place the pairing predicate lives, so the spec
/// and its tests read the same law. Spot has no mark price at all.
fn should_pair_mark(symbol: &str, engine_names: bool) -> bool {
    let inverse = || matches!(engine_route(symbol), Some((_, Category::Inverse)));
    perp_split(symbol).1 || (engine_names && inverse())
}

/// **bybit's half of the data-client engine**: the public bases every lane dials, resolved ONCE from the
/// venue's `live` rows (every account reads MAINNET market data, a demo one included, as it always has),
/// and whether a bar symbol is an engine's name.
///
/// ⚠ **The engine's status line has a reader that can only SUPPRESS**: `crates/vike-tradehub/src/
/// feeds.rs`'s `StrategyLanesLive::reconcile_feed_statuses` health-gated bybit's reconcile leg on it, and on
/// 2026-09-10 that row suppressed the leg for 42 hours. A second lane sharing the one string (a `.P`
/// symbol's paired mark lane, a second interval) makes it ambiguous evidence, which is why the engine
/// answers `status_writer_lanes`.
#[derive(Debug)]
pub struct DataClientSpecBybit {
    /// The public bases every lane dials.
    hosts: BybitUrlsFeed,
    /// Whether a `subscribe_bars` symbol is an ENGINE name ([`Self::with_engine_named_bars`]).
    engine_names: bool,
}

impl DataClientSpecBybit {
    /// The spec over bybit's address rows `urls`: the public bases ([`BybitUrlsFeed`]) are looked up here,
    /// ONCE, at `live`.
    ///
    /// # Errors
    /// The refusal naming the missing row: nothing is built, no lane can be dialled.
    pub fn new(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(DataClientSpecBybit { hosts: BybitUrlsFeed::from_urls(urls)?, engine_names: false })
    }

    /// **Read a `subscribe_bars` symbol as an ENGINE name** (a coin-margined `BTCUSD` is the inverse book, a
    /// pair is `BTCUSD.S`; [`engine_route`]), for its bars and its paired mark; trades and depth keep the
    /// data plane's reading. Without it the bare `BTCUSD`, which bybit lists as a spot pair AND an inverse
    /// perpetual, is refused (decision 0061), so a strategy mounted on an inverse account heard no bar. The
    /// gatewayhub sets it: the only bar subscriber it serves is the datahub's strategy lane, whose symbols are
    /// the engines' own (`crates/vike-datahub/src/feeds/mod.rs`'s `DataClientBrokerHandle`).
    #[must_use]
    pub fn with_engine_named_bars(mut self) -> Self {
        self.engine_names = true;
        self
    }
}

impl DataClientSpec for DataClientSpecBybit {
    const VENUE: &'static str = VENUE;
    const DISPLAY: &'static str = "Bybit";
    /// This venue's mark wire is documented, so its mark stream ships ON;
    /// `venue.bybit.mark_streams = 0` turns it off (decision 0095).
    const MARK_STREAMS_DEFAULT_ON: bool = true;

    fn pairs_mark(&self, symbol: &str) -> bool {
        should_pair_mark(symbol, self.engine_names)
    }

    /// Every route is resolved HERE, on the lane's thread: the first resolution per process reads the
    /// venue's instrument listings, which must not stall the caller's (often the picker's) thread.
    fn plan(
        &self,
        lane: DataClientLane,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientLanePlan, String> {
        match lane {
            DataClientLane::Bars => {
                bar::bars_plan(symbol, interval, &self.hosts, self.engine_names)
            }
            DataClientLane::Mark => mark::mark_plan(symbol, &self.hosts, self.engine_names),
            DataClientLane::Trades => tick::trades::trades_plan(symbol, &self.hosts),
            DataClientLane::Depth => depth::depth_main(symbol, &self.hosts),
            // The caps row refuses both before a thread exists (`live_data.quotes`/`book` are false): this
            // venue publishes no book-ticker channel, L1 is derived from the L2 top.
            DataClientLane::Quotes | DataClientLane::Book => {
                Err(format!("bybit serves no live {} lane", lane.word(interval)))
            }
        }
    }
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;
