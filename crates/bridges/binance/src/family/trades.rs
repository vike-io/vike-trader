//! The Binance-grammar aggTrade stack — WS/REST decode, the live `subscribe_trades` feed, and the
//! backward-paging backfill. Shared by vike-binance (`crate::trades`) and vike-aster
//! (`vike_aster::trades`), each passing its own [`FamilySpec`] (F9, dedup rung 2).
//!
//! Aster's `@aggTrade` wire shape (`a`/`p`/`q`/`T`/`m`, both the WS push event and the REST array
//! element) is byte-identical to Binance's, so the two venues' `trades.rs` files were copies. The
//! decode + splice + paging logic lives here once; the venues keep only their host resolution
//! (the [`UrlTable`] they hand in) and their thin `pub` backfill wrappers.
//!
//! Field mapping — the WS `@aggTrade` push event and each REST `aggTrades` array element share the
//! same shape (REST just omits `"e"`/`"s"`):
//!
//! | wire key | → | [`AggTrade`] / [`vike_model::TradeTick`] field |
//! |----------|---|-------------------------------------------------|
//! | `"a"`    |   | `AggTrade.id` — aggregate trade id (REST ascending; the dedup/order key) |
//! | `"T"`    |   | `tick.ts` — trade time ms (NOT event time `"E"`) |
//! | `"p"`    |   | `tick.price` — string → f64 |
//! | `"q"`    |   | `tick.size` — string → f64 |
//! | `"m"`    |   | `tick.is_buyer_maker` — the aggTrades convention (see vike-orderflow's `classify.rs`) |
//! | (caller) |   | `tick.symbol` — never read off the wire (WS carries `"s"`, REST carries none); always the caller's argument, same convention as the kline mapper |
//!
//! **Non-finite/non-positive guard:** a decoded `price`/`size` that fails to parse, is non-finite
//! (`inf`/`NaN` — Rust's `f64::from_str` parses `"inf"`/`"infinity"`/`"nan"` successfully and an
//! out-of-range exponent like `"1e400"` silently overflows to `+Inf` rather than erroring, so a
//! parse-failure check alone would NOT catch these), or is `<= 0.0` is rejected at this seam:
//! `None` from [`ws_agg_trade`], silently skipped element-wise (not batch-failing) in
//! [`rest_agg_trades`]. A `+Inf` or zero size reaching a downstream volume fold (tick/volume-bar
//! builders) would spin or corrupt the running total — this is the one guard seam both entry
//! points share.
//!
//! **Live feed ([`run_trades_feed`]).** One thread per symbol (each venue's
//! `market_feed::Feeds::subscribe_trades` spawns it via a thin `FeedCtx`-unpacking adapter, same
//! per-key stop-flag/`JoinHandle` bookkeeping as `subscribe_bars`/`subscribe_depth`), reading
//! `{ws_host}/ws/{symbol}@aggTrade` — or `@trade` on a venue whose perp aggTrade lane is dead, see
//! [`PerpTradesLane`](super::PerpTradesLane).
//!
//! **Startup (gapless splice).** Connect the WS FIRST — before touching REST at all — so the socket
//! (OS/tungstenite receive buffer) is already collecting whatever the venue pushes from that moment
//! on. THEN fetch the REST `aggTrades?limit=1000` warmup snapshot (a single blocking round-trip; a
//! failure degrades to an empty seed, same posture as the kline feed's own seed error — the
//! buffered live trades are still good data). THEN drain whatever queued up on the socket while
//! that REST call was in flight (one bounded pass, timeout-terminated — never blocks past one
//! read-timeout tick). THEN splice via [`handoff`] (ascending warmup ++ buffered trades whose id is
//! greater than the warmup's max, so anything the warmup already covers is dropped) and emit the
//! result through `sink.trade` in order. Only THEN does the connection fall into the ordinary live
//! loop.
//!
//! **Earliest-live-id reporting.** Immediately after computing the startup splice (before emitting
//! it), [`run_trades_feed`] records `min(existing, spliced.first().id)` for `symbol` into a shared
//! `market_feed::Feeds::earliest_live_ids` map (via the tiny [`record_earliest_id`] helper,
//! factored out so the min-tracking logic is unit-testable without a live socket). This is the
//! oldest aggTrade id this live feed has ever emitted — a backfill thread reads it as the
//! strictly-older paging boundary (`global-constraints.md`'s no-double-count invariant: backfill
//! ids must stay `< min_live_id`). Because the splice is startup-only (next paragraph), this
//! recording also happens exactly once per feed-thread lifetime — a reconnect can never move it.
//!
//! **Reconnect.** The WS session/reconnect LIFECYCLE (read-timeout stop poll, server-ping
//! auto-pong, stop-aware 30×100 ms backoff, reconnect) rides the shared
//! [`vike_bridge_core::market_pump`] driver (dedup A6, wave 3) — the same driver + backoff the
//! kline feed (`family::market_feed::feed_main`) runs on, behavior-identical to the pre-driver
//! copy. Deliberately does NOT re-run the REST warmup or re-buffer on a reconnect: nothing downstream
//! de-dupes past the one-time startup splice, so re-warming up on every reconnect would re-emit a
//! whole historical block the sink has already seen. A reconnect just resumes decoding WS directly.
//! This is an at-least-once posture — a rare duplicate live trade across a reconnect is a far
//! smaller problem for a tick/volume-bar builder than either a re-emitted historical block or a
//! gap, matching the kline feed's own tolerance for a duplicate/overlapping bar on reconnect.
//!
//! **No `stream_status` wiring** (net-hardening §B's typed gap/stale disclosure) — v1 follow-up;
//! [`vike_data::LiveDataSink::stream_status`] has a default no-op so this behaves fine without it.
//!
//! **Backfill: a REST error is NOT end-of-history.** The backward pager stops on an empty page,
//! because on a contiguous id space an empty page below `before_id` means there is nothing older to
//! fetch. That made the *fetch's* failure modes load-bearing, and they used to be erased twice over:
//! the backfill entry point called the page fetch through `unwrap_or_default()`, so ANY transport
//! fault, 429, or 5xx became an empty page and therefore a clean "end of history"; and a `200` whose
//! body was not a JSON array parsed to an empty vec through the deliberately-lenient
//! [`rest_agg_trades`], with the same result. `truncated` was set only by the `max_pages` cap, so an
//! error-truncated backfill was indistinguishable from a complete one — and vike-app's run-once
//! spawn guard (`bf_spawned`, insert-only) meant that symbol never refetched.
//!
//! The producer half of that is fixed here (PR #937 fixed the consumer half — it staged pages it
//! could not deliver, which does not help when the pages were never fetched):
//!
//! 1. The page fetch (`fetch_agg_trades_from_id`) now RETRIES a `429`/`418` on the shared
//!    [`vike_bridge_core::retry::retry_rate_limited`] cadence — honoring `Retry-After`, the same
//!    driver and the same classification the sibling kline pager
//!    ([`crate::family::klines`]'s `fetch_page_rate_limited`) uses. This is not a second retry
//!    layer: `ureq` has none, and this path had none. Everything else (transport fault, 4xx, 5xx,
//!    unparseable body, retry exhaustion) fails FAST with the venue's error string — a 400
//!    `Invalid symbol` is not worth six backoffs.
//! 2. A `200` whose body is not a JSON array is an error, not an empty page
//!    ([`parse_agg_trades_page`], the strict twin of [`rest_agg_trades`]).
//! 3. The walk reports WHY it stopped — [`BackfillOutcome`]/[`BackfillStop`]. `Complete`, `Capped`
//!    and `Failed` are three distinguishable answers, and the two truncating ones warrant different
//!    operator responses: `Capped` means more history exists and a bigger cap would reach it;
//!    `Failed` means this call lost data the venue still has, so the SAME request is worth retrying.
//!
//! Whatever pages were already collected are still emitted on a `Failed` — they are good data, and
//! dropping them would just be a second silent loss.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use vike_bridge_core::http::{body_head, get_raw};
use vike_bridge_core::market_pump::{
    connect_market_socket, run_market_feed_on, FrameOutcome, MarketPumpOpts, MarketStream,
};
use vike_bridge_core::retry::{retry_rate_limited, BackoffPolicy, Verdict};
use vike_bridge_core::user_data::{StreamError, StreamMsg, UserStream};
use vike_bridge_core::ws::{is_timeout, TungsteniteStream, WsSocket};
use vike_data::LiveDataSink;

use super::market_feed::{now_ms, pump_opts};
use super::{FamilySpec, PerpTradesLane, UrlTable};

