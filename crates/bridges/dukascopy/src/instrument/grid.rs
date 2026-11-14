//! **The account's instrument list, as the JForex sidecar states it at its login**: the `instruments` line
//! (`crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/InstrumentList.java`), one [`JforexInstrument`] per
//! instrument `IClient.getAvailableInstruments()` returns, with the figures the SDK holds for it from the login's own
//! properties (the server's pip, which overrides the SDK's predefined one; the venue's group; its amount per contract).
//! The sidecar writes it just before its `ready`. Nothing here is compiled in (owner 2026-11-13, "remove all hardcoded
//! symbols"): the exec reader keeps the latest
//! statement on the process's bus (`crate::feed::QuoteBus::instrument_list`), every reader of a grid, a class or a
//! conversion pair takes it from there, and the gatewayhub's catalog hands the same rows to the datahub, which keeps them
//! as `<project>/instruments/dukascopy.json` (`crate::catalog`).
//!
//! **What a row is, and is not.**
//! - `pip_value`, `pip_scale` and `tick_scale` are the login's (the SDK's tick scale is its pip scale plus one, for every
//!   instrument): [`JforexInstrument::tick_size`] is the price grid.
//! - The SDK's minimum, increment and maximum at login are its CLASS constants, not the venue's, so the line carries none:
//!   a metal's or a CFD's sizes are its `terms` line, read once it is subscribed (`crate::instrument::terms`), and a
//!   currency pair's are the sidecar's own floor (`crate::instrument::any_symbol::SIDECAR_MIN_UNITS`).
//! - [`JforexInstrument::point_value`] is the point value of the instrument's `.bi5` history: `10^tick_scale`, measured
//!   on one decoded hour per class (the measurement is `crates/bridges/dukascopy/tests/bi5_agrees_with_the_bridge.rs`'s
//!   live probe; no source states it).
//! - The list is the ACCOUNT's (the demo account's login of 2026-11-13 listed 905), not the public catalogue: a name it
//!   lacks is refused at the door in words, like an unknown symbol.

use serde::{Deserialize, Serialize};

/// The SDK's `IFinancialInstrument.Type`, as the line spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum InstrumentType {
    Forex,
    Metal,
    Cfd,
}

/// The venue's group of the coins (`IInstrumentGroup.CRYPTO_CURR`): a coin is refused at the door whatever the SDK
/// types it (`crate::instrument::any_symbol`).
pub const GROUP_COIN: &str = "CRYPTO_CURR";

/// One instrument of the account's list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JforexInstrument {
    /// The wire spelling (`OrderMap.canonicalSymbol`): six letters for a currency or metal pair (`EURUSD`), the SDK's
    /// slashed name for every other instrument (`COPPER.CMD/USD`).
    pub symbol: String,
    /// The SDK's slashed name, `Instrument.toString()`: `EUR/USD`, `USA500.IDX/USD`.
    pub pair: String,
    /// The SDK's type.
    #[serde(rename = "type")]
    pub kind: InstrumentType,
    /// The venue's group (`IInstrumentGroup.getName()`: `FX_MAJOR`, `FX_CROSS`, `FX_METAL`, `CRYPTO_CURR`, `STK_CASH`,
    /// `ETF`, `IDX_CASH`, `COM_SPOT`, `BONDS`); empty when the SDK states none.
    #[serde(default)]
    pub group: String,
    /// One pip in price units, the login's (`getPipValue`).
    pub pip_value: f64,
    /// `getPipScale`.
    pub pip_scale: u32,
    /// `getTickScale`: the price step is `10^-tick_scale`.
    pub tick_scale: u32,
    /// How much of the underlying one contract is, the login's `commoditiesPerContract` (`getAmountPerContract`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_contract: Option<f64>,
    /// The leverage the venue allows on it (`getLeverageUse`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leverage: Option<f64>,
    /// The instrument's country (`getCountry`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// The venue's description (`getDescription`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl JforexInstrument {
    /// The price step, `10^-tick_scale` (`0.00001` for EUR/USD).
    pub fn tick_size(&self) -> f64 {
        // the literal "1e-5" parses to the nearest f64, the same value as the source literal 0.00001
        format!("1e-{}", self.tick_scale).parse().unwrap_or(f64::NAN)
    }

    /// The point value of the instrument's `.bi5` prices: a record's integer price divided by it is the price,
    /// `10^tick_scale` (100,000 for EUR/USD, 1,000 for USD/JPY and XAU/USD). See the module doc for how it was measured.
    pub fn point_value(&self) -> f64 {
        format!("1e{}", self.tick_scale).parse().unwrap_or(f64::NAN)
    }

    /// The name the keyless `.bi5` datafeed files the instrument under: the SDK's name with no slash and no dot
    /// (`EURUSD`, `USA500IDXUSD`), which is also the SDK's enum-style name of a CFD.
    pub fn history_name(&self) -> String {
        self.pair.replace(['/', '.'], "")
    }

    /// The currency the price is quoted in, read off the SDK's name (`USD` for `EUR/USD`, `GBX` for `LSE.GB/GBX`).
    pub fn quote(&self) -> &str {
        self.pair.rsplit('/').next().unwrap_or_default()
    }

    /// Whether the venue lists it as a coin ([`GROUP_COIN`]).
    pub fn is_coin(&self) -> bool {
        self.group == GROUP_COIN
    }
}

/// Why there is no list to read (no sidecar has stated one in this process: no dukascopy account has logged in since this
/// gatewayhub started, or its jar predates the list), as the one line that says what to do (`vike_bi5::NO_INSTRUMENT_LIST`).
#[must_use]
pub fn not_listed() -> String {
    format!("dukascopy: {}", vike_bi5::NO_INSTRUMENT_LIST)
}

