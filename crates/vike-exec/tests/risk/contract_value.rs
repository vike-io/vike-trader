//! The contract value (decision 0170) through the gate, the account and the snapshot: deribit's
//! two cases on the order path, and the proof that a LINEAR contract is what it was.
//!
//! * `BTC-PERPETUAL` is inverse: its `amount` is US dollars, so an order of 10 is worth 10 against a
//!   100 ceiling and 11 is not the price-times-ten figure it used to be priced at.
//! * `BTC_USDC-PERPETUAL` is linear in the base coin: 0.01 BTC at 65,700 is 657 and a 100 ceiling
//!   refuses it where the old lot-as-multiplier figure (0.0657) let it through.

use indexmap::IndexMap;
use vike_exec::{Account, BalanceMode, RiskContext, RiskGate};
use vike_model::events::{AccountState, FillEvent, PositionSide};
use vike_model::{ContractValue, OrderRequest, OrderType, RiskLimits};

const MARK: f64 = 65_700.0;

fn limits(cap: f64) -> RiskLimits {
    RiskLimits { max_notional_per_order: Some(cap), ..RiskLimits::new() }
}

fn ctx(value: ContractValue) -> RiskContext {
    RiskContext { mark_price: MARK, contract_value: value, ..RiskContext::default() }
}

fn limit_order(qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "c1".into(),
        venue: "deribit".into(),
        symbol: "BTC-PERPETUAL".into(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(MARK),
        ..Default::default()
    }
}

/// THE regression of the the CI box evidence: qty 10 on the inverse perpetual passes a 100 ceiling,
/// 110 does not, and neither depends on the price.
#[test]
fn an_inverse_order_is_judged_by_its_dollar_size_against_the_ceiling() {
    let inverse = ctx(ContractValue::inverse(1.0));
    let mut g = RiskGate::new(limits(100.0));
    assert!(g.check(&limit_order(10.0), &inverse).ok, "10 USD is under a 100 ceiling");
    let mut g = RiskGate::new(limits(100.0));
    let v = g.check(&limit_order(110.0), &inverse);
    assert!(!v.ok && v.reason == "over-max-notional", "110 USD is over it: {v:?}");
    // the price does not enter: the same order at a 10x price is the same verdict
    let mut dear = limit_order(10.0);
    dear.price = Some(MARK * 10.0);
    let mut g = RiskGate::new(limits(100.0));
    assert!(g.check(&dear, &inverse).ok);
}

/// What the old scalar did to the same order, kept as the measured contrast: contract_size 10 as
/// a LINEAR multiplier priced qty 10 at 10 * 65,700 * 10.
#[test]
fn the_old_scalar_priced_the_same_order_at_millions() {
    let as_scalar = ctx(ContractValue::linear(10.0));
    let mut g = RiskGate::new(limits(100.0));
    let v = g.check(&limit_order(10.0), &as_scalar);
    assert!(!v.ok && v.reason == "over-max-notional", "{v:?}");
}

/// The linear USDC perpetual: 0.01 BTC at 65,700 is 657, so a 100 ceiling refuses it.
#[test]
fn a_linear_usdc_order_is_qty_times_price_and_is_refused_where_it_used_to_pass() {
    let mut order = limit_order(0.01);
    order.symbol = "BTC_USDC-PERPETUAL".into();
    let mut g = RiskGate::new(limits(100.0));
    let v = g.check(&order, &ctx(ContractValue::linear(1.0)));
    assert!(!v.ok && v.reason == "over-max-notional", "657 > 100: {v:?}");
    // …and the old figure (the 0.0001 lot as a multiplier) passed it
    let mut g = RiskGate::new(limits(100.0));
    assert!(g.check(&order, &ctx(ContractValue::linear(0.0001))).ok);
}

fn fill(
    trade_id: &'static str,
    side: i32,
    qty: f64,
    px: f64,
    commission: f64,
    asset: &str,
) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: trade_id.to_string(),
        venue: "deribit".into(),
        symbol: "BTC-PERPETUAL".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission,
        commission_asset: asset.into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

fn inverse_account() -> Account {
    let mut grid = IndexMap::new();
    grid.insert("BTC-PERPETUAL".to_string(), ContractValue::inverse(1.0));
    Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta)
}

