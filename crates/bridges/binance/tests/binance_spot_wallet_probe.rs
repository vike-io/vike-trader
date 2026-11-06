//! **READ-ONLY.** Prints the binance spot DEMO account's non-zero wallet balances, one `WALLET` line per
//! asset, as the venue states them. It places nothing and cancels nothing.
//!
//! It is the independent half of the live proof that a spot BUY's position is NET of the base-coin
//! fee (decision 0215; `crates/bridges/binance/src/family/spot_fee.rs`): the wallet is read from the
//! venue's `GET /api/v3/account`, not from anything the node booked, so the node's position after a BUY
//! can be compared with the coin the account really holds. The script that drives the round trip and
//! judges the comparison is `tb11-binance-spot-net-position.sh` of the 0.1.97 live-test kit (kept
//! beside the release notes, not in this tree).
//!
//! Run it on a lane before and after the trade (network + demo credentials; `#[ignore]`d):
//!
//! ```text
//! cargo test -p vike-binance --test binance_spot_wallet_probe -- --ignored --nocapture
//! ```
//!
//! Output, one line per asset with a non-zero balance (the numbers are the venue's own strings):
//!
//! ```text
//! WALLET BTC free=0.00100000 locked=0.00000000
//! ```
//!
//! No credentials: `SKIP: BINANCE_DEMO creds absent`, and nothing is read. A skip and a read are the
//! same exit code, so look for a `WALLET` line.

use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::now_ms;
use vike_secrets::credentials::api_credentials;

use vike_binance::spot::{DEMO_REST, PATH_ACCOUNT, PATH_TIME};

#[test]
#[ignore = "network + demo creds — read-only — run manually (see module doc)"]
fn binance_spot_wallet_probe() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "binance") else {
        println!("SKIP: BINANCE_DEMO creds absent — nothing was read");
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
        return;
    };
    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate());
    let signer = BinanceHmacSigner::new(&creds, now_ms);
    // The signed read needs the demo host's own clock (recvWindow is short).
    let server_ms = transport
        .public(DEMO_REST, PATH_TIME, &[])
        .expect("server time")
        .get("serverTime")
        .and_then(|t| t.as_i64())
        .expect("serverTime");
    signer.set_offset_ms(server_ms - now_ms());
    let body = transport.signed(DEMO_REST, PATH_ACCOUNT, "GET", &[], &signer).expect("account");

    let mut printed = 0;
    for row in body.get("balances").and_then(|b| b.as_array()).into_iter().flatten() {
        let word = |key: &str| row.get(key).and_then(|v| v.as_str()).unwrap_or("0");
        let total = row.get("free").and_then(json_num).unwrap_or(0.0)
            + row.get("locked").and_then(json_num).unwrap_or(0.0);
        if total > 0.0 {
            println!(
                "WALLET {} free={} locked={}",
                row.get("asset").and_then(|a| a.as_str()).unwrap_or("?"),
                word("free"),
                word("locked")
            );
            printed += 1;
        }
    }
    println!("WALLET-READ-DONE assets={printed}");
}
