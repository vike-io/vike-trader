//! `CpapiFeeds`' refusals, every one decided before the network is touched: the client is built
//! with `unconnected` (no tickle, no pump), against a closed loopback port, so a refusal that
//! reached the gateway by mistake fails instead of passing.

use std::sync::Arc;

use vike_data::{DataClient, LiveDataError, LiveDataSink};

use super::CpapiFeeds;
use crate::data_door::test_support::{RecSink, offline_cfg};
use crate::error::IbkrError;
use vike_model::IbkrBackend;

fn feeds() -> CpapiFeeds {
    let sink: Arc<dyn LiveDataSink> = RecSink::arc();
    CpapiFeeds::unconnected(&offline_cfg(IbkrBackend::Cpapi), sink)
}

/// No book, no depth on cpapi (decision 0140): both verbs answer `Unsupported` and start no pump.
#[test]
fn book_and_depth_are_unsupported_on_cpapi() {
    let mut f = feeds();
    assert!(matches!(f.subscribe_book("AAPL.SMART.USD"), Err(LiveDataError::Unsupported(_))));
    assert!(matches!(f.subscribe_depth("AAPL.SMART.USD"), Err(LiveDataError::Unsupported(_))));
    assert!(f.pump.is_none(), "a refused verb opens nothing");
}

/// The cpapi client only rides a cpapi session: a socket config is refused before any request.
#[test]
fn connect_refuses_a_config_of_another_backend() {
    let sink: Arc<dyn LiveDataSink> = RecSink::arc();
    match CpapiFeeds::connect(&offline_cfg(IbkrBackend::Socket), sink) {
        Err(IbkrError::Unsupported(msg)) => assert!(msg.contains("Socket"), "names it: {msg}"),
        Err(other) => panic!("expected Unsupported, got {other:?}"),
        Ok(_) => panic!("a socket config must not open a cpapi data client"),
    }
}

/// An expiry-bearing contract is refused as ambiguous (the same refusal the exec and reconcile
/// lanes make), and an unreadable canonical is refused — both before `secdef/search`.
#[test]
fn an_expiry_bearing_or_unreadable_symbol_is_refused_up_front() {
    let mut f = feeds();
    match f.subscribe_quotes("ES.GLOBEX.USD.FUT.20251219") {
        Err(LiveDataError::Subscribe(msg)) => {
            assert!(msg.contains("another contract month"), "names the hazard: {msg}");
        }
        other => panic!("expected a Subscribe refusal, got {:?}", other.err()),
    }
    assert!(matches!(f.subscribe_trades("nonsense"), Err(LiveDataError::Subscribe(_))));
    assert!(f.pump.is_none(), "nothing was started");
}

/// A bar interval the shared parser cannot read is refused, never defaulted.
#[test]
fn an_unreadable_bar_interval_is_refused() {
    let mut f = feeds();
    for bad in ["", "0m", "abc"] {
        assert!(
            matches!(f.subscribe_bars("AAPL.SMART.USD", bad), Err(LiveDataError::Subscribe(_))),
            "{bad:?}"
        );
    }
}
