//! **A venue's HISTORY SOURCE — the store-free fetch half of a backfill.**
//!
//! A venue bridge implements one of these traits; `vike-backfill` owns the other half (the
//! commit key, the still-forming-candle guard, the append); `vike-datahub` holds the registries
//! that say which bridge serves which venue. This module is the seam, in the storage crate
//! because it is the lowest crate both ends already name: every venue bridge depends on
//! `vike-data` for `live::DataClient`, and `vike-backfill` writes the store.
//!
//! **Store-free by construction.** A source returns rows. It never sees a `HistoryStore`, never
//! spells a commit key and never decides whether a candle is still forming, so every venue
//! inherits the same ingest and a new venue is one impl plus one registry row
//! (`docs/decisions/0094-backfill-names-no-venue.md`).
//!
//! **The ONE page walk, both directions, every row type** ([`page_walk_forward`],
//! [`page_walk_backward`] over a [`PageWalkRow`]): a venue's klines and its funding rates page by the
//! same rule, so the rule lives here once, below every bridge. A walk is pure: it never sleeps and
//! never touches the network; the page fetch, its throttle and its retries are the caller's closure
//! (`vike_bridge_core::klines::kline_page_walk_backward` is the paced one). The venue facts are its
//! parameters: the direction, the row cap a forward walk stops short of, the cursor a forward page
//! starts at ([`PageWalkRow::next_cursor_ms`]), and whatever the closure does with the cursor it is
//! handed (okx asks `after = end + 1`, deribit's funding `start - 1`).
//!
//! ⚠ **A page that does not move the cursor is an ERROR, never a short success** (owner ruling
//! H-D6, 2026-11-12): a venue answering the same page again would otherwise end the backfill "OK"
//! with a hole in it. The walk names the point ([`PageWalkStuck`]) and the caller turns it into its
//! own error, in [`PageWalkStuck::text`]'s one sentence. An EMPTY page stays the venue's end, and so
//! does a SHORT forward page. The rows read before that point are not handed back: a whole window
//! lands under ONE commit key (`vike-backfill`'s `ingest_klines`), so storing a short list would
//! spend that key on it for good, and a re-run asks the whole window again.

use serde::{Deserialize, Serialize};
use vike_marketdata::{Bar, FundingRatePoint};
use vike_utility::errors::{CarriedRefusal, Declared};

#[path = "source_tests.rs"]
#[cfg(test)]
mod source_tests;

/// **Where a fill can come from** — the closed roster of history providers, with ONE stable
/// spelling ([`BackfillProvider::as_str`]).
///
/// `vike-backfill` keys its capability table by it (`crates/vike-backfill/src/capabilities.rs`'s
/// `backfill_capabilities`, an exhaustive match, so a new variant here fails that crate's build until the
/// table decides). `Databento` and `Tardis` are vendor lanes that exist only as hand-run backfill
/// bins. There is no `Off`: this is a roster of providers, and "no source" is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BackfillProvider {
    /// The venue's own historical API — free, and only as complete as the venue exposes.
    Venue,
    /// The `data.vike.io` / pmxt archives — paid per GB or keyless CC-BY, complete L2 + trades.
    Archive,
    /// Databento — a paid historical vendor.
    Databento,
    /// Tardis — a paid historical vendor.
    Tardis,
}

impl BackfillProvider {
    /// Every source, in declaration order.
    pub const ALL: [BackfillProvider; 4] = [
        BackfillProvider::Venue,
        BackfillProvider::Archive,
        BackfillProvider::Databento,
        BackfillProvider::Tardis,
    ];

    /// The stable lowercase spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            BackfillProvider::Venue => "venue",
            BackfillProvider::Archive => "archive",
            BackfillProvider::Databento => "databento",
            BackfillProvider::Tardis => "tardis",
        }
    }
}

