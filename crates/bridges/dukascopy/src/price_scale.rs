//! The per-instrument PRICE SCALE of a Dukascopy `.bi5` record — a table with evidence on every row,
//! and a refusal outside it. The archive import lane's half of
//! `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §3.4.
//!
//! A `.bi5` record carries its prices as integer "points"; the price is points divided by the
//! instrument's POINT VALUE. The vendor's data-export page
//! (`https://www.dukascopy.com/wiki/en/development/data-export/`, read in full 2026-09-30) states it
//! for FX only: "most FX pairs" 100,000, the JPY pairs it lists 1,000, and for indices and
//! commodities "Varies — Verify per-instrument", warning that "silently applying 100,000 to a non-FX
//! instrument will produce wrong prices with no error raised".
//!
//! ⚠ **This is NOT the HTTP lane's scale, and the difference is the point.** That lane scales by
//! `crates/bridges/dukascopy/src/data.rs`'s `point_divisor` — a SUFFIX rule (JPY gives 1,000,
//! everything else 100,000) — so it scales XAUUSD, XAGUSD and the HUF/CZK crosses of
//! `crates/bridges/dukascopy/src/catalog.rs`'s `bundled_instruments` by 100,000 without a word
//! (the design's finding F4; whether those prices are wrong is unverified, and Q7 decides whether
//! that lane adopts this table, as its own PR). This table admits ONLY what the vendor's rule
//! actually covers:
//!
//! - **the G10 pairs of `bundled_instruments`** — both legs among USD, EUR, GBP, JPY, CHF, CAD,
//!   AUD, NZD — at the vendor's FX rule;
//! - **no exotic.** "Most" is not "every": the TRY, MXN, ZAR, SGD, HKD, NOK, SEK, PLN, DKK, CNH,
//!   HUF and CZK pairs wait for a measurement like any non-FX instrument, and a HUF- or CZK-quoted
//!   cross is exactly where a 100,000 scale is doubtful;
//! - **no non-FX row until one is measured.** The vendor's own sample decoder carries a comment of
//!   100 for XAUUSD and US30; that is an example, not a statement.
//!
//! A row is ADDED once measured: one decoded hour of the instrument compared with an independent
//! quote for the same minute. `crate::archive`'s tests carry the `#[ignore]`d live probe that
//! decodes that hour for every unscaled instrument of `bundled_instruments`.
//!
//! On every row the table carries, the archive scale EQUALS the HTTP lane's suffix rule — a test
//! holds it — so a tick decoded by either lane is the same `f64` (§3.2's one-owner rule rests on
//! that).

/// Where a 100,000 row's point value comes from.
const VENDOR_MOST_FX: &str = "vendor page, \"most FX pairs\"";
/// Where a 1,000 row's point value comes from: the page lists USDJPY, EURJPY, GBPJPY, AUDJPY,
/// NZDJPY, CADJPY and CHFJPY.
const VENDOR_JPY_LIST: &str = "vendor page, the JPY-pair list";

/// One instrument's `.bi5` price scale and the evidence for it.
///
/// Its fields are private and the only rows are this module's table, reached through
/// [`bi5_price_scale`], so a decoder that takes a `&Bi5PriceScale` cannot be handed a scale nobody
/// measured — the table IS the admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bi5PriceScale {
    instrument: &'static str,
    point_value: u32,
    evidence: &'static str,
}

impl Bi5PriceScale {
    /// The vendor's upper-case instrument name — `"EURUSD"`.
    pub fn instrument(&self) -> &'static str {
        self.instrument
    }

    /// Integer points per unit of price: a record's price field divided by this is the price.
    pub fn point_value(&self) -> u32 {
        self.point_value
    }

    /// Why this row's point value is believed — printed beside it in an import plan.
    pub fn evidence(&self) -> &'static str {
        self.evidence
    }
}

/// A row at the vendor's "most FX pairs" rule.
const fn most_fx(instrument: &'static str) -> Bi5PriceScale {
    Bi5PriceScale { instrument, point_value: 100_000, evidence: VENDOR_MOST_FX }
}

