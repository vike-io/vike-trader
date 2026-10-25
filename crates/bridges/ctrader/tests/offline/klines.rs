//! The candle-history source (`vike_ctrader::klines::CtraderKlines`) over a SCRIPTED wire: no socket, no
//! clock, no sleep. Every trap the module doc names has a test here that fails when the trap is walked
//! into: the delta arithmetic and the fixed price scale, the absent-field readings, the forming bar, the
//! window clamps, the tiling of a wide window, the venue's `hasMore`, the pace and the retries, the
//! interval and symbol refusals. The live socket (the handshake, the framing, the real pace gate) is
//! `tests/history_wire.rs`, against the fake server.
//!
//! The fixtures are built by hand from the proto definitions (`ProtoOATrendbar`: `low` plus unsigned
//! deltas, a stamp in minutes) with the numbers of `tests/common`'s seed bars (EURUSD low 1.139, open
//! +5, close +12, high +20 in 1/100000) and the live-verified USDJPY scale (16233600 is 162.336).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_ctrader::event_mapper::{
    trendbar_period_for_interval, trendbar_period_ms, trendbar_to_bar,
};
use vike_ctrader::klines::{
    CtraderKlines, HistoryConfigError, HistorySession, HistoryWire, MAX_REQUESTS_PER_FETCH,
    MAX_REQUESTS_PER_SECOND, MAX_WINDOW_BARS, SERVED_INTERVALS, Window, WireError, window_request,
};
use vike_ctrader::proto::{
    ProtoOaGetTrendbarsRes, ProtoOaLightSymbol, ProtoOaSymbol, ProtoOaTrendbar,
    ProtoOaTrendbarPeriod,
};
use vike_ctrader::symbols::SymbolMap;
use vike_data::source::{KlineSource, SourceError};
use vike_marketdata::Bar;
use vike_marketdata::interval::interval_ms;

const MIN: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// Far after every stamp used below (2001-09-09) and before the venue's latest `to`.
const NOW: i64 = 1_000_000_000_000;
const EURUSD: i64 = 1;
const USDJPY: i64 = 4;

// ------------------------------------------------------------------------------------------------
// the scripted wire
// ------------------------------------------------------------------------------------------------

type Answer = Box<dyn FnMut(&Window) -> Result<ProtoOaGetTrendbarsRes, WireError> + Send>;

/// What the wire saw, for the assertions, and the clock and the open script a test steers it with.
#[derive(Default)]
struct Log {
    /// Sessions opened (an `open` that succeeded).
    opens: usize,
    /// Every `open` call, a refused one included.
    open_attempts: usize,
    paces: usize,
    sleeps: Vec<Duration>,
    asked: Vec<Window>,
    /// The wire's clock, epoch ms; a test moves it with `Rig::set_now`.
    now: i64,
    /// The wire's monotonic clock, ms from an arbitrary start; a test moves it with `Rig::set_mono`. The
    /// idle age is read off this one, never off the wall clock.
    mono: i64,
    /// What the next `open` calls do, front first: `Some(error)` refuses that call, `None` (or an empty
    /// queue) opens a session.
    open_script: VecDeque<Option<WireError>>,
}

struct Script {
    log: Arc<Mutex<Log>>,
    answer: Arc<Mutex<Answer>>,
    no_grant: Option<HistoryConfigError>,
}

struct ScriptSession {
    log: Arc<Mutex<Log>>,
    answer: Arc<Mutex<Answer>>,
    symbols: SymbolMap,
}

impl HistoryWire for Script {
    fn open(&mut self) -> Result<Box<dyn HistorySession>, WireError> {
        if let Some(why) = self.no_grant {
            return Err(WireError::NotConfigured(why));
        }
        {
            let mut log = self.log.lock().expect("log lock");
            log.open_attempts += 1;
            if let Some(Some(refusal)) = log.open_script.pop_front() {
                return Err(refusal);
            }
            log.opens += 1;
        }
        Ok(Box::new(ScriptSession {
            log: self.log.clone(),
            answer: self.answer.clone(),
            symbols: symbols(),
        }))
    }

    fn pace(&mut self) {
        self.log.lock().expect("log lock").paces += 1;
    }

    fn sleep(&mut self, wait: Duration) {
        self.log.lock().expect("log lock").sleeps.push(wait);
    }

    fn now_ms(&self) -> i64 {
        self.log.lock().expect("log lock").now
    }

    fn monotonic_ms(&self) -> i64 {
        self.log.lock().expect("log lock").mono
    }
}

impl HistorySession for ScriptSession {
    fn symbols(&self) -> &SymbolMap {
        &self.symbols
    }

    fn ask(&mut self, window: &Window) -> Result<ProtoOaGetTrendbarsRes, WireError> {
        self.log.lock().expect("log lock").asked.push(*window);
        let mut answer = self.answer.lock().expect("answer lock");
        (*answer)(window)
    }
}

struct Rig {
    source: CtraderKlines,
    log: Arc<Mutex<Log>>,
}

impl Rig {
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        from: i64,
        to: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        self.source.fetch(symbol, interval, from, to)
    }

    fn opens(&self) -> usize {
        self.log.lock().expect("log lock").opens
    }

    fn open_attempts(&self) -> usize {
        self.log.lock().expect("log lock").open_attempts
    }

    /// Move the wire's clock to `ms` (epoch ms).
    fn set_now(&self, ms: i64) {
        self.log.lock().expect("log lock").now = ms;
    }

    /// Move the wire's monotonic clock to `ms`.
    fn set_mono(&self, ms: i64) {
        self.log.lock().expect("log lock").mono = ms;
    }

    /// Script the next `open` calls, front first (see `Log::open_script`).
    fn script_opens(&self, script: Vec<Option<WireError>>) {
        self.log.lock().expect("log lock").open_script = script.into();
    }

    fn paces(&self) -> usize {
        self.log.lock().expect("log lock").paces
    }

    fn sleeps(&self) -> Vec<Duration> {
        self.log.lock().expect("log lock").sleeps.clone()
    }

    fn asked(&self) -> Vec<Window> {
        self.log.lock().expect("log lock").asked.clone()
    }
}

