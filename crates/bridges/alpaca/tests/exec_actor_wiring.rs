//! Proves `AlpacaExecutionClient::spawn` wires the real `ExecActor` + `AlpacaRest` path:
//! `submit` emits `OrderSubmitted` synchronously, and a transport failure (an unroutable host)
//! yields a terminal `OrderRejected` — the venue-adapter contract that an order intent never
//! silently vanishes. Mirrors OANDA's `exec_actor_wiring.rs` shape but drives the REAL network
//! path against a dead host rather than a fake command loop, since Alpaca's OAuth token
//! exchange + order POST are both exercised end-to-end here.

use tokio::sync::mpsc;
use vike_alpaca::AlpacaExecutionClient;
use vike_exec::ExecutionClient;
use vike_exec::{EventSender, Ingest};
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, TimeInForce};
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};

fn cfg() -> vike_alpaca::AlpacaConfig {
    // Point hosts at an unroutable base so the POST fails fast → terminal OrderRejected.
    let fields = CredentialFields::from_pairs([
        ("CLIENT_ID", "id"),
        ("CLIENT_SECRET", "sec"),
        ("ACCOUNT_ID", "acct"),
    ]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    let mut c = vike_alpaca::load_alpaca_config_from(AccountTier::Demo, &creds).unwrap();
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
        order_type: vike_model::OrderType::Market,
        price: None,
        trigger_price: None,
        time_in_force: TimeInForce::Day,
        ts: 1,
        ..Default::default()
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
