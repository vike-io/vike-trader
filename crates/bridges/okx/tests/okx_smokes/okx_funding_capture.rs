//! **What does the okx DEMO account's bills endpoint answer for funding, and for a `begin` older than its
//! 7 days?** A READ-ONLY capture, run by hand:
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture okx_funding_capture::
//!
//! Every request is a signed GET of `/api/v5/account/bills` (`type=8`, funding; `instType=SWAP`) on the demo
//! header (nothing is placed, cancelled or moved), printed sanitized with its row count and `ts` range: with no
//! `begin`, with a `begin` three days back, and with one ten days back (a node up past the endpoint's 7 days
//! sends its spawn floor there: `crates/bridges/okx/src/execution/funding.rs`'s `OkxFundingPoller`).

use serde_json::{Value, json};

use crate::demo_credentials::load_demo_credentials;
use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::now_ms;
use vike_okx::execution::rest::PATH_BILLS;
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;

const DAY: i64 = 86_400_000;

#[test]
#[ignore = "network + demo creds — a read-only CAPTURE, run by hand (see module doc)"]
fn okx_funding_capture() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    // `true` = demo, through the header (`okx_identity_probe`'s spelling).
    let transport =
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate());
    let signer = OkxV5Signer::new(&creds, now_ms);
    let now = now_ms();
    for (tag, begin) in
        [("no-begin", None), ("begin-3d", Some(now - 3 * DAY)), ("begin-10d", Some(now - 10 * DAY))]
    {
        let mut params = vec![("instType", json!("SWAP")), ("type", json!("8"))];
        if let Some(b) = begin {
            params.push(("begin", json!(b.to_string())));
        }
        match transport.signed(&demo_net().rest, PATH_BILLS, "GET", &params, &signer) {
            Ok(body) => {
                let rows = body.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
                let ts: Vec<i64> = rows
                    .iter()
                    .filter_map(|r| r["ts"].as_str().and_then(|s| s.parse().ok()))
                    .collect();
                println!(
                    "WINDOW {tag} begin={begin:?}: code {} {} rows, ts {:?}..{:?}",
                    body["code"],
                    rows.len(),
                    ts.iter().min(),
                    ts.iter().max()
                );
                println!("CAPTURED {tag} {}", capture_frame("okx", "bills", &body).frame);
            }
            Err(e) => println!("REFUSED {tag} begin={begin:?}: {e}"),
        }
    }
}
