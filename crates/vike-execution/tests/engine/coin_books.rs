//! An engine mounted on an INVERSE contract keeps its books in the settle coin and values them in the
//! account's currency at the contract's own price (decision 0258), with the numbers of the inverse research's
//! worked example (deribit `BTC-PERPETUAL`, a 0.1 BTC wallet, a 3.5 bp taker fee) and its hedge case (1 BTC
//! short 60,000 USD, flat in dollars).
//!
//! The wallet enters through the frame the node builds from a reconcile pass (`apply_account_state` with the
//! engine's own quote label); the price through the engine's board (the resolver) and the account's slot.

use indexmap::IndexMap;
use vike_execution::testing::RecordingClient;
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, MarginCall, MarkSource,
    PriceBoardConfig, RiskGate, check_margin_call_priced,
};
use vike_model::events::{AccountState, Event, FillEvent, FundingEvent, PositionSide};
use vike_model::{ContractValue, MarginCallConfig, RiskLimits};

const SYMBOL: &str = "BTC-PERPETUAL";

fn engine_on(symbol: &str, value: ContractValue) -> Engine<RecordingClient> {
    let mut grid = IndexMap::new();
    grid.insert(symbol.to_string(), value);
    Engine::new(
        Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        symbol,
    )
}

fn inverse_engine() -> Engine<RecordingClient> {
    engine_on(SYMBOL, ContractValue::inverse(1.0))
}

/// The wallet a reconcile pass reports, in the frame the node builds (the engine's own quote label).
fn adopt(e: &mut Engine<RecordingClient>, coin: f64) {
    let label = e.quote_asset.clone();
    e.account.apply_account_state(
        &AccountState {
            venue: "deribit".into(),
            balances: vec![(label.clone(), coin)],
            timestamp: 0,
            route_key: None,
        },
        &label,
    );
}

