//! Live Aster public MARKET data → the vike-core tick/L2 lanes (R8 HFT track): the venue face of
//! the shared [`vike_binance::family::depth`]. Distinct from [`crate::market_feed::Feeds`] (the
//! `vike_data::DataClient`/`LiveDataSink` GUI seam): this feeds the SUB-BAR lanes a mounted
//! `Strategy` reads directly via a `vike_exec::TickSender`: `<sym>@bookTicker` → `QuoteTick`
//! (on_quote_tick), `<sym>@trade` → `TradeTick` (on_trade_tick), `<sym>@depth@100ms` → `L2Book`
//! (on_order_book).
//!
//! Aster's public streams are a verbatim Binance fork (same stream names, same field letters, same
//! `U`/`u` depth-recovery rule), so this module's decoders + depth-sync state machine + WS pump —
//! previously a byte-for-byte copy of Binance's — now live once in the Binance-wire-grammar core
//! and both venues call in, each supplying its own venue string and hosts (F8/F11, dedup rung 2).
//! What stays HERE is exactly Aster's own part: `env`-keyed host resolution and the spot-vs-futures
//! depth-path split. See the family module for the U/u sequence-rule contract. The tests below stay
//! HERE, exercising the shared code through Aster's own wrapper: Binance's twin does the same
//! through its own, so the one shared implementation is proven twice.
//!
//! **Futures, not spot.** Unlike binance's own `market_data.rs` (spot-only) — and unlike this
//! crate's `market_feed.rs`, whose DOM depth lane deliberately stays spot-only, matching binance's
//! template choice verbatim — this HFT tick track combined-streams and snapshots against the USDⓈ-M
//! FUTURES host (`fapi_ws`/`fapi_rest`), per the design spec's explicit `GET /fapi/v3/depth`
//! endpoint: Aster's central use case is perp trading, so the tick track a mounted maker/directional
//! strategy reads from tracks the futures book. `env` (testnet/mainnet) is threaded through from the
//! spawn call — see [`crate::urls::urls_for`].
//!
//! **Consolidation note:** `DepthOutcome`/`apply_depth_event`/`fetch_depth_snapshot` are `pub` here
//! and `market_feed.rs`'s depth lane imports them (matching binance's own shape). Because this
//! module's own depth runs on futures while `market_feed.rs`'s stays on spot, [`fetch_depth_snapshot`]
//! takes `env`+an explicit `perp: bool` and resolves the REST host INTERNALLY (mirroring
//! `data.rs::rest_klines_base(env, perp)`) rather than taking a raw host string — so a caller can't
//! pass a mismatched (host, perp) pair that silently builds a nonexistent URL (review round 1
//! finding). Same spot/perp-flag shape `data.rs`'s `fetch_klines_latest`/`rest_klines_base` and
//! `market_feed.rs`'s own `run_live`/`feed_main` already use elsewhere in this crate.

use vike_bridge_core::Environment;
use vike_exec::TickSender;

use vike_binance::family::depth::{combined_stream_url, parse_depth_snapshot, PumpSpec};

use crate::urls;

// The pure protocol + the pump handle, re-exported under this module's existing public paths so
// callers (and `market_feed.rs`'s DOM depth lane) are unchanged.
pub use vike_binance::family::depth::{
    apply_depth_event, decode_book_ticker, decode_trade, route_frame, DepthOutcome, MarketDataFeed,
    MdEvent,
};

const VENUE: &str = "aster";

/// `(lastUpdateId, bids, asks)` — a REST depth snapshot ready to seed an `L2Book`. `pub(crate)` —
/// shared with `market_feed.rs`'s DOM depth lane, but never named outside this crate (callers
/// destructure the tuple, never spell the alias).
pub(crate) use vike_binance::family::depth::DepthSnapshot;

