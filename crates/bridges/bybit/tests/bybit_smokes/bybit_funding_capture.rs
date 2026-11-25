//! **What does the bybit DEMO account's transaction log answer for funding, and over which window?** A
//! READ-ONLY capture, run by hand, printed for `crates/bridges/bybit/tests/fixtures/funding/`:
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-bybit --test bybit_smokes -- --ignored --nocapture bybit_funding_capture::
//!
//! Every request is a signed GET of `/v5/account/transaction-log` on the demo host (the release seed's demo
//! `trade` row, `perp::seed_endpoints`; nothing is placed,
//! cancelled or moved), each answer printed sanitized (`vike_bridge_core::capture::capture_frame`) with its
//! row count and the `transactionTime` range it covers: the SETTLEMENT rows with no window, with a
//! `startTime` ALONE six days back (the poller's request before 2026-11-08, its floor), and with a
//! `startTime`/`endTime` pair over the same six days, paged; the same pair on the inverse book in BTC; and
//! every row type with a `startTime` alone three days back, which measures the window a lone `startTime`
//! opens even when the account settled no funding. ⚠ Read every printed row before committing it.

use serde_json::{Value, json};

use crate::demo_label::demo_label;
use vike_bridge_core::capture::capture_frame;
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_bybit::execution::funding::LOG_PATH;
use vike_bybit::execution::rest::seed_endpoints;
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_model::{AccountTier, now_ms};
use vike_secrets::credentials::api_credentials;

const DAY: i64 = 86_400_000;

/// One signed GET of the transaction log, printed sanitized with what its rows cover; the next page's
/// cursor, if any.
fn read(
    t: &RestTransportJsonUreq,
    signer: &BybitV5Signer,
    tag: &str,
    params: &[(&str, Value)],
) -> Option<String> {
    read_at(t, signer, LOG_PATH, "transactionTime", tag, params)
}

/// [`read`] of any `path`, its rows' times read from `time_key`.
fn read_at(
    t: &RestTransportJsonUreq,
    signer: &BybitV5Signer,
    path: &str,
    time_key: &str,
    tag: &str,
    params: &[(&str, Value)],
) -> Option<String> {
    let shown: serde_json::Map<String, Value> =
        params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
    let demo = seed_endpoints(AccountTier::Demo).rest;
    let env = match t.signed(&demo, path, "GET", params, signer) {
        Ok(env) => env,
        Err(e) => {
            println!("REFUSED {tag} {}: [{}] {}", Value::Object(shown), e.code, e.msg);
            return None;
        }
    };
    let rows = env.pointer("/result/list").and_then(Value::as_array).cloned().unwrap_or_default();
    let times: Vec<i64> =
        rows.iter().filter_map(|r| r[time_key].as_str().and_then(|s| s.parse().ok())).collect();
    println!(
        "WINDOW {tag} {}: {} rows, transactionTime {:?}..{:?}",
        Value::Object(shown),
        rows.len(),
        times.iter().min(),
        times.iter().max()
    );
    let s = capture_frame("bybit", "transaction-log", &env);
    println!("CAPTURED {tag} redacted={:?} {}", s.redacted_paths, s.frame);
    env.pointer("/result/nextPageCursor")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

#[test]
#[ignore = "network + demo creds — a read-only CAPTURE, run by hand (see module doc)"]
fn bybit_funding_capture() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &demo_label(&directory), AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "bybit") else {
        println!("SKIP: the bybit DEMO credentials are absent");
        return;
    };
    let signer = BybitV5Signer::new(&creds, now_ms);
    let t = rest_transport();
    let now = now_ms();
    let linear = ("category", json!("linear"));
    let settlement = ("type", json!("SETTLEMENT"));
    let limit = ("limit", json!(50));
    let six_days = json!((now - 6 * DAY).to_string());

    read(
        &t,
        &signer,
        "linear-settlement-no-window",
        &[linear.clone(), settlement.clone(), limit.clone()],
    );
    read(
        &t,
        &signer,
        "linear-settlement-start-only",
        &[linear.clone(), settlement.clone(), limit.clone(), ("startTime", six_days.clone())],
    );
    let mut cursor: Option<String> = None;
    for page in 0..10 {
        let mut params = vec![
            linear.clone(),
            settlement.clone(),
            limit.clone(),
            ("startTime", six_days.clone()),
            ("endTime", json!(now.to_string())),
        ];
        if let Some(c) = &cursor {
            params.push(("cursor", json!(c)));
        }
        cursor = read(&t, &signer, &format!("linear-settlement-window-p{page}"), &params);
        if cursor.is_none() {
            break;
        }
    }
    read(
        &t,
        &signer,
        "inverse-settlement-window",
        &[
            ("category", json!("inverse")),
            ("currency", json!("BTC")),
            settlement,
            limit.clone(),
            ("startTime", six_days),
            ("endTime", json!(now.to_string())),
        ],
    );
    read(
        &t,
        &signer,
        "linear-any-start-only-3d",
        &[linear.clone(), limit.clone(), ("startTime", json!((now - 3 * DAY).to_string()))],
    );
    // Whether this account's log lists anything at all: every type over the last day, beside the fills
    // `execution/list` holds over its default window and the positions held now.
    read(&t, &signer, "linear-any-no-window", &[linear.clone(), limit.clone()]);
    read(&t, &signer, "unified-any-no-window", &[("accountType", json!("UNIFIED")), limit.clone()]);
    read_at(&t, &signer, "/v5/execution/list", "execTime", "executions", &[linear.clone(), limit]);
    read_at(
        &t,
        &signer,
        "/v5/position/list",
        "updatedTime",
        "positions",
        &[linear, ("settleCoin", json!("USDT"))],
    );
}
