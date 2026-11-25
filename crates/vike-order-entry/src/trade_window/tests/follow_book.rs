//! How the ladder follows the book: the rows hold while the price stays in view, the automatic
//! grouping, the trader's grouping kept per instrument, and the CI box's recorded streams replayed.

use super::*;

/// **The ladder holds its rows while the price is on screen** (the owner, 2026-11-16: "chart redraws all
/// time"). A sequence of depth snapshots whose mid wanders up to 8 rows either way in a 21-row view
/// leaves the centre where it was, so every level the books keep stays on the same row; only the
/// mid leaving the rows re-centres, once, on it. (The rule before re-centred past a quarter of the
/// rows, 5.25 here, and moved every row on the 6- and 8-row steps below.)
#[test]
fn the_ladder_holds_its_rows_while_the_price_stays_in_view() {
    use vike_marketdata::L2BookLevel;
    const ROWS: usize = 21;
    // A snapshot around `mid`: the touch a tenth either side, and two levels that never move.
    let book = |mid: f64| {
        let mut b = L2Book::new(0.1);
        let bids = [L2BookLevel::new(mid - 0.05, 1.0), L2BookLevel::new(64_990.0, 2.0)];
        let asks = [L2BookLevel::new(mid + 0.05, 1.0), L2BookLevel::new(65_010.0, 3.0)];
        b.apply_snapshot(1, &bids, &asks);
        b
    };
    let (tick, group) = (0.1, 1);
    let row_of = |state: &TradeWindowState, price: f64| {
        let top =
            ladder::rows::row_key(state.anchor.expect("anchored"), tick, group) + (ROWS / 2) as i64;
        top - ladder::rows::row_key(price, tick, group)
    };
    // The re-centre rule alone: a grouping the trader chose stays put (`auto_group` is its own test).
    let mut state = TradeWindowState { group_chosen: true, ..TradeWindowState::default() };
    let first = book(65_000.05);
    follow_book(&mut state, &inputs(&first, None, None), ROWS);
    let centre = state.anchor;
    let (kept_bid, kept_ask) = (row_of(&state, 64_990.0), row_of(&state, 65_010.0));
    for (step, mid) in
        [65_000.35, 65_000.65, 65_000.85, 64_999.45, 64_999.25, 65_000.05].into_iter().enumerate()
    {
        let b = book(mid);
        follow_book(&mut state, &inputs(&b, None, None), ROWS);
        assert_eq!(state.anchor, centre, "step {step}: the centre moved for a mid still on screen");
        assert_eq!(
            row_of(&state, 64_990.0),
            kept_bid,
            "step {step}: a level that stayed changed rows"
        );
        assert_eq!(row_of(&state, 65_010.0), kept_ask, "step {step}");
    }
    // The mid leaves the rows: one re-centre, on it.
    let far = book(65_002.05);
    follow_book(&mut state, &inputs(&far, None, None), ROWS);
    let moved = state.anchor.expect("anchored");
    assert!((moved - 65_002.05).abs() < 1e-9, "re-centred on the mid: {moved}");
    let again = book(65_002.35);
    follow_book(&mut state, &inputs(&again, None, None), ROWS);
    assert_eq!(state.anchor, Some(moved), "and holds there");
}

/// The automatic grouping's arithmetic: the smallest of 1, 2, 5, 10, 20, 50, … ticks a row at
/// which the rows cover ten typical steps. The the CI box numbers: bybit (step 3.1, 0.1 tick, 21 rows)
/// gets 20, a 42 span; binance (step 1.65, 0.01 tick) gets 100, a 21 span.
#[test]
fn the_automatic_grouping_covers_ten_typical_steps() {
    assert_eq!(auto_group(3.1, 21, 0.1), 20);
    assert_eq!(auto_group(1.65, 21, 0.01), 100);
    assert_eq!(auto_group(3.1, 30, 0.1), 20);
    assert_eq!(auto_group(0.05, 21, 0.1), 1, "a calm market keeps one tick a row");
    assert_eq!(auto_group(0.0, 21, 0.1), 1);
}

/// The trader's Group wins: once he presses it, no step of the price moves it, and it is the one
/// the window comes back to on that instrument; an instrument he never grouped is picked afresh.
#[test]
fn the_traders_grouping_wins_and_is_kept_per_instrument() {
    use vike_marketdata::L2BookLevel;
    let book = |mid: f64| {
        let mut b = L2Book::new(0.1);
        b.apply_snapshot(
            1,
            &[L2BookLevel::new(mid - 0.05, 1.0)],
            &[L2BookLevel::new(mid + 0.05, 1.0)],
        );
        b
    };
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    let start = book(65_000.05);
    s.sync(&inputs(&start, None, None), &mut acts);
    s.group = ladder::rows::next_group(s.group); // the press
    s.group_chosen = true;
    for step in 0..12 {
        let b = book(65_000.05 + 3.0 * f64::from(step));
        follow_book(&mut s, &inputs(&b, None, None), 21);
    }
    assert_eq!(s.group, 2, "no step of the price moves the trader's grouping");
    let eth = TradeWindowInputs { symbol: "ETHUSDT", ..inputs(&start, None, None) };
    s.sync(&eth, &mut acts);
    assert_eq!(
        (s.group, s.group_chosen),
        (1, false),
        "an instrument he never grouped starts afresh"
    );
    s.sync(&inputs(&start, None, None), &mut acts);
    assert_eq!((s.group, s.group_chosen), (2, true), "and his grouping is back on BTCUSDT");
}