/// Build the `GET .../depth` URL for a resolved REST host — pure so the host/path swap is
/// unit-testable without network. `perp` picks `/fapi/v3/depth` (futures) vs `/api/v3/depth`
/// (spot) — see [`fetch_depth_snapshot`]'s doc for which caller uses which. The path choice is
/// ASTER's (Binance's futures depth is a different endpoint version), which is why it is decided
/// here and not in the shared builder.
fn depth_snapshot_url(rest: &str, symbol: &str, perp: bool) -> String {
    let path = if perp { "fapi/v3/depth" } else { "api/v3/depth" };
    vike_binance::family::depth::depth_snapshot_url(rest, path, symbol)
}

/// GET `.../depth` → the snapshot to seed the book before applying diffs. Resolves the REST host
/// from `env`+`perp` INTERNALLY (mirrors `data.rs::rest_klines_base(env, perp)`) rather than taking
/// a raw host string, so a caller cannot pass a mismatched (host, perp) pair — e.g. `sapi_rest` with
/// `perp=true` would silently build the nonexistent `{sapi_rest}/fapi/v3/depth` and both call sites
/// discard the `Err`. `perp = true` hits the USDⓈ-M futures endpoint (`fapi_rest`, `/fapi/v3/depth`
/// — this module's own HFT tick track, per the design spec's `GET /fapi/v3/depth`); `perp = false`
/// hits the spot endpoint (`sapi_rest`, `/api/v3/depth` — `market_feed.rs`'s DOM depth lane, which
/// stays spot-only matching binance's own template choice, see that module's doc). One shared
/// function so the fetch+parse logic isn't duplicated across the two depth consumers even though
/// they track different books on different hosts. `pub` — shared with `market_feed.rs`.
pub fn fetch_depth_snapshot(
    env: Environment,
    symbol: &str,
    perp: bool,
) -> Result<DepthSnapshot, Box<dyn std::error::Error>> {
    let u = urls::urls_for(env);
    let rest = if perp { u.fapi_rest } else { u.sapi_rest };
    let url = depth_snapshot_url(rest, symbol, perp);
    // The BOUNDED seed agent shared with binance (`vike_binance::family::depth`'s
    // `DEPTH_SEED_TIMEOUT`), not the 30 s pager: this call sits between a depth session's bounded
    // dial and its first stop poll, on BOTH of this venue's consumers (the DOM lane's `depth_main`
    // and this module's own HFT pump). 4xx/5xx still come back as responses, so a 429/418 body
    // simply fails the `lastUpdateId` parse below.
    let agent = vike_binance::family::depth::depth_seed_agent();
    let mut resp = agent.get(url.as_str()).call()?;
    parse_depth_snapshot(&resp.body_mut().read_to_string()?)
}

/// Spawn the Aster market-data feed: combined bookTicker + trade + depth streams (USDⓈ-M futures)
/// pushed into the core's tick/L2 lanes via `ticks`. `env` picks testnet vs mainnet (see
/// [`crate::urls::urls_for`]); `tick_size` sizes the `L2Book` price grid. Stoppable.
/// The pump's WIRE plumbing for a mount `symbol`, pure: `(combined-stream WS URL, wire symbol)`.
///
/// ⚠ The SERIES symbol and the WIRE symbol are different strings whenever the caller mounts vike's
/// perp spelling (`BTCUSDT.P`, the symbol `vike_run::WIRED_MARKETS` pins for this venue), and the
/// pump needs BOTH: every emitted tick is labeled with the caller's own `symbol` (the core
/// dispatches `on_quote_tick`/`on_order_book` on the MOUNT's `(venue, symbol)` pair, and the exec
/// engine's `accepts_symbol` is equality on the mounted spelling), while the stream names and
/// `GET /fapi/v3/depth` only resolve the exchange's own `BTCUSDT`. Building the URL from the raw
/// `.P` symbol dials `btcusdt.p@bookTicker` — a stream nothing serves — so the pump connects,
/// reports healthy and delivers nothing forever: the silent-no-quote failure shape. The split is
/// the SAME [`vike_catalog::split_perp`] the kline/trades/depth lanes in `market_feed.rs` already
/// apply; a bare (suffix-less) symbol splits to itself, so spot-spelled callers are byte-identical
/// to before — and this track was already futures-only regardless (`perp = true` below).
fn pump_wire(env: Environment, symbol: &str) -> (String, String) {
    let (api_symbol, _) = vike_catalog::split_perp(symbol);
    (combined_stream_url(urls::urls_for(env).fapi_ws, api_symbol), api_symbol.to_string())
}

