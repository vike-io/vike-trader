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
//!   currency as the base ([`parse_socket_balance`]), else no seed.
//!
//! Every fetch is bounded by [`READ_DEADLINE`]; a read that does not reach its end in time is an
//! `Err`, never a partial answer: a short list would look like orders or positions the venue no
//! longer holds.
//!
//! Statuses go through the one IB-status classifier the cpapi client uses
//! (`crate::recon::cpapi::normalize_order_status`), so both backends read a status alike.
//!
//! ## ⚠ Unverified against a gateway
//!
//! Every parser here is pinned offline over CONSTRUCTED ibapi rows
//! (`crates/bridges/vike-ibkr/tests/ibkr_socket_reconcile_parse.rs`). Nothing has run against a gateway
//! yet; the vehicle is the read-only `crates/bridges/vike-ibkr/tests/ibkr_socket_reconcile_smoke.rs`.
//! Three things only that run (or a fill) can settle:
//!
//! 1. **Whose executions a non-master client id sees.** IB may answer `reqExecutions` with only the
//!    ASKING client's executions unless it is the Gateway's Master API client id. Then this client
//!    (id `client_id + 2`) sees none of the exec client's fills, and `fetch_fill_reports` is
//!    quietly empty. [`IbkrSocketReconClient::executions_by_client_id`] asks with NO client filter
//!    and counts the rows per client id, so a smoke run after a fill shows directly whether the
//!    exec client's rows are visible. If they are not, the fix is the Gateway's Master API client
//!    id setting (decision 0140's "What would reopen this").
//! 2. **The `$LEDGER` tag names.** `CashBalance` per currency is IB's documented `$LEDGER:ALL`
//!    vocabulary, not an observation. This number becomes the BALANCE seed under `quarantine`.
//! 3. **The fill time.** `Execution.time` is a preformatted string whose timezone has never been
//!    seen (`crate::transport::socket`'s module doc). A fill report is stamped with the READ time,
//!    the same receive-time convention the exec path uses for `FillEvent.ts`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ibapi::accounts::types::AccountGroup;
use ibapi::accounts::{AccountSummary, AccountSummaryResult, Position, PositionUpdate};
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
use crate::transport::gateway_gate::gate_for;
use crate::transport::socket::{OPEN_DEADLINE, bounded, build_contract};

/// How long one report read may take, end to end. A read past it is an `Err`, never a partial list.
pub const READ_DEADLINE: Duration = Duration::from_secs(10);

/// The `account_summary` tag that asks for the per-currency ledger (every currency, plus `BASE`).
const LEDGER_ALL: &str = "$LEDGER:ALL";

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
/// base currency code (⚠ IB's documented `$LEDGER` vocabulary, not yet observed).
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

// --- the client ----------------------------------------------------------------------------------

/// One socket reconcile client per (account, canonical vike symbol): its own ibapi connection on
/// `recon_client_id`, the symbol's pinned conId, and the exec client id whose fills it reports. It also
/// reads every symbol the engine admitted on demand (`ReconClient::admit_symbol`, decision 0173), each
/// pinned to its conId on first use.
pub struct IbkrSocketReconClient {
    client: Arc<Client>,
    account: String,
    exec_client_id: i32,
    symbol: String,
    conid: i32,
    currency: String,
    /// The symbols the engine admitted (the engine's spelling, upper case).
    admitted: AdmittedSymbols,
    /// Each admitted symbol's conId, resolved once.
    resolved: Mutex<HashMap<String, i32>>,
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
            let addr = format!("{}:{}", cfg.host, cfg.port);
            let client = Client::connect(&addr, cfg.recon_client_id).map_err(|e| {
                IbkrError::Connect(format!("recon {addr} (client_id={}): {e}", cfg.recon_client_id))
            })?;
            let managed = client
                .managed_accounts()
                .map_err(|e| IbkrError::Connect(format!("recon managed_accounts: {e}")))?;
            require_managed_account(&managed, &cfg.account).map_err(IbkrError::Unsupported)?;
            let details = client
                .contract_details(&build_contract(&contract))
                .map_err(|e| IbkrError::Connect(format!("recon contract_details {symbol}: {e}")))?;
            let conid = resolve_conid(&details, &symbol).map_err(IbkrError::Unsupported)?;
            Ok(IbkrSocketReconClient {
                client: Arc::new(client),
                account: cfg.account.clone(),
                exec_client_id: cfg.client_id,
                symbol,
                conid,
                currency: contract.currency,
                admitted: AdmittedSymbols::default(),
                resolved: Mutex::new(HashMap::new()),
            })
        })
        .0
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
        let sub = self.client.executions(filter).map_err(|e| format!("ibkr executions: {e}"))?;
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
        let (client, asked) = (Arc::clone(&self.client), build_contract(&contract));
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
        let sub = self.client.all_open_orders().map_err(|e| format!("ibkr open orders: {e}"))?;
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
        let sub = self.client.positions().map_err(|e| format!("ibkr positions: {e}"))?;
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

    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let group = AccountGroup("All".to_string());
        let sub = self
            .client
            .account_summary(&group, &[LEDGER_ALL])
            .map_err(|e| format!("ibkr account summary: {e}"))?;
        let rows = drain(
            |t| sub.next_timeout(t),
            READ_DEADLINE,
            "account summary",
            EndsBy::Marker,
            |r| matches!(r, AccountSummaryResult::End),
        )?;
        let summaries: Vec<AccountSummary> = rows
            .into_iter()
            .filter_map(|r| match r {
                AccountSummaryResult::Summary(s) => Some(s),
                AccountSummaryResult::End => None,
            })
            .collect();
        parse_socket_balance(&summaries, &self.account, &self.currency)
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
}
