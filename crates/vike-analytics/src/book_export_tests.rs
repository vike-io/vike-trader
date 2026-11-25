//! `book_export`: hand-computed books on a whole-number grid (tick 1.0, so every mid is exact), the
//! refusals, and the point-in-time property (a shorter input keeps every row that survives it).

use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind};

use super::{
    BookExportParameters, BookLabel, BookSampling, BookTrainingData, book_windows,
    export_book_rows_keeping_unlabelled, export_book_training_data,
};

fn event(
    kind: L2BookUpdateKind,
    timestamp: i64,
    sequence: u64,
    bids: &[(f64, f64)],
    asks: &[(f64, f64)],
) -> L2BookUpdate {
    let levels = |side: &[(f64, f64)]| {
        side.iter().map(|&(price, qty)| L2BookLevel::new(price, qty)).collect()
    };
    L2BookUpdate {
        timestamp,
        local_timestamp: 0,
        sequence,
        kind,
        tick_size: 1.0,
        bids: levels(bids),
        asks: levels(asks),
        symbol: String::new(),
    }
}

fn snapshot(
    timestamp: i64,
    sequence: u64,
    bids: &[(f64, f64)],
    asks: &[(f64, f64)],
) -> L2BookUpdate {
    event(L2BookUpdateKind::Snapshot, timestamp, sequence, bids, asks)
}

