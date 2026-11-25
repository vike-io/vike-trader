//! **The wire takes a symbol only by its user name** (the C-D4 follow-up and decision 0290, `vike_catalog::respelled_symbol`
//! over the lease's venue's kept list): an adapter start whose mount or leg is a kept row's user name written another way
//! is not started, and a new order on such a symbol is refused terminally before the adapter. A mount on bybit's
//! `BTCUSDT.P` (the perpetual) and on `BTCUSDT` (the spot pair) starts as written, and a venue whose list its running
//! account's session reads (ig) starts with no list kept: the session is what lists it.

use std::sync::Arc;
use std::time::Duration;

use vike_gatewayhub_client::proto::{AdapterReply, AdapterStart, TradeDown, TradeUp};
use vike_model::events::Event;
use vike_model::{
    AccountTier, Contract, ContractPerp, HaltAdmit, Instrument, Margining, OrderRequest,
};

use super::{AccountSessions, TradeLeaseOpenRequest};
use crate::budget::ledger::Budget;
use crate::testkit::trade::TradeLaneScript;
use crate::testkit::trade_rig::hub_config;
use crate::trade::lease::{TradeLeaseHoldFlow, TradeLeaseHoldNext};

const WAIT: Duration = Duration::from_secs(5);

/// bybit's kept list as the datahub keeps it: the spot pair under its bare name, the perpetuals with `.P`.
fn hold_bybit_list() {
    let perp = || {
        Contract::Perp(ContractPerp {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        })
    };
    vike_catalog::hold_kept_list(
        "bybit",
        vec![
            Instrument::new("bybit", "BTCUSDT", "BTC", "USDT", Contract::CryptoSpot),
            Instrument::new("bybit", "BTCUSDT.P", "BTC", "USDT", perp()),
            Instrument::new("bybit", "ETHUSDT.P", "ETH", "USDT", perp()),
        ],
    );
}

fn open() -> TradeLeaseOpenRequest {
    open_on("bybit")
}

fn open_on(venue: &str) -> TradeLeaseOpenRequest {
    TradeLeaseOpenRequest {
        client_id: "tradehub-1".to_string(),
        venue: venue.to_string(),
        account: None,
        tier: AccountTier::Demo,
        resume: None,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    }
}

fn start(symbol: &str, legs: &[&str]) -> AdapterStart {
    start_on("bybit", symbol, legs)
}

fn start_on(venue: &str, symbol: &str, legs: &[&str]) -> AdapterStart {
    AdapterStart {
        rid: 7,
        symbol: symbol.to_string(),
        declared_legs: legs.iter().map(|s| (*s).to_string()).collect(),
        reconcile_enabled: false,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        read_account_currency: false,
        route_key: venue.to_string(),
    }
}

#[test]
fn a_start_whose_mount_or_leg_is_a_user_name_written_another_way_is_not_started() {
    hold_bybit_list();
    for (symbol, legs, written) in
        [("btcusdt.p", &[][..], "BTCUSDT.P"), ("BTCUSDT.P", &["ethusdt.p"][..], "ETHUSDT.P")]
    {
        let lane = TradeLaneScript::new(&["bybit"]);
        let hub =
            AccountSessions::new(Arc::new(lane.clone()), hub_config(), Arc::new(Budget::new()));
        match hub.session_for(&open()).start_adapter(start(symbol, legs)) {
            AdapterReply::NotRunning { refusal_text, .. } => assert!(
                refusal_text.contains(&format!("is written `{written}` on bybit")),
                "{refusal_text}"
            ),
            other => panic!("{symbol} {legs:?}: started: {other:?}"),
        }
        assert!(lane.adapter("bybit", None).is_none(), "{symbol} {legs:?}: an adapter was built");
    }
}

/// **A mount on bybit's `BTCUSDT.P` is the perpetual and the bare `BTCUSDT` the spot pair, each started as written**
/// (decision 0290), and an ig account starts with NO ig list kept: its session is what lists it (the list refresh rides
/// the account's session, `vike_ig::operator_row`), so no door refuses it for want of one. (ig because this binary's
/// tests hold the oanda, ctrader, dukascopy and fxcm lists, and a kept list is process-wide: under a plain `cargo test`
/// fxcm's, held by `symbol_facts_pin_tests` and the price desk's tests, failed this test's own precondition.)
#[test]
fn a_mount_named_by_its_user_name_starts_and_a_session_that_lists_its_venue_starts_with_no_list() {
    hold_bybit_list();
    for symbol in ["BTCUSDT.P", "BTCUSDT"] {
        let lane = TradeLaneScript::new(&["bybit"]);
        let hub =
            AccountSessions::new(Arc::new(lane.clone()), hub_config(), Arc::new(Budget::new()));
        let reply = hub.session_for(&open()).start_adapter(start(symbol, &[]));
        assert!(!matches!(reply, AdapterReply::NotRunning { .. }), "{symbol}: {reply:?}");
        assert!(lane.adapter("bybit", None).is_some(), "{symbol}: no adapter was built");
    }
    assert!(vike_catalog::kept_list("ig").is_none(), "no ig list is kept here");
    let lane = TradeLaneScript::new(&["ig"]);
    let hub = AccountSessions::new(Arc::new(lane.clone()), hub_config(), Arc::new(Budget::new()));
    let reply = hub.session_for(&open_on("ig")).start_adapter(start_on("ig", "EURUSD", &[]));
    assert!(!matches!(reply, AdapterReply::NotRunning { .. }), "ig: {reply:?}");
    assert!(lane.adapter("ig", None).is_some(), "the ig session started");
}

#[test]
fn a_new_order_on_a_symbol_the_door_spells_another_way_is_refused_before_the_adapter() {
    let lane = TradeLaneScript::new(&["bybit"]);
    let hub = AccountSessions::new(Arc::new(lane.clone()), hub_config(), Arc::new(Budget::new()));
    let lease = hub.open(open()).unwrap_or_else(|e| panic!("the open is accepted: {e:?}")).lease;
    hold_bybit_list();
    assert_eq!(
        lease.handle(TradeUp::AdapterStart(start("ETHUSDT.P", &[]))),
        TradeLeaseHoldFlow::Continue
    );
    let order = OrderRequest {
        order_client_id: "c-1".to_string(),
        venue: "bybit".to_string(),
        symbol: "ethusdt.p".to_string(),
        side: 1,
        qty: 1.0,
        price: Some(100.0),
        timestamp: 1_700_000_000_000,
        ..Default::default()
    };
    assert_eq!(
        lease.handle(TradeUp::Submit { command_sequence: 1, order }),
        TradeLeaseHoldFlow::Continue
    );
    let reason = loop {
        match lease.next(WAIT) {
            TradeLeaseHoldNext::Frame(TradeDown::Event {
                event: Event::OrderRejected(r), ..
            }) => break r.reason,
            TradeLeaseHoldNext::Frame(_) => {}
            other => panic!("expected the order's rejection, got {other:?}"),
        }
    };
    assert!(reason.contains("is written `ETHUSDT.P` on bybit"), "{reason}");
    let adapter = lane.adapter("bybit", None).expect("the adapter was started");
    assert!(adapter.submitted().is_empty(), "{:?}", adapter.submitted());
}
