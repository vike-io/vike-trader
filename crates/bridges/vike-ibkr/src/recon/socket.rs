//! IBKR's SOCKET `ReconClient` — the report seam (`vike_exec::recon::ReconClient`) over the ibapi
//! TWS protocol, on the SAME IB Gateway the socket exec dials, under its OWN API client id
//! (`crate::config::IbkrConfig::recon_client_id`, the account's `RECON_CLIENT_ID` field, default
//! `client_id + 2`). Decision 0140 (`docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`):
//! reconcile rides the exec's own product, so a socket start never opens a Client Portal session
//! beside its IB Gateway one. Extra API client ids on one gateway are ONE login, not a second
//! session. The cpapi twin is `crate::recon::cpapi::IbkrReconClient`; `crate::adapter`'s `start_with`
//! picks between them by `cfg.backend`.
//!
//! ## Why its own client id, not the exec's `Client`
//!
//! The executions replay fans onto the exec's global order-update stream
//! (`crate::transport::socket::SocketTransport`'s `request_executions` doc), so reading reports
//! through the exec connection would feed the exec pump rows it did not ask for. A separate
//! connection keeps the two apart.
//!
//! ## What each fetch reads (per-symbol, conId-filtered, like the cpapi client)
//!
//! One client per (account, canonical vike symbol). The conId is resolved ONCE at
//! [`IbkrSocketReconClient::connect`] through `contract_details` on the exact contract the exec
//! places (`crate::transport::socket::build_contract`). That request carries the expiry, so this
//! client CAN pin a future (the cpapi one refuses: `IbkrContract::conid_search_is_ambiguous`).
//! An answer naming more than one conId is refused, never picked from.
//!
//! ## Admitted symbols (any-symbol, decision 0173)
//!
//! [`ReconClient::admit_symbol`] tells the client each symbol the account's engine was admitted on demand.
//! The three wire reads are account-wide, so a pass still makes ONE of each and parses it per instrument
//! ([`plan_targets`]: the mounted conId first, then each admitted symbol pinned to its conId on first use by
//! a bounded `contract_details`; [`order_reports_for`], [`fill_reports_for`], [`position_reports_for`]), and a
//! symbol that cannot be pinned FAILS the whole fetch (the restore planner reads an order a successful report
//! lacks as gone). With nothing admitted a pass reads exactly what it always read. The balance seed stays the
//! mounted contract's currency.
//!
//! ## The account is CHECKED, never assumed
//!
//! One IB Gateway login can hold several accounts, and every read below answers for all of them.
//! So `connect` refuses unless `cfg.account` is one of the login's `managed_accounts`
//! ([`require_managed_account`]), and the positions parser refuses a non-empty read in which the
//! account never appears ([`parse_socket_position_reports`]): a wrong `ACCOUNT` must never produce
//! a complete-looking FLAT report. The exec stamps the same account on every order it places
//! (`crate::transport::socket::build_order`), so on a multi-account login the two target one
//! account.
//!
//! - orders: `all_open_orders` (every API client's and TWS's open orders), filtered to the account
//!   and the conId; the `OrderStatus` rows are joined to their `OrderData` by `permId`.
//! - fills: `executions` filtered to the EXEC client id (server-side AND by the parser, which reads
//!   each row's `client_id`), the account and the conId; commissions joined by `execution_id`.
//! - positions: `positions` up to its `PositionEnd`, filtered to the account and the conId. An
//!   absent symbol gets the FLAT zero row (`crate::recon::cpapi::parse_position_reports`' contract)
//!   — only when the read is empty or carries the account.
//! - balance: `account_summary` with the `$LEDGER:ALL` tag, the `CashBalance` row (CASH, not
//!   equity) in the contract's currency; the `BASE` block only when it declares the contract's
//!   currency as the base ([`parse_socket_balance`]), else no seed; the rows are read under the stamp the Gateway
//!   puts on them, `All`, and only on a login that manages just this account ([`ledger_account_for`]).
//!   **One subscription, kept** (`crate::recon::summary`'s module doc): it is NOT requested per pass, the pass
//!   reads its cache.
//!
//! Every fetch is bounded by [`READ_DEADLINE`]; a read that does not reach its end in time is an
//! `Err`, never a partial answer: a short list would look like orders or positions the venue no
//! longer holds.
//!
//! ## The connection is REPLACED when its summary allowance is spent (A33)
//!
//! The Gateway lets one SESSION (one TCP connection) make two `reqAccountSummary` requests in all, a cancelled one
//! included (`crate::recon::summary`), so a connection that has made both can never serve a balance again. This
//! client used to hold its `Client` for life and wait for the adapter to be rebuilt; it now replaces the whole
//! connection (`Link`: the `Client` and the feed kept on it) itself: the pass that finds the feed spent
//! (`LedgerBalance::spent`) closes the old client (IB refuses a second client under an id in use, error 326), dials
//! a new one through the gate's turn (`gate_for`'s `dial`: after the previous dial, spaced by `CONNECT_GAP`) and
//! swaps it in, and the NEXT pass opens the new session's first request. The order half of the venue is untouched:
//! the exec and the data connections are other clients, and the mounted conId and the symbols resolved so far are
//! kept. The re-dial is paced (`Pace`): a minute after one that worked, and 1, 2, then 5 minutes after one that
//! failed; it is not made while the gate's rebuild schedule of a lost gateway link is armed
//! (`GatewayGate::permit_data`); a failed one leaves the client without a
//! connection, and every read says so (`LOST_WORDS`) until the schedule allows the next dial. Not measured
//! against a gateway (the attended paper-session proof is open item B04).
//!
//! Statuses go through the one IB-status classifier the cpapi client uses
//! (`crate::recon::cpapi::normalize_order_status`), so both backends read a status alike.
//!
//! ## ⚠ Unverified against a gateway
//!
//! Every parser here is pinned offline over CONSTRUCTED ibapi rows
//! (`crates/bridges/vike-ibkr/tests/ibkr_socket_reconcile_parse.rs`). Nothing has run against a gateway
//! yet; the vehicle is the read-only `crates/bridges/vike-ibkr/tests/ibkr_socket_reconcile_smoke.rs`.
//! Three things only that run (or a fill) can settle (item 2 has been):
//!
//! 1. **Whose executions a non-master client id sees.** IB may answer `reqExecutions` with only the
//!    ASKING client's executions unless it is the Gateway's Master API client id. Then this client
//!    (id `client_id + 2`) sees none of the exec client's fills, and `fetch_fill_reports` is
//!    quietly empty. [`IbkrSocketReconClient::executions_by_client_id`] asks with NO client filter
//!    and counts the rows per client id, so a smoke run after a fill shows directly whether the
//!    exec client's rows are visible. If they are not, the fix is the Gateway's Master API client
//!    id setting (decision 0140's "What would reopen this").
//! 2. **The `$LEDGER` tag names: OBSERVED 2026-10-20** (the ladder probe on the demo login: the raw bytes of 50 rows,
//!    decoded). `CashBalance` and `Currency` are the right tags, in the currencies `USD` and `BASE`. ⚠ EVERY row is
//!    stamped with the account `All` (the group the request names, `crate::recon::summary`'s `LEDGER_GROUP`), never the
//!    account's number: read under `cfg.account`, no row matched and every pass answered "not reported" with no error,
//!    so the engine never adopted the venue's cash. [`LedgerBalance`] reads the rows under the stamp
//!    [`ledger_account_for`] picks: `All` when the login manages only this account (IB's page says it sums the ledger
//!    over every account of the login; not a measurement here), then the account's own number; a several-account login
//!    reads only its own number, so it stays unreported. This number becomes the BALANCE seed under `quarantine`. NOT
//!    verified: that the demo login manages only its own account, that any login stamps its rows with the account's
//!    own number, and that a one-account login other than the demo one stamps `All` too.
//! 3. **The fill time.** `Execution.time` is a preformatted string whose timezone has never been
//!    seen (`crate::transport::socket`'s module doc). A fill report is stamped with the READ time,
//!    the same receive-time convention the exec path uses for `FillEvent.ts`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ibapi::accounts::{AccountSummary, Position, PositionUpdate};
use ibapi::client::blocking::Client;
use ibapi::contracts::ContractDetails;
use ibapi::orders::{Action, ExecutionFilter, ExecutionSide, Executions, Liquidity, Orders};
use ibapi::subscriptions::SubscriptionItem;
use vike_exec::recon::{AdmittedSymbols, ReconClient};
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};
use vike_venues::IbkrBackend;