/// One aggregated trade: the id used for dedup/ordering across the REST-warmup → WS-live splice,
/// plus the mapped [`vike_model::TradeTick`].
///
/// `Debug` so a `Result<Vec<AggTrade>, _>` — the page shape [`AggTradePageFetch`] returns — can be
/// asserted on directly (`unwrap_err` and friends require the `Ok` type to be printable).
#[derive(Debug)]
pub struct AggTrade {
    pub id: u64,
    pub tick: vike_model::TradeTick,
}

/// Wire shape shared by the WS `@aggTrade` push event and each REST `aggTrades` array element. `e`
/// is only ever present on the WS side (REST omits it — `#[serde(default)]` tolerates that).
/// `E`/`s`/`f`/`l` carry nothing this module needs and are simply left undeclared — serde ignores
/// unknown JSON keys by default.
#[derive(Deserialize)]
struct AggTradeWire {
    #[serde(default)]
    e: Option<String>,
    a: u64,
    p: String,
    q: String,
    #[serde(rename = "T")]
    ts: i64,
    m: bool,
}

/// The WS `@trade` push event and each REST `trades` array element — the RAW-trade lane's twin of
/// [`AggTradeWire`] (see [`PerpTradesLane::Raw`]).
///
/// Two payloads, one struct, because they disagree on every field NAME while carrying the same five
/// values. Measured live:
///
/// ```text
/// WS   {"e":"trade","E":..,"T":1785701708156,"s":"BTCUSDT","t":7947574407,"p":"63437.30","q":"0.003","m":false,..}
/// REST {"id":7947579848,"price":"63440.80","qty":"0.001","time":1785702045391,"isBuyerMaker":true,..}
/// ```
///
/// `serde(alias)` accepts either spelling per field, so one `Deserialize` covers both; extra keys
/// (`X`, `st`, `quoteQty`, `isRPITrade`) are ignored as usual.
#[derive(Deserialize)]
struct RawTradeWire {
    #[serde(default)]
    e: Option<String>,
    #[serde(alias = "id")]
    t: u64,
    #[serde(alias = "price")]
    p: String,
    #[serde(alias = "qty")]
    q: String,
    #[serde(rename = "T", alias = "time")]
    ts: i64,
    #[serde(alias = "isBuyerMaker")]
    m: bool,
}

impl From<RawTradeWire> for AggTradeWire {
    /// The raw lane reuses every downstream step by mapping onto the aggregate shape — its `t` (raw
    /// trade id) becomes `a`. **The two id spaces are NOT interchangeable**; see
    /// [`PerpTradesLane::Raw`] for what that costs.
    fn from(w: RawTradeWire) -> Self {
        AggTradeWire { e: w.e, a: w.t, p: w.p, q: w.q, ts: w.ts, m: w.m }
    }
}

/// WS `@trade` event JSON → [`AggTrade`]; `None` on a non-`trade` event or a bad price/size — the
/// raw-lane twin of [`ws_agg_trade`].
pub fn ws_raw_trade(symbol: &str, json: &str) -> Option<AggTrade> {
    let wire: RawTradeWire = serde_json::from_str(json).ok()?;
    if wire.e.as_deref() != Some("trade") {
        return None;
    }
    map_wire(symbol, wire.into())
}

/// REST `trades` array JSON → `Vec<AggTrade>` — the raw-lane twin of [`rest_agg_trades`], with the
/// same element-wise tolerance (a malformed element is skipped; a malformed body yields empty).
pub fn rest_raw_trades(symbol: &str, json: &str) -> Vec<AggTrade> {
    let Ok(wires) = serde_json::from_str::<Vec<RawTradeWire>>(json) else {
        return Vec::new();
    };
    wires.into_iter().filter_map(|w| map_wire(symbol, w.into())).collect()
}

/// The shared non-finite/non-positive guard (module doc) — applied identically to WS and REST.
fn is_valid_trade(price: f64, size: f64) -> bool {
    price.is_finite() && price > 0.0 && size.is_finite() && size > 0.0
}

/// Decode+validate one wire record; `None` on an unparseable or out-of-range price/size.
fn map_wire(symbol: &str, w: AggTradeWire) -> Option<AggTrade> {
    let price = w.p.parse::<f64>().ok()?;
    let size = w.q.parse::<f64>().ok()?;
    if !is_valid_trade(price, size) {
        return None;
    }
    Some(AggTrade {
        id: w.a,
        tick: vike_model::TradeTick {
            ts: w.ts,
            // 0 = "not stamped" sentinel (vike-model doc): mappers stay pure/deterministic;
            // the feed loop stamps machine receive time just before each sink.trade emit.
            local_ts: 0,
            price,
            size,
            is_buyer_maker: w.m,
            symbol: symbol.to_string(),
        },
    })
}

/// WS `@aggTrade` event JSON → [`AggTrade`]. `None` on a non-`aggTrade` event, malformed JSON, or a
/// non-finite/non-positive price or size (module doc).
pub fn ws_agg_trade(symbol: &str, json: &str) -> Option<AggTrade> {
    let wire: AggTradeWire = serde_json::from_str(json).ok()?;
    if wire.e.as_deref() != Some("aggTrade") {
        return None;
    }
    map_wire(symbol, wire)
}

/// REST `aggTrades` array JSON → `Vec<AggTrade>`, ascending by id (venue order preserved).
/// Malformed top-level JSON yields an empty vec; an individual malformed/non-finite/non-positive
/// element is skipped in place rather than failing the whole batch.
pub fn rest_agg_trades(symbol: &str, json: &str) -> Vec<AggTrade> {
    let Ok(wires) = serde_json::from_str::<Vec<AggTradeWire>>(json) else {
        return Vec::new();
    };
    wires.into_iter().filter_map(|w| map_wire(symbol, w)).collect()
}

/// Strict twin of [`rest_agg_trades`] for the BACKFILL page path: identical element-wise tolerance
/// (a non-finite/non-positive element is skipped in place), but a body that is not a JSON array of
/// aggTrade records at all is an **error** instead of an empty vec.
///
/// The two callers genuinely want different things from the same bytes. The live warmup is a
/// best-effort seed — an unusable body there costs a historical prefix the buffered live trades make
/// up for, so [`rest_agg_trades`]'s lenient `Vec::new()` is right. On the backfill path an empty
/// page is a *decision*: [`backfill_agg_trades_backward_reported`] reads it as "nothing older
/// exists" and stops the walk. A garbled body must not be able to impersonate that (module doc).
pub fn parse_agg_trades_page(
    venue: &str,
    symbol: &str,
    body: &str,
) -> Result<Vec<AggTrade>, String> {
    let wires: Vec<AggTradeWire> = serde_json::from_str(body)
        .map_err(|e| format!("{venue} aggTrades json: {e}: {}", body_head(body)))?;
    Ok(wires.into_iter().filter_map(|w| map_wire(symbol, w)).collect())
}

/// Snapshot+buffer handoff for the REST-warmup → WS-live splice: the warmup trades (already
/// ascending by id) followed by any buffered live trades whose id is greater than the warmup's max
/// — dropping anything the warmup already covered.
pub fn handoff(warmup: Vec<AggTrade>, buffered: Vec<AggTrade>) -> Vec<AggTrade> {
    let max = warmup.last().map(|t| t.id).unwrap_or(0);
    warmup.into_iter().chain(buffered.into_iter().filter(|t| t.id > max)).collect()
}

/// Records `min(existing, id)` for `symbol` in the shared earliest-live-id map (module doc's
/// "Earliest-live-id reporting" section). Pulled out as its own tiny helper (rather than inlined at
/// the one call site) purely so the min-tracking logic is unit-testable without a live socket — see
/// each venue's `tests`; the feed thread's own wiring is covered by the live-smoke conventions
/// instead.
pub fn record_earliest_id(ids: &Mutex<HashMap<String, u64>>, symbol: &str, id: u64) {
    let mut m = ids.lock().unwrap();
    let e = m.entry(symbol.to_string()).or_insert(u64::MAX);
    *e = (*e).min(id);
}

// -------------------------------------------------------------------------------------------
// URL builders (pure — each venue unit-tests them through its own wrapper, with its own hosts)
// -------------------------------------------------------------------------------------------

