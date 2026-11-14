//! **A bracket entry whose POST got no answer, whose re-query failed, and which then filled: its stop goes out.**
//!
//! The whole road, end to end: a real core mints the bracket and hands its entry to the venue's client, the real bybit
//! client's POST and re-query both fail (so it says nothing, `resolve_ambiguous_submit`), the venue's `execution` row
//! comes through the real stream decoder and the real fence, and the core folds it. Without the accept the fill implies,
//! the wrap is refused on a `Submitted` order and not kept, the bracket's drive never runs, and an open position has no
//! stop and no `POSITION WITHOUT STOP` line either. Nothing here opens a socket.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use vike_bridge_core::exec_actor::submit_fence::{SubmitFence, VenueRestOrderSubmitFence};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_bybit::execution::user_data::perp_frame_decoder;
use vike_core::{CoreConfig, spawn_core};
use vike_execution::{
    Account, BalanceMode, Command, Engine, ExecutionClient, OrderIntent, RiskGate,
};
use vike_model::events::Event;
use vike_model::{OrderBracket, OrderRequest, OrderStatus, OrderType, RiskLimits};

use crate::r6_bybit_parity::{bybit_perp, fixture};

/// A venue that answers nothing: the POST times out and so does the read behind it.
struct NoAnswer;

impl RestTransportJson for NoAnswer {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        Err(VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: "no answer".into() })
    }
}

/// The exec thread's queue, as the core sees it: what it was handed, and no event back.
struct Queue(Arc<Mutex<Vec<OrderRequest>>>);

impl ExecutionClient for Queue {
    fn submit(&mut self, request: &OrderRequest) {
        self.0.lock().expect("queue").push(request.clone());
    }
    fn cancel(&mut self, _order_client_id: &str) {}
}

fn execution_row(order_client_id: &str) -> Value {
    json!({"topic": "execution", "data": [{
        "execType": "Trade", "symbol": "BTCUSDT", "orderLinkId": order_client_id, "execId": "E1",
        "side": "Buy", "execPrice": "100", "execQty": "0.01", "execFee": "0.06",
        "feeCurrency": "USDT", "isMaker": false, "cumExecQty": "0.01", "orderQty": "0.01",
        "leavesQty": "0"}]})
}

#[test]
fn a_bracket_entry_with_no_answer_releases_its_stop_when_it_fills() {
    let queue: Arc<Mutex<Vec<OrderRequest>>> = Arc::default();
    let engine = Engine::new(
        Account::new(10_000.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        Queue(Arc::clone(&queue)),
        "bybit",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, CoreConfig { seed_cash: 10_000.0, ..CoreConfig::default() });
    handle.send_command(Command::Order(OrderIntent::Bracket(Box::new(OrderBracket {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 0.01,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    }))));
    assert!(vike_log::poll::wait_until(Duration::from_secs(5), || {
        !queue.lock().expect("queue").is_empty()
    }));
    let entry = queue.lock().expect("queue")[0].clone();
    assert_eq!(entry.order_type, OrderType::Limit, "the exits are held behind the entry");

    let client = bybit_perp(&fixture(), NoAnswer);
    let fence = SubmitFence::new(handle.event_sender());
    let returned = VenueRestOrderSubmitFence::new(&client, &fence).submit_order(&entry);
    assert!(returned.is_empty(), "the fence sent what there was: {returned:?}");

    let decode = perp_frame_decoder("BTCUSDT");
    let fill = decode(&execution_row(&entry.order_client_id));
    assert!(fill.iter().any(|e| matches!(e, Event::OrderFilled(_))), "{fill:?}");
    for event in fill {
        assert!(fence.deliver(event));
    }

    let stop_out = vike_log::poll::wait_until(Duration::from_secs(5), || {
        queue.lock().expect("queue").iter().any(|r| r.order_type == OrderType::Stop)
    });
    let sent = queue.lock().expect("queue").clone();
    assert!(stop_out, "an open position with no stop: the core sent only {sent:?}");
    let stop = sent.iter().find(|r| r.order_type == OrderType::Stop).expect("the stop");
    assert_eq!((stop.side, stop.reduce_only, stop.trigger_price), (-1, true, Some(95.0)));

    let cell = handle.snapshot_cell();
    handle.shutdown_and_join();
    let snapshot = cell.load_full();
    let order = snapshot.orders.iter().find(|o| o.order_client_id == entry.order_client_id);
    assert_eq!(order.map(|o| o.status), Some(OrderStatus::Filled), "{:?}", snapshot.orders);
}
