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
/// collide with its spot twin's. Delegates to [`klines::fetch_klines_range`] with Binance's spec
/// for the routed book (`BINANCE_KLINE_SPOT`, `BINANCE_KLINE_PERP` or `BINANCE_KLINE_COIN_M`).
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

/// This venue's row in the kline registry (`KLINE_SOURCES`): the store-free fetch half of a
/// backfill. The ingest it feeds is `crates/vike-backfill/src/kline_source.rs`'s
/// `backfill_kline_source`.
pub struct BinanceKlines;

impl vike_data::source::KlineSource for BinanceKlines {
    fn venue(&self) -> &str {
        crate::VENUE
    }

    /// [`fetch_klines_range`] verbatim.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        fetch_klines_range(symbol, interval, start_ms, end_ms)
            .map_err(vike_data::source::SourceError::Fetch)
    }
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
/// `"spot"` otherwise: the key a pace seed must be matched on, because a spot sample must never
/// seed a perp fetch.
///
/// Exposed (rather than left to the caller's own suffix check) because it is the SAME routing
/// decision [`range_target`] makes, and the two must not be able to disagree: binance's hosts
/// publish different budgets (6000 vs 2400/min) and price `/klines` differently (weight 2 vs 5), so
/// a sample taken under the wrong market is a sample that paces the other host wrong.
/// ⚠ **Still TWO-valued with three books, and that is deliberate.** This key names a BUDGET, not a
/// host: the only thing a seed may not do is pace one host against another host's counter. (It was
/// also half of a persisted pace record's key until docs/decisions/0094 deleted the one-shot kline
/// programs that kept that record.) Binance's two FUTURES hosts publish ONE budget — MEASURED
/// 2026-09-16, `REQUEST_WEIGHT` `MINUTE` = 2400 on both and weight 5 per `limit=1000` page on
/// both — so a COIN-M fetch seeded under `"perp"` is seeding a pacer with a sample taken against
/// the identical ceiling and per-page cost.
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

// --- funding-rate source -------------------------------------------------------------------

/// Binance USDⓈ-M funding-rate history endpoint (keyless public GET).
const FUNDING_URL: &str = "https://fapi.binance.com/fapi/v1/fundingRate";
/// Binance COIN-M funding-rate history endpoint (keyless public GET) — [`funding_url_for`]'s other
/// rung.
///
/// ⚠ **MEASURED 2026-09-29**: `fapi`'s `/fundingRate` answers HTTP 200 with NO rows for a COIN-M
/// symbol (`BTCUSD_PERP`) — it does not refuse the request, it silently has nothing to answer with
/// — so before this constant existed `binance:BTCUSD_PERP.P:funding` wrote nothing and reported no
/// error either. This host answers the real COIN-M rows, in the SAME shape fapi's does (`symbol,
/// fundingTime, fundingRate, markPrice, rateType`), so [`parse_funding_rates`] reads either host's
/// body unchanged.
const COIN_M_FUNDING_URL: &str = "https://dapi.binance.com/dapi/v1/fundingRate";
/// Binance caps `fundingRate` at 1000 rows per response. Assumed equal on `dapi` — unmeasured
/// beyond the response SHAPE [`COIN_M_FUNDING_URL`]'s doc measures, but every other paged endpoint
/// this venue publishes shares one cap across its books, and a second unverified number is worse
/// than reusing the measured one.
const FUNDING_PAGE_CAP: usize = 1000;

/// Parse a `GET .../fundingRate` body — `{symbol, fundingTime (ms number), fundingRate
/// (decimal string), markPrice (string)}` rows, timestamp from `fundingTime`, no premium. Shared by
/// both funding hosts [`funding_url_for`] routes to.
pub fn parse_funding_rates(body: &str) -> Result<Vec<vike_data::source::FundingRatePoint>, String> {
    vike_data::source::parse_funding_rate_rows(body, "fundingTime")
}

