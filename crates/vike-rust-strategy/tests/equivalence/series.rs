//! The deterministic bar series and tick tape both lanes are driven over, and the engine params.

use vike_marketdata::{Bar, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_simulator::{InputSeries, Inputs, StrategyEngineConfig, TickEvent};

use super::{BOOK_EVERY, INPUT_NAME, N_BARS, N_TICKS, OTHER_SYMBOL, SYMBOL};

/// The length of one bar of [`bar_series`], in ms: where a bar step decides (`Inputs::bar_ms`).
pub(super) const BAR_MS: i64 = 60_000;

/// How many bars of an input closed before the run's first event, so its first decision already
/// reads a history.
const INPUT_LEAD: usize = 20;

// ---------------------------------------------------------------------------------------------
// The series and the engine
// ---------------------------------------------------------------------------------------------

/// A deterministic bar series: an LCG random walk in whole hundredths, built from integer
/// arithmetic and one division so it is bit-identical on every box and carries no platform
/// transcendental. `symbol` is left `None` — `StrategyEngine::new` stamps every bar with
/// `format_instrument(default_venue, symbol)`, and with the default (absent) venue that is the
/// bare symbol `OrderBrokerSimulator::index` resolves.
pub(super) fn bar_series() -> Vec<Bar> {
    walk(0x5EED_FACE_C0DE_1234, 100.0)
}

/// [`bar_series`]'s walk from `seed` and a first `price`.
fn walk(mut seed: u64, mut price: f64) -> Vec<Bar> {
    let mut out = Vec::with_capacity(N_BARS);
    for i in 0..N_BARS {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        // -2.00 ..= +2.00 in hundredths: enough amplitude that a 5/20 SMA pair crosses often.
        let step = ((seed >> 33) % 401) as f64 / 100.0 - 2.0;
        price = (price + step).max(5.0);
        out.push(Bar {
            timestamp: 1_700_000_000_000 + i as i64 * BAR_MS,
            open: price,
            high: price + 0.5,
            low: (price - 0.5).max(0.01),
            close: price,
            volume: 1_000.0 + (i % 17) as f64,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    out
}

/// The inputs a run loads for a strategy that declares `names`: [`input_series`] for the one the
/// fixtures name, nothing for any other (so a name the run was not built for reads nothing, as an
/// undeclared one does). The bar lane decides at its bar's close; the tick lane at the tick.
pub(super) fn inputs_of(names: &[String], tick_lane: bool) -> Inputs {
    let interval_ms = if tick_lane { 1_000 } else { BAR_MS };
    Inputs {
        bar_ms: if tick_lane { 0 } else { BAR_MS },
        series: names
            .iter()
            .filter(|n| *n == INPUT_NAME)
            .map(|n| input_series(n, interval_ms))
            .collect(),
    }
}

/// A deterministic input series: an LCG walk of its own seed, [`bar_series`]'s integer arithmetic,
/// opening [`INPUT_LEAD`] bars before the run and running past its end. One-minute bars on the bar
/// lane; on the tick lane, whose tape spans about seventy seconds, one-second bars stand in under
/// the same name (the simulator keys an input by its name and reads its bar length from the series),
/// so the input's view moves during that run too.
pub(super) fn input_series(name: &str, interval_ms: i64) -> InputSeries {
    let mut seed: u64 = 0x1A_9E7C_0FFE_E000;
    let mut price: f64 = 50.0;
    let start = 1_700_000_000_000 - INPUT_LEAD as i64 * interval_ms;
    let bars = (0..INPUT_LEAD + N_BARS)
        .map(|i| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let step = ((seed >> 33) % 201) as f64 / 100.0 - 1.0;
            price = (price + step).max(5.0);
            Bar {
                timestamp: start + i as i64 * interval_ms,
                open: price,
                high: price + 0.25,
                low: price - 0.25,
                close: price,
                volume: 10.0 + (i % 7) as f64,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect();
    InputSeries { name: name.to_string(), interval_ms, bars }
}

pub(super) fn universe() -> Vec<(String, Vec<Bar>)> {
    vec![(SYMBOL.to_string(), bar_series())]
}

/// [`universe`] and a second symbol, [`OTHER_SYMBOL`], on a walk of its own over the same bar
/// times: the run a rule trading two symbols (`Tactics::on`) is compared over.
pub(super) fn pair_universe() -> Vec<(String, Vec<Bar>)> {
    let mut out = universe();
    out.push((OTHER_SYMBOL.to_string(), walk(0x0DD5_EED0_2BAD_C0DE, 60.0)));
    out
}

/// Built fresh per run rather than cloned: `StrategyEngineConfig` carries boxed closures and is not
/// `Clone`. Non-zero fees and slippage on purpose — they put more arithmetic between the
/// strategy's order and the trade the result reports.
pub(super) fn engine_params() -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 100_000.0,
        fee_rate: 0.000_6,
        slippage: 0.000_4,
        ..Default::default()
    }
}

/// A deterministic tick tape for the `run_ticks` lane: quotes and trades alternating, with an
/// anchoring book SNAPSHOT before the first priced tick and a book DELTA every [`BOOK_EVERY`].
///
/// ⚠ **The book's PRICE levels never move; only the best bid's resting QTY does.** That is what
/// makes the fixture's `book_bias` read a moving number while `best_bid`/`best_ask` stay put, so a
/// cursor that lost the quantities — or delivered the two sides in the wrong order — changes every
/// subsequent order's size instead of changing nothing. A delta that introduced new price levels
/// would grow the book without bound and make the bias drift for a reason that says nothing about
/// the boundary.
///
/// Sequence numbers are contiguous from the snapshot, because `run_ticks` folds a `Delta` only
/// when `L2Book::delta_decision` says Apply under `L2BookSequencePolicy::Strict` (`seq == last_sequence + 1`) and
/// DROPS the book otherwise — a gap here would silently stop delivering `on_order_book` at all.
pub(super) fn tick_series() -> Vec<TickEvent> {
    fn book(ts: i64, seq: u64, kind: L2BookUpdateKind, bids: Vec<L2BookLevel>) -> TickEvent {
        TickEvent::Book(L2BookUpdate {
            timestamp: ts,
            local_timestamp: 0,
            sequence: seq,
            kind,
            tick_size: 0.5,
            bids,
            asks: if matches!(kind, L2BookUpdateKind::Snapshot) {
                vec![L2BookLevel::new(100.0, 6.0), L2BookLevel::new(100.5, 4.0)]
            } else {
                Vec::new()
            },
            symbol: SYMBOL.to_string(),
        })
    }

    let mut seed: u64 = 0x1234_5678_9ABC_DEF0;
    let mut price: f64 = 100.0;
    let mut ts = 1_700_000_000_000i64;
    let mut seq = 1u64;
    let mut out: Vec<TickEvent> = Vec::with_capacity(N_TICKS + N_TICKS / BOOK_EVERY + 1);
    // Anchor the book BEFORE any priced tick, so `book_bias` is already real by the time the
    // fixture sizes its first order rather than being zero for the first stretch of the run.
    out.push(book(
        ts,
        seq,
        L2BookUpdateKind::Snapshot,
        vec![
            L2BookLevel::new(99.5, 10.0),
            L2BookLevel::new(99.0, 5.0),
            L2BookLevel::new(98.5, 7.0),
        ],
    ));
    for i in 0..N_TICKS {
        ts += 100;
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        // Same integer-arithmetic walk as `bar_series`: whole hundredths, one division, no
        // platform transcendental, so the tape is bit-identical on every box.
        let step = ((seed >> 33) % 401) as f64 / 100.0 - 2.0;
        price = (price + step).max(5.0);
        if i.is_multiple_of(2) {
            out.push(TickEvent::Quote(TickQuote {
                timestamp: ts,
                local_timestamp: 0,
                bid: price - 0.05,
                ask: price + 0.05,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: SYMBOL.to_string(),
            }));
        } else {
            out.push(TickEvent::Trade(TickTrade {
                timestamp: ts,
                local_timestamp: 0,
                price,
                size: 0.5,
                is_buyer_maker: i % 4 == 1,
                symbol: SYMBOL.to_string(),
            }));
        }
        if i % BOOK_EVERY == BOOK_EVERY - 1 {
            ts += 1;
            seq += 1;
            // Re-price the SAME best-bid level with a different resting size — the one input
            // `on_order_book` feeds into the fixture's order sizing.
            let qty = 4.0 + (i % 7) as f64;
            out.push(book(ts, seq, L2BookUpdateKind::Delta, vec![L2BookLevel::new(99.5, qty)]));
        }
    }
    out
}
