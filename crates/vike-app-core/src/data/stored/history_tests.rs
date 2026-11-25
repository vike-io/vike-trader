//! The view rule, frame by frame: what a chart asks for when it opens, zooms out, zooms in, scrolls and
//! comes back, against a fake datahub that resamples with the datahub's own fold and counts every read.

use std::sync::Mutex;

use vike_data::{HistoryStore, HistoryStoreMemory};

use super::*;
use crate::data::stored::{ChartBarSource, StoredBarRequest, plan_store_reads, read_into_store};
use crate::ui::series_follow::ChartSeriesFollowTarget;

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// 2024-01-01T00:00:00Z: a day boundary, so the windows below are whole days.
const NOW: i64 = 1_704_067_200_000;
const WIDE: f32 = 2_000.0;
const KEY: &str = "bybit:BTCUSDT@1m";

fn chart(has_bars: bool) -> ChartSeriesFollowTarget {
    ChartSeriesFollowTarget {
        id: egui::Id::unique("the chart"),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        pinned: false,
        interval_pinned: false,
        has_bars,
        first_open: None,
        class: None,
    }
}

fn span(t0: i64, t1: i64) -> ChartViewSpan {
    ChartViewSpan { timestamps: Some((t0, t1)), width_px: WIDE }
}

fn narrow(t0: i64, t1: i64) -> ChartViewSpan {
    ChartViewSpan { timestamps: Some((t0, t1)), width_px: 100.0 }
}

/// Show `view` at `at`, then plan once the debounce has passed.
fn settle(history: &mut ChartHistory, view: ChartViewSpan, at: i64) -> Vec<StoredBarRequest> {
    history.observe_view(chart(true).id, Some(view), at);
    plan_store_reads(&[chart(true)], &[], history, at + VIEW_DEBOUNCE_MS)
}

/// The window of the one read planned.
fn only(plan: &[StoredBarRequest]) -> &crate::data::stored::StoredBarView {
    assert_eq!(plan.len(), 1, "one read: {plan:?}");
    plan[0].view.as_ref().expect("a view read")
}

/// One read the fake datahub was asked: the interval it resampled to (`None` for the stored bars) and the range.
type AskedRead = (Option<String>, Option<i64>, Option<i64>);

/// A datahub double: the stored `1m` bars, resampled with `vike_marketdata::resample` exactly as the datahub
/// answers (`crates/vike-datahub-client/tests/remote_bar_resample.rs` holds the real one to it), and every
/// read counted.
struct FakeDatahub {
    store: HistoryStoreMemory,
    resamples: Option<bool>,
    reads: Mutex<Vec<AskedRead>>,
}

impl FakeDatahub {
    fn holding(days: i64, resamples: Option<bool>) -> Self {
        let store = HistoryStoreMemory::default();
        let bars: Vec<Bar> = (0..days * 1_440)
            .map(|i| Bar::ohlcv(NOW - days * DAY + i * MINUTE, 1.0, 2.0, 0.5, 1.5, 1.0))
            .collect();
        store.append_bars("bybit", "BTCUSDT", "1m", &bars, None).expect("seed");
        Self { store, resamples, reads: Mutex::default() }
    }

    fn reads(&self) -> Vec<AskedRead> {
        std::mem::take(&mut *self.reads.lock().unwrap())
    }
}

