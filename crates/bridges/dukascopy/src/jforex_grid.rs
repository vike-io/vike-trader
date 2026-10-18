//! The JForex SDK's own price-grid constants for the instruments it predefines, as a committed
//! table: `crates/bridges/dukascopy/data/jforex_instruments.tsv`.
//!
//! **Where it comes from.** `crates/bridges/dukascopy/jforex-bridge/src/test/java/vike/jforex/InstrumentTableTest.java`
//! renders `com.dukascopy.api.Instrument.values()` from the SDK jar and fails when the committed file
//! differs (CI's `jforex-bridge.yml`, which already resolves that jar for the sidecar build). The file
//! is never hand-edited. Rust reads it with `include_str!`; no test here touches a network or Java.
//!
//! **What it is, and is not.**
//! - `pip_value`, `pip_scale`, `tick_scale` are constructor constants in the SDK (tick scale = pip
//!   scale + 1): [`JforexInstrument::tick_size`] is the instrument's price grid, offline. Only rows
//!   whose [`GridBasis`] is `SdkConstant` are such a statement; a `CfdPlaceholder` row is the SDK's
//!   blanket default for a CFD it builds without a pip (0.01 / 2), not a measured grid.
//! - the `default_*` columns are the SDK's declared DEFAULTS, which its own source says are
//!   "overridden in runtime after instrument subscription". They are NOT venue facts and nothing
//!   here may feed an order's size from them: `default_min_amount` 1000 on a `FOREX` row is in UNITS
//!   (the sidecar's `OrderMap.MIN_UNITS` is the same 1000; the JForex order API takes millions, so
//!   0.001), but the metals and CFDs the venue really trades carry other minimums that only a logged
//!   in session reports (the vendor wiki gives XAG 50 oz against the SDK default 1).
//! - it holds the ~180 predefined constants only. The venue lists on the order of ~700; every other
//!   instrument is built at run time by `Instrument.fromString` and has no row.
//!
//! This module changes no behaviour: `crate::catalog` and `crate::data::point_divisor` still decide
//! what they decided. The tests below pin where they AGREE with the SDK and the four symbols where
//! they do not.

use std::sync::OnceLock;

/// The SDK's `IFinancialInstrument.Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JforexKind {
    Forex,
    Metal,
    Cfd,
}

/// Whether a row's pip/tick is a constant the SDK states for that instrument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridBasis {
    /// Stated per instrument in the SDK source.
    SdkConstant,
    /// The SDK's blanket CFD default (pip 0.01, scale 2): a placeholder, not a grid.
    CfdPlaceholder,
}

/// One predefined SDK instrument.
#[derive(Clone, Debug, PartialEq)]
pub struct JforexInstrument {
    /// Enum constant name with the slash removed: `"EURUSD"`, `"USA500IDXUSD"`.
    pub symbol: String,
    /// The SDK's slashed spelling: `"EUR/USD"`, `"USA500.IDX/USD"`.
    pub pair: String,
    pub kind: JforexKind,
    pub pip_value: f64,
    pub pip_scale: u32,
    pub tick_scale: u32,
    /// SDK default minimum amount (units for FOREX), a run-time-overridden default: see the module doc.
    pub default_min_amount: f64,
    /// SDK default amount increment, same caveat.
    pub default_amount_increment: f64,
    pub basis: GridBasis,
}

impl JforexInstrument {
    /// The price step, `10^-tick_scale` (`0.00001` for EUR/USD).
    pub fn tick_size(&self) -> f64 {
        // the literal "1e-5" parses to the nearest f64, the same value as the source literal 0.00001
        format!("1e-{}", self.tick_scale).parse().unwrap_or(f64::NAN)
    }
}

const TABLE: &str = include_str!("../data/jforex_instruments.tsv");

/// The columns, in order; the parser refuses a file whose header differs.
const HEADER: &str = "symbol\tpair\ttype\tpip_value\tpip_scale\ttick_scale\tdefault_min_amount\t\
default_amount_increment\tdefault_max_amount\tdefault_amount_per_contract\tgrid_basis";

fn parse(text: &str) -> Result<Vec<JforexInstrument>, String> {
    let mut lines = text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty());
    if lines.next() != Some(HEADER) {
        return Err("jforex_instruments.tsv: header row differs from the parser's".into());
    }
    lines
        .map(|line| {
            let c: Vec<&str> = line.split('\t').collect();
            if c.len() != 11 {
                return Err(format!("jforex_instruments.tsv: {} columns in `{line}`", c.len()));
            }
            let bad = |what: &str| format!("jforex_instruments.tsv: bad {what} in `{line}`");
            Ok(JforexInstrument {
                symbol: c[0].to_owned(),
                pair: c[1].to_owned(),
                kind: match c[2] {
                    "FOREX" => JforexKind::Forex,
                    "METAL" => JforexKind::Metal,
                    "CFD" => JforexKind::Cfd,
                    _ => return Err(bad("type")),
                },
                pip_value: c[3].parse().map_err(|_| bad("pip_value"))?,
                pip_scale: c[4].parse().map_err(|_| bad("pip_scale"))?,
                tick_scale: c[5].parse().map_err(|_| bad("tick_scale"))?,
                default_min_amount: c[6].parse().map_err(|_| bad("default_min_amount"))?,
                default_amount_increment: c[7]
                    .parse()
                    .map_err(|_| bad("default_amount_increment"))?,
                basis: match c[10] {
                    "sdk-constant" => GridBasis::SdkConstant,
                    "cfd-placeholder" => GridBasis::CfdPlaceholder,
                    _ => return Err(bad("grid_basis")),
                },
            })
        })
        .collect()
}

