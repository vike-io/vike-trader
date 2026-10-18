//! A venue's quotes reach EVERY engine of that venue that was admitted the symbol (decisions 0173
//! and 0174), through the REAL runtime: `spawn_core_multi` + the command lane + the tick lane + the
//! published snapshot.
//!
//! The owner's rule is that an ACCOUNT trades any symbol of its exchange at any time. The core used to
//! hand a venue's quote, trade and book to ONE engine, the venue's DEFAULT account, so a LABELLED account
//! (`binance#ALT`) that was admitted a symbol held it with no price: a market order was denied
//! `no-reference-price`, a limit order skipped the collar and a position was never priced in equity.
//! This suite pins the fix end to end; the white-box half (the fan-out list itself, the default
//! account never in it, a restored admission) is `runtime/tests/route_key.rs`.

use vike_core::spawn_core_multi;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, QuoteUpdate, RiskGate,
    SymbolAdmission, TradeUpdate,
};
use vike_marketdata::{QuoteTick, TradeTick};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{OrderRequest, OrderType, RiskLimits, SymbolGrid};

use crate::kit::engines::test_config;
use crate::kit::handle::wait_for_snapshot;

const VENUE: &str = "binance";
const MOUNTED: &str = "BTCUSDT";
const SECOND: &str = "ETHUSDT";

/// A binance/BTCUSDT engine under `route_key` (the venue itself for the default account, the way
/// `vike_tradehub_core::make_engine_for_account` stamps it; the `Account` keeps the canonical venue).
fn engine(route_key: &str) -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits { tick_size: Some(0.1), lot_size: Some(0.001), ..RiskLimits::new() };
    let mut e = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        VENUE,
        MOUNTED,
    );
    e.route_key = route_key.into();
    e
}

fn admit(route_key: &str) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: route_key.into(),
        symbol: SECOND.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.0001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
    }))
}

fn eth_quote(px: f64, ts: i64) -> QuoteUpdate {
    QuoteUpdate {
        venue: VENUE.into(),
        symbol: SECOND.into(),
        quote: QuoteTick {
            ts,
            local_ts: ts,
            bid: px,
            ask: px,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: SECOND.into(),
        },
    }
}

/// A MARKET buy on ETH for the account `label` (`"DEFAULT"` = the default account). A market order
/// carries no price of its own, so the engine's gate needs the engine's own reference price.
fn market(label: &str, coid: &str) -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: SECOND.into(),
        side: 1,
        qty: 0.1,
        order_type: OrderType::Market,
        // Two accounts of one exchange: an order naming none is refused as ambiguous.
        account: Some(match label {
            "DEFAULT" => AccountLabel::Default,
            named => AccountLabel::Named(named.into()),
        }),
        ..Default::default()
    })))
}

fn eth_fill(tid: &'static str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: tid.into(),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: SECOND.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

#[test]
fn a_labelled_account_admitted_a_symbol_is_priced_by_its_quotes() {
    let handle = spawn_core_multi(
        engine(VENUE),
        vec![(1_000.0, engine("binance#ALT"))],
        test_config(10_000.0),
    );
    let cell = handle.snapshot_cell();
    handle.send_command(admit("binance#ALT"));
    handle.tick_sender().quote(eth_quote(3000.0, 1)).unwrap();
    handle.send_command(market("ALT", "o-alt"));
    // Before the fan-out the market order was DENIED `no-reference-price` (the quote reached only the
    // default engine), so it never reached the registry and this wait timed out.
    wait_for_snapshot(&cell, 5, "ALT's market order on ETH to rest", |s| !s.orders.is_empty());
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.orders.len(), 1);
    let order = &snap.orders[0];
    assert_eq!(order.symbol, SECOND);
    assert_eq!(
        order.account,
        Some(AccountLabel::Named("ALT".into())),
        "ALT's book, not the default's"
    );
}

