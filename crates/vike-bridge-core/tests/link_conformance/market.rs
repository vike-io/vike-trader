//! The covered MARKET rows: every market lane on the shared market pump (`VenueConnectionDriver::MarketPump`) has a
//! [`MarketLinkVenue`]. Where the engine's dial is the lane's socket, the lane is driven through the data-client
//! engine's own `run_plan_on` with the venue's REAL plan (its subscribe frame, keepalive payload and decoder; the REST
//! warm-up dropped, a read and not the socket), its pump row's REAL knobs
//! (`vike_bridge_core::websocket_session::spec::row`) and its status written through `DataClientContext::set_status`,
//! on a virtual clock. A lane whose loop dials for itself is driven through that loop with its dial handed in: the
//! binance family's trade tape through `vike_binance::family::feed::tick::trades::run_trades_feed_on` (its REST warm-up
//! empty), polymarket's shard through `vike_polymarket::run_token_shard`. A scripted session is
//! `vike_bridge_core::scripted`'s user-data double on its market seam, so a stall is the silence the driver's idle
//! watchdog measures, a sent ping is answered with the venue's own pong and a server ping with a pong.
//!
//! Two scenarios run here, the ones a market row's knobs decide: **S5** (the idle watchdog trips the row's
//! `idle_threshold` after the last frame, state `Error`, then `Connected` on the reopened session) and **S11** (the
//! venue's ping at the row's `keepalive_every` and a quiet lane the venue keeps answering never cut, however long no
//! data comes; or, on a row with no keepalive, every server ping answered and nothing of ours sent). A cell an impl's
//! shape answers itself ([`MarketLinkVenue::cell`]: a lane that dials its own socket or body)
//! is DEFERRED with why or proven on the venue's own seam; the rest are DEFERRED or do not apply, with why ([`cell`]).

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vike_binance::family::FamilySpec;
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::ws::{
    DataClientLane, DataClientLanePlan, DataClientSpec, run_plan_on,
};
use vike_bridge_core::pump_clock::VirtualClock;
use vike_bridge_core::scripted::{
    UserScriptHeard, UserScriptLog, UserScriptStep, WebsocketSessionUserStreamScript,
};
use vike_bridge_core::websocket_session::market::{
    WebsocketSessionMarketNotice, WebsocketSessionMarketOptions,
};
use vike_bridge_core::websocket_session::spec::{WebsocketSessionKnobs, row};
use vike_bridge_core::websocket_session::user::{USER_DATA_POLL, WebsocketSessionUserMessage};
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_venues::venue_connection::{VenueConnectionAnnouncedClose, connection_for};
use vike_venues::venue_rate_limits::VenueRateLimitsMarket;
use vike_venues::venue_urls::VenueUrls;

use super::scenarios::Cell;
use super::sessions::Proof;
use super::{Lane, Scenario, rows};

/// The dial a lane is run over: one scripted session per call.
type Dial<'a> = dyn FnMut(
        &str,
        &WebsocketSessionMarketOptions<'_>,
    ) -> Result<WebsocketSessionUserStreamScript, String>
    + 'a;

/// A covered market row: its venue's real pieces.
pub(super) trait MarketLinkVenue: Sync {
    fn venue(&self) -> &'static str;
    /// The lanes this harness drives, each with the user name it is planned on: by default the kline and trades lanes
    /// (connection-sync C6) on [`SYMBOL`].
    fn lanes(&self) -> Vec<(DataClientLane, &'static str)> {
        vec![(DataClientLane::Bars, SYMBOL), (DataClientLane::Trades, SYMBOL)]
    }
    /// The frame a session of `lane` on `symbol` opens with, as the venue's socket delivers it, which confirms the lane:
    /// its subscribe ack, or its first data frame where the ack confirms nothing or there is none; `None` where the
    /// venue sends neither unasked.
    fn ack(&self, lane: DataClientLane, symbol: &str) -> Option<&'static str>;
    /// How long after its dial `lane`'s first session reaches the driver, whose keepalive cadence starts then; none
    /// (the default) where the dial hands the socket straight over.
    fn reads_before_the_driver(&self, _lane: DataClientLane) -> Duration {
        Duration::ZERO
    }
    /// The venue's ping, as the bridge names it, and the frame the venue answers it with; `None` where the lane sends
    /// no ping of its own (the server pings).
    fn ping(&self) -> Option<(&'static str, &'static str)>;
    /// Run `lane` on `symbol` through the venue's REAL session over `dial`.
    fn run(&self, lane: DataClientLane, symbol: &str, ctx: &DataClientContext, dial: &mut Dial<'_>);
    /// A cell the lane's shape answers itself, with why; `None` (the default) leaves it to [`cell`]. It is the ONE way
    /// an impl answers a cell.
    fn cell(&self, _scenario: Scenario) -> Option<Cell> {
        None
    }
}

/// The user name a lane is planned on by default, a row of each venue's held kept list (decision 0290).
const SYMBOL: &str = "BTCUSDT.P";

/// The covered market rows. Extending: a venue whose market lane rides the shared pump implements
/// [`MarketLinkVenue`] and joins here.
pub(super) fn covered() -> Vec<Box<dyn MarketLinkVenue>> {
    vec![
        Box::new(Okx),
        Box::new(Bybit),
        Box::new(Binance),
        Box::new(Aster),
        Box::new(Deribit),
        Box::new(Hyperliquid),
        Box::new(Polymarket),
        Box::new(Ig),
        Box::new(Alpaca),
        Box::new(Oanda),
    ]
}

/// `lane` of `spec` on `symbol` through the engine's `run_plan_on`, its REST warm-up dropped. Only an engine session
/// is: any other plan dials for itself, never through `dial`.
fn run_spec<S: DataClientSpec>(
    spec: &S,
    lane: DataClientLane,
    symbol: &str,
    ctx: &DataClientContext,
    dial: &mut Dial<'_>,
) {
    let interval = "1m";
    let mut plan = spec.plan(lane, symbol, interval).expect("the lane's plan");
    let DataClientLanePlan::Session { seed, .. } = &mut plan else {
        panic!("{} {lane:?} is no engine session: the engine's dial is not its socket", S::VENUE)
    };
    *seed = None;
    run_plan_on::<S, _>(plan, lane, symbol, interval, ctx, dial);
}

/// OKX v5 (`crates/bridges/okx/src/feed/mod.rs`'s `DataClientSpecOkx`): the candle lane on the business socket, the
/// trades lane on the public one.
struct Okx;

impl MarketLinkVenue for Okx {
    fn venue(&self) -> &'static str {
        "okx"
    }
    // After `crates/bridges/okx/src/feed/feed_tests.rs`'s `subscribe_ack_is_classified_as_ack_pong_and_junk_ignored`.
    fn ack(&self, _lane: DataClientLane, _symbol: &str) -> Option<&'static str> {
        Some(
            r#"{"event":"subscribe","arg":{"channel":"candle1m","instId":"BTC-USDT-SWAP"},"connId":"a4d3ae55"}"#,
        )
    }
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        Some((vike_okx::feed::tick::KEEPALIVE, "pong"))
    }
    fn run(
        &self,
        lane: DataClientLane,
        symbol: &str,
        ctx: &DataClientContext,
        dial: &mut Dial<'_>,
    ) {
        vike_okx::instrument::kept_list_for_tests();
        let spec = vike_okx::feed::DataClientSpecOkx::new(&VenueUrls::seed("okx"))
            .expect("the seed holds okx's data rows");
        run_spec(&spec, lane, symbol, ctx, dial);
    }
}

