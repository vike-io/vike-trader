//! A RECORDED order book turned into training data for a DeepLOB-style network (Zhang, Zohren and
//! Roberts, "DeepLOB: Deep Convolutional Neural Networks for Limit Order Books", 2019): one row per
//! sample of the best `levels` price levels on each side, and a three-class label saying where the
//! mid price is `horizon_events` book events later.
//!
//! # The contract
//!
//! - **The input** is a recorded book chain in store order `(timestamp, sequence)`, as
//!   `vike_data::HistoryStore::scan_book_updates` (the lossless lane) or `scan_depth` (the conflating
//!   lane) return it. A study reads them through `StudyContext::book_updates` / `StudyContext::depth`,
//!   so this module needs no storage of its own.
//! - **The book is the backtest's.** Every event folds through [`apply_book_event`] on the event's own
//!   `tick_size`, exactly as the simulator's replay folds it before a strategy's `on_order_book`, so a
//!   network trains on the books a strategy is later handed. A hole in the sequence, an orphan delta
//!   or a `GapStart`/`Stale` marker leaves NO book until the next snapshot, and nothing is sampled
//!   off a missing book.
//! - **An event** is a fold that changed the book: a snapshot, or an in-sequence delta (one
//!   `on_order_book` in a backtest). The label horizon counts these.
//! - **Sampling.** [`BookSampling::EveryEvent`] takes a row after each event.
//!   [`BookSampling::Interval`] takes a row at each multiple of `milliseconds` (epoch-aligned UTC),
//!   from the book after every event whose `timestamp` is at or before that instant. A boundary
//!   after the last event is not sampled: no later event exists to label it. A sample needs a book
//!   with both sides present (a mid); one without is counted in
//!   [`BookTrainingData::skipped_without_book`].
//! - **Point in time.** A row's features come from events at or before the row's time and never from
//!   a later one; only the LABEL looks ahead, by design. Cutting the input short never changes a row
//!   that survives the cut (`book_export_tests`' `a_shorter_input_keeps_every_surviving_row`).
//! - **The label.** With `p[j]` the mid after event `j`, the row's event `i` and `k =
//!   horizon_events`, [`BookLabel`] picks the relative move `r`; the default, [`BookLabel::MidAfter`],
//!   is `r = (p[i+k] - p[i]) / p[i]`, and the two SMOOTHED forms average the next `k` mids. Then `+1`
//!   when `r > up_threshold`, `-1` when `r < -down_threshold`, `0` otherwise. A row whose mids run
//!   past the input, cross a break in the chain (they sit on another anchor) or reach a one-sided
//!   book has no honest label, and is counted in [`BookTrainingData::skipped_without_label`].
//! - **A label filter selects on the future.** [`export_book_training_data`] DROPS a row without a
//!   label. A training set may leave it out, but whether a label can be formed is decided by what
//!   happens AFTER the row, so a set JUDGED on those rows is selected on the future: the moments
//!   just before a side of the book empties (often the most volatile ones) never appear in it, and
//!   a live strategy cannot know in advance that the next `k` events stay two-sided.
//!   [`export_book_rows_keeping_unlabelled`] keeps every row the book at the row's own time allows,
//!   each one without a label marked `false` in [`BookTrainingData::labelled`], so an evaluation
//!   scores it as its own outcome instead of never seeing it.
//! - **The label's reach.** `k` counts EVENTS, so the label of a row ends whenever the book printed
//!   its `k`-th event after it: a second in a busy book, a minute in a quiet one. Each row records
//!   that instant ([`BookTrainingData::label_end_timestamps`], the time of event `i + k`, or of the
//!   last event when the input ends first, which only a row without a label meets), and a split
//!   that fits on early windows and judges later ones starts judging after the latest fitted label
//!   end ([`BookWindows::first_judged_window`]), never a fixed count of windows later.
//! - **The features**, row-major, four columns per level in DeepLOB's order: ask price, ask
//!   quantity, bid price, bid quantity for level 1 (the best), then level 2, and so on
//!   ([`BookTrainingData::column_names`]). A level the book does not have is a NaN price and a zero
//!   quantity: nothing is invented.
//! - **The network's input.** [`book_windows`] stacks each row with the rows before it into DeepLOB's
//!   window of `steps` book rows, scaled by the window itself (prices in basis points from its last
//!   mid, quantities over its mean quantity), so a window holds nothing later than its own last row.
//! - Pure: no clock, file or randomness, and arithmetic only (no transcendental).

