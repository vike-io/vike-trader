//! Proves `AlpacaExecutionClient::spawn` wires the real `ExecActor` + `AlpacaRest` path:
//! `submit` emits `OrderSubmitted` synchronously, and a transport failure (an unroutable host)
//! yields a terminal `OrderRejected` — the venue-adapter contract that an order intent never
//! silently vanishes. Mirrors OANDA's `exec_actor_wiring.rs` shape but drives the REAL network
//! path against a dead host rather than a fake command loop, since Alpaca's OAuth token
//! exchange + order POST are both exercised end-to-end here.

use std::collections::HashMap;
use tokio::sync::mpsc;
use vike_alpaca::AlpacaExecutionClient;
use vike_bridge_core::credentials::Environment;
use vike_exec::ExecutionClient;
use vike_exec::lanes::{EventSender, Ingest};
use vike_model::events::Event;
use vike_model::{OrderRequest, TimeInForce};

fn cfg() -> vike_alpaca::AlpacaConfig {
    // Point hosts at an unroutable base so the POST fails fast → terminal OrderRejected.
    let mut vars = HashMap::new();
    vars.insert("ALPACA_SANDBOX_CLIENT_ID".into(), "id".into());
    vars.insert("ALPACA_SANDBOX_CLIENT_SECRET".into(), "sec".into());
    vars.insert("ALPACA_SANDBOX_ACCOUNT_ID".into(), "acct".into());
    let mut c = vike_alpaca::load_alpaca_config_from(Environment::Demo, &vars).unwrap();
    c.hosts.authx = "http://127.0.0.1:9";
    c.hosts.broker = "http://127.0.0.1:9";
    c
}

fn order() -> OrderRequest {
    OrderRequest {
        client_order_id: "c1".into(),
        venue: "alpaca".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        reduce_only: false,
        time_in_force: TimeInForce::Day,
        gtd_expiry: None,
        ts: 1,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
        combo_legs: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_emits_submitted_then_rejected_on_dead_host() {
    let (tx, mut rx) = mpsc::channel::<Ingest>(64);
    let mut client =
        AlpacaExecutionClient::spawn(cfg(), EventSender { ingest: tx, route_key: None });
    client.submit(&order());

    let mut saw_submitted = false;
    let mut saw_rejected = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !(saw_submitted && saw_rejected) {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(Ingest::Event(Event::OrderSubmitted(s)))) if s.client_order_id == "c1" => {
                saw_submitted = true
            }
            Ok(Some(Ingest::Event(Event::OrderRejected(r)))) if r.client_order_id == "c1" => {
                saw_rejected = true
            }
            Ok(Some(_)) => {}
            _ => {}
        }
    }
    assert!(saw_submitted, "OrderSubmitted not observed");
    assert!(saw_rejected, "OrderRejected (dead host) not observed");
    client.detach();
}
