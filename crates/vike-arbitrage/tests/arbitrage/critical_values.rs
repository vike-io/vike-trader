use vike_arbitrage::cointegration::JohansenCase;
use vike_arbitrage::cointegration::critical_values::{
    JohansenStatistic, Significance, engle_granger_critical_values, johansen_critical_values,
};

const CASES: [JohansenCase; 3] = [
    JohansenCase::NoConstant,
    JohansenCase::RestrictedConstant,
    JohansenCase::UnrestrictedConstant,
];

/// The response surface by hand: `N = 2`, 5%, `T = 100` is `−3.33613 − 6.1101/100 − 6.823/100²`; `N = 3`, 1%
/// has the `β₃` the paper printed: `−4.29374 − 14.4354/100 − 33.195/100² + 47.433/100³`.
#[test]
fn the_engle_granger_response_surface_by_hand() {
    let two = engle_granger_critical_values(2, 100).unwrap();
    assert!((two.five_percent - -3.3979133).abs() < 1e-9, "{two:?}");
    let three = engle_granger_critical_values(3, 100).unwrap();
    assert!((three.one_percent - -4.441366067).abs() < 1e-9, "{three:?}");
    // As the sample grows the surface settles on β∞, the asymptotic value.
    let large = engle_granger_critical_values(2, 1_000_000_000).unwrap();
    assert!((large.one_percent - -3.89644).abs() < 1e-6, "{large:?}");
    assert_eq!(two.at(Significance::FivePercent), two.five_percent);
    assert_eq!(two.at(Significance::TenPercent), two.ten_percent);
    assert_eq!(two.at(Significance::OnePercent), two.one_percent);
}

/// A transcription check on the copied table: a critical value falls as the level tightens (1% below
/// 5% below 10%) and as the number of series grows.
#[test]
fn the_engle_granger_table_is_ordered() {
    for observations in [30, 100, 1000] {
        for variables in 2..=12 {
            let v = engle_granger_critical_values(variables, observations).unwrap();
            assert!(v.one_percent < v.five_percent && v.five_percent < v.ten_percent, "{v:?}");
            if variables < 12 {
                let more = engle_granger_critical_values(variables + 1, observations).unwrap();
                assert!(more.one_percent < v.one_percent, "{variables} {observations}");
                assert!(more.five_percent < v.five_percent, "{variables} {observations}");
                assert!(more.ten_percent < v.ten_percent, "{variables} {observations}");
            }
        }
    }
}

#[test]
fn the_engle_granger_values_stop_where_the_table_does() {
    assert!(engle_granger_critical_values(1, 100).is_none());
    assert!(engle_granger_critical_values(13, 100).is_none());
    assert!(engle_granger_critical_values(2, 0).is_none());
}

/// Cells of the published tables, read back: Case 1 (an unrestricted constant) `p − r = 2`, Case 0 with
/// one series left, and the last row of Case 1*.
#[test]
fn johansen_cells_by_hand() {
    let trace =
        johansen_critical_values(JohansenCase::UnrestrictedConstant, JohansenStatistic::Trace, 2)
            .unwrap();
    assert_eq!((trace.one_percent, trace.five_percent, trace.ten_percent), (19.94, 15.50, 13.43));
    let maximum = johansen_critical_values(
        JohansenCase::UnrestrictedConstant,
        JohansenStatistic::MaximumEigenvalue,
        2,
    )
    .unwrap();
    assert_eq!(
        (maximum.one_percent, maximum.five_percent, maximum.ten_percent),
        (18.52, 14.26, 12.30)
    );
    let one =
        johansen_critical_values(JohansenCase::NoConstant, JohansenStatistic::Trace, 1).unwrap();
    assert_eq!((one.one_percent, one.five_percent, one.ten_percent), (6.94, 4.13, 2.98));
    let last =
        johansen_critical_values(JohansenCase::RestrictedConstant, JohansenStatistic::Trace, 12)
            .unwrap();
    assert_eq!((last.one_percent, last.five_percent, last.ten_percent), (365.64, 348.99, 340.38));
}

/// A transcription check on the three copied tables: a critical value falls as the level loosens, grows with the
/// number of series left, the trace is never below the maximal eigenvalue's, and the two agree
/// when one series is left (the trace of one term IS the maximal-eigenvalue term).
#[test]
fn the_johansen_tables_are_ordered() {
    for case in CASES {
        for dimension in 1..=12 {
            let get = |statistic, d| johansen_critical_values(case, statistic, d).unwrap();
            let (trace, maximum) = (
                get(JohansenStatistic::Trace, dimension),
                get(JohansenStatistic::MaximumEigenvalue, dimension),
            );
            for v in [trace, maximum] {
                assert!(v.one_percent > v.five_percent && v.five_percent > v.ten_percent, "{v:?}");
            }
            assert!(trace.five_percent >= maximum.five_percent, "{case:?} {dimension}");
            if dimension == 1 {
                assert_eq!(trace, maximum, "{case:?}");
            }
            if dimension < 12 {
                for statistic in [JohansenStatistic::Trace, JohansenStatistic::MaximumEigenvalue] {
                    let (now, next) = (get(statistic, dimension), get(statistic, dimension + 1));
                    assert!(next.one_percent > now.one_percent, "{case:?} {dimension}");
                    assert!(next.five_percent > now.five_percent, "{case:?} {dimension}");
                    assert!(next.ten_percent > now.ten_percent, "{case:?} {dimension}");
                }
            }
        }
    }
}

#[test]
fn the_johansen_values_stop_where_the_table_does() {
    for case in CASES {
        assert!(johansen_critical_values(case, JohansenStatistic::Trace, 0).is_none());
        assert!(johansen_critical_values(case, JohansenStatistic::Trace, 13).is_none());
    }
}
