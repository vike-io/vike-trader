//! The per-instrument PRICE SCALE of a Dukascopy `.bi5` record, and the refusal without one. The archive import lane's
//! half of `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §3.4.
//!
//! A `.bi5` record carries its prices as integer "points"; the price is points divided by the instrument's POINT VALUE.
//! The vendor's data-export page (`https://www.dukascopy.com/wiki/en/development/data-export/`) states it for FX only
//! ("most FX pairs" 100,000, the JPY pairs 1,000) and says "Verify per-instrument" for the rest, warning that
//! "silently applying 100,000 to a non-FX instrument will produce wrong prices with no error raised".
//!
//! **The point value is the instrument's TICK SCALE**, `10^tick_scale`: the reciprocal of the price step the account's
//! instrument list states for it (`<project>/instruments/dukascopy.json`, kept by the datahub from the list the JForex
//! sidecar states at its login). Measured on 2026-11-13, one decoded hour per class (2024-01-03 14:00 UTC, the bridge's
//! `crates/bridges/dukascopy/tests/bi5_agrees_with_the_bridge.rs` live probe) against the market of that hour: EUR/USD
//! and USD/TRY at 100,000, USD/JPY, XAU/USD, XAG/USD and USA500.IDX/USD at 1,000, VOL.IDX/USD at 100, BTC/USD at 10.
//! No instrument list is compiled into this crate (owner 2026-11-13, "remove all hardcoded symbols"): the CALLER, the
//! datahub's archive import, builds the scale from the kept row ([`Bi5PriceScale::from_tick_size`]), and an instrument
//! the kept list has no row for is refused before a file is opened ([`unscaled_instrument_refusal`]).
//!
//! The bridge's HTTP lane scales by the same rule from the same list (`crates/bridges/dukascopy/src/instrument/grid.rs`'s
//! `history_point_value`), so a tick decoded by either lane is the same `f64`.

/// One instrument's `.bi5` price scale and the evidence for it.
///
/// Its fields are private and its one constructor ([`Bi5PriceScale::from_tick_size`]) admits only a power of ten, so a
/// decoder that takes a `&Bi5PriceScale` cannot be handed a scale no list row states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bi5PriceScale {
    instrument: String,
    point_value: u32,
    evidence: String,
}

impl Bi5PriceScale {
    /// The scale of `instrument` whose kept list row states the price step `tick_size`: the point value is the step's
    /// reciprocal, a power of ten from 1 to 10^9. `None` for any step that is not one (zero, negative, not a number, or
    /// not `10^-n`), so a doubtful row is a refusal, never a rounded guess.
    #[must_use]
    pub fn from_tick_size(instrument: &str, tick_size: f64) -> Option<Self> {
        if !(tick_size.is_finite() && tick_size > 0.0) {
            return None;
        }
        let (scale, point_value) = (0u32..=9)
            .map(|n| (n, 10u32.pow(n)))
            .find(|&(_, p)| (f64::from(p) * tick_size - 1.0).abs() < 1e-9)?;
        Some(Self {
            instrument: instrument.to_string(),
            point_value,
            evidence: format!(
                "the kept dukascopy instrument list's price step {tick_size} (tick scale {scale})"
            ),
        })
    }

    /// The vendor's upper-case instrument name — `"EURUSD"`.
    pub fn instrument(&self) -> &str {
        &self.instrument
    }

    /// Integer points per unit of price: a record's price field divided by this is the price.
    pub fn point_value(&self) -> u32 {
        self.point_value
    }

    /// Why this point value is believed — printed beside it in an import plan.
    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

/// **What to do when there is no Dukascopy instrument list at all**, in one plain line. The owner's ruling of 2026-11-13:
/// Dukascopy history REQUIRES a Dukascopy account (a free demo account is enough), and there is no history-only fallback
/// without one. Every "not now" a missing list causes says it word for word: the bridge's door facts, conversion and `.bi5`
/// history (`crates/bridges/dukascopy/src/instrument/grid.rs`'s `not_listed`) and the datahub's archive import.
pub const NO_INSTRUMENT_LIST: &str = "no Dukascopy instrument list yet: open a free Dukascopy demo account, add it with \
     `vike-cli secrets account add --venue dukascopy --tier demo --no-label`, file its login with `vike-cli secrets set \
     --account <ID> LOGIN` and `PASSWORD` (each value on stdin), and start it; the list arrives at its first login";

/// The refusal for an instrument the kept list has no usable row for. It quotes the vendor's own warning, because that
/// warning is the whole reason a missing row is a refusal rather than a default.
pub fn unscaled_instrument_refusal(instrument: &str) -> String {
    format!(
        "{instrument}: the kept dukascopy instrument list (<project>/instruments/dukascopy.json) has no row for it \
         with a price step, so its price scale is not known and it is not imported. The vendor's own warning: \
         \"silently applying 100,000 to a non-FX instrument will produce wrong prices with no error raised\". The \
         list is the account's, refreshed from the JForex sidecar's login: an instrument the account does not list \
         has no scale here."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured point values (the module doc) come out of the steps the list rows state.
    #[test]
    fn a_power_of_ten_step_is_its_reciprocal() {
        for (name, step, points) in [
            ("EURUSD", 0.00001, 100_000),
            ("USDJPY", 0.001, 1_000),
            ("XAUUSD", 0.001, 1_000),
            ("EURCZK", 0.0001, 10_000),
            ("VOLIDXUSD", 0.01, 100),
            ("BTCUSD", 0.1, 10),
            ("WHOLE", 1.0, 1),
            ("FINE", 1e-9, 1_000_000_000),
        ] {
            let scale = Bi5PriceScale::from_tick_size(name, step).expect(name);
            assert_eq!((scale.instrument(), scale.point_value()), (name, points), "{name}");
            assert!(scale.evidence().contains("kept dukascopy instrument list"), "{name}");
        }
    }

    #[test]
    fn a_step_that_is_no_power_of_ten_is_no_scale() {
        for step in [0.0, -0.001, f64::NAN, f64::INFINITY, 0.00002, 0.25, 10.0, 1e-10] {
            assert_eq!(Bi5PriceScale::from_tick_size("EURUSD", step), None, "{step}");
        }
    }

    /// The line names the shipped verbs as an operator types them (`vike-cli secrets account add` takes `--venue`, `--tier`
    /// and `--label`/`--no-label`; `vike-cli secrets set` takes `--account` and the FIELD, the value on stdin).
    #[test]
    fn the_no_list_line_says_what_to_do_in_one_line() {
        assert!(NO_INSTRUMENT_LIST.starts_with("no Dukascopy instrument list yet: "));
        assert!(!NO_INSTRUMENT_LIST.contains('\n'));
        for words in [
            "free Dukascopy demo account",
            "`vike-cli secrets account add --venue dukascopy --tier demo --no-label`",
            "`vike-cli secrets set --account <ID> LOGIN`",
            "the list arrives at its first login",
        ] {
            assert!(NO_INSTRUMENT_LIST.contains(words), "{words}");
        }
    }

    #[test]
    fn the_refusal_quotes_the_vendors_warning() {
        let msg = unscaled_instrument_refusal("XAUUSD");
        assert!(msg.starts_with("XAUUSD: "), "{msg}");
        assert!(
            msg.contains(
                "silently applying 100,000 to a non-FX instrument will produce wrong prices with \
                 no error raised"
            ),
            "{msg}"
        );
    }
}