fn fill(id: &'static str, side: i32, qty: f64, px: f64, fee: f64, asset: &str) -> FillEvent {
    FillEvent {
        trade_id: id.into(),
        order_client_id: id.to_string(),
        venue: "deribit".into(),
        symbol: SYMBOL.into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: fee,
        commission_asset: asset.into(),
        liquidity_side: "taker".into(),
        timestamp: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

fn fold(e: &mut Engine<RecordingClient>, f: FillEvent) {
    let mut ob = EventBusOutbox::default();
    e.on_event(&Event::Fill(f), &mut ob);
}

/// The price `px` on the board and in the account's slot, as the feeds write both.
fn mark(e: &mut Engine<RecordingClient>, px: f64) {
    e.price_board.set_mark("deribit", SYMBOL, px, 1);
    e.account.set_mark_from("deribit", SYMBOL, px, MarkSource::VenueMark, 1);
}

fn resolved(e: &Engine<RecordingClient>) -> f64 {
    e.resolved_equity(0.0, &PriceBoardConfig::default())
}

fn close(got: f64, want: f64, tolerance: f64) -> bool {
    (got - want).abs() < tolerance
}

/// The research's worked example. Long 1,000 USD at 60,000 on a 0.1 BTC wallet, fee 0.0000058333 BTC: at
/// 66,000 the venue's account is 0.0999941667 BTC × 66,000 + 100 = 6,699.62, where the old seed ledger read
/// 6,099.65 on a seed of 6,000. The coin fee stays a coin amount.
#[test]
fn the_worked_example_values_the_coin_wallet_at_the_mark() {
    let mut e = inverse_engine();
    adopt(&mut e, 0.1);
    assert_eq!(e.account.balance_mode, BalanceMode::Authoritative, "the coin wallet is adopted");
    assert_eq!(e.account.balance, 0.1);
    fold(&mut e, fill("o1", 1, 1_000.0, 60_000.0, 0.000_005_833_3, "BTC"));
    assert!(
        close(e.account.fees_paid, 0.000_005_833_3, 1e-15),
        "the fee in BTC: {}",
        e.account.fees_paid
    );
    assert!(close(e.account.balance, 0.099_994_166_7, 1e-12), "{}", e.account.balance);
    mark(&mut e, 66_000.0);
    let equity = resolved(&e);
    let want = (0.1 - 0.000_005_833_3) * 66_000.0 + 100.0; // 6,699.62
    assert!(close(equity, want, 1e-6), "resolved equity {equity}");
    assert!(close(equity, 6_699.62, 0.01), "{equity}");
    assert!(close(e.account.equity_all(0.0), equity, 1e-9), "the account's own read agrees");
    assert_eq!(
        e.coin_rate(&PriceBoardConfig::default()),
        Some(66_000.0),
        "the perpetual's own price"
    );
}

/// Closing books the coin PnL, `1,000·(1/60,000 − 1/66,000)` = 0.0015151515 BTC, and the wallet then follows the
/// coin's price: 0.1015040152 BTC is 6,699.27 at 66,000 and 6,090.24 back at 60,000 (the old ledger: 6,099.30).
#[test]
fn closing_books_the_coin_pnl_and_the_wallet_follows_the_coin_price() {
    let mut e = inverse_engine();
    adopt(&mut e, 0.1);
    fold(&mut e, fill("o1", 1, 1_000.0, 60_000.0, 0.000_005_833_3, "BTC"));
    fold(&mut e, fill("c1", -1, 1_000.0, 66_000.0, 0.000_005_303_0, "BTC"));
    let coin_pnl = 1_000.0 * (1.0 / 60_000.0 - 1.0 / 66_000.0);
    assert!(close(e.account.realized_pnl, coin_pnl, 1e-15), "{}", e.account.realized_pnl);
    assert_eq!(e.account.closed_pnls.len(), 1);
    assert!(close(e.account.fees_paid, 0.000_011_136_3, 1e-15), "{}", e.account.fees_paid);
    mark(&mut e, 66_000.0);
    assert!(close(resolved(&e), 0.101_504_015_2 * 66_000.0, 1e-5), "{}", resolved(&e));
    mark(&mut e, 60_000.0);
    assert!(close(resolved(&e), 6_090.240_91, 1e-4), "{}", resolved(&e));
}

/// An order's fill VWAP on an inverse contract is HARMONIC, the average the position folds: a buy of 2,000 USD
/// filled 1,000 at 50,000 and 1,000 at 60,000 paid 54,545.45 a coin, where the fold's arithmetic mean said 55,000.
/// The order and the position it opened state the same price.
#[test]
fn an_inverse_orders_fill_vwap_is_the_harmonic_price_its_position_holds() {
    use vike_model::events::{OrderAccepted, OrderFilled, OrderPartiallyFilled};
    let mut e = inverse_engine();
    let req = vike_model::OrderRequest {
        order_client_id: "c1".into(),
        venue: "deribit".into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 2_000.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(60_000.0),
        timestamp: 1,
        ..Default::default()
    };
    let mut ob = EventBusOutbox::default();
    e.submit_order(&req, 1, &mut ob);
    let accepted =
        OrderAccepted { order_client_id: "c1".into(), venue_order_id: None, timestamp: 1 };
    for event in [
        Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: "c1".into(),
            timestamp: 1,
        }),
        Event::OrderAccepted(accepted),
    ] {
        e.on_event(&event, &mut ob);
    }
    let leg = |id: &'static str, px: f64| FillEvent {
        order_client_id: "c1".into(),
        ..fill(id, 1, 1_000.0, px, 0.0, "BTC")
    };
    for (event, f) in [
        (
            Event::OrderPartiallyFilled(OrderPartiallyFilled {
                order_client_id: "c1".into(),
                fill: leg("t1", 50_000.0),
                timestamp: 2,
            }),
            leg("t1", 50_000.0),
        ),
        (
            Event::OrderFilled(OrderFilled {
                order_client_id: "c1".into(),
                fill: leg("t2", 60_000.0),
                timestamp: 3,
            }),
            leg("t2", 60_000.0),
        ),
    ] {
        fold(&mut e, f);
        e.on_event(&event, &mut ob);
    }
    let o = &e.registry["c1"];
    assert_eq!(o.status, vike_model::OrderStatus::Filled);
    assert!(
        close(o.average_fill_price, 54_545.454_545, 1e-6),
        "the order's VWAP: {}",
        o.average_fill_price
    );
    let key = ("deribit".into(), SYMBOL.into(), PositionSide::Both);
    let held = e.account.positions[&key].average_price;
    assert!(
        close(o.average_fill_price, held, 1e-9),
        "the order {} and its position {held}",
        o.average_fill_price
    );
}

