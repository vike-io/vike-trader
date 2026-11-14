//! A higher timeframe, as strategy files read one: a day of the run's own symbol is its daily INPUT
//! (`crate::Tactics::input("1d")`, decision 0271: the venue's own daily bars, its whole stored
//! history before the window included, each seen once it has closed); a week or a month is built from
//! those days, and an hour or four from the rule's own bars, by `crate::HigherTimeframe`. Copied from
//! the strategy gallery (plan 2b, C3), with [`HigherTimeframeBars`], the bucketed frame 18 files share; the
//! Heikin-Ashi ticker on the tick is `vike_indicators::strategy_kit::HeikinAshiCandles`, as this crate names no
//! indicator (decision 0269).
//!
//! The reference's call for a higher timeframe, without lookahead, sees the last higher bar closed by
//! the chart bar's close; a day, a week and a month reach back into the venue's history before the
//! window, and a frame of minutes or hours starts at the window. Each reading is the strategy lab's,
//! measured against the reference rows (its `strategy_lab_b/timeframe.rs` has the numbers).
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

use crate::{HigherTimeframe, HigherTimeframePeriod, TacticsInput};

/// Another builder of this module's frames (the module doc): each method makes one, as a frame of that
/// kind is made.
pub trait HigherTimeframeSource {
    /// A day, a week or a month ([`HigherTimeframeSourceCalendar`]).
    fn calendar(&self, every: HigherTimeframePeriod) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send>;
    /// A frame of `minutes` ([`HigherTimeframeSourceIntraday`]).
    fn intraday(&self, minutes: i64) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send>;
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

const DAY_MS: i64 = 86_400_000;

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
            HigherTimeframePeriod::Week | HigherTimeframePeriod::Month => {
                Some(HigherTimeframe::new(every, DAY_MS))
            }
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

/// The length of the chart bar [`HigherTimeframeBars`] is fed: its files run on 15-minute bars.
const CHART_BAR_MS: i64 = 900_000;
/// 1970-01-05 00:00 UTC, a Monday.
const FIRST_MONDAY_MS: i64 = 4 * 86_400_000;
const WEEK_MS: i64 = 7 * 86_400_000;

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
            length_ms: minutes * 60_000,
            anchor_ms: 0,
            bucket: i64::MIN,
            forming: None,
        }
    }
    pub fn weeks() -> Self {
        HigherTimeframeBars {
            length_ms: WEEK_MS,
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