/// Why a source returned no rows. `Fetch`: the venue was asked and the answer failed. `Refused`:
/// the request cannot be expressed, the venue was NOT asked, and retrying cannot help.
///
/// ⚠ **A variant name is a wire tag**: the gatewayhub's `FetchFailed` reply carries this enum as it is
/// (`crates/vike-gatewayhub-client/src/proto.rs`'s `Response`), inner text byte for byte, so a renamed
/// variant is a protocol break, and a new one ships with a captured frame in `fixtures/gatewayhub_wire/`
/// (`crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`'s `DATA_SOURCE_SOURCE`).
///
/// Each payload is the bridge's refusal with its id kept ([`CarriedRefusal`]: `refusal.carried()`, free text
/// `.into()`), and it is SERIALIZED AS ITS TEXT ONLY (`carried_text`), so those bytes never move: the id crosses
/// the gatewayhub's hop in the frame's own `refusal_id` beside it, and the client puts it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceError {
    /// The venue was asked and failed: network, HTTP status or decode.
    Fetch(#[serde(with = "carried_text")] CarriedRefusal),
    /// The request cannot be expressed by this source; nothing was asked of the venue.
    Refused(#[serde(with = "carried_text")] CarriedRefusal),
}

impl SourceError {
    /// The inner text, whichever variant: what a `CatalogError` or a tick fetch's `String` carries.
    pub fn text(&self) -> &str {
        self.refusal().text()
    }

    /// The inner refusal, its id kept, whichever variant.
    #[must_use]
    pub fn refusal(&self) -> &CarriedRefusal {
        match self {
            SourceError::Fetch(carried) | SourceError::Refused(carried) => carried,
        }
    }
}

/// The id of the refusal inside, whichever variant (`None` for free text).
impl Declared for SourceError {
    fn declared_id(&self) -> Option<&str> {
        self.refusal().declared_id()
    }
}

/// A [`SourceError`] payload on the wire: its text alone, the bytes a `String` wrote, read back with no id.
mod carried_text {
    use serde::{Deserialize, Deserializer, Serializer};
    use vike_utility::errors::CarriedRefusal;

    pub(super) fn serialize<S: Serializer>(
        carried: &CarriedRefusal,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(carried.text())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<CarriedRefusal, D::Error> {
        String::deserialize(deserializer).map(CarriedRefusal::from)
    }
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::Fetch(e) => write!(f, "venue fetch: {e}"),
            SourceError::Refused(e) => write!(f, "refused: {e}"),
        }
    }
}

impl std::error::Error for SourceError {}

/// One venue's OHLCV history. `Send + Sync` because the datahub's registry is a `static` of
/// `&'static dyn KlineSource` and its table moves each borrow into a `Send + Sync` closure.
pub trait KlineSource: Send + Sync {
    /// The store `venue` partition the rows land under — the bridge's own venue id.
    fn venue(&self) -> &str;

    /// Fetch `[start_ms, end_ms]` of `(symbol, interval)` bars. Does the network I/O and the
    /// venue's own paging; touches no store. `symbol` is the unified store symbol: a venue whose
    /// own spelling differs maps it here, and REFUSES ([`SourceError::Refused`]) rather than
    /// guessing when the mapping cannot be expressed.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError>;
}

/// The reserved `interval` LABEL of the market funding-rate series — not a cadence. Funding
/// cadence varies by venue and symbol and lives in the spacing of the stored `Bar::ts`; one
/// stable label keeps the series in its own `(venue, symbol, "funding")` keyspace, where it can
/// never interleave with an OHLCV series. The datahub's `Backfill` verb routes a request carrying
/// this interval to a [`FundingRateSource`].
pub const FUNDING_INTERVAL: &str = "funding";

/// One venue's market funding-rate history. `Send + Sync` for the same reason as
/// [`KlineSource`].
pub trait FundingRateSource: Send + Sync {
    /// The store `venue` partition — the bridge's own venue id.
    fn venue(&self) -> &str;

    /// Fetch the venue's funding points for `symbol` over `[start_ms, end_ms]`, ts-ascending and
    /// de-duplicated. Does the network I/O and the venue's paging; touches no store.
    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<FundingRatePoint>, SourceError>;
}

/// The reserved `interval` LABEL of an options venue's public TRADE log (`kind=option_trade`), not a
/// cadence: the datahub's `Backfill` verb routes a request carrying it to an [`OptionTradeSource`], with
/// the request's `symbol` naming the venue's CURRENCY (`deribit:BTC:option-trades`), and the lane derives
/// the hourly `kind=chain` rows from what it stores (`crate::option_trade_log::hourly_chain_rows`).
pub const OPTION_TRADES_INTERVAL: &str = "option-trades";

/// One venue's public option-trade log. `Send + Sync` for the same reason as [`KlineSource`].
pub trait OptionTradeSource: Send + Sync {
    /// The store `venue` partition — the bridge's own venue id.
    fn venue(&self) -> &str;

    /// Fetch every option trade of `currency` (the venue's own currency word: `BTC`, `ETH`, `USDC`) over
    /// `[start_ms, end_ms]`, ts-ascending, each trade ONCE (two trades of one millisecond are two rows).
    /// Does the network I/O and the venue's paging; touches no store. A currency the venue does not know
    /// is the venue's own refusal ([`SourceError::Fetch`]).
    fn fetch(
        &self,
        currency: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<crate::option_trade_log::OptionTradeRow>, SourceError>;
}

/// The reserved `interval` LABEL of a prediction market's RULES texts (`kind=text`, the text kind
/// `crate::text_log::TEXT_KIND_POLYMARKET_RULES`), not a cadence: the datahub's `Backfill` verb routes a
/// request carrying it to a [`MarketRulesSource`], whose `symbol` is the series' collector: `archive`, every
/// closed market (`polymarket:archive:rules`), or ONE recurring series named by its markets' slug stem, those
/// markets alone (`polymarket:btc-updown-5m:rules`, [`MarketRulesSource::closed_of_series`]).
pub const RULES_INTERVAL: &str = "rules";

/// One prediction-market venue's RULES texts: each market's question, its rules (the venue's own
/// description), its resolution source and, once it has resolved, its final outcome, as
/// `crate::text_log::TextRow`s (`crate::text_log`'s module doc carries the columns). `Send + Sync` for the
/// same reason as [`KlineSource`]. Does the network I/O and the venue's paging; touches no store.
pub trait MarketRulesSource: Send + Sync {
    /// The venue the texts are read from (`polymarket`).
    fn venue(&self) -> &str;