/// Bybit V5 (`crates/bridges/bybit/src/feed/mod.rs`'s `DataClientSpecBybit`): both lanes on the linear socket.
struct Bybit;

impl MarketLinkVenue for Bybit {
    fn venue(&self) -> &'static str {
        "bybit"
    }
    fn ack(&self, _lane: DataClientLane, _symbol: &str) -> Option<&'static str> {
        Some(
            r#"{"success":true,"ret_msg":"","conn_id":"2324d924-8f6e-4f0e-9bc3-6f3e2a2b1c7c","req_id":"","op":"subscribe"}"#,
        )
    }
    // The linear socket's pong, from bybit's v5 "Connect" page (`crates/bridges/bybit/src/feed/feed_tests.rs`'s
    // `every_session_lane_pings_and_its_pong_is_neither_data_nor_a_fault`).
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        Some((
            vike_bybit::feed::tick::KEEPALIVE,
            r#"{"success":true,"ret_msg":"pong","conn_id":"465772b1-7630-4fdc-a492-e003e6f0f260","req_id":"","op":"ping"}"#,
        ))
    }
    fn run(
        &self,
        lane: DataClientLane,
        symbol: &str,
        ctx: &DataClientContext,
        dial: &mut Dial<'_>,
    ) {
        vike_bybit::history::kept_list_for_tests();
        let spec = vike_bybit::feed::DataClientSpecBybit::new(&VenueUrls::seed("bybit"))
            .expect("the seed holds bybit's public rows");
        run_spec(&spec, lane, symbol, ctx, dial);
    }
}

/// A binance-family `@kline_1m` frame of a forming bar, after the frames of
/// `crates/bridges/binance/tests/offline/coin_m_feed.rs`: the lane subscribes by its URL and the venue acks nothing, so
/// its first data frame is what confirms it.
const FAMILY_KLINE: &str = r#"{"e":"kline","E":1794160198677,"s":"BTCUSDT","k":{"t":1794160140000,"T":1794160199999,"s":"BTCUSDT","i":"1m","f":1163308136,"L":1163308152,"o":"75640.0","c":"75640.0","h":"75640.0","l":"75639.9","v":"865","n":17,"x":false,"q":"1.14357557","V":"439","Q":"0.58038075","B":"0"}}"#;

/// A binance-family `@aggTrade` frame (every spot book, and aster's perp), after
/// `crates/bridges/binance/src/family/feed/tick/dial_tests.rs`'s `agg_trade`: the trade tape subscribes by its URL and
/// the venue acks nothing, so its first trade is what confirms it.
const FAMILY_AGG_TRADE: &str = r#"{"e":"aggTrade","E":1794160140001,"s":"BTCUSDT","a":1163308152,"p":"75640.0","q":"0.1","f":1,"l":1,"T":1794160140000,"m":false}"#;

/// Binance's USDⓈ-M perp trade tape rides the RAW `@trade` stream (`vike_binance::family::PerpTradesLane::Raw`, decision
/// 0251), after `crates/bridges/binance/src/family/feed/tick/perp_lane_tests.rs`'s `WEBSOCKET_RAW`.
const BINANCE_PERP_TRADE: &str = r#"{"e":"trade","E":1785701708157,"T":1785701708156,"s":"BTCUSDT","t":7947574407,"p":"63437.30","q":"0.003","X":"MARKET","m":false,"st":1}"#;

