//! **The venue's OWN trade terms of one instrument**, as the JForex sidecar states them on its `terms` line
//! (`crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/InstrumentTerms.java`), and nothing else.
//!
//! Why a line and not the committed SDK table (`crate::jforex_grid`): the table's `default_*` columns and a CFD's pip
//! are the SDK's own DEFAULTS, which its source says it "overrides in runtime after instrument subscription". A metal
//! or a CFD is therefore admitted only on figures the venue stated for the instrument, read once the instrument is
//! subscribed (the first tick, which is where the sidecar writes the line). [`crate::quotes::QuoteBus::venue_terms`]
//! asks for them and waits; [`crate::any_symbol::facts_from_terms`] turns them into the grid the door answers, or the
//! sentence that refuses them. Every field is OPTIONAL on the wire: a figure the SDK did not state (or stated as a
//! number that is not one) is absent, and the reading refuses an instrument whose terms are not whole.
//!
//! Additive both ways, like `tick`: a jar that does not announce `terms` (`crate::quotes::FEATURE_TERMS`) never sends the
//! line, and an old reader logs-and-skips the unknown kind.

use serde::{Deserialize, Serialize};

/// One `terms` envelope. Amounts are in UNITS of the instrument's own amount (the unit of an order's `qty`: ounces for
/// a metal, contracts for a CFD), not JForex millions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueTerms {
    /// The wire spelling (`XAUUSD`, `COPPER.CMD/USD`): what every fill and tick of the instrument is stamped with.
    pub symbol: String,
    /// The SDK's `IFinancialInstrument.Type`: `FOREX`, `METAL` or `CFD`.
    #[serde(rename = "type", default)]
    pub class: String,
    /// The smallest order, `getMinTradeAmount`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// The amount increment, `getTradeAmountIncrement`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// The largest order, `getMaxTradeAmount`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// `getAmountPerContract`: how much of the underlying one contract is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_contract: Option<f64>,
    /// `getPipValue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pip: Option<f64>,
    /// `getPipScale`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pip_scale: Option<u32>,
    /// `getTickScale` (the price grid is `10^-tick_scale`; the SDK's tick scale is its pip scale plus one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_scale: Option<u32>,
    /// The currency the price is quoted in (`getSecondaryJFCurrency`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{Envelope, encode_line, parse_envelope};

    /// Exactly what the Java emitter writes (`InstrumentTerms.java`, Gson property order, a doubled figure printed as
    /// Java prints it: `5.0E8`), parsed; and the round trip. `type` is the wire name of `class`.
    #[test]
    fn the_terms_line_the_java_writes_parses_and_round_trips() {
        let java = r#"{"kind":"terms","symbol":"COPPER.CMD/USD","type":"CFD","min":20.0,"step":1.0,"max":5.0E8,"per_contract":1.0,"pip":0.001,"pip_scale":3,"tick_scale":4,"quote":"USD"}"#;
        let terms = VenueTerms {
            symbol: "COPPER.CMD/USD".into(),
            class: "CFD".into(),
            min: Some(20.0),
            step: Some(1.0),
            max: Some(5.0e8),
            per_contract: Some(1.0),
            pip: Some(0.001),
            pip_scale: Some(3),
            tick_scale: Some(4),
            quote: Some("USD".into()),
        };
        assert_eq!(parse_envelope(java), Some(Envelope::Terms(terms.clone())));
        let line = encode_line(&Envelope::Terms(terms.clone()));
        assert!(
            line.starts_with(r#"{"kind":"terms","symbol":"COPPER.CMD/USD","type":"CFD""#),
            "{line}"
        );
        assert_eq!(parse_envelope(&line), Some(Envelope::Terms(terms)));
    }

    /// A figure the jar could not state is ABSENT (never `NaN`: `InstrumentTerms.java` omits it), and reads as `None`;
    /// a key this reader does not know is ignored (a newer jar, an older reader).
    #[test]
    fn an_absent_figure_is_none_and_an_unknown_key_is_ignored() {
        let line = r#"{"kind":"terms","symbol":"XAUUSD","type":"METAL","step":1.0,"pip_scale":2,"tick_scale":3,"future":{"x":1}}"#;
        match parse_envelope(line) {
            Some(Envelope::Terms(t)) => {
                assert_eq!(
                    (t.min, t.max, t.per_contract, t.pip, t.quote),
                    (None, None, None, None, None)
                );
                assert_eq!((t.step, t.pip_scale, t.tick_scale), (Some(1.0), Some(2), Some(3)));
            }
            other => panic!("{other:?}"),
        }
        // a line with no `symbol` is not a terms line at all
        assert_eq!(parse_envelope(r#"{"kind":"terms","type":"CFD"}"#), None);
    }
}
