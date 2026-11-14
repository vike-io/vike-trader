//! Live sandbox smokes — #[ignore], self-skip unless the credential store holds a DEMO alpaca
//! account (its `CLIENT_ID` / `CLIENT_SECRET` / `ACCOUNT_ID`).
//! Run one at a time, e.g.:
//!   cargo test -p vike-alpaca --test alpaca_live_smoke -- --ignored --nocapture alpaca_oauth_login

use std::sync::Arc;
use vike_alpaca::{AlpacaTransport, TokenSource, create_test_account, fund_test_account};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

/// The default DEMO alpaca account's config, from the credential store; `None` (→ self-skip) when
/// the store holds no such account or it lacks a field.
fn config() -> Option<vike_alpaca::AlpacaConfig> {
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("alpaca", &AccountLabel::Default, AccountTier::Demo);
    let seed = vike_venues::venue_urls::VenueUrls::seed("alpaca");
    vike_alpaca::load_alpaca_config_from(AccountTier::Demo, &creds, &seed)
        .map(|c| c.expect("the seed's sandbox rows"))
}

fn rest_and_config() -> Option<(AlpacaTransport, vike_alpaca::AlpacaConfig)> {
    let c = config()?;
    let token = Arc::new(TokenSource::new(
        c.client_id.expose().clone(),
        c.client_secret.expose().clone(),
        c.hosts.authx.to_string(),
    ));
    Some((AlpacaTransport::new(token), c))
}

#[test]
#[ignore = "live sandbox; needs a demo alpaca account's credentials"]
fn alpaca_oauth_login() {
    let Some((rest, c)) = rest_and_config() else {
        eprintln!("skip: no creds");
        return;
    };
    let accounts = rest.get(&c.hosts.broker, "/v1/accounts", "page_size=2").expect("list accounts");
    println!("accounts: {accounts}");
    assert!(accounts.is_array());
}

#[test]
#[ignore = "live sandbox; creates a test account"]
fn alpaca_sandbox_provision() {
    let Some((rest, c)) = rest_and_config() else {
        eprintln!("skip: no creds");
        return;
    };
    let email = format!("vike.test.{}@example.com", std::process::id());
    let id = create_test_account(&rest, &c, &email, "587-24-9310").expect("create account");
    println!("created account_id={id}");
    // Best-effort fund; print the response so we can confirm/iterate the transfer body shape.
    match fund_test_account(&rest, &c, &id, 100000.0) {
        Ok(v) => println!("fund resp: {v}"),
        Err(e) => println!("fund err (iterate body): {e}"),
    }
    assert_eq!(id.len(), 36);
}

#[test]
#[ignore = "live sandbox; places a real crypto order on the pinned account"]
fn alpaca_execution_smoke() {
    use tokio::sync::mpsc;
    use vike_execution::{EventSender, ExecutionClient, Ingest};
    use vike_model::events::Event;
    use vike_model::{OrderRequest, TimeInForce};

    let Some(c) = config() else {
        eprintln!("skip: no creds");
        return;
    };
    let rt =
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async move {
        let (tx, mut rx) = mpsc::channel::<Ingest>(256);
        let mut client =
            vike_alpaca::execution::spawn(c, EventSender { ingest: tx, route_key: None });
        let order = OrderRequest {
            order_client_id: format!("smoke-{}", std::process::id()),
            venue: "alpaca".into(),
            symbol: "BTCUSD".into(),
            side: 1,
            qty: 0.001,
            order_type: vike_model::OrderType::Market,
            price: None,
            trigger_price: None,
            time_in_force: TimeInForce::Gtc,
            timestamp: 1,
            ..Default::default()
        };
        client.submit(&order);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(Ingest::Event(event))) =
                tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
            {
                println!("event: {event:?}");
                if matches!(event, Event::OrderFilled(_) | Event::OrderRejected(_)) {
                    break;
                }
            }
        }
        client.detach();
    });
}

