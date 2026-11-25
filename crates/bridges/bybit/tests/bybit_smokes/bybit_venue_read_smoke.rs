//! **The operator's balance read on the demo account labelled `INV`, READ-ONLY** (the CI box's bybit row 19, the
//! `BTCUSD` inverse engine's account, on the same keys as `DEMO`): which account version the venue says it is, what the
//! unified wallet lists, what the separate inverse (`CONTRACT`) wallet of a UTA 1.0 account answers, and what the
//! production reads make of it: the operator's (`vike_bybit::reconcile::venue_read::read_balance`) and the `BTCUSD` engine's
//! coin cash (`vike_bybit::reconcile`'s `fetch_balance`).
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture --test-threads=1 bybit_venue_read_smoke::
//!
//! Places nothing and writes nothing: signed GETs on the DEMO host (`account/info`, the unified wallet with and
//! without `coin=BTC`, the `CONTRACT` wallet with and without it, then the two production reads). Every answer is a
//! `FINDING` line; the assertions are that each production read states the BTC figure of the wallet the raw reads
//! put the inverse book in (`CONTRACT` where it answers, else the unified one). Only `venue-read smoke green` is a
//! proof; a `SKIP:` line is not.
//!
//! Measured 2026-11-09 on the CI box's `INV` row: `unifiedMarginStatus` 3 (UTA 1.0), and `CONTRACT` refused, with and
//! without `coin`, `10001` "accountType only support UNIFIED.": the demo account keeps the unified wallet alone.

use serde_json::{Value, json};
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::rest::{PATH_ACCOUNT, seed_endpoints};
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_bybit::transport::client::unwrap_envelope;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, now_ms};
use vike_secrets::credentials::api_credentials;

/// the CI box's label for the inverse engine's demo row.
const INVERSE_LABEL: &str = "INV";

fn coin_rows(result: &Value) -> Vec<(String, String, String)> {
    result["list"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["coin"].as_array())
        .flatten()
        .map(|c| {
            let text = |k: &str| c[k].as_str().unwrap_or("").to_string();
            (text("coin"), text("walletBalance"), text("equity"))
        })
        .collect()
}

/// The BTC row's `walletBalance` in a wallet answer, `None` when it lists none.
fn btc_of(result: &Value) -> Option<f64> {
    coin_rows(result)
        .into_iter()
        .find(|(coin, _, _)| coin == "BTC")
        .and_then(|(_, wallet, _)| wallet.parse::<f64>().ok())
}

#[test]
#[ignore = "network + demo creds — read-only — run manually (see module doc)"]
fn the_balance_read_states_the_inverse_accounts_coin() {
    vike_log::test_init();
    vike_bybit::history::kept_list_for_tests();
    let directory = vike_secrets::smoke_directory();
    let label = AccountLabel::parse(INVERSE_LABEL).expect("a legal label");
    let account = directory.credentials_for("bybit", &label, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: the bybit demo row labelled INV holds no key pair");
        return;
    };
    let demo = seed_endpoints(AccountTier::Demo).rest; // the demo host, from the release seed's rows
    let signer = BybitV5Signer::new(&creds, now_ms);
    let wire = rest_transport();
    let get = |path: &str, params: &[(&str, Value)]| {
        wire.signed_requery(&demo, path, "GET", params, &signer).and_then(unwrap_envelope)
    };

    let info = get("/v5/account/info", &[]).expect("account/info");
    println!(
        "FINDING bybit INV account/info: unifiedMarginStatus={} marginMode={}",
        info["unifiedMarginStatus"], info["marginMode"]
    );

    let unified =
        get(PATH_ACCOUNT, &[("accountType", json!("UNIFIED"))]).expect("the unified wallet");
    println!(
        "FINDING bybit INV unified wallet (coin, walletBalance, equity): {:?}",
        coin_rows(&unified)
    );
    println!(
        "FINDING bybit INV unified totals: totalEquity={} totalWalletBalance={}",
        unified["list"][0]["totalEquity"], unified["list"][0]["totalWalletBalance"]
    );
    let scoped = get(PATH_ACCOUNT, &[("accountType", json!("UNIFIED")), ("coin", json!("BTC"))])
        .expect("the unified wallet, coin=BTC");
    println!("FINDING bybit INV unified coin=BTC: {:?}", coin_rows(&scoped));
    let mut contract_btc = None;
    for params in [
        vec![("accountType", json!("CONTRACT"))],
        vec![("accountType", json!("CONTRACT")), ("coin", json!("BTC"))],
    ] {
        match get(PATH_ACCOUNT, &params) {
            Ok(contract) => {
                println!(
                    "FINDING bybit INV CONTRACT wallet {params:?}: rows (coin, walletBalance, equity) {:?}; raw {contract}",
                    coin_rows(&contract)
                );
                contract_btc = Some(btc_of(&contract));
            }
            Err(e) => println!(
                "FINDING bybit INV CONTRACT wallet {params:?}: refused [{}] {}",
                e.code, e.msg
            ),
        }
    }

    let read = vike_bybit::reconcile::venue_read::read_balance(&wire, &signer, &demo)
        .expect("the production balance read");
    println!("FINDING bybit INV production read: {read:?}");
    let figure = |asset: &str| {
        std::iter::once((read.asset.as_str(), read.balance))
            .chain(read.other_assets.iter().map(|o| (o.asset.as_str(), o.balance)))
            .find(|(a, _)| *a == asset)
            .map(|(_, balance)| balance)
    };
    let unified_btc = btc_of(&scoped);
    assert_eq!(
        figure("BTC"),
        unified_btc.filter(|b| *b != 0.0),
        "the read states the unified BTC the raw read does"
    );
    assert_eq!(
        figure("BTC (CONTRACT wallet)"),
        contract_btc.flatten().filter(|b| *b != 0.0),
        "and the CONTRACT wallet's BTC, named as that wallet's, where it answers"
    );

    // The inverse perpetual's USER name (decision 0290): the bare `BTCUSD` is the spot pair, which settles in no coin.
    let recon =
        vike_bybit::recon_client(&creds, "BTCUSD.P", &demo).expect("the BTCUSD.P reconcile client");
    let cash = recon.fetch_balance().expect("the inverse book's coin cash");
    println!("FINDING bybit INV production inverse cash (BTCUSD.P): {cash:?} BTC");
    // The CONTRACT wallet where the venue keeps one, else the unified wallet (the demo's answer, 2026-11-09).
    let expected = contract_btc.unwrap_or(unified_btc);
    assert_eq!(cash, expected, "the inverse cash is the BTC of the wallet the inverse book is in");
    tracing::info!(target: "vike_bybit", "venue-read smoke green");
}