/// **The hedge.** 1 BTC short 60,000 USD at 60,000 is flat in dollars: 60,000 at +10% and at −10%, where the old
/// seed ledger read 54,000 and 66,000. The margin sweep reads that figure, so it sees a healthy account.
#[test]
fn a_hedged_short_reads_flat_at_every_price_and_the_sweep_reads_it() {
    let mut e = inverse_engine();
    adopt(&mut e, 1.0);
    fold(&mut e, fill("s1", -1, 60_000.0, 60_000.0, 0.0, "BTC"));
    for px in [66_000.0, 54_000.0, 120_000.0, 30_000.0] {
        mark(&mut e, px);
        let equity = resolved(&e);
        assert!(close(equity, 60_000.0, 1e-6), "at {px}: {equity}");
        let verdict =
            check_margin_call_priced(&e.account, equity, &MarginCallConfig::default(), |k, _| {
                e.account.mark_of_key(&(k.0, k.1))
            });
        assert_eq!(verdict, MarginCall::Healthy, "at {px}");
    }
    // own PnL is the short's dollars at the coin's price (the collateral is not this engine's PnL)
    mark(&mut e, 66_000.0);
    assert!(close(e.resolved_own_pnl(&PriceBoardConfig::default()), -6_000.0, 1e-6));
}

/// An unpriced coin counts NOTHING: an adopted wallet with no price of the mounted contract anywhere values
/// at zero (the margin lane then denies an opening), and a price makes it the wallet's dollars.
#[test]
fn an_unpriced_coin_counts_nothing() {
    let mut e = inverse_engine();
    adopt(&mut e, 0.1);
    assert_eq!(e.coin_rate(&PriceBoardConfig::default()), Some(0.0));
    assert_eq!(resolved(&e), 0.0);
    assert_eq!(e.account.equity_all(0.0), 0.0);
    // the account's last mark (what a restore brings back) values it when the board has none
    e.account.set_mark_from("deribit", SYMBOL, 50_000.0, MarkSource::VenueMark, 1);
    assert!(close(resolved(&e), 5_000.0, 1e-9));
}

/// Before the wallet is adopted the configured seed stays the account's dollars and the coin it booked is
/// valued at the price; a dollar-stated fee is divided by the fill price; funding is booked in the coin as
/// stated, whatever mark the event carries.
#[test]
fn a_delta_coin_ledger_keeps_its_seed_and_books_fees_and_funding_in_coin() {
    let mut e = inverse_engine();
    fold(&mut e, fill("o1", 1, 1_000.0, 50_000.0, 0.5, "USD"));
    assert!(close(e.account.fees_paid, 0.5 / 50_000.0, 1e-18), "{}", e.account.fees_paid);
    e.account.apply_funding(&FundingEvent {
        venue: "deribit".into(),
        symbol: SYMBOL.into(),
        position_side: PositionSide::Both,
        funding_rate: 0.0001,
        amount: -0.000_01,
        mark_price: Some(50_000.0),
        timestamp: 2,
        route_key: None,
        currency: None,
    });
    assert!(close(e.account.funding_paid, -0.000_01, 1e-18), "{}", e.account.funding_paid);
    mark(&mut e, 50_000.0);
    let want = 1_000.0 + (-0.000_01 - 0.000_01) * 50_000.0;
    assert!(close(e.resolved_equity(1_000.0, &PriceBoardConfig::default()), want, 1e-9));
}

/// **One coin-fee rule** (`Account::commission_in_settle`): the fee the coin ledger books is the figure a
/// strategy hears, to the bit. A dollar fee is divided by the fill price; a coin fee, and a dollar fee on a
/// fill with no price, are as stated (the ledger always kept that figure, where `in_settle` reads 0.0).
#[test]
fn the_coin_ledger_books_each_fee_by_the_rule_a_strategy_hears_it_by() {
    for (id, px, fee, asset, booked) in [
        ("usd", 50_000.0, 0.5, "USD", 0.5_f64 / 50_000.0),
        ("btc", 50_000.0, 0.000_01, "BTC", 0.000_01),
        ("unpriced", 0.0, 0.5, "USD", 0.5),
    ] {
        let mut e = inverse_engine();
        let f = fill(id, 1, 1_000.0, px, fee, asset);
        let heard = Account::commission_in_settle(ContractValue::inverse(1.0), &f);
        e.account.apply_fill(&f);
        assert_eq!(e.account.fees_paid.to_bits(), booked.to_bits(), "{id}: the ledger");
        assert_eq!(heard.to_bits(), booked.to_bits(), "{id}: the strategy");
    }
}