fn rig(
    now: i64,
    answer: impl FnMut(&Window) -> Result<ProtoOaGetTrendbarsRes, WireError> + Send + 'static,
) -> Rig {
    rig_with(now, None, Box::new(answer))
}

fn rig_without_grant(why: HistoryConfigError) -> Rig {
    rig_with(NOW, Some(why), Box::new(|_| unreachable!("no request may be sent without a grant")))
}

fn rig_with(now: i64, no_grant: Option<HistoryConfigError>, answer: Answer) -> Rig {
    let log = Arc::new(Mutex::new(Log { now, ..Log::default() }));
    let script = Script { log: log.clone(), answer: Arc::new(Mutex::new(answer)), no_grant };
    Rig { source: CtraderKlines::with_wire(Box::new(script)), log }
}

// ------------------------------------------------------------------------------------------------
// fixtures
// ------------------------------------------------------------------------------------------------

/// The account's symbols: EURUSD (five digits) and USDJPY (three).
fn symbols() -> SymbolMap {
    let listed = [(EURUSD, "EURUSD", 5), (USDJPY, "USDJPY", 3)];
    let light: Vec<ProtoOaLightSymbol> = listed
        .iter()
        .map(|&(id, name, _)| ProtoOaLightSymbol {
            symbol_id: id,
            symbol_name: Some(name.to_string()),
            ..Default::default()
        })
        .collect();
    let full: Vec<ProtoOaSymbol> = listed
        .iter()
        .map(|&(id, _, digits)| ProtoOaSymbol { symbol_id: id, digits, ..Default::default() })
        .collect();
    SymbolMap::from_symbols(&light, &full)
}

/// A complete trendbar: `deltas` are (open, close, high) from the low.
fn trendbar(minutes: u32, low: i64, deltas: (u64, u64, u64), volume: i64) -> ProtoOaTrendbar {
    ProtoOaTrendbar {
        volume,
        low: Some(low),
        delta_open: Some(deltas.0),
        delta_close: Some(deltas.1),
        delta_high: Some(deltas.2),
        utc_timestamp_in_minutes: Some(minutes),
        ..Default::default()
    }
}

/// The seed bars' EURUSD numbers at `minutes`.
fn plain(minutes: u32) -> ProtoOaTrendbar {
    trendbar(minutes, 113_900, (5, 12, 20), 10)
}

/// An answer to `window` holding `bars`, in the order given.
fn answer(window: &Window, bars: Vec<ProtoOaTrendbar>, has_more: bool) -> ProtoOaGetTrendbarsRes {
    ProtoOaGetTrendbarsRes {
        ctid_trader_account_id: 99,
        period: window.period as i32,
        trendbar: bars,
        symbol_id: Some(window.symbol_id),
        has_more: Some(has_more),
        ..Default::default()
    }
}

#[derive(Clone, Copy)]
enum Keeps {
    Newest,
    Oldest,
}

/// A venue holding a one-minute bar at each of `minutes`: it answers a window with the bars inside,
/// cut to `cap` entries (keeping the `keeps` end) and flagged `hasMore` when it cut and `flag` is set.
fn venue(
    minutes: Vec<u32>,
    cap: usize,
    keeps: Keeps,
    flag: bool,
) -> impl FnMut(&Window) -> Result<ProtoOaGetTrendbarsRes, WireError> + Send + 'static {
    move |w: &Window| {
        let mut inside: Vec<u32> = minutes
            .iter()
            .copied()
            .filter(|m| (w.from_ms..=w.to_ms).contains(&(i64::from(*m) * MIN)))
            .collect();
        inside.sort_unstable();
        let cut = inside.len() > cap;
        if cut {
            match keeps {
                Keeps::Newest => inside = inside.split_off(inside.len() - cap),
                Keeps::Oldest => inside.truncate(cap),
            }
        }
        Ok(answer(w, inside.into_iter().map(plain).collect(), cut && flag))
    }
}

fn empty_venue() -> impl FnMut(&Window) -> Result<ProtoOaGetTrendbarsRes, WireError> + Send + 'static
{
    |w: &Window| Ok(answer(w, Vec::new(), false))
}

/// The stamps of `bars` in minutes.
fn stamps(bars: &[Bar]) -> Vec<u32> {
    bars.iter().map(|b| u32::try_from(b.ts / MIN).expect("a small stamp")).collect()
}

fn fetch_text(rig: &Rig, symbol: &str, interval: &str, from: i64, to: i64) -> String {
    rig.fetch(symbol, interval, from, to).expect_err("the fetch should fail").to_string()
}

// ------------------------------------------------------------------------------------------------
// decoding: the delta arithmetic, the fixed scale, the absent-field readings
// ------------------------------------------------------------------------------------------------

#[test]
fn the_store_partition_is_ctrader() {
    let rig = rig(NOW, empty_venue());
    assert_eq!(rig.source.venue(), "ctrader");
}

