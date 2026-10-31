//! **The Dukascopy reconcile leg, over real child processes of the scripted fake** (`src/bin/fake_jforex_bridge.rs`): every
//! shape of the sidecar's `recon` answer and every way the question fails, through the real stdio pipe and the real
//! reader thread. The Java half (what `engine.getOrders()` becomes on the wire) is `ReconSnapshot.java` and
//! `ReconQueryTest.java`; the adapter's hand-out is `tests/dukascopy_adapter.rs`; the netting lifecycle is
//! `tests/dukascopy_reconcile.rs`.
//!
//! What these prove: a changed venue is read at the NEXT question (a position the venue closed unheard reads flat, where
//! the push-line snapshot this leg replaced kept the node's own belief); the node's resting protections are reported while
//! the venue still holds their price and dropped when it does not; and an answer that could not be read, or never came, or
//! came from a jar that cannot give one, is an ERROR on every read and never an empty report.
//!
//! Each test uses the fake in a mode of its own; the answer body of `recon <file>` is rewritten between questions.

use std::time::{Duration, Instant};

use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient, SidecarState};
use vike_exec::recon::ReconClient;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_log::capture::captured;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, OrderType};
use vike_venues::venue_timings::Reconnect;

const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

/// A restart policy in milliseconds: 40 ms, doubling to 160 ms.
const FAST: Reconnect =
    Reconnect::new(Duration::from_millis(40), Duration::from_millis(160), "a test");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
        // The engine's mount: a symbol the client states FLAT whenever the venue lists nothing on it.
        ledger_mount: Some("EURUSD".into()),
    }
}

fn scratch(tag: &str) -> vike_paths::scratch::ScratchDir {
    vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), tag).expect("scratch dir")
}

/// A client over the fake's `recon <body file>` mode, with a short question bound; the body file starts as `body`.
fn client_answering(
    body: &str,
) -> (
    DukascopyExecutionClient,
    std::path::PathBuf,
    tokio::sync::mpsc::Receiver<Ingest>,
    vike_paths::scratch::ScratchDir,
) {
    let dir = scratch("vike-duka-recon-leg");
    let file = dir.path().join("recon-body.json");
    std::fs::write(&file, body).expect("the body file");
    let (events, rx) = event_channel(64);
    let client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["recon".into(), file.display().to_string()],
        &config(),
        events,
    )
    .expect("ready");
    client.set_recon_timeout(Duration::from_millis(400));
    (client, file, rx, dir)
}

fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    match rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .expect("timed out waiting for event")
        .expect("ingest channel closed")
    {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The venue as `ReconSnapshot.java` writes it: a resting limit of ours, one opened in the platform, a long USDJPY position
/// of ours carrying a stop-loss and a short one from the platform, and the net over the two.
const BUSY_VENUE: &str = r#"{"ts":5000,"foreign":2,"orders":[
  {"order_id":"11","coid":"c1","symbol":"EURUSD","side":1,"state":"OPENED","command":"BUYLIMIT","amount":0.0,"requested":2000.0,"price":1.05,"stop_loss":0.0,"take_profit":0.0,"commission":0.0,"created":1000,"filled":0},
  {"order_id":"12","symbol":"EURUSD","side":-1,"state":"OPENED","command":"SELLLIMIT","amount":0.0,"requested":1000.0,"price":1.2,"stop_loss":0.0,"take_profit":0.0,"commission":0.0,"created":1100,"filled":0},
  {"order_id":"22","coid":"m1","symbol":"USDJPY","side":1,"state":"FILLED","command":"BUY","amount":1000.0,"requested":1000.0,"price":156.9,"stop_loss":156.5,"take_profit":0.0,"commission":-0.03,"created":1200,"filled":1300},
  {"order_id":"23","symbol":"USDJPY","side":-1,"state":"FILLED","command":"SELL","amount":400.0,"requested":400.0,"price":157.0,"stop_loss":0.0,"take_profit":0.0,"commission":-0.01,"created":1400,"filled":1500}],
 "positions":[{"symbol":"USDJPY","size":600.0,"avg_px":156.8333}]}"#;

