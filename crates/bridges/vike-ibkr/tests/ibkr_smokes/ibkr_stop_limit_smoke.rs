//! LIVE PAPER smoke - a STOP-LIMIT through the bridge (wave 1 of the new order kinds). Places ONE real buy
//! `STP LMT` of 1 AAPL far above the market (trigger 5000, limit 5001: it rests untriggered and cannot fill) on the
//! PAPER account, amends its LIMIT to 5002, and cancels it.
//!
//! ⚠ **NOT RUN YET. It runs on MONDAY 2026-11-09 OR LATER, never before**: IBKR is an FX broker under the owner's rule
//! of 2026-11-06, and no broker contact of any kind is allowed before that Monday. Everything it checks is proven
//! offline only until then (`src/execution/order.rs`'s `stop_limit_maps_the_limit_and_the_trigger`, `src/transport/socket.rs`'s
//! `a_stop_limit_is_built_with_its_limit_and_its_trigger`, `tests/ibkr_amend.rs`'s
//! `a_stop_limit_amend_moves_the_limit_and_keeps_the_trigger`).
//!
//! What only a gateway can answer, one line each in the printed `STOP-LIMIT` table:
//!
//! 1. **TWS takes `STP LMT` with `lmtPrice` the limit and `auxPrice` the trigger**: `OrderAccepted`, or an
//!    `OrderRejected` carrying IB's words (never a silence: an unlisted rejection code is the core watchdog's, and this
//!    smoke reports it as NO ANSWER).
//! 2. **The amend moves the limit and keeps the trigger**: TWS's `openOrder` echo of the re-placed order carries the new
//!    limit, which the bridge reports as `OrderModified` with that price (`exec::amend`'s `PendingAmends` matches a
//!    stop-limit's echo on its limit), or an `OrderModifyRejected` saying why.
//! 3. **The cancel ends it**: `OrderCanceled`.
//!
//! It dials the exec client under a client id of its own (from the clock, never one the daemon dials) and the
//! order's order client id is `b4ibsl1`. IB may answer a precaution about the far prices; the table then carries IB's words, and
//! the fix is prices nearer the market (still above it), never a bypassed precaution. The only hard failures are a
//! target that is not a paper `DU...` account and a run in which the submit was not answered at all. A run that prints
//! no `STOP-LIMIT` table tested nothing (it self-skipped), whatever its exit code says.
//!
//! ⚠ It PLACES, AMENDS AND CANCELS A REAL PAPER ORDER. Never run it casually; the lane, the credential store and the
//! fence are `docs/ops/live-smoke-lane.md`'s, and no workflow runs it. On a the CI box lane (the gateway is on the box),
//! on 2026-11-09 or later:
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr --test ibkr_smokes -- --ignored --nocapture ibkr_stop_limit_smoke::
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account holding an `ACCOUNT`, on
//! the `socket` backend, exists and the gateway answers; and a hard assert refuses anything but a paper `DU...` account.
#![cfg(feature = "ibkr")]

use crate::common::ibkr_settings;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::error::TryRecvError;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::connect_execution_client;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, OrderType};
use vike_venues::IbkrBackend;

/// Far above any AAPL price: a buy stop there rests untriggered, so the order can never fill.
const TRIGGER: f64 = 5_000.0;
const LIMIT: f64 = 5_001.0;
const AMENDED_LIMIT: f64 = 5_002.0;
const ORDER_CLIENT_ID: &str = "b4ibsl1";

/// A client id this gateway has not seen from us: from the clock (the daemon's ids are small and configured).
fn fresh_id() -> i32 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    crate::common::SMOKE_CLIENT_ID_BASE + 8_000 + (secs % 8_000) as i32
}