#[test]
fn deltas_are_added_to_the_low_and_scaled_by_one_hundred_thousand() {
    let rig = rig(NOW, |w| Ok(answer(w, vec![trendbar(1000, 113_900, (5, 12, 20), 42)], false)));
    let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, 1000 * MIN).expect("fetch");
    assert_eq!(bars.len(), 1);
    let b = &bars[0];
    // low 1.139; open = low + 5 units, close = low + 12, high = low + 20 (each unit is 1/100000).
    assert_eq!(
        (b.ts, b.open, b.high, b.low, b.close, b.volume),
        (60_000_000, 1.13905, 1.1392, 1.139, 1.13912, 42.0)
    );
}

#[test]
fn a_three_digit_symbol_is_scaled_by_the_same_fixed_hundred_thousand() {
    // USDJPY has digits=3, and its prices are STILL in 1/100000: a divisor of 10^digits would read
    // 16233600 as 16233.6.
    let rig =
        rig(NOW, |w| Ok(answer(w, vec![trendbar(2000, 16_233_600, (10, 40, 100), 7)], false)));
    let bars = rig.fetch("USDJPY", "1m", 2000 * MIN, 2000 * MIN).expect("fetch");
    let b = &bars[0];
    assert_eq!((b.low, b.open, b.close, b.high), (162.336, 162.3361, 162.3364, 162.337));
}

#[test]
fn the_history_decode_equals_the_live_seed_decode_on_complete_bars() {
    let fixtures = [
        trendbar(1000, 113_900, (5, 12, 20), 42),
        trendbar(1001, 16_233_600, (10, 40, 100), 7),
        trendbar(1002, -3_000, (0, 50, 50), 1),
        trendbar(1003, 113_950, (30, 3, 30), 17),
    ];
    let served = fixtures.to_vec();
    let rig = rig(NOW, move |w| Ok(answer(w, served.clone(), false)));
    let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, 1003 * MIN).expect("fetch");
    let seed: Vec<Bar> =
        fixtures.iter().map(|tb| trendbar_to_bar(tb).expect("complete trendbar")).collect();
    assert_eq!(bars, seed);
}

#[test]
fn volume_is_the_tick_count_and_the_row_carries_no_bid_or_ask() {
    let rig = rig(NOW, |w| Ok(answer(w, vec![trendbar(1000, 113_900, (5, 12, 20), 4321)], false)));
    let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, 1000 * MIN).expect("fetch");
    let b = &bars[0];
    assert_eq!(b.volume, 4321.0);
    assert!(b.bid.is_none() && b.ask.is_none() && b.funding.is_none() && b.symbol.is_none());
}

#[test]
fn a_quiet_minute_with_absent_deltas_is_a_flat_bar_not_a_dropped_one() {
    // The wire fields are optional and a sender may leave a zero unwritten.
    let quiet = ProtoOaTrendbar {
        volume: 3,
        low: Some(113_900),
        utc_timestamp_in_minutes: Some(1000),
        ..Default::default()
    };
    let opens_at_its_low = ProtoOaTrendbar {
        volume: 5,
        low: Some(113_900),
        delta_close: Some(7),
        delta_high: Some(9),
        utc_timestamp_in_minutes: Some(1001),
        ..Default::default()
    };
    let served = vec![quiet, opens_at_its_low];
    let rig = rig(NOW, move |w| Ok(answer(w, served.clone(), false)));
    let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, 1001 * MIN).expect("fetch");
    assert_eq!(stamps(&bars), vec![1000, 1001]);
    assert_eq!(
        (bars[0].open, bars[0].high, bars[0].low, bars[0].close),
        (1.139, 1.139, 1.139, 1.139)
    );
    assert_eq!(
        (bars[1].open, bars[1].high, bars[1].low, bars[1].close),
        (1.139, 1.13909, 1.139, 1.13907)
    );
}

#[test]
fn a_trendbar_without_its_low_or_its_stamp_fails_the_window_instead_of_being_skipped() {
    let no_low =
        ProtoOaTrendbar { volume: 1, utc_timestamp_in_minutes: Some(1000), ..Default::default() };
    let no_stamp = ProtoOaTrendbar { volume: 1, low: Some(113_900), ..Default::default() };
    for broken in [no_low, no_stamp] {
        let rig = rig(NOW, move |w| Ok(answer(w, vec![broken, plain(1001)], false)));
        let text = fetch_text(&rig, "EURUSD", "1m", 1000 * MIN, 1001 * MIN);
        assert!(text.contains("no low price or no stamp"), "{text}");
    }
}

#[test]
fn a_bar_that_opens_or_closes_above_its_high_fails_the_window() {
    // Deltas are measured from the low, so open above high means they were read wrongly.
    for bad in [trendbar(1000, 113_900, (30, 12, 20), 1), trendbar(1000, 113_900, (5, 30, 20), 1)] {
        let rig = rig(NOW, move |w| Ok(answer(w, vec![bad], false)));
        let text = fetch_text(&rig, "EURUSD", "1m", 1000 * MIN, 1000 * MIN);
        assert!(text.contains("above its high"), "{text}");
    }
}

#[test]
fn extreme_wire_values_never_panic() {
    for low in [i64::MAX, i64::MIN, 0] {
        let wild = trendbar(1000, low, (u64::MAX, u64::MAX, u64::MAX), i64::MAX);
        let rig = rig(NOW, move |w| Ok(answer(w, vec![wild], false)));
        let _ = rig.fetch("EURUSD", "1m", 1000 * MIN, 1000 * MIN);
    }
}

