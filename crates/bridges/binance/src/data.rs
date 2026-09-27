//! Binance Spot/perp REST kline history — the venue face of the shared
//! [`crate::family::klines`] rung (F, dedup rung 6).
//!
//! Both the live market feed's REST warmup seed (`fetch_klines_latest`, "the newest N klines, last
//! one still forming") and the vike-backfill historical backfill (`fetch_klines_range`, a paged
//! `[start, end]` window) delegate to the shared family kline core; this module supplies only
//! Binance's OWN host resolution and the WIRING of its rate-limit budget — the budget's NUMBERS
//! live in `vike_model::venue_rate_limits`'s `BINANCE_SPOT`/`BINANCE_PERP` rows, because a venue's
//! published ceiling is a fact about the venue rather than a constant of this crate — and keeps the
//! public surface (`parse_klines`/`fetch_klines_latest`/`fetch_klines_range`) unchanged for
//! vike-backfill and the feed. See [`crate::family::klines`] for the endpoint/response/rate-limit
//! contract.
//!
//! Endpoint: `GET https://api.binance.com/api/v3/klines?symbol=&interval=&limit=&[startTime=]&[endTime=]`
//! (spot), `GET https://fapi.binance.com/fapi/v1/klines?…` (USDⓈ-M), or
//! `GET https://dapi.binance.com/dapi/v1/klines?…` (COIN-M). Binance's hosts are hardcoded
//! `&'static str` consts — never env-resolved — so they stay exactly what they were before the
//! family module existed.
//!
//! ## ⚠ THREE books, and the `.P` boolean can only name two
//!
//! `docs/decisions/0061-an-instrument-names-its-kind.md` phase 4 applied to binance. The `.P`
//! suffix answers *spot or futures*; it cannot answer *which futures book*, because binance runs
//! two — USDⓈ-M on `fapi` and COIN-M (coin-margined, inverse) on `dapi` — and 0061 REFUSES to split
//! [`vike_model::AssetClass::CryptoPerp`] into linear and inverse, so a caller's class claim
//! cannot answer it either. **The third input is the VENUE'S OWN LISTING**
//! ([`crate::instruments`]), which is the one answer that is neither a symbol-shape guess nor an
//! encoding of the route into the vocabulary.
//!
//! [`route_target`] is the whole decision and it is PURE — the listing arrives as an injected
//! closure, consulted only when it can change the outcome, so a spot backfill opens no extra
//! socket. Four properties worth carrying before editing anything here:
//!
//! * **`BTCUSD_PERP.P` is the spelling, and `BTCUSD.P` is not.** MEASURED: dapi's `symbol` values
//!   are disjoint from fapi's and from spot's Trading set, while dapi's `pair` values collide with
//!   four live spot pairs (`BNBUSD`, `BTCUSD`, `ETHUSD`, `SOLUSD`) and `BTCUSD` on fapi answers
//!   `-1121 Invalid symbol`. The name is the venue's `symbol`, never its `pair`.
//! * **`.P` is not being made to mean two things.** It says PERPETUAL, binance's own listing says
//!   `contractType: "PERPETUAL"` for `BTCUSD_PERP`, so the suffix is TRUE and the coin-margining
//!   rides on the listing. A DATED COIN-M contract reached through the same suffix is REFUSED
//!   ([`dated_contract_refusal`]) rather than routed, which is what keeps that true.
//! * **fapi serves the COIN-M tape**, byte-identically to dapi (MEASURED). So the third host is not
//!   what makes the bars reachable — [`crate::family::klines::VolumeColumn`] is. Index 5 on a
//!   COIN-M row is a CONTRACT COUNT, not the base asset, and routing on the venue's leniency would
//!   have fetched correct-looking bars with the wrong unit in `Bar::volume`.
//! * **The picker still cannot offer one.** `crates/bridges/binance/src/catalog.rs` fetches spot
//!   and fapi only, so no COIN-M instrument exists in the catalog by any path, and this module's
//!   reach is the CLI and hand-typed callers. That is stated rather than fixed — see this crate's
//!   `CLAUDE.md`.

use std::borrow::Cow;
use std::time::Duration;

use vike_model::Bar;
use vike_model::rate_limits::PaceSample;
use vike_model::venue_rate_limits::{BINANCE_PERP, BINANCE_SPOT};

use crate::VENUE;
use crate::family::klines::{self, KlineRateLimit, KlineSpec, VolumeColumn};

/// Re-exported pure map (venue-agnostic) — kept at `vike_binance::data::parse_klines` for
/// vike-backfill's backfill + its fixture test. See [`klines::parse_klines`].
pub use crate::family::klines::parse_klines;

const REST_KLINES: &str = "https://api.binance.com/api/v3/klines";
/// USDS-M futures (fapi) twin of [`REST_KLINES`] — same response shape, just a different host/path.
/// Reached by the live feed's perp seed (`fetch_klines_latest(.., perp: true)`) AND by the paged
/// range backfill, which derives the same flag from the caller's [`vike_catalog::PERP_SUFFIX`].
const REST_KLINES_FAPI: &str = "https://fapi.binance.com/fapi/v1/klines";

/// Where SPOT publishes its own `REQUEST_WEIGHT` budget — the same host [`REST_KLINES`] pages, which
/// is the only host whose budget applies to it. Read once per backfill by
/// [`klines::fetch_klines_range`]; see that module's "Discovered pacing".
const REST_EXCHANGE_INFO: &str = "https://api.binance.com/api/v3/exchangeInfo";
/// The fapi twin of [`REST_EXCHANGE_INFO`], paired with [`REST_KLINES_FAPI`]. A SEPARATE const for
/// the same reason the two specs are separate: fapi's published budget is 2400/min, spot's is 6000.
const REST_EXCHANGE_INFO_FAPI: &str = "https://fapi.binance.com/fapi/v1/exchangeInfo";

/// COIN-M (coin-margined / inverse) futures — the THIRD host, and the one this bridge had no
/// spelling for at all until now. A symbol here is binance's own (`BTCUSD_PERP`), and WHICH symbols
/// those are is read off the venue rather than guessed: [`crate::instruments`] is the third routing
/// input, and its module doc is the authority for why a symbol-shape test and a caller's asset-class
/// claim are both refused as answers.
///
/// ⚠ **This host is not needed to FETCH a COIN-M bar, and it is still the right host.** MEASURED
/// 2026-09-16: `fapi/v1/klines?symbol=BTCUSD_PERP` answers BYTE-IDENTICALLY to
/// `dapi/v1/klines?symbol=BTCUSD_PERP`, and dapi answers for `BTCUSDT` too — binance's two futures
/// hosts route each other's symbols. Relying on that would be relying on the venue's LENIENCY, the
/// exact shape `crates/bridges/bybit/src/data.rs`'s `ambiguous_bare_symbol_refusal` refuses to
/// document as a cure ("a property of the venue's API, not a claim anybody made"). Routing on the
/// listing instead means the request says what it means, and it is what makes
/// [`BINANCE_KLINE_COIN_M`]'s volume column reachable — which fapi's leniency does NOT give you.
const REST_KLINES_DAPI: &str = "https://dapi.binance.com/dapi/v1/klines";
/// The dapi twin of [`REST_EXCHANGE_INFO`]. Its published `REQUEST_WEIGHT` `MINUTE` is 2400 —
/// fapi's number, MEASURED — but it is still a SEPARATE const because a budget belongs to the HOST
/// that published it; see [`BINANCE_KLINE_COIN_M`].
const REST_EXCHANGE_INFO_DAPI: &str = "https://dapi.binance.com/dapi/v1/exchangeInfo";

