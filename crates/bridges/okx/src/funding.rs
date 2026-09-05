//! OKX funding via REST bills — funding is NOT on any OKX WS channel. Exact port of
//! `exec/okx/funding.py`.
//!
//! GET /api/v5/account/bills?instType=SWAP&type=8: the cashflow is `pnl` (documented
//! funding field; a funding bill has fee==0 so balChg == pnl) with `balChg` as the
//! documented FALLBACK. Both received-positive (income > 0, expense < 0) — fed DIRECTLY
//! into FundingEvent.amount. No rate in the bill (funding_rate=0), no mark. Dedup-FIRST
//! by billId (apply_funding is NOT idempotent); bounded seen-set.
//!
//! **Spawn-time floor (the restart law).** The billId-dedup is in-memory only, so at every
//! process (re)start the seen-set is empty and — with no time bound — the venue's default bills
//! window (up to 7 days) would re-fold through the non-idempotent `Account::apply_funding`
//! (`balance += amount`). Those historical bills are ALREADY embedded in the venue-reported
//! balance the live account runs on: live engines start `BalanceMode::Delta` but flip
//! `Authoritative` on the first venue account frame (`Account::apply_account_state`, absolute
//! assignment) and on every reconcile pass (`ReconClient::fetch_balance` seeds `Account::balance`
//! absolutely — see `vike_core::runtime::reconcile_reports`), so re-emitting a pre-start bill
//! double-counts it. This lane exists ONLY for bills that land WHILE mounted. Hence
//! [`OkxFundingPoller`] takes a `floor_ms` (the spawner passes its spawn instant) and NEVER emits
//! a bill with `ts < floor_ms` — the client-side ts filter is the guarantee; the `begin` request
//! param merely shrinks the response. The in-memory billId-dedup remains the second guard within
//! a process lifetime.

use std::collections::HashSet;

use serde_json::{Value, json};
use vike_model::events::{Event, FundingEvent};

use crate::perp::{OkxPerpRest, PATH_BILLS};
use crate::transport::OkxTransport;

const FUNDING_BILL_TYPE: &str = "8";
const SEEN_CAP: usize = 4096;

/// `pnl` primary, `balChg` fallback; None when neither is a real value (null/""/"0").
fn funding_amount(bill: &Value) -> Option<String> {
    for key in ["pnl", "balChg"] {
        match bill.get(key) {
            Some(Value::String(s)) if !s.is_empty() && s != "0" => return Some(s.clone()),
            Some(Value::Number(n)) if n.as_f64() != Some(0.0) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

pub fn decode_okx_funding_bills(bills: &[Value], venue: &str, symbol: &str) -> Vec<FundingEvent> {
    let mut out = Vec::new();
    for b in bills {
        let btype = match b.get("type") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        if btype != FUNDING_BILL_TYPE {
            continue;
        }
        let Some(chg) = funding_amount(b) else { continue };
        out.push(FundingEvent {
            venue: venue.to_string().into(),
            symbol: match b.get("instId") {
                Some(Value::String(s)) => s.clone().into(),
                _ => symbol.to_string().into(),
            },
            position_side: "BOTH".to_string().into(),
            funding_rate: 0.0,
            amount: chg.parse::<f64>().unwrap_or(0.0), // received-positive, NO flip
            mark_price: None,
            ts: match b.get("ts") {
                Some(Value::String(s)) => s.parse::<i64>().unwrap_or(0),
                Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
                _ => 0,
            },
            // Stamped at the MOUNT — see `vike_model::events::FundingEvent::route_key`.
            route_key: None,
        });
    }
    out
}

/// Bill `ts` in ms (string or number) — the floor-filter key; 0 when absent/unparsable.
fn bill_ts(b: &Value) -> i64 {
    match b.get("ts") {
        Some(Value::String(s)) => s.parse::<i64>().unwrap_or(0),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Bills poller: `poll()` returns NEW funding events (`ts >= floor_ms`, dedup-FIRST by billId,
/// seen marked before decode like the Python original); a fetch failure is `Err` (so the caller
/// can rate-limit a warn) and marks nothing seen — the next poll retries.
pub struct OkxFundingPoller<'a, T: OkxTransport> {
    client: &'a OkxPerpRest<T>,
    symbol: String,
    /// Emission floor, ms: bills with `ts < floor_ms` are NEVER emitted (see the module doc's
    /// restart law). Spawners pass their spawn instant; `0` = no floor (tests/probes).
    floor_ms: i64,
    seen_bill_ids: HashSet<String>,
}

impl<'a, T: OkxTransport> OkxFundingPoller<'a, T> {
    pub fn new(client: &'a OkxPerpRest<T>, symbol: &str, floor_ms: i64) -> Self {
        OkxFundingPoller {
            client,
            symbol: symbol.to_string(),
            floor_ms,
            seen_bill_ids: HashSet::new(),
        }
    }

    fn remember(&mut self, bill_id: String) {
        self.seen_bill_ids.insert(bill_id);
        if self.seen_bill_ids.len() > SEEN_CAP {
            let stale: Vec<String> =
                self.seen_bill_ids.iter().take(SEEN_CAP / 2).cloned().collect();
            for id in stale {
                self.seen_bill_ids.remove(&id);
            }
        }
    }

    pub fn poll(&mut self) -> Result<Vec<Event>, String> {
        // `begin` is a response-size optimization ONLY — the ts filter below is the law (sent
        // only with a real floor, so a floorless probe stays byte-identical to before).
        let mut params = vec![
            ("instType", json!("SWAP")),
            ("type", json!(FUNDING_BILL_TYPE)),
            ("instId", json!(self.symbol)),
        ];
        if self.floor_ms > 0 {
            params.push(("begin", json!(self.floor_ms.to_string())));
        }
        let bills = self.client.call_get(PATH_BILLS, &params).map_err(|e| e.msg)?;
        let no_rows = vec![];
        let rows = bills.as_array().unwrap_or(&no_rows);
        let mut fresh: Vec<Value> = Vec::new();
        for b in rows {
            let btype = match b.get("type") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            if btype != FUNDING_BILL_TYPE || funding_amount(b).is_none() {
                continue;
            }
            if bill_ts(b) < self.floor_ms {
                continue; // pre-floor: permanently excluded, never marked seen
            }
            let bill_id = match b.get("billId") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            };
            if !bill_id.is_empty() && self.seen_bill_ids.contains(&bill_id) {
                continue;
            }
            if !bill_id.is_empty() {
                self.remember(bill_id);
            }
            fresh.push(b.clone());
        }
        Ok(decode_okx_funding_bills(&fresh, "okx", &self.symbol)
            .into_iter()
            .map(Event::Funding)
            .collect())
    }
}