/// **The point value of `symbol`'s `.bi5` history** (the datafeed's name, `EURUSD` or `USA500IDXUSD`, any case), from
/// its row of the account's list, or the sentence for why there is none. Nothing is assumed for a name the list lacks:
/// the vendor warns that a scale applied to the wrong instrument gives wrong prices with no error raised.
///
/// # Errors
/// The name is not on the list.
pub fn history_point_value(rows: &[JforexInstrument], symbol: &str) -> Result<f64, String> {
    let name = symbol.trim().to_ascii_uppercase();
    rows.iter().find(|r| r.history_name() == name).map(JforexInstrument::point_value).ok_or_else(
        || {
            format!(
                "dukascopy: `{name}` is not on the account's instrument list ({} instruments, as the JForex sidecar \
                 stated them), so the point value of its .bi5 prices is not known and its history is not decoded",
                rows.len()
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::listed;

    #[test]
    fn the_line_shape_parses_and_round_trips() {
        let line = r#"{"symbol":"COPPER.CMD/USD","pair":"COPPER.CMD/USD","type":"CFD","group":"COM_SPOT","pip_value":0.001,"pip_scale":3,"tick_scale":4,"per_contract":1.0,"leverage":10.0,"country":"US","description":"Copper","future":1}"#;
        let row: JforexInstrument = serde_json::from_str(line).expect("a row");
        assert_eq!(
            (row.kind, row.group.as_str(), row.tick_size(), row.point_value()),
            (InstrumentType::Cfd, "COM_SPOT", 0.0001, 10_000.0)
        );
        assert_eq!((row.history_name().as_str(), row.quote()), ("COPPERCMDUSD", "USD"));
        let back: JforexInstrument =
            serde_json::from_str(&serde_json::to_string(&row).expect("encodes")).expect("parses");
        assert_eq!(back, row);
        // only the figures every instrument has are required: a row with no group, contract or text still reads
        let bare = r#"{"symbol":"EURUSD","pair":"EUR/USD","type":"FOREX","pip_value":0.0001,"pip_scale":4,"tick_scale":5}"#;
        let bare: JforexInstrument = serde_json::from_str(bare).expect("a bare row");
        assert_eq!((bare.group.as_str(), bare.per_contract, bare.is_coin()), ("", None, false));
    }

    /// Every key this reader takes is one the Java writes (`InstrumentList.java`, read at run time: the public mirror ships
    /// no `jforex-bridge/` sources a test may `include_str!`), and a checkout without it says so.
    #[test]
    fn every_key_the_reader_takes_is_one_the_java_writes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("jforex-bridge/src/main/java/vike/jforex/InstrumentList.java");
        let Ok(java) = std::fs::read_to_string(&path) else {
            eprintln!("SKIPPED: {} is not in this checkout", path.display());
            return;
        };
        for key in [
            "instruments",
            "symbol",
            "pair",
            "type",
            "group",
            "pip_value",
            "pip_scale",
            "tick_scale",
            "per_contract",
            "leverage",
            "country",
            "description",
        ] {
            assert!(
                java.contains(&format!("\"{key}\"")),
                "{} never writes `{key}`",
                path.display()
            );
        }
    }

    #[test]
    fn a_row_states_a_consistent_grid() {
        for r in listed().iter() {
            assert_eq!(
                r.tick_scale,
                r.pip_scale + 1,
                "{}: the SDK derives the tick scale from the pip",
                r.pair
            );
            let pip: f64 = format!("1e-{}", r.pip_scale).parse().unwrap_or(f64::NAN);
            assert_eq!(r.pip_value, pip, "{}: pip_value is 10^-pip_scale", r.pair);
            assert!((r.point_value() * r.tick_size() - 1.0).abs() < 1e-12, "{}", r.pair);
        }
    }

    #[test]
    fn spot_values_of_the_recorded_list() {
        let rows = listed();
        let t = |s: &str| {
            rows.iter()
                .find(|i| i.symbol == s)
                .map(|i| (i.tick_size(), i.pip_value, i.point_value()))
        };
        assert_eq!(t("EURUSD"), Some((0.00001, 0.0001, 100_000.0)));
        assert_eq!(t("USDJPY"), Some((0.001, 0.01, 1_000.0)));
        assert_eq!(t("XAUUSD"), Some((0.001, 0.01, 1_000.0)));
        assert_eq!(t("EURHUF"), Some((0.001, 0.01, 1_000.0)));
        assert_eq!(t("EURCZK"), Some((0.0001, 0.001, 10_000.0)));
        // CORRECTED: the committed SDK table had EUR/THB at pip 0.001 (tick 0.0001); the server states 0.01, and the SDK
        // overrode it at the login of 2026-11-13 ("Received pip value is different than predefined").
        assert_eq!(t("EURTHB"), Some((0.001, 0.01, 1_000.0)));
        assert_eq!(t("GBPNOK"), None, "not on the recorded list");
    }

    #[test]
    fn the_history_point_value_is_the_rows_and_a_name_off_the_list_is_refused() {
        let rows = listed();
        assert_eq!(history_point_value(&rows, "EURUSD"), Ok(100_000.0));
        assert_eq!(history_point_value(&rows, "usdjpy"), Ok(1_000.0));
        assert_eq!(history_point_value(&rows, "USA500IDXUSD"), Ok(1_000.0));
        assert_eq!(history_point_value(&rows, "COPPERCMDUSD"), Ok(10_000.0));
        let why = history_point_value(&rows, "GBPNOK").expect_err("off the list");
        assert!(why.contains("`GBPNOK` is not on the account's instrument list"), "{why}");
        assert!(history_point_value(&[], "EURUSD").is_err(), "no list, no scale");
    }
}
