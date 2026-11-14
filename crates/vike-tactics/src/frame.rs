//! [`HigherTimeframe`], a higher timeframe built as a series' bars come, and [`HigherTimeframePeriod`], its period (moved from
//! `vike-simulator`, decision 0271's strategy side): what a rule builds a week or a month from its daily input with
//! (`crate::Tactics::input`), or an hour from its own 30-minute bars.
//!
//! A frame of a [`HigherTimeframePeriod`] period (whole minutes on the epoch, or a UTC day, week or month), fed one bar at a time.
//! Every option is off by default, and then a higher bar is handed out on the bar whose close ends its period, never
//! while it is in progress, and the first one starts at the first bar fed. The options, each a reading a script platform
//! gives its higher-timeframe call, which the strategy lab measures:
//!
//! - **lookahead** ([`HigherTimeframe::looking_ahead`]): each higher bar is handed out on the first bar of its period, at
//!   its final values, so the base series handed in must hold the bars still to come. No read of the order language
//!   gives one (an input is read as of its last closed bar): only a caller holding the whole series can;
//! - **the history before the window, per frame kind** ([`HigherTimeframeHistoryBefore`]): a frame that reaches back is built from the base
//!   series' bars before the first bar fed as well, a frame that does not starts at that bar;
//! - **frames from a base series**: the history and the lookahead read come from a series handed in on the first bar fed,
//!   whose bars tile the frame (30-minute bars for a 2- or 3-hour frame, daily bars for a week or a month), not from the
//!   bars fed.
//!
//! A higher bar keeps its period's first bar's `funding`, `bid`, `ask` and `symbol`.

use vike_marketdata::Bar;
use vike_model::time::{MS_PER_DAY, floor_ms_to_utc_day, utc_day, utc_weekday};
use vike_utility::calendar::{civil_from_days, days_from_civil};

/// A higher timeframe's period, aligned on UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HigherTimeframePeriod {
    /// Whole minutes, aligned on the epoch (240: 00:00, 04:00, ... UTC).
    Minutes(i64),
    /// From 00:00 UTC.
    Day,
    /// From Monday 00:00 UTC.
    Week,
    /// From the first, 00:00 UTC.
    Month,
}

impl HigherTimeframePeriod {
    /// The start of the period `ts` lies in.
    pub fn start(self, ts: i64) -> i64 {
        match self {
            HigherTimeframePeriod::Minutes(m) => ts - ts.rem_euclid(m * 60_000),
            HigherTimeframePeriod::Day => floor_ms_to_utc_day(ts),
            HigherTimeframePeriod::Week => (utc_day(ts) - i64::from(utc_weekday(ts))) * MS_PER_DAY,
            HigherTimeframePeriod::Month => {
                let (y, m, _) = civil_from_days(utc_day(ts));
                days_from_civil(y, m, 1) * MS_PER_DAY
            }
        }
    }

    /// The end of the period that starts at `start`.
    pub fn end(self, start: i64) -> i64 {
        match self {
            HigherTimeframePeriod::Minutes(m) => start + m * 60_000,
            HigherTimeframePeriod::Day => start + MS_PER_DAY,
            HigherTimeframePeriod::Week => start + 7 * MS_PER_DAY,
            HigherTimeframePeriod::Month => {
                let (y, m, _) = civil_from_days(utc_day(start));
                let (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                days_from_civil(y, m, 1) * MS_PER_DAY
            }
        }
    }
}

/// Which frames reach back before the first chart bar, by kind: a frame that does is built from every bar of its base
/// series that closed before that bar as well. The default reaches back with none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HigherTimeframeHistoryBefore {
    /// A day, a week and a month.
    pub calendar: bool,
    /// A frame of minutes.
    pub intraday: bool,
}

impl HigherTimeframeHistoryBefore {
    /// Whether a frame of `every` reaches back.
    pub fn reaches_back(self, every: HigherTimeframePeriod) -> bool {
        match every {
            HigherTimeframePeriod::Minutes(_) => self.intraday,
            HigherTimeframePeriod::Day
            | HigherTimeframePeriod::Week
            | HigherTimeframePeriod::Month => self.calendar,
        }
    }
}

/// The higher bars of one timeframe, built as the chart's bars come (the module doc has the options).
#[derive(Clone, Debug)]
pub struct HigherTimeframe {
    every: HigherTimeframePeriod,
    /// The length of a chart bar.
    chart_ms: i64,
    /// Each higher bar on the first chart bar of its period, at its final values.
    ahead: bool,
    history: HigherTimeframeHistoryBefore,
    /// Whether the first chart bar has come.
    started: bool,
    /// The higher bar in progress.
    building: Option<Bar>,
    /// When `ahead`: every higher bar the base series makes from where the frame starts, and how many are handed out.
    whole: Option<(Vec<Bar>, usize)>,
}

impl HigherTimeframe {
    /// A frame of `every` over a chart of `chart_ms` bars, every option off.
    pub fn new(every: HigherTimeframePeriod, chart_ms: i64) -> Self {
        assert!(chart_ms > 0, "a chart bar has a length");
        HigherTimeframe {
            every,
            chart_ms,
            ahead: false,
            history: HigherTimeframeHistoryBefore::default(),
            started: false,
            building: None,
            whole: None,
        }
    }