/// The snapshot carries the coin books in `coin` and writes the dollar fields as nothing booked, so a build
/// that predates them reads a fresh book; this build restores the coin books exactly, hash for hash.
#[test]
fn the_snapshot_carries_the_coin_books_and_zeroes_the_dollar_fields() {
    let mut e = inverse_engine();
    adopt(&mut e, 0.1);
    fold(&mut e, fill("o1", 1, 1_000.0, 60_000.0, 0.000_005_833_3, "BTC"));
    fold(&mut e, fill("c1", -1, 500.0, 66_000.0, 0.0, "BTC"));
    let snap = e.snapshot_state();
    let a = &snap.account;
    assert_eq!((a.balance, a.realized_pnl, a.fees_paid, a.funding_paid), (0.0, 0.0, 0.0, 0.0));
    assert_eq!(a.balance_mode, BalanceMode::Delta);
    assert!(a.closed_pnls.is_empty());
    let coin = a.coin.as_ref().expect("the coin books ride the snapshot");
    assert_eq!(coin.symbol, SYMBOL);
    assert_eq!(coin.balance_mode, BalanceMode::Authoritative);
    assert_eq!(coin.realized_pnl, e.account.realized_pnl);
    assert_eq!(a.positions.len(), 1, "positions are the same in both units");

    let back = Engine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(back.account.coin_key(), e.account.coin_key());
    assert_eq!(back.account.balance, e.account.balance);
    assert_eq!(back.snapshot_state(), snap);
    assert_eq!(
        vike_execution::state_hash(&[back.snapshot_state()]),
        vike_execution::state_hash(&[snap])
    );
}

/// A snapshot written BEFORE coin books (its money in dollars, no `coin` key) is not read as coin: the
/// positions come back and the money starts at zero, through both restore paths (no migration).
#[test]
fn a_snapshot_from_before_coin_books_restores_its_positions_and_not_its_dollars() {
    let mut e = inverse_engine();
    fold(&mut e, fill("o1", 1, 1_000.0, 60_000.0, 0.000_005_833_3, "BTC"));
    let mut old = e.snapshot_state();
    old.account.coin = None;
    (old.account.balance, old.account.realized_pnl, old.account.fees_paid) = (-0.35, 12.0, 0.35);
    old.account.closed_pnls = vec![12.0];

    let replayed = Engine::from_snapshot(&old, RecordingClient::default());
    let mut booted = inverse_engine();
    booted.restore_runtime_state(&old);
    for (name, r) in [("from_snapshot", &replayed), ("restore_runtime_state", &booted)] {
        let a = &r.account;
        assert!(a.coin_key().is_some(), "{name}: still a coin ledger");
        assert_eq!((a.balance, a.realized_pnl, a.fees_paid), (0.0, 0.0, 0.0), "{name}");
        assert!(a.closed_pnls.is_empty(), "{name}");
        assert_eq!(a.positions.len(), 1, "{name}: the position is back");
    }
}

/// The reconcile tolerance of a coin ledger is in the coin: the dollar floor at the last journaled fill price,
/// zero before any fill (the relative band alone), carried through the snapshot; a dollar ledger's is as it is.
#[test]
fn the_reconcile_floor_is_the_dollar_floor_at_the_last_fill_price() {
    let tolerance =
        vike_execution::recon::ReconcileBalanceTolerance { abs_floor: 1.0, rel_frac: 1e-4 };
    let mut e = inverse_engine();
    assert_eq!(
        e.account.coin_tolerance(tolerance).abs_floor,
        0.0,
        "no fill yet: the relative band alone"
    );
    assert_eq!(e.account.coin_tolerance(tolerance).rel_frac, 1e-4);
    fold(&mut e, fill("o1", 1, 1_000.0, 60_000.0, 0.0, "BTC"));
    assert_eq!(e.account.coin_tolerance(tolerance).abs_floor, 1.0 / 60_000.0);
    let back = Engine::from_snapshot(&e.snapshot_state(), RecordingClient::default());
    assert_eq!(
        back.account.coin_tolerance(tolerance).abs_floor,
        1.0 / 60_000.0,
        "the snapshot carries it"
    );
    let linear = engine_on("BTC_USDC-PERPETUAL", ContractValue::linear(1.0));
    assert_eq!(linear.account.coin_tolerance(tolerance), tolerance);
}

