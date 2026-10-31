//! Live Binance MARKET data → the vike-core tick/L2 lanes (R8 HFT track): the venue face of
//! the shared [`crate::family::depth`]. The GUI's `market_feed.rs` feeds only klines; this feeds
//! the SUB-BAR lanes the HFT primitives added: `<sym>@bookTicker` → `QuoteTick` (on_quote_tick),
//! `<sym>@trade` → `TradeTick` (on_trade_tick), `<sym>@depth@100ms` → `L2Book` (on_order_book).
//! A bare symbol is the SPOT book; a `.P` symbol (the lane twin `binance#PERP`'s `BTCUSDT.P`) is the
//! USDⓈ-M perpetual's, on the futures hosts (`pump_wire`).
//!
//! Aster forks these public streams verbatim, so the DECODERS + the depth-sync state machine + the
//! WS pump live once in the family module and both venues call in, each supplying its own venue
//! string and its own hosts (F8/F11, dedup rung 2). What stays HERE is exactly the Binance-specific
//! part: the mainnet host consts and the spot `/api/v3/depth` snapshot fetch. See the family module
//! for the U/u sequence-rule contract. The tests below stay HERE, exercising the shared code through
//! Binance's own wrapper: Aster's twin does the same through its own, so the one shared
//! implementation is proven twice.

use vike_bridge_core::poller::StopHandle;
use vike_exec::TickSender;

use crate::family::depth::{
    PumpSpec, combined_stream_url, depth_seed_agent, depth_snapshot_url, parse_depth_snapshot,
};

// The pure protocol, re-exported under this module's existing public paths so callers (and
// `market_feed.rs`'s DOM depth lane) are unchanged. The pump handle is NOT here any more: it is
// `vike_bridge_core::poller::StopHandle`, the one stop-and-join owner every depth pump shares.
pub use crate::family::depth::{
    DepthOutcome, DepthSnapshot, MdEvent, apply_depth_event, decode_book_ticker, decode_trade,
    route_frame,
};

// The venue id is the crate's, not this module's: it was a private third copy of the same literal
// until the exec/feeds seam gave `crate::VENUE` a home both planes can see. One definition, so the
// string this feed stamps on a tick and the string the signed order path sends cannot drift apart.
use crate::VENUE;

pub const MAINNET_WS: &str = "wss://stream.binance.com:9443";
pub const MAINNET_REST: &str = "https://api.binance.com";

/// Binance's spot depth-snapshot path. A bare symbol's tick track tracks the SPOT book (Aster's twin
/// is futures-only, its central use case being perps); a `.P` symbol snapshots `PERP_DEPTH_PATH`.
const SPOT_DEPTH_PATH: &str = "api/v3/depth";

/// USDⓈ-M futures REST origin — the perp half of [`fetch_depth_snapshot_for`]'s host pair.
pub const PERP_REST: &str = "https://fapi.binance.com";
/// USDⓈ-M futures depth-snapshot path. Binance serves perp depth at `v1` (its aggTrades sibling is
/// `/fapi/v1/aggTrades` too); the response carries the same `lastUpdateId`/`bids`/`asks` the spot
/// one does, plus `E`/`T` the parser ignores — so [`parse_depth_snapshot`] is shared, verified
/// against the live endpoint.
const PERP_DEPTH_PATH: &str = "fapi/v1/depth";

/// GET /api/v3/depth → the snapshot to seed the book before applying diffs. The host is a plain
/// argument (Binance has exactly one mainnet REST host per instrument class — nothing here is
/// env-resolved); the parse is [`crate::family::depth::parse_depth_snapshot`].
pub fn fetch_depth_snapshot(
    rest: &str,
    symbol: &str,
) -> Result<DepthSnapshot, Box<dyn std::error::Error>> {
    let url = depth_snapshot_url(rest, SPOT_DEPTH_PATH, symbol);
    // The BOUNDED seed agent, not the shared 30 s pager — see `family::depth`'s `DEPTH_SEED_TIMEOUT`
    // for why a call between a depth session's dial and its first stop poll is budgeted differently
    // from one in a backfill loop. 4xx/5xx still come back as responses, so a 429/418 body simply
    // fails the `lastUpdateId` parse below.
    let agent = depth_seed_agent();
    let mut resp = agent.get(url.as_str()).call()?;
    parse_depth_snapshot(&resp.body_mut().read_to_string()?)
}

