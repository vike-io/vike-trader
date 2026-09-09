//! Calendar-period returns and drawdown table — port of `analysis/periods.py`.
//!
//! - [`periodic_returns`]: group an equity curve into calendar buckets and compute the period
//!   return for each bucket.
//! - [`monthly_return_matrix`]: produce the year x month heatmap data structure.
//! - [`drawdown_table`]: find the top-N drawdown episodes with peak/trough/recovery timestamps
//!   and bar-counts.

use chrono::{DateTime, Datelike, Utc};
use indexmap::IndexMap;
use std::collections::BTreeMap;

fn epoch_ms_to_utc(epoch_ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(epoch_ms).expect("epoch_ms out of range for DateTime<Utc>")
}

/// A comparable label for the calendar period containing `ts_ms` (UTC).
///
/// `period` is one of `"daily"`, `"weekly"`, `"monthly"`, `"quarterly"`, `"yearly"`:
/// - daily     -> `"2024-03-15"`
/// - weekly    -> `"2024-W11"` (ISO week)
/// - monthly   -> `"2024-03"`
/// - quarterly -> `"2024-Q1"`
/// - yearly    -> `"2024"`
///
/// # Panics
///
/// Panics if `period` is not one of the supported values. That is inherited behaviour: the
/// ported `analysis/periods.py` raised `ValueError` here. Provenance only — nothing compares
/// this function against Python any more.
pub fn period_key(ts_ms: i64, period: &str) -> String {
    // Delegate "daily" to the workspace's one day-law home (`vike_model::time`, div_euclid-based)
    // so this bucket key can never drift from the store's `date=` partition math or the EOD/klines
    // day-floor, rather than re-deriving Y-M-D via `chrono` locally (dedup A-class fix). Output is
    // identical: both format `{y:04}-{m:02}-{d:02}` off the same UTC calendar day.
    if period == "daily" {
        return vike_model::time::epoch_ms_to_utc_date(ts_ms);
    }
    let dt = epoch_ms_to_utc(ts_ms);
    match period {
        "weekly" => {
            let iso = dt.iso_week();
            format!("{:04}-W{:02}", iso.year(), iso.week())
        }
        "monthly" => format!("{:04}-{:02}", dt.year(), dt.month()),
        "quarterly" => {
            let q = (dt.month() - 1) / 3 + 1;
            format!("{:04}-Q{}", dt.year(), q)
        }
        "yearly" => format!("{}", dt.year()),
        other => {
            panic!("period must be 'daily','weekly','monthly','quarterly','yearly'; got {other:?}")
        }
    }
}

/// `(label, return_fraction)` pairs grouped by calendar period, in chronological order.
///
/// `return_fraction = last_equity_in_period / first_equity_at_start_of_period - 1`.
///
/// # Panics
///
/// Panics if `equity_curve` and `timestamps` differ in length, or `period` is unsupported.
pub fn periodic_returns(
    equity_curve: &[f64],
    timestamps: &[i64],
    period: &str,
) -> Vec<(String, f64)> {
    assert_eq!(
        equity_curve.len(),
        timestamps.len(),
        "equity_curve and timestamps must have equal length"
    );
    if equity_curve.is_empty() {
        return Vec::new();
    }

    // Validate period early (panics for unknown values) — inherited from the ported
    // `analysis/periods.py`, whose `_period_label()` call ran just as early.
    let _ = period_key(timestamps[0], period);

    // label -> (first_equity, last_equity); IndexMap preserves first-insertion (chronological)
    // order, mirroring Python's insertion-ordered dict + explicit label_order list.
    let mut groups: IndexMap<String, (f64, f64)> = IndexMap::new();
    let mut prev_label: Option<String> = None;
    let mut prev_equity = equity_curve[0];

    for (&eq, &ts) in equity_curve.iter().zip(timestamps.iter()) {
        let lbl = period_key(ts, period);
        if let Some(&(entry_eq, _)) = groups.get(&lbl) {
            groups.insert(lbl.clone(), (entry_eq, eq));
        } else {
            let entry_eq = if prev_label.is_some() { prev_equity } else { equity_curve[0] };
            groups.insert(lbl.clone(), (entry_eq, eq));
        }
        prev_label = Some(lbl);
        prev_equity = eq;
    }

    groups
        .into_iter()
        .map(|(lbl, (entry_eq, last_eq))| {
            let ret = if entry_eq == 0.0 { 0.0 } else { last_eq / entry_eq - 1.0 };
            (lbl, ret)
        })
        .collect()
}

