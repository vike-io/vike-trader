//! A higher timeframe, as strategy files read one: a day of the run's own symbol is its daily INPUT
//! (`crate::Tactics::input("1d")`, decision 0271: the venue's own daily bars, its whole stored
//! history before the window included, each seen once it has closed); a week or a month is built from
//! those days, and an hour or four from the rule's own bars, by `crate::HigherTimeframe`. Copied from
//! the strategy gallery (plan 2b, C3), with [`HigherTimeframeBars`], the bucketed frame 18 files share; the
//! Heikin-Ashi ticker on the tick is `vike_indicators::strategy_kit::HeikinAshiCandles`, as this crate names no
//! indicator (decision 0269).
//!
//! The reference's call for a higher timeframe, without lookahead, sees the last higher bar closed by
//! the chart bar's close; a day, a week, a month and a quarter reach back into the venue's history before
//! the window, and a frame of minutes or hours starts at the window. Each reading is the strategy lab's,
//! measured against the reference rows (its `strategy_lab_b/timeframe.rs` has the numbers).
//!
//! **With lookahead on** ([`HigherTimeframeSourceAhead`]) the call sees the higher bar in progress at its
//! FINAL values, from the first chart bar of its period: the same frames, built from the input's whole
//! series (`crate::Tactics::input_whole`, decision 0271's amendment of 2026-11-14). It looks into the
//! future, so a backtest serves that series to a file marked `runs_on = "backtest"` alone, and paper and
//! live never mount one.
//!
//! **Another source for the frames** ([`HigherTimeframeSource`], [`with_higher_timeframe_source`]): a frame made while a source is
//! installed on the thread is built by it instead, for its whole life, from the rule's own bars. A
//! harness that measures a rule's reading against other readings (the strategy lab's controls: the bar
//! in progress, no history, the venue's intraday history) runs the rule on its own frames that way;
//! with no source installed nothing changes. The candles' source is
//! `vike_indicators::strategy_kit::with_heikin_ashi_candles_source`.

#![expect(
    clippy::type_complexity,
    reason = "the frame and candle builders' types are written out: their aliases added no name (plan 2b section 8)"
)]

use std::cell::RefCell;
use std::rc::Rc;

use vike_marketdata::Bar;
use vike_model::{MS_PER_DAY, MS_PER_MINUTE, MS_PER_WEEK};

use crate::{
    HigherTimeframe, HigherTimeframeHistoryBefore, HigherTimeframePeriod, Tactics, TacticsInput,
};

/// Another builder of this module's frames (the module doc): each method makes one, as a frame of that
/// kind is made.
pub trait HigherTimeframeSource {
    /// A day, a week, a month or a quarter ([`HigherTimeframeSourceCalendar`]).
    fn calendar(&self, every: HigherTimeframePeriod) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send>;
    /// A frame of `minutes` ([`HigherTimeframeSourceIntraday`]).
    fn intraday(&self, minutes: i64) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send>;
    /// A frame of `every` read with lookahead on ([`HigherTimeframeSourceAhead`]).
    fn ahead(&self, every: HigherTimeframePeriod) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send>;
}

thread_local! {
    /// The source [`with_higher_timeframe_source`] installed on this thread, if any.
    static SOURCE: RefCell<Option<Rc<dyn HigherTimeframeSource>>> = const { RefCell::new(None) };
}

/// Runs `f` with `source` building every frame made on this thread meanwhile, and the source before it
/// back afterwards, a panic included.
pub fn with_higher_timeframe_source<T>(
    source: Rc<dyn HigherTimeframeSource>,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Rc<dyn HigherTimeframeSource>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let before = self.0.take();
            SOURCE.with(|s| *s.borrow_mut() = before);
        }
    }
    let _restore = Restore(SOURCE.with(|s| s.borrow_mut().replace(source)));
    f()
}

/// The installed source, if any.
fn source() -> Option<Rc<dyn HigherTimeframeSource>> {
    SOURCE.with(|s| s.borrow().clone())
}

/// The input a day is read from: the run's own symbol's daily bars.
pub const DAY: &str = "1d";

/// The bars of one input a rule has not taken yet, oldest first: on the first call every one closed
/// so far (its history before the window, then the ones closed by this bar's close), then each new
/// one once, on the bar whose close it closed by.
#[derive(Debug, Default, Clone)]
pub struct HigherTimeframeSourceUnseen {
    last: Option<i64>,
}

impl HigherTimeframeSourceUnseen {
    pub fn new() -> Self {
        HigherTimeframeSourceUnseen::default()
    }

