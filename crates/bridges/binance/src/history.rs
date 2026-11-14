//! Binance Spot/perp REST kline history — the venue face of the shared
//! [`crate::family::history::klines`] rung (F, dedup rung 6).
//!
//! Both the live market feed's REST warmup seed (`fetch_klines_latest`, "the newest N klines, last
//! one still forming") and the vike-backfill historical backfill (`fetch_klines_range`, a paged
//! `[start, end]` window) delegate to the shared family kline core; this module supplies only
//! Binance's OWN host resolution and the WIRING of its rate-limit budget — the budget's NUMBERS
//! live in `vike_venues::venue_rate_limits`'s `BINANCE_SPOT`/`BINANCE_PERP` rows, because a venue's
//! published ceiling is a fact about the venue rather than a constant of this crate — and keeps the
//! public surface (`parse_klines`/`fetch_klines_latest`/`fetch_klines_range`) unchanged for
//! vike-backfill and the feed. See [`crate::family::history::klines`] for the endpoint/response/rate-limit
//! contract.
//!
//! Endpoint: `GET https://api.binance.com/api/v3/klines?symbol=&interval=&limit=&[startTime=]&[endTime=]`
//! (spot), `GET https://fapi.binance.com/fapi/v1/klines?…` (USDⓈ-M), or
//! `GET https://dapi.binance.com/dapi/v1/klines?…` (COIN-M). The HOSTS are binance's `venue_url` rows
//! at `live` `history` (products `spot`, `usdm`, `coinm`), held by [`KlineSourceBinance`] from
//! [`KlineSourceBinance::from_urls`]; the paths stay here. The release's rows dial exactly the URLs above.
//!
//! ## ⚠ THREE books, and the `.P` boolean can only name two
//!
//! `docs/decisions/0061-an-instrument-names-its-kind.md` phase 4 applied to binance. The `.P`
//! suffix answers *spot or futures*; it cannot answer *which futures book*, because binance runs
//! two — USDⓈ-M on `fapi` and COIN-M (coin-margined, inverse) on `dapi` — and 0061 REFUSES to split
//! [`vike_model::AssetClass::CryptoPerp`] into linear and inverse, so a caller's class claim
//! cannot answer it either. **The third input is the VENUE'S OWN LISTING**
//! ([`crate::instrument::listing`]), which is the one answer that is neither a symbol-shape guess nor an
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
//!   what makes the bars reachable — [`crate::family::history::klines::VolumeColumn`] is. Index 5 on a
//!   COIN-M row is a CONTRACT COUNT, not the base asset, and routing on the venue's leniency would
//!   have fetched correct-looking bars with the wrong unit in `Bar::volume`.
//! * **The picker still cannot offer one.** `crates/bridges/binance/src/catalog.rs` fetches spot
//!   and fapi only, so no COIN-M instrument exists in the catalog by any path, and this module's
//!   reach is the CLI and hand-typed callers. That is stated rather than fixed — see this crate's
//!   `CLAUDE.md`.

use std::borrow::Cow;
use std::time::Duration;

use vike_marketdata::Bar;
use vike_model::rate_limits::PaceSample;
use vike_venues::venue_rate_limits::{BINANCE_PERP, BINANCE_SPOT};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::VENUE;
use crate::family::history::klines::{self, KlineRateLimit, KlineSpec, VolumeColumn};

/// Re-exported pure map (venue-agnostic) — kept at `vike_binance::history::parse_klines` for
/// vike-backfill's backfill + its fixture test. See [`klines::parse_klines`].
pub use crate::family::history::klines::parse_klines;

/// The SPOT kline path, on the `spot` row's host.
const PATH_KLINES_SPOT: &str = "/api/v3/klines";
/// USDS-M futures (fapi) twin of [`PATH_KLINES_SPOT`] — same response shape, just a different host/path.
/// Reached by the live feed's perp seed (`fetch_klines_latest(.., perp: true)`) AND by the paged
/// range backfill, which derives the same flag from the caller's [`vike_venues::PERP_SUFFIX`].
const PATH_KLINES_FAPI: &str = "/fapi/v1/klines";

/// Where SPOT publishes its own `REQUEST_WEIGHT` budget — on the same host [`PATH_KLINES_SPOT`] pages, which
/// is the only host whose budget applies to it. Read once per backfill by
/// [`klines::fetch_klines_range`]; see that module's "Discovered pacing".
const PATH_INFO_SPOT: &str = "/api/v3/exchangeInfo";
/// The fapi twin of [`PATH_INFO_SPOT`], paired with [`PATH_KLINES_FAPI`]. A SEPARATE path for
/// the same reason the two specs are separate: fapi's published budget is 2400/min, spot's is 6000.
const PATH_INFO_FAPI: &str = "/fapi/v1/exchangeInfo";

