//! `Account::apply_fee_adjustment` DIRECTLY, with no engine in front: booked once by the difference,
//! converted like a fill's fee, refused for a fill it never folded or another venue's, and carried
//! through the snapshot. The engine-level suite (`tests/engine/fee_adjustment_fold.rs`) drives the
//! same ledger through `on_event`.

use super::*;
use vike_model::events::{FeeAdjustmentEvent, FillEvent, TradeId};

fn fill(trade_id: &str, symbol: &str, px: f64, comm: f64, asset: &str) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new(trade_id).expect("test ids are non-empty"),
        client_order_id: "c1".to_string(),
        venue: "ibkr".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: 2.0,
        last_px: px,
        commission: comm,
        commission_asset: asset.into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

fn adj(trade_id: &str, symbol: &str, px: f64, extra: f64, asset: &str) -> FeeAdjustmentEvent {
    FeeAdjustmentEvent {
        trade_id: TradeId::new(trade_id).expect("test ids are non-empty"),
        client_order_id: "c1".to_string(),
        venue: "ibkr".into(),
        symbol: symbol.into(),
        last_px: px,
        extra_commission: extra,
        commission_asset: asset.into(),
        ts: 2,
    }
}

fn acct() -> Account {
    Account::new(1.0, "ibkr", None, BalanceMode::Delta)
}

fn usd(a: &Account) -> Option<f64> {
    a.fees_by_asset.get(&Ustr::from("USD")).copied()
}

#[test]
fn a_correction_books_once_and_a_repeat_books_nothing() {
    let mut a = acct();
    a.apply_fill(&fill("t1", "AAPL", 190.0, 0.0, ""));
    assert_eq!(
        a.apply_fee_adjustment(&adj("t1", "AAPL", 190.0, 1.25, "USD")),
        FeeFold::Applied { charge: 1.25 }
    );
    assert_eq!((a.balance, a.fees_paid, usd(&a)), (-1.25, 1.25, Some(1.25)));
    assert_eq!(a.apply_fee_adjustment(&adj("t1", "AAPL", 190.0, 1.25, "USD")), FeeFold::Unchanged);
    assert_eq!(
        (a.balance, a.fees_paid, usd(&a)),
        (-1.25, 1.25, Some(1.25)),
        "the repeat moved nothing"
    );
}

#[test]
fn a_changed_total_books_the_difference_and_a_rebate_is_negative() {
    let mut a = acct();
    a.apply_fill(&fill("t1", "AAPL", 190.0, 0.0, ""));
    a.apply_fee_adjustment(&adj("t1", "AAPL", 190.0, 1.25, "USD"));
    match a.apply_fee_adjustment(&adj("t1", "AAPL", 190.0, 1.00, "USD")) {
        FeeFold::Applied { charge } => assert!((charge + 0.25).abs() < 1e-12, "{charge}"),
        other => panic!("expected the difference, got {other:?}"),
    }
    assert!((a.fees_paid - 1.00).abs() < 1e-12 && (a.balance + 1.00).abs() < 1e-12);
    assert!((usd(&a).unwrap() - 1.00).abs() < 1e-12);
}

#[test]
fn a_fill_never_folded_or_another_venue_books_nothing() {
    let mut a = acct();
    assert_eq!(
        a.apply_fee_adjustment(&adj("ghost", "AAPL", 190.0, 1.0, "USD")),
        FeeFold::UnknownFill
    );
    a.apply_fill(&fill("t1", "AAPL", 190.0, 0.0, ""));
    let mut foreign = adj("t1", "AAPL", 190.0, 1.0, "USD");
    foreign.venue = "binance".into();
    assert_eq!(a.apply_fee_adjustment(&foreign), FeeFold::ForeignVenue);
    assert_eq!((a.balance, a.fees_paid, usd(&a)), (0.0, 0.0, None));
}

/// The correction converts like the fee of a fill: on an INVERSE contract a coin fee enters the
/// one-currency ledger at the fill's price (decision 0170), so 0.001 BTC at 50 000 is 50.
#[test]
fn an_inverse_contracts_coin_fee_enters_at_the_fills_price() {
    let values: IndexMap<String, vike_model::ContractValue> =
        [("BTC-PERPETUAL".to_string(), vike_model::ContractValue::inverse(1.0))]
            .into_iter()
            .collect();
    let mut a = Account::new(1.0, "ibkr", Some(values), BalanceMode::Delta);
    a.apply_fill(&fill("t1", "BTC-PERPETUAL", 50_000.0, 0.0, ""));
    match a.apply_fee_adjustment(&adj("t1", "BTC-PERPETUAL", 50_000.0, 0.001, "BTC")) {
        FeeFold::Applied { charge } => assert!((charge - 50.0).abs() < 1e-9, "{charge}"),
        other => panic!("expected a booking, got {other:?}"),
    }
    assert!((a.fees_paid - 50.0).abs() < 1e-9);
    assert_eq!(
        a.fees_by_asset.get(&Ustr::from("BTC")).copied(),
        Some(0.001),
        "the coin amount, per asset"
    );
}

/// The first fill carried no currency and the correction names one: the per-asset tally is the
/// correction's alone, and a later correction that moves to ANOTHER currency takes the old amount
/// back from the old one.
#[test]
fn a_correction_that_changes_the_currency_takes_the_old_amount_back() {
    let mut a = acct();
    a.apply_fill(&fill("t1", "AAPL", 190.0, 0.0, ""));
    a.apply_fee_adjustment(&adj("t1", "AAPL", 190.0, 1.0, "USD"));
    a.apply_fee_adjustment(&adj("t1", "AAPL", 190.0, 2.0, "EUR"));
    assert!((usd(&a).unwrap()).abs() < 1e-12, "the old currency is taken back to zero");
    assert_eq!(a.fees_by_asset.get(&Ustr::from("EUR")).copied(), Some(2.0));
}

#[test]
fn the_ledger_round_trips_through_the_snapshot_and_is_absent_while_empty() {
    let mut a = acct();
    a.apply_fill(&fill("t1", "AAPL", 190.0, 0.0, ""));
    assert!(a.snapshot().fee_adjusted.is_empty(), "sparse until a fee is corrected");
    a.apply_fee_adjustment(&adj("t1", "AAPL", 190.0, 1.25, "USD"));
    let snap = a.snapshot();
    assert_eq!(snap.fee_adjusted, vec![("t1".to_string(), 1.25, Ustr::from("USD"), 1.25)]);

    let mut restored = Account::restore(&snap);
    restored.seed_seen_fill_ids(["t1".to_string()]);
    assert_eq!(
        restored.apply_fee_adjustment(&adj("t1", "AAPL", 190.0, 1.25, "USD")),
        FeeFold::Unchanged
    );
    assert_eq!(restored.snapshot(), snap);
}
