//! Live Binance-Spot MARKET data → the vike-core tick/L2 lanes (R8 HFT track): the venue face of
//! the shared [`crate::family::depth`]. The GUI's `market_feed.rs` feeds only klines; this feeds
//! the SUB-BAR lanes the HFT primitives added: `<sym>@bookTicker` → `QuoteTick` (on_quote_tick),
//! `<sym>@trade` → `TradeTick` (on_trade_tick), `<sym>@depth@100ms` → `L2Book` (on_order_book).
//!
//! Aster forks these public streams verbatim, so the DECODERS + the depth-sync state machine + the
//! WS pump live once in the family module and both venues call in, each supplying its own venue
//! string and its own hosts (F8/F11, dedup rung 2). What stays HERE is exactly the Binance-specific
//! part: the mainnet host consts and the spot `/api/v3/depth` snapshot fetch. See the family module
//! for the U/u sequence-rule contract. The tests below stay HERE, exercising the shared code through
//! Binance's own wrapper: Aster's twin does the same through its own, so the one shared
//! implementation is proven twice.

use vike_exec::TickSender;

use crate::family::depth::{
    PumpSpec, combined_stream_url, depth_seed_agent, depth_snapshot_url, parse_depth_snapshot,
};

// The pure protocol + the pump handle, re-exported under this module's existing public paths so
// callers (and `market_feed.rs`'s DOM depth lane) are unchanged.
pub use crate::family::depth::{
    DepthOutcome, DepthSnapshot, MarketDataFeed, MdEvent, apply_depth_event, decode_book_ticker,
    decode_trade, route_frame,
};

// The venue id is the crate's, not this module's: it was a private third copy of the same literal
// until the exec/feeds seam gave `crate::VENUE` a home both planes can see. One definition, so the
// string this feed stamps on a tick and the string the signed order path sends cannot drift apart.
use crate::VENUE;

pub const MAINNET_WS: &str = "wss://stream.binance.com:9443";
pub const MAINNET_REST: &str = "https://api.binance.com";

/// Binance's spot depth-snapshot path. Spot-only by design — this HFT tick track tracks the SPOT
/// book (unlike Aster's twin, whose central use case is perps and which therefore snapshots
/// `/fapi/v3/depth`).
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

/// Spawn the Binance-spot market-data feed: combined bookTicker + trade + depth streams pushed
/// into the core's tick/L2 lanes via `ticks`. `tick_size` sizes the `L2Book` price grid. Stoppable.
pub fn spawn_binance_market_data(
    ticks: TickSender,
    symbol: &str,
    tick_size: f64,
) -> MarketDataFeed {
    let snapshot_symbol = symbol.to_string();
    let spec = PumpSpec {
        venue: VENUE,
        ws_url: combined_stream_url(MAINNET_WS, symbol),
        fetch_snapshot: Box::new(move || fetch_depth_snapshot(MAINNET_REST, &snapshot_symbol)),
    };
    crate::family::depth::spawn_market_data(spec, VENUE, ticks, symbol, tick_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::L2Book;

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
        book.apply_snapshot(100, &[(60000.0, 5.0)], &[(60001.0, 5.0)]);
        assert_eq!(book.last_seq, 100);

        // stale: u <= 100 -> dropped, no change
        let stale = combined(
            "btcusdt@depth",
            serde_json::json!({"U":90,"u":100,"b":[["60000.0","9"]],"a":[]}),
        );
        assert_eq!(route_frame(&stale, "BTCUSDT", &mut book), MdEvent::Ignored);
        assert_eq!(book.best_bid(), Some((60000.0, 5.0)), "stale must not apply");

        // contiguous: U(101) == last_seq+1 -> applied, best bid qty updated
        let ok = combined(
            "btcusdt@depth",
            serde_json::json!({"U":101,"u":105,"b":[["60000.0","8"]],"a":[["60001.0","0"]]}),
        );
        assert_eq!(route_frame(&ok, "BTCUSDT", &mut book), MdEvent::BookUpdated);
        assert_eq!(book.best_bid(), Some((60000.0, 8.0)));
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
}
