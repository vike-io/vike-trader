//! Alpaca `/v1/assets` → `SymbolProperties` (tick/step/min). Feeds `RiskLimits::from_properties` at the
//! app root. Equities default to penny tick / whole-share step when the venue omits increments.

use vike_model::{AssetClass, SymbolProperties};

use crate::config::AlpacaConfig;
use crate::rest::AlpacaRest;

fn num(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_str().and_then(|s| s.parse().ok()).or_else(|| x.as_f64()))
}

/// The asset class of ONE `/v1/assets` entry, from the venue's own `class` — the same field
/// `crates/bridges/alpaca/src/catalog.rs`'s `parse_assets` selects picker rows on, read off the
/// same entry the grid came from rather than re-derived from the symbol text
/// (`docs/decisions/0061-an-instrument-names-its-kind.md`). The `/` in `BTC/USD` is exactly the
/// implicit encoding that record removes, and this venue is where it would be most tempting: the
/// pair separator is the ONLY textual difference between `BTC/USD` and `AAPL`.
///
/// `us_equity` answers [`AssetClass::Equity`] and NOT [`AssetClass::Etf`], deliberately: alpaca
/// publishes one word for both, so an ETF row says `us_equity` and nothing on this entry says
/// which it is. Picking `Etf` for some subset would be this parser inventing a distinction the
/// venue did not make. (The `/v1/assets` entry does carry an `attributes[]` list, but it is a
/// tradability/eligibility flag set — `fractional_eh_enabled`, `ptp_no_exception` — not a
/// taxonomy.)
///
/// An absent, empty or unrecognised `class` is `None`, not the nearest variant — see
/// [`vike_model::SymbolProperties::asset_class`] on why a missing answer stays missing.
fn asset_class_of(asset: &serde_json::Value) -> Option<AssetClass> {
    match asset.get("class").and_then(|c| c.as_str()).unwrap_or("") {
        "us_equity" => Some(AssetClass::Equity),
        "crypto" => Some(AssetClass::CryptoSpot),
        "us_option" => Some(AssetClass::Option),
        _ => None,
    }
}

/// Parse one `/v1/assets` entry. `None` when untradable. Defaults suit equities (0.01 tick, 1 step).
pub fn parse_asset_properties(asset: &serde_json::Value) -> Option<(String, SymbolProperties)> {
    if !asset.get("tradable").and_then(|t| t.as_bool()).unwrap_or(false) {
        return None;
    }
    let symbol = asset.get("symbol").and_then(|s| s.as_str())?.to_string();
    let properties = SymbolProperties {
        tick_size: num(asset, "price_increment").unwrap_or(0.01),
        step_size: num(asset, "min_trade_increment").unwrap_or(1.0),
        min_qty: num(asset, "min_order_size").unwrap_or(0.0),
        min_notional: 0.0,
        // The class is the VENUE'S OWN `class`, read off this entry — see [`asset_class_of`],
        // which also records why `us_equity` never becomes `Etf` and why an unknown word is
        // `None`. ⚠ An entry with no `class` still yields a grid: this parser gates on `tradable`
        // and on `symbol`, never on the class, so dropping the row would change what the mount
        // gets. It carries no class instead.
        asset_class: asset_class_of(asset),
        ..Default::default()
    };
    Some((symbol, properties))
}

/// Fetch + parse properties for one symbol. `None` on any error/untradable (never panics).
pub fn fetch_properties(
    rest: &AlpacaRest,
    config: &AlpacaConfig,
    symbol: &str,
) -> Option<SymbolProperties> {
    let path = format!("/v1/assets/{symbol}");
    let resp = rest.get(config.hosts.broker, &path, "").ok()?;
    parse_asset_properties(&resp).map(|(_, f)| f)
}