#[test]
fn the_stamp_is_the_open_minute_in_milliseconds() {
    // 16,000,000 minutes is 9.6e11 ms: past what a 32-bit millisecond product holds.
    let rig = rig(NOW, |w| Ok(answer(w, vec![plain(16_000_000)], false)));
    let bars = rig.fetch("EURUSD", "1m", 16_000_000 * MIN, 16_000_000 * MIN).expect("fetch");
    assert_eq!(bars[0].ts, 16_000_000 * MIN);
}

#[test]
fn an_empty_answer_is_an_empty_result_not_an_error() {
    let rig = rig(NOW, empty_venue());
    assert!(rig.fetch("EURUSD", "1m", 1000 * MIN, 2000 * MIN).expect("fetch").is_empty());
}

// ------------------------------------------------------------------------------------------------
// the window: clamps, the forming bar, strays, tiling
// ------------------------------------------------------------------------------------------------

#[test]
fn the_bar_still_forming_is_dropped() {
    let now = 1002 * MIN + 30_000; // halfway through the bar opened at minute 1002
    let rig = rig(now, venue(vec![1000, 1001, 1002], 100, Keeps::Newest, false));
    let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, i64::MAX).expect("fetch");
    assert_eq!(stamps(&bars), vec![1000, 1001]);
}

#[test]
fn a_bar_that_closed_exactly_now_is_kept() {
    let now = 1002 * MIN; // bar 1001 closes at this instant, bar 1002 opens at it
    let rig = rig(now, venue(vec![1000, 1001, 1002], 100, Keeps::Newest, false));
    let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, i64::MAX).expect("fetch");
    assert_eq!(stamps(&bars), vec![1000, 1001]);
}

#[test]
fn a_window_ending_in_the_future_is_asked_only_up_to_now() {
    let rig = rig(NOW, empty_venue());
    rig.fetch("EURUSD", "1m", NOW - 10 * MIN, NOW + 1_000_000_000).expect("fetch");
    let asked = rig.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!((asked[0].from_ms, asked[0].to_ms), (NOW - 10 * MIN, NOW));
}

#[test]
fn a_window_wholly_in_the_future_asks_nothing_and_opens_no_session() {
    let rig = rig(NOW, empty_venue());
    let bars = rig.fetch("EURUSD", "1m", NOW + MIN, NOW + 2 * MIN).expect("fetch");
    assert!(bars.is_empty());
    assert_eq!((rig.opens(), rig.asked().len()), (0, 0));
}

#[test]
fn an_inverted_window_asks_nothing_and_opens_no_session() {
    let rig = rig(NOW, empty_venue());
    assert!(rig.fetch("EURUSD", "1m", 10 * MIN, 5 * MIN).expect("fetch").is_empty());
    assert_eq!((rig.opens(), rig.asked().len()), (0, 0));
}

#[test]
fn a_start_before_the_epoch_is_asked_from_zero() {
    let rig = rig(NOW, empty_venue());
    rig.fetch("EURUSD", "1m", -5_000_000, 10 * MIN).expect("fetch");
    assert_eq!(rig.asked()[0].from_ms, 0);
}

#[test]
fn the_venues_latest_to_is_respected() {
    let rig = rig(3_000_000_000_000, empty_venue());
    rig.fetch("EURUSD", "1m", 2_147_000_000_000, 3_000_000_000_000).expect("fetch");
    assert_eq!(rig.asked()[0].to_ms, 2_147_483_646_000);
}

#[test]
fn bars_outside_the_window_are_dropped_and_the_result_is_ascending_and_unique() {
    // Newest first, minute 1002 twice, and two strays the venue should not have sent.
    let rig = rig(NOW, |w| {
        let bars =
            vec![plain(1003), plain(1001), plain(1002), plain(1002), plain(900), plain(2000)];
        Ok(answer(w, bars, false))
    });
    let bars = rig.fetch("EURUSD", "1m", 1001 * MIN, 1003 * MIN).expect("fetch");
    assert_eq!(stamps(&bars), vec![1001, 1002, 1003]);
}

#[test]
fn a_wide_window_is_asked_in_pieces_that_tile_it() {
    let rig = rig(NOW, empty_venue());
    let (from, to) = (1000 * MIN, 26_000 * MIN - 1); // 25,000 one-minute bars: seventeen days and a bit
    rig.fetch("EURUSD", "1m", from, to).expect("fetch");
    let mut asked = rig.asked();
    asked.sort_by_key(|w| w.from_ms);
    assert_eq!(asked.len(), 18, "a piece of minute bars is at most one day");
    assert_eq!((asked[0].from_ms, asked[17].to_ms), (from, to));
    for pair in asked.windows(2) {
        assert_eq!(pair[1].from_ms, pair[0].to_ms + 1, "the pieces leave no gap and no overlap");
    }
    for piece in &asked {
        assert!(piece.to_ms - piece.from_ms < DAY, "{piece:?}");
        assert!(piece.to_ms - piece.from_ms < MAX_WINDOW_BARS * MIN, "{piece:?}");
    }
    assert_eq!(rig.paces(), 18);
}

/// The vendor limits the distance between the two bounds per period and publishes no numbers, so the
/// source never asks more than a day of minute bars or a week of anything coarser, whatever the bar count
/// would allow: a caller that bypasses the ingest's chunking is exposed otherwise.
#[test]
fn a_piece_is_a_day_below_the_hour_and_a_week_from_the_hour_up() {
    for (interval, piece_ms, pieces) in [
        ("1m", DAY, 30),
        ("30m", DAY, 30),
        ("1h", 7 * DAY, 5),
        ("4h", 7 * DAY, 5),
        ("12h", 7 * DAY, 5),
        ("1d", 7 * DAY, 5),
    ] {
        let rig = rig(NOW, empty_venue());
        rig.fetch("EURUSD", interval, 0, 30 * DAY - 1).expect("fetch"); // thirty days
        let mut asked = rig.asked();
        asked.sort_by_key(|w| w.from_ms);
        assert_eq!(asked.len(), pieces, "{interval}");
        for piece in &asked {
            assert!(piece.to_ms - piece.from_ms < piece_ms, "{interval}: {piece:?}");
        }
        assert_eq!((asked[0].from_ms, asked[pieces - 1].to_ms), (0, 30 * DAY - 1), "{interval}");
    }
}

