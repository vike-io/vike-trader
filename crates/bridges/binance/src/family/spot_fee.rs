//! A SPOT fill's fee, stated in the QUOTE currency: the one place the family turns a venue's
//! `commission` + `commissionAsset` into the figure the core books.
//!
//! The core folds `FillEvent::commission` into `balance` and `fees_paid` AS STATED on every linear
//! contract, whatever `commission_asset` says (`vike_exec::Account`'s `commission_in_quote` converts an
//! INVERSE contract's coin fee and nothing else). A spot venue that charges the fee in the coin it
//! DELIVERS therefore has to be converted here, where the fill price is known. Binance spot takes the fee
//! from what the fill receives: a BUY pays in the BASE coin (`n` = 0.0000001 `N` = BTC on a BTCUSDT
//! BUY), a SELL in the quote (USDT), and with the BNB discount either in BNB. Booked raw, every BUY of
//! the demo spot account counted its fee as that many USDT, i.e. at 1/80,000 of its value (the BTC
//! price; measured on the CI box, v0.1.91).
//!
//! The rule, by the fee asset `N` against the exchange symbol (`BTCUSDT`):
//!
//! - `N` is the symbol's SUFFIX (the quote): the fee is already quote units, unchanged.
//! - `N` is the symbol's PREFIX (the base): `fee x fill price`, stated in the quote asset (the rest of
//!   the symbol), so `fees_by_asset` keeps a true quote tally too.
//! - Anything else (BNB on a pair that is not BNB's own): unchanged and WARNED. Its quote value needs a
//!   BNB/quote rate nothing in a pure mapper holds, so it still enters the ledger as BNB units
//!   (`commission_asset` stays `BNB`, which keeps `fees_by_asset` true in coin); the cure is the fee
//!   currency route of decision 0183 item 9, not a guess here.
//! - No fee asset stated (the frozen `fixtures/r6/mapper.json` frames): unchanged.
//!
//! ⚠ The base test is a prefix test, so a symbol whose base merely STARTS with the fee asset
//! (`BNBUPUSDT` against a BNB-discounted fee) reads as a base fee. Those leveraged-token pairs are
//! delisted; a venue that lists one again needs the instrument's base asset passed in instead.
//!
//! Shared by the live executionReport mapper, the history replay and the reconcile fill parser, so the
//! three legs book one fill identically (the engine keeps the FIRST delivery of a trade id for good).
//! Aster's spot lane runs the same code: it converts only a fee its own venue states in the base asset.

use std::sync::atomic::{AtomicU64, Ordering};

/// Fills met whose fee is in a third asset (BNB), for the warning schedule only.
static THIRD_ASSET_FEES: AtomicU64 = AtomicU64::new(0);

/// `(commission, commission_asset)` of one spot fill as the ledger must book it; see the module doc.
/// `symbol` is the exchange symbol (`BTCUSDT`), `price` the fill's own price.
pub fn spot_fee_in_quote(
    symbol: &str,
    fee: f64,
    asset: &str,
    price: f64,
    venue: &str,
) -> (f64, String) {
    let (sym, a) = (symbol.as_bytes(), asset.as_bytes());
    if a.is_empty() || sym.len() <= a.len() {
        return (fee, asset.to_string());
    }
    if sym[sym.len() - a.len()..].eq_ignore_ascii_case(a) {
        return (fee, asset.to_string());
    }
    if sym[..a.len()].eq_ignore_ascii_case(a) {
        let quote = symbol.get(a.len()..).unwrap_or_default().to_ascii_uppercase();
        return (fee * price, quote);
    }
    // A logarithmic schedule (1st, 2nd, 4th, 8th ...): a market maker pays this on every fill.
    let seen = THIRD_ASSET_FEES.fetch_add(1, Ordering::Relaxed) + 1;
    if seen.is_power_of_two() {
        tracing::warn!(
            venue,
            symbol,
            fee,
            fee_asset = asset,
            seen,
            "spot fee charged in an asset that is neither the symbol's base nor its quote (BNB \
             discount): booked as that many quote units, because no rate converts it here, so \
             `fees_paid` is understated by the asset's price"
        );
    }
    (fee, asset.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BTC fee on a BTCUSDT BUY: booked = fee x price, in USDT.
    #[test]
    fn a_base_coin_fee_is_booked_at_the_fill_price_in_the_quote() {
        let (c, a) = spot_fee_in_quote("BTCUSDT", 0.000_000_1, "BTC", 65_000.0, "binance");
        assert!((c - 0.0065).abs() < 1e-12, "{c}");
        assert_eq!(a, "USDT");
    }

    /// A quote fee (every SELL, and a BUY on a venue that charges the quote) is untouched.
    #[test]
    fn a_quote_fee_is_unchanged() {
        assert_eq!(
            spot_fee_in_quote("BTCUSDT", 0.0065, "USDT", 65_000.0, "binance"),
            (0.0065, "USDT".into())
        );
        assert_eq!(
            spot_fee_in_quote("ETHBTC", 0.000_01, "BTC", 0.05, "binance"),
            (0.000_01, "BTC".into())
        );
    }

    /// A base fee on a coin-quoted pair converts into THAT quote; a BNB pair's own coin is its base.
    #[test]
    fn the_quote_is_whatever_the_symbol_says() {
        let (c, a) = spot_fee_in_quote("ETHBTC", 0.001, "ETH", 0.05, "binance");
        assert!((c - 0.000_05).abs() < 1e-15 && a == "BTC", "{c} {a}");
        let (c, a) = spot_fee_in_quote("BNBUSDT", 0.001, "BNB", 600.0, "binance");
        assert!((c - 0.6).abs() < 1e-12 && a == "USDT", "{c} {a}");
    }

    /// BNB on BTCUSDT cannot be converted here: unchanged, still named BNB.
    #[test]
    fn a_third_asset_fee_stays_as_stated() {
        assert_eq!(
            spot_fee_in_quote("BTCUSDT", 0.0003, "BNB", 65_000.0, "binance"),
            (0.0003, "BNB".into())
        );
    }

    /// No stated asset, an empty symbol and a symbol that is only the asset change nothing.
    #[test]
    fn an_unstated_asset_or_symbol_is_unchanged() {
        assert_eq!(
            spot_fee_in_quote("BTCUSDT", 0.0004, "", 62_584.56, "binance"),
            (0.0004, String::new())
        );
        assert_eq!(spot_fee_in_quote("", 0.5, "BTC", 1.0, "binance"), (0.5, "BTC".into()));
        assert_eq!(spot_fee_in_quote("BTC", 0.5, "BTC", 1.0, "binance"), (0.5, "BTC".into()));
    }
}
