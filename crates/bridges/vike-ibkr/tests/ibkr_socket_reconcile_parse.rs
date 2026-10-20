//! Pure parse tests for the SOCKET `ReconClient`'s report parsers (decision 0140) — NO network:
//! CONSTRUCTED ibapi rows go straight into `vike_ibkr::recon::socket`'s pure `parse_socket_*` and
//! `resolve_conid` functions. The counterpart of `tests/ibkr_reconcile_parse.rs` (the cpapi
//! bodies): conId + account filtering, canonical-symbol stamping, the exec-client-id filter on
//! executions, the permId join of an open order to its status, the commission join by execution
//! id, the flat row for an absent symbol (and the ERROR for a read that never names the account), the
//! BASE ledger rule, and the refusal of an
//! ambiguous conId.
//!
//! Behind `ibkr` (the umbrella the socket reconcile module compiles under); the CI lane
//! `cargo test -p vike-ibkr --features ibkr` compiles it.
#![cfg(feature = "ibkr")]

use ibapi::accounts::{AccountSummary, Position};
use ibapi::contracts::{Contract, ContractDetails, Currency};
use ibapi::orders::{
    Action, CommissionReport, Execution, ExecutionData, ExecutionSide, Executions, Liquidity,
    Order, OrderData, OrderState, OrderStatus, OrderStatusKind, Orders,
};
use vike_ibkr::recon::socket::{
    fill_reports_for, order_reports_for, parse_socket_balance, parse_socket_fill_reports,
    parse_socket_order_reports, parse_socket_position_reports, plan_targets, position_reports_for,
    resolve_conid,
};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{OrderStatus as Fsm, OrderType};

const CONID: i32 = 265598; // AAPL
const OTHER_CONID: i32 = 76792991; // TSLA
const SYMBOL: &str = "AAPL.SMART.USD";
const ACCOUNT: &str = "DU1234567";
const EXEC_CLIENT: i32 = 7;

fn contract(conid: i32) -> Contract {
    Contract { contract_id: conid, currency: Currency::from("USD"), ..Contract::default() }
}

// --- resolve_conid --------------------------------------------------------------------------------

/// One conId, repeated per exchange row, is one contract; two different ones are refused (never the
/// first); none is refused.
#[test]
fn the_conid_is_unique_or_refused() {
    let row = |id| ContractDetails { contract: contract(id), ..ContractDetails::default() };
    assert_eq!(resolve_conid(&[row(CONID), row(CONID)], SYMBOL), Ok(CONID));
    let many = resolve_conid(&[row(CONID), row(OTHER_CONID)], SYMBOL).unwrap_err();
    assert!(many.contains("2 contracts"), "{many}");
    assert!(resolve_conid(&[], SYMBOL).is_err());
    assert!(resolve_conid(&[row(0)], SYMBOL).is_err(), "a zero conId names no contract");
}

// --- parse_socket_order_reports -------------------------------------------------------------------

fn open_order(order_id: i32, perm_id: i64, conid: i32, account: &str) -> Orders {
    Orders::OrderData(OrderData {
        order_id,
        contract: contract(conid),
        order: Order {
            order_id,
            perm_id,
            action: Action::Buy,
            total_quantity: 100.0,
            order_type: "LMT".to_string(),
            limit_price: Some(150.0),
            order_ref: "vt-coid-1".to_string(),
            account: account.to_string(),
            ..Order::default()
        },
        order_state: OrderState { status: OrderStatusKind::PreSubmitted, ..OrderState::default() },
    })
}

fn status(
    order_id: i32,
    perm_id: i64,
    kind: OrderStatusKind,
    filled: f64,
    remaining: f64,
) -> Orders {
    Orders::OrderStatus(OrderStatus {
        order_id,
        perm_id,
        status: kind,
        filled,
        remaining,
        average_fill_price: Some(149.5),
        ..OrderStatus::default()
    })
}