    /// The closed bars of `input` not taken before, oldest first.
    pub fn next<'a>(&mut self, input: TacticsInput<'a>) -> &'a [Bar] {
        let new = match self.last {
            None => input.bars(usize::MAX),
            // Nothing closed since: the empty tail `since` would find, without its search over the
            // whole input (twice a bar in a rule reading days and weeks).
            Some(ts) if input.time().is_none_or(|last| last <= ts) => input.bars(0),
            Some(ts) => input.since(ts + 1),
        };
        if let Some(b) = new.last() {
            self.last = Some(b.timestamp);
        }
        new
    }
}

/// A day, a week or a month of the run's own symbol, closed bars only, each handed out once: the days
/// are its daily input ([`DAY`]), a week or a month is built from them (`crate::HigherTimeframe`
/// over days). Every closed one is handed out on the rule's first bar, its history included.
pub struct HigherTimeframeSourceCalendar {
    unseen: HigherTimeframeSourceUnseen,
    /// `None` for a day; a week's or a month's frame, fed each day.
    frame: Option<HigherTimeframe>,
    /// An installed [`HigherTimeframeSource`]'s frame, which reads the rule's bars instead.
    other: Option<Box<dyn FnMut(&Bar) -> Vec<Bar> + Send>>,
}

impl HigherTimeframeSourceCalendar {
    /// The frame of `every`, a day, a week or a month.
    ///
    /// # Panics
    /// On a frame of minutes: an intraday frame starts at the window and is built from the rule's own
    /// bars ([`HigherTimeframeSourceIntraday`]).
    pub fn new(every: HigherTimeframePeriod) -> Self {
        let frame = match every {
            HigherTimeframePeriod::Day => None,
            HigherTimeframePeriod::Week
            | HigherTimeframePeriod::Month
            | HigherTimeframePeriod::Quarter => Some(HigherTimeframe::new(every, MS_PER_DAY)),
            HigherTimeframePeriod::Minutes(m) => panic!(
                "{m} minutes is an intraday frame: `HigherTimeframeSourceIntraday` builds it"
            ),
        };
        let other = source().map(|s| s.calendar(every));
        HigherTimeframeSourceCalendar { unseen: HigherTimeframeSourceUnseen::new(), frame, other }
    }

    /// The higher bars closed since the last call, oldest first, read from `day` (the rule's
    /// `strategy.input(DAY)`) on the rule's `bar`.
    pub fn next(&mut self, bar: &Bar, day: TacticsInput<'_>) -> Vec<Bar> {
        if let Some(other) = &mut self.other {
            return other(bar);
        }
        let days = self.unseen.next(day);
        match &mut self.frame {
            None => days.to_vec(),
            Some(frame) => days.iter().flat_map(|d| frame.next(d, None)).collect(),
        }
    }
}

/// A frame of whole minutes that are a multiple of 30 (`HigherTimeframePeriod::Minutes`), built from the run's own
/// symbol's 30-minute bars ([`HALF_HOUR`], an input) from the rule's first bar on, so it starts at the
/// window; a bar of it is handed out on the bar whose close ends its period. On a 30-minute chart those
/// are the chart's own bars; on any other the frame is the same, as it never needs the chart's length.
pub struct HigherTimeframeSourceIntraday {
    unseen: HigherTimeframeSourceUnseen,
    /// The rule's first bar's open: no bar before it is fed.
    from: Option<i64>,
    frame: HigherTimeframe,
    /// An installed [`HigherTimeframeSource`]'s frame.
    other: Option<Box<dyn FnMut(&Bar) -> Vec<Bar> + Send>>,
}

/// The input an intraday frame is built from: the run's own symbol's 30-minute bars.
pub const HALF_HOUR: &str = "30m";

const HALF_HOUR_MS: i64 = 1_800_000;

impl HigherTimeframeSourceIntraday {
    /// A frame of `minutes`.
    ///
    /// # Panics
    /// When `minutes` is not a positive multiple of 30.
    pub fn new(minutes: i64) -> Self {
        assert!(minutes > 0 && minutes % 30 == 0, "{minutes} minutes is no multiple of 30");
        let frame = HigherTimeframe::new(HigherTimeframePeriod::Minutes(minutes), HALF_HOUR_MS);
        let other = source().map(|s| s.intraday(minutes));
        HigherTimeframeSourceIntraday {
            unseen: HigherTimeframeSourceUnseen::new(),
            from: None,
            frame,
            other,
        }
    }

