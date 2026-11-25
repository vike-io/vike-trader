//! **USER names above the wire** (decision 0290, SYM-NAMES step 2, offline). Over the crate's held kept list
//! (`vike_ctrader::test_support::kept_list_for_tests`, rows made by the catalog's own parser from a broker listing
//! `EURUSD`, `GBPUSD`, `USDJPY`, a suffixed `AUDCADm`, the index CFD `US500` and the coin CFD `BTCUSD`), every seam
//! speaks the user name and the wire speaks the kept row's broker name, then its `symbolId`:
//!
//! * an order on `AUDCAD` is sent on the id of the broker's `AUDCADm`, one on `US500.CFD` on the broker's `US500`;
//! * a fill on that id comes back as `AUDCAD` / `US500.CFD`;
//! * a spot subscription on `US500.CFD` subscribes the broker's `US500`, and its quotes are keyed `US500.CFD`;
//! * a name no kept row answers (the broker's own `US500`, an unlisted pair) is refused in words, nothing sent.
//!
//! The pure halves (a suffixed `EURUSDm`, no list at all, two rows) are `src/instrument/names_tests.rs`; the
//! reconcile's exact case is `src/reconcile/mod.rs`'s tests and `reconcile_admitted.rs`; the start with no kept list
//! is `src/adapter_contract_tests.rs`'s.

use std::sync::{Arc, Mutex, mpsc};

use vike_bridge_core::data_client::bus::{DataClientBus, DataClientBusSink};
use vike_ctrader::execution::ExecutionClientCtrader;
use vike_ctrader::execution::positions::{CloseTracker, PositionsById};
use vike_ctrader::feed::DataClientBusSpecCtrader;
use vike_ctrader::feed::decode::{execution_event_to_events, spot_to_quote};
use vike_ctrader::instrument::symbols::SymbolMap;
use vike_ctrader::test_support::{KEPT_FOR_TESTS, NoopSink, kept_list_for_tests, listed, rejected};
use vike_ctrader::transport::connection::{ConnectionCommand, ConnectionShared};
use vike_ctrader::transport::proto::{
    ProtoOaDeal, ProtoOaExecutionEvent, ProtoOaExecutionType, ProtoOaOrder, ProtoOaOrderType,
    ProtoOaSpotEvent, ProtoOaTradeData, ProtoOaTradeSide,
};
use vike_data::DataClient;
use vike_execution::{ExecutionClient, event_channel};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

/// The ids [`KEPT_FOR_TESTS`] lists the broker's names under.
const AUDCADM: i64 = 4;
const US500: i64 = 5;

/// The handshake's map of the broker [`KEPT_FOR_TESTS`] describes.
fn symbols() -> Arc<SymbolMap> {
    Arc::new(SymbolMap::from_lists(&listed(&KEPT_FOR_TESTS)))
}

/// The connection's shared state over a bare command channel, holding the kept list.
fn shared() -> (ConnectionShared, mpsc::Receiver<ConnectionCommand>) {
    kept_list_for_tests();
    let (tx, commands) = mpsc::channel();
    let shared = ConnectionShared {
        tx,
        symbols: symbols(),
        ctid: 99,
        orders: Arc::default(),
        positions: Arc::new(Mutex::new(PositionsById::default())),
        closes: Arc::new(Mutex::new(CloseTracker::default())),
    };
    (shared, commands)
}

/// A 1,000-unit market BUY of the USER name `symbol`.
fn buy(order_client_id: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "ctrader".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1000.0,
        order_type: OrderType::Market,
        ..Default::default()
    }
}

/// The `symbolId` of every `NewOrder` written.
fn new_order_ids(commands: &mpsc::Receiver<ConnectionCommand>) -> Vec<i64> {
    commands
        .try_iter()
        .filter_map(|c| match c {
            ConnectionCommand::NewOrder(order) => Some(order.symbol_id),
            _ => None,
        })
        .collect()
}