/// COIN-M (coin-margined / inverse) futures — the THIRD host, and the one this bridge had no
/// spelling for at all until now. A symbol here is binance's own (`BTCUSD_PERP`), and WHICH symbols
/// those are is read off the venue rather than guessed: [`crate::instrument::listing`] is the third routing
/// input, and its module doc is the authority for why a symbol-shape test and a caller's asset-class
/// claim are both refused as answers.
///
/// ⚠ **This host is not needed to FETCH a COIN-M bar, and it is still the right host.** MEASURED
/// 2026-09-16: `fapi/v1/klines?symbol=BTCUSD_PERP` answers BYTE-IDENTICALLY to
/// `dapi/v1/klines?symbol=BTCUSD_PERP`, and dapi answers for `BTCUSDT` too — binance's two futures
/// hosts route each other's symbols. Relying on that would be relying on the venue's LENIENCY, the
/// exact shape `crates/bridges/bybit/src/history.rs`'s `ambiguous_bare_symbol_refusal` refuses to
/// document as a cure ("a property of the venue's API, not a claim anybody made"). Routing on the
/// listing instead means the request says what it means, and it is what makes
/// [`BINANCE_KLINE_COIN_M`]'s volume column reachable — which fapi's leniency does NOT give you.
/// Its budget's twin of [`PATH_INFO_SPOT`] is `crate::instrument::listing::PATH_DAPI_EXCHANGE_INFO`: published
/// `REQUEST_WEIGHT` `MINUTE` 2400 — fapi's number, MEASURED — but a SEPARATE address because a budget
/// belongs to the HOST that published it; see [`BINANCE_KLINE_COIN_M`].
const PATH_KLINES_DAPI: &str = "/dapi/v1/klines";

/// Binance's SPOT paged-backfill budget. **MEASURED 2026-08-04** from the CI box, not assumed:
/// `x-mbx-used-weight` reports **2** for `/api/v3/klines?limit=1000`, against the
/// `REQUEST_WEIGHT` `MINUTE` limit of **6000** in live `exchangeInfo`. That is 3000 req/min of
/// headroom; `page_delay` of 50ms yields ~1200 req/min ⇒ ~2400 weight/min, ~40% of budget.
///
/// Those numbers are now the FALLBACK, not the plan: with [`PATH_INFO_SPOT`] wired, the pager
/// reads the 6000 off the venue each run and derives the gap from the observed per-request weight,
/// so `page_delay` applies only when discovery does not answer. It is kept — and kept accurate —
/// precisely because that is a mode the pager must still be correct in.
///
/// **The two rate numbers below are no longer this crate's to choose.** `page_delay` and
/// `weight_soft_limit` come from [`vike_venues::venue_rate_limits::BINANCE_SPOT`], the workspace's
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
    // The host is a row, so the discovery URL is filled per host by `KlineSourceBinance::target` (the
    // `Cow::Owned` of the SAME URL the const once borrowed); the template carries none.
    exchange_info_url: None,
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
/// [`vike_venues::venue_rate_limits::BINANCE_PERP`] (which carries the 2400 ceiling and asserts the
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
    exchange_info_url: None,
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
/// `vike_venues::venue_rate_limits` row and why [`kline_market`] stays two-valued —
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
    exchange_info_url: None,
    utilization: vike_model::rate_limits::DEFAULT_UTILIZATION,
    // ⚠ THE ONE FIELD THAT DIFFERS, and the reason this spec exists as more than a host swap. A
    // COIN-M row's index 5 is a CONTRACT COUNT and its index 7 is the base asset — the opposite of
    // every other binance book. `VolumeColumn`'s own doc carries the measurement and the arithmetic.
    volume_column: VolumeColumn::Index7,
};

