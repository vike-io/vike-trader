//! The path grammar's tests (§3.1).

use vike_model::MS_PER_DAY;

use super::{day, now};
use crate::archive::path::{Bi5Day, Bi5Path, DAILY_SUFFIX, classify_bi5_path, is_bi5_layout_dir};

// ── the path grammar (§3.1) ─────────────────────────────────────────────────────────────────

#[test]
fn the_month_is_zero_based() {
    let jan_15 = classify_bi5_path(&["2024", "00", "15_ticks.bi5"], now());
    assert_eq!(jan_15, Bi5Path::Daily(day(2024, 1, 15)));
    let Bi5Path::Daily(d) = jan_15 else { unreachable!() };
    assert_eq!(d.start_ms(), 1_705_276_800_000, "2024-01-15T00:00:00Z");
    assert_eq!(d.to_string(), "2024-01-15");
    assert_eq!(
        classify_bi5_path(&["2024", "11", "31_ticks.bi5"], now()),
        Bi5Path::Daily(day(2024, 12, 31)),
        "11 is December"
    );
}

#[test]
fn month_12_is_refused() {
    assert_eq!(classify_bi5_path(&["2024", "12", "01_ticks.bi5"], now()), Bi5Path::Other);
    assert!(!is_bi5_layout_dir(&["2024", "12"], now()));
}

#[test]
fn day_00_day_32_and_30_february_are_refused() {
    for (month0, dom) in [("00", "00"), ("00", "32"), ("01", "30"), ("01", "31"), ("03", "31")] {
        let file = format!("{dom}{DAILY_SUFFIX}");
        assert_eq!(
            classify_bi5_path(&["2024", month0, file.as_str()], now()),
            Bi5Path::Other,
            "2024/{month0}/{file}"
        );
        assert!(!is_bi5_layout_dir(&["2024", month0, dom], now()), "2024/{month0}/{dom}");
    }
    // 29 February exists in 2024 and not in 2023
    assert_eq!(
        classify_bi5_path(&["2024", "01", "29_ticks.bi5"], now()),
        Bi5Path::Daily(day(2024, 2, 29))
    );
    assert_eq!(classify_bi5_path(&["2023", "01", "29_ticks.bi5"], now()), Bi5Path::Other);
}

#[test]
fn the_year_runs_from_1970_to_the_current_year() {
    let at = |year: &str| classify_bi5_path(&[year, "00", "02_ticks.bi5"], now());
    assert_eq!(at("1969"), Bi5Path::Other);
    assert_eq!(at("1970"), Bi5Path::Daily(day(1970, 1, 2)));
    assert_eq!(at("2026"), Bi5Path::Daily(day(2026, 1, 2)), "the current year");
    assert_eq!(at("2027"), Bi5Path::Other, "a year after now");
    assert_eq!(at("24"), Bi5Path::Other, "four digits, exactly");
    assert_eq!(at("02024"), Bi5Path::Other);
    assert!(is_bi5_layout_dir(&["1970"], now()));
    assert!(!is_bi5_layout_dir(&["1969"], now()));
    assert!(!is_bi5_layout_dir(&["2027"], now()));
}

#[test]
fn the_hourly_layout_is_recognised_not_imported() {
    assert_eq!(
        classify_bi5_path(&["2024", "00", "15", "10h_ticks.bi5"], now()),
        Bi5Path::Hourly { day: day(2024, 1, 15), hour: 10 }
    );
    assert_eq!(
        classify_bi5_path(&["2024", "00", "15", "00h_ticks.bi5"], now()),
        Bi5Path::Hourly { day: day(2024, 1, 15), hour: 0 }
    );
    for file in ["24h_ticks.bi5", "10_ticks.bi5", "1h_ticks.bi5", "10H_ticks.bi5"] {
        assert_eq!(classify_bi5_path(&["2024", "00", "15", file], now()), Bi5Path::Other, "{file}");
    }
    // the hourly layout under an impossible day is not a layout at all
    assert_eq!(classify_bi5_path(&["2024", "01", "30", "10h_ticks.bi5"], now()), Bi5Path::Other);
    assert!(is_bi5_layout_dir(&["2024", "00", "15"], now()));
}

#[test]
fn candle_and_other_names_are_other() {
    let others: [&[&str]; 13] = [
        &["2024", "00", "15", "BID_candles_min_1.bi5"],
        &["2024", "00", "15", "ASK_candles_min_1.bi5"],
        &["2024", "00", "BID_candles_hour_1.bi5"],
        &["2024", "BID_candles_day_1.bi5"],
        &["2024", "00", "15_ticks.bi5.part"],
        &["2024", "00", "15_TICKS.BI5"],
        &["2024", "00", "5_ticks.bi5"],
        &["2024", "0", "15_ticks.bi5"],
        &["2024", "00", "15_ticks.bi5", "x"],
        &["2024", "00", "15", "10h_ticks.bi5", "x"],
        &["README.md"],
        &["15_ticks.bi5"],
        &[],
    ];
    for rel in others {
        assert_eq!(classify_bi5_path(rel, now()), Bi5Path::Other, "{rel:?}");
    }
}

#[test]
fn a_walk_descends_only_into_layout_directories() {
    assert!(is_bi5_layout_dir(&["2024"], now()));
    assert!(is_bi5_layout_dir(&["2024", "00"], now()));
    assert!(is_bi5_layout_dir(&["2024", "11"], now()));
    assert!(is_bi5_layout_dir(&["2024", "00", "15"], now()));
    for rel in [
        &[][..],
        &["candles"][..],
        &["2024", "Jan"][..],
        &["2024", "00", "15", "10"][..],
        &["2024", "00", "15_ticks.bi5"][..],
    ] {
        assert!(!is_bi5_layout_dir(rel, now()), "{rel:?}");
    }
}

#[test]
fn a_day_is_a_utc_midnight_on_or_after_the_epoch() {
    assert_eq!(Bi5Day::from_start_ms(0).map(|d| d.to_string()), Some("1970-01-01".to_string()));
    assert_eq!(Bi5Day::from_start_ms(MS_PER_DAY - 1), None);
    assert_eq!(Bi5Day::from_start_ms(MS_PER_DAY + 1), None);
    assert_eq!(Bi5Day::from_start_ms(-MS_PER_DAY), None);
    assert_eq!(Bi5Day::from_start_ms(1_705_276_800_000), Some(day(2024, 1, 15)));
}