/// An inverse position folds the harmonic average, books quote-currency PnL at the exit and values
/// exposure and margin in dollars.
#[test]
fn an_inverse_account_folds_books_and_values_in_quote_units() {
    let mut a = inverse_account();
    a.apply_fill(&fill("t1", 1, 30.0, 60_000.0, 0.0, "USD"));
    a.apply_fill(&fill("t2", 1, 30.0, 66_000.0, 0.0, "USD"));
    let key = ("deribit".into(), "BTC-PERPETUAL".into(), PositionSide::Both);
    let p = a.positions[&key];
    assert_eq!(p.size, 60.0);
    let harmonic = 60.0 / (30.0 / 60_000.0 + 30.0 / 66_000.0);
    assert!((p.avg_px - harmonic).abs() < 1e-9, "{} vs {harmonic}", p.avg_px);

    // exposure and margin are the dollars held, whatever the mark
    a.set_mark_from("deribit", "BTC-PERPETUAL", 70_000.0, vike_exec::MarkSource::VenueMark, 0);
    let px = |key: &vike_exec::PositionKey, _p: &vike_exec::PositionEntry| {
        a.mark_of_key(&(key.0, key.1))
    };
    assert_eq!(a.gross_notional_priced(px), 60.0);
    assert_eq!(a.net_notional_priced(px), 60.0);
    assert_eq!(a.margin_in_use_priced(px, |_, _| Some(0.5)), 30.0);

    // close 60 at 69_000: realized = 60 * (69000 - avg)/avg, in the quote currency
    a.apply_fill(&fill("t3", -1, 60.0, 69_000.0, 0.0, "USD"));
    let want = 60.0 * (69_000.0 - harmonic) / harmonic;
    assert!((a.realized_pnl - want).abs() < 1e-9, "{} vs {want}", a.realized_pnl);
}

/// The fee of an inverse contract is charged in the settle coin and enters the ONE-currency
/// ledger at the fill price; a dollar fee and a linear contract are untouched.
#[test]
fn a_coin_fee_on_an_inverse_contract_enters_the_quote_ledger_at_the_fill_price() {
    let mut a = inverse_account();
    a.apply_fill(&fill("t1", 1, 30.0, 60_000.0, 0.00001, "BTC"));
    assert!((a.fees_paid - 0.6).abs() < 1e-12, "{}", a.fees_paid);
    assert!((a.balance + 0.6).abs() < 1e-12, "{}", a.balance);
    a.apply_fill(&fill("t2", 1, 30.0, 60_000.0, 0.25, "USD"));
    assert!((a.fees_paid - 0.85).abs() < 1e-12, "a dollar fee is a dollar fee");

    let mut linear = Account::new(1.0, "deribit", None, BalanceMode::Delta);
    linear.apply_fill(&fill("t3", 1, 30.0, 60_000.0, 0.00001, "BTC"));
    assert_eq!(linear.fees_paid, 0.00001, "a linear contract books the commission as stated");
}

/// A coin wallet is REPORTED in the per-asset ledger but never absorbed into an inverse engine's
/// quote ledger, balance mode or cash-reconcile baseline.
#[test]
fn a_coin_wallet_is_reported_and_not_absorbed_by_an_inverse_engine() {
    let mut a = inverse_account();
    let before = (a.balance, a.balance_mode, a.realized_pnl_at_balance_sync);
    a.apply_account_state(
        &AccountState {
            venue: "deribit".into(),
            balances: vec![("BTC".to_string(), 0.5)],
            ts: 0,
            route_key: None,
        },
        "USDT",
    );
    assert_eq!((a.balance, a.balance_mode, a.realized_pnl_at_balance_sync), before);
    assert_eq!(a.balances_by_asset.get("BTC"), Some(&0.5), "reported");

    let mut plain = Account::new(1.0, "binance", None, BalanceMode::Delta);
    plain.apply_account_state(
        &AccountState {
            venue: "binance".into(),
            balances: vec![("USDT".to_string(), 100.0)],
            ts: 0,
            route_key: None,
        },
        "USDT",
    );
    assert_eq!(plain.balance, 100.0, "a linear engine absorbs its wallet as it always did");
}

