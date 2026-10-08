//! Hyperliquid perp funding-payment history — fetches the keyless-vs-master `userFunding` `/info`
//! records (realized funding credits/debits over time) for perp PnL accounting.
//!
//! Endpoint: `POST /info` with `{"type":"userFunding","user":"<master addr>","startTime","endTime"}`
//! → a JSON array of `{"time":<ms>,"hash","delta":{"type":"funding","coin","usdc","szi",
//! "fundingRate"}}`. `usdc` is the signed realized payment (decimal string).
//!
//! Mirrors the [`mod@crate::recon_client`] idiom: a PURE `parse_user_funding(body: &str) ->
//! Result<Vec<_>, String>` free function (fixture-tested against recorded `/info` JSON, NO network,
//! never panics — malformed JSON is an `Err`) that every read delegates to, decoding decimal-string /
//! number fields through [`vike_bridge_core::json`]. All reads scope to the **MASTER** account address
//! (never the agent wallet — agent reads return empty; same pitfall the recon reads guard against).
//!
//! There is no `FundingReport` type in `vike-model` (only the live `FundingEvent` union member, which
//! carries `position_side`/`mark_price` but neither the `hash` dedup key nor the `szi` at funding time
//! this historical record needs), so this defines a local [`FundingPayment`] — the same choice
//! [`mod@crate::recon_client`] makes, reusing `vike-model` report types only where they already exist.

use std::collections::HashSet;

use serde_json::{Value, json};

use vike_bridge_core::json::{json_int, json_num};
use vike_model::events::{Event, FundingEvent, PositionSide};

use crate::transport::HyperliquidTransport;

/// One realized perp funding payment, decoded from a `userFunding` row's top-level fields plus its
/// nested `delta`. All HL numerics arrive as decimal STRINGS (or, for `time`, a JSON number).
#[derive(Debug, Clone, PartialEq)]
pub struct FundingPayment {
    /// Funding timestamp, ms since epoch (the row's top-level `time`).
    pub time_ms: i64,
    /// Venue `coin` (the perp base, e.g. `"BTC"`) — carried VERBATIM, unresolved (same convention as
    /// [`mod@crate::recon_client`] and [`crate::event_mapper`]).
    pub coin: String,
    /// The SIGNED realized funding payment, in USDC: **negative = paid**, **positive = received**
    /// (`delta.usdc`).
    pub usdc: f64,
    /// Signed position size at funding time (`delta.szi`; + long / − short).
    pub szi: f64,
    /// The funding rate applied this interval (`delta.fundingRate`).
    pub funding_rate: f64,
    /// The venue transaction hash (`0x…`), the per-row dedup key for at-most-once accounting.
    pub hash: String,
}

// --- pure parser -------------------------------------------------------------------------------

/// Parse `body` as the top-level `userFunding` JSON ARRAY → [`FundingPayment`]s, erroring (never
/// panicking) on non-array / malformed JSON. Each row's realized fields live under the nested `delta`
/// envelope: rows whose `delta.type != "funding"` (or that carry no `delta` at all) are SKIPPED — the
/// `delta` envelope is shared across HL's ledger-update endpoints, so this stays robust if a mixed
/// stream is ever passed. `time` rides on the top-level row; `coin`/`usdc`/`szi`/`fundingRate` come
/// from `delta`; the signed `usdc`/`szi` decode as-is (a leading `-` survives [`json_num`]'s parse).
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
                usdc: delta.get("usdc").and_then(json_num).unwrap_or(0.0),
                szi: delta.get("szi").and_then(json_num).unwrap_or(0.0),
                funding_rate: delta.get("fundingRate").and_then(json_num).unwrap_or(0.0),
                hash: row.get("hash").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect())
}

// --- fetch -------------------------------------------------------------------------------------

/// Fetch realized perp funding payments over `[start_ms, end_ms]` for the **master** account address
/// via one keyless `POST /info` `userFunding` read, delegating the 200 body (re-serialized once for
/// the pure seam, the [`mod@crate::recon_client`] pairing) to [`parse_user_funding`]. The transport's
/// [`vike_bridge_core::transport::VenueApiError`] is flattened to its `msg` `String` so both this and
/// the parser share one error type — matching [`mod@crate::recon_client`]'s style.
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
    let resp = transport.info(&body).map(|v| v.to_string()).map_err(|e| e.msg)?;
    parse_user_funding(&resp)
}

