//! The Client Portal backend's refusals, every one decided before the network is touched: the backend
//! is built with `unconnected` (no tickle, no pump), against a closed loopback port, so a refusal that
//! reached the gateway by mistake fails instead of passing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_bridge_core::data_client::bus::DataClientBusSpecKey;
use vike_data::{DataClientError, DataClientSink};

use super::DataClientBusSpecBackendIbkrCpapi;
use crate::feed::price::DataClientBusSpecBackendIbkr;
use crate::feed::test_support::{RecSink, offline_config};
use crate::transport::error_codes::IbkrError;
use vike_venues::IbkrBackend;

fn feeds() -> DataClientBusSpecBackendIbkrCpapi {
    let sink: Arc<dyn DataClientSink> = RecSink::arc();
    DataClientBusSpecBackendIbkrCpapi::unconnected(&offline_config(IbkrBackend::Cpapi), sink)
}

fn quotes(symbol: &str) -> DataClientBusSpecKey {
    DataClientBusSpecKey::Quotes(symbol.to_string())
}

/// No book on cpapi (decision 0140): a book key answers `Unsupported` and starts no pump.
#[test]
fn a_book_is_unsupported_on_cpapi() {
    let mut f = feeds();
    let book = DataClientBusSpecKey::Book("AAPL.SMART.USD".into());
    assert!(matches!(f.want(&book), Err(DataClientError::Unsupported(_))));
    assert!(f.pump.is_none(), "a refused key opens nothing");
}

/// The cpapi backend only rides a cpapi session: a socket config is refused before any request.
#[test]
fn connect_refuses_a_config_of_another_backend() {
    let sink: Arc<dyn DataClientSink> = RecSink::arc();
    match DataClientBusSpecBackendIbkrCpapi::connect(&offline_config(IbkrBackend::Socket), sink) {
        Err(IbkrError::Unsupported(msg)) => assert!(msg.contains("Socket"), "names it: {msg}"),
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!("a socket config must not open a cpapi data backend"),
    }
}

/// An expiry-bearing contract is refused as ambiguous (the same refusal the exec and reconcile
/// lanes make), and an unreadable canonical is refused — both before `secdef/search`.
#[test]
fn an_expiry_bearing_or_unreadable_symbol_is_refused_up_front() {
    let mut f = feeds();
    match f.want(&quotes("ES.GLOBEX.USD.FUT.20251219")) {
        Err(DataClientError::Subscribe(msg)) => {
            assert!(msg.contains("another contract month"), "names the hazard: {msg}");
        }
        other => panic!("expected a Subscribe refusal, got {:?}", other.err()),
    }
    let trades = DataClientBusSpecKey::Trades("nonsense".into());
    assert!(matches!(f.want(&trades), Err(DataClientError::Subscribe(_))));
    assert!(f.pump.is_none(), "nothing was started");
}

/// **A `begin_shutdown` ends the pump and not the backend**: the session stays usable (a tradehub that restarts sends
/// it, and its successor subscribes on this backend), so the next stream starts a fresh pump instead of answering
/// "feed thread is gone" until the session ends. A contract pinned by id subscribes with no request, and the gateway's
/// closed loopback port only keeps the pump redialling, so nothing here reaches a network.
#[test]
fn a_begin_shutdown_does_not_end_the_backend() {
    const PINNED: &str = "AAPL.SMART.USD.STK.265598";
    let mut f = feeds();
    f.want(&quotes(PINNED)).expect("a pinned contract needs no request");
    f.begin_shutdown();
    let began = Instant::now();
    while !f.pump.as_ref().expect("started").join.is_finished() {
        assert!(began.elapsed() < Duration::from_secs(10), "the stop ends the pump thread");
        std::thread::sleep(Duration::from_millis(20));
    }
    let next = f.want(&quotes(PINNED)).expect("the next holder gets a fresh pump");
    assert!(!f.pump.as_ref().expect("started").join.is_finished(), "a live pump");
    f.unwant(next);
    f.shutdown();
    assert!(f.pump.is_none(), "the end of the backend still ends it");
}

/// A bar interval the shared parser cannot read is refused, never defaulted.
#[test]
fn an_unreadable_bar_interval_is_refused() {
    let mut f = feeds();
    for bad in ["", "0m", "abc"] {
        let bars =
            DataClientBusSpecKey::Bars { symbol: "AAPL.SMART.USD".into(), interval: bad.into() };
        assert!(matches!(f.want(&bars), Err(DataClientError::Subscribe(_))), "{bad:?}");
    }
}

/// The pump thread pins itself to the `md` role FIRST, as the WebSocket venues' feeds do (owner
/// ruling G2.5 row 15). A no-op unless `config.pin_cores` names `md`; before the ruling the thread
/// never asked, so it floated on any core, the trading core included.
#[test]
fn the_pump_thread_pins_to_the_market_data_role() {
    let src = include_str!("mod.rs");
    let spawn = src.find(".name(\"ibkr-cpapi-feed\"").expect("the pump's spawn");
    let body = &src[spawn..];
    let body = &body[..body.find("lane::run(").expect("the pump's body")];
    assert!(body.contains("pin_current_thread(") && body.contains("Role::MarketData"), "{body}");
}