/// A LINEAR mount is no coin ledger: its snapshot writes no `coin` key and its fee is the venue's figure.
#[test]
fn a_linear_mount_writes_no_coin_key_and_books_as_it_always_did() {
    let mut e = engine_on("BTC_USDC-PERPETUAL", ContractValue::linear(1.0));
    assert_eq!(e.account.coin_key(), None);
    assert_eq!(e.coin_rate(&PriceBoardConfig::default()), None);
    let mut f = fill("l1", 1, 0.01, 60_000.0, 0.21, "USDC");
    f.symbol = "BTC_USDC-PERPETUAL".into();
    fold(&mut e, f);
    assert_eq!(e.account.fees_paid, 0.21);
    let json = serde_json::to_string(&e.snapshot_state()).unwrap();
    assert!(!json.contains("\"coin\""), "{json}");
}

/// **The owner's example (2026-11-15, decision 0258's second question, "нет")**: a demo wallet of 100 BTC and a
/// 1000 USD seed size from 1000 USD (0.0167 BTC at 60,000), never from the 6 million the wallet is worth, and the
/// seed held in coin moves with it (1100 at 66,000). The solvency figure keeps the wallet. `max_sizing_equity`
/// then caps what is left: the lower of the two wins. A fill's `equity_after` reads the same cap at the mark.
#[test]
fn a_coin_wallet_sizes_from_the_seed_held_in_coin_never_from_the_wallet() {
    let config = PriceBoardConfig::default();
    let mut e = inverse_engine();
    e.equity_seed = 1_000.0;
    adopt(&mut e, 100.0);
    mark(&mut e, 60_000.0);
    assert_eq!(
        e.resolved_equity(1_000.0, &config),
        6_000_000.0,
        "the margin call reads the wallet"
    );
    assert_eq!(e.sizing_equity(1_000.0, &config), 1_000.0, "before the latch converts: the seed");
    assert!(close(e.hold_seed_in_coin(1_000.0, &config).unwrap(), 1_000.0, 1e-9));
    assert!(
        close(e.coin_seed.unwrap(), 1_000.0 / 60_000.0, 1e-15),
        "0.0167 BTC: {:?}",
        e.coin_seed
    );
    mark(&mut e, 66_000.0);
    // 1e-6: the 6.6 million wallet is taken out again, so the figure carries a few of its ulps
    assert!(
        close(e.sizing_equity(1_000.0, &config), 1_100.0, 1e-6),
        "the seed moves with the coin"
    );
    assert_eq!(e.resolved_equity(1_000.0, &config), 6_600_000.0);
    e.gate.limits.max_sizing_equity = Some(500.0);
    assert_eq!(e.sizing_equity(1_000.0, &config), 500.0, "a lower ceiling wins");
    e.gate.limits.max_sizing_equity = Some(5_000.0);
    assert!(
        close(e.sizing_equity(1_000.0, &config), 1_100.0, 1e-6),
        "a higher one changes nothing"
    );
    e.gate.limits.max_sizing_equity = None;
    e.collect_applied_fills = true;
    fold(&mut e, fill("o1", 1, 1_000.0, 66_000.0, 0.0, "BTC"));
    let after = e.applied_fills.last().expect("collected").equity_after;
    assert!(close(after, 1_100.0, 1e-6), "on_fill's ctx.equity: {after}");
}

/// **A USD-margined account is unchanged**: its adopted dollar wallet is what it sizes from, bit for bit the
/// resolved equity, and no coin seed is ever held.
#[test]
fn a_dollar_wallet_still_sizes_from_the_wallet() {
    let config = PriceBoardConfig::default();
    let mut e = engine_on("BTC_USDC-PERPETUAL", ContractValue::linear(1.0));
    let label = e.quote_asset.clone();
    e.account.apply_account_state(
        &AccountState {
            venue: "deribit".into(),
            balances: vec![(label.clone(), 100_000.0)],
            timestamp: 0,
            route_key: None,
        },
        &label,
    );
    assert_eq!(e.account.balance_mode, BalanceMode::Authoritative);
    assert_eq!(e.hold_seed_in_coin(1_000.0, &config), None);
    assert_eq!(e.coin_seed, None);
    let sizing = e.sizing_equity(1_000.0, &config);
    assert_eq!(sizing.to_bits(), e.resolved_equity(1_000.0, &config).to_bits());
    assert_eq!(sizing, 100_000.0);
}