#[test]
#[ignore = "live sandbox; subscribes to crypto quotes"]
fn alpaca_data_smoke() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vike_data::{DataClient, DataClientSink};
    // A counting sink.
    struct CountSink(Arc<AtomicUsize>);
    impl DataClientSink for CountSink {
        fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<vike_marketdata::Bar>) {}
        fn close_bar(&self, _: &str, _: &str, _: &str, _: vike_marketdata::Bar) {}
        fn forming_bar(&self, _: &str, _: &str, _: &str, _: vike_marketdata::Bar) {}
        fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
        fn quote(&self, _: &str, _: &str, _: vike_marketdata::TickQuote) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn trade(&self, _: &str, _: &str, _: vike_marketdata::TickTrade) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn book(&self, _: &str, _: &str, _: Arc<vike_marketdata::L2Book>) {}
    }
    let Some(c) = config() else {
        eprintln!("skip: no creds");
        return;
    };
    let count = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(vike_alpaca::feed::DataClientBusSpecAlpaca::new(c, || {}));
    let mut client = vike_bridge_core::data_client::bus::DataClientBus::on(
        &source,
        Arc::new(CountSink(count.clone())),
    );
    // Subscribe TWO verbs on the same (crypto) symbol. Pre-multiplex, each verb opened its own WS
    // connection and the 2nd hit Alpaca's `406 connection limit exceeded`; the single-connection
    // multiplex must carry both over one connection. A 3rd, equity-class subscription opens a
    // SECOND connection (different endpoint) concurrently — proving the per-class model.
    let _q = client.subscribe_quotes("BTC/USD").expect("subscribe quotes");
    let _t = client.subscribe_trades("BTC/USD").expect("subscribe trades");
    let _e = client.subscribe_quotes("AAPL").expect("subscribe equity quotes");
    // Sandbox crypto data is sparse (~0.5 msg/s), so a short window can legitimately see zero;
    // wait long enough to reliably catch some. Crypto trades 24/7, so this is independent of US
    // equity market hours.
    std::thread::sleep(std::time::Duration::from_secs(20));
    client.shutdown();
    println!("crypto quote+trade msgs received: {}", count.load(Ordering::SeqCst));
    assert!(count.load(Ordering::SeqCst) > 0, "no crypto quote/trade msgs received in 20s");
}

/// End-to-end live exercise of the exec client + SSE trade-events reader (which now opens with the
/// `since_id` reconnect watermark). Places a resting limit equity order (queues, won't fill) and
/// asserts the order lifecycle flows back through the ingest lane. Needs a FUNDED account.
#[test]
#[ignore = "live sandbox; funded account; places a resting equity order"]
fn alpaca_events_stream_smoke() {
    use tokio::sync::mpsc;
    use vike_execution::{EventSender, ExecutionClient, Ingest};
    use vike_model::events::Event;
    use vike_model::{OrderRequest, TimeInForce};

    let Some(c) = config() else {
        eprintln!("skip: no creds");
        return;
    };
    let rt =
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async move {
        let (tx, mut rx) = mpsc::channel::<Ingest>(256);
        let mut client =
            vike_alpaca::execution::spawn(c, EventSender { ingest: tx, route_key: None });
        let order_client_id = format!("evt-{}", std::process::id());
        let order = OrderRequest {
            order_client_id: order_client_id.clone(),
            venue: "alpaca".into(),
            symbol: "NVDA".into(),
            side: 1,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(1.00), // far below market → rests, does not fill
            trigger_price: None,
            time_in_force: TimeInForce::Gtc,
            timestamp: 1,
            ..Default::default()
        };
        client.submit(&order);
        let mut saw_submitted = false;
        let mut saw_accepted = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while tokio::time::Instant::now() < deadline && !(saw_submitted && saw_accepted) {
            if let Ok(Some(Ingest::Event(event))) =
                tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
            {
                println!("event: {event:?}");
                match &event {
                    Event::OrderSubmitted(s) if s.order_client_id == order_client_id => {
                        saw_submitted = true
                    }
                    Event::OrderAccepted(a) if a.order_client_id == order_client_id => {
                        saw_accepted = true
                    }
                    _ => {}
                }
            }
        }
        client.cancel(&order_client_id);
        client.detach();
        assert!(saw_submitted, "OrderSubmitted not observed");
        assert!(saw_accepted, "OrderAccepted not observed (exec POST + SSE path)");
    });
}