use crate::config::IbkrConfig;
use crate::contract::parse_simplified;
use crate::error::IbkrError;
use crate::recon::cpapi::{VENUE, normalize_order_status, normalize_order_type};
use crate::recon::summary::{ClientGateway, LEDGER_GROUP, SummaryFeed, SummaryGateway};
use crate::redial::backoff;
use crate::transport::gateway_gate::gate_for;
use crate::transport::socket::{OPEN_DEADLINE, bounded, build_contract};

/// How long one report read may take, end to end. A read past it is an `Err`, never a partial list.
pub const READ_DEADLINE: Duration = Duration::from_secs(10);

/// How long closing the spent connection may take before the re-dial goes on without waiting for it (the thread that
/// closes it is counted in the gate, which makes the dial wait for it for a few seconds more).
const CLOSE_DEADLINE: Duration = Duration::from_secs(5);

/// How long after a re-dial that WORKED the next one waits: a session that spends its two requests at once (two
/// `[1101]` in a row) must not turn the Gateway's socket into a loop.
const REDIAL_GAP: Duration = Duration::from_secs(60);

/// The words of a read made while the reconcile connection is closed and its re-dial has not worked: the same for
/// every such read, so the node's edge-triggered failure line is said once. The last dial error follows it.
const LOST_WORDS: &str = "ibkr reconcile: this client's connection was closed to begin a new account-summary session and \
                          its re-dial has not worked yet; it is retried on a spaced schedule (1, 2, then 5 minutes), \
                          or turn account.active off and on to rebuild the adapter";

/// The ledger row that is the account's cash in one currency.
const CASH_BALANCE: &str = "CashBalance";

// --- the bounded read --------------------------------------------------------------------------

/// How a subscription says it is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndsBy {
    /// The stream closes (`all_open_orders`' `OpenOrderEnd`, `executions`' `ExecutionDataEnd`).
    Close,
    /// A data item marks the end (`PositionEnd`, the summary's `End`); a close before it is a fault.
    Marker,
}

/// Drain one ibapi subscription into a list, bounded by `deadline`. `next` is the subscription's
/// `next_timeout`; `is_end` names the end-marker item (never included) for [`EndsBy::Marker`].
/// A notice is logged and skipped (the stream stays open, as ibapi documents). An error, a timeout,
/// or a close before the marker is an `Err` naming `what`: the reconcile engine must never read
/// a list that stopped early as the venue's whole answer.
pub(crate) fn drain<T>(
    mut next: impl FnMut(Duration) -> Option<Result<SubscriptionItem<T>, ibapi::Error>>,
    deadline: Duration,
    what: &str,
    ends_by: EndsBy,
    is_end: impl Fn(&T) -> bool,
) -> Result<Vec<T>, String> {
    let start = Instant::now();
    let mut out = Vec::new();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(format!("ibkr {what}: no end of the reply within {deadline:?}"));
        }
        match next(remaining) {
            Some(Ok(SubscriptionItem::Data(item))) => {
                if ends_by == EndsBy::Marker && is_end(&item) {
                    return Ok(out);
                }
                out.push(item);
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                tracing::debug!(target: "vike_ibkr::recon", what, code = n.code, "notice: {}", n.message);
            }
            Some(Err(e)) => return Err(format!("ibkr {what}: {e}")),
            None => {
                return match ends_by {
                    EndsBy::Close if start.elapsed() < deadline => Ok(out),
                    EndsBy::Close => {
                        Err(format!("ibkr {what}: no end of the reply within {deadline:?}"))
                    }
                    EndsBy::Marker => {
                        Err(format!("ibkr {what}: the stream ended before its end marker"))
                    }
                };
            }
        }
    }
}

// --- pure parsers (over constructed ibapi rows — the fixture-tested units) ----------------------

/// The ONE conId `contract_details` names for the contract. Rows that repeat one conId (a SMART
/// equity answered per exchange) are one contract; two DIFFERENT conIds are refused, never picked
/// from, because every report row is filtered to this id and `PositionDrift` is the kind `hybrid`
/// auto-applies. An empty answer is refused too.
pub fn resolve_conid(details: &[ContractDetails], symbol: &str) -> Result<i32, String> {
    let mut ids: Vec<i32> =
        details.iter().map(|d| d.contract.contract_id).filter(|id| *id != 0).collect();
    ids.sort_unstable();
    ids.dedup();
    match ids.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("IBKR contract_details named no conId for {symbol}")),
        many => Err(format!(
            "IBKR contract_details named {} contracts for {symbol} ({many:?}); reconcile pins \
             exactly one, so this venue-symbol stays unreconciled",
            many.len()
        )),
    }
}

/// Whether a row's account is this client's. An empty account on the row is accepted (IB fills it
/// for every report this client reads; a blank one says nothing against it).
fn same_account(row: &str, account: &str) -> bool {
    row.is_empty() || row == account
}

/// The join key of an open order and its status: IB's `permId`, unique per order across every
/// API client (an `order_id` is only unique per client), falling back to the order id when IB sent
/// no permId.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum OrderKey {
    Perm(i64),
    Id(i32),
}

fn order_key(perm_id: i64, order_id: i32) -> OrderKey {
    if perm_id != 0 { OrderKey::Perm(perm_id) } else { OrderKey::Id(order_id) }
}

/// `all_open_orders` rows → `OrderStatusReport`, filtered to `account` and `conid`, stamped with
/// the canonical `symbol`. Each `OrderData` is one report; its `OrderStatus` (joined by permId,
/// the last one wins) supplies the status, filled quantity and average price, else the
/// `OrderData`'s own `order_state.status` with nothing filled. `order_id` → `venue_order_id` (the
/// number the exec path binds, `crate::exec::event_mapper`); `order_ref` → `client_order_id`;
/// `Buy` → +1 and every sell action → -1. An `OrderStatus` with no `OrderData` cannot be tied to a
/// contract and is not reported. `ts` is `0`: the rows carry no time.
pub fn parse_socket_order_reports(
    rows: &[Orders],
    account: &str,
    conid: i32,
    symbol: &str,
) -> Vec<OrderStatusReport> {
    let mut status = HashMap::new();
    for row in rows {
        if let Orders::OrderStatus(s) = row {
            status.insert(order_key(s.perm_id, s.order_id), s);
        }
    }
    rows.iter()
        .filter_map(|row| match row {
            Orders::OrderData(od) => Some(od),
            Orders::OrderStatus(_) => None,
        })
        .filter(|od| od.contract.contract_id == conid && same_account(&od.order.account, account))
        .map(|od| {
            let joined = status.get(&order_key(od.order.perm_id, od.order_id));
            let (raw, filled, remaining, avg_px) = match joined {
                Some(s) => (
                    s.status.as_str(),
                    s.filled,
                    Some(s.remaining),
                    s.average_fill_price.unwrap_or(0.0),
                ),
                None => (od.order_state.status.as_str(), 0.0, None, 0.0),
            };
            let qty = if od.order.total_quantity > 0.0 {
                od.order.total_quantity
            } else {
                remaining.map_or(0.0, |r| filled + r)
            };
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                venue_order_id: od.order_id.to_string().into(),
                client_order_id: Some(od.order.order_ref.clone()).filter(|r| !r.is_empty()),
                side: if matches!(od.order.action, Action::Buy) { 1 } else { -1 },
                order_type: normalize_order_type(&od.order.order_type),
                qty,
                filled_qty: filled,
                avg_px,
                status: normalize_order_status(raw, filled, qty),
                ts: 0,
            }
        })
        .collect()
}