/// **Every part of one answer lands in the right report**, over the real pipe: the resting orders (ours with a coid, the
/// platform's without), the FILLED orders as fills under `<id>:1` (and as no order at all), and the positions: the venue's
/// row and a FLAT row for the mount the venue lists nothing on.
#[test]
fn one_answer_becomes_the_orders_the_fills_and_the_positions() {
    let (client, _file, _rx, _dir) = client_answering(BUSY_VENUE);
    let recon = client.recon_client();

    let orders = recon.fetch_order_status_reports(0).expect("orders");
    assert_eq!(
        orders.len(),
        2,
        "the two RESTING orders; the FILLED ones are positions: {orders:?}"
    );
    let ours = orders.iter().find(|o| o.client_order_id.as_deref() == Some("c1")).expect("c1");
    assert_eq!((ours.symbol.as_str(), ours.side, ours.qty), ("EURUSD", 1, 2000.0));
    assert_eq!(ours.order_type, OrderType::Limit);
    assert_eq!(ours.status, OrderStatus::Accepted);
    assert!(
        orders.iter().any(|o| o.client_order_id.is_none()),
        "the platform's order names no coid"
    );

    let fills = recon.fetch_fill_reports(0).expect("fills");
    let ids: Vec<&str> = fills.iter().map(|f| f.trade_id.as_str()).collect();
    assert_eq!(ids, ["22:1", "23:1"], "OrderMap.entryTradeId(orderId, 1)");
    assert_eq!(fills[0].client_order_id.as_deref(), Some("m1"));
    assert_eq!(
        fills[0].commission, 0.03,
        "the venue's negative charge is the engine's positive one"
    );
    assert_eq!(
        recon.fetch_fill_reports(1_400).expect("fills").len(),
        1,
        "since: only the later fill"
    );

    let positions = recon.fetch_position_status_reports().expect("positions");
    let rows: Vec<(&str, f64)> = positions.iter().map(|p| (p.symbol.as_str(), p.qty)).collect();
    assert_eq!(
        rows,
        [("EURUSD", 0.0), ("USDJPY", 600.0)],
        "the mount is stated flat beside the venue's row"
    );
    assert_eq!(positions[1].avg_px, 156.8333);

    let mass = recon.fetch_mass_status(0).expect("the composed snapshot");
    assert_eq!((mass.orders.len(), mass.fills.len(), mass.positions.len()), (2, 2, 2));
}

/// **The venue is READ at each question, so a position it closed unheard reads flat at the next one.** The node folded a
/// long USDJPY (a fill named the symbol), the venue's stop-loss then fired with no sidecar listening: the push-line
/// snapshot this leg replaced kept the node's own belief and read "in sync"; the pull says 0, which is the row
/// `vike_exec::recon::diff` raises a `PositionDrift` on (held under `quarantine`).
#[test]
fn a_position_the_venue_closed_unheard_reads_flat_at_the_next_question() {
    let (client, file, _rx, _dir) = client_answering(BUSY_VENUE);
    let recon = client.recon_client();
    recon.admit_symbol("USDJPY"); // the engine admitted it: a flat row is owed when the venue lists nothing on it

    let size = |recon: &dyn ReconClient, symbol: &str| {
        recon
            .fetch_position_status_reports()
            .expect("positions")
            .into_iter()
            .find(|p| p.symbol == symbol)
            .map(|p| p.qty)
    };
    assert_eq!(size(&recon, "USDJPY"), Some(600.0));

    // The venue closes it: its next answer lists no order on USDJPY at all.
    std::fs::write(&file, r#"{"orders":[],"positions":[]}"#).expect("the venue changed");
    assert_eq!(size(&recon, "USDJPY"), Some(0.0), "read at the next question, stated flat");
    assert_eq!(size(&recon, "EURUSD"), Some(0.0));

    // The engine gives the symbol back: it is no longer owed a row (the mount always is).
    recon.forget_symbol("USDJPY");
    assert_eq!(size(&recon, "USDJPY"), None);
    assert_eq!(size(&recon, "EURUSD"), Some(0.0), "the mount is never forgotten");
}

/// An empty file is a venue that holds nothing: an ANSWER on all three reads (not an error), and the mount is flat.
#[test]
fn a_flat_venue_is_an_answer_and_not_an_error() {
    let (client, _file, _rx, _dir) = client_answering("");
    let recon = client.recon_client();
    assert_eq!(recon.fetch_order_status_reports(0), Ok(Vec::new()));
    assert_eq!(recon.fetch_fill_reports(0), Ok(Vec::new()));
    let positions = recon.fetch_position_status_reports().expect("positions");
    assert_eq!(positions.len(), 1);
    assert_eq!((positions[0].symbol.as_str(), positions[0].qty), ("EURUSD", 0.0));
}

fn resting_stop(coid: &str, trigger: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side: -1,
        qty: 1000.0,
        order_type: OrderType::Stop,
        trigger_price: Some(trigger),
        reduce_only: true,
        ts: 1,
        ..Default::default()
    }
}