use vike_marketdata::{L2Book, L2BookUpdate, apply_book_event};

/// When a row is taken. See the module doc's "Sampling".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookSampling {
    /// One row after every event (every change of the book).
    EveryEvent,
    /// One row at every multiple of `milliseconds` (UTC epoch), from the book as it stood then.
    Interval {
        /// The sampling step; must be positive.
        milliseconds: i64,
    },
}

/// How the label measures the mid move over `k = horizon_events` events, `p[j]` being the mid after
/// event `j` and `i` the row's event. See the module doc's "The label".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BookLabel {
    /// `r = (p[i+k] - p[i]) / p[i]`: the mid `k` events later against the mid now. The default.
    #[default]
    MidAfter,
    /// `r = (mean(p[i+1..=i+k]) - p[i]) / p[i]`: the mean of the next `k` mids against the mid now.
    /// The smoothed label of the FI-2010 benchmark (Ntakaris et al., 2018), which DeepLOB trains on
    /// for that data set.
    MeanOfNext,
    /// `r = (mean(p[i+1..=i+k]) - mean(p[i-k+1..=i])) / mean(p[i-k+1..=i])`: the mean of the next `k`
    /// mids against the mean of the last `k`, the current one included. DeepLOB's own label (Zhang,
    /// Zohren and Roberts, 2019, equations 3 and 4; their `m_-` sum written from `i = 0` to `k` is read
    /// as `k` terms). A row needs `k - 1` earlier events on its anchor too.
    MeanOfNextAgainstMeanOfLast,
}

/// What [`export_book_training_data`] builds: the depth, the sampling and the label.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BookExportParameters {
    /// Price levels per side in a row (DeepLOB uses 10); at least 1.
    pub levels: usize,
    /// When a row is taken.
    pub sampling: BookSampling,
    /// `k`: the label compares the mid now with the mid (or the mean of the mids) this many events
    /// later; at least 1.
    pub horizon_events: usize,
    /// How the move over the horizon is measured.
    pub label: BookLabel,
    /// The label is `+1` when the relative mid change is strictly above this; finite and `>= 0`.
    pub up_threshold: f64,
    /// The label is `-1` when the relative mid change is strictly below minus this; finite and `>= 0`.
    pub down_threshold: f64,
}

/// The rows [`export_book_training_data`] built: every vector holds one entry per row, in time order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BookTrainingData {
    /// Price levels per side; a row has `4 * levels` feature columns.
    pub levels: usize,
    /// The row's time (epoch milliseconds, UTC): the event's `timestamp`, or the interval boundary.
    pub timestamps: Vec<i64>,
    /// `rows * 4 * levels` values, row-major, in [`BookTrainingData::column_names`] order.
    pub features: Vec<f64>,
    /// The mid price the row's label is measured from.
    pub mids: Vec<f64>,
    /// The relative mid change over the horizon, `(m' - m) / m`, that the label was cut from; NaN
    /// for a row without a label.
    pub horizon_returns: Vec<f64>,
    /// `-1` (down), `0` (flat) or `+1` (up); `0` for a row without a label, so read
    /// [`BookTrainingData::labelled`] first.
    pub labels: Vec<i8>,
    /// Whether the row has a label. Always `true` from [`export_book_training_data`]; `false` for a
    /// row [`export_book_rows_keeping_unlabelled`] kept without one.
    pub labelled: Vec<bool>,
    /// The time (epoch milliseconds, UTC) of the last event the row's label reads, event `i + k`:
    /// the label's actual reach. See the module doc's "The label's reach".
    pub label_end_timestamps: Vec<i64>,
    /// Samples taken while the book was missing or one-sided: no row.
    pub skipped_without_book: usize,
    /// Samples whose horizon ran past the input, across a break in the chain or onto a one-sided
    /// book: no label. [`export_book_training_data`] leaves them out (no row);
    /// [`export_book_rows_keeping_unlabelled`] keeps each as a row whose `labelled` is `false`.
    pub skipped_without_label: usize,
}

impl BookTrainingData {
    /// The number of rows.
    pub fn rows(&self) -> usize {
        self.labels.len()
    }

    /// Feature columns per row: `4 * levels`.
    pub fn columns(&self) -> usize {
        4 * self.levels
    }

