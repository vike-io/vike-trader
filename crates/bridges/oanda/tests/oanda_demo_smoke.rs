//! LIVE demo smoke (network + fxPractice creds — run manually):
//!     cargo test -p vike-oanda --test oanda_demo_smoke -- --ignored --nocapture
//!
//! OANDA's `fxPractice` environment is a real, safe paper account (virtual balance, same v20
//! API as `fxTrade`) — the OANDA analogue of the crypto venues' testnet/demo accounts, so this
//! mirrors `crates/bridges/binance/tests/binance_demo_smoke.rs`'s ladder rather than falling back
//! to a connect-only smoke: submit a far-from-market resting LIMIT BUY (price = 0.10, nowhere near
//! any real EUR/USD rate, 1 unit — negligible even against virtual margin) through the REAL
//! `OandaExecutionClient` -> verify it rests in `pendingOrders` -> cancel -> verify it's gone.
//!
//! Credentials come from the workspace's gitignored `.env` (`OANDA_DEMO_API_KEY` /
//! `OANDA_DEMO_ACCOUNT_ID` — see `oanda_env_var_names`), loaded via
//! `vike_bridge_core::credentials::load_workspace_dotenv_from`. Absent creds -> the test SKIPS (the
//! live gate), never fails CI.

use std::time::Duration;

use vike_bridge_core::credentials::{load_workspace_dotenv_from, Environment};
use vike_exec::{event_channel, ExecutionClient, Ingest};
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};
use vike_oanda::{load_oanda_config_from, OandaExecutionClient, OandaRest};

use vike_model::clock::now_ms;

/// Drain the ingest channel until `pred` matches an `Event`, or `total` elapses (panics on
/// timeout — a demo-account submit/cancel that never acks is itself a failure worth seeing).
fn wait_for(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    total: Duration,
    mut pred: impl FnMut(&Event) -> bool,
) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + total;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let ingest = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("timed out waiting for a matching event")
                .expect("ingest channel closed");
            if let Ingest::Event(ev) = ingest {
                if pred(&ev) {
                    return ev;
                }
            }
        }
    })
}

/// Fetch pending (resting) orders and report whether `coid` is among them.
fn is_resting(rest: &OandaRest, base: &str, account_id: &str, coid: &str) -> bool {
    let path = format!("/v3/accounts/{account_id}/pendingOrders");
    let resp = rest.get(base, &path, "").expect("pendingOrders");
    resp.get("orders")
        .and_then(|o| o.as_array())
        .map(|orders| {
            orders
                .iter()
                .any(|o| o.pointer("/clientExtensions/id").and_then(|c| c.as_str()) == Some(coid))
        })
        .unwrap_or(false)
}

#[test]
#[ignore = "network + OANDA_DEMO (fxPractice) creds — run manually (see module doc)"]
fn oanda_demo_place_and_cancel_far_from_market_limit() {
    vike_log::test_init();
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let Some(config) = load_oanda_config_from(Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_oanda::smoke", "SKIP: OANDA_DEMO creds absent");
        return;
    };

    let (events, mut rx) = event_channel(64);
    let mut client = OandaExecutionClient::spawn(config.clone(), events);

    let coid = format!("vtroandasmoke{}", now_ms() % 100_000_000);
    let request = OrderRequest {
        client_order_id: coid.clone(),
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0, // 1 unit — negligible even on a real account, irrelevant on virtual fxPractice
        order_type: "limit".into(),
        price: Some(0.10), // nowhere near any real EUR/USD rate -> must rest, never fill
        time_in_force: TimeInForce::Gtc,
        ts: now_ms(),
        ..Default::default()
    };

    client.submit(&request);
    let accepted_or_rejected = wait_for(&mut rx, Duration::from_secs(20), |ev| {
        matches!(ev, Event::OrderAccepted(a) if a.client_order_id == coid)
            || matches!(ev, Event::OrderRejected(r) if r.client_order_id == coid)
    });
    match accepted_or_rejected {
        Event::OrderAccepted(_) => {
            tracing::info!(target: "vike_oanda::smoke", "order {coid} ACCEPTED")
        }
        Event::OrderRejected(r) => panic!("demo order {coid} was REJECTED: {}", r.reason),
        other => unreachable!("wait_for predicate guarantees Accepted|Rejected, got {other:?}"),
    }

    let rest = OandaRest::new(config.api_token.clone());
    assert!(
        is_resting(&rest, &config.rest_base, &config.account_id, &coid),
        "order {coid} must be resting in pendingOrders after acceptance"
    );
    tracing::info!(target: "vike_oanda::smoke", "order {coid} confirmed resting");

    client.cancel(&coid);
    wait_for(
        &mut rx,
        Duration::from_secs(20),
        |ev| matches!(ev, Event::OrderCanceled(c) if c.client_order_id == coid),
    );

    assert!(
        !is_resting(&rest, &config.rest_base, &config.account_id, &coid),
        "order {coid} must be gone from pendingOrders after cancel"
    );
    tracing::info!(target: "vike_oanda::smoke", "cancel confirmed — demo ladder green");

    drop(client); // deterministic teardown (ExecActor::drop joins the exec + stream threads)
}