/// Year x month return heatmap, mirroring Python's `monthly_return_matrix()` dict.
#[derive(Debug, Clone, PartialEq)]
pub struct MonthlyReturnMatrix {
    pub years: Vec<i32>,
    pub matrix: BTreeMap<i32, BTreeMap<u32, Option<f64>>>,
    pub annual: BTreeMap<i32, f64>,
}

/// Build a year x month heatmap of returns.
///
/// # Panics
///
/// Panics if `equity_curve` and `timestamps` differ in length.
pub fn monthly_return_matrix(equity_curve: &[f64], timestamps: &[i64]) -> MonthlyReturnMatrix {
    let monthly = periodic_returns(equity_curve, timestamps, "monthly");

    let mut matrix: BTreeMap<i32, BTreeMap<u32, Option<f64>>> = BTreeMap::new();
    for (label, ret) in &monthly {
        let year: i32 = label[..4].parse().unwrap();
        let month: u32 = label[5..7].parse().unwrap();
        matrix.entry(year).or_default().insert(month, Some(*ret));
    }

    let mut annual: BTreeMap<i32, f64> = BTreeMap::new();
    for (&year, months) in &matrix {
        let mut product = 1.0;
        for m in 1..=12u32 {
            if let Some(Some(r)) = months.get(&m) {
                product *= 1.0 + r;
            }
        }
        annual.insert(year, product - 1.0);
    }

    let years: Vec<i32> = matrix.keys().copied().collect();
    MonthlyReturnMatrix { years, matrix, annual }
}

/// One drawdown episode, mirroring Python's `drawdown_table()` dict entries.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawdownEpisode {
    /// fractional drawdown from peak (positive, e.g. 0.20 == 20%)
    pub depth: f64,
    pub peak_ts: i64,
    pub trough_ts: i64,
    /// `None` if never recovered within the curve
    pub recovery_ts: Option<i64>,
    /// bars from peak to trough (inclusive)
    pub length: i64,
    /// bars from trough to recovery (inclusive); `None` if not recovered
    pub recovery: Option<i64>,
}