    /// The feature column names in row order: `ask_price_1`, `ask_quantity_1`, `bid_price_1`,
    /// `bid_quantity_1`, then the same for level 2, and so on.
    pub fn column_names(&self) -> Vec<String> {
        (1..=self.levels)
            .flat_map(|level| {
                ["ask_price", "ask_quantity", "bid_price", "bid_quantity"]
                    .map(|name| format!("{name}_{level}"))
            })
            .collect()
    }
}

/// A sample read off the book before its label is known.
struct Candidate {
    timestamp: i64,
    /// The index of the last event folded into the book it was read off.
    event: usize,
    mid: f64,
}

/// Turn a recorded book chain into DeepLOB-style training rows. See the module doc for the contract.
/// A row without a label is left out, which selects on the future (the module doc's "A label
/// filter selects on the future"): a set judged on these rows needs
/// [`export_book_rows_keeping_unlabelled`].
///
/// Refuses parameters it cannot honour (no level, a zero horizon, a non-positive interval, a
/// threshold that is negative, NaN or infinite) with the reason, so a study can pass the words on.
pub fn export_book_training_data(
    updates: &[L2BookUpdate],
    parameters: &BookExportParameters,
) -> Result<BookTrainingData, String> {
    export_rows(updates, parameters, false)
}

/// [`export_book_training_data`]'s rows, plus every row whose label cannot be formed, kept with
/// `labelled` `false`: which rows exist is decided by the book at each row's own time alone, so a
/// set judged on them is not selected on the future. Same refusals.
pub fn export_book_rows_keeping_unlabelled(
    updates: &[L2BookUpdate],
    parameters: &BookExportParameters,
) -> Result<BookTrainingData, String> {
    export_rows(updates, parameters, true)
}

fn export_rows(
    updates: &[L2BookUpdate],
    parameters: &BookExportParameters,
    keep_unlabelled: bool,
) -> Result<BookTrainingData, String> {
    validate(parameters)?;
    let levels = parameters.levels;
    let mut data = BookTrainingData { levels, ..BookTrainingData::default() };

    let mut slot: Option<L2Book> = None;
    // Per event: the mid after it (NaN when one-sided) and the anchor it sits on. The anchor moves on
    // at every break of the chain, so two events on one anchor have every event between them.
    let mut event_mids: Vec<f64> = Vec::new();
    let mut event_timestamps: Vec<i64> = Vec::new();
    let mut event_anchors: Vec<usize> = Vec::new();
    let mut anchor = 0usize;
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut candidate_features: Vec<f64> = Vec::new();
    let mut next_boundary: Option<i64> = None;

    for update in updates {
        if let BookSampling::Interval { milliseconds } = parameters.sampling {
            let boundary =
                next_boundary.get_or_insert_with(|| first_boundary(update.timestamp, milliseconds));
            // Every boundary before this event sees the book as it stands: this event is later.
            while *boundary < update.timestamp {
                // The last event folded; never read while no event has made a book.
                let event = event_mids.len().saturating_sub(1);
                if !sample(
                    &slot,
                    *boundary,
                    event,
                    levels,
                    &mut candidates,
                    &mut candidate_features,
                ) {
                    data.skipped_without_book += 1;
                }
                *boundary += milliseconds;
            }
        }
        let changed = apply_book_event(&mut slot, update, update.tick_size, |book| book);
        if slot.is_none() {
            anchor += 1;
        }
        if changed {
            event_mids.push(slot.as_ref().and_then(L2Book::mid).unwrap_or(f64::NAN));
            event_timestamps.push(update.timestamp);
            event_anchors.push(anchor);
            if parameters.sampling == BookSampling::EveryEvent {
                let event = event_mids.len() - 1;
                if !sample(
                    &slot,
                    update.timestamp,
                    event,
                    levels,
                    &mut candidates,
                    &mut candidate_features,
                ) {
                    data.skipped_without_book += 1;
                }
            }
        }
    }

    let columns = data.columns();
    for (candidate, features) in candidates.iter().zip(candidate_features.chunks_exact(columns)) {
        let horizon_return = label_return(parameters, &event_mids, &event_anchors, candidate);
        let labelled = horizon_return.is_finite();
        if !labelled {
            data.skipped_without_label += 1;
            if !keep_unlabelled {
                continue;
            }
        }
        let horizon_return = if labelled { horizon_return } else { f64::NAN };
        // A NaN compares false both ways, so a row without a label reads 0.
        let label = if horizon_return > parameters.up_threshold {
            1
        } else if horizon_return < -parameters.down_threshold {
            -1
        } else {
            0
        };
        data.timestamps.push(candidate.timestamp);
        data.features.extend_from_slice(features);
        data.mids.push(candidate.mid);
        data.horizon_returns.push(horizon_return);
        data.labels.push(label);
        data.labelled.push(labelled);
        // A label read event `i + k`; a row without one may run past the last event. A candidate
        // needs a book, so at least one event exists.
        let reach = candidate.event.saturating_add(parameters.horizon_events);
        data.label_end_timestamps.push(event_timestamps[reach.min(event_timestamps.len() - 1)]);
    }
    Ok(data)
}