/// The binance family's lanes driven here, each on the spot and the perp socket: the kline lane, an engine session, and
/// the trade tape, a loop of its own (`crates/bridges/binance/src/family/feed/mod.rs`'s `trades_main`, an `Own` plan),
/// driven through [`run_family_trades`].
const FAMILY_LANES: [(DataClientLane, &str); 4] = [
    (DataClientLane::Bars, "BTCUSDT"),
    (DataClientLane::Bars, "BTCUSDT.P"),
    (DataClientLane::Trades, "BTCUSDT"),
    (DataClientLane::Trades, "BTCUSDT.P"),
];

/// The family's trade tape on `symbol` (a lane name, `.P` a perp: the catalog's `split_perp_at` on these venues) over
/// `family`, the row the venue's plan reads, through its REAL loop with `dial` handed in:
/// `vike_binance::family::feed::tick::trades::run_trades_feed_on`, the plan's `notice` for the book, and an empty REST
/// warm-up (a read, not the socket).
fn run_family_trades(
    family: &FamilySpec,
    symbol: &str,
    notice: fn(bool) -> Option<WebsocketSessionMarketNotice>,
    ctx: &DataClientContext,
    dial: &mut Dial<'_>,
) {
    let (api_symbol, is_perp) =
        symbol.strip_suffix(".P").map_or((symbol, false), |api| (api, true));
    vike_binance::family::feed::tick::trades::run_trades_feed_on(
        family,
        symbol,
        api_symbol,
        is_perp,
        ctx,
        &Mutex::default(),
        notice(is_perp),
        || Ok(Vec::new()),
        dial,
    );
}

/// The trade tape's first session drains its socket before the driver takes it, until a read times out: past the
/// opening frame, one scripted read tick.
fn family_reads_before_the_driver(lane: DataClientLane) -> Duration {
    if matches!(lane, DataClientLane::Trades) { USER_DATA_POLL } else { Duration::ZERO }
}

/// Binance spot and USDⓈ-M (`crates/bridges/binance/src/feed/mod.rs`'s `DataClientSpecBinance`): one market row, both
/// books' sockets.
struct Binance;

impl MarketLinkVenue for Binance {
    fn venue(&self) -> &'static str {
        "binance"
    }
    fn lanes(&self) -> Vec<(DataClientLane, &'static str)> {
        FAMILY_LANES.to_vec()
    }
    fn ack(&self, lane: DataClientLane, symbol: &str) -> Option<&'static str> {
        Some(match lane {
            DataClientLane::Trades if symbol.ends_with(".P") => BINANCE_PERP_TRADE,
            DataClientLane::Trades => FAMILY_AGG_TRADE,
            _ => FAMILY_KLINE,
        })
    }
    fn reads_before_the_driver(&self, lane: DataClientLane) -> Duration {
        family_reads_before_the_driver(lane)
    }
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        None
    }
    fn run(
        &self,
        lane: DataClientLane,
        symbol: &str,
        ctx: &DataClientContext,
        dial: &mut Dial<'_>,
    ) {
        vike_binance::instrument::names::kept_list_for_tests();
        let spec = vike_binance::feed::DataClientSpecBinance::new(&VenueUrls::seed("binance"))
            .expect("the seed holds binance's data rows");
        match lane {
            DataClientLane::Trades => run_family_trades(
                &spec.family(symbol),
                symbol,
                vike_binance::feed::market_notice,
                ctx,
                dial,
            ),
            _ => run_spec(&spec, lane, symbol, ctx, dial),
        }
    }
}

/// Aster (`crates/bridges/aster/src/feed/mod.rs`'s `DataClientSpecAster`): binance's lanes on aster's hosts.
struct Aster;

impl MarketLinkVenue for Aster {
    fn venue(&self) -> &'static str {
        "aster"
    }
    fn lanes(&self) -> Vec<(DataClientLane, &'static str)> {
        FAMILY_LANES.to_vec()
    }
    // Aster's perp tape stays on `@aggTrade` (`PerpTradesLane::Aggregated`).
    fn ack(&self, lane: DataClientLane, _symbol: &str) -> Option<&'static str> {
        Some(if matches!(lane, DataClientLane::Trades) { FAMILY_AGG_TRADE } else { FAMILY_KLINE })
    }
    fn reads_before_the_driver(&self, lane: DataClientLane) -> Duration {
        family_reads_before_the_driver(lane)
    }
    // The keepalive's answer, as `fstream` sent it on a quiet `/ws/<stream>` socket (2026-11-22, the pump row's
    // comment), the stream named as the kline lane subscribes it (a trade tape's names its `@aggTrade`; neither lane's
    // decoder reads the answer).
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        Some((vike_aster::feed::KEEPALIVE, r#"{"id":1,"result":["btcusdt@kline_1m"]}"#))
    }
    fn run(
        &self,
        lane: DataClientLane,
        symbol: &str,
        ctx: &DataClientContext,
        dial: &mut Dial<'_>,
    ) {
        // The gatewayhub builds it at `Live`.
        let spec = vike_aster::feed::DataClientSpecAster::new(
            &VenueUrls::seed("aster"),
            vike_model::AccountTier::Live,
        )
        .expect("the seed holds aster's data rows");
        match lane {
            // Aster's plan hands its trade tape no notice: its streams document none.
            DataClientLane::Trades => {
                run_family_trades(&spec.family(), symbol, |_| None, ctx, dial)
            }
            _ => run_spec(&spec, lane, symbol, ctx, dial),
        }
    }
}

/// Deribit (`crates/bridges/deribit/src/feed/mod.rs`'s `DataClientSpecDeribit`): one keyless JSON-RPC socket per lane,
/// on the perpetual's user name.
struct Deribit;

