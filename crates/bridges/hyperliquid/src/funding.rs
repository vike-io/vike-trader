//! Hyperliquid perp funding-payment history — fetches the keyless-vs-master `userFunding` `/info`
//! records (realized funding credits/debits over time) for perp PnL accounting.
//!
//! Endpoint: `POST /info` with `{"type":"userFunding","user":"<master addr>","startTime","endTime"}`
//! → a JSON array of `{"time":<ms>,"hash","delta":{"type":"funding","coin","usdc","szi",
//! "fundingRate"}}`. `usdc` is the signed realized payment (decimal string).
//!
//! Mirrors the [`crate::recon_client`] idiom: a PURE `parse_user_funding(body: &str) ->
//! Result<Vec<_>, String>` free function (fixture-tested against recorded `/info` JSON, NO network,
//! never panics — malformed JSON is an `Err`) that every read delegates to, decoding decimal-string /
//! number fields through [`vike_bridge_core::json`]. All reads scope to the **MASTER** account address
//! (never the agent wallet — agent reads return empty; same pitfall the recon reads guard against).
//!
//! There is no `FundingReport` type in `vike-model` (only the live `FundingEvent` union member, which
//! carries `position_side`/`mark_price` but neither the `hash` dedup key nor the `szi` at funding time
//! this historical record needs), so this defines a local [`FundingPayment`] — the same choice
//! [`crate::recon_client`] makes, reusing `vike-model` report types only where they already exist.

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
    /// [`crate::recon_client`] and [`crate::event_mapper`]).
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
/// the pure seam, the [`crate::recon_client`] pairing) to [`parse_user_funding`]. The transport's
/// [`vike_bridge_core::transport::VenueApiError`] is flattened to its `msg` `String` so both this and
/// the parser share one error type — matching [`crate::recon_client`]'s style.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic `userFunding` array: a PAID (negative `usdc`, long `szi`) row and a RECEIVED
    /// (positive `usdc`, short `szi`) row — count, signed `usdc`, `coin`, `szi`, rate, time, hash.
    #[test]
    fn parse_user_funding_signed_paid_and_received() {
        // Top-level `time` is a NUMBER; hash/coin/usdc/szi/fundingRate are STRINGS.
        let body = r#"[
            {"time":1681222254710,"hash":"0xabc","delta":{"type":"funding","coin":"BTC","usdc":"-1.25","szi":"0.5","fundingRate":"0.0000125"}},
            {"time":1681222254720,"hash":"0xdef","delta":{"type":"funding","coin":"ETH","usdc":"0.75","szi":"-2.0","fundingRate":"-0.0000088"}}
        ]"#;
        let r = parse_user_funding(body).unwrap();
        assert_eq!(r.len(), 2, "both funding rows parsed");

        let paid = &r[0];
        assert_eq!(paid.time_ms, 1681222254710);
        assert_eq!(paid.coin, "BTC", "coin carried verbatim");
        assert_eq!(paid.usdc, -1.25, "negative usdc = paid, signed as-is");
        assert_eq!(paid.szi, 0.5, "long position size");
        assert_eq!(paid.funding_rate, 0.0000125);
        assert_eq!(paid.hash, "0xabc");

        let received = &r[1];
        assert_eq!(received.coin, "ETH");
        assert_eq!(received.usdc, 0.75, "positive usdc = received");
        assert_eq!(received.szi, -2.0, "short position size (signed)");
        assert_eq!(received.funding_rate, -0.0000088, "negative funding rate survives");
        assert_eq!(received.hash, "0xdef");
    }

    /// Non-`funding` deltas (the envelope is shared across HL ledger endpoints) and rows missing a
    /// `delta` are skipped; only the real funding rows survive, in order.
    #[test]
    fn parse_user_funding_skips_non_funding_and_deltaless_rows() {
        let body = r#"[
            {"time":1,"hash":"0x1","delta":{"type":"funding","coin":"BTC","usdc":"-1.0","szi":"0.5","fundingRate":"0.00001"}},
            {"time":2,"hash":"0x2","delta":{"type":"deposit","usdc":"100.0"}},
            {"time":3,"hash":"0x3"},
            {"time":4,"hash":"0x4","delta":{"type":"funding","coin":"ETH","usdc":"0.5","szi":"-1.0","fundingRate":"-0.00002"}}
        ]"#;
        let r = parse_user_funding(body).unwrap();
        assert_eq!(r.len(), 2, "deposit + delta-less rows skipped");
        assert_eq!(r.iter().map(|p| p.coin.as_str()).collect::<Vec<_>>(), ["BTC", "ETH"]);
    }

    /// An empty array is an empty (Ok) result, not an error.
    #[test]
    fn parse_user_funding_empty_array_is_ok_empty() {
        assert_eq!(parse_user_funding("[]").unwrap(), Vec::new());
    }

    /// Malformed / non-array bodies are errors, never panics (mirrors the recon_client robustness).
    #[test]
    fn parse_user_funding_malformed_is_error_not_panic() {
        assert!(parse_user_funding("not json").is_err());
        assert!(
            parse_user_funding("{}").is_err(),
            "an object (not the expected array) is an error"
        );
        assert!(parse_user_funding("null").is_err());
        assert!(parse_user_funding("42").is_err());
    }

    // --- live-fold producer ---

    fn payment(hash: &str, coin: &str, usdc: f64, time_ms: i64) -> FundingPayment {
        FundingPayment {
            time_ms,
            coin: coin.to_string(),
            usdc,
            szi: 1.0,
            funding_rate: 0.0001,
            hash: hash.to_string(),
        }
    }

    /// The platform convention (`FundingEvent.amount` received-positive) maps DIRECTLY off HL's
    /// already-signed `usdc` — no flip either direction, unlike bybit's WS-vs-REST sign trap.
    #[test]
    fn funding_payment_to_event_carries_the_signed_amount_unflipped() {
        let paid = funding_payment_to_event(&payment("0x1", "BTC", -1.25, 1000), "hyperliquid");
        match paid {
            Event::Funding(f) => {
                assert_eq!(f.venue, "hyperliquid");
                assert_eq!(f.symbol, "BTC");
                assert_eq!(f.position_side, PositionSide::Both);
                assert_eq!(f.amount, -1.25, "negative = paid, carried as-is");
                assert_eq!(f.funding_rate, 0.0001);
                assert_eq!(f.ts, 1000);
                assert_eq!(f.mark_price, None);
            }
            other => panic!("expected Event::Funding, got {other:?}"),
        }
        let received = funding_payment_to_event(&payment("0x2", "ETH", 0.75, 2000), "hyperliquid");
        match received {
            Event::Funding(f) => assert_eq!(f.amount, 0.75, "positive = received, carried as-is"),
            other => panic!("expected Event::Funding, got {other:?}"),
        }
    }

    /// The poll/spawn-glue seam: a canned `fetch` closure (NO network, NO transport fake needed —
    /// [`HyperliquidTransport`] has no seam to fake) proves the watermark advances and new rows
    /// decode to `Event::Funding`s.
    #[test]
    fn poller_emits_new_rows_and_advances_the_watermark() {
        let mut poller = HlFundingPoller::new(
            |start, end| {
                assert_eq!(start, 0, "first poll's window floors at 0, never negative");
                assert_eq!(end, 10_000);
                Ok(vec![payment("0xabc", "BTC", 1.5, 500)])
            },
            "hyperliquid",
            0, // unfloored: watermark(0) - OVERLAP_MS would go negative -> clamped to 0
        );
        let evs = poller.poll(10_000).unwrap();
        match evs.as_slice() {
            [Event::Funding(f)] => {
                assert_eq!(f.symbol, "BTC");
                assert_eq!(f.amount, 1.5);
            }
            other => panic!("expected one Event::Funding, got {other:?}"),
        }
    }

    /// The restart law: a poller floored at its spawn instant (as `spawn_funding_poll` does) must
    /// NEVER emit a payment with `time_ms` BEFORE the floor — those are already embedded in the
    /// authoritative venue balance — while a payment AT/after the floor is emitted exactly once.
    /// Simulates the (re)start-with-empty-seen-set case the in-memory hash-dedup cannot cover.
    #[test]
    fn poller_never_emits_payments_before_the_spawn_floor() {
        const FLOOR: i64 = 5_000;
        let mut poller = HlFundingPoller::new(
            |start, _end| {
                assert_eq!(start, FLOOR, "the first window starts AT the floor, never before");
                // The venue answers with history straddling the floor anyway (defensive: the
                // filter, not the request window, is the guarantee).
                Ok(vec![
                    payment("0xold", "BTC", 9.99, 1_000),
                    payment("0xedge", "BTC", 0.25, 5_000),
                    payment("0xnew", "ETH", 1.5, 9_000),
                ])
            },
            "hyperliquid",
            FLOOR,
        );
        let evs = poller.poll(10_000).unwrap();
        match evs.as_slice() {
            [Event::Funding(edge), Event::Funding(new)] => {
                assert_eq!(edge.ts, 5_000, "ts == floor is emitted (floor is inclusive)");
                assert_eq!(new.ts, 9_000);
                assert_eq!(new.amount, 1.5);
            }
            other => panic!("expected exactly the at/post-floor payments, got {other:?}"),
        }
        // The next cadence re-serves the same rows (overlap window): nothing re-emits.
        assert!(poller.poll(10_060).unwrap().is_empty(), "no duplicates on the second poll");
    }

    /// A row whose `hash` the poller already emitted (the overlap window re-fetching it) is NOT
    /// re-emitted — the precise dedup key doing its job across two overlapping polls.
    #[test]
    fn poller_dedupes_repeated_hashes_across_polls() {
        let mut poller = HlFundingPoller::new(
            // Every call returns the SAME row (as the overlap window would re-fetch it).
            |_start, _end| Ok(vec![payment("0xdup", "BTC", 2.0, 500)]),
            "hyperliquid",
            0,
        );
        let first = poller.poll(1_000).unwrap();
        assert_eq!(first.len(), 1, "first sighting emits");
        let second = poller.poll(2_000).unwrap();
        assert!(second.is_empty(), "the same hash on the next poll must not re-emit");
    }

    /// A fetch failure surfaces as `Err` (never panics, so the caller can rate-limit a warn) and
    /// does NOT advance the watermark — the next poll retries the same window rather than
    /// silently skipping it.
    #[test]
    fn poller_fetch_failure_is_err_and_does_not_advance_watermark() {
        let mut first_call = true;
        let mut poller = HlFundingPoller::new(
            move |start, _end| {
                if first_call {
                    first_call = false;
                    return Err("transient failure".to_string());
                }
                // The retry's window start must be the SAME as if the failed poll never happened —
                // i.e. still floored from the original watermark, not advanced past it.
                assert_eq!(start, 0);
                Ok(vec![payment("0xretry", "BTC", 3.0, 500)])
            },
            "hyperliquid",
            0,
        );
        assert!(poller.poll(100_000).is_err(), "a fetch failure surfaces as Err, never panics");
        let retried = poller.poll(100_000).unwrap();
        assert_eq!(retried.len(), 1, "the retry succeeds against the SAME (unadvanced) window");
    }
}