// The two published `REQUEST_WEIGHT` `MINUTE` ceilings (SPOT 6000, fapi 2400 — MEASURED 2026-08-04
// from live `exchangeInfo` on both hosts) and the COMPILE-TIME invariant that keeps each spec's
// `weight_soft_limit` under its own host's ceiling now live in
// `vike_venues::venue_rate_limits::{BINANCE_SPOT, BINANCE_PERP}`, beside every other venue's. They
// are FACTS ABOUT THE VENUE, not tuning values — not ours to raise — which is why they belong in the
// per-venue capability table rather than in the one consumer that happened to read them.
//
// The invariant itself is unchanged in strength: a soft limit at or above the hard ceiling can never
// fire, so the guard silently becomes dead code and the venue's 418 -> IP ban is what stops you
// instead. Both hosts shared ONE spec at 5000 (the SPOT budget) until 2026-08-04 and nothing caught
// it, because nothing read the number. The build still refuses — one `const _: ()` per table row.

/// **The three books binance's kline REST serves.** The type that exists because a `perp` `bool` is
/// spent: two hosts chosen by one boolean cannot express three,
/// and widening the boolean into a second boolean would encode the third book as "perp and also
/// something", which is the implicit encoding
/// `docs/decisions/0061-an-instrument-names-its-kind.md` exists to remove. `perp` (the seed's word)
/// still means USDⓈ-M, exactly as it always did ([`Product::of_perp`]).
///
/// ⚠ These are BOOKS, not asset classes. `Product::UsdM` and `Product::CoinM` are both
/// [`vike_model::AssetClass::CryptoPerp`] for a perpetual — 0061 REFUSES to split that variant
/// ("the vocabulary names the PRODUCT, never the venue's word for the route"), so this enum is
/// deliberately local to the one file that routes, and nothing in `vike-catalog` learns the word
/// "coin-margined".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Product {
    /// `api.binance.com` — the cash book.
    Spot,
    /// `fapi.binance.com` — USDⓈ-M (linear, stablecoin-margined) futures.
    UsdM,
    /// `dapi.binance.com` — COIN-M (inverse, coin-margined) futures.
    CoinM,
}

impl Product {
    /// The seed's two-valued word: `perp` is USDⓈ-M, else spot (the third book is the listing's).
    fn of_perp(perp: bool) -> Product {
        if perp { Product::UsdM } else { Product::Spot }
    }
}

/// The ONE place a book becomes a path and a budget. Path, discovery path and budget are one
/// decision, never three: pairing a host with another host's `REQUEST_WEIGHT` is the defect the
/// two-spec split already documents, and a third host is a third chance to make it. The host is
/// [`KlineSourceBinance`]'s ([`KlineSourceBinance::target`] joins them).
fn book_target(book: Product) -> (&'static str, &'static str, &'static KlineSpec) {
    match book {
        Product::Spot => (PATH_KLINES_SPOT, PATH_INFO_SPOT, &BINANCE_KLINE_SPOT),
        Product::UsdM => (PATH_KLINES_FAPI, PATH_INFO_FAPI, &BINANCE_KLINE_PERP),
        Product::CoinM => (
            PATH_KLINES_DAPI,
            crate::instrument::listing::PATH_DAPI_EXCHANGE_INFO,
            &BINANCE_KLINE_COIN_M,
        ),
    }
}

/// The live feed's warmup seed: the newest `limit` klines (no time bound), the last of which is the
/// still-forming bar. `perp = true` routes to the USDS-M futures host ([`PATH_KLINES_FAPI`]) instead
/// of spot ([`PATH_KLINES_SPOT`]), and a contract the venue's COIN-M listing names to the COIN-M host,
/// read with that book's volume column ([`BINANCE_KLINE_COIN_M`]): the stream this seed warms up is
/// `dstream`'s (`crate::feed::BinanceTables::urls_for`), and its bars carry the base asset too. The hosts are
/// `src`'s ([`KlineSourceBinance`]). Delegates to [`klines::fetch_klines_latest`].
///
/// ⚠ fapi answers a COIN-M symbol too, byte-identically, with a contract count in the column the rung
/// reads, so a COIN-M seed that fell through to the USDⓈ-M arm would warm the chart with bars ~760
/// times too heavy. A listing that cannot be read is the seed's `Err`, rendered as the lane's status
/// (`feed_main` keeps streaming), as before.
pub fn fetch_klines_latest(
    src: &KlineSourceBinance,
    symbol: &str,
    interval: &str,
    limit: usize,
    perp: bool,
) -> Result<Vec<Bar>, String> {
    if perp && src.coin_m(symbol)?.is_some() {
        return fetch_coin_m_klines_latest(src, symbol, interval, limit);
    }
    let (base, _spec) = src.target(Product::of_perp(perp));
    klines::fetch_klines_latest(&base, symbol, interval, limit, VENUE)
}