impl MarketLinkVenue for Deribit {
    fn venue(&self) -> &'static str {
        "deribit"
    }
    fn lanes(&self) -> Vec<(DataClientLane, &'static str)> {
        vec![(DataClientLane::Bars, "BTCUSD.P"), (DataClientLane::Trades, "BTCUSD.P")]
    }
    // A `public/subscribe` reply naming its channel (`crates/bridges/deribit/src/feed/decode.rs`'s
    // `classify_rpc_reply`: an `Ack` on every lane).
    fn ack(&self, _lane: DataClientLane, _symbol: &str) -> Option<&'static str> {
        Some(r#"{"jsonrpc":"2.0","id":1,"result":["chart.trades.BTC-PERPETUAL.1"]}"#)
    }
    // The documented `public/test` answer (`crates/bridges/deribit/src/feed/feed_tests.rs`'s
    // `only_a_refusal_is_a_refusal`).
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        Some((
            vike_deribit::feed::KEEPALIVE_PING,
            r#"{"jsonrpc":"2.0","id":9929,"result":{"version":"1.2.26"}}"#,
        ))
    }
    fn run(
        &self,
        lane: DataClientLane,
        symbol: &str,
        ctx: &DataClientContext,
        dial: &mut Dial<'_>,
    ) {
        vike_deribit::names::hold_deribit_kept_rows();
        let spec = vike_deribit::feed::DataClientSpecDeribit::new(&VenueUrls::seed("deribit"))
            .expect("the seed holds deribit's live rows");
        run_spec(&spec, lane, symbol, ctx, dial);
    }
}

/// Hyperliquid (`crates/bridges/hyperliquid/src/feed/mod.rs`'s `DataClientSpecHyperliquid`) on the mainnet socket,
/// its symbology loaded and named by the crate's test rows without holding them process-wide (the exec row's
/// symbology names its fills with none held).
struct Hyperliquid;

impl MarketLinkVenue for Hyperliquid {
    fn venue(&self) -> &'static str {
        "hyperliquid"
    }
    fn lanes(&self) -> Vec<(DataClientLane, &'static str)> {
        vec![(DataClientLane::Bars, "BTCUSDC.P"), (DataClientLane::Trades, "BTCUSDC.P")]
    }
    // HL's `subscriptionResponse` confirms nothing, so a session is confirmed by its first data frame (after
    // `crates/bridges/hyperliquid/src/feed/feed_tests.rs`'s `candle` and `crates/bridges/hyperliquid/src/feed/decode.rs`'s
    // `trades_from_frame`).
    fn ack(&self, lane: DataClientLane, _symbol: &str) -> Option<&'static str> {
        Some(match lane {
            DataClientLane::Trades => {
                r#"{"channel":"trades","data":[{"coin":"BTC","side":"B","px":"75640","sz":"0.1","time":1794160140000,"hash":"0x0","tid":1}]}"#
            }
            _ => {
                r#"{"channel":"candle","data":{"t":1794160140000,"T":1794160199999,"s":"BTC","i":"1m","o":"75640","c":"75640","h":"75640","l":"75640","v":"1","n":1}}"#
            }
        })
    }
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        Some((vike_hyperliquid::feed::PING_MESSAGE, r#"{"channel":"pong"}"#))
    }
    fn run(
        &self,
        lane: DataClientLane,
        symbol: &str,
        ctx: &DataClientContext,
        dial: &mut Dial<'_>,
    ) {
        let symbology = vike_hyperliquid::instrument::symbology::Symbology::from_meta_named_by(
            &serde_json::json!({"universe": [{"name": "BTC", "szDecimals": 5, "maxLeverage": 40}]}),
            &serde_json::json!({"tokens": [], "universe": []}),
            Some(vike_hyperliquid::test_support::kept_rows().as_slice()),
        );
        let spec = vike_hyperliquid::feed::DataClientSpecHyperliquid::new(
            vike_model::VenueNetwork::Mainnet,
            &VenueUrls::seed("hyperliquid"),
        )
        .expect("the seed holds hyperliquid's data rows")
        .with_symbology(Arc::new(symbology));
        run_spec(&spec, lane, symbol, ctx, dial);
    }
}

/// Polymarket's market channel: a shard thread's loop dials for itself (`crates/bridges/polymarket/src/feed/mod.rs`'s
/// `shard_main`), so each lane is driven through that REAL loop with its dial handed in,
/// `vike_polymarket::run_token_shard`: one token, its tick size seated with no REST read, and the shard's own
/// subscribe, status words and row knobs.
struct Polymarket;

/// A CLOB token id, as a shard subscribes it.
const POLYMARKET_TOKEN: &str =
    "71321045679252212594626385532706912750332728571942532289631379312455583992563";

/// The CLOB answers a subscribe with each token's `book` snapshot, here [`POLYMARKET_TOKEN`]'s (after
/// `crates/bridges/polymarket/src/feed/decode_tests.rs`'s `decode_book_real_captured_frame`).
const POLYMARKET_BOOK: &str = r#"{"market":"0x66bbf6d55e0296278858b3147689f3df9259374f158f9f028b608baa322a639c","asset_id":"71321045679252212594626385532706912750332728571942532289631379312455583992563","timestamp":"1783516513010","hash":"43b683fda60e94ae739c04a2c31ab18ef450a006","bids":[{"price":"0.01","size":"22260.18"}],"asks":[{"price":"0.99","size":"655294.85"}],"tick_size":"0.01","event_type":"book","last_trade_price":"0.600"}"#;

