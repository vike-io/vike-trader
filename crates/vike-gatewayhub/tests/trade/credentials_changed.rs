//! **A changed credential reconnects ONE account, and no other** (decision 0287).
//!
//! `Request::CredentialsChanged`, sent the way the datahub relays an operator's `vike-cli secrets set` (a
//! control-scope connection carrying the account's ROW ID and nothing else), tears that account's adapter down
//! without cancelling anything at the venue, ends its lease once and begins a new epoch, so its client drops the
//! link, comes back and starts the adapter again on what the store holds now. Every other account keeps its lease,
//! its adapter and its epoch. Over the real server and the client crate's real `TradeLease`.
#![cfg(feature = "trade")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_data::{DataClient, DataClientSink, RecordingSink};
use vike_execution::{ExecutionClient, Ingest, RestoredOrder};
use vike_gatewayhub::testkit::data::{DataCall, DataLaneScript};
use vike_gatewayhub::testkit::gateway::{LoopbackGateway, TradeKit};
use vike_gatewayhub::testkit::rest::RestLaneScript;
use vike_gatewayhub::testkit::trade::{ExecutionCall, TradeLaneScript};
use vike_gatewayhub::testkit::trade_rig::{
    Rig, fast_trade_timings, order, quick_config, rig_with, start_args,
};
use vike_gatewayhub_client::client::GatewayClient;
use vike_gatewayhub_client::opener::StreamOpenerGateway;
use vike_gatewayhub_client::proto::{AdapterReply, OrderResolution, ResolveState};
use vike_gatewayhub_client::test_support::fake_trade_peer::{Lane, lane_with_capacity};
use vike_gatewayhub_client::trade::{
    TradeAccount, TradeLease, TradeLeaseBuilder, TradeLinkSignal, TradeLinkState,
};
use vike_hub_protocol::auth::Scope;
use vike_log::poll::wait_until;
use vike_market_stream::market::DataClientStream;
use vike_model::AccountTier;
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderFilled, TradeId};

const WAIT: Duration = Duration::from_secs(8);

/// What a lease told its core: each loss, each return and each epoch change, counted.
#[derive(Default)]
struct Told {
    lost: AtomicUsize,
    restored: AtomicUsize,
    epochs: AtomicUsize,
}

impl TradeLinkSignal for Told {
    fn link_lost(&self, _why: &str) {
        self.lost.fetch_add(1, Ordering::SeqCst);
    }
    fn link_restored(&self) {
        self.restored.fetch_add(1, Ordering::SeqCst);
    }
    fn epoch_changed(&self, _old: u64, _new: u64) {
        self.epochs.fetch_add(1, Ordering::SeqCst);
    }
}

impl Told {
    fn counts(&self) -> (usize, usize, usize) {
        (
            self.lost.load(Ordering::SeqCst),
            self.restored.load(Ordering::SeqCst),
            self.epochs.load(Ordering::SeqCst),
        )
    }
}

/// A lease for binance account `label` on account row `row`, its adapter started, and the core lane it delivers to
/// (held by the caller: a lease whose core is gone closes for good).
fn running(
    rig: &Rig,
    client_id: &str,
    label: Option<&str>,
    row: i64,
) -> (TradeLease, Arc<Told>, Lane) {
    running_on(rig, client_id, label, Some(row))
}

/// [`running`] for an account that names no row on its open (a derived lane twin carries none of its own).
fn running_unrowed(rig: &Rig, client_id: &str, label: &str) -> (TradeLease, Arc<Told>, Lane) {
    running_on(rig, client_id, Some(label), None)
}

fn running_on(
    rig: &Rig,
    client_id: &str,
    label: Option<&str>,
    row: Option<i64>,
) -> (TradeLease, Arc<Told>, Lane) {
    let told = Arc::new(Told::default());
    let route_key = label.map_or_else(|| "binance".to_string(), |l| format!("binance#{l}"));
    let account = TradeAccount {
        client_id: client_id.to_string(),
        venue: "binance".to_string(),
        label: label.map(str::to_string),
        tier: AccountTier::Demo,
        route_key,
    };
    let (events, core) = lane_with_capacity(1024);
    let lease = TradeLeaseBuilder::new(rig.gw.trade_link(), account, events)
        .with_timings(fast_trade_timings())
        .with_row(row)
        .with_signal(Arc::clone(&told) as Arc<dyn TradeLinkSignal>)
        .open()
        .expect("the lease opens");
    assert!(matches!(lease.start_adapter(start_args()), Ok(AdapterReply::Running { .. })));
    (lease, told, core)
}