/// Binance's SPOT paged-backfill budget. **MEASURED 2026-08-04** from the CI box, not assumed:
/// `x-mbx-used-weight` reports **2** for `/api/v3/klines?limit=1000`, against the
/// `REQUEST_WEIGHT` `MINUTE` limit of **6000** in live `exchangeInfo`. That is 3000 req/min of
/// headroom; `page_delay` of 50ms yields ~1200 req/min ⇒ ~2400 weight/min, ~40% of budget.
///
/// Those numbers are now the FALLBACK, not the plan: with [`REST_EXCHANGE_INFO`] wired, the pager
/// reads the 6000 off the venue each run and derives the gap from the observed per-request weight,
/// so `page_delay` applies only when discovery does not answer. It is kept — and kept accurate —
/// precisely because that is a mode the pager must still be correct in.
///
/// **The two rate numbers below are no longer this crate's to choose.** `page_delay` and
/// `weight_soft_limit` come from [`vike_model::venue_rate_limits::BINANCE_SPOT`], the workspace's
/// per-venue rate-limit table, which also carries the 6000 ceiling they are sized against and
/// enforces `soft_limit < ceiling` at compile time for every venue at once. The retry/backoff knobs
/// beside them (`weight_cooldown`, `max_rate_limit_retries`, `initial_backoff`, `max_backoff`) are
/// this pager's own POLICY, not venue facts, so they stay here.
const BINANCE_KLINE_SPOT: KlineSpec = KlineSpec {
    venue: VENUE,
    rate_limit: KlineRateLimit {
        page_delay: BINANCE_SPOT.history.page_delay(),
        weight_soft_limit: BINANCE_SPOT.history.soft_limit(),
        weight_cooldown: Duration::from_secs(10),
        max_rate_limit_retries: 6,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(60),
    },
    // `Cow::Borrowed` of the SAME `&'static str` this field held before it became a `Cow`: binance's
    // hosts are compile-time constants, so the spec stays a `const` and the request is unchanged.
    exchange_info_url: Some(Cow::Borrowed(REST_EXCHANGE_INFO)),
    utilization: vike_model::rate_limits::DEFAULT_UTILIZATION,
    volume_column: VolumeColumn::Index5,
};

/// Binance's USDⓈ-M FUTURES budget — a SEPARATE const, because fapi's is not spot's.
///
/// **MEASURED 2026-08-04**: fapi `exchangeInfo` reports `REQUEST_WEIGHT` `MINUTE` = **2400**
/// (spot's is 6000), and `/fapi/v1/klines?limit=1000` costs weight **5** (confirmed on a clean
/// probe of Aster's byte-identical fapi twin, whose counter carried no concurrent traffic).
/// 480 req/min ceiling. See the RTT note below before reasoning about what a delay costs.
///
/// ⚠ **`weight_soft_limit` MUST stay below 2400.** Both hosts previously shared ONE spec with
/// `weight_soft_limit: 5000` — the SPOT budget. On this path that guard is unreachable dead code:
/// binance's hard 2400 ceiling (HTTP 418 → IP ban) fires long before a 5000 counter could. It was
/// harmless only while a fixed 300ms delay held usage near 600/min — safety by accident, not by
/// design — and became reachable at all when #1029 first routed the range backfill to fapi.
///
/// ⚠ **Reason about this budget as sleep + RTT, never sleep alone.** The zero-RTT arithmetic says
/// 150 ms ⇒ 400 req/min ⇒ ~2000 weight/min, 83 % of the ceiling — and it is wrong by ~3.6x, because
/// each request's own round trip (~290 ms from the CI box) dominates the sleep. MEASURED end-to-end
/// 2026-08-04: a full 24-month `BTCUSDT.P` backfill at this delay finished in **8m00s** and left
/// `x-mbx-used-weight-1m` at **556**, i.e. **~23 %** of the 2400 ceiling. An earlier draft of this
/// comment quoted the 83 % figure and argued for doubling the delay; it would have made a
/// comfortably-safe path 2x slower for nothing.
///
/// The pacer HAS since folded RTT in: each page is wall-clocked and fed to
/// `Pacer::observe_request`, which subtracts the measured request time from the next sleep, so
/// `utilization` now means what it says END TO END (0.40 of 2400 ⇒ ~192 req/min ⇒ ~960 weight/min,
/// versus the ~506 the sleep-only arithmetic delivered). Two things follow. First, this
/// `page_delay` is now a genuinely SLOWER fallback than the discovered pace, not a coincidentally
/// similar one — which is the right shape for a fallback. Second, `utilization` became a live knob:
/// raising it now raises real consumption roughly proportionally, where before it bought about half
/// of what it asked for. Keep it under `weight_soft_limit`'s share of the ceiling.
///
/// Same split of ownership as the spot spec above: `page_delay`/`weight_soft_limit` come from
/// [`vike_model::venue_rate_limits::BINANCE_PERP`] (which carries the 2400 ceiling and asserts the
/// invariant), the retry/backoff knobs stay this pager's own policy.
const BINANCE_KLINE_PERP: KlineSpec = KlineSpec {
    venue: VENUE,
    rate_limit: KlineRateLimit {
        page_delay: BINANCE_PERP.history.page_delay(),
        weight_soft_limit: BINANCE_PERP.history.soft_limit(),
        weight_cooldown: Duration::from_secs(10),
        max_rate_limit_retries: 6,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(60),
    },
    exchange_info_url: Some(Cow::Borrowed(REST_EXCHANGE_INFO_FAPI)),
    utilization: vike_model::rate_limits::DEFAULT_UTILIZATION,
    volume_column: VolumeColumn::Index5,
};

/// Binance's COIN-M (coin-margined, inverse) budget — the THIRD spec, for the third host.
///
/// ⚠ **Its rate-limit knobs are `BINANCE_PERP`'s, and that is a MEASUREMENT rather than a
/// shortcut.** 2026-09-16, live: `dapi/v1/exchangeInfo` publishes `REQUEST_WEIGHT` `MINUTE` =
/// **2400**, the same number fapi publishes, and `dapi/v1/klines?limit=1000` costs weight **5**
/// (two successive calls moved `x-mbx-used-weight-1m` 9 -> 14), the same as fapi's. So the two
/// futures hosts are one budget wearing two names, which is why this crate adds no third
/// `vike_model::venue_rate_limits` row and why [`kline_market`] stays two-valued —
/// `the_coin_m_host_is_paced_on_the_measured_twin_of_fapis_budget` is the assertion that stops
/// that claim rotting silently.
///
/// What is NOT shared is the DISCOVERY URL: a published budget belongs to a HOST, and
/// [`klines::discovery_url`] refuses a spec whose `exchange_info_url` is on a different host from
/// the base being paged. Pairing dapi's pager with fapi's `exchangeInfo` would degrade to the fixed
/// fallback pace rather than pace against a counter on another machine.
const BINANCE_KLINE_COIN_M: KlineSpec = KlineSpec {
    venue: VENUE,
    rate_limit: BINANCE_KLINE_PERP.rate_limit,
    exchange_info_url: Some(Cow::Borrowed(REST_EXCHANGE_INFO_DAPI)),
    utilization: vike_model::rate_limits::DEFAULT_UTILIZATION,
    // ⚠ THE ONE FIELD THAT DIFFERS, and the reason this spec exists as more than a host swap. A
    // COIN-M row's index 5 is a CONTRACT COUNT and its index 7 is the base asset — the opposite of
    // every other binance book. `VolumeColumn`'s own doc carries the measurement and the arithmetic.
    volume_column: VolumeColumn::Index7,
};