/// The COIN-M seed: [`klines::fetch_klines_latest`]'s one request, on the COIN-M host and parsed with
/// that book's volume column (the rung's own parse reads index 5, a CONTRACT count there).
fn fetch_coin_m_klines_latest(
    src: &KlineSourceBinance,
    symbol: &str,
    interval: &str,
    limit: usize,
) -> Result<Vec<Bar>, String> {
    let (base, spec) = src.target(Product::CoinM);
    let agent = vike_bridge_core::http::blocking_agent();
    let url = klines::klines_url(&base, symbol, interval, None, None, limit);
    let raw = vike_bridge_core::http::get_raw(&agent, &url, "binance klines", &[])?;
    if (200..300).contains(&raw.status) {
        klines::parse_klines_with(&raw.body, spec.volume_column)
    } else {
        let head = vike_bridge_core::http::body_head(&raw.body);
        Err(format!("binance klines HTTP {}: {head}", raw.status))
    }
}

/// Fetch closed-kline history for the inclusive `[start_ms, end_ms]` window, paging through Binance's
/// 1000-rows/response cap under the family rate-limit policy. A `symbol` carrying
/// [`vike_venues::PERP_SUFFIX`] routes to the USDⓈ-M futures host and is stripped before it reaches
/// the wire; the CALLER's suffixed symbol remains the store/series key, so a perp's series can never
/// collide with its spot twin's. Delegates to [`klines::fetch_klines_range`] with Binance's spec
/// for the routed book (`BINANCE_KLINE_SPOT`, `BINANCE_KLINE_PERP` or `BINANCE_KLINE_COIN_M`).
/// ⚠ **This is the NO-CLAIM entry point, and it is now the one that can REFUSE.** A `.P` symbol
/// binance lists as a DATED COIN-M contract comes back as an `Err` rather than a tape, and so does
/// a route whose COIN-M listing could not be read — see [`route_target`] and
/// [`crate::instrument::listing`]. Callers that KNOW the class bind [`fetch_klines_range_classed`].
pub fn fetch_klines_range(
    src: &KlineSourceBinance,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    fetch_klines_range_classed(src, symbol, interval, start_ms, end_ms, None)
}

/// This venue's row in the kline registry (`KLINE_SOURCES`): the store-free fetch half of a
/// backfill. The ingest it feeds is `crates/vike-backfill/src/kline_source.rs`'s
/// `backfill_kline_source`.
///
/// It holds the three books' REST hosts, binance's `venue_url` rows at `live` `history` (products
/// `spot`, `usdm`, `coinm`), looked up ONCE by [`KlineSourceBinance::from_urls`]: the hosts the kline pages, the
/// budget discovery and the COIN-M listing ([`crate::instrument::listing`]) are read on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KlineSourceBinance {
    spot: String,
    usdm: String,
    coinm: String,
}

impl KlineSourceBinance {
    /// The source over `urls`, binance's `venue_url` rows.
    ///
    /// # Errors
    /// A book's row is missing: binance's history is NOT RUNNING, the refusal names the row.
    pub fn from_urls(urls: &VenueUrls) -> Result<KlineSourceBinance, UrlRefusal> {
        let at = |product| {
            urls.url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, product)
                .map(str::to_string)
        };
        Ok(KlineSourceBinance { spot: at("spot")?, usdm: at("usdm")?, coinm: at("coinm")? })
    }

    /// **The ONE place a book becomes a URL and a budget**: the book's host joined to its kline path, and
    /// its spec with the discovery URL on that SAME host ([`book_target`]; [`klines::discovery_url`]
    /// refuses a budget from another host).
    fn target(&self, book: Product) -> (String, KlineSpec) {
        let host = match book {
            Product::Spot => &self.spot,
            Product::UsdM => &self.usdm,
            Product::CoinM => &self.coinm,
        };
        let (path, info, spec) = book_target(book);
        let spec = KlineSpec {
            exchange_info_url: Some(Cow::Owned(format!("{host}{info}"))),
            ..spec.clone()
        };
        (format!("{host}{path}"), spec)
    }

    /// What binance's COIN-M listing, read on this source's `coinm` host, says `wire` is
    /// ([`crate::instrument::listing::coin_m_contract`]).
    fn coin_m(
        &self,
        wire: &str,
    ) -> Result<Option<crate::instrument::listing::CoinMContract>, String> {
        crate::instrument::listing::coin_m_contract(&self.coinm, wire)
    }
}