#[test]
fn a_year_of_daily_bars_is_fifty_three_requests_of_a_week() {
    let rig = rig(NOW, empty_venue());
    rig.fetch("EURUSD", "1d", 0, 365 * DAY - 1).expect("fetch");
    assert_eq!(rig.asked().len(), 53);
}

#[test]
fn the_request_carries_both_bounds_and_no_count() {
    let window =
        Window { symbol_id: USDJPY, period: ProtoOaTrendbarPeriod::H1, from_ms: 10, to_ms: 20 };
    let req = window_request(77, &window);
    assert_eq!(
        (
            req.ctid_trader_account_id,
            req.from_timestamp,
            req.to_timestamp,
            req.symbol_id,
            req.count
        ),
        (77, Some(9), Some(20), USDJPY, None)
    );
    assert_eq!(req.period, ProtoOaTrendbarPeriod::H1 as i32);
}

/// Whether the venue's `fromTimestamp` is inclusive is NOT MEASURED, and a chunk starts on a whole minute:
/// were the bound exclusive, the first bar of every stored chunk would be missing and its key spent. The
/// request asks one millisecond below (no bar is stamped there: stamps are whole minutes) and the
/// client-side window filter keeps what was wanted.
#[test]
fn the_request_asks_one_millisecond_below_the_first_wanted_stamp() {
    let window = Window {
        symbol_id: EURUSD,
        period: ProtoOaTrendbarPeriod::M1,
        from_ms: 1000 * MIN,
        to_ms: 1005 * MIN,
    };
    let req = window_request(77, &window);
    assert_eq!((req.from_timestamp, req.to_timestamp), (Some(1000 * MIN - 1), Some(1005 * MIN)));
    let from_the_epoch = Window { from_ms: 0, ..window };
    assert_eq!(
        window_request(77, &from_the_epoch).from_timestamp,
        Some(0),
        "never below the epoch"
    );
}

// ------------------------------------------------------------------------------------------------
// the venue's `hasMore`
// ------------------------------------------------------------------------------------------------

#[test]
fn a_cut_answer_is_completed_below_and_above_whichever_end_the_venue_kept() {
    for keeps in [Keeps::Newest, Keeps::Oldest] {
        let minutes: Vec<u32> = (1000..1120).collect(); // 120 bars, the venue returns 50 a request
        let rig = rig(NOW, venue(minutes.clone(), 50, keeps, true));
        let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, 1119 * MIN).expect("fetch");
        assert_eq!(stamps(&bars), minutes);
        assert_eq!(rig.asked().len(), 4, "two cut answers, each followed by its remainders");
    }
}

#[test]
fn a_flag_that_says_more_without_a_bar_inside_the_window_is_an_error_not_a_loop() {
    let rig = rig(NOW, |w| Ok(answer(w, Vec::new(), true)));
    let text = fetch_text(&rig, "EURUSD", "1m", 1000 * MIN, 2000 * MIN);
    assert!(text.contains("more bars follow but sent none"), "{text}");
    assert_eq!(rig.asked().len(), 1);
}

#[test]
fn a_venue_that_never_finishes_is_stopped_at_the_request_cap() {
    // Each answer is flagged "more" and holds the newest whole minute of its window; a remainder that
    // holds no whole minute answers empty.
    let rig = rig(NOW, |w| {
        let top = w.to_ms / MIN;
        if top * MIN >= w.from_ms {
            Ok(answer(w, vec![plain(u32::try_from(top).expect("small"))], true))
        } else {
            Ok(answer(w, Vec::new(), false))
        }
    });
    let text = fetch_text(&rig, "EURUSD", "1m", 1000 * MIN, 1400 * MIN - 1);
    assert!(text.contains(&format!("more than {MAX_REQUESTS_PER_FETCH} requests")), "{text}");
    assert_eq!(rig.asked().len(), MAX_REQUESTS_PER_FETCH);
}

#[test]
fn an_answer_for_another_period_or_symbol_is_refused() {
    let other_period = rig(NOW, |w| {
        let mut res = answer(w, vec![plain(1000)], false);
        res.period = ProtoOaTrendbarPeriod::M5 as i32;
        Ok(res)
    });
    let text = fetch_text(&other_period, "EURUSD", "1m", 1000 * MIN, 1000 * MIN);
    assert!(text.contains("another symbol or period"), "{text}");

    let other_symbol = rig(NOW, |w| {
        let mut res = answer(w, vec![plain(1000)], false);
        res.symbol_id = Some(999);
        Ok(res)
    });
    let text = fetch_text(&other_symbol, "EURUSD", "1m", 1000 * MIN, 1000 * MIN);
    assert!(text.contains("another symbol or period"), "{text}");
}

#[test]
fn an_answer_that_names_no_symbol_is_accepted() {
    let rig = rig(NOW, |w| {
        let mut res = answer(w, vec![plain(1000)], false);
        res.symbol_id = None;
        Ok(res)
    });
    assert_eq!(rig.fetch("EURUSD", "1m", 1000 * MIN, 1000 * MIN).expect("fetch").len(), 1);
}