    /// The higher bars closed by `bar`'s close, oldest first, read from `half_hour` (the rule's
    /// `strategy.input(HALF_HOUR)`).
    pub fn next(&mut self, bar: &Bar, half_hour: TacticsInput<'_>) -> Vec<Bar> {
        if let Some(other) = &mut self.other {
            return other(bar);
        }
        let from = *self.from.get_or_insert(bar.timestamp);
        let bars = self.unseen.next(half_hour);
        let fed = &bars[bars.partition_point(|b| b.timestamp < from)..];
        fed.iter().flat_map(|b| self.frame.next(b, None)).collect()
    }
}

/// A frame read with LOOKAHEAD ON (`crate::HigherTimeframe::looking_ahead`): each higher bar handed out on
/// the first bar of its period at its FINAL values, built from an input's whole series
/// (`crate::Tactics::input_whole`): a day, a week, a month or a quarter from the run's own daily bars
/// ([`DAY`]), its history before the window included, as [`HigherTimeframeSourceCalendar`] reaches back; a
/// frame of minutes from the 30-minute bars ([`HALF_HOUR`]) from the rule's first bar on, as
/// [`HigherTimeframeSourceIntraday`] starts. The rule declares [`HigherTimeframeSourceAhead::input`].
/// The whole series ends at the run's last event, so the last period holds the bars up to there: a frame
/// of minutes ends with the run's data, and a day of the daily input still in progress then is never
/// handed out.
///
/// It looks into the future: a file that reads one says `runs_on = "backtest"`, its `why` starting "looks
/// into the future", and is served the series in a backtest alone.
pub struct HigherTimeframeSourceAhead {
    /// [`DAY`] or [`HALF_HOUR`].
    input: &'static str,
    /// Set once the whole series has been read, on the rule's first bar.
    started: bool,
    frame: HigherTimeframe,
    /// An installed [`HigherTimeframeSource`]'s frame.
    other: Option<Box<dyn FnMut(&Bar) -> Vec<Bar> + Send>>,
}

impl HigherTimeframeSourceAhead {
    /// A frame of `every`, read with lookahead on.
    ///
    /// # Panics
    /// On a frame of minutes that is not a positive multiple of 30.
    pub fn new(every: HigherTimeframePeriod) -> Self {
        let input = match every {
            HigherTimeframePeriod::Minutes(m) => {
                assert!(m > 0 && m % 30 == 0, "{m} minutes is no multiple of 30");
                HALF_HOUR
            }
            _ => DAY,
        };
        let history = HigherTimeframeHistoryBefore { calendar: true, intraday: false };
        // The chart bar's length is read by a frame without lookahead alone.
        let frame =
            HigherTimeframe::new(every, HALF_HOUR_MS).looking_ahead(true).with_history(history);
        let other = source().map(|s| s.ahead(every));
        HigherTimeframeSourceAhead { input, started: false, frame, other }
    }

    /// The input the frame is built from, [`DAY`] or [`HALF_HOUR`]: the rule declares it
    /// (`crate::Rule::inputs`).
    pub fn input(&self) -> &'static str {
        self.input
    }

    /// The higher bars whose period starts by `bar`, oldest first, each at its final values: on the first
    /// bar every one from the series' first (a calendar frame) or from that bar (minutes), its own
    /// period's included, then each on the first bar of its period. The whole series is read on the first
    /// bar alone.
    ///
    /// # Panics
    /// On the first bar, when the run serves no whole series: the backtest of a file whose header does not
    /// say `runs_on = "backtest"` (and paper or live, which never mount one).
    pub fn next(&mut self, bar: &Bar, strategy: &Tactics<'_>) -> Vec<Bar> {
        if let Some(other) = &mut self.other {
            return other(bar);
        }
        let base = (!std::mem::replace(&mut self.started, true)).then(|| {
            let whole = strategy.input_whole(self.input).unwrap_or_else(|| {
                panic!(
                    "a frame read with lookahead on needs the whole series of the input {:?}, which a \
                     backtest serves a file whose header says runs_on = \"backtest\" alone",
                    self.input
                )
            });
            let span = if self.input == DAY { MS_PER_DAY } else { HALF_HOUR_MS };
            (whole, span)
        });
        self.frame.next(bar, base)
    }
}

/// The length of the chart bar [`HigherTimeframeBars`] is fed: its files run on 15-minute bars.
const CHART_BAR_MS: i64 = 900_000;
/// 1970-01-05 00:00 UTC, a Monday.
const FIRST_MONDAY_MS: i64 = 4 * MS_PER_DAY;