impl vike_data::source::KlineSource for KlineSourceBinance {
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
        fetch_klines_range(self, symbol, interval, start_ms, end_ms)
            .map_err(vike_data::source::SourceError::Fetch)
    }
}

/// [`fetch_klines_range`] with the caller's asset-class CLAIM — the seam
/// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 3 specifies, spelled as an
/// `Option<AssetClass>` because most callers genuinely have no class to give. The binance twin of
/// `vike_bybit::history::fetch_klines_range_classed`.
///
/// ⚠ **A claim does NOT reach a different book here than the suffix does**, and that is the one
/// place this differs from bybit's. `CryptoPerp` names both of binance's futures books (0061
/// refuses to split the variant), so the claim can only say spot-or-futures; which futures book is
/// always the venue's listing's answer. What the claim buys is a caller who holds a class not
/// having to synthesise a `.P` string to express it.
pub fn fetch_klines_range_classed(
    src: &KlineSourceBinance,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    class: Option<vike_model::AssetClass>,
) -> Result<Vec<Bar>, String> {
    let (base, wire, spec) = range_target(src, symbol, class)?;
    klines::fetch_klines_range(&base, wire, interval, start_ms, end_ms, &spec)
}

/// [`klines::fetch_klines_range_paced`] with BOUNDED CONCURRENCY — several disjoint sub-windows in
/// flight instead of one, at the SAME aggregate rate. See [`klines::lanes::fetch_klines_range_lanes`] for
/// the full contract; the two facts a caller here needs:
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
/// wants the strictly sequential path calls [`fetch_klines_range`], which is unchanged.
pub fn fetch_klines_range_lanes(
    src: &KlineSourceBinance,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let (base, wire, spec) = range_target(src, symbol, None)?;
    klines::lanes::fetch_klines_range_lanes(
        &base,
        wire,
        interval,
        start_ms,
        end_ms,
        &spec,
        seed,
        vike_bridge_core::concurrent::MAX_LANES,
    )
}

/// Which MARKET a caller's symbol routes to — `"perp"` for a [`vike_venues::PERP_SUFFIX`] symbol,
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
/// which is also why no third `vike_venues::venue_rate_limits::VenueRateLimitsMarket` variant was minted for a
/// number that is the same number.
pub fn kline_market(symbol: &str) -> &'static str {
    if vike_catalog::split_perp_at(VENUE, symbol).1 { "perp" } else { "spot" }
}

/// The `(base, wire_symbol, rate_limit_spec)` triple a range fetch routes to — extracted out of
/// [`fetch_klines_range`] so the routing DECISION is assertable without network I/O.
///
/// Testing the seed's host choice directly proves nothing about this: that path has understood
/// `perp` since the live warmup seed was written, and the bug was that `fetch_klines_range` never
/// asked it. This function is what the fetcher actually calls, so a test against it fails if the
/// fetcher is ever re-pinned to spot — and if the fetcher stops calling it, `dead_code` trips the
/// `-D warnings` merge gate.
///
/// The spec rides along because host and budget are ONE decision, not two: fapi's 2400-weight
/// ceiling belongs to the fapi host, and pairing a host with the other host's budget is exactly
/// the defect the two specs above document.
///
/// The impure half of [`route_target`]: it supplies the COIN-M listing lookup (on `src`'s `coinm`
/// host), and does so **only when the answer can change the outcome** — see [`route_target`]'s
/// `coin_m` parameter.
fn range_target<'a>(
    src: &KlineSourceBinance,
    symbol: &'a str,
    claimed: Option<vike_model::AssetClass>,
) -> Result<(String, &'a str, KlineSpec), String> {
    let (book, wire) = route_target(symbol, claimed, &|wire| src.coin_m(wire))?;
    let (base, spec) = src.target(book);
    Ok((base, wire, spec))
}

/// **The whole routing decision, PURE** — `(book, wire_symbol)` or a refusal, given the
/// caller's symbol, the class the caller CLAIMED (if any), and a LOOKUP of what binance's own
/// COIN-M listing says about the wire symbol. The book's host and budget are [`KlineSourceBinance::target`]'s.
///
/// `docs/decisions/0061-an-instrument-names-its-kind.md` verdict 3 is the shape: the seam takes
/// `Option<AssetClass>`, and a claim that cannot be honoured is a REFUSAL rather than a coercion.
/// The binance twin of `crates/bridges/bybit/src/history.rs`'s function of the same name, with one
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
    coin_m: &dyn Fn(&str) -> Result<Option<crate::instrument::listing::CoinMContract>, String>,
) -> Result<(Product, &'a str), String> {
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
        return Ok((Product::Spot, wire));
    }
    let book = match coin_m(wire)? {
        None => Product::UsdM,
        Some(crate::instrument::listing::CoinMContract::Perpetual) => Product::CoinM,
        Some(dated @ crate::instrument::listing::CoinMContract::Delivery(_)) => {
            return Err(dated_contract_refusal(wire, dated));
        }
    };
    Ok((book, wire))
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
        vike_venues::PERP_SUFFIX,
        vike_venues::PERP_SUFFIX
    )
}

