use super::*;
use vike_marketdata::test_support::flat_bar_zero_volume;

const HOUR: i64 = 3_600_000;

/// A coarse bar carries the SUM of every point due by it, and nothing is dropped inside the run:
/// daily bars over 8h settlements, the 00:00 point on its own day and the 08:00 and 16:00 ones on
/// the next day's bar (due by its open).
#[test]
fn a_coarse_bar_sums_every_point_due_by_it() {
    let day = 24 * HOUR;
    let mut price = vec![flat_bar_zero_volume(0, 100.0), flat_bar_zero_volume(day, 100.0)];
    let events = vec![(0, 0.01), (8 * HOUR, 0.02), (16 * HOUR, 0.03)];
    let stats = window_join_funding(&mut price, &events);
    assert_eq!(price[0].funding, Some(0.01));
    assert_eq!(price[1].funding.map(f64::to_bits), Some((0.02_f64 + 0.03).to_bits()));
    assert_eq!(stats, FundingJoinStats { placed: 2, collisions: 1, dropped_before: 0 });
}

/// A point after the last bar belongs to no bar of the run; one before the first is dropped.
#[test]
fn points_outside_the_bars_reach_no_bar() {
    let mut price =
        vec![flat_bar_zero_volume(10 * HOUR, 1.0), flat_bar_zero_volume(11 * HOUR, 1.0)];
    let stats =
        window_join_funding(&mut price, &[(5 * HOUR, 0.1), (11 * HOUR, 0.2), (12 * HOUR, 0.3)]);
    assert_eq!((price[0].funding, price[1].funding), (None, Some(0.2)));
    assert_eq!(stats.dropped_before, 1);
}

/// A settlement venue's points are the charges, mark and all; a continuous venue's hour is cut into
/// sixty one-minute slices ending at the point, the rate shared out and the mark kept.
#[test]
fn the_charges_follow_the_venues_accrual() {
    let points = [(8 * HOUR, 0.0003, Some(76_000.0)), (16 * HOUR, -0.0001, None)];
    let settled = funding_charges(&points, FundingAccrual::of_venue("binance"));
    assert_eq!(
        settled,
        vec![
            FundingRatePoint {
                timestamp_ms: 8 * HOUR,
                rate: 0.0003,
                premium: None,
                mark: Some(76_000.0)
            },
            FundingRatePoint { timestamp_ms: 16 * HOUR, rate: -0.0001, premium: None, mark: None },
        ]
    );
    let accrual = FundingAccrual::of_venue("deribit");
    assert_eq!(accrual, FundingAccrual::Continuous { period_ms: HOUR });
    let sliced = funding_charges(&[(HOUR, 0.0006, Some(2_000.0))], accrual);
    assert_eq!(sliced.len(), 60);
    assert_eq!(
        (sliced[0].timestamp_ms, sliced[59].timestamp_ms),
        (ACCRUAL_SLICE_MS, HOUR),
        "the hour ENDING at the point"
    );
    assert!(sliced.iter().all(|c| c.rate == 0.0006 / 60.0 && c.mark == Some(2_000.0)));
    for venue in ["bybit", "okx", "hyperliquid", "aster"] {
        assert_eq!(FundingAccrual::of_venue(venue), FundingAccrual::AtSettlement, "{venue}");
    }
}

/// The yardstick is the series' own spacing around a point: a symbol on 1h is held to 1h, one on 8h
/// to 8h, and one re-cadenced from 8h to 4h has no hole at the change.
#[test]
fn a_gap_is_judged_against_the_series_own_spacing() {
    let hourly: Vec<i64> = (0..10).map(|h| h * HOUR).collect();
    assert!(funding_gaps("BTC", &hourly, 0, 9 * HOUR).is_empty());
    let mut holed = hourly.clone();
    holed.retain(|&t| t != 4 * HOUR && t != 5 * HOUR);
    let lines = funding_gaps("BTC", &holed, 0, 9 * HOUR);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("1 gap(s)") && lines[0].contains("3.0h"), "{}", lines[0]);
    // 8h settlements are not a hole at 8h apart, and a re-cadence DOWN (8h then 4h) is none either.
    let mixed = [0, 8 * HOUR, 16 * HOUR, 20 * HOUR, 24 * HOUR, 28 * HOUR];
    assert!(funding_gaps("ETH", &mixed, 0, 28 * HOUR).is_empty());
    // A series that starts or ends far inside the run says so.
    let edges = funding_gaps("ETH", &[10 * HOUR, 11 * HOUR, 12 * HOUR], 0, 20 * HOUR);
    assert_eq!(edges.len(), 2, "{edges:?}");
    assert!(funding_gaps("ETH", &[HOUR], 0, 20 * HOUR).is_empty(), "one point: no spacing");
}