/// A DeepLOB input: [`book_windows`]' windows of book rows, scaled, with one label each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BookWindows {
    /// Rows (steps) per window.
    pub steps: usize,
    /// Numbers per step: `4 * levels`, in [`BookTrainingData::column_names`] order.
    pub columns: usize,
    /// `windows * steps * columns` values: window after window, each oldest step first (step-major,
    /// the layout of DeepLOB's `[1, steps, 4 * levels]` image).
    pub features: Vec<f64>,
    /// Each window's label: its LAST row's.
    pub labels: Vec<i8>,
    /// Each window's time: its last row's.
    pub timestamps: Vec<i64>,
    /// Each window's label end: its last row's [`BookTrainingData::label_end_timestamps`].
    pub label_end_timestamps: Vec<i64>,
    /// Whether each window has a label: its last row's [`BookTrainingData::labelled`]. A fit leaves
    /// out a window without one; an evaluation scores it (it is not selected away on the future).
    pub labelled: Vec<bool>,
    /// Windows dropped because a row in them misses a level (a NaN price) or holds no quantity.
    pub skipped_incomplete: usize,
}

impl BookWindows {
    /// The number of windows.
    pub fn rows(&self) -> usize {
        self.labels.len()
    }

    /// The first window a network fitted on the windows before `fitted` may be judged on: the first
    /// whose own time is AFTER every fitted window's label end, then `embargo` windows more. So no
    /// label the network learned from reads an event a judged label reads, however long the book
    /// took to print the `k` events (a fixed count of windows covers them only in a busy book). A
    /// fitted window without a label counts too: it is not learned from, so it can only skip more.
    ///
    /// The bound is checked, not assumed: the first window past the reach follows the LAST window at
    /// or before it, so every window from the answer on is after it, in whatever order the times
    /// come. Refuses, in words, when nothing is fitted or no window is left to judge.
    pub fn first_judged_window(&self, fitted: usize, embargo: usize) -> Result<usize, String> {
        if fitted == 0 || fitted >= self.rows() {
            return Err(format!(
                "{fitted} fitted windows of {} leave none to fit or none to judge",
                self.rows()
            ));
        }
        let reach = self.label_end_timestamps[..fitted].iter().copied().max().unwrap_or(i64::MIN);
        let past = self
            .timestamps
            .iter()
            .rposition(|&time| time <= reach)
            .map_or(0, |window| window + 1)
            .max(fitted);
        let first = past.saturating_add(embargo);
        if first >= self.rows() {
            return Err(format!(
                "the fitted labels read the book up to {reach} ms, and of the {} windows none after \
                 that (and an embargo of {embargo} more) is left to judge",
                self.rows()
            ));
        }
        Ok(first)
    }
}

/// Each row of `data` from the `steps`-th on, as the window of its last `steps` rows (it included):
/// DeepLOB's input, each window SCALED BY ITSELF, so nothing outside the window (and nothing later
/// than its last row) enters it.
///
/// - A price becomes basis points from the window's LAST mid, `(price / mid - 1) * 10_000`: the book
///   is read relative to the price at decision time, whatever the level of the market.
/// - A quantity becomes a multiple of the window's mean quantity (every level, both sides, every
///   step), so a busy and a quiet hour feed the network the same scale.
/// - The label, whether there is one, the time and the label end are the last row's.
///
/// DeepLOB itself scales by the previous five days' mean and deviation (Zhang, Zohren and Roberts,
/// 2019, section IV-A), which a few minutes of recording do not have. A window with a missing level
/// (a NaN price) or no quantity at all is dropped and counted, never filled.
///
/// shortcut: a window is `steps` consecutive ROWS, so one can straddle a row the export dropped (a
/// break in the chain); pass the windows of one unbroken recording, or add the anchor to
/// [`BookTrainingData`] when a recording with holes needs windows.
pub fn book_windows(data: &BookTrainingData, steps: usize) -> Result<BookWindows, String> {
    if steps == 0 {
        return Err("a window needs at least one step".to_string());
    }
    let columns = data.columns();
    let mut windows = BookWindows { steps, columns, ..BookWindows::default() };
    for last in steps - 1..data.rows() {
        let rows = &data.features[(last + 1 - steps) * columns..(last + 1) * columns];
        // Columns alternate price and quantity (ask price, ask quantity, bid price, bid quantity).
        let mean_quantity = vike_math::statistics::mean(rows.iter().skip(1).step_by(2).copied());
        // A NaN anywhere (a missing level's price) is caught first, so the mean is a number here.
        if rows.iter().any(|value| value.is_nan()) || mean_quantity <= 0.0 {
            windows.skipped_incomplete += 1;
            continue;
        }
        let mid = data.mids[last];
        windows.features.extend(rows.iter().enumerate().map(|(column, &value)| {
            if column % 2 == 0 { (value / mid - 1.0) * BASIS_POINTS } else { value / mean_quantity }
        }));
        windows.labels.push(data.labels[last]);
        windows.timestamps.push(data.timestamps[last]);
        windows.label_end_timestamps.push(data.label_end_timestamps[last]);
        windows.labelled.push(data.labelled[last]);
    }
    Ok(windows)
}