// ------------------------------------------------------------------------------------------------
// the vocabulary: intervals and symbols
// ------------------------------------------------------------------------------------------------

#[test]
fn an_interval_the_venue_does_not_serve_is_refused_before_a_session_opens() {
    for interval in ["5s", "7m", "funding", "", "1h30m"] {
        let rig = rig(NOW, empty_venue());
        let err = rig.fetch("EURUSD", interval, 0, MIN).expect_err("refused");
        assert!(matches!(err, SourceError::Refused(_)), "{interval:?}: {err}");
        assert_eq!(rig.opens(), 0, "{interval:?}");
    }
}

#[test]
fn weeks_and_months_are_refused_as_calendar_width() {
    for interval in ["1w", "1M"] {
        let rig = rig(NOW, empty_venue());
        let err = rig.fetch("EURUSD", interval, 0, MIN).expect_err("refused");
        assert!(matches!(&err, SourceError::Refused(t) if t.contains("calendar-width")), "{err}");
        assert_eq!(rig.opens(), 0);
    }
}

#[test]
fn each_served_interval_asks_its_own_period_whose_width_is_the_intervals() {
    let mut periods = Vec::new();
    for interval in SERVED_INTERVALS {
        let period = trendbar_period_for_interval(interval).expect("a period");
        assert_eq!(
            trendbar_period_ms(period),
            interval_ms(interval).expect("a width"),
            "{interval}"
        );
        let rig = rig(NOW, empty_venue());
        rig.fetch("EURUSD", interval, 0, 10 * 86_400_000).expect("fetch");
        assert_eq!(rig.asked()[0].period, period, "{interval}");
        periods.push(period as i32);
    }
    periods.sort_unstable();
    periods.dedup();
    assert_eq!(periods.len(), SERVED_INTERVALS.len());
}

#[test]
fn an_unknown_symbol_is_refused_after_the_handshake_with_no_trendbar_request() {
    let rig = rig(NOW, |_| unreachable!("no request may be sent for an unknown symbol"));
    let err = rig.fetch("GBPJPY", "1m", 0, MIN).expect_err("refused");
    assert!(
        matches!(&err, SourceError::Refused(t) if t.contains("not a symbol this account lists"))
    );
    assert_eq!((rig.opens(), rig.asked().len()), (1, 0));
}

#[test]
fn names_are_the_brokers_and_a_wrong_case_is_refused_with_the_listed_spelling() {
    let rig = rig(NOW, |_| unreachable!("no request may be sent for a wrong-case symbol"));
    let err = rig.fetch("eurusd", "1m", 0, MIN).expect_err("refused");
    assert!(matches!(&err, SourceError::Refused(t) if t.contains("EURUSD")), "{err}");
}

#[test]
fn the_request_carries_the_listed_symbol_id() {
    let rig = rig(NOW, empty_venue());
    rig.fetch("USDJPY", "1m", 0, MIN).expect("fetch");
    assert_eq!(rig.asked()[0].symbol_id, USDJPY);
}

// ------------------------------------------------------------------------------------------------
// the session, the pace and the retries
// ------------------------------------------------------------------------------------------------

#[test]
fn one_session_serves_every_fetch_of_the_source() {
    let rig = rig(NOW, empty_venue());
    rig.fetch("EURUSD", "1m", 0, MIN).expect("fetch");
    rig.fetch("USDJPY", "1h", 0, MIN).expect("fetch");
    let _ = rig.fetch("GBPJPY", "1m", 0, MIN); // a refusal keeps the session too
    rig.fetch("EURUSD", "1d", 0, MIN).expect("fetch");
    assert_eq!(rig.opens(), 1);
}

#[test]
fn a_dropped_session_is_reopened_once_and_the_window_asked_again() {
    let mut first = true;
    let rig = rig(NOW, move |w| {
        if std::mem::take(&mut first) {
            Err(WireError::Dropped("connection closed".to_string()))
        } else {
            Ok(answer(w, vec![plain(1000)], false))
        }
    });
    let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, 1000 * MIN).expect("fetch");
    assert_eq!(stamps(&bars), vec![1000]);
    let asked = rig.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0], asked[1]);
    assert_eq!(rig.opens(), 2);
    assert_eq!(rig.paces(), 2, "a retry takes a pace turn like any request");
}

#[test]
fn two_drops_in_a_row_fail_the_fetch() {
    let rig = rig(NOW, |_| Err(WireError::Dropped("connection closed".to_string())));
    let text = fetch_text(&rig, "EURUSD", "1m", 1000 * MIN, 1000 * MIN);
    assert!(text.contains("dropped twice in a row"), "{text}");
    assert_eq!((rig.opens(), rig.asked().len()), (2, 2));
}

#[test]
fn each_request_has_its_own_reopen() {
    // Calls 1 and 3 drop, in two different pieces of the window: each request has its own one reopen,
    // so the fetch completes.
    let mut calls = 0;
    let rig = rig(NOW, move |w| {
        calls += 1;
        if calls == 1 || calls == 3 {
            Err(WireError::Dropped("connection closed".to_string()))
        } else {
            Ok(answer(w, Vec::new(), false))
        }
    });
    rig.fetch("EURUSD", "1m", 1000 * MIN, 26_000 * MIN - 1).expect("fetch");
    assert_eq!((rig.asked().len(), rig.opens()), (20, 3)); // eighteen pieces, two of them asked twice
}

