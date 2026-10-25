//! The grid ONE OANDA instrument is written on, read from the instrument's OWN row.
//!
//! OANDA refuses a price written with more decimals than the instrument's `displayPrecision`
//! (`PRICE_PRECISION_EXCEEDED`) and a unit count with more decimals than its `tradeUnitsPrecision`, and
//! the two differ by instrument: 5 for `EUR_USD`, 3 for `USD_JPY`, `EUR_JPY` and `GBP_JPY`; units are whole
//! for a currency pair. So the exec client no longer writes one fixed precision: it reads the account's
//! instrument list (`GET /v3/accounts/{id}/instruments`, the list `crate::catalog::parse_instruments`
//! already parses for the gatewayhub's facts) and writes each order on its instrument's grid.
//!
//! **Docs-shaped, not captured.** The shapes are those of OANDA's documentation, no practice-account
//! payload has been recorded: the endpoint is
//! <https://developer.oanda.com/rest-live-v20/account-ep/> and the `Instrument` fields
//! (`displayPrecision`: "the number of decimal places that should be used to display prices for this
//! instrument"; `tradeUnitsPrecision`: "the amount of decimal places that may be provided when specifying
//! the number of units traded") are at <https://developer.oanda.com/rest-live-v20/primitives-df/>.
//! Whether the venue accepts every price that has FEWER decimals than `displayPrecision` is the same
//! reading of the docs and is unproven on the wire.
//!
//! # Fail closed
//!
//! A row whose precision is missing, not an integer, negative or beyond `MAX_DECIMALS` has NO grid: its
//! [`InstrumentGrid`] is an `Err` with a sentence, and an order on it is refused (`GridBook::of`). Nothing
//! here ever falls back to five decimals. The ONE five-decimal constant, `InstrumentGrid::MAJORS`, is the
//! grid of the offline fixtures' `EUR_USD` (`build_order_body`, `build_replace_body`); `run_on` never reads it.
//!
//! # One rule for the door and the exec client
//!
//! [`grid_refusal`] is the gatewayhub's admission fence and [`InstrumentGrid::from_properties`] is the exec
//! client's grid; both read the SAME [`SymbolProperties`] a catalog row carries (`tick_size` =
//! `10^-displayPrecision`, `step_size` = `10^-tradeUnitsPrecision`), so the grid the engine judges an order
//! on is the grid the wire writes it on.
//!
//! # What stays refused
//!
//! Only a `CURRENCY` row is admitted on its own price grid, and only on whole units. A metal's or a CFD's
//! amount unit is unmeasured (the Dukascopy lesson), so every other class keeps the grid the adapter was
//! proven on (five decimals, whole units): what the door refused before it still refuses, and what it
//! admitted it still admits. [`InstrumentGrid::units`] already writes fractional units for an instrument
//! whose `tradeUnitsPrecision` is above zero, so lifting that refusal later is a fence edit, not an
//! adapter one.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::format::format_to_step;
use vike_model::SymbolProperties;

use crate::catalog::parse_instruments;

/// `10^-k` for `k` in `0..=MAX_DECIMALS`, as the f64 NEAREST to the decimal (literals are correctly
/// rounded, exactly what `crate::catalog`'s `pow10` parses from `1e-k`), so `==` against a catalog tick
/// is exact. Real rows sit in `0..=5`; the bound only keeps a garbage integer from becoming a grid.
const TICKS: [f64; 9] = [1.0, 0.1, 0.01, 0.001, 0.0001, 0.00001, 0.000001, 0.0000001, 0.00000001];

/// `10^k` for the same `k`: exact in f64, the scale of the fractional-unit path.
const SCALES: [f64; 9] = [1.0, 10.0, 100.0, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8];

/// The most decimals a price or unit grid is believed to have.
const MAX_DECIMALS: u32 = (TICKS.len() - 1) as u32;

/// The venue word a `CURRENCY` row carries (`crate::catalog`'s `parse_instruments` keeps OANDA's `type`).
const VENUE_WORD_CURRENCY: &str = "CURRENCY";

/// The price grid of the five-decimal instruments the adapter was proven on.
const LEGACY_TICK: f64 = 0.00001;

/// A price within this many (relative) f64 steps of a grid point NAMES that grid point: the engine
/// rounds with `(x / step).round() * step`, which can land an ULP off the decimal, and a SELL must not be
/// moved a whole tick up for that. `4 * EPSILON` is above the 3-ULP budget
/// `crates/vike-bridge-core/tests/format_props.rs` derives for `format_to_step`'s own grid-point rescue.
const GRID_NOISE: f64 = 4.0 * f64::EPSILON;

/// How many decimals `x` (a catalog tick or step) is, when it is `10^-k` for a `k` this adapter can write.
fn decimals_of(x: f64) -> Option<u32> {
    TICKS.iter().position(|t| *t == x).map(|k| k as u32)
}