/// The first event about `ORDER_CLIENT_ID` that `pick` answers, within `window`: `(verdict, detail)`, or NO ANSWER.
fn wait_for(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    window: Duration,
    pick: impl Fn(&Event) -> Option<(&'static str, String)>,
) -> (&'static str, String) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(e)) => {
                if let Some(answer) = pick(&e) {
                    return answer;
                }
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(50)),
            Err(TryRecvError::Disconnected) => break,
        }
    }
    ("NO ANSWER", format!("nothing within {} s", window.as_secs()))
}

#[test]
#[ignore = "live paper, Monday 2026-11-09 or later: places, amends and cancels one real paper STP LMT order; needs a running Gateway logged into a DU... account"]
fn ibkr_stop_limit_place_amend_cancel() {
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
    if config.backend == IbkrBackend::Cpapi {
        eprintln!(
            "skip: venue.ibkr.demo.backend is cpapi; this smoke speaks the IB Gateway socket"
        );
        return;
    }
    // HARD SAFETY: paper only - this assert stays a hard failure, never a skip.
    assert!(
        config.account.starts_with("DU"),
        "smoke must target a paper (DU\u{2026}) account, got {}",
        config.account
    );
    config.client_id = fresh_id();
    let (events, mut rx) = event_channel(256);
    let mut client = match connect_execution_client(&config, events) {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("skip: the bridge could not connect (client id {}): {e}", config.client_id);
            return;
        }
    };

    let order = OrderRequest {
        order_client_id: ORDER_CLIENT_ID.into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::StopLimit,
        price: Some(LIMIT),
        trigger_price: Some(TRIGGER),
        ..Default::default()
    };
    client.submit(&order);
    let mut table = vec![(
        "submit STP LMT",
        wait_for(&mut rx, Duration::from_secs(20), |e| match e {
            Event::OrderAccepted(a) if a.order_client_id == ORDER_CLIENT_ID => {
                Some(("ACCEPTED", "OrderAccepted".into()))
            }
            Event::OrderRejected(r) if r.order_client_id == ORDER_CLIENT_ID => {
                Some(("REFUSED", format!("OrderRejected: {}", r.reason)))
            }
            _ => None,
        }),
    )];
    let accepted = table[0].1.0 == "ACCEPTED";

    if accepted {
        client.modify(&order, None, Some(AMENDED_LIMIT));
        table.push((
            "amend the limit",
            wait_for(&mut rx, Duration::from_secs(15), |e| match e {
                Event::OrderModified(m) if m.order_client_id == ORDER_CLIENT_ID => Some((
                    if m.new_price == Some(AMENDED_LIMIT) { "MODIFIED" } else { "OTHER TERMS" },
                    format!("OrderModified: qty {:?}, price {:?}", m.new_qty, m.new_price),
                )),
                Event::OrderModifyRejected(r) if r.order_client_id == ORDER_CLIENT_ID => {
                    Some(("REFUSED", format!("OrderModifyRejected: {}", r.reason)))
                }
                _ => None,
            }),
        ));
    }
    // Cancel whatever the answer was: an order IB holds must not be left resting.
    client.cancel(ORDER_CLIENT_ID);
    table.push((
        "cancel",
        wait_for(&mut rx, Duration::from_secs(10), |e| match e {
            Event::OrderCanceled(c) if c.order_client_id == ORDER_CLIENT_ID => {
                Some(("CANCELED", "OrderCanceled".into()))
            }
            Event::OrderCancelRejected(r) if r.order_client_id == ORDER_CLIENT_ID => {
                Some(("REFUSED", format!("OrderCancelRejected: {}", r.reason)))
            }
            _ => None,
        }),
    ));
    client.detach();

    eprintln!(
        "STOP-LIMIT (AAPL.SMART.USD, buy 1 STP LMT trigger {TRIGGER} limit {LIMIT} -> {AMENDED_LIMIT}, paper)"
    );
    for (step, (verdict, detail)) in &table {
        eprintln!("  {step:<16}  {verdict:<11}  {detail}");
    }
    assert_ne!(table[0].1.0, "NO ANSWER", "the submit was not answered: nothing was measured");
}