/// Builds higher-timeframe bars from 15-minute chart bars, in UTC buckets (the bucketed frame of the 18 files written from the
/// validation corpus's multi-timeframe scripts; NOT `crate::HigherTimeframe`, which stamps a higher bar with its period's start,
/// may hand out two on one bar and knows any chart length). A bucket closes ON its last chart bar, so that bar already sees the
/// closed value; between closes a rule keeps the last closed value, even for an expression over the higher series. A higher bar
/// keeps its first chart bar's `ts`.
pub struct HigherTimeframeBars {
    length_ms: i64,
    anchor_ms: i64,
    bucket: i64,
    forming: Option<Bar>,
}

impl HigherTimeframeBars {
    /// Buckets of whole minutes counted from the epoch, so 60, 240 and 1440 line up with midnight UTC.
    pub fn minutes(minutes: i64) -> Self {
        HigherTimeframeBars {
            length_ms: minutes * MS_PER_MINUTE,
            anchor_ms: 0,
            bucket: i64::MIN,
            forming: None,
        }
    }
    pub fn weeks() -> Self {
        HigherTimeframeBars {
            length_ms: MS_PER_WEEK,
            anchor_ms: FIRST_MONDAY_MS,
            bucket: i64::MIN,
            forming: None,
        }
    }
    fn bucket_of(&self, ts: i64) -> i64 {
        (ts - self.anchor_ms).div_euclid(self.length_ms)
    }
    /// Takes one chart bar and returns the higher bar it closes, if any. A data gap can swallow a bucket's last chart bar, and
    /// that bucket then closes on the next bar held. A second close due on that same bar waits one bar more.
    pub fn push(&mut self, bar: &Bar) -> Option<Bar> {
        let bucket = self.bucket_of(bar.timestamp);
        let late = if bucket == self.bucket { None } else { self.forming.take() };
        self.bucket = bucket;
        match &mut self.forming {
            Some(forming) => {
                forming.high = forming.high.max(bar.high);
                forming.low = forming.low.min(bar.low);
                forming.close = bar.close;
                forming.volume += bar.volume;
            }
            None => self.forming = Some(bar.clone()),
        }
        if late.is_none() && self.bucket_of(bar.timestamp + CHART_BAR_MS) != bucket {
            return self.forming.take();
        }
        late
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use vike_marketdata::Bar;

    use super::{
        HigherTimeframeBars, HigherTimeframeSource, HigherTimeframeSourceCalendar,
        HigherTimeframeSourceIntraday, with_higher_timeframe_source,
    };
    use crate::{HigherTimeframePeriod, TacticsInput};

    /// Hands out the bar it is fed, marked.
    struct Marked;

    impl HigherTimeframeSource for Marked {
        fn calendar(
            &self,
            _every: HigherTimeframePeriod,
        ) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send> {
            Box::new(|bar| vec![Bar { volume: -1.0, ..bar.clone() }])
        }
        fn intraday(&self, _minutes: i64) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send> {
            Box::new(|bar| vec![Bar { volume: -2.0, ..bar.clone() }])
        }
        fn ahead(&self, _every: HigherTimeframePeriod) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send> {
            Box::new(|bar| vec![Bar { volume: -3.0, ..bar.clone() }])
        }
    }

    /// A frame read with lookahead on is built from the daily input for a calendar period and from the
    /// 30-minute one for minutes (what its rule declares); a frame of minutes that 30-minute bars do not
    /// tile is refused.
    #[test]
    fn a_frame_ahead_names_the_input_it_is_built_from() {
        use super::{DAY, HALF_HOUR, HigherTimeframeSourceAhead};
        for every in [
            HigherTimeframePeriod::Day,
            HigherTimeframePeriod::Week,
            HigherTimeframePeriod::Month,
            HigherTimeframePeriod::Quarter,
        ] {
            assert_eq!(HigherTimeframeSourceAhead::new(every).input(), DAY, "{every:?}");
        }
        let hours = HigherTimeframeSourceAhead::new(HigherTimeframePeriod::Minutes(240));
        assert_eq!(hours.input(), HALF_HOUR);
        let odd = std::panic::catch_unwind(|| {
            HigherTimeframeSourceAhead::new(HigherTimeframePeriod::Minutes(45))
        });
        assert!(odd.is_err(), "45 minutes is no multiple of 30");
    }

    fn bar(ts: i64) -> Bar {
        Bar::ohlcv(ts, 1.0, 1.0, 1.0, 1.0, 1.0)
    }

    /// A frame made while a source is installed is the source's for its whole life; one made after it
    /// is uninstalled reads its input again.
    #[test]
    fn a_frame_made_under_a_source_is_the_sources_and_only_then() {
        let none = TacticsInput::new(&[]);
        let (mut day, mut hour) = with_higher_timeframe_source(Rc::new(Marked), || {
            (
                HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
                HigherTimeframeSourceIntraday::new(60),
            )
        });
        assert_eq!(day.next(&bar(0), none).iter().map(|b| b.volume).collect::<Vec<_>>(), [-1.0]);
        assert_eq!(hour.next(&bar(0), none).iter().map(|b| b.volume).collect::<Vec<_>>(), [-2.0]);
        let mut plain = HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day);
        assert!(plain.next(&bar(0), none).is_empty(), "no source: the input, which holds nothing");
    }

