//! The wall-clock firing rule (`TimeRule`) shared by the live runtime and the backtest engine.
//!
//! **Shared home.** `TimeRule` is a PURE, allocation-free, clock-free value type, so it lives in the
//! domain model where BOTH consumers reach it down-only: `vike-core`'s live runtime fires it off the
//! `DeadlineTimerWheel` at the drain-loop boundary (and keeps the `LiveSchedule` registry that owns
//! the mount's rules), and `vike-backtest` drops the SAME `TimeRule` into its `DateRule::Time`
//! variant, checked per bar. One rule, one law: [`TimeRule::last_instant_at_or_before`] is the single
//! crossing primitive both paths call, so a rule computes byte-identical fire INSTANTS whether it is
//! driven by bar timestamps (backtest) or by the injected boundary clock (live).
//!
//! MOVED here from `vike_core::schedule` (same file, verbatim) so `vike-backtest` no longer needs a
//! dependency on the live core for this one type — the `vike-model::sizing` hoist is the precedent.
//! `vike_core::schedule` re-exports it, so every existing `vike_core::TimeRule` call site is
//! unchanged. The live-only registry (`LiveSchedule`) stayed in vike-core: it is the runtime's mount
//! table, and the runtime is its only consumer.
//!
//! **Not the hot path.** The live registry is consulted only at the drain-loop boundary (never per
//! market message), so the `p99 < 10µs` fold is untouched — see
//! `vike_core::runtime::strategy_drive::drive_schedule`.

/// One millisecond count of a UTC day (86_400_000). Local alias so this module reads standalone; the
/// constant itself is defined once, in [`crate::order::MS_PER_DAY`].
const DAY_MS: i64 = crate::order::MS_PER_DAY;

/// A wall-clock firing rule: fire at `minute_of_day` (HH:MM) in a FIXED-offset timezone, on the
/// selected weekdays. Pure value type — no clock, no allocation, `Copy`.
///
/// **Timezone model.** `tz_offset_min` is the fixed offset of the reference timezone from UTC in
/// minutes (UTC → 0, UTC+2 → 120, UTC−5 → −300). This is deliberately DST-UNAWARE: it covers the
/// "00:00 UTC" / "08:00 UTC" (Deribit expiry) cases the task names, plus any fixed-offset market;
/// an IANA/DST-aware variant is a future extension (it would need a calendar dependency this pure
/// core deliberately avoids).
///
/// **Market open/close offsets** are expressed with [`TimeRule::session_offset`]: an anchor
/// minute-of-day (the session open or close) plus a signed minute offset (e.g. "5 minutes before
/// the 16:00 close" ⇒ anchor `960`, offset `-5`), restricted to trading weekdays via the mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRule {
    /// minute past local midnight the rule fires at (`0..=1439`).
    minute_of_day: u32,
    /// fixed offset of the reference timezone from UTC, in minutes.
    tz_offset_min: i32,
    /// weekday bitmask: bit `i` set ⇒ fires on weekday `i` (Mon=0 … Sun=6). `0` ⇒ never fires.
    weekdays: u8,
}

impl TimeRule {
    /// Every weekday, Monday through Sunday — the default for a plain daily rule.
    pub const ALL_DAYS: u8 = 0b0111_1111;
    /// Monday through Friday — the common trading-week mask for a market open/close offset.
    pub const WEEKDAYS: u8 = 0b0001_1111;

    /// Fire daily at `hour:minute` UTC (the "recalibrate at 00:00 UTC" / "Deribit 08:00" case).
    pub fn daily_at_utc(hour: u32, minute: u32) -> Self {
        Self::daily_at(hour, minute, 0)
    }

    /// Fire daily at `hour:minute` in a fixed-offset timezone (`tz_offset_min` minutes from UTC).
    pub fn daily_at(hour: u32, minute: u32, tz_offset_min: i32) -> Self {
        Self::on_days(hour, minute, tz_offset_min, Self::ALL_DAYS)
    }

    /// Fire at `hour:minute` (fixed-offset tz) only on the weekdays selected by `weekdays`
    /// (bit Mon=0 … Sun=6). Panics on an out-of-range `hour`/`minute` (a programmer error).
    pub fn on_days(hour: u32, minute: u32, tz_offset_min: i32, weekdays: u8) -> Self {
        assert!(hour < 24 && minute < 60, "HH:MM out of range: {hour}:{minute}");
        TimeRule { minute_of_day: hour * 60 + minute, tz_offset_min, weekdays }
    }

    /// A MARKET-OPEN/CLOSE OFFSET: fire `offset_min` minutes from `anchor_minute_of_day` (the
    /// session open or close, minutes past local midnight), in a fixed-offset tz, on `weekdays`.
    /// The anchor + offset wraps into the day (`rem_euclid` 1440), so "5 minutes before the 16:00
    /// close" is `session_offset(960, -5, tz, WEEKDAYS)` ⇒ 15:55.
    pub fn session_offset(
        anchor_minute_of_day: i32,
        offset_min: i32,
        tz_offset_min: i32,
        weekdays: u8,
    ) -> Self {
        let m = (anchor_minute_of_day + offset_min).rem_euclid(24 * 60) as u32;
        TimeRule { minute_of_day: m, tz_offset_min, weekdays }
    }

