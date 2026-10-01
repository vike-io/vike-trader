use super::*;
use crate::equity::equity_curve_from_trades;
use crate::trades::reconstruct_trades;
use vike_model::events::TradeId;

/// A `FillEvent` on a single venue/symbol/one-way position — mirrors `trades.rs`'s helper.
fn fill(side: i32, qty: f64, px: f64, ts: i64, commission: f64) -> FillEvent {
    FillEvent {
        // minted here, not read off a wire — same `t<ts>-<side>` bytes as before
        trade_id: TradeId::prefixed("t", format_args!("{ts}-{side}")),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    }
}

/// A step price schedule keyed on the sample ts — no store needed for the pure core.
fn stepped(ts: i64) -> Option<f64> {
    match ts {
        10 => Some(100.0),
        20 => Some(110.0),
        30 => Some(120.0),
        40 => Some(120.0),
        _ => None,
    }
}

#[test]
fn mtm_curve_marks_between_and_after_close() {
    // buy 1 @ 100 (ts 10), sell 1 @ 120 (ts 30); marks at open / midpoint / close / past.
    let fills = vec![fill(1, 1.0, 100.0, 10, 0.0), fill(-1, 1.0, 120.0, 30, 0.0)];
    let marks = [10, 20, 30, 40];
    let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| stepped(ts));

    assert_eq!(pts.len(), 4, "one sample per mark — none dropped");
    // ts10: long 1 @ 100 marked at 100 → flat unrealized.
    assert_eq!(pts[0].equity, 1_000.0);
    assert_eq!(pts[0].gross_notional, 100.0);
    assert_eq!(pts[0].traded_notional, 100.0);
    // ts20: still long, marked at 110 → +10 unrealized (MTM between opens/closes).
    assert_eq!(pts[1].equity, 1_010.0);
    assert_eq!(pts[1].unrealized, 10.0);
    assert_eq!(pts[1].gross_notional, 110.0);
    assert_eq!(pts[1].net_notional, 110.0);
    // ts30: sell closes the long, realized +20, flat → unrealized 0.
    assert_eq!(pts[2].equity, 1_020.0);
    assert_eq!(pts[2].realized, 20.0);
    assert_eq!(pts[2].unrealized, 0.0);
    assert_eq!(pts[2].gross_notional, 0.0);
    assert_eq!(pts[2].traded_notional, 220.0);
    // ts40: still flat, equity holds at realized.
    assert_eq!(pts[3].equity, 1_020.0);
    assert!(pts.iter().all(|p| p.missing_prices == 0));
}

#[test]
fn missing_price_is_flagged_not_dropped() {
    // buy 1 @ 100 (ts 10). At ts 20 the price is missing while the position is still OPEN.
    let fills = vec![fill(1, 1.0, 100.0, 10, 0.0)];
    let marks = [10, 20];
    let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| match ts {
        10 => Some(100.0),
        _ => None, // ts 20: gap
    });
    assert_eq!(pts.len(), 2, "the missing-price sample is EMITTED, not dropped");
    assert_eq!(pts[1].missing_prices, 1, "the gap is flagged");
    // silent-zero: the unpriceable position contributes 0.0 unrealized and 0 notional.
    assert_eq!(pts[1].unrealized, 0.0);
    assert_eq!(pts[1].gross_notional, 0.0);
    assert_eq!(pts[1].net_notional, 0.0);
    // equity still resolves (seed + realized 0 + unrealized 0).
    assert_eq!(pts[1].equity, 1_000.0);
}

#[test]
fn mtm_equals_realized_curve_when_flat() {
    // With fees, the last (flat) MTM equity is bit-identical to the realized-only fallback.
    let fills = vec![fill(1, 1.0, 100.0, 10, 0.1), fill(-1, 1.0, 120.0, 30, 0.2)];
    let marks = [10, 20, 30];
    let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| stepped(ts));

    let trades = reconstruct_trades(&fills);
    let (eq, _ts) = equity_curve_from_trades(1_000.0, &trades);
    let realized_last = *eq.last().unwrap();
    let mtm_last = pts.last().unwrap().equity;
    assert_eq!(
        mtm_last.to_bits(),
        realized_last.to_bits(),
        "flat MTM equity must bit-match the realized-only fallback"
    );
}

#[test]
fn runtime_stats_turnover_and_exposure() {
    let fills = vec![fill(1, 1.0, 100.0, 10, 0.0), fill(-1, 1.0, 120.0, 30, 0.0)];
    let marks = [10, 20, 30, 40];
    let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| stepped(ts));
    let rs = RuntimeStats::from_points(&pts);

    // total traded notional = 100 (buy) + 120 (sell) = 220.
    assert_eq!(rs.traded_notional, 220.0);
    // turnover = traded_notional / final_equity, bit-identical to the same division here.
    assert_eq!(rs.turnover.to_bits(), (220.0_f64 / 1_020.0).to_bits());
    // peak gross notional is at ts20 (110), giving the peak margin + the peak leverage ratio.
    assert_eq!(rs.peak_margin_used, 110.0);
    assert_eq!(rs.peak_gross_exposure.to_bits(), (110.0_f64 / 1_010.0).to_bits());
    // all-long book → net leverage equals gross leverage.
    assert_eq!(rs.peak_net_exposure.to_bits(), (110.0_f64 / 1_010.0).to_bits());
}

#[test]
fn empty_fills_and_empty_points_are_inert() {
    let pts = reconstruct_mtm(&[], 1_000.0, &[10, 20], 1.0, |_v, _s, _ts| Some(1.0));
    // No fills → two flat samples at seed.
    assert_eq!(pts.len(), 2);
    for p in &pts {
        assert_eq!(p.equity, 1_000.0);
        assert_eq!(p.traded_notional, 0.0);
    }
    // Empty series → all-zero summary (house sentinel), never a divide-by-zero.
    let rs = RuntimeStats::from_points(&[]);
    assert_eq!(rs.turnover, 0.0);
    assert_eq!(rs.peak_gross_exposure, 0.0);
    assert_eq!(rs.peak_margin_used, 0.0);
}

#[test]
fn short_position_marks_and_nets_signed() {
    // sell 1 @ 120 (ts 10): a short profits as price falls; net notional is negative.
    let fills = vec![fill(-1, 1.0, 120.0, 10, 0.0)];
    let marks = [10, 20];
    let pts = reconstruct_mtm(&fills, 1_000.0, &marks, 1.0, |_v, _s, ts| match ts {
        10 => Some(120.0),
        20 => Some(110.0), // price fell 10 → short gains 10
        _ => None,
    });
    assert_eq!(pts[1].unrealized, 10.0, "short gains as price falls: (110-120)*(-1)");
    assert_eq!(pts[1].equity, 1_010.0);
    assert_eq!(pts[1].gross_notional, 110.0, "|−1|·110");
    assert_eq!(pts[1].net_notional, -110.0, "−1·110 (signed short)");
    // net-exposure ratio uses the ABSOLUTE net notional; the peak is at ts10 (120/1000 = 0.12,
    // above ts20's 110/1010), proving the |·| is applied to the SIGNED short notional.
    let rs = RuntimeStats::from_points(&pts);
    assert_eq!(rs.peak_net_exposure.to_bits(), (120.0_f64 / 1_000.0).to_bits());
    assert_eq!(rs.peak_margin_used, 120.0, "peak gross notional is the ts10 |−1|·120");
}
