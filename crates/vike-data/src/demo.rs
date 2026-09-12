//! The demo tape — the deterministic, obviously-synthetic bars a fresh install is seeded with, so
//! that the FIRST command a new user runs has something to run on.
//!
//! # Why this exists
//!
//! A clean install has an empty hist store, and every surface built on one is therefore empty: the
//! chart draws nothing, the Data Manager lists nothing, and `backtest --profile` over the shipped
//! example profile reports a run with no trades. Nothing is broken and nothing says so — the
//! shipped profile carried a `⚠ Change this to a slice your store actually holds` comment, which
//! is an accurate description of the gap and no help in closing it. This module closes it: one
//! command writes a slice the shipped profile already names.
//!
//! # Why `venue = "demo"` and not `binance`
//!
//! These bars are a closed-form function, not market data. Writing them under a REAL venue id
//! would put synthetic rows in the same namespace as the rows a live recorder or a `--fetch`
//! writes, where the only thing separating a backtest result computed on invented prices from one
//! computed on real prices is whoever remembers which is which. So the demo tape gets a venue of
//! its own, [`DEMO_VENUE`], and the separation is structural: a store can hold both, a query names
//! one, and no aggregation can silently mix them.
//!
//! # Determinism, and what the commit key buys
//!
//! Every bar is a pure function of its timestamp — no clock, no RNG, no file — so two seeds of the
//! same version produce byte-identical rows, and the seed is safe to re-run. Re-running is in fact
//! free rather than merely safe: each slice appends under a versioned commit key
//! ([`DemoSlice::commit_key`]), and `HistStore::append_bars` spends a key once, so a second seed
//! over a store that already holds this tape writes zero rows and reports it.
//!
//! ⚠ **`DEMO_TAPE_VERSION` is part of that key.** Changing the generator without bumping the
//! version leaves a store holding the OLD bars while every gate here describes the new ones, and
//! nothing anywhere would notice; bumping it makes the next seed write the new tape alongside.

use crate::hist::{DataError, HistStore};
use vike_model::Bar;

/// The venue id the demo tape is written under. NOT a real venue — see the module doc.
pub const DEMO_VENUE: &str = "demo";

/// Bumped whenever [`bars_for`] changes what it produces. It is part of every slice's commit key,
/// so a bump is what lets a re-seed write the new tape into a store that holds an old one.
pub const DEMO_TAPE_VERSION: u32 = 1;

/// One symbol/interval span of the demo tape.
///
/// The spans are `const` rather than parameters because they are a CONTRACT with the shipped
/// example profile (`vike-cli init`'s `user_data/profiles/backtest.toml`), and
/// `crates/vike-cli/tests/demo_tape_profile.rs` compares the two. A caller
/// that could ask for an arbitrary span would make that agreement unverifiable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemoSlice {
    /// Instrument id, in the venue's own spelling.
    pub symbol: &'static str,
    /// Bar interval, in the hist store's spelling (`1m`, `1h`).
    pub interval: &'static str,
    /// First bar's timestamp, epoch milliseconds (UTC), INCLUSIVE.
    pub from_ms: i64,
    /// One past the last bar, epoch milliseconds (UTC), EXCLUSIVE — the half-open convention the
    /// rest of the store uses, so a slice's `to_ms` is the next slice's `from_ms` with no overlap.
    pub to_ms: i64,
    /// Milliseconds between bars. Must divide `to_ms - from_ms`.
    pub step_ms: i64,
}

impl DemoSlice {
    /// How many bars [`bars_for`] will produce for this slice.
    #[must_use]
    pub const fn len(&self) -> usize {
        ((self.to_ms - self.from_ms) / self.step_ms) as usize
    }