/// One recorded stream (`replay/*.txt`): its tick, and the mid of every frame.
fn recorded(text: &str) -> (f64, f64, Vec<f64>) {
    let mut lines = text.lines().filter(|l| !l.starts_with('#'));
    let head: Vec<f64> =
        lines.next().expect("a head").split(' ').map(|w| w.parse().expect("a number")).collect();
    let mids = lines.next().expect("mids").split(' ').map(|w| w.parse().expect("a mid")).collect();
    (head[0], head[1], mids)
}

/// Whole-ladder shifts while `mids` play through [`follow_book`]: every frame whose centre or
/// grouping is not the frame before's moves every row. `chosen` pins the grouping as a press does.
/// Split as (while the first [`AUTO_GROUP_STEPS`] moves are measured, after).
fn shifts(mids: &[f64], tick: f64, rows: usize, chosen: Option<i64>) -> (usize, usize) {
    use vike_marketdata::L2BookLevel;
    let mut state = TradeWindowState::default();
    if let Some(g) = chosen {
        (state.group, state.group_chosen) = (g, true);
    }
    let mut shifts = (0, 0);
    let mut drawn = None;
    for &mid in mids {
        let measuring = state.steps.len() < AUTO_GROUP_STEPS;
        let mut b = L2Book::new(tick);
        b.apply_snapshot(
            1,
            &[L2BookLevel::new(mid - tick / 2.0, 1.0)],
            &[L2BookLevel::new(mid + tick / 2.0, 1.0)],
        );
        let on = TradeWindowInputs {
            grid: Grid { tick_size: Some(tick), ..BTC },
            ..inputs(&b, None, None)
        };
        follow_book(&mut state, &on, rows);
        let now = (state.anchor.map(f64::to_bits), state.group);
        let moved = usize::from(drawn.is_some_and(|d| d != now));
        if measuring {
            shifts.0 += moved
        } else {
            shifts.1 += moved
        }
        drawn = Some(now);
    }
    shifts
}

/// The rule before 2026-11-16 at one tick a row: re-centre past a quarter of the rows.
fn shifts_before(mids: &[f64], tick: f64, rows: usize) -> usize {
    let band = (rows as f64 / 4.0).max(2.0) * tick;
    let (mut anchor, mut shifts) = (None::<f64>, 0);
    for &mid in mids {
        anchor = match anchor {
            Some(a) if (mid - a).abs() <= band => Some(a),
            Some(_) => {
                shifts += 1;
                Some(mid)
            }
            None => Some(mid),
        };
    }
    shifts
}

/// **the CI box's own 60 s of bybit and binance BTCUSDT depth, replayed** (the owner, 2026-11-16: the
/// ladder "redraws all time"). With the automatic grouping the ladder shifts a few times a minute at
/// most once its first moves are measured, and those moves (the window's first second or so) shift
/// it at most once each; at one tick a row, before and after the re-centre rule, it shifted about
/// every two seconds.
#[test]
fn the_recorded_streams_shift_the_ladder_a_few_times_a_minute_at_most() {
    for (name, text) in [
        ("bybit", include_str!("../replay/bybit_btcusdt_60s.txt")),
        ("binance", include_str!("../replay/binance_btcusdt_60s.txt")),
    ] {
        let (tick, seconds, mids) = recorded(text);
        let per_minute = |n: usize| n as f64 * 60.0 / seconds;
        for rows in [21, 30] {
            let before = per_minute(shifts_before(&mids, tick, rows));
            let (warm, after) = shifts(&mids, tick, rows, Some(1));
            let one_tick = per_minute(warm + after);
            let (measuring, after) = shifts(&mids, tick, rows, None);
            let auto = per_minute(after);
            eprintln!(
                "{name} {rows} rows: before {before:.1}/min, re-centre rule at one tick {one_tick:.1}/min, \
                 automatic grouping {auto:.1}/min after {measuring} shifts while its first moves were measured"
            );
            assert!(
                measuring <= AUTO_GROUP_STEPS,
                "{name} {rows} rows: {measuring} shifts while measuring"
            );
            assert!(auto <= 5.0, "{name} {rows} rows: {auto:.1} shifts a minute");
        }
    }
}