/// REST warmup depth (module doc: `limit=1000`) — the newest N aggTrades, ascending by id.
pub const TRADES_SEED_LIMIT: usize = 1000;
/// Wall-clock ceiling on the one-time post-warmup catch-up drain ([`drain_buffered_trades`]).
///
/// It is deliberately the SAME span as the socket's read timeout, which is why it reads as one
/// number: the drain's guarantee is "never blocks past one read tick", and it enforces that with a
/// wall clock because on a busy symbol the read itself never idles (see that function's doc). The
/// socket's read timeout is NOT this constant, though — that comes from the venue's
/// `MarketPumpSpec` row like every other pump knob, and
/// `the_drain_deadline_matches_the_rows_read_timeout` is what stops the two drifting apart.
const DRAIN_DEADLINE: Duration = Duration::from_secs(2);

/// Spot-vs-USDⓈ-M-futures `@aggTrade` WS URL for `api_symbol` (already `.P`-stripped — the EXCHANGE
/// symbol). Spot stays on the spot stream host, a perp routes to the futures stream; the frame
/// shape is identical on both. Pure (no network) so both branches are unit-tested per venue.
pub fn agg_trades_ws_url(urls: &UrlTable, api_symbol: &str, is_perp: bool) -> String {
    format!("{}/ws/{}{}", urls.ws(is_perp), api_symbol.to_lowercase(), trades_stream(urls, is_perp))
}

/// The WS stream suffix this (venue, instrument-class) pair's trade lane rides.
///
/// Spot is always `@aggTrade`. A perp follows [`UrlTable::perp_trades_lane`] — `@aggTrade` on a
/// venue whose futures stream works (Aster), `@trade` on one whose does not (Binance).
pub fn trades_stream(urls: &UrlTable, is_perp: bool) -> &'static str {
    match (is_perp, urls.perp_trades_lane) {
        (true, PerpTradesLane::Raw) => "@trade",
        _ => "@aggTrade",
    }
}

/// The REST warmup URL for whichever lane [`trades_stream`] picked, so the seed's ids land in the
/// SAME space as the live stream's. Pairing them wrongly would make [`handoff`]'s id dedup compare
/// aggregate ids against raw trade ids — two unrelated sequences, so nothing would ever dedup.
pub fn trades_rest_url(urls: &UrlTable, api_symbol: &str, is_perp: bool, limit: usize) -> String {
    match (is_perp, urls.perp_trades_lane) {
        (true, PerpTradesLane::Raw) => format!(
            "{}{}?symbol={api_symbol}&limit={limit}",
            urls.perp_rest, urls.perp_raw_trades_path
        ),
        _ => agg_trades_rest_url(urls, api_symbol, is_perp, limit, None),
    }
}

/// Spot-vs-USDⓈ-M-futures `aggTrades` REST URL (the warmup + the `fromId` backfill page share this
/// builder) for `api_symbol` (already `.P`-stripped) — the ONE place that decides which host+path
/// serves aggTrades, so the URL-builder tests and the real fetch never drift. `from_id: None`
/// builds the warmup's `?symbol=&limit=` query; `Some(id)` appends `&fromId=` for the
/// backward-paging backfill. Pure (no network).
///
/// Note the perp path is a genuine per-venue divergence carried by the [`UrlTable`], NOT unified —
/// see [`UrlTable::perp_agg_trades_path`].
pub fn agg_trades_rest_url(
    urls: &UrlTable,
    api_symbol: &str,
    is_perp: bool,
    limit: usize,
    from_id: Option<u64>,
) -> String {
    let (base, path) = (urls.rest(is_perp), urls.agg_trades_path(is_perp));
    let mut url = format!("{base}{path}?symbol={api_symbol}&limit={limit}");
    if let Some(id) = from_id {
        url.push_str(&format!("&fromId={id}"));
    }
    url
}

// -------------------------------------------------------------------------------------------
// Live feed: REST warmup fetch + the WS session/reconnect loop. See the module doc's
// Startup/Reconnect sections for the full contract.
// -------------------------------------------------------------------------------------------

/// Wall-clock ceiling on the startup REST warmup ([`fetch_agg_trades_latest`]) — the one blocking
/// call that runs INSIDE [`run_trades_feed`]'s connect closure, behind the bounded dial.
///
/// **Why it needs its own number.** The shared `vike_bridge_core::http::blocking_agent` carries a
/// 30 s global timeout, sized for a backfill pager. That is fine where it is and wrong here: this
/// call sits between the bounded dial and the bounded drain in a closure a live recorder's teardown
/// waits on, so a 30 s worst case would be the largest single position a feed thread can occupy at
/// stop time — larger than the dial the whole bounding exercise was about, and outside
/// `crates/vike-recorder/src/recorder_cli.rs`'s `FEED_STOP_BUDGET_SECS` derivation entirely.
/// Bounding a dial while a longer unbudgeted call hides two lines below it moves the ceiling, it
/// does not lower it.
///
/// **Ten seconds, measured.** MEASURED 2026-08-08 from the CI box (DE), `curl -w %{time_total}` against
/// `api.binance.com/api/v3/aggTrades?symbol=BTCUSDT&limit=1000` — the exact request this issues,
/// [`TRADES_SEED_LIMIT`] and all — ten samples: 0.275, 0.278, 0.284, 0.490, 0.508, 0.714, 0.719,
/// 0.732, 0.950, 0.954 s. So a healthy warmup is a third of a second and a slow one is under one, and
/// ten seconds is 10–36× the observed range. It is deliberately the same window as the dial
/// (`vike_bridge_core::pump_spec`'s `CONNECT_10S`), which is what keeps the feed-stop budget's
/// arithmetic a single largest-position number rather than a growing sum.
///
/// **What a timeout costs, so the trade is visible:** the warmup degrades to an EMPTY seed (the arm
/// below already does this for any fetch error) — the feed still goes live on buffered trades and
/// loses only the historical prefix, which is exactly what a 30 s wait was buying at 30x the
/// teardown cost.
const WARMUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The agent [`fetch_agg_trades_latest`] issues its one request through — a named rung so the bound
/// is OBSERVABLE (`agent.config().timeouts().global`) rather than merely written down. See
/// `the_startup_warmup_runs_on_a_bounded_agent_not_the_pagers`.
fn warmup_agent() -> ureq::Agent {
    vike_bridge_core::http::blocking_agent_with_timeout(WARMUP_TIMEOUT)
}

/// REST `GET …/aggTrades?symbol=&limit=` — the trades feed's one-time-per-connection startup
/// warmup. Mirrors each venue's `data.rs::fetch_klines_latest` shape exactly: one un-throttled
/// request (no paging/rate-limit retry — this is called once per connection attempt, never in a
/// backfill loop), a plain 2xx/error status split. `api_symbol` (already
/// `.P`-stripped) + `is_perp` pick the endpoint via [`agg_trades_rest_url`]; `label_symbol` is the
/// SINK/series label stamped on every decoded [`vike_model::TradeTick`] — the ORIGINAL
/// (`.P`-suffixed for a perp) symbol, so a perp's ticks never collide with its spot twin's series
/// key. Spot passes `api_symbol == label_symbol` and `is_perp = false`.
///
/// ⚠ The agent is [`WARMUP_TIMEOUT`]-bounded, NOT the shared 30 s `blocking_agent` every other REST
/// helper in this file uses — see that constant for why a call inside the connect closure is
/// budgeted differently from one in a pager.
fn fetch_agg_trades_latest(
    spec: &FamilySpec,
    api_symbol: &str,
    label_symbol: &str,
    is_perp: bool,
    limit: usize,
) -> Result<Vec<AggTrade>, String> {
    let venue = spec.venue;
    let agent = warmup_agent();
    let url = trades_rest_url(&spec.urls, api_symbol, is_perp, limit);
    let mut resp = agent.get(&url).call().map_err(|e| format!("{venue} aggTrades GET: {e}"))?;
    let status = resp.status().as_u16();
    let body =
        resp.body_mut().read_to_string().map_err(|e| format!("{venue} aggTrades read: {e}"))?;
    if (200..300).contains(&status) {
        // Parse in the lane the URL just fetched — the two array element shapes share no field
        // names, so a mismatch here silently yields an EMPTY seed rather than an error.
        Ok(match (is_perp, spec.urls.perp_trades_lane) {
            (true, PerpTradesLane::Raw) => rest_raw_trades(label_symbol, &body),
            _ => rest_agg_trades(label_symbol, &body),
        })
    } else {
        let head: String = body.chars().take(200).collect();
        Err(format!("{venue} aggTrades HTTP {status}: {head}"))
    }
}