    /// `HigherTimeframeSourceUnseen::next` before its early return: `since` searched on every call.
    #[derive(Default)]
    struct SearchedEveryCall {
        last: Option<i64>,
    }

    impl SearchedEveryCall {
        fn next<'a>(&mut self, input: TacticsInput<'a>) -> &'a [Bar] {
            let new = match self.last {
                None => input.bars(usize::MAX),
                Some(ts) => input.since(ts + 1),
            };
            if let Some(b) = new.last() {
                self.last = Some(b.timestamp);
            }
            new
        }
    }

    /// The early return changes no answer: over 340 days of 15-minute chart bars, each read twice a bar
    /// as a rule reading days and weeks reads it, the unseen days are the very slice (address and length)
    /// the search found, and the days and weeks handed out are the same bars. The daily input has its
    /// history before the chart, weekends missing for 100 days, a 20-day hole and single missing days;
    /// the chart misses a 3-day stretch and some hours, so one chart bar may close several days.
    #[test]
    fn the_early_return_hands_out_what_the_search_did_over_gaps_and_day_boundaries() {
        use super::{HigherTimeframeSourceUnseen, MS_PER_DAY};
        use crate::HigherTimeframe;
        let quarter = 900_000;
        let held = |d: i64| {
            !((100..200).contains(&d) && d % 7 >= 5) && !(250..270).contains(&d) && d % 13 != 0
        };
        let days: Vec<Bar> = (0..420).filter(|&d| held(d)).map(|d| bar(d * MS_PER_DAY)).collect();
        let chart = (60 * 96..400 * 96)
            .filter(|q| !(300 * 96..303 * 96).contains(q) && q % 96 / 4 != 7)
            .map(|q| q * quarter);
        let (mut unseen, mut searched) =
            (HigherTimeframeSourceUnseen::new(), SearchedEveryCall::default());
        let mut calendars = [HigherTimeframePeriod::Day, HigherTimeframePeriod::Week]
            .map(|every| (HigherTimeframeSourceCalendar::new(every), SearchedEveryCall::default()));
        let mut week_frame = HigherTimeframe::new(HigherTimeframePeriod::Week, MS_PER_DAY);
        let (mut handed, mut empty, mut several) = (0, 0, 0);
        for ts in chart {
            let closed =
                &days[..days.partition_point(|d| d.timestamp + MS_PER_DAY <= ts + quarter)];
            let input = TacticsInput::new(closed);
            for _ in 0..2 {
                let (new, old) = (unseen.next(input), searched.next(input));
                assert!(std::ptr::eq(new, old), "at {ts}: {} vs {} days", new.len(), old.len());
                (handed, empty) = (handed + new.len(), empty + usize::from(new.is_empty()));
                several += usize::from(new.len() > 1);
            }
            let chart_bar = bar(ts);
            let [(day, day_old), (week, week_old)] = &mut calendars;
            assert_eq!(day.next(&chart_bar, input), day_old.next(input).to_vec(), "days at {ts}");
            let built: Vec<Bar> =
                week_old.next(input).iter().flat_map(|d| week_frame.next(d, None)).collect();
            assert_eq!(week.next(&chart_bar, input), built, "weeks at {ts}");
        }
        assert_eq!(handed, days.partition_point(|d| d.timestamp < 400 * MS_PER_DAY));
        assert!(empty > 60_000 && several > 1, "{empty} empty calls, {several} with several days");
    }

    /// An hour of 15-minute bars closes on its fourth, stamped with its first; a gap closes the bucket late.
    #[test]
    fn an_hour_closes_on_its_last_quarter_and_late_after_a_gap() {
        let quarter = 900_000;
        let mut hours = HigherTimeframeBars::minutes(60);
        let closed: Vec<Option<i64>> =
            (0..4).map(|i| hours.push(&bar(i * quarter)).map(|b| b.timestamp)).collect();
        assert_eq!(closed, [None, None, None, Some(0)]);
        assert_eq!(hours.push(&bar(4 * quarter)).map(|b| b.timestamp), None);
        // the hour's last quarter is missing: the next bar held closes it
        assert_eq!(hours.push(&bar(8 * quarter)).map(|b| b.timestamp), Some(4 * quarter));
    }
}
