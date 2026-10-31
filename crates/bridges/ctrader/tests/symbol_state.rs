//! A symbol the broker CHANGES while the session runs (`ProtoOASymbolChangedEvent`) moves the trading state the exec
//! client judges an opening order by, end to end against the scripted venue.
//!
//! Before this suite the state was the one the broker reported when the session first connected, and an event naming a
//! changed symbol was dropped as an unhandled frame, so a symbol the broker restricted (or freed) later stayed as it
//! was until the session was rebuilt (`docs/ops/known-limitations-2026-10.md`, the cTrader symbol-state row). The
//! scripted venue pushes the event the way the proto documents it; whether and when a real broker does is NOT
//! measured (a demo account cannot be made to change a symbol).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::conn::{ConnConfig, connect_and_auth_exec};
use vike_ctrader::proto::ProtoOaTradingMode;
use vike_ctrader::trading_state::Restriction;
use vike_exec::{ExecutionClient, event_channel};
use vike_model::events::Event;

use common::{FakeCtrader, NoopSink, collect, market_buy, wait_open_filled};

/// Poll `cond` for up to five seconds (the actor reads a frame, then re-reads the symbols, on its own thread).
fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_symbol_changed_event_moves_the_state_an_opening_order_is_judged_by() {
    let (server, control) = FakeCtrader::start_symbol_state();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let symbols = handle.symbols.clone(); // the map the exec client holds: never swapped
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());
    assert_eq!(symbols.restriction(1), None, "an ordinary symbol at connect");

    // The broker switches EURUSD to close-only: the event is read, the state re-asked, the refusal follows.
    control.change(ProtoOaTradingMode::CloseOnlyMode as i32, true);
    wait_for("EURUSD close-only", || symbols.restriction(1) == Some(Restriction::CloseOnly));
    exec.submit(&market_buy("c-closed"));
    match collect(&mut ingest, 2).as_slice() {
        [Event::OrderSubmitted(_), Event::OrderRejected(r)] => {
            assert_eq!(r.client_order_id, "c-closed");
            assert!(r.reason.contains("CLOSE_ONLY_MODE"), "{}", r.reason);
        }
        other => {
            panic!("an opening order in a close-only symbol must be refused by name: {other:?}")
        }
    }
    assert!(!server.saw_all(&["NEW_ORDER_REQ"]), "a refused order must not reach the wire");

    // Hidden from traders (`enabled=false`, light list) is read from the list, not the full record.
    control.change(ProtoOaTradingMode::Enabled as i32, false);
    wait_for("EURUSD hidden", || symbols.restriction(1) == Some(Restriction::NotVisible));

    // The broker frees it: the next opening order goes out and fills.
    control.change(ProtoOaTradingMode::Enabled as i32, true);
    wait_for("EURUSD tradable again", || symbols.restriction(1).is_none());
    exec.submit(&market_buy("c-open"));
    wait_open_filled(&mut ingest, "c-open");
    server.assert_saw(&["SYMBOLS_LIST_REQ", "SYMBOL_BY_ID_REQ", "NEW_ORDER_REQ"]);
}

/// A reconnect writes the state its own handshake read into the map the exec client holds, so a change whose event
/// never came (or was never sent) is followed all the same at the next reconnect.
#[test]
fn a_reconnect_reads_the_state_afresh_even_when_no_event_came() {
    let (server, control) = FakeCtrader::start_symbol_state();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let symbols = handle.symbols.clone();
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    // The broker restricts EURUSD and the session dies before any event: only the reconnect's handshake can tell.
    control.set_quietly(ProtoOaTradingMode::CloseOnlyMode as i32, true);
    control.drop_connection();
    wait_for("EURUSD close-only, learned by the reconnect's handshake", || {
        symbols.restriction(1) == Some(Restriction::CloseOnly)
    });

    // And frees it again the same way.
    control.set_quietly(ProtoOaTradingMode::Enabled as i32, true);
    control.drop_connection();
    wait_for("EURUSD freed by the next reconnect's handshake", || symbols.restriction(1).is_none());
    server.wait_until_count_at_least("ACCOUNT_AUTH_REQ", 3, Duration::from_secs(10));

    exec.submit(&market_buy("c-after"));
    wait_open_filled(&mut ingest, "c-after");
}
