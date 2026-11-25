//! Hyperliquid perp funding-payment history — fetches the keyless-vs-master `userFunding` `/info`
//! records (realized funding credits/debits over time) for perp PnL accounting.
//!
//! Endpoint: `POST /info` with `{"type":"userFunding","user":"<master addr>","startTime","endTime"}`
//! → a JSON array of `{"time":<ms>,"hash","delta":{"type":"funding","coin","usdc","szi",
//! "fundingRate"}}`. `usdc` is the signed realized payment (decimal string).
//!
//! Mirrors the [`mod@crate::reconcile`] idiom: a PURE `parse_user_funding(body: &str) ->
//! Result<Vec<_>, String>` free function (fixture-tested against recorded `/info` JSON, NO network,
//! never panics — malformed JSON is an `Err`) that every read delegates to, decoding decimal-string /
//! number fields through [`vike_bridge_core::json`]. All reads scope to the **MASTER** account address
//! (never the agent wallet — agent reads return empty; same pitfall the recon reads guard against).
//!
//! There is no `FundingReport` type in `vike-model` (only the live `FundingEvent` union member, which
//! carries `position_side`/`mark_price` but neither the row's `hash` nor the `szi` at funding time
//! this historical record needs), so this defines a local [`FundingPayment`] — the same choice
//! [`mod@crate::reconcile`] makes, reusing `vike-model` report types only where they already exist.

use serde_json::{Value, json};

use vike_bridge_core::exec_actor::funding_poll::FundingWindow;
use vike_bridge_core::json::{json_int, json_number};
use vike_model::events::{Event, FundingEvent, PositionSide};

use crate::refusals::HyperliquidHistoryRefusal;
use crate::transport::HyperliquidTransport;

/// One realized perp funding payment, decoded from a `userFunding` row's top-level fields plus its
/// nested `delta`. All HL numerics arrive as decimal STRINGS (or, for `time`, a JSON number).
#[derive(Debug, Clone, PartialEq)]
pub struct FundingPayment {
    /// Funding timestamp, ms since epoch (the row's top-level `time`).
    pub time_ms: i64,
    /// Venue `coin` (the perp base, e.g. `"BTC"`) — carried VERBATIM, unresolved (same convention as
    /// [`mod@crate::reconcile`] and [`crate::execution::event_mapper`]).
    pub coin: String,
    /// The SIGNED realized funding payment, in USDC: **negative = paid**, **positive = received**
    /// (`delta.usdc`).
    pub usdc: f64,
    /// Signed position size at funding time (`delta.szi`; + long / − short).
    pub szi: f64,
    /// The funding rate applied this interval (`delta.fundingRate`).
    pub funding_rate: f64,
    /// The row's `hash` (`0x…`). ⚠ The ZERO hash on every funding row the venue lists, so it keys
    /// nothing: [`HyperliquidFundingPoller`] dedups by `(time_ms, coin)`.
    pub hash: String,
}

// --- pure parser -------------------------------------------------------------------------------

/// Parse `body` as the top-level `userFunding` JSON ARRAY → [`FundingPayment`]s, erroring (never
/// panicking) on non-array / malformed JSON. Each row's realized fields live under the nested `delta`
/// envelope: rows whose `delta.type != "funding"` (or that carry no `delta` at all) are SKIPPED — the
/// `delta` envelope is shared across HL's ledger-update endpoints, so this stays robust if a mixed
/// stream is ever passed. `time` rides on the top-level row; `coin`/`usdc`/`szi`/`fundingRate` come
/// from `delta`; the signed `usdc`/`szi` decode as-is (a leading `-` survives [`json_number`]'s parse).
pub fn parse_user_funding(body: &str) -> Result<Vec<FundingPayment>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a top-level JSON array".to_string())?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let delta = row.get("delta")?;
            // Only realized funding rows: skip any non-`funding` delta (the envelope is shared).
            if delta.get("type").and_then(|t| t.as_str()) != Some("funding") {
                return None;
            }
            Some(FundingPayment {
                time_ms: row.get("time").and_then(json_int).unwrap_or(0),
                coin: delta.get("coin").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                usdc: delta.get("usdc").and_then(json_number).unwrap_or(0.0),
                szi: delta.get("szi").and_then(json_number).unwrap_or(0.0),
                funding_rate: delta.get("fundingRate").and_then(json_number).unwrap_or(0.0),
                hash: row.get("hash").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect())
}

// --- fetch -------------------------------------------------------------------------------------

