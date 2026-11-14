//! Live PAPER smoke for the **cpapi** backend's two STOP kinds (wave 1 of the new order kinds): places, reads back and
//! cancels ONE far buy `STP` and ONE far buy `STOP_LIMIT` of 1 AAPL on the IBKR PAPER account, through a running,
//! browser-authenticated Client Portal Gateway, by the production path (`connect_execution_client` over the cpapi transport,
//! whose `order_json` builds the body). Both triggers sit far above any AAPL price, so neither order can trigger or fill.
//!
//! ⚠ **NOT RUN YET. It runs on MONDAY 2026-11-09 OR LATER, never before**: IBKR is an FX broker under the owner's rule
//! of 2026-11-06, and no broker contact of any kind is allowed before that Monday.
//!
//! What it measures, which no capture has yet (`transport/cpapi/mod.rs`'s module doc and `order_json`'s doc):
//!
//! 1. **A stop's trigger rides `auxPrice`** with `price` null, as IBKR's schema at
//!    `https://api.ibkr.com/gw/api/v3/api-docs` says ("Stop or trailing price. Required for `STP`, `STOP_LIMIT`, and
//!    trailing order types."). The July capture placed only a limit order.
//! 2. **A stop-limit is `orderType` `STOP_LIMIT`**, the limit in `price` and the trigger in `auxPrice`.
//! 3. **Each rests and reads back with those terms**: the order's row in `GET /iserver/account/orders`, found by the
//!    order client id it carries (`order_ref`, `cOID` or `local_order_id`), is printed whole, so its `orderType`, `price`,
//!    `auxPrice` and `orderId` are on the page.
//! 4. **Each cancels**: `OrderCanceled`.
//!
//! It prints the bridge's events (`OrderSubmitted`, `OrderAccepted` or `OrderRejected` with IBKR's words, `OrderCanceled`)
//! and the read-back rows, then a `CP STOP KINDS` table. It prints no credential: an IBKR account holds none here (the
//! gateway session is the login), and nothing reads a key. The only hard failures are an account or a gateway SESSION
//! that is not a paper `DU...` account, and a run in which neither order was answered. A run that prints no
//! `CP STOP KINDS` table tested nothing (it self-skipped), whatever its exit code says.
//!
//! ⚠ It PLACES AND CANCELS TWO REAL PAPER ORDERS. Never run it casually; the lane, the credential store and the fence
//! are `docs/ops/live-smoke-lane.md`'s, and no workflow runs it. With the paper CP Gateway up and logged in, on
//! 2026-11-09 or later:
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr --test ibkr_smokes -- --ignored --nocapture ibkr_cpapi_stop_kinds_smoke::
//!
//! Gated like `ibkr_cpapi_smoke.rs`, its twin for a limit order: `#[ignore]`; self-skips unless a demo ibkr account
//! holding an `ACCOUNT` exists and the gateway answers; hard asserts refuse a non-`DU` account and a gateway session on
//! a non-`DU` account (cpapi routes by the SESSION, not by `ACCOUNT`).
#![cfg(feature = "ibkr")]

use crate::common::ibkr_settings;

use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::error::TryRecvError;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::transport::CpapiRest;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType};
use vike_venues::IbkrBackend;

/// Far above any AAPL price: a buy stop there rests untriggered, so neither order can fill.
const TRIGGER: f64 = 5_000.0;
const LIMIT: f64 = 5_001.0;

/// The first event about `order_client_id` that `pick` answers within `window`, printing every event that comes meanwhile.
fn wait_for(
    rx: &mut Receiver<Ingest>,
    order_client_id: &str,
    window: Duration,
    pick: impl Fn(&Event) -> Option<(&'static str, String)>,
) -> (&'static str, String) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(e)) => {
                println!("  event: {e:?}");
                if let Some(answer) = pick(&e) {
                    return answer;
                }
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => break,
        }
    }
    ("NO ANSWER", format!("nothing about {order_client_id} within {} s", window.as_secs()))
}

/// The order client id a `GET /iserver/account/orders` row carries, in the spellings `transport/cpapi/decode.rs`'s
/// `row_order_client_id` reads.
fn row_order_client_id(row: &Value) -> Option<&str> {
    ["order_ref", "cOID", "local_order_id"]
        .iter()
        .find_map(|k| row.get(*k).and_then(Value::as_str).filter(|v| !v.is_empty()))
}

