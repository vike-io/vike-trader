//! Live Binance MARKET data → the vike-core tick/L2 lanes (R8 HFT track): the venue face of
//! the shared [`crate::family::feed::tick`]. The GUI's `feed/mod.rs` feeds only klines; this feeds
//! the SUB-BAR lanes the HFT primitives added: `<sym>@bookTicker` → `TickQuote` (on_quote_tick),
//! `<sym>@trade` → `TickTrade` (on_trade_tick), `<sym>@depth@100ms` → `L2Book` (on_order_book).
//! A bare symbol is the SPOT book; a `.P` symbol (the lane twin `binance#PERP`'s `BTCUSDT.P`) is the
//! USDⓈ-M perpetual's, on the futures hosts (`pump_wire`), and a COIN-M one (`BTCUSD_PERP.P`) the COIN-M
//! book's, on `dstream` with its depth seeded from `dapi` (`crate::feed::BinanceTables::urls_for`).
//!
//! Aster forks these public streams verbatim, so the DECODERS + the depth-sync state machine + the
//! WS pump live once in the family module and both venues call in, each supplying its own venue
//! string and its own hosts (F8/F11, dedup rung 2). What stays HERE is exactly the Binance-specific
//! part: the depth paths (the hosts are binance's `venue_url` rows, `crate::feed::BinanceTables`)
//! and the spot `/api/v3/depth` snapshot fetch. See the family module
//! for the U/u sequence-rule contract. The tests below stay HERE, exercising the shared code through
//! Binance's own wrapper: Aster's twin does the same through its own, so the one shared
//! implementation is proven twice.

pub(crate) mod trades;

use vike_execution::TickSender;
use vike_utility::poller::StopHandle;
use vike_venues::venue_urls::VenueUrls;

use crate::family::feed::tick::{
    FeedTickSpec, combined_stream_url, depth_seed_agent, depth_snapshot_url, parse_depth_snapshot,
};
use crate::feed::BinanceTables;

// The pure protocol, re-exported under this module's existing public paths so callers (and
// `feed/mod.rs`'s DOM depth lane) are unchanged. The pump handle is NOT here any more: it is
// `vike_utility::poller::StopHandle`, the one stop-and-join owner every depth pump shares.
pub use crate::family::feed::tick::{
    DepthOutcome, DepthSnapshot, apply_depth_event, decode_book_ticker, decode_trade, route_frame,
};

// The venue id is the crate's, not this module's: it was a private third copy of the same literal
// until the exec/feeds seam gave `crate::VENUE` a home both planes can see. One definition, so the
// string this feed stamps on a tick and the string the signed order path sends cannot drift apart.
use crate::VENUE;

/// Binance's spot depth-snapshot path. A bare symbol's tick track tracks the SPOT book (Aster's twin
/// is futures-only, its central use case being perps); a `.P` symbol snapshots `PERP_DEPTH_PATH`.
const SPOT_DEPTH_PATH: &str = "api/v3/depth";

/// USDⓈ-M futures depth-snapshot path, on the USDⓈ-M REST origin (the perp half of
/// [`fetch_depth_snapshot_for`]'s host pair). Binance serves perp depth at `v1` (its aggTrades sibling is
/// `/fapi/v1/aggTrades` too); the response carries the same `lastUpdateId`/`bids`/`asks` the spot
/// one does, plus `E`/`T` the parser ignores — so [`parse_depth_snapshot`] is shared, verified
/// against the live endpoint.
const PERP_DEPTH_PATH: &str = "fapi/v1/depth";
/// COIN-M futures depth-snapshot path, on `crate::feed::BinanceTables::coin_m`'s REST origin: the
/// same `lastUpdateId`/`bids`/`asks` body plus `symbol`, `pair`, `E` and `T` (measured 2026-11-08), sizes
/// in contracts.
const COIN_M_DEPTH_PATH: &str = "dapi/v1/depth";

