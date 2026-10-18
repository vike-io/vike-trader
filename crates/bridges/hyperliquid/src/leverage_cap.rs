//! **What the venue lets THIS account margin one coin at** (decision 0181): the leverage the
//! tradehub may size an account's orders against when the operator states `account.leverage`.
//!
//! Two PUBLIC `/info` reads, both unauthenticated (a key signs nothing here):
//!
//! * `meta` — the coin's `maxLeverage`, the venue's own ceiling for the instrument (it falls with
//!   the coin's tier: 40x for BTC, 3x for a thin one). A coin the universe does not list, or that
//!   is isolated-only (`onlyIsolated`, `marginMode` `strictIsolated`/`noCross`), is refused: the engine
//!   trades cross margin.
//! * `activeAssetData` — the account's OWN leverage setting for that coin (`leverage.type` and
//!   `leverage.value`). The venue margins an order at THAT figure, whatever the engine believes, so
//!   the answer is the lower of the two. An ISOLATED setting is refused (the engine would margin
//!   cross while the venue margins the wallet of one position).
//!
//! The answer is a CAP, not a setting: nothing here sends `updateLeverage`, and the adapter still
//! never does (`crate::adapter`'s module doc). If the account holds 20x cross on the coin and the
//! operator states 10x, the engine's gate uses 10x and the venue keeps enforcing 20x underneath it.
//!
//! The cost is two weight-20 reads (plus the weight-60 `userRole` read when the account row names no
//! address and the key's own address must be resolved), once per adapter start of an account that
//! states a leverage, and nothing for any other account.

use serde_json::Value;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::AccountCredentials;

use crate::base::config;
use crate::base::transport::HyperliquidTransport;

/// The `/info` body of the account's leverage on `coin`. Exposed so a test pins the wire shape.
#[must_use]
pub fn active_asset_data_body(user: &str, coin: &str) -> Value {
    serde_json::json!({ "type": "activeAssetData", "user": user, "coin": coin })
}

/// The cap from the two answers, PURE: the lower of the coin's `maxLeverage` in `meta` and the
/// account's cross leverage in `active`. Every shape this does not understand is a refusal in words,
/// never a guess.
///
/// # Errors
/// The coin is not a listed core perpetual, is isolated-only, or the account holds an isolated
/// leverage on it; or a field is missing or not a usable figure (`>= 1`).
pub fn cap_from_reads(meta: &Value, active: &Value, coin: &str) -> Result<f64, String> {
    let row = meta
        .get("universe")
        .and_then(Value::as_array)
        .and_then(|rows| rows.iter().find(|r| r.get("name").and_then(Value::as_str) == Some(coin)))
        .ok_or_else(|| format!("hyperliquid lists no core perpetual `{coin}` (meta)"))?;
    if row.get("isDelisted").and_then(Value::as_bool) == Some(true) {
        return Err(format!("hyperliquid has delisted `{coin}`"));
    }
    let margin_mode = row.get("marginMode").and_then(Value::as_str);
    if row.get("onlyIsolated").and_then(Value::as_bool) == Some(true)
        || matches!(margin_mode, Some("strictIsolated" | "noCross"))
    {
        return Err(format!("`{coin}` can only be traded isolated; the engine margins cross"));
    }
    let venue_max = row
        .get("maxLeverage")
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite() && *x >= 1.0)
        .ok_or_else(|| format!("meta carries no usable maxLeverage for `{coin}`"))?;
    let leverage = active
        .get("leverage")
        .ok_or_else(|| format!("activeAssetData carries no leverage for `{coin}`"))?;
    if leverage.get("type").and_then(Value::as_str) != Some("cross") {
        return Err(format!(
            "this account holds a non-cross leverage on `{coin}`; the engine margins cross, so \
             set it to cross on the venue first"
        ));
    }
    let held = leverage
        .get("value")
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite() && *x >= 1.0)
        .ok_or_else(|| format!("activeAssetData carries no usable cross leverage for `{coin}`"))?;
    Ok(venue_max.min(held))
}