#[test]
fn a_labelled_account_is_priced_by_trades_too() {
    let handle = spawn_core_multi(
        engine(VENUE),
        vec![(1_000.0, engine("binance#ALT"))],
        test_config(10_000.0),
    );
    let cell = handle.snapshot_cell();
    handle.send_command(admit("binance#ALT"));
    handle
        .tick_sender()
        .trade(TradeUpdate {
            venue: VENUE.into(),
            symbol: SECOND.into(),
            trade: TradeTick {
                ts: 1,
                local_ts: 1,
                price: 3000.0,
                size: 1.0,
                is_buyer_maker: false,
                symbol: SECOND.into(),
            },
        })
        .unwrap();
    handle.send_command(market("ALT", "o-alt"));
    wait_for_snapshot(&cell, 5, "ALT's market order on ETH to rest", |s| !s.orders.is_empty());
    handle.shutdown_and_join();
}

#[test]
fn a_labelled_positions_equity_is_priced_off_the_fanned_out_quote() {
    let handle = spawn_core_multi(
        engine(VENUE),
        vec![(1_000.0, engine("binance#ALT"))],
        test_config(10_000.0),
    );
    let cell = handle.snapshot_cell();
    handle.send_command(admit("binance#ALT"));
    // Only ALT claims ETH, so the venue fill routes to it by symbol.
    handle.event_sender().blocking_send(eth_fill("t1", 0.1, 3000.0)).unwrap();
    handle.tick_sender().quote(eth_quote(3100.0, 2)).unwrap();
    wait_for_snapshot(&cell, 5, "ALT's ETH position to be priced", |s| {
        s.portfolio.venues.get(1).is_some_and(|b| {
            b.positions.iter().any(|p| p.symbol == SECOND) && b.missing_prices == 0
        })
    });
    handle.shutdown_and_join();

    let snap = cell.load_full();
    let alt = &snap.portfolio.venues[1];
    assert!((alt.unrealized - 10.0).abs() < 1e-6, "0.1 ETH up 100: {}", alt.unrealized);
    assert_eq!(snap.portfolio.venues[0].positions.len(), 0, "the default account holds nothing");
}

#[test]
fn two_labelled_accounts_admitting_one_symbol_both_get_its_quotes() {
    let handle = spawn_core_multi(
        engine(VENUE),
        vec![(1_000.0, engine("binance#ALT")), (1_000.0, engine("binance#BOB"))],
        test_config(10_000.0),
    );
    let cell = handle.snapshot_cell();
    handle.send_command(admit("binance#ALT"));
    handle.send_command(admit("binance#BOB"));
    handle.tick_sender().quote(eth_quote(3000.0, 1)).unwrap();
    handle.send_command(market("ALT", "o-alt"));
    handle.send_command(market("BOB", "o-bob"));
    wait_for_snapshot(&cell, 5, "both accounts' market orders to rest", |s| s.orders.len() == 2);
    handle.shutdown_and_join();

    let snap = cell.load_full();
    let mut labels: Vec<_> = snap.orders.iter().map(|o| o.account.clone()).collect();
    labels.sort_by_key(|l| format!("{l:?}"));
    assert_eq!(
        labels,
        vec![Some(AccountLabel::Named("ALT".into())), Some(AccountLabel::Named("BOB".into()))]
    );
}

/// The default account's own admitted symbol is priced exactly as before: the quote reaches it once,
/// through the existing route. (That a labelled account which did NOT admit the symbol is handed no
/// price is pinned white-box: an engine that is not fail-closed would rest its order unpriced, so the
/// published orders cannot tell.)
#[test]
fn the_default_accounts_admitted_symbol_is_unaffected() {
    let handle = spawn_core_multi(
        engine(VENUE),
        vec![(1_000.0, engine("binance#ALT"))],
        test_config(10_000.0),
    );
    let cell = handle.snapshot_cell();
    handle.send_command(admit(VENUE));
    handle.tick_sender().quote(eth_quote(3000.0, 1)).unwrap();
    handle.send_command(market("DEFAULT", "o-default"));
    wait_for_snapshot(&cell, 5, "the default account's market order to rest", |s| {
        !s.orders.is_empty()
    });
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.orders.len(), 1);
    assert_eq!(snap.orders[0].account, None, "the default book");
}