fn resting_take_profit(coid: &str, price: f64) -> OrderRequest {
    OrderRequest {
        order_type: OrderType::Limit,
        trigger_price: None,
        price: Some(price),
        ..resting_stop(coid, 0.0)
    }
}

/// **The node's resting protections are reported while the venue still holds their price on an order they cover, and are
/// ABSENT the moment it does not.** A stop-loss and a take-profit are no orders at the venue (a price on a FILLED order,
/// no coid stored), so without this the node's resting stop would be an orphan local order every pass. The venue here is the
/// adopting fake: its census files stand for `engine.getOrders()` and are rewritten between questions.
#[test]
fn a_resting_stop_and_take_profit_are_reported_while_the_venue_holds_their_prices() {
    let dir = scratch("vike-duka-recon-protections");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_restart(
        BRIDGE,
        &["adopt-die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    client.set_recon_timeout(Duration::from_millis(400));
    let recon = client.recon_client();

    client.submit(&resting_stop("s1", 1.09));
    client.submit(&resting_take_profit("t1", 1.2));
    for _ in 0..4 {
        // Submitted and Accepted of each: the protections are acknowledged once the reader has handled the Accepted.
        let _ = recv_event(&mut rx);
    }

    let adopted = format!("{}.adopted", log.display());
    let coids = |recon: &dyn ReconClient| -> Vec<String> {
        let mut ids: Vec<String> = recon
            .fetch_order_status_reports(0)
            .expect("orders")
            .into_iter()
            .filter_map(|o| o.client_order_id)
            .collect();
        ids.sort();
        ids
    };

    // The venue holds a long order with BOTH prices: both protections are reported, as a stop and a limit.
    std::fs::write(&adopted, "m1 EURUSD 1 FILLED 1000 1.09 1.2\n").expect("venue order");
    assert_eq!(coids(&recon), ["s1", "t1"]);
    let reports = recon.fetch_order_status_reports(0).expect("orders");
    let stop = reports.iter().find(|o| o.client_order_id.as_deref() == Some("s1")).expect("s1");
    assert_eq!(
        (&stop.order_type, stop.side, stop.status),
        (&OrderType::Stop, -1, OrderStatus::Accepted)
    );
    let tp = reports.iter().find(|o| o.client_order_id.as_deref() == Some("t1")).expect("t1");
    assert_eq!(tp.order_type, OrderType::Limit);

    // The stop-loss fired (or was taken off) at the venue: only the take-profit is still held.
    std::fs::write(&adopted, "m1 EURUSD 1 FILLED 1000 0 1.2\n").expect("venue order");
    assert_eq!(coids(&recon), ["t1"], "a protection the venue no longer holds is absent");

    // The order itself is gone: nothing is held, and the answer is an empty report, not an error.
    std::fs::write(&adopted, "").expect("venue order");
    assert_eq!(coids(&recon), Vec::<String>::new());

    // A stop at ANOTHER price than the one the venue holds is not that stop.
    std::fs::write(&adopted, "m1 EURUSD 1 FILLED 1000 1.0 0\n").expect("venue order");
    assert_eq!(coids(&recon), Vec::<String>::new());
    client.detach();
}

/// **An answer that could not be read, or never came, is an ERROR on all three reads, in the sidecar's words, never an
/// empty report**: the sidecar's own `error`, a line with neither lists nor an error, silence, and an answer to another
/// question.
#[test]
fn an_answer_that_is_an_error_malformed_silent_or_for_another_question_is_an_error_on_every_read() {
    for (body, expect) in [
        (r#"{"error":"the venue's orders could not be read: no session"}"#, "no session"),
        ("{}", "neither its lists nor an error"),
        ("SILENT", "did not answer"),
        ("WRONG-ID", "did not answer"),
    ] {
        let (client, _file, _rx, _dir) = client_answering(body);
        let recon = client.recon_client();
        for (read, result) in [
            ("orders", recon.fetch_order_status_reports(0).map(|_| ())),
            ("fills", recon.fetch_fill_reports(0).map(|_| ())),
            ("positions", recon.fetch_position_status_reports().map(|_| ())),
        ] {
            let error = result.expect_err(&format!("{body}: {read} must be an error"));
            assert!(error.contains(expect), "{body}: {read}: {error}");
        }
        assert!(recon.fetch_mass_status(0).is_err(), "{body}: the composed snapshot fails too");
    }
}

/// **A jar that does not announce `recon` cannot be asked**: nothing is written to it, every read is an error in words that
/// name the jar, and the client says so (`recon_supported`, which the adapter reads before it hands a client out).
#[test]
fn a_jar_without_the_query_is_an_error_in_words_and_is_never_asked() {
    let (events, _rx) = event_channel(64);
    let client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["no-recon".into()],
        &config(),
        events,
    )
    .expect("ready");
    assert!(!client.recon_supported());
    let recon = client.recon_client();
    let error = recon.fetch_position_status_reports().expect_err("no answer is possible");
    assert!(error.contains("predates the reconcile query"), "{error}");
    assert!(recon.fetch_order_status_reports(0).is_err() && recon.fetch_fill_reports(0).is_err());

    // …and the jar that does announce it says so.
    let (client, _file, _rx, _dir) = client_answering("");
    assert!(client.recon_supported());
}

/// **A sidecar that ends with the question open fails it at once**, with the death in the words, not after the timeout.
#[test]
fn a_sidecar_that_ends_with_the_question_open_fails_it_at_once_in_words() {
    let (events, _rx) = event_channel(64);
    let client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["recon-die".into()],
        &config(),
        events,
    )
    .expect("ready");
    // The bound is long: only the EOF drain can end the question quickly.
    client.set_recon_timeout(Duration::from_secs(30));
    let began = Instant::now();
    let error = client.recon_client().fetch_order_status_reports(0).expect_err("the sidecar died");
    assert!(began.elapsed() < Duration::from_secs(10), "failed by the drain, not by the bound");
    assert!(error.contains("ended while the reconcile query was open"), "{error}");
    assert!(error.contains("NOT read"), "{error}");
}

/// **While the sidecar is down (the supervisor is starting another) every read is refused by name, with the reason.**
#[test]
fn a_down_sidecar_is_an_error_that_names_why() {
    let dir = scratch("vike-duka-recon-down");
    let log = dir.path().join("launches.txt");
    let (events, _rx) = event_channel(64);
    // The first launch says ready and dies at once; every later one hangs: the client stays DOWN while it restarts.
    let client = DukascopyExecutionClient::spawn_with_restart(
        BRIDGE,
        &["die-then-hang".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    wait_until("the sidecar to be down", || client.sidecar_state() != SidecarState::Up);
    let error = client.recon_client().fetch_position_status_reports().expect_err("down");
    assert!(error.contains("bridge unavailable") && error.contains("NOT read"), "{error}");
    drop(client);
}

/// **A failed question is SAID at `error`** (once a minute per client; every fetch of a pass fails the same way), so the
/// operator reads that the books were not compared, and the failure is returned every time.
#[test]
fn a_failed_question_is_said_at_error() {
    let (events, _rx) = event_channel(64);
    let client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["no-recon".into()],
        &config(),
        events,
    )
    .expect("ready");
    let recon = client.recon_client();
    let (results, lines) =
        captured(|| (recon.fetch_order_status_reports(0), recon.fetch_fill_reports(0)));
    assert!(results.0.is_err() && results.1.is_err(), "the failure is returned every time");
    let errors: Vec<_> = lines.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
    assert_eq!(errors.len(), 1, "said once, not once per fetch: {lines:?}");
    assert!(errors[0].message.contains("reconcile query FAILED"), "{:?}", errors[0]);
    assert!(errors[0].field("error").is_some_and(|e| e.contains("predates")), "{:?}", errors[0]);
}