fn delta(timestamp: i64, sequence: u64, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> L2BookUpdate {
    event(L2BookUpdateKind::Delta, timestamp, sequence, bids, asks)
}

fn parameters(
    sampling: BookSampling,
    levels: usize,
    horizon_events: usize,
) -> BookExportParameters {
    BookExportParameters {
        levels,
        sampling,
        horizon_events,
        label: BookLabel::default(),
        up_threshold: 0.001,
        down_threshold: 0.001,
    }
}

#[test]
fn the_default_label_is_the_plain_mid_after_k_events() {
    assert_eq!(BookLabel::default(), BookLabel::MidAfter);
}

/// Mids 100, 100.5, 101, 100.5 and `k = 2`: the next two average 100.75 from the first row and
/// from the second; DeepLOB's own form also averages the last two (100.25 at the second row) and
/// has no row at the first (one mid before it is missing).
#[test]
fn the_smoothed_labels_average_the_next_k_mids() {
    let smoothed = |label| {
        let p = BookExportParameters { label, ..parameters(BookSampling::EveryEvent, 1, 2) };
        export_book_training_data(&four_events(), &p).unwrap()
    };
    let next = smoothed(BookLabel::MeanOfNext);
    assert_eq!(next.timestamps, [1000, 1001]);
    assert_eq!(next.horizon_returns, [0.75 / 100.0, 0.25 / 100.5]);
    assert_eq!(next.labels, [1, 1]);
    assert_eq!(next.skipped_without_label, 2);

    let deeplob = smoothed(BookLabel::MeanOfNextAgainstMeanOfLast);
    assert_eq!(deeplob.timestamps, [1001]);
    assert_eq!(deeplob.horizon_returns, [0.5 / 100.25]);
    assert_eq!(deeplob.labels, [1]);
    assert_eq!(deeplob.skipped_without_label, 3);
}

/// Bits, so a NaN feature (a missing level) compares as itself.
fn bits(values: &[f64]) -> Vec<u64> {
    values.iter().map(|v| v.to_bits()).collect()
}

/// Mids 100 -> 100.5 -> 101 -> 100.5, one event each.
fn four_events() -> Vec<L2BookUpdate> {
    vec![
        snapshot(1000, 1, &[(99.0, 1.0), (98.0, 2.0)], &[(101.0, 3.0), (102.0, 4.0)]),
        delta(1001, 2, &[], &[(101.0, 0.0)]),
        delta(1002, 3, &[(100.0, 5.0)], &[]),
        delta(1003, 4, &[], &[(101.0, 1.0)]),
    ]
}

#[test]
fn every_event_rows_carry_the_top_levels_and_the_mid_move_k_events_later() {
    let data =
        export_book_training_data(&four_events(), &parameters(BookSampling::EveryEvent, 2, 1))
            .unwrap();
    assert_eq!(data.timestamps, [1000, 1001, 1002]);
    assert_eq!(data.mids, [100.0, 100.5, 101.0]);
    assert_eq!(data.labels, [1, 1, -1]);
    assert_eq!(data.horizon_returns, [0.5 / 100.0, 0.5 / 100.5, -0.5 / 101.0]);
    assert_eq!((data.skipped_without_book, data.skipped_without_label), (0, 1));
    assert_eq!(data.columns(), 8);
    assert_eq!(data.features.len(), data.rows() * data.columns());
    // Row 0 is the snapshot; row 1 lost its best ask, so level 2's ask is MISSING: NaN, zero.
    let (rows, _) = data.features.as_chunks::<8>();
    assert_eq!(bits(&rows[0]), bits(&[101.0, 3.0, 99.0, 1.0, 102.0, 4.0, 98.0, 2.0]));
    assert_eq!(bits(&rows[1]), bits(&[102.0, 4.0, 99.0, 1.0, f64::NAN, 0.0, 98.0, 2.0]));

    // Two events ahead: 100 -> 101 is up, 100.5 -> 100.5 is flat; the last two have no horizon.
    let data =
        export_book_training_data(&four_events(), &parameters(BookSampling::EveryEvent, 1, 2))
            .unwrap();
    assert_eq!(data.labels, [1, 0]);
    assert_eq!(data.skipped_without_label, 2);
}

#[test]
fn the_thresholds_cut_the_flat_band() {
    let wide = BookExportParameters {
        up_threshold: 0.005,
        down_threshold: 0.0049,
        ..parameters(BookSampling::EveryEvent, 1, 1)
    };
    // +0.005 is not ABOVE 0.005 (flat), +0.004975 is flat, -0.004950 is below -0.0049 (down).
    let data = export_book_training_data(&four_events(), &wide).unwrap();
    assert_eq!(data.labels, [0, 0, -1]);
}

#[test]
fn an_interval_row_sees_every_event_at_or_before_its_boundary_and_nothing_later() {
    let updates = vec![
        snapshot(500, 1, &[(99.0, 1.0)], &[(101.0, 1.0), (102.0, 1.0)]), // mid 100
        delta(1000, 2, &[], &[(101.0, 0.0)]),                            // mid 100.5, ON a boundary
        delta(2500, 3, &[(100.0, 1.0)], &[]),                            // mid 101
        delta(2600, 4, &[], &[(101.0, 1.0)]),                            // mid 100.5
        delta(4000, 5, &[(100.0, 0.0)], &[]),                            // mid 100
    ];
    let data = export_book_training_data(
        &updates,
        &parameters(BookSampling::Interval { milliseconds: 1000 }, 1, 1),
    )
    .unwrap();
    // 1000 and 2000 both see the book after the event stamped 1000; 3000 sees the one after 2600.
    // 4000 comes after the last event and is not sampled.
    assert_eq!(data.timestamps, [1000, 2000, 3000]);
    assert_eq!(data.mids, [100.5, 100.5, 100.5]);
    assert_eq!(data.labels, [1, 1, -1]);
    assert_eq!((data.skipped_without_book, data.skipped_without_label), (0, 0));
}

/// Mids 100, 100.5 | a sequence hole (3 is missing), an orphan delta | a re-anchoring snapshot at
/// 100, then 100.5.
fn chain_with_a_gap() -> Vec<L2BookUpdate> {
    vec![
        snapshot(1, 1, &[(99.0, 1.0)], &[(101.0, 1.0), (102.0, 1.0)]),
        delta(2, 2, &[], &[(101.0, 0.0)]),
        delta(3, 4, &[(100.0, 1.0)], &[]),
        delta(4, 5, &[(100.0, 2.0)], &[]),
        snapshot(5, 10, &[(99.0, 1.0)], &[(101.0, 1.0), (102.0, 1.0)]),
        delta(6, 11, &[], &[(101.0, 0.0)]),
    ]
}

#[test]
fn no_label_crosses_a_break_in_the_chain() {
    let data =
        export_book_training_data(&chain_with_a_gap(), &parameters(BookSampling::EveryEvent, 1, 1))
            .unwrap();
    // 100 -> 100.5 before the hole and after it; the row before the hole would reach across it.
    assert_eq!(data.timestamps, [1, 5]);
    assert_eq!(data.labels, [1, 1]);
    assert_eq!((data.skipped_without_book, data.skipped_without_label), (0, 2));
}

#[test]
fn no_interval_row_is_read_off_a_broken_book() {
    let data = export_book_training_data(
        &chain_with_a_gap(),
        &parameters(BookSampling::Interval { milliseconds: 1 }, 1, 1),
    )
    .unwrap();
    // Boundaries 3 and 4 fall while the book is missing; boundary 2's horizon is across the hole.
    assert_eq!(data.timestamps, [1, 5]);
    assert_eq!((data.skipped_without_book, data.skipped_without_label), (2, 1));
}

#[test]
fn a_conflating_lane_of_snapshots_is_one_unbroken_chain() {
    let updates = vec![
        snapshot(100, 7, &[(99.0, 1.0)], &[(101.0, 1.0)]),
        snapshot(200, 9, &[(99.0, 1.0)], &[(103.0, 1.0)]),
        snapshot(300, 12, &[(101.0, 1.0)], &[(103.0, 1.0)]),
    ];
    let data =
        export_book_training_data(&updates, &parameters(BookSampling::EveryEvent, 1, 2)).unwrap();
    assert_eq!(data.timestamps, [100]);
    assert_eq!(data.horizon_returns, [0.02]);
}

#[test]
fn the_column_names_follow_the_row_order() {
    let data = BookTrainingData { levels: 2, ..BookTrainingData::default() };
    assert_eq!(
        data.column_names(),
        [
            "ask_price_1",
            "ask_quantity_1",
            "bid_price_1",
            "bid_quantity_1",
            "ask_price_2",
            "ask_quantity_2",
            "bid_price_2",
            "bid_quantity_2",
        ]
    );
}

#[test]
fn parameters_it_cannot_honour_are_refused_with_the_reason() {
    let good = parameters(BookSampling::EveryEvent, 1, 1);
    let refused =
        |p: BookExportParameters| export_book_training_data(&four_events(), &p).unwrap_err();
    assert!(refused(BookExportParameters { levels: 0, ..good }).contains("levels"));
    assert!(refused(BookExportParameters { horizon_events: 0, ..good }).contains("horizon_events"));
    let interval = BookSampling::Interval { milliseconds: 0 };
    assert!(refused(BookExportParameters { sampling: interval, ..good }).contains("interval"));
    for bad in [-0.1, f64::NAN, f64::INFINITY] {
        assert!(
            refused(BookExportParameters { up_threshold: bad, ..good }).contains("up_threshold")
        );
        assert!(
            refused(BookExportParameters { down_threshold: bad, ..good })
                .contains("down_threshold")
        );
    }
}

/// A seeded chain of 300 events on a three-level book with two holes, each re-anchored by a
/// snapshot two events later. A plain linear congruential stream, so the test needs no `rand`.
fn long_chain() -> Vec<L2BookUpdate> {
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = |modulo: u64| {
        state =
            state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % modulo
    };
    let anchor = |timestamp: i64, sequence: u64| {
        snapshot(
            timestamp,
            sequence,
            &[(99.0, 1.0), (98.0, 1.0), (97.0, 1.0)],
            &[(101.0, 1.0), (102.0, 1.0), (103.0, 1.0)],
        )
    };
    let mut updates = vec![anchor(0, 1)];
    let mut sequence = 1;
    for i in 1..300i64 {
        // Several events share a millisecond, as a busy feed's do.
        let timestamp = i / 3;
        if i == 100 || i == 200 {
            sequence += 2; // a hole
        } else if i == 102 || i == 202 {
            sequence += 1;
            updates.push(anchor(timestamp, sequence));
            continue;
        } else {
            sequence += 1;
        }
        let price = 95.0 + next(4) as f64 + if next(2) == 0 { 0.0 } else { 6.0 };
        let level = vec![(price, next(3) as f64)];
        let (bids, asks) = if price < 100.0 { (level, Vec::new()) } else { (Vec::new(), level) };
        updates.push(delta(timestamp, sequence, &bids, &asks));
    }
    updates
}

/// What a row is, all of it: a shorter input must reproduce it exactly.
fn rows(data: &BookTrainingData) -> Vec<(i64, Vec<u64>, u64, i8)> {
    let columns = data.columns();
    (0..data.rows())
        .map(|r| {
            (
                data.timestamps[r],
                bits(&data.features[r * columns..(r + 1) * columns]),
                data.horizon_returns[r].to_bits(),
                data.labels[r],
            )
        })
        .collect()
}

#[test]
fn a_shorter_input_keeps_every_surviving_row() {
    let updates = long_chain();
    // 300 events over 100 ms: about 290 event rows and 90 one-millisecond rows.
    let labels =
        [BookLabel::MidAfter, BookLabel::MeanOfNext, BookLabel::MeanOfNextAgainstMeanOfLast];
    for sampling in [BookSampling::EveryEvent, BookSampling::Interval { milliseconds: 1 }] {
        for label in labels {
            let parameters = BookExportParameters { label, ..parameters(sampling, 3, 5) };
            let full = rows(&export_book_training_data(&updates, &parameters).unwrap());
            assert!(full.len() > 40, "{sampling:?} {label:?}: too few rows, {}", full.len());
            for cut in 0..updates.len() {
                let shorter =
                    rows(&export_book_training_data(&updates[..cut], &parameters).unwrap());
                assert_eq!(
                    shorter[..],
                    full[..shorter.len()],
                    "{sampling:?} {label:?}: cut at {cut}, a row changed or a later row came first"
                );
            }
        }
    }
}

/// Three one-level rows by hand: each window is scaled by itself, its last mid and its mean quantity.
#[test]
fn a_window_is_its_rows_scaled_by_its_own_last_mid_and_mean_quantity() {
    let data = BookTrainingData {
        levels: 1,
        timestamps: vec![10, 20, 30],
        // ask price, ask quantity, bid price, bid quantity
        features: vec![101.0, 2.0, 99.0, 2.0, 102.0, 1.0, 100.0, 3.0, 101.0, 4.0, f64::NAN, 0.0],
        mids: vec![100.0, 101.0, 100.0],
        horizon_returns: vec![0.0; 3],
        labels: vec![1, -1, 0],
        label_end_timestamps: vec![15, 25, 35],
        labelled: vec![true; 3],
        ..BookTrainingData::default()
    };
    let windows = book_windows(&data, 2).unwrap();
    // Window 1 = rows 0..=1: mid 101, mean quantity (2 + 2 + 1 + 3) / 4 = 2. Row 2 misses its bid.
    let want = [
        (101.0 / 101.0 - 1.0) * 1e4,
        1.0,
        (99.0 / 101.0 - 1.0) * 1e4,
        1.0,
        (102.0 / 101.0 - 1.0) * 1e4,
        0.5,
        (100.0 / 101.0 - 1.0) * 1e4,
        1.5,
    ];
    assert_eq!(bits(&windows.features), bits(&want));
    assert_eq!((windows.steps, windows.columns, windows.rows()), (2, 4, 1));
    assert_eq!((windows.labels.as_slice(), windows.timestamps.as_slice()), (&[-1][..], &[20][..]));
    assert_eq!(windows.label_end_timestamps, [25]);
    assert_eq!(windows.skipped_incomplete, 1, "the window ending on the one-sided row");
    assert_eq!(book_windows(&data, 4).unwrap().rows(), 0, "no row has 4 rows behind it");
    assert!(book_windows(&data, 0).is_err());
}

/// A QUIET book (lookahead R2-2): one event every 3 s, one row a second, the label 2 events ahead,
/// so each label reaches 3 to 6 s past its row. A fixed skip of 2 windows lands inside the reach of
/// the last fitted label; the judged windows start after it instead.
#[test]
fn judging_starts_after_the_last_fitted_label_ends_however_quiet_the_book() {
    let mut updates = vec![snapshot(0, 1, &[(99.0, 1.0)], &[(101.0, 1.0)])];
    for event in 1..=10u64 {
        updates.push(delta(3000 * event as i64, event + 1, &[(99.0, event as f64)], &[]));
    }
    let data = export_book_training_data(
        &updates,
        &parameters(BookSampling::Interval { milliseconds: 1000 }, 1, 2),
    )
    .unwrap();
    // A row at boundary `b` reads event `b / 3000`; its label ends at event `b / 3000 + 2`.
    let boundaries: Vec<i64> = (0..27).map(|second| second * 1000).collect();
    assert_eq!(data.timestamps, boundaries);
    let ends: Vec<i64> = boundaries.iter().map(|b| 3000 * (b / 3000 + 2)).collect();
    assert_eq!(data.label_end_timestamps, ends);
    assert_eq!(data.skipped_without_label, 3, "boundaries 27 to 29 s: event 11 never comes");

    let windows = book_windows(&data, 1).unwrap();
    assert_eq!(windows.label_end_timestamps, ends);
    let fitted = 10;
    // Window 9 (9 s) reads event 3, so the fitted labels reach event 5, at 15 s.
    let reach = *windows.label_end_timestamps[..fitted].iter().max().unwrap();
    assert_eq!(reach, 15_000);
    let fixed_gap = 2;
    assert!(windows.timestamps[fitted + fixed_gap] <= reach, "the fixed count leaks");
    let first = windows.first_judged_window(fitted, 0).unwrap();
    assert_eq!((first, windows.timestamps[first]), (16, 16_000));
    assert!(windows.timestamps[first..].iter().all(|&time| time > reach));
    assert_eq!(
        windows.first_judged_window(fitted, fixed_gap),
        Ok(18),
        "the gap is an embargo on top"
    );

    // Fit through 24 s and the labels reach event 10 (30 s), past the last window: refused.
    assert!(windows.first_judged_window(25, 0).unwrap_err().contains("30000 ms"));
    assert!(windows.first_judged_window(0, 0).is_err());
    assert!(windows.first_judged_window(windows.rows(), 0).is_err());
}

/// Mids 100, 100.5, ONE-SIDED, 101, 100.5 (lookahead R2-3), one event ahead: the row just before
/// the ask side empties and the last row (the input ends) have no label. The training export drops
/// them; the keeping export holds every row the book at its own time allows, those two marked
/// unlabelled, so a set judged on it is not cut on what happened after each row.
#[test]
fn a_row_whose_label_cannot_be_formed_is_kept_unlabelled_not_dropped() {
    let updates = vec![
        snapshot(1000, 1, &[(99.0, 1.0)], &[(101.0, 1.0)]),
        delta(1001, 2, &[(100.0, 1.0)], &[]),
        delta(1002, 3, &[], &[(101.0, 0.0)]),
        delta(1003, 4, &[], &[(102.0, 1.0)]),
        delta(1004, 5, &[(100.0, 0.0)], &[]),
    ];
    let p = parameters(BookSampling::EveryEvent, 1, 1);
    let dropped = export_book_training_data(&updates, &p).unwrap();
    assert_eq!(dropped.timestamps, [1000, 1003]);
    assert_eq!(dropped.labelled, [true, true]);
    assert_eq!((dropped.skipped_without_book, dropped.skipped_without_label), (1, 2));

    let kept = export_book_rows_keeping_unlabelled(&updates, &p).unwrap();
    assert_eq!(kept.timestamps, [1000, 1001, 1003, 1004]);
    assert_eq!(kept.labelled, [true, false, true, false]);
    assert_eq!(kept.labels, [1, 0, -1, 0]);
    assert_eq!((kept.skipped_without_book, kept.skipped_without_label), (1, 2));
    let returns = &kept.horizon_returns;
    assert_eq!((returns[0], returns[2]), (0.5 / 100.0, -0.5 / 101.0));
    assert!(returns[1].is_nan() && returns[3].is_nan());
    // Event `i + 1`, or the last event where the input ends first.
    assert_eq!(kept.label_end_timestamps, [1001, 1002, 1004, 1004]);
    // Ask, ask quantity, bid, bid quantity: each row is the book at its own time.
    let books = [101.0, 1.0, 99.0, 1.0, 101.0, 1.0, 100.0, 1.0];
    let later = [102.0, 1.0, 100.0, 1.0, 102.0, 1.0, 99.0, 1.0];
    assert_eq!(kept.features, [books, later].concat());
    // Its labelled rows are the training export's, to the bit.
    let labelled: Vec<_> = rows(&kept)
        .into_iter()
        .zip(&kept.labelled)
        .filter(|(_, has_label)| **has_label)
        .map(|(row, _)| row)
        .collect();
    assert_eq!(labelled, rows(&dropped));

    let windows = book_windows(&kept, 2).unwrap();
    assert_eq!(windows.timestamps, [1001, 1003, 1004]);
    assert_eq!(windows.labelled, [false, true, false]);
}