// The two published `REQUEST_WEIGHT` `MINUTE` ceilings (SPOT 6000, fapi 2400 — MEASURED 2026-08-04
// from live `exchangeInfo` on both hosts) and the COMPILE-TIME invariant that keeps each spec's
// `weight_soft_limit` under its own host's ceiling now live in
// `vike_model::venue_rate_limits::{BINANCE_SPOT, BINANCE_PERP}`, beside every other venue's. They
// are FACTS ABOUT THE VENUE, not tuning values — not ours to raise — which is why they belong in the
// per-venue capability table rather than in the one consumer that happened to read them.
//
// The invariant itself is unchanged in strength: a soft limit at or above the hard ceiling can never
// fire, so the guard silently becomes dead code and the venue's 418 -> IP ban is what stops you
// instead. Both hosts shared ONE spec at 5000 (the SPOT budget) until 2026-08-04 and nothing caught
// it, because nothing read the number. The build still refuses — one `const _: ()` per table row.

/// Spot vs USDS-M-futures host — the ONE place that decides fapi-vs-api, for BOTH
/// `fetch_klines_latest`'s seed request and `fetch_klines_range`'s paged backfill, so the
/// URL-building tests below and the real fetches never drift.
///
/// ⚠ **Two-valued on purpose, and it is no longer the whole host decision.** A `bool` can name two
/// books and binance serves three; the third is reached through [`Book`]/[`book_target`], which
/// this function is now the two-arm face of. Nothing that took the two-book answer changed
/// meaning: `perp` here means USDⓈ-M, exactly as it always did.
fn rest_klines_base(perp: bool) -> &'static str {
    book_target(if perp { Book::UsdM } else { Book::Spot }).0
}

/// **The three books binance's kline REST serves.** The type that exists because
/// [`rest_klines_base`]'s `bool` is spent: two hosts chosen by one boolean cannot express three,
/// and widening the boolean into a second boolean would encode the third book as "perp and also
/// something", which is the implicit encoding
/// `docs/decisions/0061-an-instrument-names-its-kind.md` exists to remove.
///
/// ⚠ These are BOOKS, not asset classes. `Book::UsdM` and `Book::CoinM` are both
/// [`vike_model::AssetClass::CryptoPerp`] for a perpetual — 0061 REFUSES to split that variant
/// ("the vocabulary names the PRODUCT, never the venue's word for the route"), so this enum is
/// deliberately local to the one file that routes, and nothing in `vike-catalog` learns the word
/// "coin-margined".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Book {
    /// `api.binance.com` — the cash book.
    Spot,
    /// `fapi.binance.com` — USDⓈ-M (linear, stablecoin-margined) futures.
    UsdM,
    /// `dapi.binance.com` — COIN-M (inverse, coin-margined) futures.
    CoinM,
}

/// The ONE place a book becomes a host and a budget. Host and budget are one decision, never two:
/// pairing a host with another host's `REQUEST_WEIGHT` is the defect the two-spec split already
/// documents, and a third host is a third chance to make it.
fn book_target(book: Book) -> (&'static str, &'static KlineSpec) {
    match book {
        Book::Spot => (REST_KLINES, &BINANCE_KLINE_SPOT),
        Book::UsdM => (REST_KLINES_FAPI, &BINANCE_KLINE_PERP),
        Book::CoinM => (REST_KLINES_DAPI, &BINANCE_KLINE_COIN_M),
    }
}

/// The live feed's warmup seed: the newest `limit` klines (no time bound), the last of which is the
/// still-forming bar. `perp = true` routes to the USDS-M futures host ([`REST_KLINES_FAPI`]) instead
/// of spot ([`REST_KLINES`]). Delegates to [`klines::fetch_klines_latest`].
///
/// ⚠ **A COIN-M symbol is REFUSED here rather than seeded, and the refusal is not about this
/// request.** fapi would answer it — MEASURED, byte-identically to dapi — so this could easily have
/// been routed like the range fetch. It is not, because the seed is the FIRST HALF of a live
/// subscription whose SECOND half is unproven: nothing in this workspace has measured
/// `dstream.binance.com` or a COIN-M `@kline_1m`/`@markPrice` stream, and
/// `crates/bridges/binance/src/family/market_feed.rs`'s `feed_main` takes
/// `crates/bridges/binance/src/market_feed.rs`'s `BINANCE_URLS`, a FOUR-field `UrlTable` with no
/// third host in it. Seeding real bars into a series whose stream then pushes nothing is the exact
/// incident class `crates/vike-catalog/src/symbol.rs`'s module doc records — a feed that
/// "connected, streamed nothing, and the recorder wrote placeholder rows for hours without a single
/// error" — and a warm, correct-looking chart is the most convincing possible disguise for it.
///
/// The refusal is FAIL-SOFT by construction, which is why it is affordable: `feed_main` renders a
/// seed error as a status string and keeps streaming
/// (`Err(e) => ctx.set_status(format!("{key} seed error: {e}"))`), so this neither kills a
/// subscription nor a mount.
pub fn fetch_klines_latest(
    symbol: &str,
    interval: &str,
    limit: usize,
    perp: bool,
) -> Result<Vec<Bar>, String> {
    if perp && crate::instruments::coin_m_contract(symbol)?.is_some() {
        return Err(live_seed_refusal(symbol));
    }
    klines::fetch_klines_latest(rest_klines_base(perp), symbol, interval, limit, VENUE)
}

/// The refusal a COIN-M symbol gets from the LIVE feed's warmup seed. Its job is to leave an ACT,
/// so it names the path that IS reachable rather than saying "unsupported".
fn live_seed_refusal(wire: &str) -> String {
    format!(
        "binance: {wire:?} is a COIN-M (coin-margined) contract, and this venue's LIVE feed has no \
         COIN-M plane — `crates/bridges/binance/src/market_feed.rs`'s `BINANCE_URLS` is a \
         four-field spot/USDⓈ-M table, so the stream this seed is warming up would be \
         `wss://stream.binance.com`/`wss://fstream.binance.com` for a symbol neither host lists. \
         Nobody has MEASURED `dstream.binance.com` from this workspace, so the seed is refused \
         rather than served from fapi (which does answer for it — that is the trap, not the cure). \
         The HISTORICAL path IS reachable: `vike_binance::data::fetch_klines_range` routes \
         {wire:?}{} to the COIN-M host and reads the base-asset volume column. What this refusal \
         is waiting on is a WS measurement plus a third `UrlTable` host, not a routing change.",
        vike_catalog::PERP_SUFFIX
    )
}

/// Fetch closed-kline history for the inclusive `[start_ms, end_ms]` window, paging through Binance's
/// 1000-rows/response cap under the family rate-limit policy. A `symbol` carrying
/// [`vike_catalog::PERP_SUFFIX`] routes to the USDⓈ-M futures host and is stripped before it reaches
/// the wire; the CALLER's suffixed symbol remains the store/series key, so a perp's series can never
/// collide with its spot twin's. Delegates to [`klines::fetch_klines_range`] with Binance's
/// [`BINANCE_KLINE`] spec.
/// ⚠ **This is the NO-CLAIM entry point, and it is now the one that can REFUSE.** A `.P` symbol
/// binance lists as a DATED COIN-M contract comes back as an `Err` rather than a tape, and so does
/// a route whose COIN-M listing could not be read — see [`route_target`] and
/// [`crate::instruments`]. Callers that KNOW the class bind [`fetch_klines_range_classed`].
pub fn fetch_klines_range(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    fetch_klines_range_classed(symbol, interval, start_ms, end_ms, None)
}