/// GET /api/v3/depth → the snapshot to seed the book before applying diffs. The host is a plain
/// argument (the caller's: a `venue_url` row — nothing here is env-resolved); the parse is
/// [`crate::family::feed::tick::parse_depth_snapshot`].
pub fn fetch_depth_snapshot(
    rest: &str,
    symbol: &str,
) -> Result<DepthSnapshot, Box<dyn std::error::Error>> {
    let url = depth_snapshot_url(rest, SPOT_DEPTH_PATH, symbol);
    // The BOUNDED seed agent, not the shared 30 s pager — see `family::feed::tick`'s `DEPTH_SEED_TIMEOUT`
    // for why a call between a depth session's dial and its first stop poll is budgeted differently
    // from one in a backfill loop. 4xx/5xx still come back as responses, so a 429/418 body simply
    // fails the `lastUpdateId` parse below.
    let agent = depth_seed_agent();
    let mut response = agent.get(url.as_str()).call()?;
    parse_depth_snapshot(&response.body_mut().read_to_string()?)
}

/// The depth snapshot for EITHER instrument class, picking host **and** path together from `perp`.
///
/// Taking the flag rather than a raw host is the point: host and path must agree, and
/// [`fetch_depth_snapshot`] above (which takes a host and hardcodes the spot path) makes it possible
/// to pass the fapi origin and silently build `fapi.binance.com/api/v3/depth`, a 404 that surfaces as a
/// parse failure — which the DOM lane's seed treats as transient and retries forever. Aster's twin
/// already resolves its host internally for exactly this reason; this brings Binance's in line. The
/// hosts are `tables`' (binance's `venue_url` rows).
pub fn fetch_depth_snapshot_for(
    tables: &BinanceTables,
    perp: bool,
    symbol: &str,
) -> Result<DepthSnapshot, Box<dyn std::error::Error>> {
    let url = depth_snapshot_url_for(tables, perp, symbol);
    // The BOUNDED seed agent — this is the recorder's own depth lane (`Stream::Depth` →
    // `subscribe_depth` → `family::feed`'s `depth_main`), so this call is spent from
    // `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`.
    let agent = depth_seed_agent();
    let mut response = agent.get(url.as_str()).call()?;
    parse_depth_snapshot(&response.body_mut().read_to_string()?)
}

/// [`fetch_depth_snapshot_for`]'s URL, split out so the host/path pairing is testable without the
/// network — the pairing IS the thing that was wrong. A perp `symbol` with a COIN-M name (the rule
/// `crate::feed::BinanceTables::urls_for` reads) seeds from `dapi`.
pub fn depth_snapshot_url_for(tables: &BinanceTables, perp: bool, symbol: &str) -> String {
    let (rest, path) = if !perp {
        (tables.linear.spot_rest, SPOT_DEPTH_PATH)
    } else if crate::instrument::names::is_coin_m(symbol) {
        (tables.coin_m.perp_rest, COIN_M_DEPTH_PATH)
    } else {
        (tables.linear.perp_rest, PERP_DEPTH_PATH)
    };
    depth_snapshot_url(rest, path, symbol)
}

/// The pump's WIRE plumbing for a start `symbol`, pure: `(combined-stream WS URL, exchange symbol, perp)`.
///
/// ⚠ The SERIES symbol and the WIRE symbol are different strings for a perpetual (`BTCUSDT.P`), and the
/// pump needs BOTH: every emitted tick keeps the caller's own spelling as its label (the core dispatches
/// it to the engine that trades `BTCUSDT.P`), while the stream names and both depth endpoints only know
/// `BTCUSDT`, on the futures hosts. Built from the raw `.P` symbol the pump dialled `btcusdt.p@bookTicker`
/// on the SPOT host (a stream nothing serves) and seeded from `api/v3/depth?symbol=BTCUSDT.P` (`-1121
/// Invalid symbol`): it connected, said nothing and delivered no tick, for ever. That is what left the lane
/// twin `binance#PERP` unpriced in v0.1.78 and its first order held for a price that never came. A spot
/// symbol splits to itself, so every spot caller is byte-identical to before. Aster's twin of this split
/// is `crates/bridges/aster/src/feed/tick/mod.rs`'s `pump_wire`. A COIN-M symbol's streams are `dstream`'s
/// (`crate::feed::BinanceTables::urls_for`): `fstream` lists no COIN-M symbol.
fn pump_wire<'a>(tables: &BinanceTables, symbol: &'a str) -> (String, &'a str, bool) {
    let (api_symbol, perp) = vike_catalog::split_perp_at(VENUE, symbol);
    (
        combined_stream_url(&tables.urls_for(symbol).public_websocket(perp), api_symbol),
        api_symbol,
        perp,
    )
}