impl MarketLinkVenue for Polymarket {
    fn venue(&self) -> &'static str {
        "polymarket"
    }
    fn lanes(&self) -> Vec<(DataClientLane, &'static str)> {
        vec![(DataClientLane::Quotes, POLYMARKET_TOKEN), (DataClientLane::Trades, POLYMARKET_TOKEN)]
    }
    // No subscribe ack (the pump row's `ack_timeout` is `None`): the book snapshot is the session's first frame.
    fn ack(&self, _lane: DataClientLane, _symbol: &str) -> Option<&'static str> {
        Some(POLYMARKET_BOOK)
    }
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        Some((vike_polymarket::feed::PING_PAYLOAD, "PONG"))
    }
    fn run(
        &self,
        lane: DataClientLane,
        symbol: &str,
        ctx: &DataClientContext,
        dial: &mut Dial<'_>,
    ) {
        let mode = match lane {
            DataClientLane::Trades => vike_polymarket::FeedLane::Trades,
            _ => vike_polymarket::FeedLane::Quotes,
        };
        let subscribe = vike_polymarket::subscribe_message(&[symbol.to_string()]);
        let opts =
            row(self.venue()).opts(Some(&subscribe), Some(vike_polymarket::feed::PING_PAYLOAD));
        vike_polymarket::run_token_shard(mode, symbol, 0.01, ctx, || {
            dial("wss://polymarket-market-channel", &opts)
        });
    }
}

/// Why a `SessionOn` lane's reopen and dial are no scripted session's.
const SESSION_ON: &str = "a `SessionOn` lane: its connect hook attaches it to the venue's one shared socket, which \
     that socket's link dials itself after a login, so the engine's dial is not its socket and no scripted session \
     reaches it";

const IG_QUOTE_FEED: &str = "crates/bridges/ig/tests/offline/quote_feed.rs";

/// IG's TLCP market feed (`crates/bridges/ig/src/feed/mod.rs`'s `DataClientSpecIg`): every lane a subscription on the
/// client's one Lightstreamer session (`crates/bridges/ig/src/feed/link.rs`'s `LightstreamerLink`).
struct Ig;

impl MarketLinkVenue for Ig {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn ack(&self, _lane: DataClientLane, _symbol: &str) -> Option<&'static str> {
        None
    }
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        None
    }
    fn run(&self, _: DataClientLane, _: &str, _: &DataClientContext, _: &mut Dial<'_>) {
        unreachable!("every ig market cell is answered by `Ig::cell`")
    }
    fn cell(&self, scenario: Scenario) -> Option<Cell> {
        match scenario {
            Scenario::Keepalive => Some(Cell::Proven {
                by: &[Proof {
                    file: IG_QUOTE_FEED,
                    test: "a_closed_market_that_sends_only_probes_is_not_redialed",
                }],
                pins: "the server's TLCP `PROBE` keeps a lane quiet for hours (a closed market) on its one socket, \
                       never redialed and never disclosed (the probe narrowed to 100 ms); the lane sends no ping of \
                       its own, its row declaring no `keepalive_every`",
            }),
            Scenario::Stall => Some(Cell::Deferred(format!(
                "{SESSION_ON}: the row's `idle_threshold` is held by that link's reader \
                 (`crates/bridges/ig/src/feed/link.rs`'s `LightstreamerLink`), and no offline test silences its socket: \
                 the suite's silence tests are a quote item's, with PROBEs flowing (`{IG_QUOTE_FEED}`'s \
                 `a_lane_whose_updates_stop_while_probes_keep_coming_is_disclosed_and_redialed`)"
            ))),
            Scenario::Drop | Scenario::ServerClose | Scenario::DialBound => {
                Some(Cell::Deferred(format!("{SESSION_ON} (`crates/bridges/ig/src/feed/link.rs`)")))
            }
            _ => None,
        }
    }
}

const ALPACA_CONNECTION: &str = "crates/bridges/alpaca/src/feed/connection_tests.rs";

/// Alpaca's market data (`crates/bridges/alpaca/src/feed/client.rs`'s `DataClientSpecAlpaca`): every lane of an asset
/// class on that stream's one socket (`crates/bridges/alpaca/src/feed/connection.rs`'s `RouteLink`).
struct Alpaca;

impl MarketLinkVenue for Alpaca {
    fn venue(&self) -> &'static str {
        "alpaca"
    }
    fn ack(&self, _lane: DataClientLane, _symbol: &str) -> Option<&'static str> {
        None
    }
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        None
    }
    fn run(&self, _: DataClientLane, _: &str, _: &DataClientContext, _: &mut Dial<'_>) {
        unreachable!("every alpaca market cell is answered by `Alpaca::cell`")
    }
    fn cell(&self, scenario: Scenario) -> Option<Cell> {
        match scenario {
            Scenario::Keepalive => Some(Cell::Proven {
                by: &[Proof {
                    file: ALPACA_CONNECTION,
                    test: "a_ping_is_answered_and_a_server_close_is_told_to_every_lane",
                }],
                pins: "the server's ping is answered with a pong and counts as the socket's liveness; the lane sends \
                       no ping of its own, its row declaring no `keepalive_every`",
            }),
            Scenario::Stall => Some(Cell::Proven {
                by: &[
                    Proof {
                        file: ALPACA_CONNECTION,
                        test: "a_silent_socket_is_ended_at_the_idle_bound",
                    },
                    Proof {
                        file: "crates/bridges/alpaca/src/feed/feed_tests.rs",
                        test: "a_lanes_session_says_live_on_its_first_frame_and_its_error_in_the_engines_words",
                    },
                ],
                pins: "a socket silent past the idle bound is ended by its reader as a `silent stall` (narrowed to \
                       300 ms; the link reads the row's 180 s from the pump row), the lane says `Error` in the engine's \
                       words and each session's first frame says `Connected`",
            }),
            Scenario::Drop | Scenario::ServerClose | Scenario::DialBound => Some(Cell::Deferred(
                format!("{SESSION_ON} (`crates/bridges/alpaca/src/feed/connection.rs`)"),
            )),
            _ => None,
        }
    }
}