/// Basis points per unit of relative price.
const BASIS_POINTS: f64 = 10_000.0;

/// The relative move `r` of [`BookLabel`] for one candidate, or NaN when it has no honest label: a
/// mid it needs is past the input, on another anchor (anchors only grow, so the two ends sharing one
/// means every event between them does) or one-sided (a NaN mid).
fn label_return(
    parameters: &BookExportParameters,
    mids: &[f64],
    anchors: &[usize],
    candidate: &Candidate,
) -> f64 {
    let (i, k) = (candidate.event, parameters.horizon_events);
    let later = i.saturating_add(k);
    if anchors.get(later) != Some(&anchors[i]) {
        return f64::NAN;
    }
    let mean = |window: &[f64]| vike_math::statistics::mean(window.iter().copied());
    match parameters.label {
        BookLabel::MidAfter => (mids[later] - candidate.mid) / candidate.mid,
        BookLabel::MeanOfNext => (mean(&mids[i + 1..=later]) - candidate.mid) / candidate.mid,
        BookLabel::MeanOfNextAgainstMeanOfLast => match (i + 1).checked_sub(k) {
            Some(first) if anchors[first] == anchors[i] => {
                let last = mean(&mids[first..=i]);
                (mean(&mids[i + 1..=later]) - last) / last
            }
            _ => f64::NAN,
        },
    }
}

fn validate(parameters: &BookExportParameters) -> Result<(), String> {
    if parameters.levels == 0 {
        return Err("levels must be at least 1".to_string());
    }
    if parameters.horizon_events == 0 {
        return Err("horizon_events must be at least 1".to_string());
    }
    if let BookSampling::Interval { milliseconds } = parameters.sampling
        && milliseconds <= 0
    {
        return Err(format!("the sampling interval must be positive, got {milliseconds} ms"));
    }
    for (name, value) in
        [("up_threshold", parameters.up_threshold), ("down_threshold", parameters.down_threshold)]
    {
        if !(value.is_finite() && value >= 0.0) {
            return Err(format!("{name} must be finite and not negative, got {value}"));
        }
    }
    Ok(())
}

/// The first multiple of `milliseconds` at or after `timestamp`.
fn first_boundary(timestamp: i64, milliseconds: i64) -> i64 {
    let floor = timestamp.div_euclid(milliseconds) * milliseconds;
    if floor < timestamp { floor + milliseconds } else { floor }
}

/// Read one candidate row off the book; `false` when there is no two-sided book to read.
fn sample(
    slot: &Option<L2Book>,
    timestamp: i64,
    event: usize,
    levels: usize,
    candidates: &mut Vec<Candidate>,
    features: &mut Vec<f64>,
) -> bool {
    let Some((book, mid)) = slot.as_ref().and_then(|book| Some((book, book.mid()?))) else {
        return false;
    };
    let (bids, asks) = book.top_n(levels);
    for level in 0..levels {
        for side in [&asks, &bids] {
            match side.get(level) {
                Some(found) => features.extend([found.price, found.qty]),
                None => features.extend([f64::NAN, 0.0]),
            }
        }
    }
    candidates.push(Candidate { timestamp, event, mid });
    true
}

#[path = "book_export_tests.rs"]
#[cfg(test)]
mod book_export_tests;