    /// The most recent scheduled fire instant (epoch ms, UTC) AT or BEFORE `now_ms`, or `None` when
    /// no scheduled instant precedes `now_ms` on an allowed weekday (e.g. an empty weekday mask, or
    /// `now_ms` before the rule's first allowed day). This is the SINGLE crossing primitive both the
    /// live registry and the backtest `DateRule::Time` call: fire iff this instant advanced past the
    /// last one fired.
    pub fn last_instant_at_or_before(&self, now_ms: i64) -> Option<i64> {
        if self.weekdays == 0 {
            return None;
        }
        let offset_ms = self.tz_offset_min as i64 * 60_000;
        let target_in_day = self.minute_of_day as i64 * 60_000;
        // shift to the rule's local wall clock, then split into (day index, ms into the day)
        let local = now_ms + offset_ms;
        let day = local.div_euclid(DAY_MS);
        let ms_in_day = local.rem_euclid(DAY_MS);
        // today's instant counts only once the local clock has reached HH:MM; else the most recent
        // candidate is yesterday's.
        let mut cand_day = if ms_in_day >= target_in_day { day } else { day - 1 };
        // walk back to the nearest allowed weekday — at most 7 steps cover the whole week.
        for _ in 0..7 {
            // Mon=0 … Sun=6; 1970-01-01 (day 0) was a Thursday (index 3). Same epoch convention as
            // [`crate::time::utc_weekday`], which applies it to an epoch-MS value rather than the
            // day index already in hand here.
            let dow = (cand_day + 3).rem_euclid(7) as u32;
            if self.weekdays & (1u8 << dow) != 0 {
                let fire_local = cand_day * DAY_MS + target_in_day;
                return Some(fire_local - offset_ms);
            }
            cand_day -= 1;
        }
        None
    }

    /// Advance the per-rule fire latch and report whether a NEW instant was crossed. This is the ONE
    /// shared law both the live registry (`vike_core::LiveSchedule::check_due`) and the backtest
    /// `DateRule::Time` call, so a rule fires at byte-identical points on both paths.
    ///
    /// The FIRST call (`*last == None`) ESTABLISHES the latch at the most-recent-past instant WITHOUT
    /// firing — so a schedule mounted (or restored after a restart) mid-period fires at the NEXT
    /// instant, never a spurious catch-up of the one that already passed before it started watching.
    /// Thereafter it fires iff the most-recent instant has advanced, coalescing any instants skipped
    /// across an idle gap to a single fire of the latest.
    pub fn crossed(&self, now_ms: i64, last: &mut Option<i64>) -> bool {
        match self.last_instant_at_or_before(now_ms) {
            Some(inst) => match *last {
                None => {
                    *last = Some(inst);
                    false
                }
                Some(prev) if inst > prev => {
                    *last = Some(inst);
                    true
                }
                _ => false,
            },
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixed modern day index so the arithmetic runs against realistic epochs (2026-ish).
    const DAY0: i64 = 20_650 * DAY_MS; // 2026-07-xx 00:00:00 UTC

    #[test]
    fn daily_utc_instant_is_midnight() {
        let r = TimeRule::daily_at_utc(0, 0);
        // exactly at midnight ⇒ today's instant
        assert_eq!(r.last_instant_at_or_before(DAY0), Some(DAY0));
        // one ms before midnight ⇒ yesterday's instant
        assert_eq!(r.last_instant_at_or_before(DAY0 - 1), Some(DAY0 - DAY_MS));
        // mid-day ⇒ still today's midnight
        assert_eq!(r.last_instant_at_or_before(DAY0 + 12 * 3_600_000), Some(DAY0));
    }

    #[test]
    fn daily_at_hhmm_utc() {
        let r = TimeRule::daily_at_utc(8, 0); // Deribit 08:00 expiry
        let eight = DAY0 + 8 * 3_600_000;
        assert_eq!(r.last_instant_at_or_before(eight), Some(eight));
        // just before 08:00 ⇒ yesterday's 08:00
        assert_eq!(r.last_instant_at_or_before(eight - 1), Some(eight - DAY_MS));
    }

    #[test]
    fn tz_offset_shifts_the_utc_instant() {
        // 00:00 in UTC+2 happens at 22:00 the PREVIOUS UTC day.
        let r = TimeRule::daily_at(0, 0, 120);
        let expected = DAY0 - 2 * 3_600_000; // 22:00 UTC of the prior day
        assert_eq!(r.last_instant_at_or_before(DAY0), Some(expected));
    }

    #[test]
    fn session_offset_is_before_close() {
        // 5 minutes before the 16:00 close, UTC, weekdays only.
        let r = TimeRule::session_offset(16 * 60, -5, 0, TimeRule::WEEKDAYS);
        let fifteen_fiftyfive = DAY0 + (15 * 60 + 55) * 60_000;
        // DAY0 is a UTC midnight; find whichever recent weekday instant it lands on — assert the
        // computed instant's minute-of-day is 15:55 regardless of which day.
        let got = r.last_instant_at_or_before(fifteen_fiftyfive).unwrap();
        assert_eq!(got.rem_euclid(DAY_MS), (15 * 60 + 55) * 60_000);
    }

    #[test]
    fn weekday_mask_skips_excluded_days() {
        // A Sunday-only rule (bit 6) never returns a non-Sunday instant.
        let sunday_only = 1u8 << 6;
        let r = TimeRule::on_days(0, 0, 0, sunday_only);
        // sample a full week of midnights; every returned instant must be a Sunday midnight.
        for k in 0..7 {
            let day = DAY0 + k * DAY_MS;
            let inst = r.last_instant_at_or_before(day).expect("a Sunday exists before any day");
            let day_idx = inst.div_euclid(DAY_MS);
            let dow = (day_idx + 3).rem_euclid(7);
            assert_eq!(dow, 6, "the fired instant must be a Sunday");
        }
    }

    #[test]
    fn empty_weekday_mask_never_fires() {
        let r = TimeRule::on_days(0, 0, 0, 0);
        assert_eq!(r.last_instant_at_or_before(DAY0), None);
    }
}