// --- live-fold producer (wires the above into the platform's `Event::Funding` law) -------------

/// One realized [`FundingPayment`] -> the canonical [`Event::Funding`]. `usdc` is ALREADY
/// received-positive (see the module doc) — no sign flip, matching the bybit/okx REST-poll
/// sourcing. `coin` rides verbatim as `symbol` (HL perp `coin == symbol`, so — unlike an
/// order/fill event — no [`crate::symbology`] remap is needed here).
pub fn funding_payment_to_event(p: &FundingPayment, venue: &str) -> Event {
    Event::Funding(FundingEvent {
        venue: venue.to_string().into(),
        symbol: p.coin.as_str().into(),
        position_side: PositionSide::Both,
        funding_rate: p.funding_rate,
        amount: p.usdc,
        mark_price: None,
        ts: p.time_ms,
        // Stamped at the MOUNT — see `vike_model::events::FundingEvent::route_key`.
        route_key: None,
    })
}

/// Overlap re-queried on every poll (beyond the advancing watermark) so a payment landing right at
/// the previous window's edge is never missed to a clock-skew race; the per-row `hash` seen-set
/// absorbs the resulting duplicate. 5 minutes is generous against HL's funding cadence (hourly).
const OVERLAP_MS: i64 = 5 * 60 * 1000;
/// Bound on the seen-hash set (mirrors bybit/okx's own bounded seen-set poller convention).
const SEEN_CAP: usize = 4096;

/// REST poller for realized perp funding payments (`userFunding`; NOT on either WS channel this
/// crate subscribes to — see [`crate::user_data`]'s module doc for the two it DOES carry,
/// `orderUpdates` + `userFills`). Generic over the fetch step (rather than embedding
/// [`HyperliquidTransport`] directly) so the dedup/floor logic is unit-testable with a canned
/// closure, no network, no transport fake.
///
/// **Spawn-time floor (the restart law).** `floor_ms` is a HARD emission floor: a payment with
/// `time_ms < floor_ms` is NEVER emitted, no matter what the venue returns. The spawner passes
/// its spawn instant, because payments settled before the mount are already embedded in the
/// venue-reported balance the live account runs on — live engines start `BalanceMode::Delta` but
/// flip `Authoritative` on every reconcile pass (`ReconClient::fetch_balance` — for HL, perp
/// `clearinghouseState.marginSummary.accountValue` — seeds `Account::balance` absolutely; see
/// `vike_core::runtime::reconcile_reports`), so re-folding a pre-start payment through the
/// non-idempotent `Account::apply_funding` (`balance += amount`) double-counts it. A trailing
/// LOOKBACK would re-fold up to that whole lookback on every restart for exactly this reason —
/// hence a floor, not a lookback. The hash seen-set (in-memory, so empty at every start) remains
/// the second guard within a process lifetime.
///
/// Each poll queries `[max(watermark - OVERLAP_MS, floor_ms), now]` and dedups by the venue's
/// per-row `hash` — a real unique id, more precise than a coarse `(time, coin)` pair (which could
/// collide if two coins pay funding at the identical millisecond).
pub struct HlFundingPoller<F> {
    fetch: F,
    venue: String,
    /// The hard emission floor (see the struct doc's restart law). Also the watermark seed.
    floor_ms: i64,
    watermark_ms: i64,
    seen_hashes: HashSet<String>,
}