/// A row from the vendor's JPY-pair list.
const fn jpy(instrument: &'static str) -> Bi5PriceScale {
    Bi5PriceScale { instrument, point_value: 1_000, evidence: VENDOR_JPY_LIST }
}

/// Every instrument the archive lane admits. The G10 pairs of `bundled_instruments`, in that list's
/// order; nothing else (see the module doc for what waits for a measurement, and why).
const PRICE_SCALES: &[Bi5PriceScale] = &[
    // majors
    most_fx("EURUSD"),
    jpy("USDJPY"),
    most_fx("GBPUSD"),
    most_fx("USDCHF"),
    most_fx("USDCAD"),
    most_fx("AUDUSD"),
    most_fx("NZDUSD"),
    // crosses
    jpy("EURJPY"),
    most_fx("EURGBP"),
    most_fx("EURCHF"),
    most_fx("EURAUD"),
    most_fx("EURCAD"),
    most_fx("EURNZD"),
    jpy("GBPJPY"),
    most_fx("GBPCHF"),
    most_fx("GBPAUD"),
    most_fx("GBPCAD"),
    most_fx("GBPNZD"),
    jpy("AUDJPY"),
    most_fx("AUDCHF"),
    most_fx("AUDCAD"),
    most_fx("AUDNZD"),
    jpy("CADJPY"),
    most_fx("CADCHF"),
    jpy("CHFJPY"),
    jpy("NZDJPY"),
    most_fx("NZDCHF"),
    most_fx("NZDCAD"),
];

/// The scale row for `instrument` (exact, upper-case match), or `None` when the archive lane does
/// not admit it — which an import must refuse at PLAN time, before any file is opened, with
/// [`unscaled_instrument_refusal`]'s text.
pub fn bi5_price_scale(instrument: &str) -> Option<&'static Bi5PriceScale> {
    PRICE_SCALES.iter().find(|row| row.instrument == instrument)
}

