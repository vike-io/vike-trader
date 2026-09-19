//! Alpaca `/v1/assets` → `SymbolProperties` (tick/step/min). Feeds `RiskLimits::from_properties` at the
//! app root. Equities default to penny tick / whole-share step when the venue omits increments.

use vike_model::SymbolProperties;

use crate::config::AlpacaConfig;
use crate::rest::AlpacaRest;

fn num(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_str().and_then(|s| s.parse().ok()).or_else(|| x.as_f64()))
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
            r#"{"symbol":"BTC/USD","tradable":true,"min_order_size":"0.0001","min_trade_increment":"0.0001","price_increment":"1"}"#,
        ).unwrap();
        let (sym, f) = parse_asset_properties(&a).unwrap();
        assert_eq!(sym, "BTC/USD");
        assert_eq!(f.tick_size, 1.0);
        assert_eq!(f.step_size, 0.0001);
        assert_eq!(f.min_qty, 0.0001);
    }

    #[test]
    fn equity_asset_defaults() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"symbol":"AAPL","tradable":true}"#).unwrap();
        let (sym, f) = parse_asset_properties(&a).unwrap();
        assert_eq!(sym, "AAPL");
        assert_eq!(f.tick_size, 0.01);
        assert_eq!(f.step_size, 1.0);
    }

    #[test]
    fn untradable_asset_is_none() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"symbol":"XXX","tradable":false}"#).unwrap();
        assert!(parse_asset_properties(&a).is_none());
    }
}
