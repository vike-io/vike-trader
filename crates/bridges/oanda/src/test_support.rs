//! The docs-shaped `GET /v3/accounts/{id}/instruments` rows the instrument-grid tests share: this crate's
//! (`instrument_tests.rs`) and the gatewayhub's facts tests
//! (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts_oanda_grid_tests.rs`), which reach them through a
//! dev edge with `test-support`. They follow OANDA's documentation of the endpoint and of `Instrument`; no
//! practice-account payload has been recorded. Beside them, [`practice_account`]: the accounts the history
//! lane's credential suites plant. Gated `#[cfg(any(test, feature = "test-support"))]`, so no shipped build
//! compiles it.

use serde_json::{Value, json};

/// One docs-shaped `Instrument` of the list: its OANDA `type`, `displayPrecision` and `tradeUnitsPrecision`
/// as the JSON values given (so a garbage one can be a string, negative or absent).
pub fn row(name: &str, kind: &str, display: Option<Value>, units: Value) -> Value {
    let mut row = json!({
        "name": name,
        "type": kind,
        "displayName": name.replace('_', "/"),
        "pipLocation": -4,
        "tradeUnitsPrecision": units,
        "minimumTradeSize": "1",
        "maximumOrderUnits": "100000000",
        "marginRate": "0.03",
    });
    if let Some(display) = display {
        row["displayPrecision"] = display;
    }
    row
}

/// A well-formed `CURRENCY` row priced to `display` decimals, traded in whole units.
pub fn currency(name: &str, display: i64) -> Value {
    row(name, "CURRENCY", Some(json!(display)), json!(0))
}

/// The OANDA practice account and its neighbours (drift register row R7's credential half), planted by the
/// history lane's suites on both sides of the gatewayhub's crate boundary (this crate's
/// `credentialed_klines_tests.rs`, the gatewayhub's `tests/lanes/oanda_credential.rs`) through
/// `vike_bridge_core::credential_fixture::write_store`. Moved here from the gatewayhub's testkit with the
/// lane.
pub mod practice_account {
    use vike_model::AccountTier;

    use vike_bridge_core::credential_fixture::Planted;

    /// The token's FIELD on the practice account.
    pub const TOKEN_FIELD: &str = vike_venues::credential_keys::API_KEY;
    /// The oanda account's other field — the neighbour on the SAME account the read must leave.
    pub const ACCOUNT_ID_FIELD: &str = "ACCOUNT_ID";

    /// The practice account's token.
    pub const PRACTICE_VALUE: &str = "fake-practice-token-7q4z";

    /// A store's worth of accounts around the practice account, with the practice account's token set to
    /// `token` (`None` = the account holds no token). The NEIGHBOURS the one-field read must leave
    /// behind: the practice account's own account id, the LIVE oanda account's pair, the PAPER oanda
    /// account's key, a LABELLED demo oanda account's key, and another venue's key. Every value
    /// distinctive.
    pub fn store_rows(token: Option<&str>) -> Vec<Planted> {
        let pair = |field: &str, value: &str| (field.to_string(), value.to_string());
        let mut practice = vec![pair(ACCOUNT_ID_FIELD, "fake-account-id-2k9d")];
        if let Some(token) = token {
            practice.push(pair(TOKEN_FIELD, token));
        }
        vec![
            ("oanda", AccountTier::Demo, None, practice),
            (
                "oanda",
                AccountTier::Live,
                None,
                vec![
                    pair(TOKEN_FIELD, "fake-live-token-8m3x"),
                    pair(ACCOUNT_ID_FIELD, "fake-live-account-5j1c"),
                ],
            ),
            ("oanda", AccountTier::Paper, None, vec![pair(TOKEN_FIELD, "fake-sim-token-3w7n")]),
            (
                "oanda",
                AccountTier::Demo,
                Some("ALT"),
                vec![pair(TOKEN_FIELD, "fake-labelled-token-9p5r")],
            ),
            ("binance", AccountTier::Live, None, vec![pair(TOKEN_FIELD, "fake-binance-key-4t8y")]),
        ]
    }

    /// Every neighbour, the practice account holding NO token.
    pub fn neighbours() -> Vec<Planted> {
        store_rows(None)
    }

    /// The practice token plus every neighbour.
    pub fn full_store_rows() -> Vec<Planted> {
        store_rows(Some(PRACTICE_VALUE))
    }

    /// Every planted value, the practice token included — what no log line may carry.
    pub fn every_value() -> Vec<String> {
        let mut values: Vec<String> = store_rows(None)
            .into_iter()
            .flat_map(|(_, _, _, fields)| fields.into_iter().map(|(_, v)| v))
            .collect();
        values.push(PRACTICE_VALUE.to_string());
        values
    }
}
