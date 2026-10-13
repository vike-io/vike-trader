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
//! **Consolidation note:** `fetch_depth_snapshot` is `pub` here and `market_feed.rs`'s depth lane
//! calls it (the pure `DepthOutcome`/`apply_depth_event` are the family's). Because this
//! module's own depth runs on futures while `market_feed.rs`'s stays on spot, [`fetch_depth_snapshot`]
//! takes `env`+an explicit `perp: bool` and resolves the REST host INTERNALLY (mirroring
//! `data.rs::rest_klines_base(env, perp)`) rather than taking a raw host string — so a caller can't
//! pass a mismatched (host, perp) pair that silently builds a nonexistent URL (review round 1
//! finding). Same spot/perp-flag shape `data.rs`'s `fetch_klines_latest`/`rest_klines_base` and
//! `market_feed.rs`'s own `run_live`/`feed_main` already use elsewhere in this crate.

use vike_bridge_core::poller::StopHandle;
use vike_exec::TickSender;
use vike_model::AccountTier;

use vike_binance::family::depth::{
    DepthSnapshot, PumpSpec, combined_stream_url, parse_depth_snapshot,
};

use crate::urls;

// The pure protocol (`DepthOutcome`, `MdEvent`, `apply_depth_event`, `decode_book_ticker`,
// `decode_trade`, `route_frame`) is named at `vike_binance::family::depth`, not re-exported here.
// The pump handle is `vike_bridge_core::poller::StopHandle`, the one stop-and-join owner every
// depth pump shares.

const VENUE: &str = "aster";

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
    env: AccountTier,
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
/// perp spelling (`BTCUSDT.P`, the symbol `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` pins for this venue), and the
/// pump needs BOTH: every emitted tick is labeled with the caller's own `symbol` (the core
/// dispatches `on_quote_tick`/`on_order_book` on the MOUNT's `(venue, symbol)` pair, and the exec
/// engine's `accepts_symbol` is equality on the mounted spelling), while the stream names and
/// `GET /fapi/v3/depth` only resolve the exchange's own `BTCUSDT`. Building the URL from the raw
/// `.P` symbol dials `btcusdt.p@bookTicker` — a stream nothing serves — so the pump connects,
/// reports healthy and delivers nothing forever: the silent-no-quote failure shape. The split is
/// the SAME [`vike_catalog::split_perp_at`] the kline/trades/depth lanes in `market_feed.rs` already
/// apply; a bare (suffix-less) symbol splits to itself, so spot-spelled callers are byte-identical
/// to before — and this track was already futures-only regardless (`perp = true` below).
fn pump_wire(env: AccountTier, symbol: &str) -> (String, String) {
    let (api_symbol, _) = vike_catalog::split_perp_at(VENUE, symbol);
    (combined_stream_url(urls::urls_for(env).fapi_ws, api_symbol), api_symbol.to_string())
}

pub fn spawn_aster_market_data(
    env: AccountTier,
    ticks: TickSender,
    symbol: &str,
    tick_size: f64,
) -> StopHandle {
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

#[path = "market_data_tests.rs"]
#[cfg(test)]
mod market_data_tests;