/// [`fetch_klines_range`] with the caller's asset-class CLAIM — the seam
/// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 3 specifies, spelled as an
/// `Option<AssetClass>` because most callers genuinely have no class to give. The binance twin of
/// `vike_bybit::data::fetch_klines_range_classed`.
///
/// ⚠ **A claim does NOT reach a different book here than the suffix does**, and that is the one
/// place this differs from bybit's. `CryptoPerp` names both of binance's futures books (0061
/// refuses to split the variant), so the claim can only say spot-or-futures; which futures book is
/// always the venue's listing's answer. What the claim buys is a caller who holds a class not
/// having to synthesise a `.P` string to express it.
pub fn fetch_klines_range_classed(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    class: Option<vike_model::AssetClass>,
) -> Result<Vec<Bar>, String> {
    let (base, wire, spec) = range_target(symbol, class)?;
    klines::fetch_klines_range(base, wire, interval, start_ms, end_ms, spec)
}

/// [`fetch_klines_range`] with the pace measurement carried across runs — `seed` is the previous
/// run's observation for THIS symbol's market, and the second element of the return is this run's
/// (`None` unless a page was timed). See [`klines::fetch_klines_range_paced`]; `fetch_klines_range`
/// is this call with `None` in and the report dropped, so it is byte-identical to what it was.
///
/// Pair the seed with [`kline_market`] — a spot record must never seed a perp fetch, and the pacer
/// enforces that independently by refusing a sample whose budget is not the one it discovered.
pub fn fetch_klines_range_paced(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let (base, wire, spec) = range_target(symbol, None)?;
    klines::fetch_klines_range_paced(base, wire, interval, start_ms, end_ms, spec, seed)
}

/// [`fetch_klines_range_paced`] with BOUNDED CONCURRENCY — several disjoint sub-windows in flight
/// instead of one, at the SAME aggregate rate. See [`klines::fetch_klines_range_lanes`] for the full
/// contract; the two facts a caller here needs:
///
/// * **The venue-facing rate does not change.** Every lane draws its dispatch slot from one shared
///   pacer, so binance still sees `utilization` x its published `REQUEST_WEIGHT` ceiling. What
///   changes is that the target becomes reachable: a sequential pager cannot space requests closer
///   than one request TAKES, and on the SPOT host the target gap (50 ms at the 0.40 default and the
///   measured weight-2 page cost) is far narrower than the ~280 ms round trip.
/// * **The lane count is derived per host, and is often 1.** `ceil(request_time / target_gap)` from
///   this run's own probe pages — MEASURED, that is **1 lane on fapi** (a 312 ms target gap is wider
///   than the round trip, so perp backfills are already at target and this call is the sequential one
///   with an extra `Option`) and **6 on spot**. ⚠ The spot figure needs the venue's real weight-2
///   page cost, which is a DELTA and therefore takes TWO readings — [`klines::PROBE_PAGES`] is that
///   rule. With one probe page the pacer is still on its pessimistic seed and would derive 3 lanes
///   on a gap twice too wide.
///
/// Worth, on spot: 213 requests/min sequential (the round trip plus a floored sleep) against the
/// gate's 1200/min ⇒ a 24-month 1m backfill goes ~4.9 min -> ~55 s, about **5.4x**, settling nearer
/// 4.8x on a longer run once the end-of-window cooldown starts firing. Arithmetic from the measured
/// round trip and the published budget — **not timed end to end**. Perp is unchanged.
///
/// The cap is [`vike_bridge_core::concurrent::MAX_LANES`] — a bound on THREADS and sockets, not on
/// rate, applied HERE (the venue face) rather than asked of every caller, so a backfill seam never
/// has to hold an opinion about a number that cannot change what the venue is sent. A caller that
/// wants the strictly sequential path calls [`fetch_klines_range_paced`], which is unchanged.
pub fn fetch_klines_range_lanes(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let (base, wire, spec) = range_target(symbol, None)?;
    klines::fetch_klines_range_lanes(
        base,
        wire,
        interval,
        start_ms,
        end_ms,
        spec,
        seed,
        vike_bridge_core::concurrent::MAX_LANES,
    )
}

/// Which MARKET a caller's symbol routes to — `"perp"` for a [`vike_catalog::PERP_SUFFIX`] symbol,
/// `"spot"` otherwise. The half of a persisted pace record's key that is not the venue.
///
/// Exposed (rather than left to the caller's own suffix check) because it is the SAME routing
/// decision [`range_target`] makes, and the two must not be able to disagree: binance's hosts
/// publish different budgets (6000 vs 2400/min) and price `/klines` differently (weight 2 vs 5), so
/// a record filed under the wrong market is a record that paces the other host wrong.
/// ⚠ **Still TWO-valued with three books, and that is deliberate.** This key names a BUDGET, not a
/// host: it is the second half of a `vike_model::rate_limits::PaceBook` key, and the only thing a
/// persisted record may not do is pace one host against another host's counter. Binance's two
/// FUTURES hosts publish ONE budget — MEASURED 2026-09-16, `REQUEST_WEIGHT` `MINUTE` = 2400 on both
/// and weight 5 per `limit=1000` page on both — so a COIN-M record filed under `"perp"` is seeding
/// a pacer with a sample taken against the identical ceiling and per-page cost.
///
/// That claim is not left to prose: [`BINANCE_KLINE_COIN_M`] takes its `rate_limit` from
/// [`BINANCE_KLINE_PERP`] by CONSTRUCTION, and
/// `the_coin_m_host_is_paced_on_the_measured_twin_of_fapis_budget` asserts the two equal. Giving
/// dapi its own budget is therefore a compile-or-test failure that says "now split this key" —
/// which is also why no third `vike_model::venue_rate_limits::Market` variant was minted for a
/// number that is the same number.
pub fn kline_market(symbol: &str) -> &'static str {
    if vike_catalog::split_perp_at(VENUE, symbol).1 { "perp" } else { "spot" }
}

/// The `(base, wire_symbol, rate_limit_spec)` triple a range fetch routes to — extracted out of
/// [`fetch_klines_range`] so the routing DECISION is assertable without network I/O.
///
/// Testing [`rest_klines_base`] directly proves nothing about this: that helper has understood
/// `perp` since the live warmup seed was written, and the bug was that `fetch_klines_range` never
/// asked it. This function is what the fetcher actually calls, so a test against it fails if the
/// fetcher is ever re-pinned to spot — and if the fetcher stops calling it, `dead_code` trips the
/// `-D warnings` merge gate.
///
/// The spec rides along because host and budget are ONE decision, not two: fapi's 2400-weight
/// ceiling belongs to the fapi host, and pairing a host with the other host's budget is exactly
/// the defect the two consts above document.
///
/// The impure half of [`route_target`]: it supplies the COIN-M listing lookup, and does so **only
/// when the answer can change the outcome** — see [`route_target`]'s `coin_m` parameter.
fn range_target(
    symbol: &str,
    claimed: Option<vike_model::AssetClass>,
) -> Result<(&'static str, &str, &'static KlineSpec), String> {
    route_target(symbol, claimed, &|wire| crate::instruments::coin_m_contract(wire))
}