impl HistoryStore for FakeDatahub {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: vike_data::TimestampRange,
    ) -> Result<Vec<Bar>, vike_data::HistoryStoreError> {
        self.reads.lock().unwrap().push((None, range.start, range.end));
        self.store.load_bars(venue, symbol, interval, range)
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

impl ChartBarSource for FakeDatahub {
    fn serves_resample(&self) -> Option<bool> {
        self.resamples
    }

    fn load_bars_resampled(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: vike_data::TimestampRange,
        to: &str,
    ) -> Result<Option<Vec<Bar>>, vike_data::HistoryStoreError> {
        // `None` is an older datahub whose `Welcome` could not be read: it does not resample either.
        if self.resamples != Some(true) {
            return Ok(None);
        }
        self.reads.lock().unwrap().push((Some(to.to_string()), range.start, range.end));
        let rows = self.store.load_bars(venue, symbol, interval, range)?;
        Ok(Some(vike_marketdata::resample(&rows, bar_step_ms(to).expect("a width"))))
    }
}

/// Plan, read and land one frame's reads; the chart then holds what landed.
fn read(history: &ChartHistory, fake: &FakeDatahub, plan: &[StoredBarRequest]) -> usize {
    let bars = crate::data::data_sink::StoredBarStore::default();
    read_into_store(plan, fake, &bars, &history.cache(), NOW, 3_000).landed
}

/// OPEN: an empty chart 2,000 points wide asks for its interval's newest 2,000 bars, and nothing resampled.
#[test]
fn an_empty_chart_asks_for_its_newest_width_of_bars() {
    let mut history = ChartHistory::default();
    history.observe_view(
        chart(false).id,
        Some(ChartViewSpan { timestamps: None, width_px: WIDE }),
        NOW,
    );
    let plan = plan_store_reads(&[chart(false)], &[], &mut history, NOW);
    let view = only(&plan);
    assert_eq!((view.window.step_ms, view.window.bars(), view.resample_to), (MINUTE, 2_000, None));
    assert_eq!(view.window.end, NOW + MINUTE - 1, "the newest bar is the one holding now");
    assert!(view.opening);
    assert!(
        plan_store_reads(&[chart(false)], &[], &mut history, NOW + 1).is_empty(),
        "in flight: once"
    );
}

/// ZOOM OUT over three years of a `1m` chart: at most 2,000 candles, the daily ones, about 1,096.
#[test]
fn zooming_out_over_three_years_asks_for_about_1096_daily_candles() {
    let mut history = ChartHistory::default();
    let plan = settle(&mut history, span(NOW - 1_096 * DAY, NOW), NOW);
    let view = only(&plan);
    assert_eq!((view.window.step_ms, view.resample_to), (DAY, Some("1d")));
    assert!(view.window.bars() <= MAX_VIEW_CANDLES, "{}", view.window.bars());
    assert!((1_095..=1_097).contains(&view.window.bars()), "{}", view.window.bars());
    assert_eq!(view.missing, vec![(view.window.start, view.window.end)], "nothing cached yet");
    // A narrower plot asks for coarser candles over a shorter span.
    let mut narrow = ChartHistory::default();
    narrow.observe_view(
        chart(true).id,
        Some(ChartViewSpan { width_px: 600.0, ..span(NOW - 90 * DAY, NOW) }),
        NOW,
    );
    let plan = plan_store_reads(&[chart(true)], &[], &mut narrow, NOW + VIEW_DEBOUNCE_MS);
    assert_eq!(
        only(&plan).window.step_ms,
        4 * 60 * MINUTE,
        "90 days on 600 points: 540 four-hour bars"
    );
}

/// ZOOM IN to one day: that day's `1m` bars, and only that day.
#[test]
fn zooming_into_one_day_asks_for_that_days_minutes_only() {
    let mut history = ChartHistory::default();
    let day = NOW - 30 * DAY;
    let plan = settle(&mut history, span(day, day + DAY - MINUTE), NOW);
    let view = only(&plan);
    assert_eq!((view.window.step_ms, view.resample_to), (MINUTE, None));
    assert_eq!((view.window.start, view.window.end), (day, day + DAY - 1));
    assert_eq!(view.missing, vec![(day, day + DAY - 1)]);
}

/// The DEBOUNCE: a view still moving asks nothing; the same view held for `VIEW_DEBOUNCE_MS` asks once.
#[test]
fn a_moving_view_asks_nothing_until_it_holds_still() {
    let mut history = ChartHistory::default();
    for frame in 0..10 {
        let t = NOW + frame * 16;
        history.observe_view(chart(true).id, Some(span(NOW - (10 + frame) * DAY, NOW)), t);
        assert!(plan_store_reads(&[chart(true)], &[], &mut history, t).is_empty(), "frame {frame}");
    }
    let still = NOW + 9 * 16;
    assert!(
        plan_store_reads(&[chart(true)], &[], &mut history, still + VIEW_DEBOUNCE_MS - 1)
            .is_empty()
    );
    assert_eq!(
        plan_store_reads(&[chart(true)], &[], &mut history, still + VIEW_DEBOUNCE_MS).len(),
        1
    );
}

/// SCROLL BACK a week from a one-day view: the missing window only, and coming back costs nothing.
#[test]
fn scrolling_back_a_week_asks_the_missing_day_only_and_coming_back_asks_nothing() {
    let fake = FakeDatahub::holding(14, Some(true));
    let mut history = ChartHistory::default();
    let day = NOW - 3 * DAY;
    let plan = settle(&mut history, span(day, day + DAY - MINUTE), NOW);
    assert_eq!(read(&history, &fake, &plan), 1);
    assert_eq!(fake.reads(), vec![(None, Some(day), Some(day + DAY - 1))]);

    let week_back = day - 7 * DAY;
    let plan = settle(&mut history, span(week_back, week_back + DAY - MINUTE), NOW + 1_000);
    assert_eq!(
        only(&plan).missing,
        vec![(week_back, week_back + DAY - 1)],
        "the missing day, only"
    );
    assert_eq!(read(&history, &fake, &plan), 1);
    assert_eq!(fake.reads(), vec![(None, Some(week_back), Some(week_back + DAY - 1))]);

    // Half a day further back: only the half not held.
    let half = week_back - DAY / 2;
    let plan = settle(&mut history, span(half, half + DAY - MINUTE), NOW + 2_000);
    assert_eq!(only(&plan).missing, vec![(half, week_back - 1)]);
    assert_eq!(read(&history, &fake, &plan), 1);
    let _ = fake.reads();

    // Back to the first day: a cache hit, landed with no read at all.
    let plan = settle(&mut history, span(day, day + DAY - MINUTE), NOW + 3_000);
    assert!(only(&plan).missing.is_empty(), "a seen view is cached");
    assert_eq!(read(&history, &fake, &plan), 1, "it still lands");
    assert!(fake.reads().is_empty(), "and asks the datahub nothing");
}

/// A landed display's own Reset view (one empty bar each side) plans nothing, at every width, so landing
/// never asks again; and the chart is re-framed onto it exactly once.
#[test]
fn a_landed_display_plans_nothing_and_is_reframed_once() {
    // A 100-point plot over 30 days: daily candles, so a month of minutes is enough to show it.
    let fake = FakeDatahub::holding(32, Some(true));
    let mut history = ChartHistory::default();
    let plan = settle(&mut history, narrow(NOW - 30 * DAY, NOW), NOW);
    assert_eq!(read(&history, &fake, &plan), 1);
    let reads = fake.reads();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].0.as_deref(), Some("1d"), "resampled by the datahub, one read");

    let shown = history.cache().lock().bars_in(KEY, only(&plan).window);
    assert_eq!(shown.len(), 30, "the 30 whole days held; today's has no minute yet");
    let chart_bars: Vec<ChartBar> = shown
        .iter()
        .enumerate()
        .map(|(i, b)| ChartBar {
            t: i as f64,
            ot: b.timestamp,
            o: b.open,
            h: b.high,
            l: b.low,
            c: b.close,
            v: b.volume,
        })
        .collect();
    assert!(!history.take_reframe(KEY, &chart_bars[1..]), "not before the chart holds the display");
    assert!(history.take_reframe(KEY, &chart_bars), "the frame that holds it re-frames");
    assert!(!history.take_reframe(KEY, &chart_bars), "once");

    let (first, last) = (shown[0].timestamp, shown[shown.len() - 1].timestamp);
    let reset = narrow(first - DAY, last + DAY);
    assert!(
        settle(&mut history, reset, NOW + 10_000).is_empty(),
        "its own reset view is the display"
    );
}