impl<F> HlFundingPoller<F>
where
    F: FnMut(i64, i64) -> Result<Vec<FundingPayment>, String>,
{
    /// `floor_ms` is the spawn instant (or `0` for an unfloored test/probe): it seeds the
    /// watermark AND permanently floors emission, so a (re)started poller never replays history.
    pub fn new(fetch: F, venue: &str, floor_ms: i64) -> Self {
        HlFundingPoller {
            fetch,
            venue: venue.to_string(),
            floor_ms,
            watermark_ms: floor_ms,
            seen_hashes: HashSet::new(),
        }
    }

    /// One poll iteration up to `now_ms`: fetch the window, decode NEW (`time_ms >= floor_ms`,
    /// not-yet-seen-hash) rows to `Event::Funding`s, advance the watermark, and trim the seen-set
    /// if it grows unbounded. A fetch failure is `Err` (so the caller can rate-limit a warn) and
    /// never advances the watermark — the next poll re-tries the same window.
    pub fn poll(&mut self, now_ms: i64) -> Result<Vec<Event>, String> {
        // The window start is an optimization; the `time_ms < floor_ms` filter below is the law.
        let start = (self.watermark_ms - OVERLAP_MS).max(self.floor_ms).max(0);
        let rows = (self.fetch)(start, now_ms)?;
        let mut out = Vec::new();
        for p in &rows {
            if p.time_ms < self.floor_ms {
                continue; // pre-floor: permanently excluded (already in the venue balance)
            }
            if !p.hash.is_empty() && !self.seen_hashes.insert(p.hash.clone()) {
                continue; // already emitted (within the overlap window)
            }
            out.push(funding_payment_to_event(p, &self.venue));
        }
        self.watermark_ms = now_ms;
        if self.seen_hashes.len() > SEEN_CAP {
            let excess: Vec<String> = self
                .seen_hashes
                .iter()
                .take(self.seen_hashes.len() - SEEN_CAP / 2)
                .cloned()
                .collect();
            for h in excess {
                self.seen_hashes.remove(&h);
            }
        }
        Ok(out)
    }
}

// --- market funding-rate source (distinct from the account-payment history above) --------------

/// Hyperliquid caps `fundingHistory` at 500 rows per response.
const FUNDING_HISTORY_PAGE_CAP: usize = 500;

/// Parse a `POST /info {"type":"fundingHistory",…}` body — `{coin, fundingRate (decimal string),
/// premium (string), time (ms number)}` rows, timestamp from `time`, premium kept.
pub fn parse_funding_history(
    body: &str,
) -> Result<Vec<vike_data::source::FundingRatePoint>, String> {
    vike_data::source::parse_funding_rate_rows(body, "time")
}

/// This venue's MARKET funding-rate source (perps; 1h cadence), a row of the datahub's
/// `FUNDING_SOURCES`. Keyless mainnet `/info` over [`crate::transport::HyperliquidTransport`],
/// whose own rate gate self-throttles. Distinct from [`fetch_funding`], which reads one
/// ACCOUNT's realized payments.
pub struct HyperliquidFunding;

impl vike_data::source::FundingRateSource for HyperliquidFunding {
    fn venue(&self) -> &str {
        crate::consts::VENUE
    }

    /// [`crate::history::identity_coin_for`] first — a `SourceError::Refused`, NOT a `Fetch`,
    /// because nothing was asked of the venue (see that variant's own doc) — the same gate its
    /// kline twin `crate::history::HyperliquidKlines::fetch` applies: a spot pair's unified
    /// `symbol` (`HYPE/USDC`) is not its venue `coin` (`@<pairIndex>`), and this seam carries one
    /// symbol and can only express `coin == symbol`. D5 (0094 follow-ups): this fetch sent the
    /// unified `symbol` straight through as `coin` and never applied the gate at all, so a spot
    /// pair reached the venue as a raw `fundingHistory` request instead of being refused.
    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<vike_data::source::FundingRatePoint>, vike_data::source::SourceError> {
        use vike_data::source::SourceError;
        let coin = crate::history::identity_coin_for(symbol).map_err(SourceError::Refused)?;
        let transport =
            crate::transport::HyperliquidTransport::new(crate::config::Network::Mainnet);
        vike_data::source::page_funding_forward(
            start_ms,
            end_ms,
            FUNDING_HISTORY_PAGE_CAP,
            |cursor| {
                let body = serde_json::json!({
                    "type": "fundingHistory",
                    "coin": coin,
                    "startTime": cursor,
                    "endTime": end_ms,
                });
                let raw = match transport.info(&body) {
                    Ok(v) => v.to_string(),
                    Err(e) => return Err(SourceError::Fetch(format!("fundingHistory: {}", e.msg))),
                };
                parse_funding_history(&raw).map_err(SourceError::Fetch)
            },
        )
    }
}

#[path = "funding_tests.rs"]
#[cfg(test)]
mod funding_tests;