/// **The whole routing decision, PURE** — `(host, wire_symbol, budget)` or a refusal, given the
/// caller's symbol, the class the caller CLAIMED (if any), and a LOOKUP of what binance's own
/// COIN-M listing says about the wire symbol.
///
/// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 3 is the shape: the seam takes
/// `Option<AssetClass>`, and a claim that cannot be honoured is a REFUSAL rather than a coercion.
/// The binance twin of `crates/bridges/bybit/src/data.rs`'s function of the same name, with one
/// difference that is the whole point of this file: **bybit needed the claim alone, binance needs
/// the claim AND the listing**, because the two books the listing separates are the SAME
/// `AssetClass` (0061 refuses to split `CryptoPerp` into linear and inverse). The claim decides
/// spot-vs-futures; the listing decides USDⓈ-M-vs-COIN-M. Neither can answer the other's question.
///
/// `coin_m` is a CLOSURE rather than a resolved value so the decision is both injected (assertable
/// with no network) and LAZY: a bare, unclaimed symbol is spot and never calls it, which is every
/// spot backfill in the workspace. `a_spot_route_never_consults_the_coin_m_listing` proves that by
/// passing a lookup that panics.
///
/// The arms, in order:
///
/// 1. **A claim this venue's data path cannot address** (`Equity`, `CryptoFuture`, …) is refused
///    rather than coerced. `vike_catalog::addressing_for` is the authority, not a list here — which
///    is also why binance's COIN-M QUARTERLIES are refused: they are `CryptoFuture`, a class that
///    row does not name, because `crates/bridges/binance/src/catalog.rs` mints none.
/// 2. **A claim that CONTRADICTS the symbol's own suffix** (`BTCUSDT.P` claimed as spot) is
///    refused. Two claims disagreeing is not a case with a right answer.
/// 3. **Not heading for a futures book** — bare and unclaimed, or claimed spot — routes to spot,
///    unchanged, without consulting anything.
/// 4. **Heading for a futures book**: the listing decides. Absent from it is USDⓈ-M (today's
///    behaviour, byte-identical); `PERPETUAL` is COIN-M; a DATED contract is refused, because the
///    `.P` marker it was reached through says PERPETUAL and the venue says otherwise.
///
/// ⚠ **`.P` says PERPETUAL, and on this venue it is not lying.** `BTCUSD_PERP.P` names binance's
/// own `BTCUSD_PERP`, which binance's own listing calls `contractType: "PERPETUAL"` — so the
/// suffix is a TRUE statement and the coin-margining is carried by the listing, not smuggled into
/// the suffix. That is the distinction bybit's refusal insists on (`.P` there landed on an inverse
/// book by the venue's leniency, which is a different thing entirely), and it is why this route
/// can use the suffix where bybit's could not.
fn route_target<'a>(
    symbol: &'a str,
    claimed: Option<vike_model::AssetClass>,
    coin_m: &dyn Fn(&str) -> Result<Option<crate::instruments::CoinMContract>, String>,
) -> Result<(&'static str, &'a str, &'static KlineSpec), String> {
    use vike_model::AssetClass;
    // `split_perp_at`, not `split_perp`: this function already asks `addressing_for(VENUE)` two
    // lines down, and the suffix question belongs to the same row. Byte-identical here — binance
    // IS a `Naming::PerpSuffix` venue — but it means the two reads cannot answer from different
    // tables, which is `docs/decisions/0061`'s Phase 1 objection.
    let (wire, suffixed) = vike_catalog::split_perp_at(VENUE, symbol);
    let wants_futures = match claimed {
        None => suffixed,
        Some(class) => {
            if !vike_catalog::addressing_for(VENUE).addresses(class) {
                return Err(unaddressable_class_refusal(symbol, class));
            }
            let wants = matches!(class, AssetClass::CryptoPerp);
            if suffixed && !wants {
                return Err(contradicting_claim_refusal(symbol, class));
            }
            wants
        }
    };
    if !wants_futures {
        let (base, spec) = book_target(Book::Spot);
        return Ok((base, wire, spec));
    }
    let book = match coin_m(wire)? {
        None => Book::UsdM,
        Some(crate::instruments::CoinMContract::Perpetual) => Book::CoinM,
        Some(dated @ crate::instruments::CoinMContract::Delivery(_)) => {
            return Err(dated_contract_refusal(wire, dated));
        }
    };
    let (base, spec) = book_target(book);
    Ok((base, wire, spec))
}

/// The refusal a caller gets for naming a class this venue's data path does not address. Its job is
/// to leave an ACT, so it names what IS addressable rather than saying "unsupported".
fn unaddressable_class_refusal(symbol: &str, class: vike_model::AssetClass) -> String {
    format!(
        "binance: this venue's kline path addresses {:?}, and {class:?} is not one of them \
         (symbol {symbol:?}). ⚠ If you are reaching for a COIN-M QUARTERLY \
         (`BTCUSD_260925` and its nine siblings, `contractType` `CURRENT_QUARTER`/`NEXT_QUARTER`): \
         those are CryptoFuture, and this row does not name that class because \
         `crates/bridges/binance/src/catalog.rs` mints no instrument for one — the picker cannot \
         offer it and no series key exists for it. The COIN-M PERPETUALS are reachable, as \
         `<venue symbol>{}` (e.g. `BTCUSD_PERP{}`).",
        vike_catalog::addressing_for(VENUE).classes,
        vike_catalog::PERP_SUFFIX,
        vike_catalog::PERP_SUFFIX
    )
}

/// The refusal for a symbol whose suffix and whose caller disagree. Copied in shape from bybit's
/// twin, down to the "drop one of them" instruction — neither claim is obeyed, because obeying
/// either would be picking a winner between two things the caller said.
fn contradicting_claim_refusal(symbol: &str, class: vike_model::AssetClass) -> String {
    format!(
        "binance: {symbol:?} carries the {:?} perpetual marker but the caller claimed {class:?} — \
         two claims that disagree, so neither is obeyed. Drop the suffix or drop the claim.",
        vike_catalog::PERP_SUFFIX
    )
}

