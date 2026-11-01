//! Property tests for the price an OANDA instrument is written with ([`vike_oanda::InstrumentGrid::price`]),
//! on the two grids that matter today: five decimals (`EUR_USD` and the other majors) and three (`USD_JPY`,
//! `EUR_JPY`, `GBP_JPY`). OANDA refuses a price with more decimals than the instrument's `displayPrecision`
//! (`PRICE_PRECISION_EXCEEDED`), so the laws are:
//!
//! (1) PRECISION: the text has EXACTLY `displayPrecision` decimals, never more;
//! (2) ROUND-TRIP: a price the engine produced (`(x / step).round() * step`, or the shortest decimal a person
//!     types) comes out as the very grid point it names, on a BUY and on a SELL;
//! (3) PASSIVE SIDE: an off-grid price goes DOWN for a BUY and UP for a SELL, by less than one tick;
//! (4) the five-decimal grid writes byte-for-byte what the adapter's old `format!("{p:.5}")` wrote for every
//!     price the engine can hand over, which is what keeps EUR/AUD/GBP/NZD unchanged.
//!
//! The integer-tick comparisons are exact; the only tolerance is the float noise (a few ULP) around the
//! passive-side bounds, spelled out where it is used. The quantizer underneath is the pinned
//! `vike_bridge_core::format::format_to_step` (its own laws: `crates/vike-bridge-core/tests/format_props.rs`).
//!
//! ⚠ **Docs-shaped**: the precisions are those OANDA's documentation gives for these instruments
//! (<https://developer.oanda.com/rest-live-v20/primitives-df/>); nothing was captured from a practice
//! account.

use proptest::prelude::*;
use vike_model::SymbolProperties;
use vike_oanda::InstrumentGrid;

/// `(decimals, tick, 10^decimals)` of the two grids.
const GRIDS: [(usize, f64, f64); 2] = [(5, 0.00001, 1e5), (3, 0.001, 1e3)];

fn grid(tick: f64) -> InstrumentGrid {
    InstrumentGrid::from_properties(&SymbolProperties {
        tick_size: tick,
        step_size: 1.0,
        ..Default::default()
    })
    .expect("a power-of-ten tick is a grid")
}

fn decimals(text: &str) -> usize {
    text.split_once('.').map_or(0, |(_, frac)| frac.len())
}

/// The text as a whole number of ticks (`"150.123"` at three decimals is 150123).
fn ticks(text: &str) -> i128 {
    text.replace('.', "").parse().unwrap_or_else(|e| panic!("{text:?} is not a plain decimal: {e}"))
}

fn any_grid() -> impl Strategy<Value = (usize, f64, f64)> {
    prop::sample::select(GRIDS.to_vec())
}

/// Float noise allowed around a bound: a few ULP of the price (`4 * EPSILON`, relative).
fn noise(price: f64) -> f64 {
    price * 4.0 * f64::EPSILON
}

proptest! {
    /// (1) + (2): the grid point the engine's own rounding names comes out exactly, on either side.
    #[test]
    fn an_engine_rounded_price_is_written_as_its_grid_point(
        (d, tick, _scale) in any_grid(),
        n in 1_u64..=200_000_000,
    ) {
        let price = n as f64 * tick; // the engine's `(x / step).round() * step`
        for side in [1, -1] {
            let text = grid(tick).price(side, price);
            prop_assert_eq!(decimals(&text), d, "{} side {}", text, side);
            prop_assert_eq!(ticks(&text), i128::from(n), "{} side {}", text, side);
        }
    }

    /// (2) again for the shortest decimal image (what a person types: `150.123`).
    #[test]
    fn a_typed_decimal_is_written_as_itself(
        (d, tick, scale) in any_grid(),
        n in 1_u64..=200_000_000,
    ) {
        let price = n as f64 / scale;
        for side in [1, -1] {
            let text = grid(tick).price(side, price);
            prop_assert_eq!(decimals(&text), d);
            prop_assert_eq!(ticks(&text), i128::from(n), "{} side {}", text, side);
        }
    }

    /// (3) exactly: between two grid points a BUY gets the lower and a SELL the upper.
    #[test]
    fn an_off_grid_price_goes_to_the_passive_side(
        (d, tick, scale) in any_grid(),
        n in 1_u64..=200_000_000,
        frac in 0.05_f64..0.95,
    ) {
        let price = (n as f64 + frac) / scale;
        let buy = grid(tick).price(1, price);
        let sell = grid(tick).price(-1, price);
        prop_assert_eq!(decimals(&buy), d);
        prop_assert_eq!(decimals(&sell), d);
        prop_assert_eq!(ticks(&buy), i128::from(n), "BUY {} for {}", buy, price);
        prop_assert_eq!(ticks(&sell), i128::from(n) + 1, "SELL {} for {}", sell, price);
    }

    /// (1) + (3) for ANY price in the real range: the decimals are exact, a BUY never above the price and a
    /// SELL never below it (up to float noise), each less than a tick away.
    #[test]
    fn any_price_keeps_the_precision_and_the_side(
        (d, tick, _scale) in any_grid(),
        price in 1e-3_f64..1e6,
    ) {
        let (buy, sell) = (grid(tick).price(1, price), grid(tick).price(-1, price));
        for text in [&buy, &sell] {
            prop_assert_eq!(decimals(text), d, "{} for {}", text, price);
            prop_assert!(text.parse::<f64>().is_ok(), "{} does not round-trip", text);
        }
        let (b, s, eps) = (buy.parse::<f64>().unwrap(), sell.parse::<f64>().unwrap(), noise(price));
        prop_assert!(b <= price + eps && price - b < tick + eps, "BUY {} for {}", buy, price);
        prop_assert!(s >= price - eps && s - price < tick + eps, "SELL {} for {}", sell, price);
    }

    /// (4) the five-decimal grid is byte-identical to the old fixed `{:.5}` for every price the engine hands
    /// over (and for the typed decimal), on both sides: EUR/AUD/GBP/NZD do not change.
    #[test]
    fn the_five_decimal_grid_is_what_the_fixed_format_wrote(n in 1_u64..=10_000_000) {
        let majors = grid(0.00001);
        for price in [n as f64 * 0.00001, n as f64 / 1e5] {
            let old = format!("{price:.5}");
            prop_assert_eq!(majors.price(1, price), old.clone());
            prop_assert_eq!(majors.price(-1, price), old);
        }
    }
}

/// The edges a generator rarely lands on, pinned by example: a price below one tick, exactly one tick, a
/// whole number, and the grids' own rounding of the prices a JPY desk quotes.
#[test]
fn the_edges() {
    let (eur, jpy) = (grid(0.00001), grid(0.001));
    assert_eq!(eur.price(1, 0.000004), "0.00000");
    assert_eq!(eur.price(-1, 0.000004), "0.00001");
    assert_eq!(eur.price(1, 0.00001), "0.00001");
    assert_eq!(jpy.price(1, 100.0), "100.000");
    assert_eq!(jpy.price(-1, 100.0), "100.000");
    assert_eq!(jpy.price(1, 149.9995), "149.999");
    assert_eq!(jpy.price(-1, 149.9995), "150.000");
}