/// The 429/418 backoff cadence for a backfill page (module doc's point 1). Deliberately the SAME
/// numbers both venues' [`crate::family::klines::KlineRateLimit`] carries (6 retries / 1s initial /
/// 60s ceiling) and deliberately a SHARED `const` rather than a per-venue [`FamilySpec`] field: this
/// is a retry *cadence* over the family's shared `Retry-After`+429/418 grammar, not a weight budget
/// (the thing klines keeps per-venue precisely because Binance's is live-verified and Aster's is a
/// guess). If a venue ever needs its own cadence, this becomes a parameter — do not diverge it by
/// copying the const.
const AGG_TRADES_BACKOFF: BackoffPolicy =
    BackoffPolicy { max_retries: 6, initial: Duration::from_secs(1), max: Duration::from_secs(60) };

/// REST `GET …/aggTrades?symbol=&limit=&fromId=` — the (up to) `limit` aggTrades with id
/// `>= from_id`, ascending by id. Same agent/URL/label shape as [`fetch_agg_trades_latest`] (every
/// error string is unchanged: `"{venue} aggTrades GET: …"` / `" read: …"` / `" HTTP {status}: …"`);
/// the differences are the `fromId` query param and the retry policy below. This is the production
/// `fetch` callback for [`backfill_agg_trades_backward_reported`]'s backward-paging loop, wired up
/// in [`agg_trades_backfill_reported`]. SPOT-only on both venues (orderflow's paged
/// backward-backfill never pages perp).
///
/// **Retry policy — 429/418 only, everything else fails fast.** The backfill bursts up to
/// `max_pages` (300) requests with no throttle between them, so a rate-limit response is the ONE
/// failure this loop provokes itself and the one worth waiting out: it is transient by definition,
/// the venue tells us how long to wait (`Retry-After`), and ignoring a Binance 429 escalates to a
/// 418 IP ban. Those two statuses ride the shared [`retry_rate_limited`] driver at
/// [`AGG_TRADES_BACKOFF`] — the exact driver + classification the sibling kline pager
/// ([`crate::family::klines`]'s `fetch_page_rate_limited`) uses, and NOT a second layer on top of an
/// existing one: `ureq` 3 has no request retry at all and this path had none.
///
/// Everything else propagates on the first attempt: a transport fault, any other 4xx (a `400 Invalid
/// symbol` will not become valid after six backoffs), any 5xx, an unparseable body, and retry
/// exhaustion. All of them reach the caller as `Err` — the point of this change is that the caller
/// can tell an error from an end of history, not that errors are papered over.
fn fetch_agg_trades_from_id(
    spec: &FamilySpec,
    symbol: &str,
    from_id: u64,
    limit: usize,
) -> Result<Vec<AggTrade>, String> {
    let venue = spec.venue;
    let label = format!("{venue} aggTrades");
    let agent = vike_bridge_core::http::blocking_agent();
    let url = agg_trades_rest_url(&spec.urls, symbol, false, limit, Some(from_id));
    // `debug` (not `info`): one line per backfill page (up to `max_pages`) — too chatty for the
    // default console level, but still captured by vike-log's always-`trace` JSON file, which is
    // exactly what makes "did backfill actually page any `fromId` requests" externally
    // observable/greppable (the gap that made this bug hard to spot: 0 REST calls logged nowhere,
    // silently indistinguishable from "not run yet").
    tracing::debug!(target: "vike_binance::family::trades", venue, url = %url, "aggTrades backfill page fetch");
    retry_rate_limited(AGG_TRADES_BACKOFF, &label, || {
        // `get_raw` captures status + `Retry-After` BEFORE draining the body, which is what lets the
        // 429 arm honor the server's own wait. The shared agent sets `http_status_as_error(false)`,
        // so a 429 arrives here as a STATUS, never as a transport error.
        let raw = get_raw(&agent, &url, &label, &[])?;
        match raw.status {
            200..=299 => Ok(Verdict::Done(parse_agg_trades_page(venue, symbol, &raw.body)?)),
            429 | 418 => Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("HTTP {} (rate limited)", raw.status),
            }),
            other => Err(format!("{venue} aggTrades HTTP {other}: {}", body_head(&raw.body))),
        }
    })
}

/// Connect the public `@aggTrade`/`@trade` stream for `api_symbol` (already `.P`-stripped — the
/// exchange symbol) through the shared dial, with BOTH windows taken from `opts` — the venue's
/// `MarketPumpSpec` row. `is_perp` picks the host via [`agg_trades_ws_url`].
///
/// **Why the shared [`connect_market_socket`] and not `tungstenite::connect`.** This lane owns its
/// connect closure (the startup drain below needs the raw `WsSocket` before the driver takes over),
/// so nothing upstream can apply `opts.connect_timeout` on its behalf — and for as long as this
/// dialed itself, it applied none: plain `tungstenite::connect` has no connect bound at all, so a
/// black-holed route pinned this thread inside the OS's SYN ladder (~127 s on Linux defaults) with
/// the stop flag already raised behind it. That is the whole of `vike-recorder`'s binance stop path
/// — its Binance feed records the trade tape and nothing else — so the daemon's `FEED_STOP_BUDGET_SECS`
/// was a planning figure rather than a ceiling, and a socket caught mid-dial at `systemctl stop`
/// could still be dialing when SIGKILL landed on the final Parquet flush.
///
/// The row is the authority for the read timeout too, for the same reason a knob table exists: a
/// second local copy is a thing that can disagree.
fn connect_trades_ws(
    urls: &UrlTable,
    api_symbol: &str,
    is_perp: bool,
    opts: &MarketPumpOpts<'_>,
) -> Result<WsSocket, String> {
    let url = agg_trades_ws_url(urls, api_symbol, is_perp);
    connect_market_socket(&url, opts.read_timeout, opts.connect_timeout, None)
}

/// One bounded pass draining whatever `@aggTrade` frames are ALREADY queued on `socket` — returns
/// the instant a read times out (meaning "caught up to real time"; never blocks past one
/// [`DRAIN_DEADLINE`] tick). Decodes each frame via [`ws_agg_trade`], appending to `buf`. Called
/// exactly once per connection attempt, right after the REST warmup call returns — since the socket
/// connected BEFORE that REST call went out, this one pass recovers everything the venue pushed
/// while the round-trip was in flight (module doc's gapless-splice guarantee).
///
/// **Wall-clock `deadline`, not just an idle-read timeout.** A read timing out alone does NOT bound
/// this loop on a busy symbol: a liquid `@aggTrade` stream routinely pushes frames faster than the
/// socket read timeout ever goes idle, so `socket.read()` kept returning `Ok` and this "catch up the
/// REST round-trip backlog" pass silently turned into an unbounded live-stream drain — measured
/// empirically on Binance BTCUSDT at 150+s / 1800+ buffered trades before a natural gap finally
/// appeared. That starved [`run_trades_feed`]'s one-time splice (and with it
/// `market_feed::Feeds::earliest_live_ids`) for far longer than the app's background aggTrades
/// backfill thread's ~15s poll budget, so orderflow restored from a saved workspace never
/// backfilled on any actively-trading symbol — this is the fix for that bug. Bounding by wall-clock
/// time (checked once per loop iteration; a single in-flight `socket.read()` can still overrun it
/// by up to one more read tick, so the real worst case is close to but not exactly
/// [`DRAIN_DEADLINE`]) instead of only "no message for one tick" restores the "never blocks past one
/// read tick" guarantee above even under continuous live traffic. Whatever hasn't drained
/// by `deadline` simply flows through the ordinary live session right after — same socket, reading
/// continues exactly where this pass left off, so nothing is lost or double-emitted.
fn drain_buffered_trades(
    series_symbol: &str,
    raw_lane: bool,
    socket: &mut WsSocket,
    stop: &AtomicBool,
    buf: &mut Vec<AggTrade>,
) -> Result<(), Box<dyn std::error::Error>> {
    use tungstenite::Message;
    let deadline = std::time::Instant::now() + DRAIN_DEADLINE;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Ok(()); // one read tick of wall-clock elapsed — stop catching up, go live
        }
        match socket.read() {
            Ok(Message::Text(txt)) => {
                // Decoded trades carry the SERIES label (the `.P`-suffixed symbol for a perp), NOT
                // the exchange symbol — the WS URL used the exchange symbol, but the sink key is
                // the series label. The decoder must match the lane the URL subscribed, or every
                // frame silently fails its `e` check and the drain returns empty.
                let decoded = if raw_lane {
                    ws_raw_trade(series_symbol, txt.as_str())
                } else {
                    ws_agg_trade(series_symbol, txt.as_str())
                };
                if let Some(t) = decoded {
                    buf.push(t);
                }
            }
            Ok(Message::Ping(p)) => socket.send(Message::Pong(p))?,
            Ok(Message::Close(_)) => return Err("server closed".into()),
            Ok(_) => {}
            Err(e) if is_timeout(&e) => return Ok(()), // caught up — nothing queued right now
            Err(e) => return Err(e.into()),
        }
    }
}

