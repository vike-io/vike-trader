use super::*;

const DAY: i64 = 86_400_000;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn fill(day: i64, low: f64, end: f64) -> Close {
    Close { timestamp: day * DAY, low, end }
}

/// The closed-trade equity from 100: up to 110, down to 98 (a fill whose rows passed 98 and ended at
/// 104), past 110 to 115, a fill that dipped to 112 and ended at 116 (no drawdown), down to 92 and
/// 96 at the end.
#[test]
fn growth_and_decline_on_the_closed_trade_equity() {
    let closes = [
        fill(1, 100.0, 110.0),
        fill(2, 99.0, 99.0),
        fill(3, 98.0, 104.0),
        fill(4, 104.0, 115.0),
        fill(5, 112.0, 116.0),
        fill(6, 92.0, 95.0),
        fill(7, 95.0, 96.0),
    ];
    let p = phases((0, 100.0), &closes);
    let got = |xs: &[Phase]| -> Vec<(f64, f64, f64)> {
        xs.iter().map(|x| (x.percent, x.days, x.money)).collect()
    };
    let same = |a: Vec<(f64, f64, f64)>, b: &[(f64, f64, f64)]| {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| close(x.0, y.0) && close(x.1, y.1) && close(x.2, y.2))
    };
    // 110 -> 98 (day 1 to day 3) and 116 -> 92, still open (day 5 to day 6): peak to the lowest point.
    assert!(
        same(got(&p.declines), &[(12.0 / 110.0, 2.0, 12.0), (24.0 / 116.0, 1.0, 24.0)]),
        "{p:?}"
    );
    // 100 -> 110, 98 -> 116 and 92 -> 96 in progress: trough to peak.
    let rises = [(0.1, 1.0, 10.0), (18.0 / 98.0, 2.0, 18.0), (4.0 / 92.0, 1.0, 4.0)];
    assert!(same(got(&p.rises), &rises), "{p:?}");
    assert!(close(p.now_rise, 4.0 / 92.0));
    let [drawdown, drawdown_days, ru, ru_days] = phase_means(&p);
    assert!(close(drawdown, (12.0 / 110.0 + 24.0 / 116.0) / 2.0) && close(drawdown_days, 1.5));
    assert!(close(ru, (0.1 + 18.0 / 98.0 + 4.0 / 92.0) / 3.0) && close(ru_days, 4.0 / 3.0));
    let top = highest_rise(&p).expect("three run-ups");
    assert!(close(top.percent, 18.0 / 98.0) && close(top.money, 18.0));
}

#[test]
fn an_equity_that_never_falls_is_one_run_up_in_progress() {
    let p = phases((0, 100.0), &[fill(1, 100.0, 101.0), fill(2, 101.0, 103.0)]);
    assert!(p.declines.is_empty());
    assert_eq!(p.rises.len(), 1);
    assert!(close(p.now_rise, 0.03) && close(p.rises[0].days, 2.0));
    assert_eq!(phases((0, 100.0), &[]), Phases::default(), "one row is no phase");
    assert_eq!(highest_rise(&Phases::default()), None);
}

#[test]
fn the_deepest_fall_starts_at_the_first_value() {
    let values = [100.0, 110.0, 99.0, 98.0, 104.0, 115.0, 112.0, 116.0, 92.0, 95.0, 96.0];
    let (fall, from) = deepest_fall(values.into_iter());
    assert!(close(fall, 24.0) && close(from, 116.0));
    assert_eq!(
        deepest_fall([90.0, 95.0].into_iter()).0,
        0.0,
        "a first loss is the start, not a fall"
    );
}

/// January ends at 110 (from 100), February at 99, March at 108.9: +10%, -10%, +10%.
#[test]
fn a_month_is_its_last_close_over_the_month_before() {
    let points = [(14 * DAY, 105.0), (30 * DAY, 110.0), (40 * DAY, 99.0), (59 * DAY, 108.9)];
    let r = monthly_returns(100.0, &points);
    assert_eq!(r.len(), 3);
    assert!(close(r[0], 0.1) && close(r[1], -0.1) && close(r[2], 0.1), "{r:?}");
}

/// The three months above at 2% a year, under each choice the documentation leaves open.
#[test]
fn the_monthly_ratios_under_each_reading() {
    let r = [0.1, -0.1, 0.1];
    let (sharpe, sortino) = sharpe_sortino(&r, 0.02, TradeClosedAccountRatio::DOCUMENTED);
    assert!(
        close(sharpe, 0.3358757210636101) && close(sortino, 0.5394912351444044),
        "{sharpe} {sortino}"
    );
    let sample = TradeClosedAccountRatio {
        divisor: TradeClosedAccountDivisor::Sample,
        ..TradeClosedAccountRatio::DOCUMENTED
    };
    let (sharpe, sortino) = sharpe_sortino(&r, 0.02, sample);
    assert!(close(sharpe, 0.27424137786507224) && close(sortino, 0.44049274893588203));
    let yearly =
        TradeClosedAccountRatio { annualised: true, ..TradeClosedAccountRatio::DOCUMENTED };
    assert!(close(sharpe_sortino(&r, 0.02, yearly).0, 1.1635076278220096));
    let compound = TradeClosedAccountRatio {
        monthly_rate: TradeClosedAccountMonthlyRate::Compound,
        ..TradeClosedAccountRatio::DOCUMENTED
    };
    assert!(close(sharpe_sortino(&r, 0.02, compound).0, 0.33603572551924327));
    assert_eq!(
        sharpe_sortino(&[0.1], 0.02, sample),
        (0.0, 0.0),
        "one month has no sample deviation"
    );
    assert_eq!(sharpe_sortino(&[], 0.02, TradeClosedAccountRatio::DOCUMENTED), (0.0, 0.0));
}

#[test]
fn growth_compounds_over_calendar_days() {
    assert!(close(cagr(0.21, 730.0), 0.1));
    assert!(close(cagr(-0.0004, 5.0), -0.028783472833302515));
    assert_eq!(cagr(-1.5, 100.0), -1.0, "an account that is gone");
    assert_eq!(cagr(0.1, 0.0), 0.0, "no time");
}

#[test]
fn every_account_field_answers_by_its_name() {
    let json = serde_json::to_value(TradeClosedAccountStatistics::default()).expect("serializes");
    for key in json.as_object().expect("an object").keys() {
        assert!(
            TradeClosedAccountStatistics::default().value_of(key).is_some(),
            "`{key}` has no value_of arm"
        );
    }
    assert_eq!(TradeClosedAccountStatistics::default().value_of("trades"), None);
}