/// `executions` rows → `FillReport`, filtered to the EXEC client's id (`exec_client_id`, each row's
/// `client_id`), `account` and `conid`, stamped with the canonical `symbol`. `execution_id` →
/// `trade_id`; `order_id` → `venue_order_id`; `order_reference` → `client_order_id`; `Bought` → +1;
/// `shares`/`price` → qty/px; the commission is the `CommissionReport` with the same
/// `execution_id` (either order), else `0.0` in the contract's currency, the cpapi parser's
/// best-effort default. `last_liquidity` → maker/taker when IB said. `ts` is `read_ms` (module doc).
///
/// A row with no `execution_id` is SKIPPED, for the reason `crate::recon::cpapi::parse_fill_reports`
/// gives: an id-less report cannot match `seen_trade_ids`, so it would manufacture a `MissingFill`
/// that `hybrid` auto-applies.
pub fn parse_socket_fill_reports(
    rows: &[Executions],
    exec_client_id: i32,
    account: &str,
    conid: i32,
    symbol: &str,
    read_ms: i64,
) -> Vec<FillReport> {
    let mut commissions = HashMap::new();
    for row in rows {
        if let Executions::CommissionReport(c) = row {
            commissions.insert(c.execution_id.as_str(), c);
        }
    }
    let mut skipped = 0usize;
    let out: Vec<FillReport> = rows
        .iter()
        .filter_map(|row| match row {
            Executions::ExecutionData(ed) => Some(ed),
            Executions::CommissionReport(_) => None,
        })
        .filter(|ed| {
            ed.execution.client_id == exec_client_id
                && ed.contract.contract_id == conid
                && same_account(&ed.execution.account_number, account)
        })
        .filter_map(|ed| {
            let e = &ed.execution;
            let Ok(trade_id) = TradeId::new(&e.execution_id) else {
                skipped += 1;
                return None;
            };
            let report = commissions.get(e.execution_id.as_str());
            let commission_asset = report
                .map(|c| c.currency.clone())
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| ed.contract.currency.0.clone());
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                trade_id,
                venue_order_id: e.order_id.to_string().into(),
                client_order_id: Some(e.order_reference.clone()).filter(|r| !r.is_empty()),
                side: if matches!(e.side, ExecutionSide::Bought) { 1 } else { -1 },
                last_qty: e.shares,
                last_px: e.price,
                commission: report.map_or(0.0, |c| c.commission),
                commission_asset,
                liquidity_side: match e.last_liquidity {
                    Liquidity::AddedLiquidity => LiquiditySide::Maker,
                    Liquidity::RemovedLiquidity => LiquiditySide::Taker,
                    _ => LiquiditySide::Unknown,
                },
                ts: read_ms,
            })
        })
        .collect();
    if skipped > 0 {
        tracing::warn!(
            venue = VENUE,
            skipped,
            conid,
            "reconcile executions rows carry no `execution_id` — skipped; an id-less FillReport \
             cannot match `seen_trade_ids` and would manufacture a MissingFill divergence"
        );
    }
    out
}

/// `positions` rows → `PositionStatusReport`, filtered to `account` (exactly: a row must NAME it)
/// and `conid`, stamped with the canonical `symbol`. `position` is signed (net, one-way:
/// `PositionSide::Both`, as the cpapi parser reads it). `average_cost` is per CONTRACT, so it is
/// divided by the contract's multiplier when the row states one above zero (an option's cost is the
/// premium times 100).
///
/// When NO row matches, the answer is the FLAT zero row — the reconcile diff needs the row present
/// to see a stale local position the venue has closed (`crate::recon::cpapi::parse_position_reports`)
/// — but ONLY when that absence is believable: the read is empty (the login holds no position
/// anywhere), or some row carries `account`. A NON-empty read in which `account` never appears is
/// an `Err`: that is the shape of a wrong `ACCOUNT` (or a login that does not hold it), and a
/// synthesized flat would hand the diff a complete-looking report for an account nobody read. The
/// cost is a multi-account login whose configured account is flat while another holds positions:
/// it stays unreconciled (an `Err` each pass), never wrongly flat.
pub fn parse_socket_position_reports(
    rows: &[Position],
    account: &str,
    conid: i32,
    symbol: &str,
) -> Result<Vec<PositionStatusReport>, String> {
    if !rows.is_empty() && !rows.iter().any(|p| p.account == account) {
        return Err(format!(
            "ibkr positions: {} row(s) and none for the configured account; refusing to report \
             it flat",
            rows.len()
        ));
    }
    let matched: Vec<PositionStatusReport> = rows
        .iter()
        .filter(|p| p.contract.contract_id == conid && p.account == account)
        .map(|p| {
            let multiplier = p
                .contract
                .multiplier
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|m| *m > 0.0)
                .unwrap_or(1.0);
            PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                position_side: PositionSide::Both,
                qty: p.position,
                avg_px: p.average_cost / multiplier,
                ts: 0,
                margin_mode: vike_model::MarginMode::default(),
                isolated_margin: None,
                delta: None,
            }
        })
        .collect();
    Ok(if matched.is_empty() { vec![PositionStatusReport::flat(VENUE, symbol)] } else { matched })
}

/// The ledger tag whose value names a block's currency; in the `BASE` block it is the account's
/// base currency code in IB's documented `$LEDGER` vocabulary. OBSERVED 2026-10-20 (the demo login): it said `USD`
/// in the `USD` block and `BASE` in the `BASE` block, so there the `BASE` block declares no currency and is never
/// the seed.
const CURRENCY_TAG: &str = "Currency";

/// `account_summary` `$LEDGER:ALL` rows → this `account`'s `CashBalance` in `currency`: CASH
/// (settled and unsettled) in that one currency, NOT equity — no position value is in it. This is
/// the figure the BALANCE seed is written from.
///
/// The `BASE` block (every currency converted into the account's base currency) is read ONLY when
/// the `currency` block is absent AND the BASE block's `Currency` row names `currency` as the
/// base. Otherwise `Ok(None)`: no seed, because a converted total is not this currency's cash. A
/// matched row whose value is not a number is an `Err`, never a guess.
pub fn parse_socket_balance(
    rows: &[AccountSummary],
    account: &str,
    currency: &str,
) -> Result<Option<f64>, String> {
    let row = |tag: &str, ccy: &str| {
        rows.iter().find(|r| r.tag == tag && r.currency == ccy && r.account == account)
    };
    let base_is_currency = row(CURRENCY_TAG, "BASE").is_some_and(|r| {
        let named = r.value.trim();
        !named.eq_ignore_ascii_case("BASE") && named.eq_ignore_ascii_case(currency)
    });
    let found = row(CASH_BALANCE, currency)
        .or_else(|| if base_is_currency { row(CASH_BALANCE, "BASE") } else { None });
    match found {
        None => Ok(None),
        Some(row) => row.value.trim().parse::<f64>().map(Some).map_err(|e| {
            format!("ibkr ledger {CASH_BALANCE} ({}) is not a number: {e}", row.currency)
        }),
    }
}

/// Whether the login's `managed_accounts` include `account`: the connect-time refusal that keeps a
/// wrong `ACCOUNT` (or a login holding another account) from reconciling anything. The error
/// names how many accounts the login holds, never their numbers.
pub fn require_managed_account(managed: &[String], account: &str) -> Result<(), String> {
    if managed.iter().any(|m| m.trim() == account) {
        Ok(())
    } else {
        Err(format!(
            "the IB Gateway login does not manage the configured IBKR account ({} managed \
             account(s)); reconcile stays unwired rather than report another account's book",
            managed.len()
        ))
    }
}

/// The `account` [`parse_socket_balance`] reads the kept `$LEDGER:ALL` rows under first. The Gateway stamps EVERY row
/// of that ledger with the group the request names, [`LEDGER_GROUP`] (`All`), never the account's number (observed
/// 2026-10-20), and IB's page says it sums the ledger over every account of the login (IB's page, not a measurement
/// here). So the rows are this account's own only when the login manages just this one: then `All`, and only when the
/// one entry IS `account` (`trim()` equal, as [`require_managed_account`] compares). On a login that manages several
/// the figure would mix the accounts, so the account's own number is returned, which no row carries: the balance is
/// not reported, and the journal says so once, here (a pass answers "not reported" with no error and says nothing).
/// The warning names how many accounts the login holds, never their numbers. A blank entry in `managed` is no account.
pub fn ledger_account_for<'a>(managed: &[String], account: &'a str) -> &'a str {
    let held: Vec<&str> = managed.iter().map(|m| m.trim()).filter(|m| !m.is_empty()).collect();
    match held.as_slice() {
        [only] if *only == account => LEDGER_GROUP,
        [] | [_] => account,
        many => {
            tracing::warn!(
                target: "vike_ibkr::recon",
                "the IB Gateway login manages {} accounts, and IB's page says its cash ledger adds up over every one of \
                 them, so this account's own cash cannot be read from it: the IBKR balance is NOT adopted and the engine \
                 stays on its configured seed_cash (a login that manages only this account would be read)",
                many.len()
            );
            account
        }
    }
}

/// What [`LedgerBalance`] is built from at connect: the login's `managed_accounts` must include `account`
/// ([`require_managed_account`]), and the stamp the ledger rows are read under first is [`ledger_account_for`].
pub(crate) fn login_ledger_account(managed: &[String], account: &str) -> Result<String, String> {
    require_managed_account(managed, account)?;
    Ok(ledger_account_for(managed, account).to_string())
}

