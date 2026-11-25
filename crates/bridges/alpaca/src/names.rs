//! `names` — **alpaca's USER name ↔ WIRE name, read off the KEPT row** (decision 0290; audit flags ALP-1, ALP-2,
//! ALP-3).
//!
//! The engine, the journal, the fills, the reconcile reports and the market-data sink speak the USER name: a share is
//! its ticker (`AAPL`, `BRK.B`), a coin base + quote (`BTCUSD`), so a coin's stored series is `BTCUSD` too (a `/` is
//! no store path's). The venue's spelling (`BTC/USD`) exists only inside this bridge, and it is the kept row's own
//! (`vike_catalog::names_of_row`). Alpaca publishes no keyless list (decision 0062), so its kept list
//! (`<project>/instruments/alpaca.json`, `vike_catalog::kept_list`) is the operator's rows (decision 0290's O-10,
//! [`ADD_A_ROW`]). The ROW's class picks the market-data stream (`crate::feed`'s `class_of`): no name is read for a
//! class or a spelling, so a ticker that ends in `USD` is never taken for a coin. No list, no row or two rows
//! answering one name is a refusal naming the fix.
//!
//! The way back, the venue's `BTC/USD` to `BTCUSD` (a stream frame, a fill of an order this process did not submit,
//! an operator's read), is [`user_of`]: every name [`wire_of`] answered is remembered, so what this process sent maps
//! back without a list read.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use vike_model::AssetClass;

use crate::refusals::AlpacaNameRefusal;

const VENUE: &str = "alpaca";

/// The fix a refusal names: the operator's row of a share or a coin.
pub(crate) const ADD_A_ROW: &str = "alpaca publishes no instrument list, so its kept list is the operator's rows: add a share \
     with `vike-cli data instruments add --venue alpaca AAPL --asset-class equity --quote USD --tick 0.01 --point-scale \
     100`, a coin the same way under the venue's pair and `--asset-class crypto_spot` (`BTC/USD`)";

/// Every wire name [`wire_of`] answered in this process, to its user name.
fn answered() -> &'static Mutex<HashMap<String, String>> {
    static ANSWERED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    ANSWERED.get_or_init(Mutex::default)
}

/// **The venue's spelling of the USER name `user`, and its kept row's class**: `BTCUSD` is `("BTC/USD", CryptoSpot)`,
/// `AAPL` `("AAPL", Equity)`. What every order, position read, reconcile read, asset read and market-data subscription
/// sends.
///
/// # Errors
/// No alpaca list is kept in this process, none of its rows is `user`, or two are (an old spelling such as `BTC/USD`
/// is no user name): the translator's words and [`ADD_A_ROW`].
pub(crate) fn wire_of(user: &str) -> Result<(String, AssetClass), AlpacaNameRefusal> {
    let rows = vike_catalog::kept_list(VENUE);
    let rows = rows.as_deref().map_or(&[][..], Vec::as_slice);
    let row = vike_catalog::kept_row_of_user_name(rows, VENUE, user)
        .map_err(|why| AlpacaNameRefusal::NotKept { why })?;
    let names =
        vike_catalog::names_of_row(row).map_err(|why| AlpacaNameRefusal::RowUnnamed { why })?;
    answered()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(names.wire.clone(), names.user);
    Ok((names.wire, row.asset_class()))
}

/// **The USER name of the venue's spelling `wire`**: one [`wire_of`] answered, else the kept row whose wire name it
/// is. `None` when no kept row is that instrument: the caller keeps the venue's spelling.
pub(crate) fn user_of(wire: &str) -> Option<String> {
    if let Some(user) = answered().lock().unwrap_or_else(PoisonError::into_inner).get(wire) {
        return Some(user.clone());
    }
    let rows = vike_catalog::kept_list(VENUE)?;
    rows.iter()
        .filter(|row| row.venue == VENUE && vike_catalog::wire_name_of_row(row) == wire)
        .find_map(|row| vike_catalog::user_name_of_row(row).ok())
}

/// Hold this crate's test rows as alpaca's kept list: the shares `AAPL` and `MSFT`, the coins `BTC/USD` and `ETH/USD`.
/// Every test of this crate that reads a name holds the same rows, so the order the tests run in changes nothing.
#[cfg(test)]
pub(crate) fn hold_test_rows() {
    use vike_model::{Contract, ContractEquity, Instrument};
    let share = |ticker: &str| {
        Instrument::new(VENUE, ticker, ticker, "USD", Contract::Equity(ContractEquity::default()))
    };
    let coin = |base: &str| {
        Instrument::new(VENUE, format!("{base}/USD"), base, "USD", Contract::CryptoSpot)
    };
    vike_catalog::hold_kept_list(
        VENUE,
        vec![share("AAPL"), share("MSFT"), coin("BTC"), coin("ETH")],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A coin's user name is sent as the venue's pair and a share's as its ticker, each with its ROW's class; the way
    /// back answers the user name.
    #[test]
    fn a_user_name_is_sent_as_its_kept_rows_spelling_and_comes_back() {
        hold_test_rows();
        assert_eq!(wire_of("BTCUSD").unwrap(), ("BTC/USD".to_string(), AssetClass::CryptoSpot));
        assert_eq!(wire_of("btcusd").unwrap().0, "BTC/USD", "a typed name ignores case");
        assert_eq!(wire_of("AAPL").unwrap(), ("AAPL".to_string(), AssetClass::Equity));
        assert_eq!(user_of("BTC/USD").as_deref(), Some("BTCUSD"));
        assert_eq!(
            user_of("ETH/USD").as_deref(),
            Some("ETHUSD"),
            "a row never asked for, found by its wire"
        );
        assert_eq!(user_of("SOL/USD"), None, "no row: the caller keeps the venue's spelling");
    }

    /// No row is a refusal naming the fix, never a guess: a ticker ending in `USD` or `BTC` is no coin (ALP-3), and the
    /// venue's own pair is no user name (decision 0117).
    #[test]
    fn a_name_without_a_kept_row_is_refused_with_the_fix() {
        hold_test_rows();
        for name in ["GBTC", "SOLUSD", "BTC/USD"] {
            let why = wire_of(name).expect_err(name);
            assert_eq!(why.id(), crate::refusals::AlpacaNameRefusalId::NotKept, "{name}: {why}");
        }
    }
}