/// The refusal a DATED COIN-M contract gets when it is reached through the perpetual marker.
///
/// ⚠ Worth refusing rather than routing, even though the host and the volume column would both be
/// right: the [`vike_catalog::PERP_SUFFIX`] is the only thing that carried it here, that suffix
/// SAYS perpetual, and the venue's own listing says this contract is not one. Routing it would make
/// `.P` mean "perpetual, or a dated future, whichever the venue happens to have" — a suffix that
/// already means one thing being made to mean two, which is exactly what
/// `docs/decisions/0061-an-instrument-names-its-kind.md` forbids. There is no second spelling to
/// recommend, so this message says so outright instead of inventing one.
fn dated_contract_refusal(wire: &str, dated: crate::instruments::CoinMContract) -> String {
    format!(
        "binance lists {wire:?} as a COIN-M {} contract — a DATED delivery future — and it was \
         reached through the {:?} marker, which says PERPETUAL. Those disagree, so the route is \
         refused rather than guessed. ⚠ There is deliberately NO other spelling to reach it with: \
         a dated future is `vike_model::AssetClass::CryptoFuture`, `vike_catalog::addressing_for`'s \
         binance row does not name that class, and \
         `crates/bridges/binance/src/catalog.rs` mints no instrument for one — so nothing in this \
         workspace can offer it, key a series for it, or price it. Admitting COIN-M quarterlies is \
         a catalog change first and a routing change second, in that order. The COIN-M PERPETUALS \
         (`contractType: \"PERPETUAL\"`, 20 of the venue's 30 COIN-M rows) ARE reachable through \
         this path.",
        dated.venue_word(),
        vike_catalog::PERP_SUFFIX
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::family::klines::klines_url;
    use crate::instruments::CoinMContract;
    use std::cell::Cell;

    /// The COIN-M listing binance actually publishes, as a lookup [`route_target`] can be driven
    /// over with no socket. Keyed on the venue's OWN `symbol` values, which is the whole point.
    fn listing(wire: &str) -> Result<Option<CoinMContract>, String> {
        Ok(match wire {
            "BTCUSD_PERP" | "ETHUSD_PERP" => Some(CoinMContract::Perpetual),
            "BTCUSD_260925" => Some(CoinMContract::Delivery("CURRENT_QUARTER")),
            "BTCUSD_261225" => Some(CoinMContract::Delivery("NEXT_QUARTER")),
            _ => None,
        })
    }

    /// A lookup that FAILS — the venue's list unreadable. Not the same answer as "not COIN-M".
    fn unreadable(_: &str) -> Result<Option<CoinMContract>, String> {
        Err("dapi.binance.com/dapi/v1/exchangeInfo GET: connection refused".into())
    }

    /// [`route_target`] over the planted listing above — the impure half's twin, with the network
    /// replaced rather than mocked away.
    fn routed(
        symbol: &str,
        claimed: Option<vike_model::AssetClass>,
    ) -> Result<(&'static str, &str, &'static KlineSpec), String> {
        route_target(symbol, claimed, &listing)
    }

    /// The FETCHER's own routing decision — the gate the URL-shaping tests below cannot provide,
    /// because they build a URL from `rest_klines_base` directly and that helper was already
    /// perp-capable before this fix. Re-pinning `fetch_klines_range` to spot fails HERE.
    #[test]
    fn range_target_routes_a_perp_symbol_to_fapi_with_the_suffix_stripped() {
        let (base, wire, spec) = routed("BTCUSDT.P", None).expect("a USDⓈ-M perp routes");
        assert_eq!((base, wire), (REST_KLINES_FAPI, "BTCUSDT"));
        assert_eq!(spec, &BINANCE_KLINE_PERP, "the fapi host must carry the FAPI budget");
    }

    /// A bare symbol still resolves the spot host and is passed through untouched.
    #[test]
    fn range_target_routes_a_bare_symbol_to_spot_unchanged() {
        let (base, wire, spec) = routed("BTCUSDT", None).expect("a bare symbol routes");
        assert_eq!((base, wire), (REST_KLINES, "BTCUSDT"));
        assert_eq!(spec, &BINANCE_KLINE_SPOT, "the spot host must carry the SPOT budget");
    }

    /// The persisted-pace key must be the SAME decision the fetcher routes on, or a record files
    /// itself under one market and paces the other. Asserted against `range_target` itself rather
    /// than against a `.P` literal, which is what makes the two unable to drift.
    #[test]
    fn kline_market_agrees_with_the_fetchers_own_routing() {
        for symbol in ["BTCUSDT", "BTCUSDT.P", "ETHUSDT", "ETHUSDT.P", "1000PEPEUSDT.P"] {
            let (base, _, spec) = routed(symbol, None).expect("routes");
            let expected = if base == REST_KLINES_FAPI { "perp" } else { "spot" };
            assert_eq!(kline_market(symbol), expected, "{symbol} routes to {base}");
            // ...and the market therefore names the budget that record was measured against.
            let want = if expected == "perp" { BINANCE_KLINE_PERP } else { BINANCE_KLINE_SPOT };
            assert_eq!(spec, &want);
        }
        // The two markets are genuinely distinct keys, so a perp record can never overwrite a spot
        // one on the same venue.
        assert_ne!(kline_market("BTCUSDT"), kline_market("BTCUSDT.P"));
    }

    /// The ceilings are asserted at COMPILE time (now in `vike_model::venue_rate_limits`, one
    /// `const _: ()` per row), which is strictly stronger than this test could be — a violation
    /// fails the build rather than a test run.
    ///
    /// What this test does gate is the WIRING: the literals below are the numbers these two specs
    /// carried as local consts before the per-venue table existed, so if the table's binance rows
    /// ever drift, this pager's pacing changes and this test says so. That is the byte-identical
    /// claim of the move, written down.
    #[test]
    fn the_specs_pace_on_the_venue_tables_binance_rows() {
        assert_eq!(BINANCE_KLINE_SPOT.rate_limit.weight_soft_limit, 4800);
        assert_eq!(BINANCE_KLINE_SPOT.rate_limit.page_delay, Duration::from_millis(50));
        assert_eq!(BINANCE_KLINE_PERP.rate_limit.weight_soft_limit, 2000);
        assert_eq!(BINANCE_KLINE_PERP.rate_limit.page_delay, Duration::from_millis(150));
        // ...and each soft limit is genuinely under ITS OWN host's published ceiling (6000 spot,
        // 2400 fapi) — the pairing whose absence let one shared spec sit above fapi's ban threshold.
        assert_eq!(BINANCE_SPOT.history.ceiling_per_min(), 6000);
        assert_eq!(BINANCE_PERP.history.ceiling_per_min(), 2400);
        assert!(
            BINANCE_KLINE_SPOT.rate_limit.weight_soft_limit
                < BINANCE_SPOT.history.ceiling_per_min()
        );
        assert!(
            BINANCE_KLINE_PERP.rate_limit.weight_soft_limit
                < BINANCE_PERP.history.ceiling_per_min()
        );
    }

    /// A discovered budget belongs to the HOST that published it, so each spec's `exchangeInfo` URL
    /// must live on the same host as the `/klines` URL it paces. Crossing them would pace the
    /// 2400-weight fapi host against spot's 6000 — the same class of defect as the shared
    /// `weight_soft_limit: 5000`, only now derived from the venue rather than hand-typed.
    ///
    /// Two things changed when the spec's URL became env-resolvable. The pairing is now the SHARED
    /// [`klines::discovery_url`] — the same gate the runtime consults and the same one aster's tests
    /// drive over its four (`Environment` x market) hosts — instead of a `host()` helper private to
    /// this file. And it is asserted against the base [`range_target`] ACTUALLY RESOLVES rather than
    /// against a literal paired by hand here, so re-pointing the fetcher fails this test too.
    #[test]
    fn each_spec_discovers_the_budget_of_the_host_it_pages() {
        for symbol in ["BTCUSDT", "BTCUSDT.P"] {
            let (base, _, spec) = routed(symbol, None).expect("routes");
            let info = klines::discovery_url(spec, base)
                .unwrap_or_else(|| panic!("{symbol}: binance publishes a budget on both hosts"));
            assert_eq!(
                klines::url_host(info),
                klines::url_host(base),
                "{info} must be the budget of {base}'s host"
            );
        }
        // ...and the two hosts are genuinely different, so the pairing above is not vacuous.
        assert_ne!(BINANCE_KLINE_SPOT.exchange_info_url, BINANCE_KLINE_PERP.exchange_info_url);
        // CROSSED, which is the defect itself: each spec against the OTHER host's base discovers
        // nothing at all, so a mispairing degrades to the fixed `page_delay` instead of pacing fapi
        // against spot's 6000.
        assert_eq!(klines::discovery_url(&BINANCE_KLINE_SPOT, REST_KLINES_FAPI), None);
        assert_eq!(klines::discovery_url(&BINANCE_KLINE_PERP, REST_KLINES), None);
    }

    /// **Byte-identity of the discovery request across the `&'static str` -> `Cow` change.** The
    /// URL each spec is asked for is the SAME literal it named before, so both hosts issue the same
    /// one `exchangeInfo` GET they issued before, read the same budget, and pace the same way. The
    /// literals are spelled out rather than compared to the consts on purpose: a test that compares
    /// a const to itself would pass through a rename of the endpoint.
    #[test]
    fn the_discovery_urls_are_the_literals_they_have_always_been() {
        let (spot_base, _, spot) = routed("BTCUSDT", None).expect("routes");
        let (perp_base, _, perp) = routed("BTCUSDT.P", None).expect("routes");
        assert_eq!(
            klines::discovery_url(spot, spot_base),
            Some("https://api.binance.com/api/v3/exchangeInfo")
        );
        assert_eq!(
            klines::discovery_url(perp, perp_base),
            Some("https://fapi.binance.com/fapi/v1/exchangeInfo")
        );
        // Both are BORROWED — binance's hosts are compile-time constants, so nothing is allocated
        // and the two specs remain `const`. (Aster's are `Owned`; that is the capability the field's
        // type change exists for.)
        for spec in [&BINANCE_KLINE_SPOT, &BINANCE_KLINE_PERP] {
            assert!(
                matches!(spec.exchange_info_url, Some(Cow::Borrowed(_))),
                "a static host must not allocate"
            );
        }
    }

    /// Utilization is vike-model's operator-facing default at every const site — this crate does not
    /// carry a second copy of the number, and a venue does not get to pick its own risk appetite.
    #[test]
    fn both_specs_target_the_shared_default_utilization() {
        for spec in [BINANCE_KLINE_SPOT, BINANCE_KLINE_PERP] {
            assert_eq!(
                spec.utilization.to_bits(),
                vike_model::rate_limits::DEFAULT_UTILIZATION.to_bits()
            );
        }
    }

    // Binance's OWN host-resolution proof (the per-venue delta): the shared pure `parse_klines`/URL
    // shaping is proven once at the rung (`crate::family::klines`); here we prove the api/fapi hosts.
    #[test]
    fn klines_url_includes_optional_bounds() {
        let latest = klines_url(REST_KLINES, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(
            latest,
            "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
        let ranged = klines_url(REST_KLINES, "BTCUSDT", "1m", Some(10), Some(20), 1000);
        assert!(ranged.ends_with("&startTime=10&endTime=20"));
    }

    #[test]
    fn klines_url_perp_uses_fapi_host_spot_uses_api_host() {
        let perp = klines_url(REST_KLINES_FAPI, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(
            perp,
            "https://fapi.binance.com/fapi/v1/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
        let spot = klines_url(REST_KLINES, "BTCUSDT", "1m", None, None, 1000);
        assert_eq!(
            spot,
            "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
    }

    #[test]
    fn fetch_klines_latest_base_url_selects_fapi_for_perp() {
        // rest_klines_base is the pure host-selection the perp seed relies on — no network.
        assert_eq!(rest_klines_base(true), REST_KLINES_FAPI);
        assert_eq!(rest_klines_base(false), REST_KLINES);
    }

    /// The perp suffix selects the fapi host AND is stripped from the wire symbol. This is the
    /// site the `.P` convention was skipped at: before this, `BTCUSDT.P` silently fetched SPOT
    /// bars and stored them under the perp key.
    #[test]
    fn perp_suffix_selects_fapi_and_strips_the_wire_symbol() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT.P");
        assert_eq!(wire, "BTCUSDT");
        assert!(perp);
        let url = klines_url(rest_klines_base(perp), wire, "1m", Some(10), Some(20), 1000);
        assert!(url.starts_with(REST_KLINES_FAPI), "perp must use fapi: {url}");
        assert!(url.contains("symbol=BTCUSDT&"), "the .P must not reach the wire: {url}");
        assert!(!url.contains(".P"), "the .P must not reach the wire: {url}");
    }

    /// A bare symbol is unchanged — spot URLs stay byte-identical.
    #[test]
    fn a_bare_symbol_still_builds_the_spot_url() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT");
        assert!(!perp);
        let url = klines_url(rest_klines_base(perp), wire, "1m", None, None, 1000);
        assert_eq!(
            url,
            "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
        );
    }

    // ---- the THIRD book -----------------------------------------------------------------

    /// **THE FINDING, closed.** A COIN-M perpetual reaches the COIN-M host with the suffix
    /// stripped, and carries the COIN-M budget with it.
    ///
    /// ⚠ This is the test the mutation proof reddens: re-point `book_target`'s `Book::CoinM` arm
    /// (or `route_target`'s listing arm) anywhere else and this fails on the host it actually
    /// resolved. It asserts the RESOLVED triple rather than a hand-paired literal, so a mutation
    /// cannot satisfy it by editing a constant beside it.
    #[test]
    fn a_coin_m_perpetual_routes_to_the_coin_m_host_with_the_suffix_stripped() {
        let (base, wire, spec) = routed("BTCUSD_PERP.P", None).expect("a COIN-M perpetual routes");
        assert_eq!(
            (base, wire),
            (REST_KLINES_DAPI, "BTCUSD_PERP"),
            "a COIN-M symbol must reach the COIN-M host, and the .P must not reach the wire"
        );
        assert_eq!(spec, &BINANCE_KLINE_COIN_M);
        let url = klines_url(base, wire, "1h", Some(10), Some(20), 1000);
        assert_eq!(
            url,
            "https://dapi.binance.com/dapi/v1/klines?symbol=BTCUSD_PERP&interval=1h\
             &limit=1000&startTime=10&endTime=20"
        );
    }

    /// ⚠ **The unit defect, gated at the route.** Index 5 of a COIN-M row is a CONTRACT COUNT and
    /// index 7 is the base asset; every other binance book is the other way round. A COIN-M route
    /// that carried `Index5` would fetch correct-looking bars with a ~760x wrong `Bar::volume` —
    /// and 0059's self-sealing commit key would make that window unrepairable.
    #[test]
    fn only_the_coin_m_book_reads_the_base_asset_from_the_other_column() {
        assert_eq!(
            routed("BTCUSD_PERP.P", None).expect("routes").2.volume_column,
            VolumeColumn::Index7
        );
        for usual in ["BTCUSDT", "BTCUSDT.P"] {
            assert_eq!(
                routed(usual, None).expect("routes").2.volume_column,
                VolumeColumn::Index5,
                "{usual} must keep the column every binance bar has always been parsed at"
            );
        }
    }

    /// A DATED COIN-M contract reached through the perpetual marker is REFUSED, not routed — the
    /// suffix says PERPETUAL and the venue says `CURRENT_QUARTER`. The message names the venue's
    /// own word, and says outright that no other spelling exists rather than inventing one.
    #[test]
    fn a_dated_coin_m_contract_is_refused_rather_than_routed() {
        let err = routed("BTCUSD_260925.P", None).expect_err("a dated future is not a perpetual");
        assert!(err.contains("CURRENT_QUARTER"), "the venue's own word must be quoted: {err}");
        assert!(err.contains("CryptoFuture"), "and the class that would name it: {err}");
        assert!(err.contains("catalog.rs"), "and what would have to change first: {err}");
        assert!(
            routed("BTCUSD_261225.P", None)
                .expect_err("both dated arms refuse")
                .contains("NEXT_QUARTER")
        );
    }

    /// ⚠ **An unreadable listing is an ERROR, never a fall-through to fapi.** The two must not
    /// reach the same code path: fapi SERVES the COIN-M tape, so falling through does not fail —
    /// it succeeds with the wrong volume column.
    #[test]
    fn an_unreadable_listing_refuses_instead_of_falling_through_to_fapi() {
        let err = route_target("BTCUSD_PERP.P", None, &unreadable)
            .expect_err("not proven USDⓈ-M must not read as proven USDⓈ-M");
        assert!(err.contains("connection refused"), "the venue's own words survive: {err}");
        // ...and the SAME unreadable listing refuses a perfectly ordinary USDⓈ-M symbol too, which
        // is the accepted residual `crate::instruments` declares: this route cannot tell the two
        // apart without the list, so it refuses both rather than guessing either.
        assert!(route_target("BTCUSDT.P", None, &unreadable).is_err());
    }

    /// ⚠ **A spot route opens no socket**, proven by a lookup that panics if it is called. This is
    /// what keeps every spot backfill in the workspace byte-identical to before the third book
    /// existed — the listing is consulted only when it can change the outcome.
    #[test]
    fn a_spot_route_never_consults_the_coin_m_listing() {
        let calls = Cell::new(0_usize);
        let counting = |wire: &str| {
            calls.set(calls.get() + 1);
            listing(wire)
        };
        for bare in ["BTCUSDT", "ETHUSDT", "BTCUSD_PERP"] {
            let (base, _, _) = route_target(bare, None, &counting).expect("a bare symbol is spot");
            assert_eq!(base, REST_KLINES, "{bare}: a bare symbol is the spot book");
        }
        let (base, _, _) =
            route_target("BTCUSDT", Some(vike_model::AssetClass::CryptoSpot), &counting)
                .expect("a spot claim is spot");
        assert_eq!(base, REST_KLINES);
        assert_eq!(calls.get(), 0, "a spot route must not reach for the venue's COIN-M listing");
        // ...and the futures route DOES consult it, so the counter above is not vacuous.
        let _ = route_target("BTCUSDT.P", None, &counting).expect("routes");
        assert_eq!(calls.get(), 1);
    }

    /// A bare `BTCUSD_PERP` still routes to SPOT, which answers `-1121 Invalid symbol` — an ERROR,
    /// never another book in silence. That is exactly what `vike_catalog::addressing_for`'s binance
    /// row means by `BareSymbol::Unambiguous`, and admitting a third book did not change it: dapi's
    /// `symbol` set is DISJOINT from spot's Trading set and from fapi's (MEASURED).
    #[test]
    fn a_bare_coin_m_symbol_is_still_the_spot_books_problem_not_another_books() {
        let (base, wire, _) = routed("BTCUSD_PERP", None).expect("a bare symbol routes to spot");
        assert_eq!((base, wire), (REST_KLINES, "BTCUSD_PERP"));
        assert!(
            !vike_catalog::addressing_for(VENUE).must_claim(),
            "binance's row stays Unambiguous — the venue REFUSES a futures string on spot rather \
             than answering from another book, which is what that column measures"
        );
    }

    /// The CLAIM seam: `Some(CryptoPerp)` reaches the futures plane with no suffix, and the listing
    /// still decides WHICH futures book — because 0061 refuses to split the variant, so the claim
    /// genuinely cannot answer that question.
    #[test]
    fn a_class_claim_reaches_the_futures_plane_and_the_listing_still_picks_the_book() {
        use vike_model::AssetClass;
        let (base, wire, _) = routed("BTCUSD_PERP", Some(AssetClass::CryptoPerp)).expect("routes");
        assert_eq!((base, wire), (REST_KLINES_DAPI, "BTCUSD_PERP"));
        let (base, _, _) = routed("BTCUSDT", Some(AssetClass::CryptoPerp)).expect("routes");
        assert_eq!(base, REST_KLINES_FAPI, "the same claim, the other book — the listing decided");
    }

    /// A claim this venue's data path cannot address is refused rather than coerced — and the
    /// COIN-M QUARTERLIES are exactly that case, because `CryptoFuture` is not in binance's row.
    #[test]
    fn a_class_the_row_does_not_name_is_refused() {
        use vike_model::AssetClass;
        let err = routed("BTCUSD_260925", Some(AssetClass::CryptoFuture)).expect_err("refused");
        assert!(err.contains("CryptoFuture"), "{err}");
        assert!(err.contains("BTCUSD_PERP"), "the message must name what IS reachable: {err}");
        assert!(routed("AAPL", Some(AssetClass::Equity)).is_err());
    }

    /// A suffix and a claim that disagree: neither is obeyed. Bybit's twin, verbatim in shape.
    #[test]
    fn a_claim_contradicting_the_suffix_is_refused() {
        use vike_model::AssetClass;
        let err = routed("BTCUSDT.P", Some(AssetClass::CryptoSpot)).expect_err("refused");
        assert!(err.contains("two claims that disagree"), "{err}");
        assert!(err.contains("Drop the suffix or drop the claim"), "{err}");
    }

    /// ⚠ **The claim [`kline_market`] rests on.** That key names a BUDGET, and it stays two-valued
    /// across three hosts only because binance's two FUTURES hosts publish ONE budget — MEASURED
    /// 2400/min and weight 5 per page on both. Give dapi its own budget and this fails, which is
    /// the signal to split the pace key (and to mint the `vike_model::venue_rate_limits::Market`
    /// variant this deliberately did not).
    #[test]
    fn the_coin_m_host_is_paced_on_the_measured_twin_of_fapis_budget() {
        assert_eq!(
            BINANCE_KLINE_COIN_M.rate_limit, BINANCE_KLINE_PERP.rate_limit,
            "the COIN-M spec takes fapi's budget BY CONSTRUCTION; if that stops being true, \
             `kline_market` must grow a third key before this route ships"
        );
        assert_eq!(kline_market("BTCUSD_PERP.P"), "perp");
        // ...and the DISCOVERY url is NOT shared, because a published budget belongs to a host.
        let (base, _, spec) = routed("BTCUSD_PERP.P", None).expect("routes");
        assert_eq!(
            klines::discovery_url(spec, base),
            Some("https://dapi.binance.com/dapi/v1/exchangeInfo")
        );
        // Crossed, the pairing degrades to the fallback pace rather than pacing against another
        // machine's counter — the same guard the two older specs already carry.
        assert_eq!(klines::discovery_url(&BINANCE_KLINE_COIN_M, REST_KLINES_FAPI), None);
        assert_eq!(klines::discovery_url(&BINANCE_KLINE_PERP, REST_KLINES_DAPI), None);
    }

    /// The three books resolve three DISTINCT hosts — so `book_target` is a real fan-out and not
    /// two arms with a third alias.
    #[test]
    fn the_three_books_are_three_hosts() {
        let hosts = [Book::Spot, Book::UsdM, Book::CoinM].map(|b| book_target(b).0);
        assert_eq!(hosts, [REST_KLINES, REST_KLINES_FAPI, REST_KLINES_DAPI]);
        let unique: std::collections::BTreeSet<&str> = hosts.into_iter().collect();
        assert_eq!(unique.len(), 3, "three books must not share a host");
    }
}