/// The reconcile connection's client id must differ from the exec's (`client_id`) and the data
/// connection's (`data_client_id`) on the one gateway: IB refuses a second connection under an id
/// already in use. (Ids must also be unique across EVERY account on that gateway — config.rs's
/// `IbkrConfig::recon_client_id` — which no single config can check.)
pub fn require_distinct_recon_id(cfg: &IbkrConfig) -> Result<(), String> {
    let clash = if cfg.recon_client_id == cfg.client_id {
        Some("the exec's (`CLIENT_ID`)")
    } else if cfg.recon_client_id == cfg.data_client_id {
        Some("the data connection's (`DATA_CLIENT_ID`)")
    } else {
        None
    };
    match clash {
        None => Ok(()),
        Some(whose) => Err(format!(
            "IBKR reconcile client id {} is {whose}; set `RECON_CLIENT_ID` to an id no other \
             connection on this gateway uses",
            cfg.recon_client_id
        )),
    }
}

// --- the instruments a pass reads ----------------------------------------------------------------

/// One instrument a pass reads: IB's contract id and the engine symbol its reports are stamped with.
pub type Target = (i32, String);

/// The instruments a pass reads: the mounted one FIRST, then every symbol the engine admitted, in the order
/// told, each resolved to its conId by `resolve`. A symbol whose conId is already in the list (two spellings
/// of one contract) is read once, under the first spelling. The first symbol that cannot be resolved FAILS
/// the plan: the restore planner reads an order a successful report lacks as gone, so a symbol that was not
/// read must never look like one that was.
pub fn plan_targets(
    mounted: (i32, &str),
    admitted: &[String],
    mut resolve: impl FnMut(&str) -> Result<i32, String>,
) -> Result<Vec<Target>, String> {
    let mut targets = vec![(mounted.0, mounted.1.to_string())];
    for symbol in admitted {
        let conid = resolve(symbol)?;
        if !targets.iter().any(|(c, _)| *c == conid) {
            targets.push((conid, symbol.clone()));
        }
    }
    Ok(targets)
}

/// [`parse_socket_order_reports`] for every target, out of the ONE account-wide read.
pub fn order_reports_for(
    rows: &[Orders],
    account: &str,
    targets: &[Target],
) -> Vec<OrderStatusReport> {
    targets
        .iter()
        .flat_map(|(conid, symbol)| parse_socket_order_reports(rows, account, *conid, symbol))
        .collect()
}

/// [`parse_socket_fill_reports`] for every target, out of the ONE account-wide read.
pub fn fill_reports_for(
    rows: &[Executions],
    exec_client_id: i32,
    account: &str,
    targets: &[Target],
    read_ms: i64,
) -> Vec<FillReport> {
    targets
        .iter()
        .flat_map(|(conid, symbol)| {
            parse_socket_fill_reports(rows, exec_client_id, account, *conid, symbol, read_ms)
        })
        .collect()
}

/// [`parse_socket_position_reports`] for every target, out of the ONE account-wide read: each target with no
/// row gets its own FLAT row, and a read that never names the account fails the whole answer.
pub fn position_reports_for(
    rows: &[Position],
    account: &str,
    targets: &[Target],
) -> Result<Vec<PositionStatusReport>, String> {
    let mut reports = Vec::new();
    for (conid, symbol) in targets {
        reports.extend(parse_socket_position_reports(rows, account, *conid, symbol)?);
    }
    Ok(reports)
}

/// The currency a canonical stock name carries (`TICKER.SMART.CCY`, upper case): its last dot-separated token,
/// when the name has the three tokens and that one is a three-letter code. Anything else names no currency.
fn stock_currency(name: &str) -> Option<&str> {
    let mut tokens = name.rsplit('.');
    let ccy = tokens.next()?;
    let has_exchange_and_ticker = tokens.count() >= 2;
    (has_exchange_and_ticker && ccy.len() == 3 && ccy.bytes().all(|b| b.is_ascii_alphabetic()))
        .then_some(ccy)
}

/// Whether any of the symbols the engine `admitted` is a stock quoted in another currency than
/// `mounted_currency`, the currency of the contract this client reads the cash of, OR a forex pair or a future
/// ([`crate::any_symbol_forex_future::moves_cash`]): then the engine's ledger holds a symbol whose position moves the
/// account's cash in a way the one-currency cash figure of [`IbkrSocketReconClient::fetch_balance`] does not follow
/// (MEASURED 2026-10-30 on the paper account: a 20000 EUR.USD buy took 22337.40 USD out of the USD cash row and put
/// 20000 into a EUR row, while the `BASE` total moved by the commission alone; a future settles its variation into
/// cash daily), and the figure is withheld.
fn holds_foreign_stock(admitted: &[String], mounted_currency: &str) -> bool {
    admitted.iter().any(|name| {
        crate::any_symbol_forex_future::moves_cash(name)
            || stock_currency(name).is_some_and(|ccy| !ccy.eq_ignore_ascii_case(mounted_currency))
    })
}

/// **The gate of [`IbkrSocketReconClient::fetch_balance`]**, a function so a test can drive it with no gateway:
/// the cash figure `read` returns is reported only while the engine holds no stock quoted in another currency than
/// the mount's ([`holds_foreign_stock`]); with one, `Ok(None)` ("not reported") and `read` is never called, so
/// nothing is spent on a figure that would be adopted into a ledger it does not count. `withheld` runs instead of
/// `read` in that case: it keeps the kept account-summary subscription folding its pushes, which no pass would drain
/// meanwhile, and CANCELS nothing, because a cancelled request still counts against the session's allowance of two
/// (`SummaryFeed::fold_only`). ⚠ `admitted` is what THIS client was told: it is empty in a fresh daemon until the node
/// tells it again, which the node covers by not asking for the balance meanwhile
/// (`vike_gatewayhub_client::trade::RECON_BALANCE_COUNTS_ADMITTED`).
pub(crate) fn balance_unless_foreign_stock(
    admitted: &[String],
    mounted_currency: &str,
    read: impl FnOnce() -> Result<Option<f64>, String>,
    withheld: impl FnOnce(),
) -> Result<Option<f64>, String> {
    if holds_foreign_stock(admitted, mounted_currency) {
        withheld();
        return Ok(None);
    }
    read()
}

/// **The reconcile balance of one connection**, generic over the gateway so a test drives the very code
/// [`IbkrSocketReconClient::fetch_balance`] runs: the kept `$LEDGER:ALL` feed ([`SummaryFeed::read`]), the foreign-stock
/// gate ([`balance_unless_foreign_stock`]) and the stamps the rows are read under.
///
/// The mounted currency's cash is read under the stamp [`ledger_account_for`] picked (`All` on a login that manages
/// just this account) and, when that finds nothing there, under the account's own number (NOT VERIFIED that any login
/// stamps its rows so; the cost of trying is nil). ⚠ A feed with rows of which none carries either stamp is the shape
/// of the original silent failure (every pass answered "not reported" with no error), so it is said ONCE per
/// connection, in the journal.
pub(crate) struct LedgerBalance<G: SummaryGateway> {
    feed: SummaryFeed<G>,
    /// The configured account's own number.
    account: String,
    /// The stamp read first ([`login_ledger_account`]): `All` on a one-account login, else `account`.
    ledger_account: String,
    /// The mounted contract's currency: the cash row asked for.
    currency: String,
    /// Whether the journal has said that no row carries either stamp (once per connection).
    unmatched_said: AtomicBool,
}

impl<G: SummaryGateway> LedgerBalance<G> {
    /// `managed` is the login's `managed_accounts`: it must include `account` ([`login_ledger_account`]).
    pub(crate) fn open(
        feed: SummaryFeed<G>,
        managed: &[String],
        account: &str,
        currency: String,
    ) -> Result<Self, String> {
        Ok(Self {
            feed,
            account: account.to_string(),
            ledger_account: login_ledger_account(managed, account)?,
            currency,
            unmatched_said: AtomicBool::new(false),
        })
    }

    /// Whether this connection's session has no request left and none open ([`SummaryFeed::spent`]): the connection can
    /// serve no balance again and is to be replaced.
    pub(crate) fn spent(&self) -> bool {
        self.feed.spent()
    }

    /// The cash figure for one pass: `None` ("not reported") and nothing read once `admitted` holds a stock in another
    /// currency than the mount's ([`balance_unless_foreign_stock`]), else the ledger's.
    pub(crate) fn fetch(&self, admitted: &[String]) -> Result<Option<f64>, String> {
        balance_unless_foreign_stock(
            admitted,
            &self.currency,
            || self.read(),
            || self.feed.fold_only(),
        )
    }