/// Fetch realized perp funding payments over `[start_ms, end_ms]` for the **master** account address
/// via one keyless `POST /info` `userFunding` read, delegating the 200 body (re-serialized once for
/// the pure seam, the [`mod@crate::reconcile`] pairing) to [`parse_user_funding`]. The transport's
/// [`vike_bridge_core::transport::VenueApiError`] is flattened to its `msg` `String` so both this and
/// the parser share one error type — matching [`mod@crate::reconcile`]'s style.
pub fn fetch_funding(
    transport: &HyperliquidTransport,
    master: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<FundingPayment>, String> {
    let body = json!({
        "type": "userFunding",
        "user": master,
        "startTime": start_ms,
        "endTime": end_ms,
    });
    let response = transport.info(&body).map(|v| v.to_string()).map_err(|e| e.msg)?;
    parse_user_funding(&response)
}

// --- live-fold producer (wires the above into the platform's `Event::Funding` law) -------------

/// One realized [`FundingPayment`] -> the canonical [`Event::Funding`]. `usdc` is ALREADY
/// received-positive (see the module doc) — no sign flip, matching the bybit/okx REST-poll
/// sourcing. `coin` rides as `symbol` here, and the exec plane's poll thread names it by its USER name
/// (`BTC` is `BTCUSDC.P`, decision 0290) through the account's universe before the event leaves the
/// bridge, as an order or fill event is.
pub fn funding_payment_to_event(p: &FundingPayment, venue: &str) -> Event {
    Event::Funding(FundingEvent {
        venue: venue.to_string().into(),
        symbol: p.coin.as_str().into(),
        position_side: PositionSide::Both,
        funding_rate: p.funding_rate,
        amount: p.usdc,
        mark_price: None,
        timestamp: p.time_ms,
        // Stamped at the START — see `vike_model::events::FundingEvent::route_key`.
        route_key: None,
        currency: None,
    })
}

/// The most rows one `userFunding` answer holds, oldest first: MEASURED 2026-11-08 on a mainnet
/// account with ~177 coins, a 3-hour window answered exactly 500 rows and stopped inside the third hour.
const USER_FUNDING_PAGE_CAP: usize = 500;

/// REST poller for realized perp funding payments (`userFunding`; NOT on either WS channel this
/// crate subscribes to — see [`crate::execution::user_data`]'s module doc for the two it DOES carry,
/// `orderUpdates` + `userFills`). Generic over the fetch step (rather than embedding
/// [`HyperliquidTransport`] directly) so the dedup/floor logic is unit-testable with a canned
/// closure, no network, no transport fake.
///
/// **Spawn-time floor (the restart law).** `floor_ms` is a HARD emission floor: a payment with
/// `time_ms < floor_ms` is NEVER emitted, no matter what the venue returns. The spawner passes
/// its spawn instant, because payments settled before the start are already embedded in the
/// venue-reported balance the live account runs on — live engines start `BalanceMode::Delta` but
/// flip `Authoritative` on every reconcile pass (`ReconcileClient::fetch_balance` — for HL, perp
/// `clearinghouseState.marginSummary.accountValue` — seeds `Account::balance` absolutely; see
/// `vike_core::runtime::reconcile_reports`), so re-folding a pre-start payment through the
/// non-idempotent `Account::apply_funding` (`balance += amount`) double-counts it. A trailing
/// LOOKBACK would re-fold up to that whole lookback on every restart for exactly this reason —
/// hence a floor, not a lookback. The `(time, coin)` seen-set (in-memory, so empty at every start)
/// remains the second guard within a process lifetime.
///
/// The window and the seen-set are `vike_bridge_core::exec_actor::funding_poll`'s `FundingWindow`
/// (every polled venue's: from the floor, then five minutes before the end of the last good window,
/// at most a week wide; an id forgotten once it is older than the next window's start), keyed by
/// `(time, coin)`: the account pays each coin's funding once per funding instant, and every row of
/// one instant carries the same `time`. ⚠ NOT the row's `hash`: a funding row is no transaction, and
/// every one the venue lists states the ZERO hash (`0x000…0`, MEASURED 2026-11-08 on 500 mainnet rows,
/// `crates/bridges/hyperliquid/tests/fixtures/hl_user_funding_mainnet.json`), so a hash seen-set
/// let through the first payment a process ever saw and dropped every later one as its duplicate.
pub struct HyperliquidFundingPoller<F> {
    fetch: F,
    venue: String,
    /// The window over `userFunding` and the `(time, coin)` keys emitted, floored at the spawn instant.
    window: FundingWindow<(i64, String)>,
}

impl<F> HyperliquidFundingPoller<F>
where
    F: FnMut(i64, i64) -> Result<Vec<FundingPayment>, String>,
{
    /// `floor_ms` is the spawn instant (or `0` for an unfloored test/probe): it starts the window
    /// AND permanently floors emission, so a (re)started poller never replays history.
    pub fn new(fetch: F, venue: &str, floor_ms: i64) -> Self {
        HyperliquidFundingPoller {
            fetch,
            venue: venue.to_string(),
            window: FundingWindow::new(floor_ms),
        }
    }

    /// One poll iteration up to `now_ms`: fetch the window, decode NEW (at or after the window's
    /// start, not-yet-seen `(time, coin)`) rows to `Event::Funding`s and move the window on. A fetch
    /// failure is `Err` (so the caller can rate-limit a warn) and never moves the window — the next
    /// poll re-tries it. A FULL answer ([`USER_FUNDING_PAGE_CAP`] rows) moves the window only to its
    /// newest row, so the next poll reads on from there instead of skipping what the cap cut off.
    pub fn poll(&mut self, now_ms: i64) -> Result<Vec<Event>, String> {
        // The window start is an optimization; the window's own start filter is the law.
        let (start, end) = self.window.span(now_ms);
        let rows = (self.fetch)(start, end)?;
        let mut out = Vec::new();
        for p in &rows {
            if self.window.admit((p.time_ms, p.coin.clone()), p.time_ms) {
                out.push(funding_payment_to_event(p, &self.venue));
            }
        }
        // shortcut: more than a page of rows at ONE instant (500 coins in funding) would re-read the
        // same page forever; an account of this platform holds a handful of coins.
        self.window.read_through(match rows.iter().map(|p| p.time_ms).max() {
            Some(newest) if rows.len() >= USER_FUNDING_PAGE_CAP => newest,
            _ => end,
        });
        Ok(out)
    }
}

// --- market funding-rate source (distinct from the account-payment history above) --------------

/// Hyperliquid caps `fundingHistory` at 500 rows per response.
const FUNDING_HISTORY_PAGE_CAP: usize = 500;

/// Parse a `POST /info {"type":"fundingHistory",…}` body — `{coin, fundingRate (decimal string),
/// premium (string), time (ms number)}` rows, timestamp from `time`, premium kept.
pub fn parse_funding_history(body: &str) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    vike_data::source::parse_funding_rate_rows(body, "time")
}