/// Convenience pre-fetch for the app-root `RiskLimits::from_properties` mount (the sibling of every
/// other bridge's `fetch_{bybit,binance,…}_properties(&config, symbol)` that `vike_mount::make_engine`
/// calls). Builds a FRESH, throwaway `TokenSource`/`AlpacaRest` — a startup-only OAuth2 lifecycle
/// isolated from the exec/recon clients — then delegates to [`fetch_properties`]. Best-effort:
/// `None` on any error/untradable, so the caller keeps the permissive default grid.
pub fn fetch_alpaca_properties(config: &AlpacaConfig, symbol: &str) -> Option<SymbolProperties> {
    let token = std::sync::Arc::new(crate::auth::TokenSource::new(
        config.client_id.clone(),
        config.client_secret.clone(),
        config.hosts.authx.to_string(),
    ));
    let rest = AlpacaRest::new(token);
    fetch_properties(&rest, config, symbol)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_asset_maps_increments() {
        let a: serde_json::Value = serde_json::from_str(
            r#"{"symbol":"BTC/USD","class":"crypto","tradable":true,"min_order_size":"0.0001","min_trade_increment":"0.0001","price_increment":"1"}"#,
        ).unwrap();
        let (sym, f) = parse_asset_properties(&a).unwrap();
        assert_eq!(sym, "BTC/USD");
        assert_eq!(f.tick_size, 1.0);
        assert_eq!(f.step_size, 0.0001);
        assert_eq!(f.min_qty, 0.0001);
        assert_eq!(f.asset_class, Some(AssetClass::CryptoSpot));
    }

    #[test]
    fn equity_asset_defaults() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"symbol":"AAPL","class":"us_equity","tradable":true}"#)
                .unwrap();
        let (sym, f) = parse_asset_properties(&a).unwrap();
        assert_eq!(sym, "AAPL");
        assert_eq!(f.tick_size, 0.01);
        assert_eq!(f.step_size, 1.0);
        assert_eq!(f.asset_class, Some(AssetClass::Equity));
    }

    /// The venue's `class` decides, not the symbol text — asserted in the direction that can only
    /// pass if the text is unread: a slashless crypto entry and a slashed equity one. A `/` test
    /// (the shape a sibling bridge had and lost) answers BOTH of these backwards.
    #[test]
    fn the_class_field_decides_not_the_slash_in_the_symbol() {
        let slashless_crypto: serde_json::Value =
            serde_json::from_str(r#"{"symbol":"BTCUSD","class":"crypto","tradable":true}"#)
                .unwrap();
        assert_eq!(
            parse_asset_properties(&slashless_crypto).unwrap().1.asset_class,
            Some(AssetClass::CryptoSpot)
        );
        let slashed_equity: serde_json::Value =
            serde_json::from_str(r#"{"symbol":"BRK/B","class":"us_equity","tradable":true}"#)
                .unwrap();
        assert_eq!(
            parse_asset_properties(&slashed_equity).unwrap().1.asset_class,
            Some(AssetClass::Equity)
        );
    }

    /// An ETF says `us_equity` like every other listed name, so it answers [`AssetClass::Equity`].
    /// Pinned because the taxonomy HAS an `Etf` variant and the temptation is to reach for it:
    /// nothing on this entry distinguishes the two, and a guess here is stored as a fetch.
    #[test]
    fn an_etf_is_whatever_the_venue_called_it() {
        let spy: serde_json::Value = serde_json::from_str(
            r#"{"symbol":"SPY","class":"us_equity","name":"SPDR S&P 500 ETF Trust","tradable":true}"#,
        )
        .unwrap();
        assert_eq!(parse_asset_properties(&spy).unwrap().1.asset_class, Some(AssetClass::Equity));
    }

    /// Absent, empty, unknown and wrong-typed all read as "the venue said nothing" — and the row
    /// still parses, because the class is not a gate here.
    #[test]
    fn an_unrecognised_class_is_unknown_not_a_guess() {
        for body in [
            r#"{"symbol":"X","tradable":true}"#,
            r#"{"symbol":"X","class":"","tradable":true}"#,
            r#"{"symbol":"X","class":"US_EQUITY","tradable":true}"#,
            r#"{"symbol":"X","class":"us_future","tradable":true}"#,
            r#"{"symbol":"X","class":7,"tradable":true}"#,
        ] {
            let a: serde_json::Value = serde_json::from_str(body).unwrap();
            let (sym, f) = parse_asset_properties(&a)
                .unwrap_or_else(|| panic!("still a tradable row, class or no class: {body}"));
            assert_eq!(sym, "X");
            assert_eq!(f.asset_class, None, "{body}");
        }
    }

    #[test]
    fn untradable_asset_is_none() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"symbol":"XXX","tradable":false}"#).unwrap();
        assert!(parse_asset_properties(&a).is_none());
    }
}