/// The `format_to_step` step text of `decimals` decimals: `"1"`, `"0.1"`, `"0.001"` ...
fn step_text(decimals: u32) -> String {
    if decimals == 0 {
        "1".to_string()
    } else {
        format!("0.{}1", "0".repeat(decimals as usize - 1))
    }
}

/// The price decimals and the unit decimals of ONE instrument. Built only from a row that states both
/// ([`Self::from_properties`]); there is no `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstrumentGrid {
    price_decimals: u32,
    unit_decimals: u32,
}

impl InstrumentGrid {
    /// The grid of the five-decimal, whole-unit majors (`EUR_USD`, `GBP_USD`, `AUD_USD`, `NZD_USD`): what
    /// the offline body builders (`build_order_body`, `build_replace_body`) and their fixtures write. The
    /// live exec loop reads the instrument's own row instead and refuses an instrument without one.
    pub(crate) const MAJORS: Self = Self { price_decimals: 5, unit_decimals: 0 };

    /// The grid a catalog row's properties state: `tick_size` must be exactly `10^-k` and `step_size`
    /// exactly `10^-u` with `k` and `u` in `0..=MAX_DECIMALS`. A missing field (`0.0`), a negative
    /// precision (a tick above 1), an out-of-range one or a non-power-of-ten is `Err` with the sentence.
    pub fn from_properties(p: &SymbolProperties) -> Result<Self, String> {
        let price_decimals = decimals_of(p.tick_size).ok_or_else(|| {
            format!(
                "its price grid ({}) is not a whole number of decimal places from 0 to \
                 {MAX_DECIMALS}: the instrument list's displayPrecision is missing, malformed or \
                 out of range",
                p.tick_size
            )
        })?;
        let unit_decimals = decimals_of(p.step_size).ok_or_else(|| {
            format!(
                "its unit step ({}) is not a whole number of decimal places from 0 to \
                 {MAX_DECIMALS}: the instrument list's tradeUnitsPrecision is missing, malformed \
                 or out of range",
                p.step_size
            )
        })?;
        Ok(Self { price_decimals, unit_decimals })
    }

    /// Decimals of a price (OANDA's `displayPrecision`).
    #[must_use]
    pub const fn price_decimals(&self) -> u32 {
        self.price_decimals
    }

    /// Decimals of a unit count (OANDA's `tradeUnitsPrecision`).
    #[must_use]
    pub const fn unit_decimals(&self) -> u32 {
        self.unit_decimals
    }

    /// `price` as the wire text, with EXACTLY [`Self::price_decimals`] decimals, on this instrument's grid.
    /// An off-grid price is rounded to the PASSIVE side: a BUY (`side >= 0`) down and a SELL up, so the
    /// venue never fills at a worse price than the caller named. A price on the grid (or an ULP off it, see
    /// `GRID_NOISE`) is written as the grid point it names, whatever the side.
    ///
    /// The quantization is `vike_bridge_core::format::format_to_step`, the pinned Decimal wire site (it
    /// truncates toward zero, which is DOWN for a price); the SELL's step up goes through the same helper
    /// from half a tick past the grid point below, so the float addition has half a tick of margin and
    /// cannot land on the wrong one.
    #[must_use]
    pub fn price(&self, side: i32, price: f64) -> String {
        let step = step_text(self.price_decimals);
        let down = format_to_step(price, &step);
        if side >= 0 {
            return down;
        }
        let Ok(down_f) = down.parse::<f64>() else { return down };
        if !price.is_finite() || price <= 0.0 || (down_f - price).abs() <= price * GRID_NOISE {
            return down;
        }
        format_to_step(down_f + 1.5 * TICKS[self.price_decimals as usize], &step)
    }

    /// The signed unit count as the wire text, rounded to the nearest unit of this grid (ties away from
    /// zero, as the whole-unit path always did), negative for a SELL. `None` when it rounds to nothing.
    /// A whole-unit grid is written as an integer, byte-for-byte the way `exec` always wrote units.
    #[must_use]
    pub fn units(&self, side: i32, qty: f64) -> Option<String> {
        let sign: i64 = if side >= 0 { 1 } else { -1 };
        if self.unit_decimals == 0 {
            let n = (qty.round() as i64).saturating_mul(sign);
            return (n != 0).then(|| n.to_string());
        }
        let scale = SCALES[self.unit_decimals as usize];
        let n = (qty.abs() * scale).round();
        if !n.is_finite() || n < 1.0 {
            return None;
        }
        let minus = if sign < 0 { "-" } else { "" };
        Some(format!("{minus}{:.*}", self.unit_decimals as usize, n / scale))
    }
}