/// The depth snapshot for EITHER instrument class, picking host **and** path together from `perp`.
///
/// Taking the flag rather than a raw host is the point: host and path must agree, and
/// [`fetch_depth_snapshot`] above (which takes a host and hardcodes the spot path) makes it possible
/// to pass `PERP_REST` and silently build `fapi.binance.com/api/v3/depth`, a 404 that surfaces as a
/// parse failure — which the DOM lane's seed treats as transient and retries forever. Aster's twin
/// already resolves its host internally for exactly this reason; this brings Binance's in line.
pub fn fetch_depth_snapshot_for(
    perp: bool,
    symbol: &str,
) -> Result<DepthSnapshot, Box<dyn std::error::Error>> {
    let url = depth_snapshot_url_for(perp, symbol);
    // The BOUNDED seed agent — this is the recorder's own depth lane (`Stream::Depth` →
    // `subscribe_depth` → `family::market_feed`'s `depth_main`), so this call is spent from
    // `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS`.
    let agent = depth_seed_agent();
    let mut resp = agent.get(url.as_str()).call()?;
    parse_depth_snapshot(&resp.body_mut().read_to_string()?)
}

/// [`fetch_depth_snapshot_for`]'s URL, split out so the host/path pairing is testable without the
/// network — the pairing IS the thing that was wrong.
pub fn depth_snapshot_url_for(perp: bool, symbol: &str) -> String {
    let (rest, path) =
        if perp { (PERP_REST, PERP_DEPTH_PATH) } else { (MAINNET_REST, SPOT_DEPTH_PATH) };
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
/// is `crates/bridges/aster/src/market_data.rs`'s `pump_wire`.
fn pump_wire(symbol: &str) -> (String, &str, bool) {
    let (api_symbol, perp) = vike_catalog::split_perp_at(VENUE, symbol);
    (combined_stream_url(crate::market_feed::BINANCE_URLS.ws(perp), api_symbol), api_symbol, perp)
}

/// Spawn the Binance market-data feed: combined bookTicker + trade + depth streams pushed into the
/// core's tick/L2 lanes via `ticks`. A bare `symbol` is the SPOT book; a `.P` one is the USDⓈ-M perpetual
/// (futures stream host, `fapi` depth seed), its ticks labelled with the caller's own `.P` spelling
/// (`pump_wire`). `tick_size` sizes the `L2Book` price grid. Stoppable.
pub fn spawn_binance_market_data(ticks: TickSender, symbol: &str, tick_size: f64) -> StopHandle {
    let (ws_url, api_symbol, perp) = pump_wire(symbol);
    let snapshot_symbol = api_symbol.to_string();
    let spec = PumpSpec {
        venue: VENUE,
        ws_url,
        fetch_snapshot: Box::new(move || fetch_depth_snapshot_for(perp, &snapshot_symbol)),
    };
    crate::family::depth::spawn_market_data(spec, VENUE, ticks, symbol, tick_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_marketdata::BookLevel;
    use vike_marketdata::L2Book;

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
            MdEvent::Quote(q) => {
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
            MdEvent::Trade(t) => {
                assert_eq!(t.ts, 1234);
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
        // snapshot seeds last_seq = 100
        book.apply_snapshot(100, &[BookLevel::new(60000.0, 5.0)], &[BookLevel::new(60001.0, 5.0)]);
        assert_eq!(book.last_seq, 100);

        // stale: u <= 100 -> dropped, no change
        let stale = combined(
            "btcusdt@depth",
            serde_json::json!({"U":90,"u":100,"b":[["60000.0","9"]],"a":[]}),
        );
        assert_eq!(route_frame(&stale, "BTCUSDT", &mut book), MdEvent::Ignored);
        assert_eq!(book.best_bid(), Some(BookLevel::new(60000.0, 5.0)), "stale must not apply");

        // contiguous: U(101) == last_seq+1 -> applied, best bid qty updated
        let ok = combined(
            "btcusdt@depth",
            serde_json::json!({"U":101,"u":105,"b":[["60000.0","8"]],"a":[["60001.0","0"]]}),
        );
        assert_eq!(route_frame(&ok, "BTCUSDT", &mut book), MdEvent::BookUpdated);
        assert_eq!(book.best_bid(), Some(BookLevel::new(60000.0, 8.0)));
        assert_eq!(book.best_ask(), None, "qty 0 removed the ask");
        assert_eq!(book.last_seq, 105);

        // gap: U(200) > last_seq+1(106) -> Resync
        let gap = combined(
            "btcusdt@depth",
            serde_json::json!({"U":200,"u":210,"b":[["60000.0","1"]],"a":[]}),
        );
        assert_eq!(route_frame(&gap, "BTCUSDT", &mut book), MdEvent::Resync);
        assert_eq!(book.last_seq, 105, "a gap must not advance the book");
    }

    #[test]
    fn unknown_stream_is_ignored() {
        let mut book = L2Book::new(0.1);
        assert_eq!(
            route_frame(&combined("btcusdt@kline_1m", serde_json::json!({})), "BTCUSDT", &mut book),
            MdEvent::Ignored
        );
        assert_eq!(route_frame("not json", "BTCUSDT", &mut book), MdEvent::Ignored);
    }

    /// The mainnet hosts + the spot depth path this venue face pins — the URLs the family's shared
    /// pump/snapshot builders resolve to for Binance. Guards the rung-2 extraction: these must stay
    /// byte-identical to the pre-extraction inline `format!`s.
    #[test]
    fn binance_urls_are_spot_mainnet_and_unchanged() {
        assert_eq!(
            combined_stream_url(MAINNET_WS, "BTCUSDT"),
            "wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
        );
        assert_eq!(
            depth_snapshot_url(MAINNET_REST, SPOT_DEPTH_PATH, "BTCUSDT"),
            "https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=1000"
        );
    }

    /// **A `.P` symbol is pumped from the FUTURES hosts** (the lane twin `binance#PERP`'s `BTCUSDT.P`): the
    /// stream names and the depth seed carry the exchange symbol, the seed goes to `fapi`, and the ticks
    /// keep the caller's `.P` label. The v0.1.78 pump took the raw `.P` symbol onto the spot host: a stream
    /// nothing serves and a seed that answers `-1121`, so the twin was never priced.
    #[test]
    fn a_perp_symbol_is_pumped_from_the_futures_hosts_and_seeded_from_fapi() {
        let (ws_url, api_symbol, perp) = pump_wire("BTCUSDT.P");
        assert_eq!(
            ws_url,
            "wss://fstream.binance.com/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
        );
        assert_eq!((api_symbol, perp), ("BTCUSDT", true));
        assert_eq!(
            depth_snapshot_url_for(perp, api_symbol),
            "https://fapi.binance.com/fapi/v1/depth?symbol=BTCUSDT&limit=1000"
        );
        assert!(!ws_url.contains(".p@"), "no stream is named with the lane suffix: {ws_url}");
    }

    /// A bare symbol is the spot book, byte for byte what the pump always dialled and seeded from.
    #[test]
    fn a_spot_symbol_is_pumped_exactly_as_before() {
        let (ws_url, api_symbol, perp) = pump_wire("BTCUSDT");
        assert_eq!(ws_url, combined_stream_url(MAINNET_WS, "BTCUSDT"));
        assert_eq!((api_symbol, perp), ("BTCUSDT", false));
        assert_eq!(
            depth_snapshot_url_for(perp, api_symbol),
            depth_snapshot_url(MAINNET_REST, SPOT_DEPTH_PATH, "BTCUSDT")
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
            MdEvent::Quote(q) => {
                assert_eq!((q.bid, q.ask), (60000.1, 60000.2));
                assert_eq!(q.symbol, "BTCUSDT.P");
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }
}