/// Every field of a resting limit order, its status joined by permId, filtered to the account and
/// the conId (another contract's order and another account's are not reported).
#[test]
fn an_open_order_reports_every_field_with_its_joined_status() {
    let rows = vec![
        open_order(11, 9001, CONID, ACCOUNT),
        status(11, 9001, OrderStatusKind::Submitted, 40.0, 60.0),
        open_order(12, 9002, OTHER_CONID, ACCOUNT),
        open_order(13, 9003, CONID, "DU7654321"),
    ];
    let r = parse_socket_order_reports(&rows, ACCOUNT, CONID, SYMBOL);
    assert_eq!(r.len(), 1, "{r:?}");
    let o = &r[0];
    assert_eq!((o.venue.as_str(), o.symbol.as_str()), ("ibkr", SYMBOL));
    assert_eq!(o.venue_order_id.as_str(), "11", "the numeric id the exec path binds");
    assert_eq!(o.client_order_id.as_deref(), Some("vt-coid-1"));
    assert_eq!(o.side, 1);
    assert_eq!(o.order_type, OrderType::Limit);
    assert_eq!((o.qty, o.filled_qty, o.avg_px), (100.0, 40.0, 149.5));
    assert_eq!(o.status, Fsm::PartiallyFilled, "Submitted with 0 < filled < total");
}

/// Two API clients may each own an order id 11; the permId keeps their statuses apart. With no
/// status row the order's own state answers, nothing filled.
#[test]
fn the_status_join_is_by_perm_id_not_the_per_client_order_id() {
    let rows = vec![
        open_order(11, 9001, CONID, ACCOUNT),
        // Another client's order 11 (permId 7777) is filled; it must not colour ours.
        status(11, 7777, OrderStatusKind::Filled, 100.0, 0.0),
    ];
    let r = parse_socket_order_reports(&rows, ACCOUNT, CONID, SYMBOL);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].status, Fsm::Accepted, "the OrderData's own PreSubmitted");
    assert_eq!((r[0].filled_qty, r[0].avg_px), (0.0, 0.0));
}

/// Every sell action is -1, a blank order ref is no client order id, a pending cancel stays one.
#[test]
fn a_sell_with_no_ref_pending_cancel() {
    let Orders::OrderData(mut od) = open_order(21, 0, CONID, ACCOUNT) else { unreachable!() };
    od.order.action = Action::SellShort;
    od.order.order_ref.clear();
    od.order.order_type = "MKT".to_string();
    let rows =
        vec![Orders::OrderData(od), status(21, 0, OrderStatusKind::PendingCancel, 0.0, 100.0)];
    let r = parse_socket_order_reports(&rows, ACCOUNT, CONID, SYMBOL);
    assert_eq!(r[0].side, -1);
    assert_eq!(r[0].client_order_id, None);
    assert_eq!(r[0].order_type, OrderType::Market);
    assert_eq!(r[0].status, Fsm::PendingCancel, "joined by order id when IB sent no permId");
}

// --- parse_socket_fill_reports --------------------------------------------------------------------

fn fill(exec_id: &str, client_id: i32, conid: i32, side: ExecutionSide) -> Executions {
    Executions::ExecutionData(ExecutionData {
        request_id: 1,
        contract: contract(conid),
        execution: Execution {
            order_id: 11,
            client_id,
            execution_id: exec_id.to_string(),
            account_number: ACCOUNT.to_string(),
            side,
            shares: 40.0,
            price: 149.5,
            order_reference: "vt-coid-1".to_string(),
            last_liquidity: Liquidity::AddedLiquidity,
            ..Execution::default()
        },
    })
}

fn commission(exec_id: &str, amount: f64) -> Executions {
    Executions::CommissionReport(CommissionReport {
        execution_id: exec_id.to_string(),
        commission: amount,
        currency: "USD".to_string(),
        ..CommissionReport::default()
    })
}

/// The exec client's fill on the conId, with its commission joined by execution id (here the
/// commission arrives FIRST), stamped with the read time. Another client's fill, another
/// contract's, and an id-less row are not reported.
#[test]
fn a_fill_is_the_exec_clients_with_its_commission() {
    let rows = vec![
        commission("0001.01", 1.25),
        fill("0001.01", EXEC_CLIENT, CONID, ExecutionSide::Bought),
        fill("0002.01", 99, CONID, ExecutionSide::Bought),
        fill("0003.01", EXEC_CLIENT, OTHER_CONID, ExecutionSide::Bought),
        fill("", EXEC_CLIENT, CONID, ExecutionSide::Bought),
    ];
    let r = parse_socket_fill_reports(&rows, EXEC_CLIENT, ACCOUNT, CONID, SYMBOL, 42);
    assert_eq!(r.len(), 1, "{r:?}");
    let f = &r[0];
    assert_eq!(f.trade_id.as_str(), "0001.01");
    assert_eq!(f.venue_order_id.as_str(), "11");
    assert_eq!(f.client_order_id.as_deref(), Some("vt-coid-1"));
    assert_eq!((f.side, f.last_qty, f.last_px), (1, 40.0, 149.5));
    assert_eq!((f.commission, f.commission_asset.as_str()), (1.25, "USD"));
    assert_eq!(f.liquidity_side, LiquiditySide::Maker);
    assert_eq!((f.symbol.as_str(), f.ts), (SYMBOL, 42));
}