/// The trades lane's [`MarketStream`]: wraps the aggTrade socket (the startup dance needs the raw
/// socket for its bounded drain BEFORE the driver takes over, so the driver's own url-connect
/// production wrapper can't be used on this lane — [`run_market_feed_on`] with this seam is the
/// published escape hatch). Per-message shape is byte-identical to the pre-driver read loop: a
/// server Ping is auto-ponged and looped past, a server Close surfaces as
/// `Closed("server closed")` (via [`TungsteniteStream::recv`] — the exact status text the loop
/// produced), a read timeout is the driver's stop-poll tick. `since_last_frame` stays the default
/// `ZERO` — no idle watchdog on this lane (as before).
struct AggTradeStream(TungsteniteStream);

impl MarketStream for AggTradeStream {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        loop {
            match self.0.recv() {
                Ok(StreamMsg::Text(t)) => return Ok(t),
                Ok(StreamMsg::Ping(p)) => self.0.pong(p)?,
                Ok(StreamMsg::Other) => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        self.0.send_text(s)
    }
}

/// Each venue's `market_feed::Feeds::subscribe_trades` feed body (module doc has the full
/// Startup/Reconnect contract). Takes plain trait-object/closure args rather than a `FeedCtx` — the
/// venue's `market_feed::trades_thread_main` is the thin adapter that unpacks one into these.
///
/// `earliest_ids` is `market_feed::Feeds::earliest_live_ids` — `subscribe_trades`'s clone of the
/// shared per-`Feeds` map, threaded through as a plain param (see module doc's "Earliest-live-id
/// reporting" section for what gets recorded into it and when). `spec` carries the venue key, the
/// display name for the status string, and the resolved hosts — the whole per-venue delta.
///
/// **Series/api/is_perp split** (mirrors the perp kline feed's `family::market_feed::feed_main`):
/// `series_symbol` is the SINK/core label — the ORIGINAL catalog symbol, `.P`-suffixed for a perp —
/// under which every trade is emitted and every `earliest_ids` entry is keyed, so a perp's series
/// key never collides with its spot twin's. `api_symbol` is the `.P`-stripped EXCHANGE symbol that
/// drives the WS URL + REST warmup, and `is_perp` picks the host pair. A spot subscription passes
/// `series_symbol == api_symbol` and `is_perp = false`.
// 8 args: the trait-object/closure feed wiring (sink/wake/set_status/stop/earliest_ids) plus the
// series/api/is_perp split — grouping them into a struct would just be a private, single-call-site
// bundle with no reuse. The one caller per venue is `market_feed::trades_thread_main`.
#[allow(clippy::too_many_arguments)]
pub fn run_trades_feed(
    spec: &FamilySpec,
    series_symbol: &str,
    api_symbol: &str,
    is_perp: bool,
    sink: &dyn LiveDataSink,
    wake: &(dyn Fn() + Send + Sync),
    set_status: &dyn Fn(String),
    stop: &AtomicBool,
    earliest_ids: &Mutex<HashMap<String, u64>>,
) {
    let urls = &spec.urls;
    // Which lane this (venue, class) rides — decided ONCE so the WS URL, the drain decoder, the
    // REST warmup parser and the live decoder can never disagree. See `PerpTradesLane`.
    let raw_lane = is_perp && urls.perp_trades_lane == PerpTradesLane::Raw;
    let key = format!("{series_symbol}{}", trades_stream(urls, is_perp));
    // One-time startup latch: the FIRST successful session runs the warmup/drain/splice dance
    // (below) before going live; reconnected sessions skip straight to the live read (module
    // doc's Reconnect section — nothing downstream de-dupes past the one-time splice).
    let mut started = false;
    // ONE `MarketPumpOpts` for the whole feed: the driver reads its lifecycle knobs off it, and the
    // connect closure below reads its DIAL knobs off the same value. Built once rather than per
    // session so there is no way for the two to be looking at different rows.
    let opts = pump_opts(spec.venue);
    run_market_feed_on(
        // The per-session connect the shared driver drives (dedup A6): every session dials fresh,
        // and the first successful one additionally runs the one-time startup — connect -> REST
        // warmup -> drain whatever queued up on the socket meanwhile -> splice via handoff -> emit
        // (module doc's Startup section). A failure anywhere in this dance (rare) returns `Err`,
        // so the driver discloses it, backs off (the same stop-aware 30×100 ms wait as before) and
        // retries the WHOLE step from scratch — the WS-before-REST ordering must hold from a FRESH
        // connect, so any partial state from a dying connection is discarded, never spliced in.
        // Errors minted here are PRE-FORMATTED with the key + their phase wording so the session-
        // error hook below can surface them verbatim (the pre-driver status strings).
        || {
            let mut socket = connect_trades_ws(urls, api_symbol, is_perp, &opts)
                .map_err(|e| format!("{key} connect error (reconnecting): {e}"))?;
            if !started {
                // A dial can spend its whole bounded window, and the stop flag is raised before any
                // feed is joined — so by the time we get here the answer may already be "stop". The
                // startup dance below is the longest stretch of this closure that does NOT poll the
                // flag (a [`WARMUP_TIMEOUT`] REST round trip, then the drain), and the recorder's
                // teardown budget is derived from the LARGEST single position a feed thread can be
                // caught in. Checking here means a stop that lands mid-dial costs the dial and
                // nothing after it, instead of the dial PLUS a fresh network call started after the
                // daemon already asked to stop. `run_market_feed_on` reads this `Err` as a session
                // fault, whose stop-aware backoff checks the flag before its first sleep and returns.
                if stop.load(Ordering::Relaxed) {
                    return Err(format!("{key} stopped before warmup"));
                }
                let warmup = match fetch_agg_trades_latest(
                    spec,
                    api_symbol,
                    series_symbol,
                    is_perp,
                    TRADES_SEED_LIMIT,
                ) {
                    Ok(w) => w,
                    Err(e) => {
                        // Degrade gracefully (empty seed) rather than reconnecting — same posture
                        // as the kline feed's own seed error: the buffered live trades collected
                        // below are still good data even with no historical prefix.
                        set_status(format!("{key} warmup error: {e}"));
                        Vec::new()
                    }
                };
                let mut buf = Vec::new();
                drain_buffered_trades(series_symbol, raw_lane, &mut socket, stop, &mut buf)
                    .map_err(|e| format!("{key} error after warmup (reconnecting): {e}"))?;
                let spliced = handoff(warmup, buf);
                // Record the earliest id THIS splice covers before emitting — the no-double-count
                // boundary a backfill thread pages strictly below (module doc's "Earliest-live-id
                // reporting" section). Keyed on `series_symbol` (the `.P` label the app's backfill
                // thread reads), not the exchange symbol. Must read `spliced.first()`, not
                // `warmup.first()`: an empty/failed warmup still has a valid oldest id once
                // buffered live trades are spliced in.
                //
                // NOT on the raw lane: those are RAW trade ids and the backward backfill pages
                // `aggTrades` with `fromId`, an unrelated sequence. Publishing one there would hand
                // the pager a boundary from the wrong space — far worse than publishing none, which
                // simply leaves it with no boundary (its pre-existing state for every venue that
                // never ran this feed).
                if let Some(first) = spliced.first().filter(|_| !raw_lane) {
                    record_earliest_id(earliest_ids, series_symbol, first.id);
                }
                for t in spliced {
                    let mut tick = t.tick;
                    tick.local_ts = now_ms(); // machine receive time (dual-timestamp capture)
                    sink.trade(spec.venue, series_symbol, tick);
                }
                set_status(format!("LIVE · {} trades {series_symbol}", spec.display));
                wake();
                started = true;
            }
            Ok(AggTradeStream(TungsteniteStream(socket)))
        },
        &opts,
        stop,
        &now_ms,
        // The live read: every decoded trade goes straight to `sink.trade` (no buffering — the
        // one-time splice already happened above), emitted under the SERIES label (the `.P`
        // symbol for a perp), never the exchange symbol the WS URL was built from — the sink/core
        // key stays distinct from the spot twin.
        |txt| match if raw_lane {
            ws_raw_trade(series_symbol, txt)
        } else {
            ws_agg_trade(series_symbol, txt)
        } {
            Some(t) => {
                let mut tick = t.tick;
                tick.local_ts = now_ms(); // machine receive time
                sink.trade(spec.venue, series_symbol, tick);
                wake();
                FrameOutcome::Confirm
            }
            None => FrameOutcome::Ignore,
        },
        || {}, // no dataless-tick judgment on this lane (the polymarket freshness knob)
        // Errors minted by the connect/startup closure arrive pre-formatted (they carry the key +
        // their own phase wording — `connect error (reconnecting)` / `error after warmup
        // (reconnecting)`); a raw transport fault from the driver gets the classic ws-error
        // wrapper. Both statuses are byte-identical to the pre-driver copy's.
        |e| {
            if e.starts_with(&key) {
                set_status(e.to_string());
            } else {
                set_status(format!("{key} ws error (reconnecting): {e}"));
            }
        },
    );
}

// -------------------------------------------------------------------------------------------
// Backward-paging backfill: a pure paging loop over `fetch_agg_trades_from_id` (injected as
// `fetch` so it's testable without the network — see each venue's `tests`).
// -------------------------------------------------------------------------------------------

/// Why a backward backfill walk stopped — the answer the pre-#937-producer-half code erased by
/// turning every fetch error into an empty page (module doc).
///
/// The distinction the operator actually acts on is `Capped` vs `Failed`: both mean history older
/// than the oldest emitted trade is missing from this walk, but for opposite reasons. `Capped` is a
/// deliberate budget decision — the venue served every page it was asked for, and raising
/// `max_pages` (or narrowing `earliest_ts`) reaches the rest. `Failed` is data LOSS — the venue
/// still has those trades, this call just failed to read them, so re-running the identical request
/// is the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackfillStop {
    /// The requested window is fully covered — either a fetched page reached back to `earliest_ts`,
    /// or there is nothing older to fetch (an empty page, or the id floor at 1). Nothing is missing.
    Complete,
    /// `max_pages` pages were fetched before `earliest_ts` was reached: more history EXISTS at the
    /// venue and was deliberately not requested.
    Capped,
    /// A page fetch failed and the walk stopped there, carrying the fetch's error verbatim.
    /// Everything older than the oldest emitted trade is MISSING (not absent), and the pages already
    /// collected were still emitted.
    Failed(String),
}