/// The venue's open-order row of `order_client_id`, asked for up to `window` (the gateway's order cache lags a placement).
fn read_back(rest: &CpapiRest, order_client_id: &str, window: Duration) -> Result<Value, String> {
    let deadline = Instant::now() + window;
    let mut last = String::from("no read made");
    while Instant::now() < deadline {
        let _ = rest.open_orders_forced();
        match rest.open_orders() {
            Ok(v) => {
                let rows = v.get("orders").and_then(Value::as_array).cloned().unwrap_or_default();
                if let Some(row) =
                    rows.into_iter().find(|r| row_order_client_id(r) == Some(order_client_id))
                {
                    return Ok(row);
                }
                last = format!("no row carries {order_client_id} yet");
            }
            Err(e) => last = format!("open orders read failed: {e:?}"),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(last)
}

#[test]
#[ignore = "live paper, Monday 2026-11-09 or later: places, reads back and cancels one STP and one STOP_LIMIT on the paper CP Gateway"]
fn cpapi_paper_stop_and_stop_limit_place_read_back_cancel() {
    vike_log::test_init();

    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("ibkr", &AccountLabel::Default, AccountTier::Demo);
    let Some(mut config) =
        load_ibkr_config_from(AccountTier::Demo, &creds, &ibkr_settings(settings_dir.as_deref()))
    else {
        eprintln!("skip: no demo ibkr account holding an `ACCOUNT` in the settings database");
        return;
    };
    config.backend = IbkrBackend::Cpapi; // force cpapi regardless of venue.ibkr.demo.backend
    // The cpapi backend refuses to start with the extended-hours switch on, and neither stop is ever flagged.
    config.extended_hours = false;
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        config.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        config.account
    );
    // HARD SAFETY (cpapi): orders follow the gateway's logged-in SESSION, not `config.account`, so the session's account
    // must be a paper one too (read-only `GET /iserver/accounts`). Unreachable or unauthenticated: skip.
    let rest = CpapiRest::new(&config.cpapi_url, &config.account);
    match rest.accounts() {
        Ok(v) => {
            let session = v
                .get("accounts")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                session.starts_with("DU"),
                "cpapi gateway SESSION is on '{session}', not a paper DU\u{2026} account: refusing to place an \
                 order (log the gateway into the paper account first)"
            );
            eprintln!("session-account guard OK: gateway session is {session}");
        }
        Err(e) => {
            eprintln!(
                "skip: could not read gateway /iserver/accounts (unreachable/unauth?): {e:?}"
            );
            return;
        }
    }

    let (events, mut rx) = event_channel(256);
    let mut client = match vike_ibkr::connect_execution_client(&config, events) {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("skip: CP Gateway unreachable/unauthenticated: {e}");
            return;
        }
    };

    let pid = std::process::id();
    let orders = [
        ("STP", format!("b4cp-stp-{pid}"), OrderType::Stop, None),
        ("STOP_LIMIT", format!("b4cp-sl-{pid}"), OrderType::StopLimit, Some(LIMIT)),
    ];
    let mut table: Vec<(&str, String, &str, String, String, &str)> = Vec::new();
    for (kind, order_client_id, order_type, price) in orders {
        let order = OrderRequest {
            order_client_id: order_client_id.clone(),
            venue: "ibkr".into(),
            symbol: "AAPL.SMART.USD".into(),
            side: 1,
            qty: 1.0,
            order_type,
            price,
            trigger_price: Some(TRIGGER),
            ..Default::default()
        };
        println!(
            "submitting {kind} {order_client_id}: buy 1 AAPL, trigger {TRIGGER}, limit {price:?}"
        );
        client.submit(&order);
        let (placed, placed_detail) =
            wait_for(&mut rx, &order_client_id, Duration::from_secs(20), |e| match e {
                Event::OrderAccepted(a) if a.order_client_id == order_client_id => {
                    Some(("ACCEPTED", format!("{a:?}")))
                }
                Event::OrderRejected(r) if r.order_client_id == order_client_id => {
                    Some(("REFUSED", format!("IBKR's words: {}", r.reason)))
                }
                _ => None,
            });
        let row = if placed == "ACCEPTED" {
            match read_back(&rest, &order_client_id, Duration::from_secs(10)) {
                Ok(row) => {
                    println!("  read back {order_client_id}: {row}");
                    format!(
                        "orderId {} orderType {} price {} auxPrice {} status {}",
                        row.get("orderId").unwrap_or(&Value::Null),
                        row.get("orderType").unwrap_or(&Value::Null),
                        row.get("price").unwrap_or(&Value::Null),
                        row.get("auxPrice").unwrap_or(&Value::Null),
                        row.get("status").unwrap_or(&Value::Null)
                    )
                }
                Err(why) => format!("NOT READ BACK: {why}"),
            }
        } else {
            "not read back (not accepted)".to_string()
        };
        // Cancel whatever the answer was: an order the gateway holds must not be left resting.
        println!("cancelling {order_client_id}");
        client.cancel(&order_client_id);
        let (canceled, _) =
            wait_for(&mut rx, &order_client_id, Duration::from_secs(15), |e| match e {
                Event::OrderCanceled(c) if c.order_client_id == order_client_id => {
                    Some(("CANCELED", String::new()))
                }
                Event::OrderCancelRejected(r) if r.order_client_id == order_client_id => {
                    Some(("CANCEL REFUSED", r.reason.to_string()))
                }
                _ => None,
            });
        table.push((kind, order_client_id, placed, placed_detail, row, canceled));
    }
    client.detach();

    eprintln!(
        "CP STOP KINDS (AAPL.SMART.USD, buy 1, trigger {TRIGGER}, stop-limit limit {LIMIT}, paper)"
    );
    for (kind, order_client_id, placed, detail, row, canceled) in &table {
        eprintln!(
            "  {kind:<10}  {order_client_id}  {placed:<9}  {canceled}\n      {detail}\n      {row}"
        );
    }
    assert!(
        table.iter().any(|(_, _, placed, ..)| *placed != "NO ANSWER"),
        "neither order was answered: nothing was measured"
    );
}