fn epoch(lease: &TradeLease) -> u64 {
    lease.opened().expect("an open lease").gateway_epoch
}

#[test]
fn a_changed_credential_reconnects_its_account_and_leaves_the_other_one_connected() {
    let lane = TradeLaneScript::new(&["binance"])
        .with_row(1, "binance", Some("ALT"))
        .with_row(2, "binance", None);
    let rig = rig_with(lane, quick_config(), TradeKit::default());
    let (alt, alt_told, _alt_core) = running(&rig, "tradehub-alt", Some("ALT"), 1);
    let (base, base_told, _base_core) = running(&rig, "tradehub-base", None, 2);
    let alt_first = rig.lane.adapter("binance", Some("ALT")).expect("ALT's adapter");
    let base_first = rig.lane.adapter("binance", None).expect("the default account's adapter");
    let (alt_epoch, base_epoch) = (epoch(&alt), epoch(&base));
    assert_eq!(rig.lane.starts(), 2);

    // What the datahub relays for `vike-cli secrets set --id 1 FIELD`: the row id, under the control key.
    let mut operator =
        GatewayClient::connect(&rig.gw.link(), Scope::Write).expect("a control connection");
    assert_eq!(operator.credentials_changed(1).expect("answered"), ["binance#ALT"]);

    // ALT's adapter is torn down, and nothing was cancelled or moved at the venue for it.
    assert!(alt_first.is_detached(), "the old adapter is torn down before the answer");
    let touched: Vec<ExecutionCall> = alt_first
        .calls()
        .into_iter()
        .filter(|c| {
            matches!(
                c,
                ExecutionCall::Cancel { .. }
                    | ExecutionCall::CancelBatch { .. }
                    | ExecutionCall::Modify { .. }
            )
        })
        .collect();
    assert!(touched.is_empty(), "a reconnect cancels nothing: {touched:?}");

    // Its client saw ONE drop and ONE return, on a new epoch: to it the daemon restarted for this account.
    assert!(wait_until(WAIT, || alt_told.counts() == (1, 1, 1)), "{:?}", alt_told.counts());
    assert_ne!(epoch(&alt), alt_epoch, "a new epoch, so the client re-teaches and starts again");
    // ...and the start it sends again (the tradehub's re-teaching does) builds a NEW adapter, read afresh.
    assert!(matches!(alt.restart_adapter(start_args()), Ok(AdapterReply::Running { .. })));
    assert_eq!(rig.lane.starts(), 3);
    let alt_second = rig.lane.adapter("binance", Some("ALT")).expect("ALT's new adapter");
    assert!(!alt_second.is_detached());

    // The default account: never dropped, same epoch, same adapter.
    assert_eq!(base_told.counts(), (0, 0, 0));
    assert_eq!(base.link_state(), TradeLinkState::Up);
    assert_eq!(epoch(&base), base_epoch);
    assert!(!base_first.is_detached());

    // An account the daemon runs no session of: nothing to reconnect, nothing touched.
    assert_eq!(operator.credentials_changed(99).expect("answered"), Vec::<String>::new());
    assert_eq!(rig.lane.starts(), 3);
    assert_eq!(base_told.counts(), (0, 0, 0));
}