impl BackfillStop {
    /// True when history older than the oldest emitted trade is missing from this walk — i.e. the
    /// walk did NOT reach `earliest_ts`. This is the historical `truncated` flag, widened from
    /// "the cap fired" to "for any reason".
    pub fn is_truncated(&self) -> bool {
        !matches!(self, BackfillStop::Complete)
    }

    /// True when re-running the IDENTICAL request could plausibly complete it — i.e. the walk was
    /// cut short by an error rather than by its own budget. `Capped` is deliberately NOT retryable:
    /// an identical re-run would cap at the same page.
    pub fn is_retryable(&self) -> bool {
        matches!(self, BackfillStop::Failed(_))
    }
}

/// What one backward-paging backfill actually did: how many pages it kept, and why it stopped.
///
/// `#[must_use]`: erasing this value is precisely the bug this type exists to prevent, and NOTHING
/// in the tree discards it any more — the one deliberate exception, a `()`-returning compat
/// wrapper each venue face re-exported, was retired once no caller took it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct BackfillOutcome {
    /// Pages successfully fetched AND kept — the historical first element of the
    /// [`backfill_agg_trades_backward`] return pair, unchanged in meaning. On a
    /// [`BackfillStop::Failed`] this counts the pages that DID arrive, not the one that failed.
    pub pages: u32,
    /// Why the walk stopped.
    pub stop: BackfillStop,
}

/// Page aggTrades backward from `before_id` (an EXCLUSIVE upper bound — typically `min_live_id`, so
/// every emitted id stays strictly below the oldest trade the live feed already covers; see
/// global-constraints.md's no-double-count invariant) until a fetched page's earliest ts is at or
/// before `earliest_ts` (that page is the last one needed: emit only its trades with
/// `ts >= earliest_ts`, then stop WITHOUT fetching any further page), or the next `from_id` would
/// hit the id floor (1), or `max_pages` pages have been fetched (truncated = true).
/// `fetch(symbol, from_id)` returns up to 1000 aggTrades ascending by id (id >= from_id). Returns
/// `(pages_fetched, truncated_by_cap)`.
///
/// Paging: the first page uses `from_id = before_id.saturating_sub(1000).max(1)` (so its ids span
/// `[from_id, from_id + 999]`, i.e. max id `before_id - 1 < before_id` — no overlap with anything
/// at/after `before_id`); after a kept page, `before_id` advances to that page's `first().id` and
/// the next `from_id` is recomputed from it the same way. Pages are fetched newest-first
/// (descending) but every page is internally ascending, and the whole `emit` sequence must read
/// oldest-first overall — so pages are buffered and replayed in reverse once the stop condition is
/// known, rather than emitted as they're fetched.
///
/// **Boundary note (why `<=`, not `<`, decides the ts stop):** once a fetched page's minimum ts is
/// *at or before* `earliest_ts`, every older page beyond it can only have even smaller ts (ids/ts
/// are monotonic), so nothing further is needed — stopping on `<=` avoids an extra probe fetch that
/// would immediately discard 100% of its page (`ts >= earliest_ts` would keep nothing from it) just
/// to confirm what the current page's minimum already proves. A strict `<` would require fetching
/// that wasted extra page whenever `earliest_ts` lands exactly on a page's first id.
///
/// **Id-floor defensive filter:** the "no overlap by construction" argument above assumes
/// `from_id = before_id - 1000` exactly. Near the id floor, `.max(1)` clamps `from_id` to `1`
/// instead, so the fetched page can reach ids at/above the current `before_id` (e.g.
/// `before_id = 500` clamps to `from_id = 1`, and a page spans up to id `1000`). Trades with
/// `id >= before_id` are filtered out of every page before it's considered — a no-op in the common
/// (unclamped) case, and required for correctness (no duplicate/overlapping emission) in the
/// clamped one. The `debug_assert!` below re-affirms the invariant on every emitted trade rather
/// than only trusting construction.
///
/// **Infallible-`fetch` compat shim.** This is
/// [`backfill_agg_trades_backward_reported`] with an error-free page source, kept verbatim (same
/// name, same params, same `(pages, truncated_by_cap)` pair) because both venues' unit tests script
/// `fetch` as a plain closure. With no error path, [`BackfillStop::Failed`] is unreachable, so
/// `truncated` still means exactly what it always did — the `max_pages` cap fired. **Production must
/// use the reporting variant**: a `Vec`-returning `fetch` has no way to say "this page failed", and
/// squeezing a REST error through it (`unwrap_or_default`) is the bug the module doc describes.
pub fn backfill_agg_trades_backward(
    symbol: &str,
    before_id: u64,
    earliest_ts: i64,
    max_pages: u32,
    fetch: &dyn Fn(&str, u64) -> Vec<AggTrade>,
    emit: &mut dyn FnMut(AggTrade),
) -> (u32, bool) {
    let infallible = |s: &str, id: u64| -> Result<Vec<AggTrade>, String> { Ok(fetch(s, id)) };
    let out = backfill_agg_trades_backward_reported(
        symbol,
        before_id,
        earliest_ts,
        max_pages,
        &infallible,
        emit,
    );
    (out.pages, out.stop == BackfillStop::Capped)
}

/// The FALLIBLE page source [`backfill_agg_trades_backward_reported`] pages through: given
/// `(symbol, from_id)`, either the (up to 1000, ascending-by-id) aggTrades with `id >= from_id`, or
/// the error that page failed with — the shape of [`fetch_agg_trades_from_id`].
///
/// A named alias rather than the inline `&dyn Fn(&str, u64) -> Result<Vec<AggTrade>, String>`
/// because that inline form trips `clippy::type_complexity`, whose remedy is exactly this. The
/// `'a` is explicit (not left to object-lifetime defaulting) so a caller can pass a closure
/// borrowing locals — which every scripted test, and the production wrapper's own adapter, do.
pub type AggTradePageFetch<'a> = dyn Fn(&str, u64) -> Result<Vec<AggTrade>, String> + 'a;

