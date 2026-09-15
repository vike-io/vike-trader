//! steal/core-live-scheduler: the shared wall-clock `TimeRule` in the BACKTEST `Schedule`, plus the
//! cross-path proof that the SAME rule fires at byte-identical instants whether driven per-bar
//! (backtest) or per-poll (live `LiveSchedule`). Also pins the existing `EveryNBars` bar rule.

use vike_backtest::{DateRule, Schedule};
use vike_core::{LiveSchedule, TimeRule};
use vike_model::MS_PER_DAY;

/// A UTC midnight epoch (day 20_650 ≈ mid-2026).
const D: i64 = 20_650 * MS_PER_DAY;

/// `EveryNBars(3)` fires on bar indices 0, 3, 6, … — the bar-cadenced rule, unchanged by this work.
#[test]
fn every_n_bars_fires_every_n_bars() {
    let mut s = Schedule::new();
    s.on(DateRule::every_n_bars(3), "rebal");
    let mut fired: Vec<usize> = Vec::new();
    for i in 0usize..7 {
        if !s.check_due(i as i64 * 60_000, i).is_empty() {
            fired.push(i);
        }
    }
    assert_eq!(fired, vec![0, 3, 6], "bar_index % 3 == 0");
}

/// `DateRule::Time(daily 00:00 UTC)` fires on the first bar AT or AFTER each midnight — establishing
/// on the first bar (no spurious fire), then once per crossed day.
#[test]
fn date_rule_time_fires_at_each_daily_instant() {
    let mut s = Schedule::new();
    s.on(DateRule::time(TimeRule::daily_at_utc(0, 0)), "midnight");
    // (ts, bar_index): pre-midnight establish, then two midnight crossings with intraday bars.
    let bars: [(i64, usize); 5] = [
        (D - 6 * 3_600_000, 0),              // establish (no fire)
        (D, 1),                              // fires (midnight D)
        (D + 6 * 3_600_000, 2),              // same day, no fire
        (D + MS_PER_DAY, 3),                 // fires (next midnight)
        (D + MS_PER_DAY + 6 * 3_600_000, 4), // same day, no fire
    ];
    let mut fired: Vec<i64> = Vec::new();
    for (ts, i) in bars {
        if !s.check_due(ts, i).is_empty() {
            fired.push(ts);
        }
    }
    assert_eq!(fired, vec![D, D + MS_PER_DAY], "one fire per crossed midnight");
}

/// THE CROSS-PATH GATE: the SAME `TimeRule`, run through the backtest `Schedule` (checked per bar)
/// and the live `LiveSchedule` (checked per poll) over one shared timestamp stream, produces
/// byte-identical fire points — they share `TimeRule::crossed`, so a rule computed live equals the
/// same rule computed in a backtest.
#[test]
fn identical_fire_points_backtest_vs_live() {
    let rule = TimeRule::daily_at_utc(0, 0);
    // hourly stamps over three days, starting mid-day BEFORE D (so the first stamp establishes).
    let stamps: Vec<i64> = (0..(3 * 24)).map(|k| D - 12 * 3_600_000 + k * 3_600_000).collect();

    // backtest: fire iff `Schedule::check_due(ts, bar_index)` is non-empty at that bar
    let mut bt = Schedule::new();
    bt.on(DateRule::time(rule), "x");
    let mut bt_fires: Vec<i64> = Vec::new();
    for (i, ts) in stamps.iter().enumerate() {
        if !bt.check_due(*ts, i).is_empty() {
            bt_fires.push(*ts);
        }
    }

    // live: fire iff `LiveSchedule::check_due(now)` is non-empty at that poll
    let mut live = LiveSchedule::new();
    live.on(rule, "x");
    let mut live_fires: Vec<i64> = Vec::new();
    for ts in &stamps {
        if !live.check_due(*ts).is_empty() {
            live_fires.push(*ts);
        }
    }

    assert_eq!(bt_fires, live_fires, "backtest and live fire at identical instants");
    // and those instants ARE the midnights crossed by the stream (D, D+1d, D+2d)
    assert_eq!(bt_fires, vec![D, D + MS_PER_DAY, D + 2 * MS_PER_DAY]);
}