/// Which funding-rate host a WIRE symbol (the store's `.P` suffix already stripped, as
/// [`BinanceFunding::fetch`] does before calling this) routes to — or the refusal for a symbol that
/// has no funding rate to ask for at all.
///
/// Decided on the symbol's own SHAPE, unlike [`route_target`]'s kline routing, which needs the
/// venue's own COIN-M listing to tell a perpetual from a dated contract: funding does not need that
/// listing, because binance's COIN-M naming already carries the answer in the wire symbol —
///
/// * a bare symbol (`BTCUSDT`) is USDⓈ-M, unchanged from before this routed;
/// * a symbol ending in the venue's own PERPETUAL marker, `_PERP` (`BTCUSD_PERP`) — binance's own
///   listing calls this `contractType: "PERPETUAL"`, the same fact [`route_target`]'s doc leans on
///   — is COIN-M, [`COIN_M_FUNDING_URL`];
/// * any other symbol carrying `_` (`BTCUSD_260925`, a DATED delivery contract) has no funding rate
///   to ask for, and is refused rather than routed to either host.
///
/// ⚠ This is a SHAPE rule, not a listing lookup, and it is exhaustive only because binance's COIN-M
/// naming is: every dated contract carries a `_YYMMDD` tail and every perpetual carries `_PERP`, so
/// there is no third shape a real wire symbol can take. A listing-verified route like klines' would
/// cost a socket this call does not need.
fn funding_url_for(wire: &str) -> Result<&'static str, String> {
    if wire.ends_with("_PERP") {
        Ok(COIN_M_FUNDING_URL)
    } else if wire.contains('_') {
        Err(format!(
            "binance: {wire:?} is a DATED COIN-M delivery contract — a dated delivery contract has \
             no funding rate to ask for. The COIN-M PERPETUALS (`<PAIR>_PERP`, e.g. `BTCUSD_PERP`) \
             do, through the store's `{}` suffix.",
            vike_catalog::PERP_SUFFIX
        ))
    } else {
        Ok(FUNDING_URL)
    }
}

/// This venue's market funding-rate source (USDⓈ-M perps; 8h cadence), a row of the datahub's
/// `FUNDING_SOURCES`. Keyless public GET, paged by `vike_data::source::page_funding_forward`. Each
/// page's request is bounded by `vike_bridge_core::http::blocking_agent`'s whole-request timeout
/// (30s) — sized for a small paged JSON response, unlike vike-backfill's own unbounded-body agent.
pub struct BinanceFunding;

impl vike_data::source::FundingRateSource for BinanceFunding {
    fn venue(&self) -> &str {
        crate::VENUE
    }

    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<vike_data::source::FundingRatePoint>, vike_data::source::SourceError> {
        use vike_data::source::SourceError;
        // ⚠ The STORE spells a Binance perpetual with `vike_catalog::PERP_SUFFIX` (`BTCUSDT.P` — the
        // kline lane files its perp bars under that key, and a bare `BTCUSDT` is SPOT), while the
        // funding endpoint knows only the wire symbol. So the suffix is stripped for the request
        // and the caller's spelling stays the store key — the kline path's own rule
        // ([`fetch_klines_range`]). A symbol WITHOUT the suffix is REFUSED, before any request:
        // spot has no funding, and filing a perp's funding under the spot spelling would put it
        // where no perp backtest looks (`vike-backtest`'s funding join reads the price series' own
        // symbol).
        let (wire, perp) = vike_catalog::split_perp_at(crate::VENUE, symbol);
        if !perp {
            return Err(SourceError::Refused(format!(
                "binance: {symbol:?} names SPOT, and funding is a perpetual's series — ask for \
                 `{symbol}{}`, the store's spelling of the perpetual (USDⓈ-M or COIN-M)",
                vike_catalog::PERP_SUFFIX
            )));
        }
        // ⚠ USDⓈ-M and COIN-M perpetuals share this ONE `.P` suffix, and until 2026-09-29 only
        // `fapi` ever answered this request — silently: a COIN-M symbol got HTTP 200 with no rows
        // rather than a refusal, so `binance:BTCUSD_PERP.P:funding` wrote nothing and said nothing
        // was wrong. See [`funding_url_for`] for the routing and [`COIN_M_FUNDING_URL`] for the
        // measurement behind it.
        let url_base = funding_url_for(wire).map_err(SourceError::Refused)?;
        let agent = vike_bridge_core::http::blocking_agent();
        vike_data::source::page_funding_forward(start_ms, end_ms, FUNDING_PAGE_CAP, |cursor| {
            let url = format!(
                "{url_base}?symbol={wire}&startTime={cursor}&endTime={end_ms}\
                 &limit={FUNDING_PAGE_CAP}"
            );
            let raw = vike_bridge_core::http::get_raw(&agent, &url, "binance-funding", &[])
                .map_err(|e| SourceError::Fetch(format!("{e} ({url})")))?;
            if !(200..300).contains(&raw.status) {
                return Err(SourceError::Fetch(format!(
                    "binance-funding HTTP {} from {url}: {}",
                    raw.status,
                    vike_bridge_core::http::body_head(&raw.body)
                )));
            }
            parse_funding_rates(&raw.body).map_err(SourceError::Fetch)
        })
    }
}

#[path = "data_tests.rs"]
#[cfg(test)]
mod data_tests;
