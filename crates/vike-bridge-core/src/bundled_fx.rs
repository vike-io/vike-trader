//! The typed rows of a hand-curated, BUNDLED FX + metals instrument table: dukascopy's and fxcm's
//! catalogs, neither of which has an instrument-list endpoint to fetch from.
//!
//! Each venue keeps its own table, its symbol spelling (`EURUSD` / `EUR/USD`), its base/quote split
//! and its pip rule; the row kinds and the `Contract` a row builds are written once here (decision
//! 0126: every instrument class is a typed contract). A field no bundled table carries is `None`:
//! lot size and swaps for an FX pair, margin rate and swaps for a metal.

use vike_model::{CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, Contract, FxSpec};

/// What a bundled-table row is: the hand-curated fact the table asserts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A spot FX pair.
    Fx,
    /// A spot precious-metal CFD (XAU, XAG).
    Metal,
}

/// The typed `Contract` a row of `kind` describes. `pip_size` is the venue's pip for an FX row
/// (`None` where the venue states none) and is not read for a metal, which is a rolling CFD (a spot
/// metal CFD rolls; it has no expiry date).
#[must_use]
pub fn contract_for(kind: RowKind, pip_size: Option<f64>) -> Contract {
    match kind {
        RowKind::Fx => {
            Contract::Fx(FxSpec { pip_size, lot_size: None, swap_long: None, swap_short: None })
        }
        RowKind::Metal => Contract::Cfd(CfdSpec {
            underlying_kind: CfdUnderlying::Metal,
            underlying: None,
            product: CfdProduct::Cfd,
            expiry: CfdExpiry::Rolling,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        }),
    }
}

/// The instrument `raw_symbol` names in `list`; panics when it is absent.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn find(list: Vec<vike_model::Instrument>, raw_symbol: &str) -> vike_model::Instrument {
    list.into_iter()
        .find(|i| i.raw_symbol == raw_symbol)
        .unwrap_or_else(|| panic!("{raw_symbol} present"))
}

/// A bundled list of `table_len` rows: as many instruments, `fx` FX pairs and `metals` CFDs.
#[cfg(any(test, feature = "test-support"))]
pub fn assert_counts(list: &[vike_model::Instrument], table_len: usize, fx: usize, metals: usize) {
    use vike_model::AssetClass;
    assert_eq!(list.len(), table_len);
    assert_eq!(list.len(), fx + metals, "{fx} FX pairs + {metals} metals");
    assert_eq!(list.iter().filter(|i| i.asset_class() == AssetClass::Fx).count(), fx);
    assert_eq!(list.iter().filter(|i| i.asset_class() == AssetClass::Cfd).count(), metals);
}

/// `inst` is `venue`'s FX pair `base`/`quote` with pip `pip`, described `base/quote`.
#[cfg(any(test, feature = "test-support"))]
pub fn assert_fx_pair(
    inst: &vike_model::Instrument,
    venue: &str,
    base: &str,
    quote: &str,
    pip: Option<f64>,
) {
    assert_eq!(inst.asset_class(), vike_model::AssetClass::Fx);
    assert_eq!(
        inst.contract,
        Contract::Fx(FxSpec { pip_size: pip, lot_size: None, swap_long: None, swap_short: None })
    );
    assert_eq!(inst.base, base);
    assert_eq!(inst.quote, quote);
    assert_eq!(inst.description, format!("{base}/{quote}"));
    assert_eq!(inst.venue, venue);
}

/// `inst` is a rolling spot-metal CFD, checked against the spec literal (not [`contract_for`], so
/// the pin cannot agree with a defect in the builder).
#[cfg(any(test, feature = "test-support"))]
pub fn assert_rolling_metal(inst: &vike_model::Instrument) {
    assert_eq!(inst.asset_class(), vike_model::AssetClass::Cfd);
    assert_eq!(
        inst.contract,
        Contract::Cfd(CfdSpec {
            underlying_kind: CfdUnderlying::Metal,
            underlying: None,
            product: CfdProduct::Cfd,
            expiry: CfdExpiry::Rolling,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        }),
        "{}",
        inst.raw_symbol
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin of dukascopy's and fxcm's former `contract_for` copies: an FX row carries exactly the
    /// pip it is handed (dukascopy's `None` for a doubtful scale included) and nothing else; a metal
    /// is the rolling CFD whatever pip arrives. Red on a mutant that drops the pip, fills a lot size
    /// or gives the metal an expiry.
    #[test]
    fn a_row_builds_the_contract_its_kind_names() {
        for pip in [Some(0.0001), Some(0.01), None] {
            assert_eq!(
                contract_for(RowKind::Fx, pip),
                Contract::Fx(FxSpec {
                    pip_size: pip,
                    lot_size: None,
                    swap_long: None,
                    swap_short: None
                })
            );
        }
        let metal = vike_model::Instrument::new(
            "v",
            "XAUUSD",
            "XAU",
            "USD",
            contract_for(RowKind::Metal, Some(0.01)),
        );
        assert_rolling_metal(&metal);
    }
}