/// The snapshot carries an inverse entry in its own sparse list, restores it, and an engine without
/// one keeps the JSON (and the hash) it always had.
#[test]
fn the_snapshot_carries_the_inverse_entry_and_a_linear_one_is_unchanged() {
    let a = inverse_account();
    let snap = a.snapshot();
    assert_eq!(snap.inverse, vec![("BTC-PERPETUAL".to_string(), 1.0)]);
    assert!(snap.mult.is_empty());
    let back = Account::restore(&snap);
    assert_eq!(back.contract_value_of("BTC-PERPETUAL"), ContractValue::inverse(1.0));
    assert_eq!(back.contract_value_of("ETH-PERPETUAL"), ContractValue::linear(1.0));
    assert_eq!(back.snapshot(), snap);

    let linear = Account::new(1.0, "binance", None, BalanceMode::Delta).snapshot();
    let json = serde_json::to_string(&linear).unwrap();
    assert!(!json.contains("inverse"), "an engine with no inverse symbol writes no key: {json}");
    let old = json.clone();
    assert_eq!(serde_json::from_str::<vike_exec::AccountSnapshot>(&old).unwrap(), linear);
}

/// An inverse engine cannot value a fill on an instrument it did not read the row of: it is DROPPED
/// (never folded as a linear contract), (the predicate below is what tells the two apart).
#[test]
fn an_inverse_engine_drops_a_fill_on_an_instrument_of_unknown_kind() {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{EventHandler, ExecutionEngine, Fold, Outbox};
    use vike_model::events::Event;
    let mut e = ExecutionEngine::new(
        inverse_account(),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        "BTC-PERPETUAL",
    );
    e.extra_symbols = vec!["ETH-PERPETUAL".to_string()];
    let mut foreign = fill("t9", 1, 30.0, 3_000.0, 0.0, "USD");
    foreign.symbol = "ETH-PERPETUAL".into();
    let mut ob = Outbox::default();
    assert!(matches!(e.on_event(&Event::Fill(foreign), &mut ob), Fold::Dropped));
    assert!(e.account.positions.is_empty(), "nothing was folded");
    assert!(e.account.kind_is_unknown_for("ETH-PERPETUAL"));
    assert!(!e.account.kind_is_unknown_for("BTC-PERPETUAL"));
    // a linear engine (no inverse symbol) never drops on this ground
    assert!(!Account::new(1.0, "binance", None, BalanceMode::Delta).kind_is_unknown_for("X"));
}

/// The inverse PnL against hand-computed cases from Deribit's own contract formula for an inverse
/// perpetual, `PnL(BTC) = USD · (1/entry − 1/exit)` (the numbers are written out; the repo holds no
/// `profit_loss` capture with a non-zero value). Long 10,000 USD at 50,000 closed at 55,000 earns
/// 0.0181818… BTC, i.e. exactly 1,000 USD at the exit; two adds (10,000 at 50,000 and 10,000 at
/// 40,000, harmonic average 44,444.44…) closed at 50,000 earn 0.05 BTC = 2,500 USD at the exit,
/// which is also the sum of the two legs closed separately (0 + 10,000 · (1/40,000 − 1/50,000)).
#[test]
fn the_inverse_pnl_matches_the_hand_computed_deribit_formula() {
    let key = ("deribit".into(), "BTC-PERPETUAL".into(), PositionSide::Both);
    let mut a = inverse_account();
    a.apply_fill(&fill("o1", 1, 10_000.0, 50_000.0, 0.0, "USD"));
    a.apply_fill(&fill("c1", -1, 10_000.0, 55_000.0, 0.0, "USD"));
    let coin: f64 = 10_000.0 * (1.0 / 50_000.0 - 1.0 / 55_000.0);
    assert!((coin - 0.018_181_818_181_818).abs() < 1e-12);
    assert!((a.realized_pnl - 1_000.0).abs() < 1e-9, "{}", a.realized_pnl);
    assert!((a.realized_pnl - coin * 55_000.0).abs() < 1e-9);
    assert_eq!(a.positions[&key].size, 0.0);

    let mut b = inverse_account();
    b.apply_fill(&fill("o2", 1, 10_000.0, 50_000.0, 0.0, "USD"));
    b.apply_fill(&fill("o3", 1, 10_000.0, 40_000.0, 0.0, "USD"));
    assert!((b.positions[&key].avg_px - 44_444.444_444_444_44).abs() < 1e-6);
    b.apply_fill(&fill("c2", -1, 20_000.0, 50_000.0, 0.0, "USD"));
    assert!((b.realized_pnl - 2_500.0).abs() < 1e-9, "{}", b.realized_pnl);
    // a short is the exact mirror
    let mut s = inverse_account();
    s.apply_fill(&fill("o4", -1, 10_000.0, 50_000.0, 0.0, "USD"));
    s.apply_fill(&fill("c3", 1, 10_000.0, 55_000.0, 0.0, "USD"));
    assert!((s.realized_pnl + 1_000.0).abs() < 1e-9, "{}", s.realized_pnl);
}