const LINE_STREAM: Proof = Proof {
    file: "crates/vike-bridge-core/src/websocket_session/user/line_stream_tests.rs",
    test: "the_market_pump_reads_a_line_stream_and_trips_on_its_silence",
};
const OANDA_FEED: &str = "crates/bridges/oanda/src/feed/feed_tests.rs";

/// OANDA's quote lane (`crates/bridges/oanda/src/feed/tick.rs`'s `quotes_main`): the chunked-HTTP pricing stream read
/// as lines on the shared market pump, its open the lane's own GET through its own agent.
struct Oanda;

impl MarketLinkVenue for Oanda {
    fn venue(&self) -> &'static str {
        "oanda"
    }
    fn ack(&self, _lane: DataClientLane, _symbol: &str) -> Option<&'static str> {
        None
    }
    fn ping(&self) -> Option<(&'static str, &'static str)> {
        None
    }
    fn run(&self, _: DataClientLane, _: &str, _: &DataClientContext, _: &mut Dial<'_>) {
        unreachable!("every oanda market cell is answered by `Oanda::cell`")
    }
    fn cell(&self, scenario: Scenario) -> Option<Cell> {
        match scenario {
            Scenario::Stall => Some(Cell::Proven {
                by: &[
                    LINE_STREAM,
                    Proof { file: OANDA_FEED, test: "the_streaming_agent_carries_the_market_rows_bounds" },
                    Proof {
                        file: OANDA_FEED,
                        test: "the_quote_pump_and_the_bar_retry_wait_the_market_rows_fixed_three_seconds",
                    },
                ],
                pins: "a line body gone silent is judged by the market pump's idle watchdog from its last whole line: \
                       live, then `silent stall`, then live on the reopened body (narrowed bounds); the lane's agent \
                       reads with the row's 20 s idle bound, which is the pump row's, and redials on its fixed 3 s",
            }),
            Scenario::Keepalive => Some(Cell::Proven {
                by: &[
                    LINE_STREAM,
                    Proof { file: OANDA_FEED, test: "heartbeats_keep_transport_alive_but_age_to_stale_once" },
                ],
                pins: "an HTTP body takes no frame, so the lane sends nothing; the venue's HEARTBEAT line (about every \
                       5 s) is a whole line the pump reads as the venue speaking, and it keeps the lane's transport \
                       health alive",
            }),
            Scenario::Drop | Scenario::ServerClose | Scenario::DialBound => Some(Cell::Deferred(
                "its open is the lane's own chunked GET through its own agent (`crates/bridges/oanda/src/feed/tick.rs`'s \
                 `quotes_main`), so the engine's dial is not its socket and no scripted session reaches it"
                    .to_string(),
            )),
            _ => None,
        }
    }
}

/// Whether `venue` has a covered market row.
pub(super) fn is_covered(venue: &str) -> bool {
    covered().iter().any(|covered| covered.venue() == venue)
}

/// What `scenario` is on `venue`'s market row: the impl's own answer first ([`MarketLinkVenue::cell`]).
pub(super) fn cell(venue: &dyn MarketLinkVenue, scenario: Scenario) -> Cell {
    if let Some(cell) = venue.cell(scenario) {
        return cell;
    }
    match scenario {
        Scenario::Stall | Scenario::Keepalive => Cell::Runs,
        Scenario::Drop | Scenario::ServerClose | Scenario::DialBound => Cell::Deferred(
            "a market lane's reopen and dial are the shared driver's own (`websocket_session/market_tests.rs`, \
             `depth_tests.rs`'s dial bound); this harness scripts S5 and S11 on it"
                .to_string(),
        ),
        Scenario::AnnouncedClose => match connection_for(venue.venue(), VenueRateLimitsMarket::Spot)
            .market
            .announced_close
        {
            VenueConnectionAnnouncedClose::NoneKnown => Cell::NotApplicable(
                "the row knows no notice the venue sends before a market socket closes".to_string(),
            ),
            VenueConnectionAnnouncedClose::Handled { notice }
            | VenueConnectionAnnouncedClose::Ignored { notice } => Cell::Deferred(format!(
                "the row's `{notice}`: the market pump's notice hook \
                 (`crates/vike-bridge-core/src/websocket_session/market.rs`'s `WebsocketSessionMarketNotice`) is not \
                 scripted by this harness yet"
            )),
        },
        _ => Cell::NotApplicable(
            "a market lane carries no fill, key or command: nothing to expire, read back or fold".to_string(),
        ),
    }
}

/// Run `scenario` on every covered market row and lane where it runs, one thread each; the failures, named.
pub(super) fn failures(scenario: Scenario) -> Vec<String> {
    let venues = covered();
    let rows = rows();
    std::thread::scope(|scope| {
        let runs: Vec<_> = venues
            .iter()
            .filter(|venue| matches!(cell(&***venue, scenario), Cell::Runs))
            .filter_map(|venue| {
                rows.iter()
                    .find(|row| row.lane == Lane::Market && row.venue == venue.venue())
                    .map(|row| (row, venue))
            })
            .flat_map(|(row, venue)| {
                venue.lanes().into_iter().map(move |(lane, symbol)| (row, venue, lane, symbol))
            })
            .map(|(row, venue, lane, symbol)| {
                let label = format!(
                    "{} {symbol} {} lane × {}",
                    row.label(),
                    lane.word("1m"),
                    scenario.label()
                );
                let thread = std::thread::Builder::new()
                    .name(label.clone())
                    .spawn_scoped(scope, move || run(&**venue, lane, symbol, scenario))
                    .expect("spawn a cell");
                (label, thread)
            })
            .collect();
        runs.into_iter()
            .filter_map(|(label, thread)| match thread.join() {
                Ok(Ok(())) => None,
                Ok(Err(why)) => Some(format!("[{label}] {why}")),
                Err(_) => Some(format!("[{label}] panicked (its message is above)")),
            })
            .collect()
    })
}

