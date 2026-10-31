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
//! balance the live account runs on: live engines start `BalanceMode::Delta` and flip
//! `Authoritative` once a venue balance lands, which the reconcile pass does
//! (`ReconClient::fetch_balance` seeds `Account::balance`
//! absolutely — see `vike_core::runtime::reconcile_reports`), so re-emitting a pre-start bill
//! double-counts it. This lane exists ONLY for bills that land WHILE started. Hence
//! [`OkxFundingPoller`] takes a `floor_ms` (the spawner passes its spawn instant) and NEVER emits
//! a bill with `ts < floor_ms` — the client-side ts filter is the guarantee; the `begin` request
//! param merely shrinks the response. The in-memory billId-dedup remains the second guard within
//! a process lifetime.
//!
//! **Admitted instruments (FEES-AUDIT 2026-10-26 §4.7, known-limitations "Funding settled on a SWAP
//! other than the mounted one").** The account trades any SWAP the engine admits on demand (decision
//! 0173), and the bills query used to name the MOUNTED `instId` only, so the funding of every other
//! instrument never reached the ledger. The poller now shares the reconcile client's admitted list
//! ([`OkxFundingPoller::with_admitted`]; the same `Arc<AdmittedSymbols>` the adapter hands both, so the
//! reconcile client being TOLD an instrument is also what lets its funding fold). With nothing admitted
//! it sends exactly the request it always sent. With something admitted it reads the account's SWAP
//! bills ONCE, without `instId`, and folds only the rows of the mounted instrument and the admitted
//! ones: dropping `instId` alone would deliver the bills of instruments the engine never admitted
//! (a position opened by hand on the exchange), whose funding the engine's ledger does not own.
//! A row of any other instrument is marked seen like any other and never folded. An instrument admitted
//! LATER does not fold the bills that settled before the poll that first saw it admitted (the account-wide
//! read reaches back to the poller's floor, and the engine owned no position there: those are in the wallet
//! a reconcile pass adopts, and folding them again would count them twice): its rows are marked seen on
//! that poll, and fold from the next one. The price is a funding settlement landing inside that one poll
//! interval, which is not folded either.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};
use vike_exec::recon::AdmittedSymbols;
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
            position_side: vike_model::events::PositionSide::Both,
            funding_rate: 0.0,
            amount: chg.parse::<f64>().unwrap_or(0.0), // received-positive, NO flip
            mark_price: None,
            ts: match b.get("ts") {
                Some(Value::String(s)) => s.parse::<i64>().unwrap_or(0),
                Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
                _ => 0,
            },
            // Stamped at the START — see `vike_model::events::FundingEvent::route_key`.
            route_key: None,
            ccy: None,
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
    /// The instruments the engine admitted beyond `symbol` (module doc). EMPTY = the mounted-only
    /// request this poller always made.
    admitted: Arc<AdmittedSymbols>,
    /// The admitted instruments an earlier good poll already saw (upper-case): their bills fold; an
    /// instrument not in it yet is `new` to the poll that reads it first.
    known: HashSet<String>,
}

impl<'a, T: OkxTransport> OkxFundingPoller<'a, T> {
    pub fn new(client: &'a OkxPerpRest<T>, symbol: &str, floor_ms: i64) -> Self {
        OkxFundingPoller {
            client,
            symbol: symbol.to_string(),
            floor_ms,
            seen_bill_ids: HashSet::new(),
            admitted: Arc::default(),
            known: HashSet::new(),
        }
    }

    /// Share the engine's admitted-instrument list (module doc): the SAME `Arc` the reconcile client
    /// is told each admission through.
    #[must_use]
    pub fn with_admitted(mut self, admitted: Arc<AdmittedSymbols>) -> Self {
        self.admitted = admitted;
        self
    }

    /// Whether `bill` folds: nothing admitted = every row of the mounted-only request (as always);
    /// otherwise the mounted instrument's, or an admitted instrument's that an EARLIER poll already saw
    /// admitted (`new` = the ones this poll sees for the first time, whose backlog is not folded: module
    /// doc). `admitted` is upper-case, as `AdmittedSymbols` stores it; compared without case anyway. A
    /// row naming no instrument cannot be attributed, so it does not fold once something is admitted.
    fn folds(&self, bill: &Value, admitted: &[String], new: &[String]) -> bool {
        if admitted.is_empty() {
            return true;
        }
        let same = |list: &[String], inst: &str| list.iter().any(|a| a.eq_ignore_ascii_case(inst));
        bill.get("instId").and_then(Value::as_str).is_some_and(|inst| {
            inst.eq_ignore_ascii_case(&self.symbol) || (same(admitted, inst) && !same(new, inst))
        })
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
        // Nothing admitted: the mounted-only request, byte for byte what this poller always sent.
        // Something admitted: ONE account-wide read (no `instId`), narrowed to the instruments the
        // engine trades below (module doc).
        let admitted = self.admitted.symbols();
        let new: Vec<String> =
            admitted.iter().filter(|a| !self.known.contains(*a)).cloned().collect();
        let mut params = vec![("instType", json!("SWAP")), ("type", json!(FUNDING_BILL_TYPE))];
        if admitted.is_empty() {
            params.push(("instId", json!(self.symbol)));
        }
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
            // The account-wide read also returns instruments the engine does not trade, and the backlog
            // of one it has only just admitted: seen (above), never folded.
            if !self.folds(b, &admitted, &new) {
                continue;
            }
            fresh.push(b.clone());
        }
        // Only after a good read: a failed one returned above, and the next poll sees `new` again.
        self.known.extend(new);
        Ok(decode_okx_funding_bills(&fresh, "okx", &self.symbol)
            .into_iter()
            .map(Event::Funding)
            .collect())
    }
}