/// The real backward-paging loop (see [`backfill_agg_trades_backward`] above for the full paging /
/// boundary / id-floor contract, which is unchanged): identical walk, but `fetch` may FAIL and the
/// stop reason is reported rather than flattened to one `bool`.
///
/// A `fetch` error ends the walk with [`BackfillStop::Failed`] — it is emphatically **not** treated
/// as an empty page, which is how the pre-fix code fabricated a clean end-of-history out of a 429.
/// Pages already collected are still emitted (oldest-first, as always): they are good data adjacent
/// to `before_id`, and discarding them on the way out would only add a second loss to the first.
pub fn backfill_agg_trades_backward_reported(
    symbol: &str,
    before_id: u64,
    earliest_ts: i64,
    max_pages: u32,
    fetch: &AggTradePageFetch<'_>,
    emit: &mut dyn FnMut(AggTrade),
) -> BackfillOutcome {
    if before_id <= 1 {
        return BackfillOutcome { pages: 0, stop: BackfillStop::Complete };
    }
    let mut cur_before = before_id;
    let mut pages = 0u32;
    let mut stop = BackfillStop::Complete;
    // Pages collected newest-fetched-first; replayed in reverse below so the overall `emit`
    // sequence is oldest-first (each individual page is already ascending by id/ts).
    let mut kept: Vec<Vec<AggTrade>> = Vec::new();

    loop {
        let from_id = cur_before.saturating_sub(1000).max(1);
        let page = match fetch(symbol, from_id) {
            Ok(p) => p,
            // The whole point: an error stops the walk as `Failed`, never as the `break` an empty
            // page takes below. Older history is missing, not absent.
            Err(e) => {
                stop = BackfillStop::Failed(e);
                break;
            }
        };
        if page.is_empty() {
            break;
        }
        // Defensive id-floor filter — see doc comment above.
        let page: Vec<AggTrade> = page.into_iter().filter(|t| t.id < cur_before).collect();
        if page.is_empty() {
            break;
        }
        pages += 1;

        let page_min_ts = page.first().map(|t| t.tick.ts).unwrap_or(i64::MAX);
        if page_min_ts <= earliest_ts {
            kept.push(page.into_iter().filter(|t| t.tick.ts >= earliest_ts).collect());
            break;
        }

        cur_before = page.first().map(|t| t.id).unwrap_or(cur_before);
        let hit_floor = from_id <= 1;
        kept.push(page);
        if pages == max_pages {
            stop = BackfillStop::Capped;
            break;
        }
        if hit_floor {
            break;
        }
    }

    for t in kept.into_iter().rev().flatten() {
        debug_assert!(t.id < before_id, "backfill emitted id >= before_id (live overlap)");
        emit(t);
    }
    BackfillOutcome { pages, stop }
}

/// Production wrapper around [`backfill_agg_trades_backward_reported`]: wires the pure
/// backward-paging loop to the real REST fetch (`fetch_agg_trades_from_id`, `limit=1000` per page,
/// which is exactly why the loop's `fetch` is fallible here) and hands `emit`
/// just the mapped [`vike_model::TradeTick`] — the [`AggTrade`] id itself is only needed
/// internally, for the no-overlap/no-double-count bookkeeping the pure loop already does.
///
/// `before_id` must be the live trades feed's `market_feed::Feeds::earliest_live_ids` entry for
/// `symbol` (an EXCLUSIVE upper bound — see `global-constraints.md`'s no-double-count invariant:
/// every id this emits is `< before_id`, so it can never overlap what the live feed already
/// covers).
///
/// A REST fetch failure ends the walk as [`BackfillStop::Failed`] and is **not** confused with the
/// end of history (module doc) — the pages already collected are still emitted, and the failure is
/// both returned and `warn!`ed. Logs (does not panic or propagate) on either truncating stop, with a
/// DIFFERENT message per cause — `global-constraints.md`'s "a `max_pages` cap (300) that logs on
/// truncation", plus the error arm that used to log nothing at all.
///
/// A venue face re-exports this behind its own thin `pub fn agg_trades_backfill_reported` that
/// supplies the [`FamilySpec`]. Binance's is the only one today (`crate::trades`, off its `SPEC`
/// const); aster's face carries none — it would build one from its `env`, the way `spec` does.
pub fn agg_trades_backfill_reported(
    spec: &FamilySpec,
    symbol: &str,
    before_id: u64,
    earliest_ts: i64,
    max_pages: u32,
    emit: &mut dyn FnMut(vike_model::TradeTick),
) -> BackfillOutcome {
    let fetch = |s: &str, from_id: u64| fetch_agg_trades_from_id(spec, s, from_id, 1000);
    let out = backfill_agg_trades_backward_reported(
        symbol,
        before_id,
        earliest_ts,
        max_pages,
        &fetch,
        &mut |t| emit(t.tick),
    );
    // Two truncating causes, two messages: a reader must be able to tell "raise the cap" from
    // "this symbol lost history and should be refetched" from the log line alone.
    match &out.stop {
        BackfillStop::Complete => {}
        BackfillStop::Capped => tracing::warn!(
            target: "vike_binance::family::trades",
            venue = spec.venue,
            symbol,
            pages = out.pages,
            max_pages,
            "aggTrades backfill hit the max_pages cap before reaching earliest_ts (partial history)"
        ),
        BackfillStop::Failed(e) => tracing::warn!(
            target: "vike_binance::family::trades",
            venue = spec.venue,
            symbol,
            pages = out.pages,
            max_pages,
            error = %e,
            "aggTrades backfill STOPPED ON A REST ERROR before reaching earliest_ts — history older \
             than the emitted pages is MISSING, not absent; this request is worth retrying"
        ),
    }
    out
}

/// The current latest aggTrade `(id, ts)` for `symbol`, via a single un-throttled `limit=1` REST
/// call — wraps [`fetch_agg_trades_latest`], same shared agent/status-split. `None` on a fetch error
/// or an empty response (e.g. an unknown symbol). Exists purely so a smoke test can derive a real,
/// always-current `before_id`/`earliest_ts` pair instead of a hand-picked constant that would go
/// stale; nothing in production calls it.
pub fn latest_agg_trade(spec: &FamilySpec, symbol: &str) -> Option<(u64, i64)> {
    // Spot-only helper (orderflow's backfill boundary is spot-only): `symbol` is both the exchange
    // symbol and the label, `is_perp = false`.
    let trades = fetch_agg_trades_latest(spec, symbol, symbol, false, 1).ok()?;
    let last = trades.last()?;
    Some((last.id, last.tick.ts))
}

#[cfg(test)]
mod dial_tests {
    use super::*;
    use crate::market_feed::BINANCE_URLS;
    use vike_bridge_core::pump_spec::market_pump_spec;

    /// The production [`UrlTable`] with ONE field changed: a stream host that cannot be parsed as a
    /// URL at all, so a dial through it fails BEFORE any socket, DNS lookup or network exists —
    /// the whole point, since the property under test is which code path did the failing, not
    /// whether a host is reachable.
    ///
    /// The space is load-bearing: it is what makes the `http::Uri` parse fail deterministically on
    /// every platform. `crates/vike-bridge-core/src/ws_proxy.rs`'s
    /// `no_proxy_bounded_arm_fails_in_the_url_parse_exactly_as_before` already pins the same input
    /// against the same parse.
    fn unparseable_urls() -> UrlTable {
        UrlTable { spot_ws: "not a url", ..BINANCE_URLS }
    }

    /// **The dial this lane performs is BOUNDED, and the bound is its `MarketPumpSpec` row's.**
    ///
    /// This is the test the defect needed and did not have. `connect_trades_ws` owned its own
    /// connect (the startup drain needs the raw socket), dialed with plain `tungstenite::connect`,
    /// and therefore applied no connect bound at all — while the venue's row ALSO said
    /// `connect_timeout: None`, so neither half of the answer was in place and every existing test
    /// passed. A recorder stopping while that socket was mid-dial ignored the stop flag for the OS's
    /// SYN ladder, blowing `crates/vike-recorder/src/recorder_cli.rs`'s `FEED_STOP_BUDGET_SECS`.
    ///
    /// **How it observes the path without a network.** `vike_bridge_core::ws_proxy::connect_ws`
    /// branches on `connect_timeout`: `Some` parses the TCP target itself first (`ws_target`, whose
    /// rejection is the distinctive `"bad ws url"`), `None` hands the whole string to
    /// `tungstenite::connect`, which fails in ITS parser with its own wording. So an unparseable
    /// host makes the two arms say different things, offline and deterministically — and the second
    /// half of this test asserts they really do differ, so the discriminator cannot quietly become
    /// something both arms satisfy.
    ///
    /// Both ways of reopening the defect go red here: put `connect_timeout: None` back in the
    /// binance row, or dial with `tungstenite::connect` again, and the error is no longer the
    /// bounded arm's.
    #[test]
    fn the_trades_dial_goes_through_the_bounded_shared_path() {
        let opts = pump_opts("binance");
        assert_eq!(
            opts.connect_timeout,
            market_pump_spec("binance").knobs().connect_timeout,
            "the lane's opts must BE the row, not a local copy of it"
        );
        assert!(
            opts.connect_timeout.is_some(),
            "binance's row must bound the dial — see pump_spec's every_on_driver_row_bounds_its_dial"
        );

        let bounded = connect_trades_ws(&unparseable_urls(), "BTCUSDT", false, &opts)
            .expect_err("an unparseable host cannot dial");
        assert!(
            bounded.starts_with("bad ws url"),
            "the trades dial did not take `connect_ws`'s BOUNDED arm — it either ignored its row's \
             connect_timeout (a hand-rolled `tungstenite::connect`) or the row lost its bound. \
             error was: {bounded}"
        );

        // …and the discriminator discriminates: the same call with the bound removed fails
        // somewhere else entirely. Without this, a future edit could make BOTH arms produce the
        // asserted prefix and the check above would pass through the defect it exists to catch.
        let unbounded_opts = MarketPumpOpts { connect_timeout: None, ..opts };
        let unbounded = connect_trades_ws(&unparseable_urls(), "BTCUSDT", false, &unbounded_opts)
            .expect_err("an unparseable host cannot dial");
        assert!(
            !unbounded.starts_with("bad ws url"),
            "the unbounded arm must be distinguishable from the bounded one, or this test proves \
             nothing. error was: {unbounded}"
        );
    }