fn run(
    venue: &dyn MarketLinkVenue,
    lane: DataClientLane,
    symbol: &str,
    scenario: Scenario,
) -> Result<(), String> {
    let knobs = row(venue.venue());
    match scenario {
        Scenario::Stall => silent_stall(venue, lane, symbol, &knobs),
        Scenario::Keepalive => keepalive_cadence(venue, lane, symbol, &knobs),
        other => Err(format!("{other:?} does not run on a market row")),
    }
}

/// One scripted session: its steps, and whether the venue answers each frame the lane sends (its ping).
struct Session {
    steps: Vec<UserScriptStep>,
    answers: bool,
}

/// What a run did, every time an offset of the virtual clock.
struct Run {
    /// When each session was dialled.
    opens: Vec<Duration>,
    /// What the lane sent, and when.
    sent: Vec<(Duration, String)>,
    /// When the lane answered a server ping.
    pongs: Vec<Duration>,
    /// Each status the lane wrote, when it changed.
    states: Vec<(Duration, DataClientConnectionState, String)>,
}

/// Run `lane` of `venue` on `symbol` over `sessions` (the last raises the lane's stop once drained) on a fresh virtual
/// clock.
fn rig(
    venue: &dyn MarketLinkVenue,
    lane: DataClientLane,
    symbol: &str,
    sessions: Vec<Session>,
) -> Run {
    let clock = VirtualClock::new(1);
    let _member = clock.enter();
    let log = UserScriptLog::default();
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(DataClientStatus::default()));
    let states = Arc::new(Mutex::new(Vec::new()));
    let wake: Arc<dyn Fn() + Send + Sync> = {
        let (status, states, clock) =
            (Arc::clone(&status), Arc::clone(&states), Arc::clone(&clock));
        Arc::new(move || {
            let now = status.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let mut states = states.lock().unwrap_or_else(PoisonError::into_inner);
            if states.last().is_none_or(|(_, state, text)| (*state, text) != (now.state, &now.text))
            {
                states.push((clock.offset(clock.now()), now.state, now.text));
            }
        })
    };
    let ctx = DataClientContext {
        venue: venue.venue(),
        sink: Arc::new(vike_data::RecordingSink::default()),
        status,
        wake,
        stop: Arc::clone(&stop),
        own: (),
    };
    let pong = venue.ping().map(|(_, pong)| pong);
    let mut queue = VecDeque::from(sessions);
    let mut opens = Vec::new();
    let mut dial = |_url: &str,
                    _opts: &WebsocketSessionMarketOptions<'_>|
     -> Result<WebsocketSessionUserStreamScript, String> {
        let session = queue.pop_front().ok_or("a dial past the script")?;
        opens.push(clock.offset(clock.now()));
        let mut script =
            WebsocketSessionUserStreamScript::from_steps(session.steps).logged(Arc::clone(&log));
        if let (true, Some(pong)) = (session.answers, pong) {
            script = script.answering_sends_with(pong);
        }
        if queue.is_empty() {
            script = script.stopping(Arc::clone(&stop));
        }
        Ok(script)
    };
    venue.run(lane, symbol, &ctx, &mut dial);
    let (mut sent, mut pongs) = (Vec::new(), Vec::new());
    for (at, heard) in log.lock().unwrap_or_else(PoisonError::into_inner).iter() {
        match heard {
            UserScriptHeard::Text(text) => sent.push((clock.offset(*at), text.clone())),
            UserScriptHeard::Pong => pongs.push(clock.offset(*at)),
        }
    }
    let states = states.lock().unwrap_or_else(PoisonError::into_inner).clone();
    Run { opens, sent, pongs, states }
}

fn text(frame: &str) -> UserScriptStep {
    UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Text(frame.to_string())))
}

/// A session's steps: the venue's opening frame on `lane` of `symbol` (where it sends one), then `rest`.
fn opened(
    venue: &dyn MarketLinkVenue,
    lane: DataClientLane,
    symbol: &str,
    rest: Vec<UserScriptStep>,
) -> Vec<UserScriptStep> {
    venue.ack(lane, symbol).map(text).into_iter().chain(rest).collect()
}

/// The frames the lane sent at a session's open before any ping: its subscribe frame where the row says it sends one.
/// `Ok` with the rest of `sent`.
fn past_the_subscribe<'a>(
    sent: &'a [(Duration, String)],
    opened_at: Duration,
    knobs: &WebsocketSessionKnobs,
    ping: Option<&str>,
) -> Result<&'a [(Duration, String)], String> {
    if !knobs.subscribe_frame {
        return Ok(sent);
    }
    match sent.split_first() {
        Some(((at, frame), rest)) if *at == opened_at && Some(frame.as_str()) != ping => Ok(rest),
        _ => {
            Err(format!("the row sends a subscribe frame and the lane's first sends are {sent:?}"))
        }
    }
}