/// The refusal for an instrument [`bi5_price_scale`] has no row for. It quotes the vendor's own
/// warning, because that warning is the whole reason a missing row is a refusal rather than a
/// default.
pub fn unscaled_instrument_refusal(instrument: &str) -> String {
    format!(
        "{instrument}: no measured price scale, so it is not imported. The vendor's own warning: \
         \"silently applying 100,000 to a non-FX instrument will produce wrong prices with no \
         error raised\". Only G10 FX pairs are admitted until a scale is measured."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::bundled_instruments;
    use crate::data::point_divisor;

    const G10: [&str; 8] = ["USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "NZD"];

    /// The whole table, verbatim — a row added, removed or re-scaled is a reviewed edit HERE.
    #[test]
    fn the_scale_table_is_pinned_verbatim() {
        let rows: Vec<(&str, u32, &str)> =
            PRICE_SCALES.iter().map(|r| (r.instrument(), r.point_value(), r.evidence())).collect();
        let fx = "vendor page, \"most FX pairs\"";
        let jp = "vendor page, the JPY-pair list";
        assert_eq!(
            rows,
            [
                ("EURUSD", 100_000, fx),
                ("USDJPY", 1_000, jp),
                ("GBPUSD", 100_000, fx),
                ("USDCHF", 100_000, fx),
                ("USDCAD", 100_000, fx),
                ("AUDUSD", 100_000, fx),
                ("NZDUSD", 100_000, fx),
                ("EURJPY", 1_000, jp),
                ("EURGBP", 100_000, fx),
                ("EURCHF", 100_000, fx),
                ("EURAUD", 100_000, fx),
                ("EURCAD", 100_000, fx),
                ("EURNZD", 100_000, fx),
                ("GBPJPY", 1_000, jp),
                ("GBPCHF", 100_000, fx),
                ("GBPAUD", 100_000, fx),
                ("GBPCAD", 100_000, fx),
                ("GBPNZD", 100_000, fx),
                ("AUDJPY", 1_000, jp),
                ("AUDCHF", 100_000, fx),
                ("AUDCAD", 100_000, fx),
                ("AUDNZD", 100_000, fx),
                ("CADJPY", 1_000, jp),
                ("CADCHF", 100_000, fx),
                ("CHFJPY", 1_000, jp),
                ("NZDJPY", 1_000, jp),
                ("NZDCHF", 100_000, fx),
                ("NZDCAD", 100_000, fx),
            ]
        );
    }

    /// The 1,000 rows are EXACTLY the vendor's JPY list — no more, no fewer.
    #[test]
    fn the_thousand_rows_are_exactly_the_vendors_jpy_list() {
        let mut thousand: Vec<&str> = PRICE_SCALES
            .iter()
            .filter(|r| r.point_value() == 1_000)
            .map(|r| r.instrument())
            .collect();
        thousand.sort_unstable();
        let mut vendor = ["USDJPY", "EURJPY", "GBPJPY", "AUDJPY", "NZDJPY", "CADJPY", "CHFJPY"];
        vendor.sort_unstable();
        assert_eq!(thousand, vendor);
        assert!(
            PRICE_SCALES.iter().all(|r| r.point_value() == 1_000 || r.point_value() == 100_000),
            "the vendor's FX rule knows two point values and no third"
        );
    }

    #[test]
    fn an_instrument_with_no_row_gets_none() {
        assert_eq!(bi5_price_scale("US30"), None);
        assert_eq!(bi5_price_scale(""), None);
        // exact match: the dataset validator keeps names upper-case, and a lower-case spelling is a
        // second series (the design's F5), not a second name for this one
        assert_eq!(bi5_price_scale("eurusd"), None);
        assert_eq!(bi5_price_scale("EURUSD").map(|r| r.point_value()), Some(100_000));
        assert_eq!(bi5_price_scale("USDJPY").map(|r| r.point_value()), Some(1_000));
    }

    /// The two instruments the design names: a metal the vendor says to verify per instrument, and a
    /// HUF cross where 100,000 is doubtful. Both are in `bundled_instruments`, so the HTTP lane
    /// scales them today; this lane does not.
    #[test]
    fn xauusd_and_eurhuf_have_no_row() {
        let bundled: Vec<String> =
            bundled_instruments().into_iter().map(|i| i.raw_symbol).collect();
        for name in ["XAUUSD", "EURHUF"] {
            assert!(bundled.iter().any(|b| b == name), "{name} is a bundled instrument");
            assert_eq!(bi5_price_scale(name), None, "{name} must wait for a measurement");
        }
    }

    /// Completeness against the catalog, both ways: every bundled pair whose two legs are G10 has a
    /// row, no other bundled instrument has one, and no row names an instrument the catalog lacks.
    #[test]
    fn every_g10_pair_of_the_catalog_has_a_row_and_no_other_instrument_does() {
        let bundled = bundled_instruments();
        for inst in &bundled {
            let g10 = G10.contains(&inst.base.as_str()) && G10.contains(&inst.quote.as_str());
            assert_eq!(
                bi5_price_scale(&inst.raw_symbol).is_some(),
                g10,
                "{}: a row exists iff both legs are G10 ({}/{})",
                inst.raw_symbol,
                inst.base,
                inst.quote
            );
        }
        for row in PRICE_SCALES {
            assert!(
                bundled.iter().any(|i| i.raw_symbol == row.instrument()),
                "{} has a row but is not a bundled instrument",
                row.instrument()
            );
        }
        let names: std::collections::BTreeSet<&str> =
            PRICE_SCALES.iter().map(|r| r.instrument()).collect();
        assert_eq!(names.len(), PRICE_SCALES.len(), "no instrument has two rows");
    }

    /// On every admitted row the archive scale is the HTTP lane's own divisor, bit for bit — so an
    /// identical record decodes to an identical price whichever lane read it.
    #[test]
    fn every_row_agrees_with_the_http_lanes_divisor() {
        for row in PRICE_SCALES {
            assert_eq!(
                f64::from(row.point_value()).to_bits(),
                point_divisor(row.instrument()).to_bits(),
                "{}",
                row.instrument()
            );
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
