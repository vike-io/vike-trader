//! **What bybit's key-info endpoint answers for the DEMO key**, captured for the withdraw-key gate (B3.20). READ-ONLY,
//! run by hand; it prints the file `tests/fixtures/captured/query_api_demo.json` holds:
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture bybit_key_permissions_capture::
//!
//! One signed `GET /v5/user/query-api` on the demo host, on the account `demo_label` names; nothing is placed, moved
//! or set. The answer is sanitized by `vike_bridge_core::capture::capture_frame` after this file blanks the fields of
//! `result` that name the key or its owner and that `REDACT_KEYS` cannot carry ([`OWN_KEYS`]: `id` is every venue's
//! order id there). The permission fields (`readOnly`, `permissions`) stay verbatim. A refused read (an expired key
//! answers `retCode 33004`) is printed and captures nothing; only `key-permissions capture green` is a capture.
//! ⚠ Read the printed body before committing it: the fixture directory ships to the public mirror.

use serde_json::{Value, json};

use crate::demo_label::demo_label;
use vike_bridge_core::capture::FrameCapture;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::rest::{PATH_QUERY_API, seed_endpoints};
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_model::{AccountTier, now_ms};
use vike_secrets::credentials::api_credentials;

/// The fixture's name: `captured/<KIND>.json`.
const KIND: &str = "query_api_demo";

/// Fields of `result` that name the key or its owner beyond `REDACT_KEYS` (which takes `apiKey`, `secret`, `userID`,
/// `parentUid`): the key's id and note, its IP allow-list, the referral and affiliate ids, its RSA public key and the
/// owner's KYC region.
const OWN_KEYS: &[&str] =
    &["id", "note", "ips", "inviterID", "affiliateID", "rsaPublicKey", "kycRegion"];

#[test]
#[ignore = "network + demo creds — read-only capture, run manually (see module doc)"]
fn bybit_query_api_answer_is_captured() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &demo_label(&directory), AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
        return;
    };
    let demo = seed_endpoints(AccountTier::Demo).rest; // the demo host, from the release seed's rows
    let signer = BybitV5Signer::new(&creds, now_ms);
    let mut body = match rest_transport().signed(&demo, PATH_QUERY_API, "GET", &[], &signer) {
        Ok(body) => body,
        Err(e) => {
            println!("REFUSED {PATH_QUERY_API}: [{}] {}", e.code, e.msg);
            return;
        }
    };
    if body["retCode"] != 0 {
        println!("REFUSED {PATH_QUERY_API}: retCode {} {}", body["retCode"], body["retMsg"]);
        return;
    }
    if let Some(result) = body.get_mut("result").and_then(Value::as_object_mut) {
        for key in OWN_KEYS {
            if let Some(v) = result.get_mut(*key).filter(|v| !v.is_null() && !v.is_boolean()) {
                // `capture_frame`'s placeholder shape: a number stays a number.
                *v = if v.is_number() {
                    json!(0)
                } else {
                    json!(format!("[redacted:{}]", key.to_ascii_lowercase()))
                };
            }
        }
    }

    let mut capture = FrameCapture::new(
        "bybit",
        format!(
            "bybit V5 DEMO ({demo}) — bybit_key_permissions_capture: one signed GET {PATH_QUERY_API}, read-only; \
             beside REDACT_KEYS the smoke blanked result's {OWN_KEYS:?}"
        ),
    );
    let redacted = capture.capture(KIND, &body).redacted_paths.clone();
    let dir = std::env::temp_dir().join(format!("bybit_key_permissions_{}", std::process::id()));
    let written = capture.write_all(&dir, now_ms()).expect("the fixture writes");
    let fixture = std::fs::read_to_string(&written[0]).expect("the fixture reads back");
    let _ = std::fs::remove_dir_all(&dir);
    println!("REDACT_KEYS took: {redacted:?}");
    println!("=== FIXTURE captured/{KIND}.json ===\n{fixture}=== END FIXTURE ===");
    println!(
        "FINDING bybit query-api: readOnly={} permissions={}",
        body["result"]["readOnly"], body["result"]["permissions"]
    );
    tracing::info!(target: "vike_bybit", "key-permissions capture green");
}