    /// The drain's wall-clock ceiling and the socket's read timeout are ONE span stated twice (see
    /// [`DRAIN_DEADLINE`]) — pinned equal here because the drain's contract ("never blocks past one
    /// read tick") is a claim about the socket, and the socket's timeout now comes from the row.
    #[test]
    fn the_drain_deadline_matches_the_rows_read_timeout() {
        assert_eq!(DRAIN_DEADLINE, market_pump_spec("binance").knobs().read_timeout);
        assert_eq!(DRAIN_DEADLINE, market_pump_spec("aster").knobs().read_timeout);
    }

    /// **The startup REST warmup is bounded, and bounded to fit the dial.**
    ///
    /// Bounding the dial is not the same as bounding the connect closure, and this is where that
    /// gap lived: [`run_trades_feed`]'s closure calls [`fetch_agg_trades_latest`] between the dial
    /// and the drain, and it did so on the shared `vike_bridge_core::http::blocking_agent` — a
    /// **30 s** global timeout sized for a backfill pager. So the longest window a feed thread could
    /// be caught in at `systemctl stop` was not the newly-bounded 10 s dial at all; it was a 30 s
    /// HTTP call two lines below it, absent from every derivation and from
    /// `crates/vike-recorder/src/recorder_cli.rs`'s `FEED_STOP_BUDGET_SECS`.
    ///
    /// Three assertions, each of which fails on a different way of reopening it: the warmup agent
    /// really carries [`WARMUP_TIMEOUT`] (revert to `blocking_agent()` and this goes red), that
    /// window is genuinely SHORTER than the shared default (so the test cannot be satisfied by the
    /// two converging on 30 s), and it fits inside the dial window the recorder's budget is derived
    /// from (so a future widening has to move the budget deliberately rather than silently).
    #[test]
    fn the_startup_warmup_runs_on_a_bounded_agent_not_the_pagers() {
        let warmup = warmup_agent().config().timeouts().global;
        assert_eq!(
            warmup,
            Some(WARMUP_TIMEOUT),
            "the startup warmup must run on its own bounded agent — it sits inside the connect \
             closure a live recorder's teardown waits on"
        );

        let shared = vike_bridge_core::http::blocking_agent()
            .config()
            .timeouts()
            .global
            .expect("the shared agent has always carried a global timeout");
        assert!(
            WARMUP_TIMEOUT < shared,
            "the warmup bound ({WARMUP_TIMEOUT:?}) must be shorter than the shared pager agent's \
             ({shared:?}), or this test is satisfied by the defect"
        );

        let dial = market_pump_spec("binance")
            .knobs()
            .connect_timeout
            .expect("binance's row bounds its dial — see every_on_driver_row_bounds_its_dial");
        assert!(
            WARMUP_TIMEOUT <= dial,
            "the warmup ({WARMUP_TIMEOUT:?}) is now the LARGEST window a feed thread can be caught \
             in, bigger than the dial ({dial:?}) the recorder's FEED_STOP_BUDGET_SECS is derived \
             from. Shorten it, or re-derive that budget deliberately."
        );
    }
}

#[cfg(test)]
mod perp_lane_tests {
    use super::*;
    use crate::market_feed::BINANCE_URLS;

    /// Real frames, captured live 2026-08-02 from `wss://fstream.binance.com/ws/btcusdt@trade`.
    const WS_RAW: &str = r#"{"e":"trade","E":1785701708157,"T":1785701708156,"s":"BTCUSDT","t":7947574407,"p":"63437.30","q":"0.003","X":"MARKET","m":false,"st":1}"#;
    /// Real body, captured live from `GET /fapi/v1/trades?symbol=BTCUSDT&limit=2`.
    const REST_RAW: &str = r#"[{"id":7947579848,"price":"63440.80","qty":"0.001","quoteQty":"63.44","time":1785702045391,"isBuyerMaker":true,"isRPITrade":false}]"#;

    #[test]
    fn the_raw_ws_frame_decodes_with_its_trade_id() {
        let t = ws_raw_trade("BTCUSDT.P", WS_RAW).expect("decodes");
        assert_eq!(t.id, 7_947_574_407, "`t`, the RAW trade id");
        assert_eq!(t.tick.ts, 1_785_701_708_156, "`T` (trade time), not `E` (event time)");
        assert_eq!(t.tick.price, 63437.30);
        assert_eq!(t.tick.size, 0.003);
        assert!(!t.tick.is_buyer_maker);
        assert_eq!(t.tick.symbol, "BTCUSDT.P", "the SERIES label, never the wire `s`");
    }

    /// The REST array uses entirely different field NAMES for the same five values — one struct
    /// covers both only because every field carries an alias.
    #[test]
    fn the_raw_rest_body_decodes_with_the_same_id_space() {
        let v = rest_raw_trades("BTCUSDT.P", REST_RAW);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, 7_947_579_848);
        assert_eq!(v[0].tick.ts, 1_785_702_045_391);
        assert_eq!(v[0].tick.price, 63440.80);
        assert!(v[0].tick.is_buyer_maker);
    }

    /// Each decoder rejects the OTHER lane's frame, so a lane/decoder mismatch fails loudly at the
    /// first frame instead of silently ignoring every one of them — which is exactly how the dead
    /// `@aggTrade` subscription looked from the inside.
    #[test]
    fn the_two_decoders_do_not_accept_each_others_frames() {
        let agg =
            r#"{"e":"aggTrade","E":1,"s":"BTCUSDT","a":5,"p":"1.0","q":"2.0","T":3,"m":false}"#;
        assert!(ws_raw_trade("S", agg).is_none(), "raw decoder must reject an aggTrade event");
        assert!(ws_agg_trade("S", WS_RAW).is_none(), "agg decoder must reject a trade event");
    }

    /// **The bug, pinned.** Binance perp subscribes `@trade`; binance spot and BOTH aster classes
    /// stay on `@aggTrade`. Measured: binance futures `@aggTrade` = 0 frames/60s, `@trade` = 770;
    /// aster futures `@aggTrade` = 10/15s.
    #[test]
    fn only_binance_perp_rides_the_raw_lane() {
        assert_eq!(trades_stream(&BINANCE_URLS, true), "@trade");
        assert_eq!(trades_stream(&BINANCE_URLS, false), "@aggTrade");
        assert_eq!(
            agg_trades_ws_url(&BINANCE_URLS, "BTCUSDT", true),
            "wss://fstream.binance.com/ws/btcusdt@trade"
        );
        assert_eq!(
            agg_trades_ws_url(&BINANCE_URLS, "BTCUSDT", false),
            "wss://stream.binance.com:9443/ws/btcusdt@aggTrade",
            "spot is byte-identical to before"
        );
    }

    /// The warmup URL must ride the SAME lane as the stream, or `handoff`'s id dedup compares two
    /// unrelated sequences and nothing ever dedups.
    #[test]
    fn the_warmup_url_follows_the_lane() {
        assert_eq!(
            trades_rest_url(&BINANCE_URLS, "BTCUSDT", true, 1000),
            "https://fapi.binance.com/fapi/v1/trades?symbol=BTCUSDT&limit=1000"
        );
        assert_eq!(
            trades_rest_url(&BINANCE_URLS, "BTCUSDT", false, 1000),
            agg_trades_rest_url(&BINANCE_URLS, "BTCUSDT", false, 1000, None),
            "spot still goes through the aggTrades builder unchanged"
        );
    }
}