    fn read(&self) -> Result<Option<f64>, String> {
        let rows = self.feed.read()?;
        let all = self.ledger_account != self.account;
        if all
            && !rows.is_empty()
            && !rows.iter().any(|r| r.account == self.ledger_account || r.account == self.account)
            && !self.unmatched_said.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                target: "vike_ibkr::recon",
                "the IB Gateway answered the account summary with {} rows and none is stamped `{}` or with this \
                 account's own number, so no cash balance can be read from it: the IBKR balance is NOT adopted (every \
                 pass answers \"not reported\", no error) and the engine stays on its configured seed_cash. Said once \
                 per connection",
                rows.len(),
                self.ledger_account
            );
        }
        match parse_socket_balance(&rows, &self.ledger_account, &self.currency)? {
            None if all => parse_socket_balance(&rows, &self.account, &self.currency),
            found => Ok(found),
        }
    }
}

// --- the client ----------------------------------------------------------------------------------

/// One socket reconcile client per (account, canonical vike symbol): its own ibapi connection on
/// `recon_client_id`, the symbol's pinned conId, and the exec client id whose fills it reports. It also
/// reads every symbol the engine admitted on demand (`ReconClient::admit_symbol`, decision 0173), each
/// pinned to its conId on first use.
pub struct IbkrSocketReconClient {
    /// The connection, replaced when its summary allowance is spent (the module doc, "REPLACED").
    conn: Mutex<Conn>,
    /// What a re-dial dials: the gateway, the ids, the account.
    cfg: IbkrConfig,
    /// The mounted contract's currency: the cash row the balance is read for, on every connection.
    currency: String,
    account: String,
    exec_client_id: i32,
    symbol: String,
    conid: i32,
    /// The symbols the engine admitted (the engine's spelling, upper case).
    admitted: AdmittedSymbols,
    /// Each admitted symbol's conId, resolved once.
    resolved: Mutex<HashMap<String, i32>>,
}

/// One reconcile CONNECTION, which is one SESSION to the Gateway: the ibapi client under `recon_client_id` and the ONE
/// `$LEDGER:ALL` account-summary subscription kept on it (`crate::recon::summary`) with the stamps its rows are read under.
struct Link {
    /// Declared before `client` so the subscription is dropped (cancelled) while the client is still up.
    balance: LedgerBalance<ClientGateway>,
    client: Arc<Client>,
}

/// When the reconcile connection may be dialled again: [`REDIAL_GAP`] after a dial that worked, `crate::redial::backoff`
/// (1, 2, then 5 minutes) after each that failed in a row.
#[derive(Debug, Default)]
struct Pace {
    next_ok: Option<Instant>,
    /// Failed dials since the last one that worked.
    failures: u32,
}

impl Pace {
    /// `Err(wait)` while the schedule does not allow a dial at `now`.
    fn due(&self, now: Instant) -> Result<(), Duration> {
        match self.next_ok {
            Some(at) if now < at => Err(at - now),
            _ => Ok(()),
        }
    }

    /// A dial ended at `now`, `worked` or not.
    fn dialled(&mut self, now: Instant, worked: bool) {
        if worked {
            self.failures = 0;
            self.next_ok = Some(now + REDIAL_GAP);
        } else {
            self.failures = self.failures.saturating_add(1);
            self.next_ok = Some(now + backoff(self.failures));
        }
    }
}

/// What the client's mutex guards: the connection (`None` between a close and a re-dial that worked), the schedule of
/// the next dial and the last dial's error.
struct Conn {
    link: Option<Arc<Link>>,
    pace: Pace,
    last_err: String,
}

/// Dial and CHECK one reconcile connection: the notice stream is bound BEFORE the connect (as the exec's is), the login
/// must manage `cfg.account` ([`login_ledger_account`], refused BEFORE anything is asked), and the kept summary feed is
/// built over it (building it sends nothing: the first pass opens the subscription). Blocking, with no timeout of its
/// own: callers run it inside `GatewayGate::dial`.
fn connect_link(cfg: &IbkrConfig, currency: &str) -> Result<Link, IbkrError> {
    let addr = format!("{}:{}", cfg.host, cfg.port);
    // `ibapi` delivers `[1101]` (data lost, so the kept account-summary subscription is void) only to a stream bound
    // before the connect.
    let (client, notices) = Client::builder()
        .address(addr.clone())
        .client_id(cfg.recon_client_id)
        .connect_with_notice_stream()
        .map_err(|e| {
            IbkrError::Connect(format!("recon {addr} (client_id={}): {e}", cfg.recon_client_id))
        })?;
    let managed = client
        .managed_accounts()
        .map_err(|e| IbkrError::Connect(format!("recon managed_accounts: {e}")))?;
    let client = Arc::new(client);
    let balance = LedgerBalance::open(
        SummaryFeed::new(ClientGateway::new(Arc::clone(&client), notices)),
        &managed,
        &cfg.account,
        currency.to_string(),
    )
    .map_err(IbkrError::Unsupported)?;
    Ok(Link { balance, client })
}

impl IbkrSocketReconClient {
    /// Connect to the exec's gateway (`cfg.host:cfg.port`) under `cfg.recon_client_id`, require the
    /// login to manage `cfg.account` ([`require_managed_account`]) and pin `symbol`'s conId with
    /// `contract_details`. Refused: a cpapi config (its reconcile is
    /// `crate::recon::cpapi::IbkrReconClient`, on the CP Gateway session), an unmappable symbol, a
    /// recon client id equal to the exec's or the data connection's ([`require_distinct_recon_id`]:
    /// the gateway would refuse the second connection), an unreachable gateway, an account the
    /// login does not manage, and a conId that is absent or not unique.
    pub fn connect(cfg: &IbkrConfig, symbol: &str) -> Result<IbkrSocketReconClient, IbkrError> {
        Self::connect_within(cfg, symbol, OPEN_DEADLINE)
    }

    /// [`Self::connect`] with the network half (the dial, `managed_accounts`, `contract_details`: `ibapi`
    /// waits with no timeout of their own) under a `within` bound
    /// (`GatewayGate::dial`); the refusals above it dial nothing and stay unbounded.
    pub(crate) fn connect_within(
        cfg: &IbkrConfig,
        symbol: &str,
        within: Duration,
    ) -> Result<IbkrSocketReconClient, IbkrError> {
        if cfg.backend == IbkrBackend::Cpapi {
            return Err(IbkrError::Unsupported(
                "the socket reconcile client does not dial a cpapi config: its reconcile is the \
                 Client Portal one, on the exec's own CP Gateway session"
                    .to_string(),
            ));
        }
        let contract = parse_simplified(symbol).map_err(IbkrError::Unsupported)?;
        require_distinct_recon_id(cfg).map_err(IbkrError::Unsupported)?;
        let (cfg, symbol) = (cfg.clone(), symbol.to_string());
        // One dial at a time per gateway, spaced after the previous one (`GatewayGate::dial`).
        let gate = gate_for(&format!("{}:{}", cfg.host, cfg.port));
        gate.dial("recon connect", within, move || {
            // Refuses an account the login does not manage BEFORE `contract_details` is asked.
            let link = connect_link(&cfg, &contract.currency)?;
            let details = link
                .client
                .contract_details(&build_contract(&contract))
                .map_err(|e| IbkrError::Connect(format!("recon contract_details {symbol}: {e}")))?;
            let conid = resolve_conid(&details, &symbol).map_err(IbkrError::Unsupported)?;
            Ok(IbkrSocketReconClient {
                conn: Mutex::new(Conn {
                    link: Some(Arc::new(link)),
                    pace: Pace::default(),
                    last_err: String::new(),
                }),
                account: cfg.account.clone(),
                exec_client_id: cfg.client_id,
                currency: contract.currency.clone(),
                cfg,
                symbol,
                conid,
                admitted: AdmittedSymbols::default(),
                resolved: Mutex::new(HashMap::new()),
            })
        })
        .0
    }

    /// The connection now in use, or a re-dial of a lost one when its schedule allows ([`Pace`]).
    fn link(&self) -> Result<Arc<Link>, String> {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        if conn.link.is_none() {
            self.dial(&mut conn)?;
        }
        conn.link.clone().ok_or_else(|| LOST_WORDS.to_string())
    }

    /// The ibapi client of the connection now in use ([`Self::link`]).
    fn client(&self) -> Result<Arc<Client>, String> {
        Ok(Arc::clone(&self.link()?.client))
    }