    /// The archived rows of every CLOSED market whose scheduled end falls in `[start_ms, end_ms]`: its rules
    /// received when it closed and, once it has resolved, its final outcome as a later row of the same text
    /// received when it resolved (each row's own `received_ms`, outside the window as often as not). A window
    /// that cannot be read whole is an error, never a short answer.
    fn closed_ending(
        &self,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<crate::text_log::TextRow>, SourceError>;

    /// The archived rows ([`Self::closed_ending`]'s two) of every CLOSED market of the recurring `series` whose
    /// window STARTS in `[start_ms, end_ms]`: polymarket's `btc-updown-5m` names the markets slugged
    /// `btc-updown-5m-{start in epoch seconds}`, one every five minutes. A name that states no window length is
    /// refused, nothing asked; a window the venue lists no closed market for is absent, never an error.
    fn closed_of_series(
        &self,
        series: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<crate::text_log::TextRow>, SourceError>;

    /// The rules row of the OPEN market that lists the outcome `token`, received now; empty when no open
    /// market lists it.
    fn of_token(&self, token: &str) -> Result<Vec<crate::text_log::TextRow>, SourceError>;
}

/// **A row a page walk reads** ([`page_walk_forward`], [`page_walk_backward`]): a kline [`Bar`], a
/// [`FundingRatePoint`], or a venue's own raw row when its cursor is not the row's time.
pub trait PageWalkRow {
    /// The row's time, ms since epoch: what the window `[start_ms, end_ms]` keeps, the order the
    /// result is sorted in and the key it is de-duplicated by.
    fn ts_ms(&self) -> i64;

    /// Where the next FORWARD page starts after this row: one ms past [`Self::ts_ms`], unless the
    /// venue pages by another of the row's times (hyperliquid's candles advance past their CLOSE `T`).
    fn next_cursor_ms(&self) -> i64 {
        self.ts_ms().saturating_add(1)
    }
}

impl PageWalkRow for Bar {
    fn ts_ms(&self) -> i64 {
        self.timestamp
    }
}

impl PageWalkRow for FundingRatePoint {
    fn ts_ms(&self) -> i64 {
        self.timestamp_ms
    }
}

/// **Where a page walk stopped because a non-empty page did not move its cursor** — the position
/// that page was asked at. The caller's `stuck` closure turns it into its own error;
/// [`Self::text`] is the one sentence every caller says it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageWalkStuck {
    /// [`page_walk_forward`]: the FULL page asked from `cursor_ms` named no row past it.
    Forward { cursor_ms: i64 },
    /// [`page_walk_backward`]: the page asked to end at `end_ms` held no row at or below it.
    Backward { end_ms: i64 },
}

impl PageWalkStuck {
    /// `"{venue} {symbol} {interval}: the page from 2024-03-10T10:00:00.001Z repeated; the backfill
    /// stopped here"` (`up to` for a backward walk). A funding walk passes [`FUNDING_INTERVAL`].
    #[must_use]
    pub fn text(self, venue: &str, symbol: &str, interval: &str) -> String {
        let (page, at_ms) = match self {
            PageWalkStuck::Forward { cursor_ms } => ("from", cursor_ms),
            PageWalkStuck::Backward { end_ms } => ("up to", end_ms),
        };
        format!(
            "{venue} {symbol} {interval}: the page {page} {} repeated; the backfill stopped here",
            vike_model::time::epoch_ms_to_utc_timestamp_ms(at_ms)
        )
    }
}

/// **The FORWARD page walk** over `[start_ms, end_ms]`: call `fetch_page(cursor)` from
/// `cursor = start_ms` until a page is empty or SHORT (`< rows_per_page`: a forward endpoint's
/// `start`/`end` is a filter, so a full page means rows remain) or the cursor passes `end_ms`. The
/// cursor moves to the page's greatest [`PageWalkRow::next_cursor_ms`], so wire order is not
/// assumed. A FULL page that would not move the cursor forward ends the walk with
/// `stuck(PageWalkStuck::Forward)` (the module doc). Rows outside the window are dropped; the result
/// is sorted and de-duplicated by [`PageWalkRow::ts_ms`].
pub fn page_walk_forward<T: PageWalkRow, E>(
    start_ms: i64,
    end_ms: i64,
    rows_per_page: usize,
    stuck: impl FnOnce(PageWalkStuck) -> E,
    mut fetch_page: impl FnMut(i64) -> Result<Vec<T>, E>,
) -> Result<Vec<T>, E> {
    let mut out: Vec<T> = Vec::new();
    let mut cursor = start_ms;
    while cursor <= end_ms {
        let page = fetch_page(cursor)?;
        let Some(next) = page.iter().map(T::next_cursor_ms).max() else { break };
        let page_len = page.len();
        out.extend(page.into_iter().filter(|r| (start_ms..=end_ms).contains(&r.ts_ms())));
        if page_len < rows_per_page {
            break;
        }
        if next <= cursor {
            return Err(stuck(PageWalkStuck::Forward { cursor_ms: cursor }));
        }
        cursor = next;
    }
    out.sort_by_key(T::ts_ms);
    out.dedup_by_key(|r| r.ts_ms());
    Ok(out)
}

/// **The BACKWARD page walk** over `[start_ms, end_ms]`, for a venue whose history endpoint is
/// END-ANCHORED (bybit, okx and deribit answer the NEWEST page of a window too wide for one page and
/// drop `start` in silence, measured 2026-08-04 for klines and 2026-11-08 for funding).
/// `fetch_page(end)` returns one page ending at `end` inclusive (any order); the walk seeds
/// `end = end_ms` and moves it one ms below the page's OLDEST row until a page is empty or reaches
/// `start_ms`. A page that would not move the cursor backward (a venue answering rows newer than
/// `end`) ends the walk with `stuck(PageWalkStuck::Backward)` (the module doc).
///
/// ⚠ A short page is NOT a stop: termination follows the observed rows, so a venue-side change to
/// its row cap cannot silently truncate a backfill. An empty window asks nothing. Rows outside the
/// window are dropped; the result is sorted and de-duplicated by [`PageWalkRow::ts_ms`].
pub fn page_walk_backward<T: PageWalkRow, E>(
    start_ms: i64,
    end_ms: i64,
    stuck: impl FnOnce(PageWalkStuck) -> E,
    mut fetch_page: impl FnMut(i64) -> Result<Vec<T>, E>,
) -> Result<Vec<T>, E> {
    let mut out: Vec<T> = Vec::new();
    let mut page_end = end_ms;
    while start_ms <= page_end {
        let page = fetch_page(page_end)?;
        let Some(oldest) = page.iter().map(T::ts_ms).min() else { break };
        out.extend(page.into_iter().filter(|r| (start_ms..=end_ms).contains(&r.ts_ms())));
        if oldest <= start_ms {
            break;
        }
        let next = oldest.saturating_sub(1);
        if next >= page_end {
            return Err(stuck(PageWalkStuck::Backward { end_ms: page_end }));
        }
        page_end = next;
    }
    out.sort_by_key(T::ts_ms);
    out.dedup_by_key(|r| r.ts_ms());
    Ok(out)
}

/// A decimal-STRING (or JSON number) price field as a positive finite `f64`, else `None`: binance
/// sends `"markPrice": ""` on its rows from before it published one (measured 2026-11-08 on 2020 rows).
pub fn positive_price(v: Option<&serde_json::Value>) -> Option<f64> {
    let v = v?;
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
        .filter(|p| p.is_finite() && *p > 0.0)
}

/// Decode the funding-rate JSON shape both wired venues share: a top-level ARRAY of objects, each
/// with an epoch-ms timestamp under `timestamp_field`, a decimal-STRING `fundingRate`, an optional
/// decimal-string `premium` and an optional `markPrice` ([`FundingRatePoint::mark`]). Strict on the
/// envelope (a non-array body is an `Err`), tolerant per row (a row missing either field, or whose
/// rate will not parse, is skipped), and softer still for the premium and the mark: absent,
/// unparseable or non-finite is `None` and the row is kept.
pub fn parse_funding_rate_rows(
    body: &str,
    timestamp_field: &str,
) -> Result<Vec<FundingRatePoint>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a top-level JSON array".to_string())?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let ts_ms = row.get(timestamp_field).and_then(serde_json::Value::as_i64)?;
            let rate_str = row.get("fundingRate").and_then(serde_json::Value::as_str)?;
            let rate = rate_str.parse::<f64>().ok()?;
            let premium = row
                .get("premium")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|p| p.is_finite());
            let mark = positive_price(row.get("markPrice"));
            Some(FundingRatePoint { timestamp_ms: ts_ms, rate, premium, mark })
        })
        .collect())
}