#[test]
fn a_missing_grant_is_refused_in_words_and_asks_nothing() {
    let rig = rig_without_grant(HistoryConfigError::NotConfigured);
    let err = rig.fetch("EURUSD", "1m", 0, MIN).expect_err("refused");
    assert!(
        matches!(&err, SourceError::Refused(t) if t.contains("CLIENT_ID") && t.contains("demo")),
        "{err}"
    );
    assert!(rig.asked().is_empty());
}

#[test]
fn an_unreadable_store_is_not_reported_as_an_absent_grant() {
    let rig = rig_without_grant(HistoryConfigError::StoreUnreadable);
    let err = rig.fetch("EURUSD", "1m", 0, MIN).expect_err("refused");
    assert!(
        matches!(&err, SourceError::Refused(t) if t.contains("could not be read") && !t.contains("CLIENT_ID")),
        "{err}"
    );
}

#[test]
fn the_process_pace_stays_under_the_vendors_five_a_second() {
    assert_eq!(MAX_REQUESTS_PER_SECOND, 4);
    const { assert!(MAX_REQUESTS_PER_SECOND < 5) };
}

fn venue_error(code: &str, description: &str, retry_after_s: Option<u64>) -> WireError {
    WireError::Venue { code: code.to_string(), description: description.to_string(), retry_after_s }
}

#[test]
fn a_rate_limit_refusal_waits_and_asks_again_on_the_same_session() {
    let mut first = true;
    let rig = rig(NOW, move |w| {
        if std::mem::take(&mut first) {
            Err(venue_error("REQUEST_FREQUENCY_EXCEEDED", "slow down", None))
        } else {
            Ok(answer(w, vec![plain(1000)], false))
        }
    });
    let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, 1000 * MIN).expect("fetch");
    assert_eq!(stamps(&bars), vec![1000]);
    assert_eq!(rig.sleeps(), vec![Duration::from_secs(2)]);
    assert_eq!((rig.paces(), rig.opens()), (2, 1));
}

#[test]
fn a_stated_retry_after_is_waited_out_up_to_a_cap() {
    for (stated, waited) in [(7, 7), (3_600, 60), (1, 2)] {
        let mut first = true;
        let rig = rig(NOW, move |w| {
            if std::mem::take(&mut first) {
                Err(venue_error("BLOCKED_PAYLOAD_TYPE", "rate limit", Some(stated)))
            } else {
                Ok(answer(w, Vec::new(), false))
            }
        });
        rig.fetch("EURUSD", "1m", 0, MIN).expect("fetch");
        assert_eq!(rig.sleeps(), vec![Duration::from_secs(waited)], "retryAfter {stated}");
    }
}

#[test]
fn a_venue_that_keeps_refusing_fails_after_three_waits() {
    let rig = rig(NOW, |_| Err(venue_error("REQUEST_FREQUENCY_EXCEEDED", "slow down", None)));
    let text = fetch_text(&rig, "EURUSD", "1m", 0, MIN);
    assert!(text.contains("REQUEST_FREQUENCY_EXCEEDED"), "{text}");
    assert_eq!((rig.sleeps().len(), rig.asked().len()), (3, 4));
}

#[test]
fn any_other_venue_error_fails_at_once_with_the_venues_words() {
    let rig = rig(NOW, |_| Err(venue_error("INVALID_REQUEST", "the window is too wide", None)));
    let text = fetch_text(&rig, "EURUSD", "1m", 0, MIN);
    assert!(text.contains("INVALID_REQUEST") && text.contains("the window is too wide"), "{text}");
    assert_eq!((rig.sleeps().len(), rig.asked().len()), (0, 1));
}

#[test]
fn a_refused_grant_names_the_remedy() {
    let rig =
        rig(NOW, |_| Err(venue_error("CH_ACCESS_TOKEN_INVALID", "Access token expired", None)));
    let text = fetch_text(&rig, "EURUSD", "1m", 0, MIN);
    assert!(text.contains("ctrader_authorize"), "{text}");
}

// ------------------------------------------------------------------------------------------------
// an idle session, and a token that a sibling's refresh has just replaced
// ------------------------------------------------------------------------------------------------

/// cTrader disconnects an idle socket (the exec lane heartbeats every ten seconds for that reason) and this
/// session sends no heartbeat, so a session unused for longer than that is opened again BEFORE the next
/// ask: the alternative is a dead socket found by a failed request, and a reply wait spent on it.
#[test]
fn a_session_idle_longer_than_the_heartbeat_age_is_opened_again_before_the_next_ask() {
    let rig = rig(NOW, empty_venue());
    rig.fetch("EURUSD", "1m", 0, MIN).expect("fetch");
    rig.set_mono(10_001);
    rig.fetch("USDJPY", "1m", 0, MIN).expect("fetch");
    assert_eq!(rig.opens(), 2, "the idle session was replaced");
    assert_eq!(rig.asked().len(), 2, "and no request was spent finding out");
}

#[test]
fn a_session_used_within_the_heartbeat_age_is_kept() {
    let rig = rig(NOW, empty_venue());
    rig.fetch("EURUSD", "1m", 0, MIN).expect("fetch");
    rig.set_mono(10_000); // exactly the age: not longer than it
    rig.fetch("USDJPY", "1m", 0, MIN).expect("fetch");
    assert_eq!(rig.opens(), 1);
}

#[test]
fn the_idle_age_runs_from_the_last_answer_and_not_from_the_first_open() {
    let rig = rig(NOW, empty_venue());
    rig.fetch("EURUSD", "1m", 0, MIN).expect("fetch");
    for step in 1..=4 {
        rig.set_mono(step * 8_000); // each gap is under the age, the whole span is not
        rig.fetch("EURUSD", "1m", 0, MIN).expect("fetch");
    }
    assert_eq!(rig.opens(), 1);
}