/// This venue's MARKET funding-rate source (perps; 1h cadence), a row of the datahub's
/// `FUNDING_SOURCES`. Keyless mainnet `/info` over [`crate::transport::HyperliquidTransport`],
/// whose own rate gate self-throttles. Distinct from [`fetch_funding`], which reads one
/// ACCOUNT's realized payments.
pub struct FundingRateSourceHyperliquid {
    rest: String,
}

impl FundingRateSourceHyperliquid {
    /// The source on hyperliquid's address rows `urls`: the `live` `data` REST row (the public RATE is
    /// market data), MAINNET for every tier, as it always was.
    ///
    /// # Errors
    /// The row's refusal.
    pub fn from_urls(
        urls: &vike_venues::venue_urls::VenueUrls,
    ) -> Result<Self, vike_venues::venue_urls::UrlRefusal> {
        let rest = crate::config::base_url(
            vike_model::VenueNetwork::Mainnet,
            urls,
            vike_venues::venue_urls::UrlChannel::Data,
            vike_venues::venue_urls::UrlProtocol::Rest,
        )?;
        Ok(Self { rest: rest.to_string() })
    }
}

impl vike_data::source::FundingRateSource for FundingRateSourceHyperliquid {
    fn venue(&self) -> &str {
        crate::transport::consts::VENUE
    }

    /// [`crate::history::klines::history_coin_for`] first — a `SourceError::Refused`, NOT a `Fetch`,
    /// because nothing was asked of the venue (see that variant's own doc) — the same gate its
    /// kline twin `crate::history::klines::KlineSourceHyperliquid::fetch` applies: the series' store name
    /// (`BTCUSDC.P`) is not its venue `coin` (`BTC`), which its kept row carries. D5 (0094 follow-ups): this
    /// fetch once sent the symbol straight through as `coin` and never applied a gate at all.
    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<vike_marketdata::FundingRatePoint>, vike_data::source::SourceError> {
        use vike_data::source::SourceError;
        let coin = crate::history::klines::history_coin_for(symbol)
            .map_err(|refusal| SourceError::Refused(refusal.carried()))?;
        let transport = crate::transport::HyperliquidTransport::new(
            vike_model::VenueNetwork::Mainnet,
            &self.rest,
        );
        let stuck = |at: vike_data::source::PageWalkStuck| {
            SourceError::Fetch(
                at.text(
                    crate::transport::consts::VENUE,
                    symbol,
                    vike_data::source::FUNDING_INTERVAL,
                )
                .into(),
            )
        };
        vike_data::source::page_walk_forward(
            start_ms,
            end_ms,
            FUNDING_HISTORY_PAGE_CAP,
            stuck,
            |cursor| {
                let body = serde_json::json!({
                    "type": "fundingHistory",
                    "coin": coin,
                    "startTime": cursor,
                    "endTime": end_ms,
                });
                let raw = match transport.info(&body) {
                    Ok(v) => v.to_string(),
                    Err(e) => {
                        let refusal = HyperliquidHistoryRefusal::FundingUnread { why: e.msg };
                        return Err(SourceError::Fetch(refusal.carried()));
                    }
                };
                parse_funding_history(&raw).map_err(|why| SourceError::Fetch(why.into()))
            },
        )
    }
}

#[path = "funding_tests.rs"]
#[cfg(test)]
mod funding_tests;
