//! The deterministic bar series and tick tape both lanes are driven over, and the engine params.

use vike_model::{Bar, BookLevel, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};
use vike_sim::{EngineParams, Tick};

use super::{BOOK_EVERY, N_BARS, N_TICKS, SYMBOL};

// ---------------------------------------------------------------------------------------------
// The series and the engine
// ---------------------------------------------------------------------------------------------

/// A deterministic bar series: an LCG random walk in whole hundredths, built from integer
/// arithmetic and one division so it is bit-identical on every box and carries no platform
/// transcendental. `symbol` is left `None` — `StrategyEngine::new` stamps every bar with
/// `format_instrument(default_venue, symbol)`, and with the default (absent) venue that is the
/// bare symbol `SimBroker::idx` resolves.
pub(super) fn bar_series() -> Vec<Bar> {
    let mut seed: u64 = 0x5EED_FACE_C0DE_1234;
    let mut price: f64 = 100.0;
    let mut out = Vec::with_capacity(N_BARS);
    for i in 0..N_BARS {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        // -2.00 ..= +2.00 in hundredths: enough amplitude that a 5/20 SMA pair crosses often.
        let step = ((seed >> 33) % 401) as f64 / 100.0 - 2.0;
        price = (price + step).max(5.0);
        out.push(Bar {
            ts: 1_700_000_000_000 + i as i64 * 60_000,
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

pub(super) fn universe() -> Vec<(String, Vec<Bar>)> {
    vec![(SYMBOL.to_string(), bar_series())]
}

/// Built fresh per run rather than cloned: `EngineParams` carries boxed closures and is not
/// `Clone`. Non-zero fees and slippage on purpose — they put more arithmetic between the
/// strategy's order and the trade the result reports.
pub(super) fn engine_params() -> EngineParams {
    EngineParams { cash: 100_000.0, fee_rate: 0.000_6, slippage: 0.000_4, ..Default::default() }
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
/// when `L2Book::delta_decision` says Apply under `SeqPolicy::Strict` (`seq == last_seq + 1`) and
/// DROPS the book otherwise — a gap here would silently stop delivering `on_order_book` at all.
pub(super) fn tick_series() -> Vec<Tick> {
    fn book(ts: i64, seq: u64, kind: BookUpdateKind, bids: Vec<BookLevel>) -> Tick {
        Tick::Book(BookUpdate {
            ts,
            local_ts: 0,
            seq,
            kind,
            tick_size: 0.5,
            bids,
            asks: if matches!(kind, BookUpdateKind::Snapshot) {
                vec![BookLevel::new(100.0, 6.0), BookLevel::new(100.5, 4.0)]
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
    let mut out: Vec<Tick> = Vec::with_capacity(N_TICKS + N_TICKS / BOOK_EVERY + 1);
    // Anchor the book BEFORE any priced tick, so `book_bias` is already real by the time the
    // fixture sizes its first order rather than being zero for the first stretch of the run.
    out.push(book(
        ts,
        seq,
        BookUpdateKind::Snapshot,
        vec![BookLevel::new(99.5, 10.0), BookLevel::new(99.0, 5.0), BookLevel::new(98.5, 7.0)],
    ));
    for i in 0..N_TICKS {
        ts += 100;
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        // Same integer-arithmetic walk as `bar_series`: whole hundredths, one division, no
        // platform transcendental, so the tape is bit-identical on every box.
        let step = ((seed >> 33) % 401) as f64 / 100.0 - 2.0;
        price = (price + step).max(5.0);
        if i.is_multiple_of(2) {
            out.push(Tick::Quote(QuoteTick {
                ts,
                local_ts: 0,
                bid: price - 0.05,
                ask: price + 0.05,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: SYMBOL.to_string(),
            }));
        } else {
            out.push(Tick::Trade(TradeTick {
                ts,
                local_ts: 0,
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
            out.push(book(ts, seq, BookUpdateKind::Delta, vec![BookLevel::new(99.5, qty)]));
        }
    }
    out
}