/// The refusal for a symbol whose suffix and whose caller disagree. Copied in shape from bybit's
/// twin, down to the "drop one of them" instruction — neither claim is obeyed, because obeying
/// either would be picking a winner between two things the caller said.
fn contradicting_claim_refusal(symbol: &str, class: vike_model::AssetClass) -> String {
    format!(
        "binance: {symbol:?} carries the {:?} perpetual marker but the caller claimed {class:?} — \
         two claims that disagree, so neither is obeyed. Drop the suffix or drop the claim.",
        vike_venues::PERP_SUFFIX
    )
}

/// The refusal a DATED COIN-M contract gets when it is reached through the perpetual marker.
///
/// ⚠ Worth refusing rather than routing, even though the host and the volume column would both be
/// right: the [`vike_venues::PERP_SUFFIX`] is the only thing that carried it here, that suffix
/// SAYS perpetual, and the venue's own listing says this contract is not one. Routing it would make
/// `.P` mean "perpetual, or a dated future, whichever the venue happens to have" — a suffix that
/// already means one thing being made to mean two, which is exactly what
/// `docs/decisions/0061-an-instrument-names-its-kind.md` forbids. There is no second spelling to
/// recommend, so this message says so outright instead of inventing one.
fn dated_contract_refusal(wire: &str, dated: crate::instrument::listing::CoinMContract) -> String {
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
        vike_venues::PERP_SUFFIX
    )
}

// --- funding-rate source -------------------------------------------------------------------

/// Binance USDⓈ-M funding-rate history path (keyless public GET), on the `usdm` row's host.
const PATH_FUNDING_FAPI: &str = "/fapi/v1/fundingRate";
/// Binance COIN-M funding-rate history path (keyless public GET), on the `coinm` row's host —
/// [`FundingRateSourceBinance::funding_url_for`]'s other rung.
///
/// ⚠ **MEASURED 2026-09-29**: `fapi`'s `/fundingRate` answers HTTP 200 with NO rows for a COIN-M
/// symbol (`BTCUSD_PERP`) — it does not refuse the request, it silently has nothing to answer with
/// — so before this rung existed `binance:BTCUSD_PERP.P:funding` wrote nothing and reported no
/// error either. This host answers the real COIN-M rows, in the SAME shape fapi's does (`symbol,
/// fundingTime, fundingRate, markPrice, rateType`), so [`parse_funding_rates`] reads either host's
/// body unchanged.
const PATH_FUNDING_DAPI: &str = "/dapi/v1/fundingRate";
/// Binance caps `fundingRate` at 1000 rows per response. Assumed equal on `dapi` — unmeasured
/// beyond the response SHAPE [`PATH_FUNDING_DAPI`]'s doc measures, but every other paged endpoint
/// this venue publishes shares one cap across its books, and a second unverified number is worse
/// than reusing the measured one.
const FUNDING_PAGE_CAP: usize = 1000;

/// Parse a `GET .../fundingRate` body — `{symbol, fundingTime (ms number), fundingRate
/// (decimal string), markPrice (string)}` rows, timestamp from `fundingTime`, no premium. Shared by
/// both funding hosts [`FundingRateSourceBinance::funding_url_for`] routes to.
pub fn parse_funding_rates(body: &str) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    vike_data::source::parse_funding_rate_rows(body, "fundingTime")
}