/// The venue's fill of the whole of `order_client_id`'s order, as its order stream states it.
fn filled(order_client_id: &str) -> Event {
    Event::OrderFilled(OrderFilled {
        order_client_id: order_client_id.to_string(),
        fill: FillEvent {
            trade_id: TradeId::new(format!("x-{order_client_id}")).expect("a non-empty trade id"),
            order_client_id: order_client_id.to_string(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_price: 100.0,
            commission: 0.01,
            commission_asset: "USDT".into(),
            liquidity_side: Default::default(),
            timestamp: 1_700_000_000_100,
            mark_price: None,
            position_side: Default::default(),
        },
        timestamp: 1_700_000_000_100,
    })
}

/// What the core's lane carries for orders, as `(kind, order client id)`.
fn order_words(ingest: &[Ingest]) -> Vec<(&'static str, String)> {
    ingest
        .iter()
        .filter_map(|i| match i.venue_event()? {
            Event::OrderSubmitted(e) => Some(("submitted", e.order_client_id.clone())),
            Event::OrderAccepted(e) => Some(("accepted", e.order_client_id.clone())),
            Event::Fill(f) => Some(("fill", f.order_client_id.clone())),
            Event::OrderFilled(e) => Some(("filled", e.order_client_id.clone())),
            Event::OrderRejected(e) => Some(("rejected", e.order_client_id.clone())),
            _ => None,
        })
        .collect()
}

/// **No order is sent twice across a credential reconnect** (the FIX review's question). The reconnect ends the
/// adapter, and the duplicate guard (`DedupSet`) with it, and begins a new epoch, so what keeps an order single is
/// that NOTHING re-sends one: the client turns a submit nothing answered into a `ResolveOrders` QUERY (its ledger
/// never re-sends a Written order), the new adapter is only asked to `confirm` it, and a resting order is taught
/// (`AdoptRestored`, the tradehub's re-teaching), never placed. One order is at the venue with its ack still on the
/// way when the reconnect comes, one rests acknowledged; after it the venue's fill of each reaches the core (the
/// in-flight one's lost submit and accept stated first, so the core's order walks to FILLED), and the venue holds
/// exactly one order of each.
#[test]
fn an_order_in_flight_or_resting_across_a_credential_reconnect_is_never_sent_again() {
    let lane = TradeLaneScript::new(&["binance"]).with_resolver("binance").with_row(
        1,
        "binance",
        Some("ALT"),
    );
    let rig = rig_with(lane, quick_config(), TradeKit::default());
    let (alt, told, mut core) = running(&rig, "tradehub-alt", Some("ALT"), 1);
    let first = rig.lane.adapter("binance", Some("ALT")).expect("ALT's adapter");
    let mut exec = alt.execution_client();
    exec.submit(&order("c-rest"));
    exec.submit(&order("c-flight"));
    assert!(wait_until(WAIT, || first.submitted().len() == 2), "{:?}", first.calls());
    // The venue acknowledged one, naming its own order id (an ack naming none is a lookup's optimistic answer and
    // settles nothing); the other's ack has not come back when the reconnect cuts the old adapter off.
    assert!(first.emit(Event::OrderAccepted(OrderAccepted {
        order_client_id: "c-rest".into(),
        venue_order_id: Some("v-rest".into()),
        timestamp: 5,
    })));
    let before = core.collect_until(WAIT, 1, |i| i.venue_event().is_some());
    assert_eq!(order_words(&before), [("accepted", "c-rest".to_string())]);

    let mut operator =
        GatewayClient::connect(&rig.gw.link(), Scope::Write).expect("a control connection");
    assert_eq!(operator.credentials_changed(1).expect("answered"), ["binance#ALT"]);
    assert!(wait_until(WAIT, || told.counts() == (1, 1, 1)), "{:?}", told.counts());
    // The in-flight order's id survived the reconnect, PENDING: in doubt, to be asked about, never sent again.
    assert_eq!(alt.unknown_orders(), ["c-flight"]);

    // What the tradehub's re-teaching does on the new epoch (`reteach.rs`): start, teach the resting orders,
    // resolve the pending ones. The new adapter knows neither, so the pending one is QUERIED by its order client id.
    assert!(matches!(alt.restart_adapter(start_args()), Ok(AdapterReply::Running { .. })));
    let second = rig.lane.adapter("binance", Some("ALT")).expect("ALT's new adapter");
    let taught = vec![RestoredOrder {
        venue_order_id: Some("v-rest".into()),
        ..RestoredOrder::new("c-rest", "BTCUSDT")
    }];
    exec.adopt_restored(&taught);
    assert_eq!(
        alt.resolve_orders(&alt.unknown_orders()).expect("answered"),
        [OrderResolution { order_client_id: "c-flight".into(), state: ResolveState::Querying }]
    );
    assert_eq!(
        second.calls(),
        [ExecutionCall::AdoptRestored(taught), ExecutionCall::Confirm("c-flight".into())],
        "the new adapter is taught and asked, and places nothing"
    );

    // The venue answers: both orders filled.
    assert!(second.emit(filled("c-flight")));
    assert!(second.emit(filled("c-rest")));
    let after = core.collect_until(WAIT, 5, |i| i.venue_event().is_some());
    assert_eq!(
        order_words(&after),
        [
            ("submitted", "c-flight".to_string()),
            ("accepted", "c-flight".to_string()),
            ("fill", "c-flight".to_string()),
            ("filled", "c-flight".to_string()),
            ("filled", "c-rest".to_string()),
        ],
        "no reject of an order the venue holds"
    );
    assert!(alt.unknown_orders().is_empty(), "the venue's word settled the pending order");

    // ONE order of each at the venue, across both adapters.
    let placed = |id: &str| {
        [&first, &second]
            .iter()
            .flat_map(|adapter| adapter.submitted())
            .filter(|o| o.order_client_id == id)
            .count()
    };
    assert_eq!((placed("c-rest"), placed("c-flight")), (1, 1));
}

/// **An account that INHERITS the changed row's keys reconnects with it**: binance's lane twin (`PERP`) signs with its
/// parent's keys (it carries no row of its own), so a change of the parent's row reconnects both, and an account on
/// another row stays connected.
#[test]
fn a_lane_twin_reconnects_with_the_row_it_inherits_its_keys_from() {
    let lane = TradeLaneScript::new(&["binance"])
        .with_row(1, "binance", Some("ALT"))
        .with_row(2, "binance", None)
        .with_heir(2, "binance", Some("PERP"));
    let rig = rig_with(lane, quick_config(), TradeKit::default());
    let (_base, base_told, _base_core) = running(&rig, "tradehub-base", None, 2);
    let (_twin, twin_told, _twin_core) = running_unrowed(&rig, "tradehub-twin", "PERP");
    let (_alt, alt_told, _alt_core) = running(&rig, "tradehub-alt", Some("ALT"), 1);
    let mut operator =
        GatewayClient::connect(&rig.gw.link(), Scope::Write).expect("a control connection");
    assert_eq!(operator.credentials_changed(2).expect("answered"), ["binance", "binance#PERP"]);
    assert!(wait_until(WAIT, || base_told.counts() == (1, 1, 1)), "{:?}", base_told.counts());
    assert!(wait_until(WAIT, || twin_told.counts() == (1, 1, 1)), "{:?}", twin_told.counts());
    assert_eq!(alt_told.counts(), (0, 0, 0), "another row's account is untouched");
    assert!(!rig.lane.adapter("binance", Some("ALT")).expect("ALT's adapter").is_detached());
}

/// **A price stream that logged in with the changed row's credentials is reopened on the new ones**: its credentialed
/// session is ended, so its client (the datahub's broker, here the real `DataClientStream`) redials and the new open
/// reads the store afresh; a stream of another venue, logged in as another row, keeps its session.
#[test]
fn a_price_stream_logged_in_with_the_changed_credentials_is_reopened_and_no_other() {
    // A keyless venue too: the data lane is advertised (`gateway_data`) only when it serves one.
    let data = DataLaneScript::new(&["binance"])
        .with_credentialed(&["oanda", "ctrader"])
        .with_feed_account("oanda", 5)
        .with_feed_account("ctrader", 6);
    let gw = LoopbackGateway::start_trade_with(
        data.clone(),
        RestLaneScript::new(),
        TradeLaneScript::new(&["binance"]),
        quick_config(),
        TradeKit::default(),
    );
    let stream = |venue: &str| {
        let opener = StreamOpenerGateway::strategy(gw.link(), None, true, false);
        let mut client = DataClientStream::new(
            opener,
            venue,
            Arc::new(RecordingSink::default()) as Arc<dyn DataClientSink>,
        )
        .expect("a reader");
        client.subscribe_quotes("EURUSD").expect("the stream opens");
        client
    };
    let (mut oanda, mut ctrader) = (stream("oanda"), stream("ctrader"));
    let opens = |venue: &str| {
        data.log()
            .iter()
            .filter(|(_, call)| matches!(call, DataCall::Open { venue: v } if v == venue))
            .count()
    };
    assert!(wait_until(WAIT, || opens("oanda") == 1 && opens("ctrader") == 1));

    let mut operator =
        GatewayClient::connect(&gw.link(), Scope::Write).expect("a control connection");
    assert_eq!(operator.credentials_changed(5).expect("answered"), ["oanda price stream"]);
    assert!(
        wait_until(WAIT, || opens("oanda") == 2),
        "the oanda stream was reopened: {}",
        opens("oanda")
    );
    assert_eq!(opens("ctrader"), 1, "another row's stream keeps its session");
    oanda.shutdown();
    ctrader.shutdown();
}

/// The verb bounces a session, so the observe key (a desktop's) may not send it: refused at the scope check,
/// with nothing touched.
#[test]
fn the_observe_scope_cannot_reconnect_an_account() {
    let lane = TradeLaneScript::new(&["binance"]).with_row(1, "binance", Some("ALT"));
    let rig = rig_with(lane, quick_config(), TradeKit::default());
    let (_alt, alt_told, _alt_core) = running(&rig, "tradehub-alt", Some("ALT"), 1);
    let mut reader = GatewayClient::connect(&rig.gw.read_link(), Scope::Read).expect("connects");
    assert!(reader.credentials_changed(1).is_err(), "a Read connection may not send it");
    assert!(!rig.lane.adapter("binance", Some("ALT")).expect("the adapter").is_detached());
    assert_eq!(alt_told.counts(), (0, 0, 0));
}