    /// One re-dial of the reconcile connection, through the gate's turn (after the previous dial, spaced by its gap,
    /// bounded by [`OPEN_DEADLINE`]) and on [`Pace`]'s schedule. On `Ok` `conn.link` is the new connection. Not made
    /// while the gate's rebuild schedule of a lost gateway link is armed: that schedule is the link watchdog's.
    fn dial(&self, conn: &mut Conn) -> Result<(), String> {
        let lost = |why: &str| format!("{LOST_WORDS} ({why})");
        if conn.pace.due(Instant::now()).is_err() {
            return Err(lost(&conn.last_err));
        }
        let gate = gate_for(&format!("{}:{}", self.cfg.host, self.cfg.port));
        if gate.permit_data().is_err() {
            return Err(lost("the Gateway has no link to IBKR and its rebuild is on a schedule"));
        }
        let (cfg, currency) = (self.cfg.clone(), self.currency.clone());
        let (dialled, _) =
            gate.dial("recon re-dial", OPEN_DEADLINE, move || connect_link(&cfg, &currency));
        match dialled {
            Ok(link) => {
                conn.pace.dialled(Instant::now(), true);
                conn.last_err.clear();
                conn.link = Some(Arc::new(link));
                tracing::info!(
                    target: "vike_ibkr::recon",
                    "reconcile connection re-dialled: a new session, so the account-summary allowance starts again"
                );
                Ok(())
            }
            Err(e) => {
                conn.pace.dialled(Instant::now(), false);
                conn.last_err = e.to_string();
                tracing::warn!(
                    target: "vike_ibkr::recon", error = %e,
                    "reconcile connection re-dial failed; reads fail until the next try (1, 2, then 5 minutes)"
                );
                Err(lost(&conn.last_err))
            }
        }
    }

    /// A pass found the summary feed of `spent` with no request left ([`LedgerBalance::spent`]): replace its whole
    /// connection, so the NEXT pass opens the first request of a new session. Does nothing when another pass already
    /// replaced it, when [`Pace`] says a dial is not due yet, or while the gateway's rebuild schedule is armed. The old
    /// client is CLOSED first (IB refuses a second client under an id in use, error 326; `Client::disconnect` ends it
    /// even while a read thread an earlier deadline abandoned still holds the client), and a re-dial that then fails
    /// leaves the client without a connection until [`Self::link`]'s next try.
    fn rebuild_spent(&self, spent: &Arc<Link>) {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        if !conn.link.as_ref().is_some_and(|current| Arc::ptr_eq(current, spent)) {
            return;
        }
        let gate = gate_for(&format!("{}:{}", self.cfg.host, self.cfg.port));
        if conn.pace.due(Instant::now()).is_err() || gate.permit_data().is_err() {
            return;
        }
        tracing::warn!(
            target: "vike_ibkr::recon",
            "reconcile connection: the account-summary allowance of this session is spent; closing it and dialling a new session"
        );
        let old = Arc::clone(&spent.client);
        let closed =
            bounded("recon disconnect", CLOSE_DEADLINE, Some(gate.settling()), move || {
                old.disconnect();
                Ok(())
            });
        if let Err(e) = closed {
            tracing::warn!(target: "vike_ibkr::recon", error = %e, "closing the spent reconcile connection did not return in time");
        }
        conn.link = None;
        // The failure, if any, is the next read's to report ([`LOST_WORDS`]); this pass answers its own.
        let _ = self.dial(&mut conn);
    }

    /// The stamp the ledger rows are read under first ([`ledger_account_for`]): `All` on a login that manages only the
    /// configured account, else the account's own number (then the balance is reported as none, by design). The live
    /// smoke reads it to know whether a balance must be there. Empty while the connection is closed for a re-dial.
    pub fn ledger_account(&self) -> String {
        self.link().map(|link| link.balance.ledger_account.clone()).unwrap_or_default()
    }

    /// The executions IB shows THIS connection today, counted per placing client id, with NO
    /// client filter: the read-only probe for the module doc's question 1. A map holding the exec
    /// client id after a fill means a non-master client id sees the exec's executions; a map
    /// holding only other ids (or none) after one means it does not.
    pub fn executions_by_client_id(&self) -> Result<BTreeMap<i32, usize>, String> {
        let filter = ExecutionFilter { account_code: self.account.clone(), ..Default::default() };
        let rows = self.executions(filter)?;
        let mut seen = BTreeMap::new();
        for row in &rows {
            if let Executions::ExecutionData(ed) = row {
                *seen.entry(ed.execution.client_id).or_insert(0) += 1;
            }
        }
        Ok(seen)
    }

    fn executions(&self, filter: ExecutionFilter) -> Result<Vec<Executions>, String> {
        let client = self.client()?;
        let sub = client.executions(filter).map_err(|e| format!("ibkr executions: {e}"))?;
        drain(|t| sub.next_timeout(t), READ_DEADLINE, "executions", EndsBy::Close, |_| false)
    }

    /// The instruments the next pass reads ([`plan_targets`]): the mounted one, then each admitted one.
    fn targets(&self) -> Result<Vec<Target>, String> {
        plan_targets((self.conid, &self.symbol), &self.admitted.symbols(), |symbol| {
            self.conid_of(symbol)
        })
    }

    /// The conId of an admitted `symbol`, asked of the gateway on first use (`admit_symbol` never touches the
    /// network) and kept. Bounded like every read here: `ibapi`'s `contract_details` wait has no timeout of its
    /// own, and the thread of one the deadline won is abandoned.
    fn conid_of(&self, symbol: &str) -> Result<i32, String> {
        let known =
            self.resolved.lock().unwrap_or_else(PoisonError::into_inner).get(symbol).copied();
        if let Some(conid) = known {
            return Ok(conid);
        }
        let contract = parse_simplified(symbol)?;
        let (client, asked) = (self.client()?, build_contract(&contract));
        let details =
            bounded(&format!("recon contract_details {symbol}"), READ_DEADLINE, None, move || {
                client
                    .contract_details(&asked)
                    .map_err(|e| IbkrError::DataUnavailable(e.to_string()))
            })
            .map_err(|e| e.to_string())?;
        let conid = resolve_conid(&details, symbol)?;
        self.resolved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(symbol.to_string(), conid);
        Ok(conid)
    }
}

impl ReconClient for IbkrSocketReconClient {
    /// `_since` is a no-op: `all_open_orders` answers the CURRENTLY open set. ONE account-wide read, parsed
    /// per instrument the pass reads (the mounted one and each admitted one); a symbol that cannot be pinned
    /// fails the whole fetch.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let targets = self.targets()?;
        let client = self.client()?;
        let sub = client.all_open_orders().map_err(|e| format!("ibkr open orders: {e}"))?;
        let rows =
            drain(|t| sub.next_timeout(t), READ_DEADLINE, "open orders", EndsBy::Close, |_| false)?;
        Ok(order_reports_for(&rows, &self.account, &targets))
    }

    /// `_since` is a no-op: IB serves only the current day's executions.
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        let targets = self.targets()?;
        let filter = ExecutionFilter {
            client_id: Some(self.exec_client_id),
            account_code: self.account.clone(),
            ..Default::default()
        };
        let rows = self.executions(filter)?;
        Ok(fill_reports_for(
            &rows,
            self.exec_client_id,
            &self.account,
            &targets,
            vike_model::now_ms(),
        ))
    }

    /// Never empty: an absent symbol is the flat row (`parse_socket_position_reports`), one per instrument the
    /// pass reads.
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let targets = self.targets()?;
        let client = self.client()?;
        let sub = client.positions().map_err(|e| format!("ibkr positions: {e}"))?;
        let rows = drain(
            |t| sub.next_timeout(t),
            READ_DEADLINE,
            "positions",
            EndsBy::Marker,
            |u| matches!(u, PositionUpdate::PositionEnd),
        )?;
        let positions: Vec<Position> = rows
            .into_iter()
            .filter_map(|u| match u {
                PositionUpdate::Position(p) => Some(p),
                PositionUpdate::PositionEnd => None,
            })
            .collect();
        position_reports_for(&positions, &self.account, &targets)
    }

    /// The engine admitted `symbol` (any-symbol, decision 0173): every later pass reads its orders, fills and
    /// position too, pinned to its conId on the first pass. Idempotent, never touches the network; the list
    /// is bounded by the engine's own cap and keeps the engine's spelling (upper-cased).
    fn admit_symbol(&self, symbol: &str) {
        if self.admitted.admit(symbol, &self.symbol) {
            tracing::info!(
                target: "vike_ibkr::recon", symbol = %symbol.to_ascii_uppercase(),
                "reconcile: the engine admitted a symbol; every pass now reads it too"
            );
        }
    }

    /// The engine gave `symbol` back (decision 0205): the next pass stops reading it.
    fn forget_symbol(&self, symbol: &str) {
        if self.admitted.forget(symbol) {
            tracing::info!(
                target: "vike_ibkr::recon", symbol = %symbol.to_ascii_uppercase(),
                "reconcile: the engine gave a symbol back; no pass reads it any more"
            );
        }
    }

    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    /// The mounted contract's currency cash (`parse_socket_balance`), read under the stamp the Gateway puts on the
    /// ledger rows (`All`, on a login that manages only this account: `ledger_account_for`), else under the account's own
    /// number: all of it in `LedgerBalance`. **Not reported once the engine holds a
    /// stock quoted in another currency** (`holds_foreign_stock`): the figure is one currency's `CashBalance` row, and a
    /// foreign stock settles in ITS currency's cash, so the cash the ledger counts in (the account's) is no longer
    /// this row alone. Answered `None` ("not reported") BEFORE any read, so the pass folds no balance and raises no
    /// drift, and nothing is spent. The core is venue-free (it adopts a venue's balance for a foreign-quoted ledger,
    /// the owner's ruling of 2026-10-19); this client withholds ITSELF, off the symbols it was told the engine
    /// admitted ([`ReconClient::admit_symbol`]).
    ///
    /// **Read from ONE kept `$LEDGER:ALL` subscription** (`crate::recon::summary`), not a request per pass: the
    /// Gateway answered a burst of `[322] Maximum number of account summary requests exceeded` at its nightly `[1102]`
    /// for as many requests as the connection had ever made, and it allows a SESSION two requests in all, a cancelled
    /// one included. The first pass opens it (and waits for its dump); every later pass folds what IB pushed meanwhile
    /// and answers at once, however quiet the account. While the figure is withheld the subscription is kept and
    /// folded, never cancelled. A session whose two requests are spent is replaced by a new connection
    /// (`rebuild_spent`), after the pass that found it spent.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let link = self.link()?;
        let got = link.balance.fetch(&self.admitted.symbols());
        // A session with no summary request left can serve no balance again (`SPENT_WORDS`): replace the connection,
        // so the NEXT pass opens the first request of a new session. This pass has no figure either way.
        if got.is_err() && link.balance.spent() {
            self.rebuild_spent(&link);
        }
        got
    }
}