/// Why the gatewayhub cannot admit an OANDA instrument on this grid, or `None`. `venue_word` is the
/// row's OANDA `type` (`CURRENCY`, `CFD`, `METAL`; `None` when the row states none).
///
/// * a `CURRENCY` row is admitted on its OWN price grid (any `displayPrecision` from 0 to
///   `MAX_DECIMALS`, 8: 5 for `EUR_USD`, 3 for a JPY pair) and on whole units;
/// * every other row keeps the grid the adapter was proven on, five decimals and whole units (the
///   module doc says why).
///
/// A row with no tick at all never gets here: `SymbolFacts::facts_via` refuses it first, in its own
/// words.
#[must_use]
pub fn grid_refusal(
    symbol: &str,
    venue_word: Option<&str>,
    p: &SymbolProperties,
) -> Option<String> {
    if venue_word == Some(VENUE_WORD_CURRENCY) {
        if decimals_of(p.tick_size).is_none() {
            return Some(format!(
                "oanda: {symbol} has a price grid of {}, which is not a whole number of decimal \
                 places from 0 to {MAX_DECIMALS} (the instrument list's displayPrecision is missing, \
                 malformed or out of range), so this adapter cannot write its price and the \
                 instrument is not admitted",
                p.tick_size
            ));
        }
    } else if (p.tick_size - LEGACY_TICK).abs() > 1e-12 {
        return Some(format!(
            "oanda: {symbol} has a price grid of {}, and only a currency pair is admitted on its \
             own price precision: any other instrument must sit on the five decimals the adapter \
             was proven on (a grid of {LEGACY_TICK}), so an instrument with another price \
             precision is not admitted",
            p.tick_size
        ));
    }
    if (p.step_size - 1.0).abs() > 1e-9 {
        return Some(format!(
            "oanda: {symbol} trades in steps of {} units, and this adapter rounds every order to \
             WHOLE units, so an instrument whose unit step is not 1 is not admitted",
            p.step_size
        ));
    }
    None
}

/// Every instrument of an instrument-list payload with the grid its own row states, or the sentence for
/// why it has none. Keyed by OANDA's name (`EUR_USD`).
pub(crate) fn parse_grids(payload: &Value) -> HashMap<String, Result<InstrumentGrid, String>> {
    parse_instruments(payload)
        .into_iter()
        .map(|row| {
            let grid = InstrumentGrid::from_properties(&row.properties)
                .map_err(|why| format!("oanda: {}: {why}", row.raw_symbol));
            (row.raw_symbol, grid)
        })
        .collect()
}

/// How long [`GridBook`] leaves the instrument list alone after reading it (or failing to): a flood of
/// orders on an instrument the list does not carry, or against a dead endpoint, costs one read per pause.
const READ_PAUSE: Duration = Duration::from_secs(30);

/// The account's instrument grids, read from the venue on the first order that needs one and again, at
/// most once per [`READ_PAUSE`], when an order names an instrument the book does not hold. It lives on the
/// exec thread (one owner, no lock). A failed read keeps what the book already holds.
#[derive(Default)]
pub(crate) struct GridBook {
    grids: HashMap<String, Result<InstrumentGrid, String>>,
    /// When the list may be read again; `None` = never read.
    next_read: Option<Instant>,
    /// Why the last read failed, if it did.
    failure: Option<String>,
}

impl GridBook {
    /// A book that already holds `grids` and has read nothing: the fixtures' `EUR_USD` of `exec`'s
    /// test-only `run`, whose scripted venues answer the order calls and nothing else.
    #[cfg(test)]
    pub(crate) fn preloaded(
        grids: impl IntoIterator<Item = (&'static str, InstrumentGrid)>,
    ) -> Self {
        Self {
            grids: grids.into_iter().map(|(name, grid)| (name.to_string(), Ok(grid))).collect(),
            ..Self::default()
        }
    }

    /// The grid of `instrument` (OANDA's name, `USD_JPY`), or the sentence the order is refused with.
    /// `read` fetches the instrument-list payload and runs only when the book must (re)read; `now`
    /// is the clock, a parameter so a test drives it.
    pub(crate) fn of(
        &mut self,
        instrument: &str,
        now: Instant,
        read: impl FnOnce() -> Result<Value, String>,
    ) -> Result<InstrumentGrid, String> {
        if let Some(found) = self.grids.get(instrument) {
            return found.clone();
        }
        if self.next_read.is_none_or(|at| now >= at) {
            match read() {
                Ok(payload) => {
                    let grids = parse_grids(&payload);
                    if grids.is_empty() {
                        self.failure =
                            Some("the account's instrument list came back empty".to_string());
                    } else {
                        self.grids = grids;
                        self.failure = None;
                    }
                }
                Err(why) => self.failure = Some(why),
            }
            self.next_read = Some(now + READ_PAUSE);
        }
        match self.grids.get(instrument) {
            Some(found) => found.clone(),
            None => Err(match &self.failure {
                Some(why) => format!(
                    "oanda: {instrument} has no known price grid, because the account's instrument \
                     list could not be read ({why}); the order is refused rather than written on a \
                     guessed grid"
                ),
                None => format!(
                    "oanda: {instrument} is not in the account's instrument list, so its price grid \
                     is unknown; the order is refused rather than written on a guessed grid"
                ),
            }),
        }
    }
}

#[path = "instrument_grid_tests.rs"]
#[cfg(test)]
mod instrument_grid_tests;
