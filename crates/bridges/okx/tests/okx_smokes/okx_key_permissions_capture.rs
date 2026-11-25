//! **What okx's account config answers for the DEMO key**, captured for the withdraw-key gate (B3.20): the VALUE of
//! `perm`, which `okx_identity_probe` only saw present. READ-ONLY, run by hand; it prints the file
//! `tests/fixtures/captured/account_config_demo.json` holds:
//!
//!     MSYS_NO_PATHCONV=1 just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture okx_key_permissions_capture::
//!
//! One signed `GET /api/v5/account/config` on the demo network (`rest_transport(true)`: the simulated-trading header,
//! as `okx_identity_probe` spells it); nothing is placed, moved or set. The answer is sanitized by
//! `vike_bridge_core::capture::capture_frame` after this file blanks the fields of each `data` row that name the key
//! and that `REDACT_KEYS` cannot carry ([`OWN_KEYS`]: `label` is deribit's order client id there). `perm` stays
//! verbatim. A refused read is printed and captures nothing; only `key-permissions capture green` is a capture.
//! ⚠ Read the printed body before committing it: the fixture directory ships to the public mirror.

use serde_json::{Value, json};

use crate::demo_credentials::load_demo_credentials;
use vike_bridge_core::capture::FrameCapture;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::now_ms;
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;

const PATH_ACCOUNT_CONFIG: &str = "/api/v5/account/config";

/// The fixture's name: `captured/<KIND>.json`.
const KIND: &str = "account_config_demo";

/// Fields of a `data` row that name the key beyond `REDACT_KEYS` (which takes `uid`, `mainUid`): the key's note and its
/// IP allow-list.
const OWN_KEYS: &[&str] = &["label", "ip"];

#[test]
#[ignore = "network + demo creds — read-only capture, run manually (see module doc)"]
fn okx_account_config_answer_is_captured() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let net = demo_net();
    let transport =
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate());
    let signer = OkxV5Signer::new(&creds, now_ms);
    let mut body = match transport.signed(&net.rest, PATH_ACCOUNT_CONFIG, "GET", &[], &signer) {
        Ok(body) => body,
        Err(e) => {
            println!("REFUSED {PATH_ACCOUNT_CONFIG}: [{}] {}", e.code, e.msg);
            return;
        }
    };
    if body["code"] != "0" {
        println!("REFUSED {PATH_ACCOUNT_CONFIG}: code {} {}", body["code"], body["msg"]);
        return;
    }
    for row in body.get_mut("data").and_then(Value::as_array_mut).into_iter().flatten() {
        let Some(row) = row.as_object_mut() else { continue };
        for key in OWN_KEYS {
            if let Some(v) = row.get_mut(*key).filter(|v| !v.is_null() && !v.is_boolean()) {
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
        "okx",
        format!(
            "okx V5 DEMO ({}, simulated-trading header) — okx_key_permissions_capture: one signed GET \
             {PATH_ACCOUNT_CONFIG}, read-only; beside REDACT_KEYS the smoke blanked each data row's {OWN_KEYS:?}",
            net.rest
        ),
    );
    let redacted = capture.capture(KIND, &body).redacted_paths.clone();
    let dir = std::env::temp_dir().join(format!("okx_key_permissions_{}", std::process::id()));
    let written = capture.write_all(&dir, now_ms()).expect("the fixture writes");
    let fixture = std::fs::read_to_string(&written[0]).expect("the fixture reads back");
    let _ = std::fs::remove_dir_all(&dir);
    println!("REDACT_KEYS took: {redacted:?}");
    println!("=== FIXTURE captured/{KIND}.json ===\n{fixture}=== END FIXTURE ===");
    println!("FINDING okx account/config: perm={}", body["data"][0]["perm"]);
    tracing::info!(target: "vike_okx", "key-permissions capture green");
}