/// Spawn the Binance market-data feed: combined bookTicker + trade + depth streams pushed into the
/// core's tick/L2 lanes via `ticks`. A bare `symbol` is the SPOT book; a `.P` one is the USDⓈ-M perpetual
/// (futures stream host, `fapi` depth seed) or a COIN-M contract (`dstream`, `dapi` depth seed), its ticks
/// labelled with the caller's own `.P` spelling (`pump_wire`). `tick_size` sizes the `L2Book` price grid.
/// The hosts are binance's `venue_url` rows `urls` at `live` `data` (`crate::feed::BinanceTables`).
/// Stoppable. `symbol` is a USER name (decision 0290): the pump dials its KEPT row's lane name (`BTCUSD.P` is
/// `BTCUSD_PERP.P`, a dated `BTCUSDT_261225` the futures `BTCUSDT_261225.P`) and labels every tick `symbol`.
///
/// # Errors
/// A row is missing, or `symbol` names no kept row: nothing is dialled, the refusal names which.
pub fn spawn_binance_feed_tick(
    urls: &VenueUrls,
    ticks: TickSender,
    symbol: &str,
    tick_size: f64,
) -> Result<StopHandle, String> {
    let tables = BinanceTables::from_urls(urls).map_err(|e| e.to_string())?;
    let lane = crate::instrument::names::lane_name_of(symbol).map_err(|r| r.to_string())?;
    let (websocket_url, api_symbol, perp) = pump_wire(&tables, &lane);
    let snapshot_symbol = api_symbol.to_string();
    let spec = FeedTickSpec {
        venue: VENUE,
        websocket_url,
        fetch_snapshot: Box::new(move || fetch_depth_snapshot_for(&tables, perp, &snapshot_symbol)),
    };
    Ok(crate::family::feed::tick::spawn_feed_tick(spec, ticks, symbol, tick_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::data_client::event::DataClientEvent;
    use vike_marketdata::L2Book;
    use vike_marketdata::L2BookLevel;

    /// The hosts a fresh box's rows name (the release's seed).
    fn seeded() -> BinanceTables {
        crate::feed::seeded_tables()
    }

    fn combined(stream: &str, data: serde_json::Value) -> String {
        serde_json::json!({ "stream": stream, "data": data }).to_string()
    }

    #[test]
    fn book_ticker_decodes_to_quote() {
        let frame = combined(
            "btcusdt@bookTicker",
            serde_json::json!({"u":1,"s":"BTCUSDT","b":"60000.1","B":"1.5","a":"60000.2","A":"2.0"}),
        );
        let mut book = L2Book::new(0.1);
        match route_frame(&frame, "BTCUSDT", &mut book) {
            DataClientEvent::Quote(q) => {
                assert_eq!(q.bid, 60000.1);
                assert_eq!(q.ask, 60000.2);
                assert_eq!(q.bid_size, 1.5);
                assert_eq!(q.ask_size, 2.0);
                assert_eq!(q.symbol, "BTCUSDT");
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    #[test]
    fn trade_decodes_to_trade_tick() {
        let frame = combined(
            "btcusdt@trade",
            serde_json::json!({"e":"trade","T":1234,"s":"BTCUSDT","p":"60000.5","q":"0.3","m":true}),
        );
        let mut book = L2Book::new(0.1);
        match route_frame(&frame, "BTCUSDT", &mut book) {
            DataClientEvent::Trade(t) => {
                assert_eq!(t.timestamp, 1234);
                assert_eq!(t.price, 60000.5);
                assert_eq!(t.size, 0.3);
                assert!(t.is_buyer_maker);
            }
            other => panic!("expected Trade, got {other:?}"),
        }
    }

    #[test]
    fn depth_sync_snapshot_delta_stale_and_gap() {
        let mut book = L2Book::new(0.1);
        // snapshot seeds last_sequence = 100
        book.apply_snapshot(
            100,
            &[L2BookLevel::new(60000.0, 5.0)],
            &[L2BookLevel::new(60001.0, 5.0)],
        );
        assert_eq!(book.last_sequence, 100);

        // stale: u <= 100 -> dropped, no change
        let stale = combined(
            "btcusdt@depth",
            serde_json::json!({"U":90,"u":100,"b":[["60000.0","9"]],"a":[]}),
        );
        assert_eq!(route_frame(&stale, "BTCUSDT", &mut book), DataClientEvent::Ignored);
        assert_eq!(book.best_bid(), Some(L2BookLevel::new(60000.0, 5.0)), "stale must not apply");

        // contiguous: U(101) == last_sequence+1 -> applied, best bid qty updated
        let ok = combined(
            "btcusdt@depth",
            serde_json::json!({"U":101,"u":105,"b":[["60000.0","8"]],"a":[["60001.0","0"]]}),
        );
        assert_eq!(
            route_frame(&ok, "BTCUSDT", &mut book),
            DataClientEvent::BookUpdated { timestamp_ms: 0 }
        );
        assert_eq!(book.best_bid(), Some(L2BookLevel::new(60000.0, 8.0)));
        assert_eq!(book.best_ask(), None, "qty 0 removed the ask");
        assert_eq!(book.last_sequence, 105);

        // gap: U(200) > last_sequence+1(106) -> Resync
        let gap = combined(
            "btcusdt@depth",
            serde_json::json!({"U":200,"u":210,"b":[["60000.0","1"]],"a":[]}),
        );
        assert_eq!(route_frame(&gap, "BTCUSDT", &mut book), DataClientEvent::Resync);
        assert_eq!(book.last_sequence, 105, "a gap must not advance the book");
    }

    #[test]
    fn unknown_stream_is_ignored() {
        let mut book = L2Book::new(0.1);
        assert_eq!(
            route_frame(&combined("btcusdt@kline_1m", serde_json::json!({})), "BTCUSDT", &mut book),
            DataClientEvent::Ignored
        );
        assert_eq!(route_frame("not json", "BTCUSDT", &mut book), DataClientEvent::Ignored);
    }

    /// The mainnet hosts + the spot depth path this venue face pins — the URLs the family's shared
    /// pump/snapshot builders resolve to for Binance, from the release's rows. Guards the rung-2
    /// extraction and the move into rows: these must stay byte-identical to the inline `format!`s.
    #[test]
    fn binance_urls_are_spot_mainnet_and_unchanged() {
        let t = seeded().linear;
        assert_eq!(
            combined_stream_url(t.spot_websocket, "BTCUSDT"),
            "wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
        );
        assert_eq!(
            depth_snapshot_url(t.spot_rest, SPOT_DEPTH_PATH, "BTCUSDT"),
            "https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=1000"
        );
        assert!(BinanceTables::from_urls(&VenueUrls::default()).is_err(), "no row, no pump");
    }

    /// **A `.P` symbol is pumped from the FUTURES hosts** (the lane twin `binance#PERP`'s `BTCUSDT.P`): the
    /// stream names and the depth seed carry the exchange symbol, the seed goes to `fapi`, and the ticks
    /// keep the caller's `.P` label. The v0.1.78 pump took the raw `.P` symbol onto the spot host: a stream
    /// nothing serves and a seed that answers `-1121`, so the twin was never priced.
    #[test]
    fn a_perp_symbol_is_pumped_from_the_futures_hosts_and_seeded_from_fapi() {
        let (websocket_url, api_symbol, perp) = pump_wire(&seeded(), "BTCUSDT.P");
        assert_eq!(
            websocket_url,
            "wss://fstream.binance.com/public/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
        );
        assert_eq!((api_symbol, perp), ("BTCUSDT", true));
        assert_eq!(
            depth_snapshot_url_for(&seeded(), perp, api_symbol),
            "https://fapi.binance.com/fapi/v1/depth?symbol=BTCUSDT&limit=1000"
        );
        assert!(
            !websocket_url.contains(".p@"),
            "no stream is named with the lane suffix: {websocket_url}"
        );
    }

    /// **A COIN-M symbol is pumped from `dstream` and seeded from `dapi`**, and its RECORDED frames decode
    /// under the caller's `.P` label. Recorded from `wss://dstream.binance.com/public/stream` (bookTicker, trade,
    /// depth@100ms; public, no account) and `GET https://dapi.binance.com/dapi/v1/depth` on 2026-11-08. The
    /// sizes are the venue's quantity, CONTRACTS (100 USD each on BTC), the unit a COIN-M order is in.
    #[test]
    fn a_coin_m_symbol_is_pumped_from_dstream_and_seeded_from_dapi() {
        let (websocket_url, api_symbol, perp) = pump_wire(&seeded(), "BTCUSD_PERP.P");
        assert_eq!(
            websocket_url,
            "wss://dstream.binance.com/public/stream?streams=btcusd_perp@bookTicker/btcusd_perp@trade/btcusd_perp@depth@100ms"
        );
        assert_eq!((api_symbol, perp), ("BTCUSD_PERP", true));
        assert_eq!(
            depth_snapshot_url_for(&seeded(), perp, api_symbol),
            "https://dapi.binance.com/dapi/v1/depth?symbol=BTCUSD_PERP&limit=1000"
        );

        let snapshot = r#"{"symbol":"BTCUSD_PERP","pair":"BTCUSD","lastUpdateId":12049825961016,"E":1794160421649,"T":1794160421646,"bids":[["75632.8","10139"],["75632.7","7"],["75632.0","51"],["75631.8","486"],["75631.7","3"]],"asks":[["75632.9","43"],["75633.5","1"],["75635.4","2"],["75635.7","133"],["75635.8","32"]]}"#;
        let (seq, bids, asks) = parse_depth_snapshot(snapshot).expect("dapi's snapshot parses");
        assert_eq!((seq, bids.len(), asks.len()), (12049825961016, 5, 5));
        assert_eq!(
            (bids[0], asks[0]),
            (L2BookLevel::new(75632.8, 10139.0), L2BookLevel::new(75632.9, 43.0))
        );

        let mut book = L2Book::new(0.1);
        let ticker = r#"{"stream":"btcusd_perp@bookTicker","data":{"e":"bookTicker","u":12049811784400,"s":"BTCUSD_PERP","ps":"BTCUSD","b":"75639.9","B":"10562","a":"75640.0","A":"4563","T":1794160196960,"E":1794160196960,"st":2}}"#;
        match route_frame(ticker, "BTCUSD_PERP.P", &mut book) {
            DataClientEvent::Quote(q) => {
                assert_eq!(
                    (q.bid, q.ask, q.bid_size, q.ask_size),
                    (75639.9, 75640.0, 10562.0, 4563.0)
                );
                assert_eq!(q.symbol, "BTCUSD_PERP.P");
            }
            other => panic!("expected Quote, got {other:?}"),
        }
        let trade = r#"{"stream":"btcusd_perp@trade","data":{"e":"trade","E":1794160198677,"T":1794160198676,"s":"BTCUSD_PERP","t":1163308152,"p":"75640.0","q":"18","X":"MARKET","m":false,"st":2}}"#;
        match route_frame(trade, "BTCUSD_PERP.P", &mut book) {
            DataClientEvent::Trade(t) => {
                assert_eq!(
                    (t.timestamp, t.price, t.size, t.is_buyer_maker),
                    (1794160198676, 75640.0, 18.0, false)
                );
                assert_eq!(t.symbol, "BTCUSD_PERP.P");
            }
            other => panic!("expected Trade, got {other:?}"),
        }
        // Three consecutive diffs: each `pu` is the previous `u` (the USDⓈ-M grammar). The first one's span
        // covers an anchor at its own `U`, the documented first-post-seed shape.
        let diffs = [
            r#"{"stream":"btcusd_perp@depth@100ms","data":{"e":"depthUpdate","E":1794160399159,"T":1794160399147,"s":"BTCUSD_PERP","ps":"BTCUSD","U":12049824271678,"u":12049824276516,"pu":12049824269360,"b":[["68069.5","2"],["75629.2","2"],["75629.6","0"]],"a":[],"st":2}}"#,
            r#"{"stream":"btcusd_perp@depth@100ms","data":{"e":"depthUpdate","E":1794160399271,"T":1794160399267,"s":"BTCUSD_PERP","ps":"BTCUSD","U":12049824277873,"u":12049824282449,"pu":12049824276516,"b":[["68069.5","2"],["75254.6","1"],["75620.3","219"],["75630.6","182"],["75632.8","1189"]],"a":[],"st":2}}"#,
            r#"{"stream":"btcusd_perp@depth@100ms","data":{"e":"depthUpdate","E":1794160399381,"T":1794160399357,"s":"BTCUSD_PERP","ps":"BTCUSD","U":12049824283197,"u":12049824287616,"pu":12049824282449,"b":[["68069.5","2"],["75576.1","1"]],"a":[["75740.6","12716"],["75745.7","0"]],"st":2}}"#,
        ];
        let mut book = L2Book::new(0.1);
        book.apply_snapshot(12049824271678, &[], &[]);
        for diff in diffs {
            assert_eq!(
                route_frame(diff, "BTCUSD_PERP.P", &mut book),
                DataClientEvent::BookUpdated { timestamp_ms: 0 },
                "{diff}"
            );
        }
        assert_eq!(book.last_sequence, 12049824287616);
        assert_eq!(book.best_bid(), Some(L2BookLevel::new(75632.8, 1189.0)));
        assert_eq!(book.best_ask(), Some(L2BookLevel::new(75740.6, 12716.0)));
    }

    /// A bare symbol is the spot book, byte for byte what the pump always dialled and seeded from.
    #[test]
    fn a_spot_symbol_is_pumped_exactly_as_before() {
        let (websocket_url, api_symbol, perp) = pump_wire(&seeded(), "BTCUSDT");
        assert_eq!(
            websocket_url,
            "wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
        );
        assert_eq!((api_symbol, perp), ("BTCUSDT", false));
        assert_eq!(
            depth_snapshot_url_for(&seeded(), perp, api_symbol),
            "https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=1000"
        );
    }

    /// A futures frame (the stream carries the exchange symbol; the venue adds `e`/`E`/`T`) decodes under the
    /// caller's `.P` label: the core routes a tick to the engine that trades that exact string.
    #[test]
    fn a_futures_frame_is_labelled_with_the_callers_perp_symbol() {
        let frame = combined(
            "btcusdt@bookTicker",
            serde_json::json!({
                "e":"bookTicker","u":400900217,"E":1568014460893_i64,"T":1568014460891_i64,
                "s":"BTCUSDT","b":"60000.1","B":"1.5","a":"60000.2","A":"2.0"
            }),
        );
        let mut book = L2Book::new(0.1);
        match route_frame(&frame, "BTCUSDT.P", &mut book) {
            DataClientEvent::Quote(q) => {
                assert_eq!((q.bid, q.ask), (60000.1, 60000.2));
                assert_eq!(q.symbol, "BTCUSDT.P");
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }
}