/// Ask the venue what `creds` may margin `coin` at on `tier`'s network. See the module doc for the cost.
///
/// # Errors
/// No key on the account, the account's book cannot be named, a read failed, or
/// [`cap_from_reads`] refused. The text is the operator's sentence and never carries a credential.
pub fn confirmed_leverage_cap(
    tier: AccountTier,
    creds: &AccountCredentials<'_>,
    label: &AccountLabel,
    coin: &str,
) -> Result<f64, String> {
    let Some(hl) = config::load(tier, creds) else {
        return Err("the account holds no hyperliquid key".to_string());
    };
    // A fresh gate: this runs once per start of an account that states a leverage, three reads at most.
    let transport = HyperliquidTransport::new(hl.network);
    let user = match hl.account_address {
        Some(address) => address,
        None => {
            // The key IS the account, or an agent wallet for another: only the venue knows, and an
            // unconfirmed answer reads a different (empty) book.
            let signer = crate::signing::Signer::from_private_key(&hl.private_key, hl.network)
                .map_err(|e| format!("the account's key is unusable ({e})"))?;
            let master = crate::adapter::resolve_master(&transport, signer.address(), label);
            if !master.confirmed {
                return Err("the venue did not confirm which account this key trades for".into());
            }
            master.address
        }
    };
    let meta = transport
        .info(&serde_json::json!({ "type": "meta" }))
        .map_err(|e| format!("meta could not be read: {e}"))?;
    let active = transport
        .info(&active_asset_data_body(&user, coin))
        .map_err(|e| format!("activeAssetData could not be read: {e}"))?;
    cap_from_reads(&meta, &active, coin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta() -> Value {
        json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25},
            {"name": "HPOS", "szDecimals": 2, "maxLeverage": 3, "onlyIsolated": true},
            {"name": "STRICT", "szDecimals": 1, "maxLeverage": 3, "marginMode": "strictIsolated"},
            {"name": "OLD", "szDecimals": 1, "maxLeverage": 3, "isDelisted": true},
        ]})
    }

    fn held(kind: &str, value: u64) -> Value {
        json!({"user": "0xabc", "coin": "BTC", "leverage": {"type": kind, "value": value},
               "maxTradeSzs": ["1.0", "1.0"], "availableToTrade": ["8.8", "8.8"], "markPx": "1.0"})
    }

    #[test]
    fn the_request_names_the_user_and_the_coin() {
        assert_eq!(
            active_asset_data_body("0xabc", "BTC"),
            json!({"type": "activeAssetData", "user": "0xabc", "coin": "BTC"})
        );
    }

    /// The lower of the venue's coin ceiling and the account's own cross leverage, either way round.
    #[test]
    fn the_cap_is_the_lower_of_the_coin_ceiling_and_the_accounts_cross_leverage() {
        assert_eq!(cap_from_reads(&meta(), &held("cross", 20), "BTC"), Ok(20.0));
        assert_eq!(cap_from_reads(&meta(), &held("cross", 50), "BTC"), Ok(40.0));
        assert_eq!(cap_from_reads(&meta(), &held("cross", 20), "ETH"), Ok(20.0));
        assert_eq!(cap_from_reads(&meta(), &held("cross", 30), "ETH"), Ok(25.0));
    }

    /// Everything the engine cannot margin as it models it is a refusal with the reason, not a cap.
    #[test]
    fn what_the_engine_cannot_margin_is_refused() {
        let cross = held("cross", 20);
        for coin in ["HPOS", "STRICT", "OLD", "NOPE"] {
            assert!(cap_from_reads(&meta(), &cross, coin).is_err(), "{coin}");
        }
        let err = cap_from_reads(&meta(), &held("isolated", 20), "BTC").unwrap_err();
        assert!(err.contains("non-cross"), "{err}");
        assert!(cap_from_reads(&meta(), &json!({"coin": "BTC"}), "BTC").is_err());
        assert!(cap_from_reads(&meta(), &held("cross", 0), "BTC").is_err());
        assert!(cap_from_reads(&json!({}), &cross, "BTC").is_err());
    }
}