/// A sold fill with no commission report yet: -1, `0.0` in the contract's currency, taker.
#[test]
fn a_fill_without_its_commission_is_zero_in_the_contracts_currency() {
    let Executions::ExecutionData(mut ed) =
        fill("0004.01", EXEC_CLIENT, CONID, ExecutionSide::Sold)
    else {
        unreachable!()
    };
    ed.execution.last_liquidity = Liquidity::RemovedLiquidity;
    let r = parse_socket_fill_reports(
        &[Executions::ExecutionData(ed)],
        EXEC_CLIENT,
        ACCOUNT,
        CONID,
        SYMBOL,
        0,
    );
    assert_eq!(r[0].side, -1);
    assert_eq!((r[0].commission, r[0].commission_asset.as_str()), (0.0, "USD"));
    assert_eq!(r[0].liquidity_side, LiquiditySide::Taker);
}

// --- parse_socket_position_reports ----------------------------------------------------------------

fn position(conid: i32, account: &str, qty: f64, avg_cost: f64, multiplier: &str) -> Position {
    Position {
        account: account.to_string(),
        contract: Contract { multiplier: multiplier.to_string(), ..contract(conid) },
        position: qty,
        average_cost: avg_cost,
    }
}

/// The signed net position on the conId and the account; another contract's and another
/// account's are not this symbol's.
#[test]
fn a_position_is_signed_and_filtered() {
    let rows = vec![
        position(CONID, ACCOUNT, -25.0, 151.0, ""),
        position(OTHER_CONID, ACCOUNT, 5.0, 200.0, ""),
        position(CONID, "DU7654321", 9.0, 1.0, ""),
    ];
    let r = parse_socket_position_reports(&rows, ACCOUNT, CONID, SYMBOL).unwrap();
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!((r[0].qty, r[0].avg_px), (-25.0, 151.0));
    assert_eq!(r[0].position_side, PositionSide::Both);
    assert_eq!(r[0].symbol, SYMBOL);
}

/// IB's `average_cost` is per CONTRACT: an option at multiplier 100 reports the premium times 100.
#[test]
fn the_average_cost_is_divided_by_the_multiplier() {
    let r = parse_socket_position_reports(
        &[position(CONID, ACCOUNT, 2.0, 350.0, "100")],
        ACCOUNT,
        CONID,
        SYMBOL,
    )
    .unwrap();
    assert_eq!(r[0].avg_px, 3.5);
}

/// **Never empty**: no row for the symbol is the FLAT zero row, so the diff can see a stale local
/// position the venue has closed — when the read carries the account, or is empty.
#[test]
fn an_absent_symbol_is_the_flat_row() {
    let r = parse_socket_position_reports(
        &[position(OTHER_CONID, ACCOUNT, 5.0, 200.0, "")],
        ACCOUNT,
        CONID,
        SYMBOL,
    );
    assert_eq!(r, Ok(vec![vike_model::PositionStatusReport::flat("ibkr", SYMBOL)]));
    assert_eq!(parse_socket_position_reports(&[], ACCOUNT, CONID, SYMBOL).unwrap().len(), 1);
}

/// **A wrong `ACCOUNT` never reads as flat**: a read whose rows are all ANOTHER account's — the
/// configured one never appears — is an error, even when one of them is on our conId.
#[test]
fn a_read_that_never_names_the_account_is_an_error() {
    let rows = vec![
        position(CONID, "DU7654321", 9.0, 1.0, ""),
        position(OTHER_CONID, "DU7654321", 5.0, 200.0, ""),
    ];
    let err = parse_socket_position_reports(&rows, ACCOUNT, CONID, SYMBOL).unwrap_err();
    assert!(err.contains("none for the configured account"), "{err}");
}