/// The idle age is a DURATION, so it is read off the monotonic clock: a wall clock stepped back by an
/// operator or a time sync would otherwise keep a session the venue closed long ago, and one stepped
/// forward would replace a session in use.
#[test]
fn the_idle_age_ignores_the_wall_clock_in_both_directions() {
    let stale = rig(NOW, empty_venue());
    stale.fetch("EURUSD", "1m", 0, MIN).expect("fetch");
    stale.set_mono(11_000);
    stale.set_now(NOW - 3_600_000); // the wall clock stepped back an hour
    stale.fetch("USDJPY", "1m", 0, MIN).expect("fetch");
    assert_eq!(
        stale.opens(),
        2,
        "a backward wall step must not keep a session idle for eleven seconds"
    );

    let busy = rig(NOW, empty_venue());
    busy.fetch("EURUSD", "1m", 0, MIN).expect("fetch");
    busy.set_mono(2_000);
    busy.set_now(NOW + 3_600_000); // the wall clock stepped forward an hour
    busy.fetch("USDJPY", "1m", 0, MIN).expect("fetch");
    assert_eq!(
        busy.opens(),
        1,
        "a forward wall step must not replace a session used two seconds ago"
    );
}

fn token_refusal() -> WireError {
    venue_error("CH_ACCESS_TOKEN_INVALID", "Access token is not valid", None)
}

/// A sibling socket's refresh invalidates this session and writes the new pair a moment AFTER the venue
/// dropped us, so an immediate reopen can present the old pair: a token-class refusal at an open waits a
/// second (the sibling's write lands) and opens once more, which presents the grant as it is by then.
#[test]
fn a_token_refusal_at_reopen_waits_a_second_and_opens_once_more() {
    let mut first = true;
    let rig = rig(NOW, move |w| {
        if std::mem::take(&mut first) {
            Err(WireError::Dropped("the account's token was invalidated".to_string()))
        } else {
            Ok(answer(w, vec![plain(1000)], false))
        }
    });
    // The first open works, the reopen after the drop is refused, the retry works.
    rig.script_opens(vec![None, Some(token_refusal())]);
    let bars = rig.fetch("EURUSD", "1m", 1000 * MIN, 1000 * MIN).expect("fetch");
    assert_eq!(stamps(&bars), vec![1000]);
    assert_eq!(rig.sleeps(), vec![Duration::from_secs(1)]);
    assert_eq!((rig.open_attempts(), rig.opens()), (3, 2));
}

#[test]
fn a_token_refusal_that_repeats_fails_with_the_remedy_after_one_wait() {
    let rig = rig(NOW, empty_venue());
    rig.script_opens(vec![Some(token_refusal()), Some(token_refusal())]);
    let text = fetch_text(&rig, "EURUSD", "1m", 0, MIN);
    assert!(text.contains("ctrader_authorize"), "{text}");
    assert_eq!(rig.sleeps(), vec![Duration::from_secs(1)]);
    assert_eq!((rig.open_attempts(), rig.opens(), rig.asked().len()), (2, 0, 0));
}

#[test]
fn a_refusal_of_an_open_that_is_not_about_the_token_is_not_retried() {
    let rig = rig(NOW, empty_venue());
    rig.script_opens(vec![Some(venue_error("CONNECTIONS_LIMIT_EXCEEDED", "too many", None))]);
    let text = fetch_text(&rig, "EURUSD", "1m", 0, MIN);
    assert!(text.contains("CONNECTIONS_LIMIT_EXCEEDED"), "{text}");
    assert_eq!((rig.sleeps().len(), rig.open_attempts()), (0, 1));
}

// ------------------------------------------------------------------------------------------------
// the stamp grid
// ------------------------------------------------------------------------------------------------

/// A minute, five-minute, thirty-minute or hourly bar opens on a multiple of its width in UTC. A stamp off
/// that grid means the venue's convention is not the one this source assumes (a shifted stamp, an
/// open/close mix-up), so the chunk fails rather than be stored under a key nothing repairs.
#[test]
fn a_stamp_off_the_grid_of_a_period_up_to_an_hour_fails_the_chunk() {
    for (interval, off_grid_minute) in
        [("3m", 1001u32), ("5m", 1001), ("15m", 1010), ("30m", 1010), ("1h", 1010)]
    {
        let rig = rig(NOW, move |w| Ok(answer(w, vec![plain(off_grid_minute)], false)));
        let text = fetch_text(&rig, "EURUSD", interval, 1000 * MIN, 1100 * MIN);
        assert!(text.contains("grid"), "{interval}: {text}");
    }
}

#[test]
fn stamps_on_the_grid_pass_and_the_longer_periods_are_not_held_to_it() {
    let on_the_grid = rig(NOW, |w| Ok(answer(w, vec![plain(1000), plain(1005)], false)));
    let bars = on_the_grid.fetch("EURUSD", "5m", 1000 * MIN, 1100 * MIN).expect("fetch");
    assert_eq!(stamps(&bars), vec![1000, 1005]);
    // Where a four-hour, twelve-hour or daily bar begins is the broker's session (NOT MEASURED): no rule.
    for interval in ["4h", "12h", "1d"] {
        let rig = rig(NOW, |w| Ok(answer(w, vec![plain(1001)], false)));
        let bars = rig.fetch("EURUSD", interval, 1000 * MIN, 1100 * MIN).expect("fetch");
        assert_eq!(stamps(&bars), vec![1001], "{interval}");
    }
}