/// **The wire symbol of a family venue's funding ask** (binance's and aster's `FundingRateSource::fetch`,
/// one body since the owner's ruling of 2026-11-13): the STORE spells a perpetual with
/// `vike_venues::PERP_SUFFIX` (`BTCUSDT.P` — the kline lane files its perp bars under that key, and a bare
/// `BTCUSDT` is SPOT), while the funding endpoint knows only the wire symbol, so the suffix is stripped for
/// the request and the caller's spelling stays the store key. A symbol WITHOUT the suffix is REFUSED, before
/// any request, in `venue`'s words with `books` after them (binance's two: ` (USDⓈ-M or COIN-M)`; aster's
/// one: nothing): spot has no funding, and filing a perp's funding under the spot spelling would put it
/// where no perp backtest looks (`vike-backtest`'s funding join reads the price series' own symbol).
pub fn funding_wire<'a>(
    venue: &str,
    symbol: &'a str,
    books: &str,
) -> Result<&'a str, vike_data::source::SourceError> {
    let (wire, perp) = vike_catalog::split_perp_at(venue, symbol);
    if !perp {
        return Err(vike_data::source::SourceError::Refused(format!(
            "{venue}: {symbol:?} names SPOT, and funding is a perpetual's series — ask for \
             `{symbol}{}`, the store's spelling of the perpetual{books}",
            vike_venues::PERP_SUFFIX
        )));
    }
    Ok(wire)
}

/// **The family's paged `fundingRate` walk** (binance's two books and aster's fapi; owner ruling F-8): the
/// forward walk (`vike_data::source::page_walk_forward`, `page_cap` rows a page) over the page `url_of(cursor)`
/// names, with the family kline pager's rate-limit rule. A 429 or 418, and a 5xx (owner ruling B-6), waits
/// (`Retry-After`, else `RetryBackoffPolicy::HISTORY_PAGE`: 1 s doubling to 60 s, six retries) and asks the SAME page again, and
/// `page_delay` (the venue's history row) is slept between pages, never before the first. Any other failure
/// fails the walk in `label`'s words, as before: one 429 used to fail the whole funding backfill. A full page
/// that does not move the cursor fails it as `stuck` says (the shared walk's rule).
pub fn funding_walk_forward(
    start_ms: i64,
    end_ms: i64,
    page_cap: usize,
    page_delay: Duration,
    label: &str,
    stuck: impl FnOnce(vike_data::source::PageWalkStuck) -> vike_data::source::SourceError,
    url_of: impl Fn(i64) -> String,
) -> Result<Vec<vike_marketdata::FundingRatePoint>, vike_data::source::SourceError> {
    use vike_bridge_core::retry::{RetryBackoffPolicy, RetryVerdict, retry_rate_limited};
    let agent = vike_bridge_core::http::blocking_agent();
    let mut first = true;
    vike_data::source::page_walk_forward(start_ms, end_ms, page_cap, stuck, |cursor| {
        if !std::mem::take(&mut first) {
            std::thread::sleep(page_delay);
        }
        let url = url_of(cursor);
        retry_rate_limited(RetryBackoffPolicy::HISTORY_PAGE, label, || {
            let raw = vike_bridge_core::http::get_raw(&agent, &url, label, &[])
                .map_err(|e| format!("{e} ({url})"))?;
            match raw.status {
                200..=299 => parse_funding_rates(&raw.body).map(RetryVerdict::Done),
                429 | 418 => Ok(RetryVerdict::RateLimited {
                    retry_after: raw.retry_after,
                    note: format!("HTTP {} (rate limited)", raw.status),
                }),
                // owner ruling B-6: a 5xx is waited out like a rate limit
                500..=599 => Ok(RetryVerdict::Unavailable {
                    retry_after: raw.retry_after,
                    status: raw.status,
                }),
                other => Err(format!(
                    "{label} HTTP {other} from {url}: {}",
                    vike_bridge_core::http::body_head(&raw.body)
                )),
            }
        })
        .map_err(vike_data::source::SourceError::Fetch)
    })
}

/// This venue's market funding-rate source (USDⓈ-M perps; 8h cadence), a row of the datahub's
/// `FUNDING_SOURCES`. Keyless public GET, paged by `vike_data::source::page_walk_forward`. Each
/// page's request is bounded by `vike_bridge_core::http::blocking_agent`'s whole-request timeout
/// (30s) — sized for a small paged JSON response, unlike vike-backfill's own unbounded-body agent.
///
/// It holds the two futures books' hosts, binance's `venue_url` rows at `live` `data` (the public RATE;
/// products `usdm` and `coinm`), looked up ONCE by [`FundingRateSourceBinance::from_urls`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundingRateSourceBinance {
    usdm: String,
    coinm: String,
}