/// Every predefined SDK instrument, sorted by `symbol`. Empty only if the committed file is
/// malformed, which the tests below refuse.
pub fn jforex_instruments() -> &'static [JforexInstrument] {
    static ROWS: OnceLock<Vec<JforexInstrument>> = OnceLock::new();
    ROWS.get_or_init(|| parse(TABLE).unwrap_or_default())
}

/// The row of `symbol` (upper-case, no slash: `"EURUSD"`), if the SDK predefines it.
pub fn jforex_instrument(symbol: &str) -> Option<&'static JforexInstrument> {
    jforex_instruments().iter().find(|i| i.symbol == symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::bundled_instruments;
    use crate::data::point_divisor;

    #[test]
    fn the_committed_file_parses_and_has_the_pinned_shape() {
        assert_eq!(
            parse(TABLE).map(|r| r.len()),
            Ok(180),
            "a row added or lost is a reviewed edit"
        );
        let rows = jforex_instruments();
        assert!(rows.windows(2).all(|w| w[0].symbol < w[1].symbol), "sorted, no duplicate");
        let count = |k| rows.iter().filter(|r| r.kind == k).count();
        assert_eq!(
            (count(JforexKind::Forex), count(JforexKind::Metal), count(JforexKind::Cfd)),
            (72, 2, 106)
        );
        assert_eq!(rows.iter().filter(|r| r.basis == GridBasis::CfdPlaceholder).count(), 103);
    }

    #[test]
    fn a_row_states_a_consistent_grid() {
        for r in jforex_instruments() {
            assert_eq!(
                r.tick_scale,
                r.pip_scale + 1,
                "{}: the SDK derives the tick scale from the pip",
                r.symbol
            );
            let pip: f64 = format!("1e-{}", r.pip_scale).parse().unwrap_or(f64::NAN);
            assert_eq!(r.pip_value, pip, "{}: pip_value is 10^-pip_scale", r.symbol);
            assert_eq!(
                r.pair.replace(['/', '.'], ""),
                r.symbol,
                "{}: the symbol is the pair, unslashed",
                r.symbol
            );
            if r.kind == JforexKind::Forex {
                assert_eq!(
                    r.default_min_amount, 1000.0,
                    "{}: the FOREX default is 1000 units",
                    r.symbol
                );
            }
        }
    }

    #[test]
    fn spot_values_read_from_the_sdk_source() {
        let t = |s: &str| jforex_instrument(s).map(|i| (i.tick_size(), i.pip_value));
        assert_eq!(t("EURUSD"), Some((0.00001, 0.0001)));
        assert_eq!(t("USDJPY"), Some((0.001, 0.01)));
        assert_eq!(t("XAUUSD"), Some((0.001, 0.01)));
        assert_eq!(t("EURHUF"), Some((0.001, 0.01)));
        assert_eq!(t("EURCZK"), Some((0.0001, 0.001)));
        assert_eq!(t("GBPNOK"), None, "not predefined: a run-time instrument");
    }

    /// The bundled catalog and the HTTP lane's suffix rule against the SDK. Agreement is pinned for
    /// every shared FX row but the ones listed; the listed ones are a FINDING (the SDK says the
    /// price has fewer decimals than `point_divisor` assumes), not a change made here.
    #[test]
    fn the_catalog_agrees_with_the_sdk_except_where_pinned() {
        let mut absent = Vec::new();
        let mut scale_differs = Vec::new();
        for i in bundled_instruments() {
            let Some(row) = jforex_instrument(&i.raw_symbol) else {
                absent.push(i.raw_symbol);
                continue;
            };
            if 10f64.powi(row.tick_scale as i32) != point_divisor(&i.raw_symbol) {
                scale_differs.push(i.raw_symbol.clone());
            }
            if let vike_model::Contract::Fx(fx) = &i.contract
                && let Some(pip) = fx.pip_size
            {
                assert_eq!(pip, row.pip_value, "{}: the catalog pip", i.raw_symbol);
            }
        }
        absent.sort();
        scale_differs.sort();
        assert_eq!(absent, ["GBPNOK", "GBPSEK"], "catalog rows the SDK does not predefine");
        assert_eq!(
            scale_differs,
            ["EURCZK", "EURHUF", "XAGUSD", "XAUUSD"],
            "tick decimals: SDK 10^tick_scale against data.rs's point_divisor"
        );
    }
}
