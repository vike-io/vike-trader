//! Bybit funding via the REST transaction-log — the documented received-positive source.
//! Exact port of `exec/bybit/funding.py`.
//!
//! GET /v5/account/transaction-log?category=linear&type=SETTLEMENT: `funding` is the
//! SIGNED cashflow (+received / −paid, doc verbatim) fed DIRECTLY into
//! FundingEvent.amount. NOT the WS execution-topic 'Funding' row, whose execFee is the
//! EXACT NEGATIVE of this cashflow (the sign trap — solved by source selection, not
//! sign-flipping). No markPrice in the log (mark_price=None); feeRate IS the rate.
//! Dedups by row 'id' BEFORE decoding (apply_funding is NOT idempotent), marking seen
//! only AFTER a successful emit; the seen-set is bounded.
//!
//! **Spawn-time floor (the restart law).** The id-dedup is in-memory only, so at every process
//! (re)start the seen-set is empty and — with no time bound — the venue's whole default
//! transaction-log window (~7 days of settlements) would re-fold through the non-idempotent
//! `Account::apply_funding` (`balance += amount`). Those historical settlements are ALREADY
//! embedded in the venue-reported balance the live account runs on: live engines start
//! `BalanceMode::Delta` and flip `Authoritative` once a venue balance lands, which the reconcile
//! pass does (`ReconClient::fetch_balance` seeds `Account::balance` absolutely — see
//! `vike_core::runtime::reconcile_reports`), so re-emitting a pre-start settlement double-counts
//! it. This lane exists ONLY for settlements that land WHILE mounted. Hence [`BybitFundingPoller`]
//! takes a `floor_ms` (the spawner passes its spawn instant) and NEVER emits a row with
//! `transactionTime < floor_ms` — the client-side ts filter is the guarantee; the `startTime`
//! request param merely shrinks the response. The in-memory id-dedup remains the second guard
//! within a process lifetime.

use std::collections::HashSet;

use serde_json::{Value, json};
use vike_bridge_core::json::{get_f64 as num, json_num};
use vike_model::events::{Event, FundingEvent};

use crate::perp::BybitPerpRest;
use crate::transport::BybitTransport;

pub const LOG_PATH: &str = "/v5/account/transaction-log";
const MAX_SEEN: usize = 4096;

/// Decode SETTLEMENT rows → FundingEvents. Skips non-SETTLEMENT rows and rows whose
/// `funding` is null, "", or the literal "0" (Python tuple-membership, ported exactly).
pub fn decode_bybit_funding_settlements(
    rows: &[Value],
    venue: &str,
    symbol: &str,
) -> Vec<FundingEvent> {
    let mut out = Vec::new();
    for r in rows {
        if r.get("type").map(py_str).unwrap_or_default() != "SETTLEMENT" {
            continue;
        }
        let funding = r.get("funding");
        let skip = match funding {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty() || s == "0",
            _ => false,
        };
        if skip {
            continue;
        }
        out.push(FundingEvent {
            venue: venue.to_string().into(),
            symbol: match r.get("symbol") {
                Some(Value::String(s)) => s.clone().into(),
                _ => symbol.to_string().into(),
            },
            position_side: "BOTH".to_string().into(),
            funding_rate: num(r, "feeRate"),
            amount: funding.map(num_v).unwrap_or(0.0), // received-positive, NO flip
            mark_price: None,
            ts: int(r, "transactionTime"),
            // Stamped at the MOUNT — see `vike_model::events::FundingEvent::route_key`.
            route_key: None,
        });
    }
    out
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Value-level `float(x or 0)` for the already-extracted `funding` cell — the keyless twin of the
/// shared keyed `num` (= `json::get_f64`) above; both fold through `json_num`, so the coercion has
/// exactly one home. (`py_str` deliberately does NOT: it needs serde's Display for the dedup guard.)
fn num_v(v: &Value) -> f64 {
    json_num(v).unwrap_or(0.0)
}

fn int(v: &Value, key: &str) -> i64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// REST poller for linear-perp funding settlements. `poll()` returns the NEW events (`ts >=
/// floor_ms`, dedup by row id) for the caller to feed into the core ingest; a fetch failure is
/// `Err` (so the caller can rate-limit a warn) and never marks anything seen — the next poll
/// retries.
pub struct BybitFundingPoller<'a, T: BybitTransport> {
    client: &'a BybitPerpRest<T>,
    symbol: String,
    /// Emission floor, ms: rows with `transactionTime < floor_ms` are NEVER emitted (see the
    /// module doc's restart law). Spawners pass their spawn instant; `0` = no floor (tests/probes).
    floor_ms: i64,
    seen_ids: HashSet<String>,
}

impl<'a, T: BybitTransport> BybitFundingPoller<'a, T> {
    pub fn new(client: &'a BybitPerpRest<T>, symbol: &str, floor_ms: i64) -> Self {
        BybitFundingPoller {
            client,
            symbol: symbol.to_string(),
            floor_ms,
            seen_ids: HashSet::new(),
        }
    }

    pub fn poll(&mut self) -> Result<Vec<Event>, String> {
        // `startTime` is a response-size optimization ONLY — the ts filter below is the law
        // (sent only with a real floor, so a floorless probe stays byte-identical to before).
        let mut params = vec![("category", json!("linear")), ("type", json!("SETTLEMENT"))];
        if self.floor_ms > 0 {
            params.push(("startTime", json!(self.floor_ms.to_string())));
        }
        let result = self.client.call_public(LOG_PATH, &params).map_err(|e| e.msg)?;
        let no_rows = vec![];
        let rows = result.get("list").and_then(|l| l.as_array()).unwrap_or(&no_rows);
        let new_rows: Vec<Value> = rows
            .iter()
            .filter(|r| {
                if int(r, "transactionTime") < self.floor_ms {
                    return false; // pre-floor: permanently excluded, never marked seen
                }
                let rid = r.get("id").map(py_str).unwrap_or_default();
                !rid.is_empty() && !self.seen_ids.contains(&rid)
            })
            .cloned()
            .collect();
        if new_rows.is_empty() {
            return Ok(Vec::new());
        }
        let events: Vec<Event> = decode_bybit_funding_settlements(&new_rows, "bybit", &self.symbol)
            .into_iter()
            .map(Event::Funding)
            .collect();
        // mark seen AFTER decoding succeeds
        for r in &new_rows {
            let rid = r.get("id").map(py_str).unwrap_or_default();
            if !rid.is_empty() {
                self.seen_ids.insert(rid);
            }
        }
        // bound the seen-set (drop arbitrary half like the Python trim)
        if self.seen_ids.len() > MAX_SEEN {
            let excess: Vec<String> =
                self.seen_ids.iter().take(self.seen_ids.len() - MAX_SEEN / 2).cloned().collect();
            for id in excess {
                self.seen_ids.remove(&id);
            }
        }
        Ok(events)
    }
}