/// AN OLDER DATAHUB (no `bar_resample`): today's read, the last `history_bars` bars of the chart's interval
/// once, and nothing on zoom or scroll.
#[test]
fn an_older_datahub_is_read_as_before_and_never_on_zoom() {
    for resamples in [Some(false), None] {
        let fake = FakeDatahub::holding(5, resamples);
        let mut history = ChartHistory::default();
        let plan = plan_store_reads(&[chart(false)], &[], &mut history, NOW);
        assert_eq!(plan.len(), 1);
        if resamples == Some(false) {
            assert_eq!(read(&history, &fake, &plan), 1);
            assert_eq!(
                fake.reads(),
                vec![(None, Some(NOW - 3_000 * MINUTE), Some(NOW))],
                "exactly today's request: the last 3,000 bars ending now"
            );
        } else {
            // A store that could not say opens by its view and learns at the first resample it asks.
            assert_eq!(read(&history, &fake, &plan), 1);
            let _ = fake.reads();
            let zoom = settle(&mut history, span(NOW - 400 * DAY, NOW), NOW);
            assert_eq!(only(&zoom).resample_to, Some("1d"));
            read(&history, &fake, &zoom);
            assert!(fake.reads().iter().all(|r| r.0.is_none()), "nothing resampled was sent");
        }
        assert_eq!(history.cache().lock().resamples, Some(false));
        assert!(
            settle(&mut history, span(NOW - 400 * DAY, NOW), NOW + 60_000).is_empty(),
            "no zoom read"
        );
    }
}

/// The cache keeps under its ceiling by dropping the entry used longest ago, never the one just written.
#[test]
fn the_cache_drops_the_oldest_entry_past_its_ceiling() {
    let mut state = ChartHistoryCacheState::default();
    let rows = |from: i64, n: i64| -> Vec<Bar> {
        (0..n).map(|i| Bar::ohlcv(from + i * MINUTE, 1.0, 1.0, 1.0, 1.0, 1.0)).collect()
    };
    let half = CACHE_BARS_CEILING as i64 / 2 + 1;
    state.hold("a", MINUTE, (0, half * MINUTE - 1), rows(0, half), NOW);
    state.hold("b", MINUTE, (0, half * MINUTE - 1), rows(0, half), NOW);
    let window = ChartHistoryWindow { step_ms: MINUTE, start: 0, end: MINUTE - 1 };
    assert!(state.bars_in("a", window).is_empty(), "the older entry went");
    assert_eq!(state.bars_in("b", window).len(), 1, "the one just written stays");
}