    /// The lookahead read when `on` (off by default): each higher bar handed out on the first chart bar of its period, at
    /// its final values.
    pub fn looking_ahead(self, on: bool) -> Self {
        HigherTimeframe { ahead: on, ..self }
    }

    /// The frame kinds that reach back before the first chart bar (none by default).
    pub fn with_history(self, history: HigherTimeframeHistoryBefore) -> Self {
        HigherTimeframe { history, ..self }
    }

    /// Whether this frame reaches back before the first chart bar.
    pub fn reaches_back(&self) -> bool {
        self.history.reaches_back(self.every)
    }

    /// Feeds one chart bar and returns the higher bars it may see, oldest first: without lookahead the ones closed by its
    /// close (on the first bar of a frame that reaches back, every one of the base series' history before it as well; the
    /// bar in progress then holds what of its period came before that bar), with lookahead each one whose period starts by
    /// this bar, at its final values.
    ///
    /// `base` is the series the frame is built from and its bar length, read on the first chart bar only (`None` there is
    /// an empty one): its bars before that bar when the frame reaches back, and with lookahead its whole frame, from its
    /// first bar when the frame reaches back and from the first chart bar when it does not. A frame without either reads
    /// none of it.
    pub fn next(&mut self, bar: &Bar, base: Option<(&[Bar], i64)>) -> Vec<Bar> {
        let mut closed = Vec::new();
        if !self.started {
            self.started = true;
            let (bars, span) = base.unwrap_or((&[], self.chart_ms));
            let back = self.reaches_back();
            if self.ahead {
                let from = if back { i64::MIN } else { bar.timestamp };
                let all = whole(self.every, &bars[bars.partition_point(|b| b.timestamp < from)..]);
                self.whole = Some((all, 0));
            } else if back {
                assert_eq!(
                    bar.timestamp.rem_euclid(span),
                    0,
                    "the first chart bar starts a base bar"
                );
                for b in bars.iter().take_while(|b| b.timestamp + span <= bar.timestamp) {
                    add(self.every, &mut self.building, b, &mut closed);
                }
            }
        }
        if let Some((all, given)) = &mut self.whole {
            let start = self.every.start(bar.timestamp);
            let from = *given;
            *given += all[from..].iter().take_while(|b| b.timestamp <= start).count();
            return all[from..*given].to_vec();
        }
        add(self.every, &mut self.building, bar, &mut closed);
        if bar.timestamp + self.chart_ms >= self.every.end(self.every.start(bar.timestamp)) {
            closed.extend(self.building.take());
        }
        closed
    }
}

/// Adds a bar to `building`, the higher bar of its period, closing the one before into `closed` when it starts a new period.
fn add(every: HigherTimeframePeriod, building: &mut Option<Bar>, bar: &Bar, closed: &mut Vec<Bar>) {
    let start = every.start(bar.timestamp);
    if let Some(b) = building.as_mut().filter(|b| b.timestamp == start) {
        b.high = b.high.max(bar.high);
        b.low = b.low.min(bar.low);
        b.close = bar.close;
        b.volume += bar.volume;
    } else {
        closed.extend(building.replace(Bar { timestamp: start, ..bar.clone() }));
    }
}

/// Every higher bar `bars` make, the last one as far as they go.
fn whole(every: HigherTimeframePeriod, bars: &[Bar]) -> Vec<Bar> {
    let (mut building, mut all) = (None, Vec::new());
    for b in bars {
        add(every, &mut building, b, &mut all);
    }
    all.extend(building);
    all
}

#[cfg(test)]
mod tests {
    use super::{HigherTimeframe, HigherTimeframeHistoryBefore, HigherTimeframePeriod};
    use vike_marketdata::Bar;

    const DAY_MS: i64 = 86_400_000;
    const HALF_HOUR_MS: i64 = 1_800_000;
    const JAN_2025: i64 = 1_735_689_600_000;

    #[test]
    fn periods_align_on_utc() {
        assert_eq!(HigherTimeframePeriod::Month.start(JAN_2025 + 5 * DAY_MS), JAN_2025);
        assert_eq!(
            HigherTimeframePeriod::Week.start(JAN_2025),
            JAN_2025 - 2 * DAY_MS,
            "2025-01-01 is a Wednesday"
        );
        let feb_2024 = 1_706_745_600_000;
        assert_eq!(
            HigherTimeframePeriod::Month
                .end(HigherTimeframePeriod::Month.start(feb_2024 + 28 * DAY_MS)),
            feb_2024 + 29 * DAY_MS,
            "a leap February"
        );
        assert_eq!(
            HigherTimeframePeriod::Month.end(JAN_2025 - DAY_MS),
            JAN_2025,
            "December runs into January"
        );
        assert_eq!(HigherTimeframePeriod::Minutes(240).start(JAN_2025 + 3 * 3_600_000), JAN_2025);
        assert_eq!(
            HigherTimeframePeriod::Minutes(180).start(JAN_2025 + 5 * 3_600_000),
            JAN_2025 + 3 * 3_600_000
        );
        assert_eq!(HigherTimeframePeriod::Day.start(JAN_2025 + DAY_MS - 1), JAN_2025);
        assert_eq!(HigherTimeframePeriod::Day.start(-1), -DAY_MS, "before 1970 floors down");
    }