// --- parse_socket_balance -------------------------------------------------------------------------

fn ledger(account: &str, tag: &str, currency: &str, value: &str) -> AccountSummary {
    AccountSummary {
        account: account.to_string(),
        tag: tag.to_string(),
        value: value.to_string(),
        currency: currency.to_string(),
    }
}

/// The contract currency's `CashBalance` for this account; else `None`. A non-number is an error,
/// never a guess.
#[test]
fn the_balance_is_the_currencys_cash() {
    let rows = vec![
        ledger(ACCOUNT, "CashBalance", "BASE", "900000.5"),
        ledger(ACCOUNT, "TotalCashBalance", "USD", "1"),
        ledger(ACCOUNT, "CashBalance", "USD", "1000000.25"),
        ledger("DU7654321", "CashBalance", "EUR", "7"),
    ];
    assert_eq!(parse_socket_balance(&rows, ACCOUNT, "USD"), Ok(Some(1_000_000.25)));
    assert_eq!(parse_socket_balance(&rows[1..2], ACCOUNT, "USD"), Ok(None));
    assert!(
        parse_socket_balance(&[ledger(ACCOUNT, "CashBalance", "USD", "n/a")], ACCOUNT, "USD")
            .is_err()
    );
}

/// The BASE block (a converted total) seeds the balance ONLY when its `Currency` row declares the
/// contract's currency as the base. A BASE-only ledger whose base is another currency, or that
/// declares none, is `None`: no seed.
#[test]
fn a_base_only_ledger_seeds_only_its_own_currency() {
    let base_usd = vec![
        ledger(ACCOUNT, "CashBalance", "BASE", "900000.5"),
        ledger(ACCOUNT, "Currency", "BASE", "USD"),
    ];
    assert_eq!(parse_socket_balance(&base_usd, ACCOUNT, "USD"), Ok(Some(900_000.5)));
    assert_eq!(parse_socket_balance(&base_usd, ACCOUNT, "EUR"), Ok(None), "base is not EUR");
    let undeclared = vec![ledger(ACCOUNT, "CashBalance", "BASE", "900000.5")];
    assert_eq!(parse_socket_balance(&undeclared, ACCOUNT, "USD"), Ok(None));
    let says_base = vec![
        ledger(ACCOUNT, "CashBalance", "BASE", "900000.5"),
        ledger(ACCOUNT, "Currency", "BASE", "BASE"),
    ];
    assert_eq!(parse_socket_balance(&says_base, ACCOUNT, "USD"), Ok(None));
}

/// **A foreign cash line is never another currency's seed** (decision 0183, the S0 inventory's IBKR balance
/// site): an account holding 10,000 EUR of cash, whose ledger is read for USD, seeds the USD ledger from its USD
/// cash or from a BASE block that DECLARES USD as the base, and from nothing else. The EUR line moves nothing.
/// The seed's currency is the mounted contract's, which the node anchors the engine's ledger to only when the
/// account's base currency is that same currency.
#[test]
fn a_foreign_cash_line_is_never_a_seed_for_another_currency() {
    let usd_base = vec![
        ledger(ACCOUNT, "CashBalance", "EUR", "10000"),
        ledger(ACCOUNT, "CashBalance", "BASE", "25000"),
        ledger(ACCOUNT, "Currency", "BASE", "USD"),
    ];
    assert_eq!(
        parse_socket_balance(&usd_base, ACCOUNT, "USD"),
        Ok(Some(25_000.0)),
        "the base block"
    );
    assert_eq!(
        parse_socket_balance(&usd_base, ACCOUNT, "EUR"),
        Ok(Some(10_000.0)),
        "EUR's own cash"
    );

    let eur_base = vec![
        ledger(ACCOUNT, "CashBalance", "EUR", "10000"),
        ledger(ACCOUNT, "CashBalance", "BASE", "10000"),
        ledger(ACCOUNT, "Currency", "BASE", "EUR"),
    ];
    assert_eq!(
        parse_socket_balance(&eur_base, ACCOUNT, "USD"),
        Ok(None),
        "no USD cash, base is EUR"
    );
    assert_eq!(parse_socket_balance(&eur_base, ACCOUNT, "EUR"), Ok(Some(10_000.0)));
}