/// The user name a fill on `symbol_id` is booked under.
fn fill_symbol(symbol_id: i64) -> String {
    let event = ProtoOaExecutionEvent {
        ctid_trader_account_id: 99,
        execution_type: ProtoOaExecutionType::OrderFilled as i32,
        order: Some(ProtoOaOrder {
            order_id: 555,
            trade_data: ProtoOaTradeData {
                symbol_id,
                volume: 100_000,
                trade_side: ProtoOaTradeSide::Buy as i32,
                label: Some("u-1".into()),
                ..Default::default()
            },
            order_type: ProtoOaOrderType::Market as i32,
            order_status: 2,
            client_order_id: Some("u-1".into()),
            ..Default::default()
        }),
        deal: Some(ProtoOaDeal {
            deal_id: 777,
            order_id: 555,
            volume: 100_000,
            filled_volume: 100_000,
            symbol_id,
            execution_timestamp: 1234,
            execution_price: Some(0.91),
            trade_side: ProtoOaTradeSide::Buy as i32,
            ..Default::default()
        }),
        ..Default::default()
    };
    let events = execution_event_to_events(&event, &symbols(), 2);
    match &events[0] {
        Event::Fill(fill) => fill.symbol.as_str().to_string(),
        other => panic!("expected a fill, got {other:?}"),
    }
}

/// **A suffixed broker's pair and an index CFD round trip**: the order on the USER name is sent on the id of the kept
/// row's broker name, and the fill on that id comes back under the user name.
#[test]
fn an_order_on_a_user_name_is_sent_on_its_rows_broker_name_and_its_fill_comes_back_under_it() {
    for (user, id) in [("AUDCAD", AUDCADM), ("US500.CFD", US500), ("EURUSD", 1)] {
        let (shared, commands) = shared();
        let (events, mut rx) = event_channel(64);
        let halt =
            std::env::temp_dir().join(format!("vike-ctrader-user-names-{}", std::process::id()));
        let mut exec =
            ExecutionClientCtrader::from_shared(shared, events).with_halt_path(halt.join("HALT"));
        exec.submit(&buy("u-1", user));
        assert_eq!(rejected(&mut rx), Vec::<String>::new(), "{user}");
        assert_eq!(new_order_ids(&commands), [id], "{user} is sent on the broker's id {id}");
        assert_eq!(fill_symbol(id), user, "the fill on id {id} is booked as {user}");
    }
}

/// **No row is a refusal in words, nothing sent**: the broker's own `US500` and `AUDCADm` are no user names (decision
/// 0117: no second spelling is read), and a pair the kept list does not carry is none either.
#[test]
fn an_order_on_a_name_no_kept_row_answers_is_refused_in_words_and_nothing_is_sent() {
    for name in ["US500", "AUDCADm", "GBPJPY"] {
        let (shared, commands) = shared();
        let (events, mut rx) = event_channel(64);
        let halt =
            std::env::temp_dir().join(format!("vike-ctrader-user-names-{}", std::process::id()));
        let mut exec =
            ExecutionClientCtrader::from_shared(shared, events).with_halt_path(halt.join("HALT"));
        exec.submit(&buy("u-2", name));
        let reasons = rejected(&mut rx);
        assert!(
            reasons.len() == 1
                && reasons[0].contains(name)
                && reasons[0].contains("no user name of a ctrader instrument")
                && reasons[0].contains("nothing was sent"),
            "{name}: {reasons:?}"
        );
        assert_eq!(new_order_ids(&commands), Vec::<i64>::new(), "{name}: nothing sent");
    }
}

/// **A spot subscription on a USER name subscribes the row's broker id, and its quotes are keyed by the user name.**
#[test]
fn a_spot_subscription_on_a_user_name_subscribes_its_rows_id_and_its_quotes_carry_the_user_name() {
    let (shared, commands) = shared();
    let view = DataClientBusSpecCtrader::view(shared, Arc::new(DataClientBusSink::default()));
    let mut data = DataClientBus::on(&Arc::new(view), Arc::new(NoopSink));
    data.subscribe_quotes("US500.CFD").expect("the index CFD's user name");
    data.subscribe_quotes("EURUSD").expect("a pair");
    let spots: Vec<i64> = commands
        .try_iter()
        .filter_map(|c| match c {
            ConnectionCommand::SubscribeSpots { symbol_id } => Some(symbol_id),
            _ => None,
        })
        .collect();
    assert_eq!(spots, [US500, 1]);
    let why = data.subscribe_quotes("US500").expect_err("the broker's own name is no user name");
    assert!(why.to_string().contains("no user name"), "{why}");

    for (id, user) in [(US500, "US500.CFD"), (AUDCADM, "AUDCAD"), (1, "EURUSD")] {
        let spot = ProtoOaSpotEvent {
            ctid_trader_account_id: 99,
            symbol_id: id,
            bid: Some(764_050_000),
            ask: Some(764_060_000),
            ..Default::default()
        };
        let (symbol, bid, _) = spot_to_quote(&spot, &symbols()).expect("a two-sided spot");
        assert_eq!(symbol, user, "the spot on id {id}");
        assert_eq!(bid, 7640.5);
    }
}