pub fn spawn_aster_market_data(
    env: Environment,
    ticks: TickSender,
    symbol: &str,
    tick_size: f64,
) -> MarketDataFeed {
    // WS URL + snapshot fetch run on the `.P`-stripped WIRE symbol; the emitted ticks keep the
    // caller's own `symbol` as the series label — see [`pump_wire`] for why both halves matter.
    let (ws_url, snapshot_symbol) = pump_wire(env, symbol);
    let spec = PumpSpec {
        venue: VENUE,
        ws_url,
        // This track is FUTURES (module doc): `perp = true`.
        fetch_snapshot: Box::new(move || fetch_depth_snapshot(env, &snapshot_symbol, true)),
    };
    vike_binance::family::depth::spawn_market_data(spec, VENUE, ticks, symbol, tick_size)
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

    #[test]
    fn combined_stream_url_uses_futures_ws_and_lowercased_symbol() {
        let demo = urls::urls_for(Environment::Demo);
        assert_eq!(
            combined_stream_url(demo.fapi_ws, "BTCUSDT"),
            "wss://fstream.asterdex-testnet.com/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
        );
        let live = urls::urls_for(Environment::Live);
        assert_eq!(
            combined_stream_url(live.fapi_ws, "ETHUSDT"),
            "wss://fstream.asterdex.com/stream?streams=ethusdt@bookTicker/ethusdt@trade/ethusdt@depth@100ms"
        );
    }

    /// The `.P` MOUNT spelling (`vike_run::WIRED_MARKETS` pins `BTCUSDT.P` for this venue) must
    /// reach the wire as the exchange's own `btcusdt`: a raw `.P` in the stream name dials a
    /// stream nothing serves, so the pump connects, reports healthy and delivers nothing forever.
    /// A bare symbol splits to itself — spot-spelled callers are byte-identical to before.
    #[test]
    fn pump_wire_strips_the_perp_suffix_for_the_stream_and_snapshot() {
        let (url, snap) = pump_wire(Environment::Live, "BTCUSDT.P");
        assert_eq!(
            url,
            "wss://fstream.asterdex.com/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
        );
        assert_eq!(snap, "BTCUSDT", "the REST depth seed must ask for the exchange symbol");
        // The identity half: a suffix-less symbol changes nothing.
        assert_eq!(pump_wire(Environment::Live, "BTCUSDT"), (url, snap));
    }

    #[test]
    fn depth_snapshot_url_perp_is_futures_v3_spot_is_api_v3() {
        let demo = urls::urls_for(Environment::Demo);
        assert_eq!(
            depth_snapshot_url(demo.fapi_rest, "BTCUSDT", true),
            "https://fapi.asterdex-testnet.com/fapi/v3/depth?symbol=BTCUSDT&limit=1000"
        );
        assert_eq!(
            depth_snapshot_url(demo.sapi_rest, "BTCUSDT", false),
            "https://sapi.asterdex-testnet.com/api/v3/depth?symbol=BTCUSDT&limit=1000"
        );
        let live = urls::urls_for(Environment::Live);
        assert_eq!(
            depth_snapshot_url(live.fapi_rest, "BTCUSDT", true),
            "https://fapi.asterdex.com/fapi/v3/depth?symbol=BTCUSDT&limit=1000"
        );
    }
}