    fn bar(ts: i64, i: f64) -> Bar {
        Bar::ohlcv(ts, i, i + 1.0, i - 1.0, i, 1.0)
    }

    fn ohlcv(b: &Bar) -> (i64, f64, f64, f64, f64, f64) {
        (b.timestamp, b.open, b.high, b.low, b.close, b.volume)
    }

    /// A day and one bar of 30-minute bars from 2025-01-01.
    fn half_hours() -> Vec<Bar> {
        (0..49).map(|i: i32| bar(JAN_2025 + i64::from(i) * HALF_HOUR_MS, i.into())).collect()
    }

    /// Feeds `bars` to `frame`, `base` on the first bar.
    fn feed(frame: &mut HigherTimeframe, bars: &[Bar], base: (&[Bar], i64)) -> Vec<Vec<Bar>> {
        bars.iter().enumerate().map(|(i, b)| frame.next(b, (i == 0).then_some(base))).collect()
    }

    #[test]
    fn a_higher_bar_is_seen_once_closed() {
        let bars = half_hours();
        let fed = feed(
            &mut HigherTimeframe::new(HigherTimeframePeriod::Day, HALF_HOUR_MS),
            &bars,
            (&[], DAY_MS),
        );
        assert!(fed[..47].iter().all(Vec::is_empty), "nothing closes before the day's last bar");
        assert_eq!(
            fed[47].iter().map(ohlcv).collect::<Vec<_>>(),
            [(JAN_2025, 0.0, 48.0, -1.0, 47.0, 48.0)],
            "the day closes with its last 30-minute bar, which it holds"
        );
        assert!(fed[48].is_empty(), "the next day is in progress");
    }

    #[test]
    fn lookahead_hands_out_each_bar_on_its_period_s_first_chart_bar() {
        let bars = half_hours();
        let mut ahead =
            HigherTimeframe::new(HigherTimeframePeriod::Day, HALF_HOUR_MS).looking_ahead(true);
        let fed = feed(&mut ahead, &bars, (&bars, HALF_HOUR_MS));
        assert!(
            fed[0].len() == 1 && fed[0][0].close == 47.0 && fed[1..48].iter().all(Vec::is_empty),
            "lookahead on: the whole day on its first bar"
        );
        assert_eq!(fed[48].iter().map(ohlcv).collect::<Vec<_>>(), [ohlcv(&bars[48])]);
    }

    #[test]
    fn history_reaches_back_by_frame_kind() {
        let bars = half_hours();
        let calendar = HigherTimeframeHistoryBefore { calendar: true, intraday: false };
        // The day before reaches a daily frame that reaches back; none reaches one that does not.
        let before = [bar(JAN_2025 - DAY_MS, 7.0)];
        let mut day =
            HigherTimeframe::new(HigherTimeframePeriod::Day, HALF_HOUR_MS).with_history(calendar);
        assert_eq!(
            day.next(&bars[0], Some((&before, DAY_MS))).iter().map(ohlcv).collect::<Vec<_>>(),
            [ohlcv(&before[0])]
        );
        let mut day = HigherTimeframe::new(HigherTimeframePeriod::Day, HALF_HOUR_MS);
        assert!(day.next(&bars[0], Some((&before, DAY_MS))).is_empty());
        // A 2-hour frame built from 30-minute bars starts at the first chart bar unless intraday frames reach back.
        let earlier: Vec<Bar> =
            (1..=4).map(|i| bar(JAN_2025 - i * HALF_HOUR_MS, 9.0)).rev().collect();
        let mut hours = HigherTimeframe::new(HigherTimeframePeriod::Minutes(120), HALF_HOUR_MS)
            .with_history(calendar);
        let fed = feed(&mut hours, &bars[..4], (&earlier, HALF_HOUR_MS));
        assert!(fed[..3].iter().all(Vec::is_empty), "no 2-hour bar from before the window");
        assert_eq!(
            fed[3].iter().map(ohlcv).collect::<Vec<_>>(),
            [(JAN_2025, 0.0, 4.0, -1.0, 3.0, 4.0)]
        );
        let intraday = HigherTimeframeHistoryBefore { calendar: false, intraday: true };
        let mut hours = HigherTimeframe::new(HigherTimeframePeriod::Minutes(120), HALF_HOUR_MS)
            .with_history(intraday);
        let fed = feed(&mut hours, &bars[..4], (&earlier, HALF_HOUR_MS));
        assert_eq!(
            fed[0].iter().map(ohlcv).collect::<Vec<_>>(),
            [(JAN_2025 - 7_200_000, 9.0, 10.0, 8.0, 9.0, 4.0)],
            "the 2 hours before the window, closed on the first chart bar"
        );
    }
}