    /// Never true for a declared slice — present because clippy asks for it beside `len`, and
    /// because a future slice with a mistyped span should be caught by the test that calls this
    /// rather than by a division producing zero rows in silence.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The append key that makes a re-seed a no-op. Carries [`DEMO_TAPE_VERSION`] — see the module
    /// doc for why that is load-bearing rather than decorative.
    #[must_use]
    pub fn commit_key(&self) -> String {
        format!("demo-tape:v{DEMO_TAPE_VERSION}:{}:{}", self.symbol, self.interval)
    }
}

/// 2025-01-01T00:00:00Z, the demo tape's origin.
const ORIGIN_MS: i64 = 1_735_689_600_000;
/// 2025-07-01T00:00:00Z — the end of the hourly span, and of the whole tape.
const END_MS: i64 = 1_751_328_000_000;
/// 2025-06-24T00:00:00Z — the minute span's start, the last seven days of the tape.
const MINUTE_FROM_MS: i64 = 1_750_723_200_000;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;

/// Every span the seed writes.
///
/// Two, for two different first impressions: the HOURLY span is the one the shipped example
/// profile runs over (six months, enough for a moving-average strategy to have a history), and the
/// MINUTE span is the last week of the same tape at chart resolution, because a chart opened over
/// hourly bars looks like a tool with no data in it.
pub const DEMO_SLICES: &[DemoSlice] = &[
    DemoSlice {
        symbol: "BTCUSDT",
        interval: "1h",
        from_ms: ORIGIN_MS,
        to_ms: END_MS,
        step_ms: HOUR,
    },
    DemoSlice {
        symbol: "BTCUSDT",
        interval: "1m",
        from_ms: MINUTE_FROM_MS,
        to_ms: END_MS,
        step_ms: MINUTE,
    },
];

/// The price at an instant, as a closed-form function of the time since [`ORIGIN_MS`].
///
/// Three superposed periods and a drift, chosen so the tape exercises what a first run wants to
/// see rather than looking pretty: a slow trend a moving-average crossover can catch, a daily
/// cycle, and a fast wobble that makes a minute chart look like a market instead of a smooth line.
/// The shape is arbitrary; what matters is that it is a FUNCTION — the same instant yields the same
/// price on every box and every re-seed.
fn price_at(ms_since_origin: i64) -> f64 {
    // Hours as an f64 is exact well past the tape's span (2^53 hours), and every period below is
    // expressed in hours so the two intervals sample ONE curve rather than two similar ones.
    let h = ms_since_origin as f64 / HOUR as f64;
    let trend = 42_000.0 + 9.5 * h;
    let weekly = 2_600.0 * (h / 168.0 * std::f64::consts::TAU).sin();
    let daily = 700.0 * (h / 24.0 * std::f64::consts::TAU).sin();
    let wobble = 90.0 * (h * 7.3).sin();
    trend + weekly + daily + wobble
}

/// The bars of one slice, in ascending timestamp order.
///
/// Each bar is built from the curve at its own open and close instants, with the high/low taken
/// from a sample INSIDE the bar rather than from `open.max(close) + constant`: a constant-width
/// wick is a giveaway that the tape is fake, and — more usefully — it makes every bar's range a
/// function of the volatility the curve actually had during that bar, which is what a strategy
/// reading ranges is entitled to assume.
#[must_use]
pub fn bars_for(slice: &DemoSlice) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(slice.len());
    let mut ts = slice.from_ms;
    while ts < slice.to_ms {
        let open = price_at(ts - ORIGIN_MS);
        let close = price_at(ts + slice.step_ms - ORIGIN_MS);
        // Four interior samples: enough for the extremes to move with the curve, few enough that
        // generating six months of minutes stays instant.
        let mut hi = open.max(close);
        let mut lo = open.min(close);
        for q in 1..5 {
            let p = price_at(ts + slice.step_ms * q / 5 - ORIGIN_MS);
            hi = hi.max(p);
            lo = lo.min(p);
        }
        // Volume rises with the bar's range — the correlation a real tape has, and the one a
        // volume-aware strategy would otherwise find missing.
        let volume = 12.0 + (hi - lo).abs() * 0.4;
        bars.push(Bar {
            ts,
            open,
            high: hi,
            low: lo,
            close,
            volume,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(slice.symbol.to_string()),
        });
        ts += slice.step_ms;
    }
    bars
}