impl FundingRateSourceBinance {
    /// The source over `urls`, binance's `venue_url` rows.
    ///
    /// # Errors
    /// A book's row is missing: binance's funding history is NOT RUNNING, the refusal names the row.
    pub fn from_urls(urls: &VenueUrls) -> Result<FundingRateSourceBinance, UrlRefusal> {
        let at = |product| {
            urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, product)
                .map(str::to_string)
        };
        Ok(FundingRateSourceBinance { usdm: at("usdm")?, coinm: at("coinm")? })
    }

    /// Which funding-rate URL a WIRE symbol (the store's `.P` suffix already stripped, as
    /// [`FundingRateSourceBinance::fetch`] does before calling this) routes to — or the refusal for a symbol that
    /// has no funding rate to ask for at all.
    ///
    /// Decided on the symbol's own SHAPE, unlike [`route_target`]'s kline routing, which needs the
    /// venue's own COIN-M listing to tell a perpetual from a dated contract: funding does not need that
    /// listing, because binance's COIN-M naming already carries the answer in the wire symbol —
    ///
    /// * a bare symbol (`BTCUSDT`) is USDⓈ-M, unchanged from before this routed;
    /// * a symbol ending in the venue's own PERPETUAL marker, `_PERP` (`BTCUSD_PERP`) — binance's own
    ///   listing calls this `contractType: "PERPETUAL"`, the same fact [`route_target`]'s doc leans on
    ///   — is COIN-M, [`PATH_FUNDING_DAPI`];
    /// * any other symbol carrying `_` (`BTCUSD_260925`, a DATED delivery contract) has no funding rate
    ///   to ask for, and is refused rather than routed to either host.
    ///
    /// ⚠ This is a SHAPE rule, not a listing lookup, and it is exhaustive only because binance's COIN-M
    /// naming is: every dated contract carries a `_YYMMDD` tail and every perpetual carries `_PERP`, so
    /// there is no third shape a real wire symbol can take. A listing-verified route like klines' would
    /// cost a socket this call does not need.
    fn funding_url_for(&self, wire: &str) -> Result<String, String> {
        if wire.ends_with("_PERP") {
            Ok(format!("{}{PATH_FUNDING_DAPI}", self.coinm))
        } else if wire.contains('_') {
            Err(format!(
                "binance: {wire:?} is a DATED COIN-M delivery contract — a dated delivery contract has \
                 no funding rate to ask for. The COIN-M PERPETUALS (`<PAIR>_PERP`, e.g. `BTCUSD_PERP`) \
                 do, through the store's `{}` suffix.",
                vike_venues::PERP_SUFFIX
            ))
        } else {
            Ok(format!("{}{PATH_FUNDING_FAPI}", self.usdm))
        }
    }
}

impl vike_data::source::FundingRateSource for FundingRateSourceBinance {
    fn venue(&self) -> &str {
        crate::VENUE
    }

    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<vike_marketdata::FundingRatePoint>, vike_data::source::SourceError> {
        use vike_data::source::SourceError;
        // ⚠ The STORE spells a Binance perpetual with `vike_venues::PERP_SUFFIX`, the endpoint the wire
        // symbol — the kline path's own rule ([`fetch_klines_range`]); a symbol WITHOUT the suffix is
        // REFUSED before any request ([`funding_wire`]).
        let wire = funding_wire(crate::VENUE, symbol, " (USDⓈ-M or COIN-M)")?;
        // ⚠ USDⓈ-M and COIN-M perpetuals share this ONE `.P` suffix, and until 2026-09-29 only
        // `fapi` ever answered this request — silently: a COIN-M symbol got HTTP 200 with no rows
        // rather than a refusal, so `binance:BTCUSD_PERP.P:funding` wrote nothing and said nothing
        // was wrong. See [`FundingRateSourceBinance::funding_url_for`] for the routing and [`PATH_FUNDING_DAPI`]
        // for the measurement behind it.
        let url_base = self.funding_url_for(wire).map_err(SourceError::Refused)?;
        funding_walk_forward(
            start_ms,
            end_ms,
            FUNDING_PAGE_CAP,
            BINANCE_PERP.history.page_delay(),
            "binance-funding",
            |at| {
                SourceError::Fetch(at.text(
                    crate::VENUE,
                    symbol,
                    vike_data::source::FUNDING_INTERVAL,
                ))
            },
            |cursor| {
                format!(
                    "{url_base}?symbol={wire}&startTime={cursor}&endTime={end_ms}\
                     &limit={FUNDING_PAGE_CAP}"
                )
            },
        )
    }
}

#[path = "history_tests.rs"]
#[cfg(test)]
mod history_tests;
