//! **The wire takes a symbol only in the control door's spelling** (the C-D4 follow-up, `vike_catalog::respelled_symbol`
//! on the lease's venue): an adapter start whose mount or leg the door writes another way is not started, and a new
//! order on such a symbol is refused terminally before the adapter. Both used to pass on a length bound alone, so a
//! bybit `ethusdt.s` was signed as a perpetual of that name (bybit's `split_spot` reads `.S` only).

use std::sync::Arc;
use std::time::Duration;

use vike_gatewayhub_client::proto::{AdapterReply, AdapterStart, TradeDown, TradeUp};
use vike_model::events::Event;
use vike_model::{AccountTier, HaltAdmit, OrderRequest};

use super::{AccountSessions, TradeLeaseOpenRequest};
use crate::budget::ledger::Budget;
use crate::testkit::trade::TradeLaneScript;
use crate::testkit::trade_rig::hub_config;
use crate::trade::lease::{TradeLeaseHoldFlow, TradeLeaseHoldNext};

const WAIT: Duration = Duration::from_secs(5);

fn open() -> TradeLeaseOpenRequest {
    TradeLeaseOpenRequest {
        client_id: "tradehub-1".to_string(),
        venue: "bybit".to_string(),
        account: None,
        tier: AccountTier::Demo,
        resume: None,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    }
}

fn start(symbol: &str, legs: &[&str]) -> AdapterStart {
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
        route_key: "bybit".to_string(),
    }
}

#[test]
fn a_start_whose_mount_or_leg_the_door_spells_another_way_is_not_started() {
    for (symbol, legs) in [("ethusdt.s", &[][..]), ("BTCUSDT", &["ethusdt.s"][..])] {
        let lane = TradeLaneScript::new(&["bybit"]);
        let hub =
            AccountSessions::new(Arc::new(lane.clone()), hub_config(), Arc::new(Budget::new()));
        match hub.session_for(&open()).start_adapter(start(symbol, legs)) {
            AdapterReply::NotRunning { refusal_text, .. } => {
                assert!(refusal_text.contains("is written `ETHUSDT.S` on bybit"), "{refusal_text}")
            }
            other => panic!("{symbol} {legs:?}: started: {other:?}"),
        }
        assert!(lane.adapter("bybit", None).is_none(), "{symbol} {legs:?}: an adapter was built");
    }
}

#[test]
fn a_new_order_on_a_symbol_the_door_spells_another_way_is_refused_before_the_adapter() {
    let lane = TradeLaneScript::new(&["bybit"]);
    let hub = AccountSessions::new(Arc::new(lane.clone()), hub_config(), Arc::new(Budget::new()));
    let lease = hub.open(open()).unwrap_or_else(|e| panic!("the open is accepted: {e:?}")).lease;
    assert_eq!(
        lease.handle(TradeUp::AdapterStart(start("ETHUSDT.S", &[]))),
        TradeLeaseHoldFlow::Continue
    );
    let order = OrderRequest {
        order_client_id: "c-1".to_string(),
        venue: "bybit".to_string(),
        symbol: "ethusdt.s".to_string(),
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
    assert!(reason.contains("is written `ETHUSDT.S` on bybit"), "{reason}");
    let adapter = lane.adapter("bybit", None).expect("the adapter was started");
    assert!(adapter.submitted().is_empty(), "{:?}", adapter.submitted());
}