/// Find the top-N drawdown episodes sorted by depth (worst first).
///
/// # Panics
///
/// Panics if `equity_curve` and `timestamps` differ in length.
pub fn drawdown_table(
    equity_curve: &[f64],
    timestamps: &[i64],
    top_n: usize,
) -> Vec<DrawdownEpisode> {
    assert_eq!(
        equity_curve.len(),
        timestamps.len(),
        "equity_curve and timestamps must have equal length"
    );
    if equity_curve.is_empty() {
        return Vec::new();
    }

    let n = equity_curve.len();
    let mut peak_val = equity_curve[0];
    let mut peak_idx = 0i64;
    let mut in_drawdown = false;
    let mut trough_val = equity_curve[0];
    let mut trough_idx = 0i64;
    let mut episodes: Vec<DrawdownEpisode> = Vec::new();

    let close_episode = |peak_val: f64,
                         peak_idx: i64,
                         trough_val: f64,
                         trough_idx: i64,
                         recovery_idx: Option<i64>| {
        let depth = if peak_val != 0.0 { (peak_val - trough_val) / peak_val } else { 0.0 };
        let length = trough_idx - peak_idx;
        let recovery = recovery_idx.map(|r| r - trough_idx);
        DrawdownEpisode {
            depth,
            peak_ts: timestamps[peak_idx as usize],
            trough_ts: timestamps[trough_idx as usize],
            recovery_ts: recovery_idx.map(|r| timestamps[r as usize]),
            length,
            recovery,
        }
    };

    for (i, &eq) in equity_curve.iter().enumerate().take(n) {
        if eq >= peak_val {
            if in_drawdown && trough_val < peak_val {
                let ep = close_episode(peak_val, peak_idx, trough_val, trough_idx, Some(i as i64));
                if ep.depth > 0.0 {
                    episodes.push(ep);
                }
            }
            peak_val = eq;
            peak_idx = i as i64;
            trough_val = eq;
            trough_idx = i as i64;
            in_drawdown = false;
        } else {
            in_drawdown = true;
            if eq < trough_val {
                trough_val = eq;
                trough_idx = i as i64;
            }
        }
    }

    if in_drawdown && trough_val < peak_val {
        let ep = close_episode(peak_val, peak_idx, trough_val, trough_idx, None);
        if ep.depth > 0.0 {
            episodes.push(ep);
        }
    }

    episodes.sort_by(|a, b| b.depth.partial_cmp(&a.depth).unwrap());
    episodes.truncate(top_n);
    episodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32) -> i64 {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap().timestamp_millis()
    }

    // --- period_key() ---

    #[test]
    fn period_key_daily() {
        assert_eq!(period_key(ts(2024, 3, 15), "daily"), "2024-03-15");
    }

    #[test]
    fn period_key_daily_delegates_to_vike_model_time_equivalently() {
        // The "daily" arm now delegates to `vike_model::time::epoch_ms_to_utc_date` instead of
        // re-deriving Y-M-D via chrono. Prove the two ways of computing a UTC calendar day agree
        // over a range spanning pre-1970 (negative epoch-ms) through post-2024 dates — chrono's
        // `DateTime::from_timestamp_millis` and `vike_model`'s div_euclid day math must never
        // silently diverge on a boundary (midnight, leap day, negative-ms floor direction).
        let probes: &[i64] = &[
            ts(1960, 1, 1),   // pre-1970
            ts(1969, 12, 31), // day before epoch
            ts(1970, 1, 1),   // epoch day
            ts(1970, 1, 2),
            ts(2000, 2, 29), // leap day
            ts(2024, 3, 15),
            ts(2024, 12, 31),
            ts(2024, 3, 15) - 1,     // last ms of the prior UTC day
            ts(2024, 3, 15) + 1,     // first ms after midnight
            -1,                      // 1ms before the epoch
            86_400_000 * 10_000 + 1, // far future
        ];
        for &t in probes {
            let chrono_key = {
                let dt = epoch_ms_to_utc(t);
                format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
            };
            assert_eq!(
                period_key(t, "daily"),
                chrono_key,
                "vike_model::time day-floor must agree with chrono at ts={t}"
            );
        }
    }

    #[test]
    fn period_key_weekly() {
        // 2024-03-15 is ISO week 11 of 2024
        assert_eq!(period_key(ts(2024, 3, 15), "weekly"), "2024-W11");
    }

    #[test]
    fn period_key_monthly() {
        assert_eq!(period_key(ts(2024, 3, 15), "monthly"), "2024-03");
    }

    #[test]
    fn period_key_quarterly_q1_through_q4() {
        assert_eq!(period_key(ts(2024, 1, 15), "quarterly"), "2024-Q1");
        assert_eq!(period_key(ts(2024, 4, 1), "quarterly"), "2024-Q2");
        assert_eq!(period_key(ts(2024, 7, 1), "quarterly"), "2024-Q3");
        assert_eq!(period_key(ts(2024, 10, 1), "quarterly"), "2024-Q4");
    }

    #[test]
    fn period_key_yearly() {
        assert_eq!(period_key(ts(2024, 3, 15), "yearly"), "2024");
    }

    #[test]
    #[should_panic(expected = "period must be")]
    fn period_key_unknown_period_panics() {
        period_key(ts(2024, 1, 1), "decadely");
    }

    // --- periodic_returns() ---

    fn three_month_equity() -> (Vec<f64>, Vec<i64>) {
        let eq = vec![10_000.0, 10_200.0, 10_100.0, 10_500.0, 10_300.0, 10_800.0];
        let t = vec![
            ts(2024, 1, 15),
            ts(2024, 1, 25),
            ts(2024, 2, 10),
            ts(2024, 2, 25),
            ts(2024, 3, 10),
            ts(2024, 3, 25),
        ];
        (eq, t)
    }

    #[test]
    fn monthly_three_entries_with_labels() {
        let (eq, t) = three_month_equity();
        let result = periodic_returns(&eq, &t, "monthly");
        let labels: Vec<&str> = result.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, vec!["2024-01", "2024-02", "2024-03"]);
    }

    #[test]
    fn monthly_jan_return_correct() {
        let (eq, t) = three_month_equity();
        let result = periodic_returns(&eq, &t, "monthly");
        assert!((result[0].1 - 0.02).abs() < 1e-9);
    }

    #[test]
    fn monthly_feb_return_correct() {
        let (eq, t) = three_month_equity();
        let result = periodic_returns(&eq, &t, "monthly");
        assert!((result[1].1 - (10_500.0 / 10_200.0 - 1.0)).abs() < 1e-9);
    }

    #[test]
    fn periodic_returns_empty() {
        assert_eq!(periodic_returns(&[], &[], "monthly"), vec![]);
    }

    #[test]
    #[should_panic(expected = "equal length")]
    fn periodic_returns_length_mismatch_panics() {
        periodic_returns(&[100.0, 110.0], &[ts(2024, 1, 1)], "monthly");
    }

    // --- monthly_return_matrix() ---

    #[test]
    fn matrix_years_and_months_present() {
        let (eq, t) = three_month_equity();
        let m = monthly_return_matrix(&eq, &t);
        assert_eq!(m.years, vec![2024]);
        let months_2024 = &m.matrix[&2024];
        assert!(months_2024.contains_key(&1));
        assert!(months_2024.contains_key(&2));
        assert!(months_2024.contains_key(&3));
        assert!(m.annual[&2024] > 0.0);
    }

    #[test]
    fn matrix_multi_year() {
        let eq = vec![10_000.0, 10_500.0, 9_800.0, 10_200.0];
        let t = vec![ts(2023, 12, 15), ts(2023, 12, 29), ts(2024, 1, 10), ts(2024, 1, 25)];
        let m = monthly_return_matrix(&eq, &t);
        assert_eq!(m.years, vec![2023, 2024]);
    }

    // --- drawdown_table() ---

    fn dd_curve() -> (Vec<f64>, Vec<i64>) {
        let eq = vec![100.0, 110.0, 120.0, 100.0, 90.0, 95.0];
        let t: Vec<i64> = (1..=6).map(|d| ts(2024, 1, d)).collect();
        (eq, t)
    }

    #[test]
    fn dd_table_one_drawdown_found_with_depth_and_positions() {
        let (eq, t) = dd_curve();
        let table = drawdown_table(&eq, &t, 5);
        assert_eq!(table.len(), 1);
        assert!((table[0].depth - 0.25).abs() < 1e-9); // (120-90)/120
        assert_eq!(table[0].peak_ts, t[2]);
        assert_eq!(table[0].trough_ts, t[4]);
        assert_eq!(table[0].recovery_ts, None);
        assert_eq!(table[0].recovery, None);
        assert_eq!(table[0].length, 2); // trough idx 4 - peak idx 2
    }

    #[test]
    fn dd_table_with_recovery() {
        let eq = vec![100.0, 120.0, 90.0, 130.0];
        let t: Vec<i64> = (1..=4).map(|d| ts(2024, 1, d)).collect();
        let table = drawdown_table(&eq, &t, 5);
        assert_eq!(table.len(), 1);
        assert_eq!(table[0].recovery_ts, Some(t[3]));
        assert_eq!(table[0].recovery, Some(1));
    }

    #[test]
    fn dd_table_monotone_up_is_empty() {
        let eq = vec![100.0, 110.0, 120.0, 130.0];
        let t: Vec<i64> = (1..=4).map(|d| ts(2024, 1, d)).collect();
        assert_eq!(drawdown_table(&eq, &t, 5), vec![]);
    }

    #[test]
    fn dd_table_empty_curve() {
        assert_eq!(drawdown_table(&[], &[], 5), vec![]);
    }

    #[test]
    fn dd_table_top_n_limits_and_sorts_descending() {
        let eq = vec![
            100.0, 90.0, 100.0, // 10% dd, recovers
            110.0, 88.0, 110.0, // 20% dd, recovers
            120.0, 84.0, 120.0, // 30% dd, recovers
        ];
        let t: Vec<i64> = (1..=9).map(|d| ts(2024, 1, d)).collect();
        let table = drawdown_table(&eq, &t, 2);
        assert_eq!(table.len(), 2);
        assert!(table[0].depth >= table[1].depth);
    }

    #[test]
    #[should_panic(expected = "equal length")]
    fn dd_table_length_mismatch_panics() {
        drawdown_table(&[100.0, 90.0], &[ts(2024, 1, 1)], 5);
    }
}