// --- the instruments a pass reads (any-symbol, decision 0173) -------------------------------------

const TSLA: &str = "TSLA.SMART.USD";

fn targets() -> Vec<(i32, String)> {
    vec![(CONID, SYMBOL.to_string()), (OTHER_CONID, TSLA.to_string())]
}

/// The mounted symbol first, then each admitted one pinned to its conId; with nothing admitted the plan is
/// the mounted symbol alone and `resolve` is never asked.
#[test]
fn the_plan_is_the_mounted_symbol_then_each_admitted_one() {
    let never = |_: &str| -> Result<i32, String> { panic!("nothing admitted: nothing to pin") };
    assert_eq!(plan_targets((CONID, SYMBOL), &[], never), Ok(vec![(CONID, SYMBOL.to_string())]));
    let asked = std::cell::RefCell::new(Vec::new());
    let resolve = |s: &str| {
        asked.borrow_mut().push(s.to_string());
        Ok(OTHER_CONID)
    };
    assert_eq!(plan_targets((CONID, SYMBOL), &[TSLA.to_string()], resolve), Ok(targets()));
    assert_eq!(*asked.borrow(), [TSLA]);
}

/// A symbol that resolves to a conId the plan already holds (two spellings of one contract) is read once,
/// under the first spelling; one that cannot be pinned fails the whole plan - a pass never claims a symbol
/// it did not read.
#[test]
fn an_alias_is_read_once_and_an_unpinned_symbol_fails_the_plan() {
    let alias = ["AAPL.NASDAQ.USD".to_string(), TSLA.to_string()];
    let resolve = |s: &str| Ok(if s == TSLA { OTHER_CONID } else { CONID });
    assert_eq!(plan_targets((CONID, SYMBOL), &alias, resolve), Ok(targets()));
    let failing = |_: &str| -> Result<i32, String> { Err("no such contract".into()) };
    assert_eq!(
        plan_targets((CONID, SYMBOL), &[TSLA.to_string()], failing),
        Err("no such contract".to_string())
    );
}

/// ONE account-wide read of each kind, parsed per instrument: an order, a fill and a position on each
/// conId are each reported under their own symbol, and a conId the read does not hold is the flat row.
#[test]
fn one_read_is_parsed_for_every_instrument_the_pass_reads() {
    let orders =
        vec![open_order(11, 9001, CONID, ACCOUNT), open_order(12, 9002, OTHER_CONID, ACCOUNT)];
    let r = order_reports_for(&orders, ACCOUNT, &targets());
    let seen: Vec<(&str, &str)> =
        r.iter().map(|o| (o.symbol.as_str(), o.venue_order_id.as_str())).collect();
    assert_eq!(seen, [(SYMBOL, "11"), (TSLA, "12")]);

    let fills = vec![
        fill("0001.01", EXEC_CLIENT, CONID, ExecutionSide::Bought),
        fill("0002.01", EXEC_CLIENT, OTHER_CONID, ExecutionSide::Sold),
    ];
    let r = fill_reports_for(&fills, EXEC_CLIENT, ACCOUNT, &targets(), 5);
    let seen: Vec<(&str, &str)> =
        r.iter().map(|f| (f.symbol.as_str(), f.trade_id.as_str())).collect();
    assert_eq!(seen, [(SYMBOL, "0001.01"), (TSLA, "0002.01")]);

    let positions = vec![position(OTHER_CONID, ACCOUNT, -3.0, 200.0, "")];
    let r = position_reports_for(&positions, ACCOUNT, &targets()).unwrap();
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!((r[0].symbol.as_str(), r[0].qty), (SYMBOL, 0.0), "the mounted symbol is flat");
    assert_eq!((r[1].symbol.as_str(), r[1].qty), (TSLA, -3.0));
}

/// A read that never names the account fails the answer for every instrument, not just the first.
#[test]
fn a_wrong_account_fails_the_positions_of_every_instrument() {
    let rows = vec![position(CONID, "DU7654321", 9.0, 1.0, "")];
    assert!(position_reports_for(&rows, ACCOUNT, &targets()).is_err());
}
