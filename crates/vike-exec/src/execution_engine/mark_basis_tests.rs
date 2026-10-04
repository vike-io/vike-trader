use super::*;
use crate::MarkSource;
use crate::execution_engine::test_clients::RecordingClient;
use crate::{Account, BalanceMode, RiskGate, RiskLimits};
use vike_model::OrderRequest;

fn engine_with(limits: RiskLimits, mark: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    e.equity_seed = 1_000_000.0;
    e.account.set_mark_from("sim", "BTCUSDT", mark, MarkSource::VenueMark, 0);
    e.price_board.set_mark("sim", "BTCUSDT", mark, 0);
    e
}

/// **The cap is a comparison, not a `min`, and NaN is why.**
///
/// `f64::min` treats NaN as the missing operand and returns the other one, so a poisoned
/// equity would come out of [`ExecutionEngine::cap_sizing_equity`] as the operator's ceiling —
/// a finite, plausible number the gate then admits against, where the NaN it replaced fails
/// every comparison and denies. A ceiling may only ever LOWER a real figure.
///
/// The other three rows are the ordinary contract: unarmed is BIT-identical (not merely equal),
/// an armed ceiling binds only above itself, and it never RAISES a figure below it — including
/// a negative one, which is what an account underwater on unrealized PnL looks like.
#[test]
fn the_sizing_cap_only_ever_lowers_and_never_launders_a_nan() {
    let unarmed = engine_with(RiskLimits::new(), 100.0);
    assert_eq!(
        unarmed.cap_sizing_equity(50_000.0).to_bits(),
        50_000.0_f64.to_bits(),
        "no ceiling ⇒ the figure is returned verbatim, not min'd against an infinity"
    );
    assert!(unarmed.cap_sizing_equity(f64::NAN).is_nan(), "and a NaN stays a NaN");

    let armed =
        engine_with(RiskLimits { max_sizing_equity: Some(20_000.0), ..RiskLimits::new() }, 100.0);
    assert_eq!(armed.cap_sizing_equity(50_000.0), 20_000.0, "above the ceiling ⇒ the ceiling");
    assert_eq!(armed.cap_sizing_equity(5_000.0), 5_000.0, "below it ⇒ untouched, never raised");
    assert_eq!(armed.cap_sizing_equity(-500.0), -500.0, "and a NEGATIVE figure is not raised");
    assert!(
        armed.cap_sizing_equity(f64::NAN).is_nan(),
        "a poisoned equity must NOT come back as the ceiling — `f64::min` would have done \
             exactly that, turning an unusable number into an admissible one"
    );
}

fn limit(coid: &str, px: f64, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        order_type: "limit".into(),
        side: 1,
        qty,
        price: Some(px),
        ts: 1,
        ..Default::default()
    }
}

/// ⚠ **THE PRICE COLLAR COULD NOT FIRE FOR A LIMIT ORDER.**
///
/// `risk_ctx` set `mark_price` from `request.price` when the order had one, and `check_inner`'s
/// collar compares `|req.price - ctx.mark_price| > band`. With the two equal that difference is
/// always ZERO, so a fat-finger limit at any distance from the true mark was admitted by the
/// very axis meant to catch it. Its own comment claims "a price 10x ABOVE the mark and one 10x
/// BELOW are the same fat finger" — while comparing the price to itself.
///
/// Mark 100, band 10%: a limit at 1000 is 10x out and must be DENIED.
///
/// NON-VACUOUS: a limit INSIDE the band is asserted admitted in the same test, so this is the
/// collar working rather than a blanket refusal of limit orders.
#[test]
fn the_price_collar_fires_on_a_fat_finger_limit() {
    let limits = RiskLimits {
        price_collar: Some(crate::PriceCollar { pct: 0.10, abs_floor: 0.0 }),
        ..RiskLimits::new()
    };

    let mut e = engine_with(limits.clone(), 100.0);
    let mut outbox = Outbox::default();
    e.submit_order(&limit("c1", 1000.0, 1.0), 1, &mut outbox);
    assert!(
        e.client.submissions.is_empty(),
        "a limit 10x above the mark must be denied by the collar — it compared the price to \
             ITSELF, so the collar was dead for every priced order"
    );
    assert!(
        outbox.0.iter().any(|ev| matches!(ev, Event::OrderDenied(_))),
        "and it denies through the normal veto path"
    );

    // ...and one INSIDE the band still goes.
    let mut ok = engine_with(limits, 100.0);
    let mut ob2 = Outbox::default();
    ok.submit_order(&limit("c2", 103.0, 1.0), 1, &mut ob2);
    assert_eq!(ok.client.submissions.len(), 1, "3% from the mark is inside a 10% band");
}

/// ⚠ **The projected-exposure cap must value at the MARK, not the order's price.**
///
/// A far-from-mark limit understated what the account would actually be exposed to once filled.
/// Mark 100, cap 500: 10 units is 1000 of exposure at the mark and must be denied — but at a
/// limit price of 10 it computed 100 and passed.
///
/// NON-VACUOUS: the same order under a cap that ACCOMMODATES the mark-valued exposure is
/// asserted admitted, so the denial is the basis and not the cap alone.
#[test]
fn the_exposure_cap_values_at_the_mark_not_the_order_price() {
    let deny = RiskLimits { max_total_exposure: Some(500.0), ..RiskLimits::new() };
    let mut e = engine_with(deny, 100.0);
    let mut outbox = Outbox::default();
    e.submit_order(&limit("c1", 10.0, 10.0), 1, &mut outbox);
    assert!(
        e.client.submissions.is_empty(),
        "10 units at a MARK of 100 is 1000 of exposure against a 500 cap — pricing it at the \
             limit (10) computed 100 and admitted it"
    );

    let allow = RiskLimits { max_total_exposure: Some(2_000.0), ..RiskLimits::new() };
    let mut ok = engine_with(allow, 100.0);
    let mut ob2 = Outbox::default();
    ok.submit_order(&limit("c2", 10.0, 10.0), 1, &mut ob2);
    assert_eq!(ok.client.submissions.len(), 1, "1000 of exposure fits a 2000 cap");
}
