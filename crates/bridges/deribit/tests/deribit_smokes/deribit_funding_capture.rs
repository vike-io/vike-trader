//! **What does the TESTNET demo account's transaction log say about perpetual funding?** A READ-ONLY
//! capture, printed for the `testnet_2026_11_08` half of `crates/bridges/deribit/tests/fixtures/deribit_funding_log.json`
//! and run by hand:
//!
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-deribit --test deribit_smokes -- --ignored --nocapture deribit_funding_capture::
//!
//! Every frame it sends is a `get_*` read on `TESTNET_WEBSOCKET` (nothing is placed, cancelled or moved): the BTC
//! account summary (`balance` and `session_rpl`, what the recon client adopts), the `BTC-PERPETUAL` position
//! (`realized_funding`), and [`DAYS`] days of the BTC transaction log, each page printed whole and sanitized
//! (`vike_bridge_core::capture::capture_frame`, and every row's `ip` dropped). Then the
//! production poller (`vike_deribit::execution::funding::DeribitFundingPoller`) reads the same window and prints the
//! events it would emit. ⚠ Read every printed row before committing it: an identifier the sanitizer does not
//! know prints in the clear.
//!
//! MEASURED 2026-11-08 (the default demo account): 55 rows over 30 days (50 `trade`, 4 `settlement`, 1
//! `options_settlement_summary`), every `BTC-PERPETUAL` row `interest_pl: 0.0` and the position's
//! `realized_funding: 0.0` (each position was held for seconds), so the poller emitted nothing. A non-zero row
//! needs a position held across funding, which a read cannot make: the fixture's `documented` page is the
//! venue's own example for that.
//!
//! `deribit_funding_capture_usdc` reads the same for the LINEAR `BTC_USDC-PERPETUAL` in USDC: whether that
//! wallet's `balance` takes a trade's realized PnL and funding at the trade or only at the 08:00 settlement
//! (each log row states the `balance` after it, beside `change`, `cashflow`, `interest_pl` and `session_rpl`).

use serde_json::{Value, json};

use crate::demo_credentials::load_demo_credentials_loudly;
use vike_bridge_core::capture::capture_frame;
use vike_deribit::execution::funding::{DeribitFundingPoller, PATH_TRANSACTION_LOG};
use vike_deribit::test_support::TESTNET_WEBSOCKET;
use vike_deribit::transport::client::DeribitTransport;
use vike_model::events::Event;

/// How far back the log is read.
const DAYS: i64 = 30;

/// One read's `result`, or the venue's refusal as text.
fn read(t: &mut DeribitTransport, method: &str, params: &Value) -> Result<Value, String> {
    let response =
        t.call(method, params).map_err(|e| format!("[{}] {}", e.error().code, e.error().msg))?;
    match response.get("error") {
        Some(err) if !err.is_null() => Err(err.to_string()),
        _ => response.get("result").cloned().ok_or_else(|| "no result".to_string()),
    }
}

/// `value` sanitized, with every row's `ip` dropped (the sanitizer does not know the key).
fn clean(kind: &str, value: &Value) -> String {
    let mut frame = capture_frame("deribit", kind, value).frame;
    let rows: Vec<&mut Value> = if frame.get("logs").is_some() {
        frame["logs"].as_array_mut().map(|a| a.iter_mut().collect()).unwrap_or_default()
    } else {
        vec![&mut frame]
    };
    for row in rows {
        if let Some(map) = row.as_object_mut() {
            map.remove("ip");
        }
    }
    serde_json::to_string(&frame).unwrap_or_default()
}

#[test]
#[ignore = "network + demo creds — a read-only CAPTURE, run by hand (see module doc)"]
fn deribit_funding_capture() {
    capture("BTC-PERPETUAL", "BTC");
}

#[test]
#[ignore = "network + demo creds — a read-only CAPTURE, run by hand (see module doc)"]
fn deribit_funding_capture_usdc() {
    capture("BTC_USDC-PERPETUAL", "USDC");
}

fn capture(instrument: &str, currency: &str) {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials_loudly() else { return };
    let mut t = DeribitTransport::new(
        TESTNET_WEBSOCKET,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        None,
    );
    if let Err(e) = t.connect() {
        println!("SKIP: deribit demo order-WS auth failed: {e}");
        return;
    }

    match read(&mut t, "private/get_account_summary", &json!({ "currency": currency })) {
        Ok(s) => println!(
            "CAPTURED account_summary {currency}: {}",
            json!({ "balance": s["balance"], "session_rpl": s["session_rpl"], "session_upl": s["session_upl"],
                    "equity": s["equity"], "total_pl": s["total_pl"], "currency": s["currency"] })
        ),
        Err(e) => println!("REFUSED account_summary: {e}"),
    }
    match read(&mut t, "private/get_position", &json!({ "instrument_name": instrument })) {
        Ok(p) => println!("CAPTURED position {instrument}: {}", clean("position", &p)),
        Err(e) => println!("REFUSED position: {e}"),
    }

    let now = vike_model::now_ms();
    let start = now - DAYS * 86_400_000;
    let mut params = json!({ "currency": currency, "start_timestamp": start, "end_timestamp": now, "count": 1000 });
    let mut types: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for page_no in 0..20 {
        let page = match read(&mut t, PATH_TRANSACTION_LOG, &params) {
            Ok(p) => p,
            Err(e) => {
                println!("REFUSED transaction_log page {page_no}: {e}");
                break;
            }
        };
        let logs = page["logs"].as_array().cloned().unwrap_or_default();
        for row in &logs {
            *types.entry(row["type"].as_str().unwrap_or("?").to_string()).or_default() += 1;
        }
        println!("CAPTURED transaction_log page {page_no}: {}", clean("transaction-log", &page));
        match page["continuation"].as_i64() {
            Some(c) if !logs.is_empty() => params["continuation"] = json!(c),
            _ => break,
        }
    }
    println!("ROW TYPES over {DAYS} days: {types:?}");

    let mut poller = DeribitFundingPoller::new(
        |p: &Value| read(&mut t, PATH_TRANSACTION_LOG, p),
        instrument,
        currency,
        start,
    );
    match poller.poll(now) {
        Ok(events) => {
            let mut sum = 0.0;
            for event in &events {
                if let Event::Funding(f) = event {
                    sum += f.amount;
                    println!(
                        "FUNDING EVENT ts {} amount {} {:?}",
                        f.timestamp, f.amount, f.currency
                    );
                }
            }
            println!("FUNDING: {} events, sum {sum} {currency}", events.len());
        }
        Err(e) => println!("POLL FAILED: {e}"),
    }
}