/// S5: the lane's socket goes silent (its pings unanswered). The idle watchdog trips the row's `idle_threshold` after
/// the last frame (never under twice the keepalive), the lane says `Error` with the stall in its words, reopens after
/// the row's wait and says `Connected` again: never a latched `Error`.
fn silent_stall(
    venue: &dyn MarketLinkVenue,
    lane: DataClientLane,
    symbol: &str,
    knobs: &WebsocketSessionKnobs,
) -> Result<(), String> {
    let idle = knobs.idle_threshold.ok_or("the pump row declares no idle_threshold")?;
    if let Some(every) = knobs.keepalive_every
        && idle < 2 * every
    {
        return Err(format!("the idle threshold {idle:?} is under twice the keepalive {every:?}"));
    }
    // The wait after a session that opened: a fixed row's one wait, an exponential row's first step (an open resets its
    // ladder), spread by up to `spread` on a growing row (`vike_venues::reconnect`'s module doc; none on a fixed one).
    let wait = knobs.backoff.initial;
    let spread = (wait / 2).min(knobs.backoff.max.saturating_sub(wait));
    if venue.ack(lane, symbol).is_none() {
        return Err("no opening frame confirms the reopened session".into());
    }
    let run = rig(
        venue,
        lane,
        symbol,
        vec![
            Session {
                steps: opened(
                    venue,
                    lane,
                    symbol,
                    vec![UserScriptStep::Stall(idle + 3 * USER_DATA_POLL)],
                ),
                answers: false,
            },
            Session {
                steps: opened(venue, lane, symbol, vec![UserScriptStep::Stall(USER_DATA_POLL)]),
                answers: false,
            },
        ],
    );
    if run.opens.len() != 2 {
        return Err(format!(
            "{} opens for one stall: {:?}; states {:?}",
            run.opens.len(),
            run.opens,
            run.states
        ));
    }
    // The opening frame is the session's last: its pings go unanswered.
    let reopened = run.opens[1] - run.opens[0];
    if reopened < idle + wait || reopened > idle + USER_DATA_POLL + wait + spread {
        return Err(format!(
            "reopened {reopened:?} after the last frame; the row's idle threshold {idle:?} and its wait {wait:?} \
             (spread by up to {spread:?}) put the reopen no sooner than their sum and no later than one poll past it"
        ));
    }
    let error = run
        .states
        .iter()
        .position(|(_, state, text)| {
            *state == DataClientConnectionState::Error && text.contains("silent stall")
        })
        .ok_or_else(|| format!("a silent stall never read as Error: {:?}", run.states))?;
    if !run.states[error..]
        .iter()
        .any(|(_, state, _)| *state == DataClientConnectionState::Connected)
    {
        return Err(format!("Error latched after the reopened session: {:?}", run.states));
    }
    Ok(())
}

/// S11. On a row with a keepalive: a quiet lane (no data for twice the idle threshold and more) whose venue answers
/// every ping; the lane sends its subscribe (where the row says it has one), then the venue's ping at the row's
/// `keepalive_every`, and the pong keeps the socket open: one session, never an `Error`. On a row with none: the server
/// pings three times, each a pong, the gaps between them past the idle threshold together; the lane sends nothing
/// but its subscribe: one session, never an `Error`.
fn keepalive_cadence(
    venue: &dyn MarketLinkVenue,
    lane: DataClientLane,
    symbol: &str,
    knobs: &WebsocketSessionKnobs,
) -> Result<(), String> {
    let cut = |run: &Run| {
        run.opens.len() != 1
            || run.states.iter().any(|(_, state, _)| *state == DataClientConnectionState::Error)
    };
    let Some(every) = knobs.keepalive_every else {
        if venue.ping().is_some() {
            return Err("the impl names a ping and the pump row declares no keepalive_every".into());
        }
        let gap = knobs.idle_threshold.map_or(knobs.read_timeout, |idle| idle / 2);
        let ping = || UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Ping(Vec::new())));
        let steps = (0..3).flat_map(|_| [ping(), UserScriptStep::Stall(gap)]).collect();
        let run = rig(
            venue,
            lane,
            symbol,
            vec![Session { steps: opened(venue, lane, symbol, steps), answers: false }],
        );
        if cut(&run) {
            return Err(format!(
                "a lane the server kept pinging was cut: opens {:?}, states {:?}",
                run.opens, run.states
            ));
        }
        let ours = past_the_subscribe(&run.sent, run.opens[0], knobs, None)?;
        if run.pongs.len() != 3 || !ours.is_empty() {
            return Err(format!(
                "{} pongs to 3 server pings, and frames of our own {ours:?}",
                run.pongs.len()
            ));
        }
        return Ok(());
    };
    let (ping, _) =
        venue.ping().ok_or("the pump row declares a keepalive and the impl names no ping")?;
    let idle = knobs.idle_threshold.ok_or("the pump row declares no idle_threshold")?;
    let quiet = 2 * idle + every / 2;
    let run = rig(
        venue,
        lane,
        symbol,
        vec![Session {
            steps: opened(venue, lane, symbol, vec![UserScriptStep::Stall(quiet)]),
            answers: true,
        }],
    );
    if cut(&run) {
        return Err(format!(
            "a quiet lane the venue kept answering was cut: opens {:?}, states {:?}",
            run.opens, run.states
        ));
    }
    let pings = past_the_subscribe(&run.sent, run.opens[0], knobs, Some(ping))?;
    // The cadence runs from the session's start in the driver.
    let start = venue.reads_before_the_driver(lane);
    let want: Vec<(Duration, String)> = (1..)
        .map(|n| start + every * n)
        .take_while(|at| *at <= quiet)
        .map(|at| (run.opens[0] + at, ping.to_string()))
        .collect();
    if pings != want.as_slice() {
        return Err(format!("sent {pings:?}; the row's cadence and the venue's ping are {want:?}"));
    }
    Ok(())
}
