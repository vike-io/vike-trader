//! The exec loop's own door, below the client that stamps an order: how long it waited is said here
//! without waiting that long.
use super::*;
use std::time::{Duration, Instant};
use vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::AccountTier;

/// **A modify under HALT is answered**, a non-terminal refusal in the shared words, where the client's
/// inherited no-op said nothing. Any existing file stands for the sentinel.
#[test]
fn a_modify_under_halt_is_refused_in_words() {
    let (events, mut ingest) = event_channel(8);
    let halt = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let actor = ExecutionClientActor::spawn("alpaca-halt-test", events.clone(), |_rx| {})
        .with_halt_path(halt);
    let mut client = with_submitted_symbols(actor, submitted_symbols());
    let order = OrderRequest { order_client_id: "resting".into(), ..Default::default() };
    client.modify(&order, Some(2.0), None);
    client.detach();
    match ingest.try_recv() {
        Ok(Ingest::Event(Event::OrderModifyRejected(r))) => {
            assert_eq!(r.order_client_id, "resting");
            assert_eq!(r.reason, vike_execution::halt::HALT_REJECT_REASON);
        }
        other => panic!("expected the HALT refusal, got {other:?}"),
    }
}

/// **An order that waited past its time on this thread's queue is refused by name, costs no call, and
/// leaves the submitted-symbol table.** Port 9: the POST would have answered with a network error.
#[test]
fn an_order_past_its_wait_is_refused_and_never_reaches_the_wire() {
    let dead = "http://127.0.0.1:9";
    let hosts =
        crate::AlpacaUrls { authx: dead.into(), broker: dead.into(), data_websocket: dead.into() };
    let config = AlpacaConfig {
        client_id: vike_model::secret::Secret::new("id".into()),
        client_secret: vike_model::secret::Secret::new("sec".into()),
        account_id: "acct".into(),
        env: AccountTier::Demo,
        hosts,
    };
    let token = Arc::new(TokenSource::new("id".into(), "sec".into(), dead.into()));
    let (events, mut ingest) = event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel();
    let order =
        OrderRequest { order_client_id: "old".into(), symbol: "AAPL".into(), ..Default::default() };
    let symbols = submitted_symbols();
    symbols.record("old", "AAPL");
    let stale = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    tx.send(ExecutionClientActorCommand::Submit(Box::new(order), stale)).unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run(config, token, events, rx, symbols.clone());

    let mut said = Vec::new();
    while let Ok(Ingest::Event(e)) = ingest.try_recv() {
        said.push(e);
    }
    match said.as_slice() {
        [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
            assert!(r.reason.contains("NOT sent"), "{}", r.reason);
        }
        other => panic!("expected [Submitted, Rejected], got {other:?}"),
    }
    assert_eq!(symbols.label("old", "AAPLX"), "AAPLX", "the refused order's entry left");
}

/// **A close past its wait is not refused for its age.** This venue ignores the flag, so it asks the position first;
/// nothing answers on port 9, a failed read is no evidence, and the POST is tried: the dead port leaves it UNKNOWN or
/// rejected in the transport's words, never "NOT sent".
#[test]
fn a_close_past_its_wait_goes_on_to_the_wire() {
    let dead = "http://127.0.0.1:9";
    let hosts =
        crate::AlpacaUrls { authx: dead.into(), broker: dead.into(), data_websocket: dead.into() };
    let config = AlpacaConfig {
        client_id: vike_model::secret::Secret::new("id".into()),
        client_secret: vike_model::secret::Secret::new("sec".into()),
        account_id: "acct".into(),
        env: AccountTier::Demo,
        hosts,
    };
    let token = Arc::new(TokenSource::new("id".into(), "sec".into(), dead.into()));
    let (events, mut ingest) = event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel();
    let order = OrderRequest {
        order_client_id: "leg".into(),
        symbol: "AAPL".into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        ..Default::default()
    };
    let stale = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    tx.send(ExecutionClientActorCommand::Submit(Box::new(order), stale)).unwrap();
    tx.send(ExecutionClientActorCommand::Shutdown).unwrap();
    run(config, token, events, rx, submitted_symbols());

    while let Ok(Ingest::Event(e)) = ingest.try_recv() {
        if let Event::OrderRejected(r) = e {
            assert!(!r.reason.contains("NOT sent"), "{}", r.reason);
        }
    }
}