/// What one slice's seed did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeededSlice {
    /// The slice that was offered to the store.
    pub slice: DemoSlice,
    /// Rows the store accepted. ZERO means the commit key was already spent — this store already
    /// holds this tape — which is a success, not a failure.
    pub rows: usize,
}

/// Write the whole demo tape into `store`.
///
/// Takes `&dyn HistStore` rather than a concrete store so this compiles in a build with no
/// DataFusion at all: the trait is always compiled, and the in-memory `MemHistStore` behind
/// `test-support` is what the unit tests below drive. The caller owns opening the real store and
/// deciding where its root is — this function reads no environment and touches no path.
///
/// # Errors
///
/// Propagates the store's own append error unchanged; a partial seed is possible and is reported
/// by the slices that did land, because a caller that must retry is better served by knowing which
/// half is already there than by a rollback this trait cannot offer.
pub fn seed(store: &dyn HistStore) -> Result<Vec<SeededSlice>, DataError> {
    let mut out = Vec::with_capacity(DEMO_SLICES.len());
    for slice in DEMO_SLICES {
        let bars = bars_for(slice);
        let rows = store.append_bars(
            DEMO_VENUE,
            slice.symbol,
            slice.interval,
            &bars,
            Some(&slice.commit_key()),
        )?;
        out.push(SeededSlice { slice: *slice, rows });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hist::TsRange;
    use crate::test_support::MemHistStore;

    #[test]
    fn every_slice_spans_a_whole_number_of_bars() {
        for slice in DEMO_SLICES {
            let span = slice.to_ms - slice.from_ms;
            assert!(span > 0, "{slice:?} spans no time");
            assert_eq!(span % slice.step_ms, 0, "{slice:?}'s span is not a whole number of steps");
            assert_eq!(
                bars_for(slice).len(),
                slice.len(),
                "{slice:?} len() disagrees with bars_for"
            );
        }
    }

    /// The property the whole module rests on: no clock, no RNG, no file. Two generations of the
    /// same slice must be bit-identical, or a "deterministic" tape reseeded on another box would
    /// hold different prices under the same commit key.
    #[test]
    fn the_tape_is_bit_identical_across_generations() {
        for slice in DEMO_SLICES {
            let a = bars_for(slice);
            let b = bars_for(slice);
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(&b) {
                assert_eq!(x.ts, y.ts);
                assert_eq!(x.open.to_bits(), y.open.to_bits(), "open at {}", x.ts);
                assert_eq!(x.high.to_bits(), y.high.to_bits(), "high at {}", x.ts);
                assert_eq!(x.low.to_bits(), y.low.to_bits(), "low at {}", x.ts);
                assert_eq!(x.close.to_bits(), y.close.to_bits(), "close at {}", x.ts);
            }
        }
    }

    /// OHLC has to be internally consistent or every consumer downstream is entitled to be wrong:
    /// a high below the open is not a bar, it is a bug that a chart renders as an inverted wick and
    /// a fill simulator reads as an impossible price.
    #[test]
    fn every_bar_is_a_well_formed_ohlc() {
        for slice in DEMO_SLICES {
            for bar in bars_for(slice) {
                assert!(
                    bar.high >= bar.open && bar.high >= bar.close,
                    "high below a body: {bar:?}"
                );
                assert!(bar.low <= bar.open && bar.low <= bar.close, "low above a body: {bar:?}");
                assert!(bar.high >= bar.low, "inverted range: {bar:?}");
                assert!(bar.open > 0.0 && bar.close > 0.0, "non-positive price: {bar:?}");
                assert!(bar.volume > 0.0, "non-positive volume: {bar:?}");
                assert!(bar.close.is_finite() && bar.high.is_finite(), "non-finite: {bar:?}");
            }
        }
    }

    /// Timestamps must be strictly ascending and exactly one step apart — a store that receives a
    /// duplicate or a gap here would have those defects blamed on it later.
    #[test]
    fn timestamps_are_a_contiguous_ascending_grid() {
        for slice in DEMO_SLICES {
            let bars = bars_for(slice);
            assert_eq!(bars.first().map(|b| b.ts), Some(slice.from_ms));
            assert_eq!(bars.last().map(|b| b.ts), Some(slice.to_ms - slice.step_ms));
            for pair in bars.windows(2) {
                assert_eq!(pair[1].ts - pair[0].ts, slice.step_ms, "step at {}", pair[0].ts);
            }
        }
    }

    /// The two intervals sample ONE curve, so the hourly bar covering a minute span must contain
    /// that span's extremes. Without this the chart and the backtest would disagree about the same
    /// instant, which is the class of divergence that is impossible to debug from a result.
    #[test]
    fn the_minute_and_hour_spans_agree_where_they_overlap() {
        let hourly = bars_for(&DEMO_SLICES[0]);
        let minutes = bars_for(&DEMO_SLICES[1]);
        let hour = minutes[0].ts - minutes[0].ts % HOUR;
        let h =
            hourly.iter().find(|b| b.ts == hour).expect("the hourly span covers the minute one");
        let inside: Vec<_> =
            minutes.iter().filter(|b| b.ts >= hour && b.ts < hour + HOUR).collect();
        assert_eq!(inside.len(), 60);
        let lo = inside.iter().fold(f64::MAX, |a, b| a.min(b.low));
        let hi = inside.iter().fold(f64::MIN, |a, b| a.max(b.high));
        // The hourly bar samples the curve at five points, the minute bars at 300, so the minute
        // extremes are the tighter bound only by sampling luck — what must hold is that they
        // describe the same curve, i.e. neither is wildly outside the other.
        assert!((hi - h.high).abs() < 400.0, "hour high {} vs minute high {hi}", h.high);
        assert!((lo - h.low).abs() < 400.0, "hour low {} vs minute low {lo}", h.low);
    }

    #[test]
    fn seeding_writes_every_slice_and_a_reseed_writes_nothing() {
        let store = MemHistStore::default();
        let first = seed(&store).expect("seed");
        assert_eq!(first.len(), DEMO_SLICES.len());
        for done in &first {
            assert_eq!(done.rows, done.slice.len(), "{:?} wrote a short tape", done.slice);
        }
        let again = seed(&store).expect("re-seed");
        for done in &again {
            assert_eq!(
                done.rows, 0,
                "{:?} was written twice — the commit key did not hold",
                done.slice
            );
        }
        // ...and the rows are readable back under the demo venue, which is the whole point.
        let slice = &DEMO_SLICES[0];
        let read = store
            .load_bars(
                DEMO_VENUE,
                slice.symbol,
                slice.interval,
                TsRange { start: Some(slice.from_ms), end: Some(slice.to_ms) },
            )
            .expect("load");
        assert_eq!(read.len(), slice.len());
    }

    /// The commit key carries the version, so a bump is what lets a changed tape land in a store
    /// that already holds the old one. If this ever fails, `seed` has become non-idempotent across
    /// versions in the other direction: silently identical keys for different bars.
    #[test]
    fn the_commit_key_names_the_version_and_the_slice() {
        for slice in DEMO_SLICES {
            let key = slice.commit_key();
            assert!(
                key.contains(&format!("v{DEMO_TAPE_VERSION}")),
                "{key} does not name the version"
            );
            assert!(
                key.contains(slice.symbol) && key.contains(slice.interval),
                "{key} is not slice-specific"
            );
        }
        let keys: std::collections::BTreeSet<String> =
            DEMO_SLICES.iter().map(DemoSlice::commit_key).collect();
        assert_eq!(keys.len(), DEMO_SLICES.len(), "two slices share a commit key");
    }
}