// --- the factory ---------------------------------------------------------------------------------

/// The socket twin of `crate::recon::cpapi::recon_client`: `None` on any connect failure (one
/// `warn!`, exec unaffected), so the venue-symbol stays unreconciled. Wired by `crate::adapter`'s
/// `start_with` for a socket (or `oauth`, which dials the socket today) config.
pub fn socket_recon_client(cfg: &IbkrConfig, symbol: &str) -> Option<Box<dyn ReconClient>> {
    match IbkrSocketReconClient::connect(cfg, symbol) {
        Ok(c) => Some(Box::new(c) as Box<dyn ReconClient>),
        Err(e) => {
            tracing::warn!(target: "vike_ibkr::recon", symbol, error = %e, "IBKR socket recon client unwired");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    type Item = Option<Result<SubscriptionItem<u32>, ibapi::Error>>;

    /// A scripted subscription: each call pops the next answer; an exhausted script is a close.
    fn script(items: Vec<Item>) -> impl FnMut(Duration) -> Item {
        let mut q: VecDeque<Item> = items.into();
        move |_| q.pop_front().flatten()
    }

    fn data(v: u32) -> Item {
        Some(Ok(SubscriptionItem::Data(v)))
    }

    /// **The client withholds its own cash figure once the engine holds a stock in another currency** (the core is
    /// venue-free and adopts a venue's balance for a foreign-quoted ledger; ibkr's figure is ONE currency's
    /// `CashBalance` row of the mounted contract, and a foreign stock settles in its own currency's cash): off the
    /// symbols the engine admitted, a stock whose currency token is not the mount's money withholds; the mount's own
    /// money, the empty list and a name that carries no currency do not.
    #[test]
    fn a_stock_in_another_currency_withholds_the_cash_figure() {
        let names = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(!holds_foreign_stock(&[], "USD"));
        assert!(!holds_foreign_stock(&names(&["AAPL.SMART.USD", "MSFT.SMART.USD"]), "USD"));
        assert!(holds_foreign_stock(&names(&["AAPL.SMART.USD", "SAP.SMART.EUR"]), "USD"));
        assert!(
            holds_foreign_stock(&names(&["AAPL.SMART.USD"]), "EUR"),
            "the cash row is the MOUNT's money, whatever the account's base"
        );
        assert!(!holds_foreign_stock(&names(&["SAP.SMART.EUR"]), "eur"), "case does not matter");
        for odd in ["IBM", "BRK.B", "EUR.USD", "AAPL.SMART", ""] {
            assert!(!holds_foreign_stock(&names(&[odd]), "USD"), "{odd:?} names no currency");
        }
        assert_eq!(stock_currency("BRK.B.SMART.USD"), Some("USD"));
    }

    /// **A forex pair or a future withholds the cash figure too, in the mount's own money** (decision 0234): the
    /// captured paper account's USD row fell by the whole notional of a EUR.USD buy while the account's `BASE` total
    /// did not, so the one-currency figure would be a false `BalanceDrift` for as long as the pair is held. A pair
    /// quoted in the mount's money (EUR.USD on a USD mount) is the case a currency token cannot see.
    #[test]
    fn a_forex_pair_or_a_future_withholds_the_cash_figure_in_the_mounts_own_money() {
        let names = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        for held in [
            "EUR.USD.IDEALPRO",
            "EUR.IDEALPRO.USD",
            "ES.CME.USD.FUT.20261218",
            "MES.CME.USD.FUT.20261218.5",
        ] {
            assert!(holds_foreign_stock(&names(&["AAPL.SMART.USD", held]), "USD"), "{held}");
        }
        assert!(!holds_foreign_stock(
            &names(&["AAPL.SMART.USD", "SPY.SMART.USD.OPT.20261218.500.C"]),
            "USD"
        ));
    }

    /// **`fetch_balance`'s gate, driven** (review 2, MEDIUM-4: the test above touched only the helpers, so deleting the
    /// `if` left every test green): with a stock in another currency admitted the figure is not reported AND the read
    /// is never made; with an empty list, or only the mount's own money, the read is the answer; a failed read
    /// stays a failure. The gate trusts the list it is handed, which is why the node does not ask a client for the
    /// balance while it owes that client symbols (see the gatewayhub client's `told_symbols` tests). A withheld pass
    /// runs `withheld` (the kept subscription keeps folding) and a served one never does.
    #[test]
    fn the_balance_gate_reads_nothing_once_a_foreign_stock_is_admitted() {
        let names = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let reads = std::cell::Cell::new(0_u32);
        let withholds = std::cell::Cell::new(0_u32);
        let read = || {
            reads.set(reads.get() + 1);
            Ok(Some(25_000.0))
        };
        let withheld = || withholds.set(withholds.get() + 1);
        assert_eq!(
            balance_unless_foreign_stock(
                &names(&["AAPL.SMART.USD", "SAP.SMART.EUR"]),
                "USD",
                read,
                withheld
            ),
            Ok(None),
            "a foreign stock withholds the figure"
        );
        assert_eq!(reads.get(), 0, "...and nothing is read for it");
        assert_eq!(withholds.get(), 1, "...and the withhold hook ran");
        assert_eq!(balance_unless_foreign_stock(&[], "USD", read, withheld), Ok(Some(25_000.0)));
        assert_eq!(
            balance_unless_foreign_stock(&names(&["MSFT.SMART.USD"]), "USD", read, withheld),
            Ok(Some(25_000.0)),
            "the mount's own money withholds nothing"
        );
        assert_eq!(reads.get(), 2);
        assert_eq!(withholds.get(), 1, "a served pass runs no withhold hook");
        assert_eq!(
            balance_unless_foreign_stock(
                &[],
                "USD",
                || Err("ibkr account summary: down".into()),
                withheld
            ),
            Err("ibkr account summary: down".to_string())
        );
        assert_eq!(withholds.get(), 1, "a failed read runs no withhold hook");
    }

    /// A close-ended stream (open orders, executions) that closes in time answers what it sent.
    #[test]
    fn a_close_ended_stream_that_closes_answers_its_rows() {
        let got =
            drain(script(vec![data(1), data(2)]), READ_DEADLINE, "t", EndsBy::Close, |_| false);
        assert_eq!(got, Ok(vec![1, 2]));
    }

    /// A marker-ended stream (positions, summary) stops AT its marker and drops it; a close before
    /// the marker is a fault, not a short list.
    #[test]
    fn a_marker_ended_stream_needs_its_marker() {
        let got = drain(
            script(vec![data(1), data(0), data(9)]),
            READ_DEADLINE,
            "t",
            EndsBy::Marker,
            |v| *v == 0,
        );
        assert_eq!(got, Ok(vec![1]));
        let cut = drain(script(vec![data(1)]), READ_DEADLINE, "t", EndsBy::Marker, |v| *v == 0);
        assert!(cut.unwrap_err().contains("before its end marker"));
    }

    /// An error mid-stream is the read's error; the rows before it are not reported.
    #[test]
    fn an_error_mid_stream_fails_the_read() {
        let got = drain(
            script(vec![data(1), Some(Err(ibapi::Error::NotImplemented))]),
            READ_DEADLINE,
            "t",
            EndsBy::Close,
            |_| false,
        );
        assert!(got.is_err());
    }

    /// A stream that never ends within the deadline is a timeout `Err`, for both end kinds: the
    /// fake waits out the time it is given and answers nothing, exactly as `next_timeout` does.
    #[test]
    fn a_stream_that_never_ends_times_out() {
        for ends_by in [EndsBy::Close, EndsBy::Marker] {
            let got = drain(
                |t: Duration| -> Item {
                    std::thread::sleep(t);
                    None
                },
                Duration::from_millis(20),
                "t",
                ends_by,
                |_| false,
            );
            let err = got.unwrap_err();
            assert!(err.contains("within") || err.contains("end marker"), "{ends_by:?}: {err}");
        }
    }

    /// A gateway that takes the connection and never answers: the reconcile connect returns a typed
    /// `Connect` error within the bound (the factory then leaves reconcile unwired; the open goes on).
    #[test]
    fn the_recon_connect_gives_up_on_a_silent_gateway() {
        use crate::transport::socket::socket_tests::{QUICK, TEST_BOUND, silent_gateway};
        let (_gateway, cfg) = silent_gateway();
        let began = Instant::now();
        let got = IbkrSocketReconClient::connect_within(&cfg, "AAPL.SMART.USD", TEST_BOUND);
        assert!(matches!(got, Err(IbkrError::Connect(_))), "a silent gateway must not connect");
        assert!(began.elapsed() < QUICK, "waited {:?}", began.elapsed());
    }

    /// A cpapi config is refused before any socket is opened, and so is a recon client id equal to
    /// the exec's (the port is dead, so a test that got past the guards would fail to connect).
    #[test]
    fn the_guards_refuse_before_dialling() {
        let mut cfg = IbkrConfig {
            tier: vike_model::AccountTier::Demo,
            backend: IbkrBackend::Cpapi,
            host: "127.0.0.1".into(),
            port: 1,
            client_id: 1,
            account: "TEST".into(),
            cpapi_url: "https://127.0.0.1:1".into(),
            mktdata_type: crate::config::MktDataType::default(),
            data_client_id: 2,
            recon_client_id: 3,
            extended_hours: false,
        };
        let refused = |cfg: &IbkrConfig| match IbkrSocketReconClient::connect(cfg, "AAPL.SMART.USD")
        {
            Err(IbkrError::Unsupported(msg)) => msg,
            Err(e) => panic!("refused for the wrong reason: {e}"),
            Ok(_) => panic!("connected past a guard"),
        };
        assert!(refused(&cfg).contains("cpapi"));
        cfg.backend = IbkrBackend::Socket;
        cfg.recon_client_id = cfg.client_id;
        assert!(refused(&cfg).contains("the exec's"));
        cfg.recon_client_id = cfg.data_client_id;
        assert!(refused(&cfg).contains("the data connection's"));
    }

    /// The account check: a login managing the configured account passes; one that does not (a
    /// wrong `ACCOUNT`, or a login holding another account) is refused, and the refusal names no
    /// account number.
    #[test]
    fn the_configured_account_must_be_managed_by_the_login() {
        let managed = vec!["DU1111111".to_string(), " DU2222222 ".to_string()];
        assert!(require_managed_account(&managed, "DU2222222").is_ok(), "trimmed");
        let err = require_managed_account(&managed, "DU9999999").unwrap_err();
        assert!(err.contains("2 managed"), "{err}");
        assert!(!err.contains("DU1111111") && !err.contains("DU9999999"), "{err}");
        assert!(require_managed_account(&[], "DU1111111").is_err());
    }

    /// **A login that manages several accounts says so ONCE, in plain words, and a one-account login says nothing**:
    /// the first is the only sign an operator gets that the IBKR balance is not adopted (the pass answers "not
    /// reported" with no error). The line names how many accounts the login holds, never their numbers.
    #[test]
    fn a_multi_account_login_journals_one_plain_line() {
        use tracing::Level;
        use vike_log::capture::captured;

        let two = vec!["DU1111111".to_string(), "DU2222222".to_string()];
        let (under, events) = captured(|| ledger_account_for(&two, "DU1111111"));
        assert_eq!(under, "DU1111111");
        let said: Vec<_> = events.iter().filter(|e| e.level == Level::WARN).collect();
        assert_eq!(said.len(), 1, "{events:?}");
        let words = &said[0].message;
        assert!(words.contains("manages 2 accounts") && words.contains("NOT adopted"), "{words}");
        assert!(!words.contains("DU1111111") && !words.contains("DU2222222"), "{words}");

        let one = vec!["DU1111111".to_string()];
        let (under, events) = captured(|| ledger_account_for(&one, "DU1111111"));
        assert_eq!(under, "All");
        assert!(events.is_empty(), "{events:?}");
    }

    /// **`All` only when the one entry IS the account** (`ledger_account_for` is public and `require_managed_account` is
    /// not its caller's only guard): a single other account is not `All`, and neither is an empty list; blanks around
    /// the entry do not matter.
    #[test]
    fn all_is_the_stamp_only_when_the_one_managed_account_is_the_configured_one() {
        let held = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(ledger_account_for(&held(&["DU9999999"]), "DU1111111"), "DU1111111");
        assert_eq!(ledger_account_for(&held(&[]), "DU1111111"), "DU1111111");
        assert_eq!(ledger_account_for(&held(&[" DU1111111 ", ""]), "DU1111111"), "All");
    }

    /// **The re-dial of the reconcile connection is paced** (A33): free before the first dial, a minute after one that
    /// worked, and 1, 2, then 5 minutes (`crate::redial::backoff`, the gateway's one table) after each failure in a row,
    /// the schedule starting again from the first step once a dial works. Driven with a clock, no gateway.
    #[test]
    fn the_redial_waits_a_minute_after_a_good_one_and_backs_off_after_failures() {
        let t0 = Instant::now();
        let mut pace = Pace::default();
        assert_eq!(pace.due(t0), Ok(()), "never dialled: due now");

        pace.dialled(t0, true);
        assert_eq!(pace.due(t0 + REDIAL_GAP - Duration::from_secs(1)), Err(Duration::from_secs(1)));
        assert_eq!(pace.due(t0 + REDIAL_GAP), Ok(()), "a minute after a dial that worked");

        for (failures, wait) in [(1, 60), (2, 120), (3, 300), (4, 300), (5, 300)] {
            pace.dialled(t0, false);
            assert_eq!(pace.failures, failures);
            let wait = Duration::from_secs(wait);
            assert_eq!(pace.due(t0), Err(wait), "failure {failures}: not before {wait:?}");
            assert_eq!(pace.due(t0 + wait), Ok(()), "failure {failures}: due after {wait:?}");
        }

        pace.dialled(t0, true);
        assert_eq!(pace.failures, 0, "a dial that works clears the failures");
        pace.dialled(t0, false);
        assert_eq!(pace.due(t0), Err(Duration::from_secs(60)), "and the schedule starts over");
    }

    /// **The connect-time pair** (`connect_within` builds `LedgerBalance` from it): a login that does not manage the
    /// account is an `Err` that names no number, a one-account login is `All`, a several-account one is the account's
    /// own number.
    #[test]
    fn the_login_decides_the_ledger_stamp_or_refuses_the_account() {
        let held = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let err = login_ledger_account(&held(&["DU2222222"]), "DU1111111").unwrap_err();
        assert!(err.contains("does not manage") && err.contains("1 managed"), "{err}");
        assert!(!err.contains("DU1111111") && !err.contains("DU2222222"), "{err}");
        assert!(login_ledger_account(&[], "DU1111111").is_err());
        assert_eq!(login_ledger_account(&held(&["DU1111111"]), "DU1111111"), Ok("All".to_string()));
        assert_eq!(
            login_ledger_account(&held(&["DU1111111", "DU2222222"]), "DU1111111"),
            Ok("DU1111111".to_string())
        );
    }
}
