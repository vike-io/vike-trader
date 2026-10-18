//! **The trade LEASE, against the real server and the real client** (B.6 and B.7 of the wave-2 plan):
//! who may open one, what a refusal looks like, the explicit fenced takeover, the watermark a reconnect
//! is answered with, the heartbeat that releases a ghost client, the exemption from the idle read
//! timeout, and the daemon's stop.
#![cfg(feature = "trade")]

use std::time::Duration;

use vike_config::ArmingBlock;
use vike_gatewayhub::testkit::gateway::{LoopbackGateway, TradeKit, keys, trade_keys};
use vike_gatewayhub::testkit::trade::{ScriptedStart, ScriptedTrade, TradeCall};
use vike_gatewayhub_client::client::TradeOpenError;
use vike_gatewayhub_client::proto::{
    KNOWN_TRADE_FEATURES, Request, Response, TradeDown, TradeUp, advertised_trade_venues,
};
use vike_gatewayhub_client::trade::{LinkState, TradeLink};
use vike_hub_proto::auth::Scope;
use vike_model::AccountTier;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/raw_peer.rs"]
mod raw_peer;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use raw_peer::Raw;
use trade_rig::{
    Rig, account, fast_trade_timings, lane, open, order, quick_config, rig, rig_with, start_args,
    wait_until,
};
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::rest::ScriptedRest;

const WAIT: Duration = Duration::from_secs(5);

fn open_request(client_id: &str, venue: &str, tier: AccountTier) -> Request {
    Request::TradeOpen {
        client_id: client_id.to_string(),
        venue: venue.to_string(),
        account: None,
        tier,
        resume: None,
        takeover: false,
    }
}

fn raw_trade(rig: &Rig) -> Raw {
    Raw::open(rig.gw.addr(), &trade_keys(), Scope::Trade)
}

#[test]
fn the_trade_class_is_advertised_by_a_trade_daemon_and_refused_by_a_data_one() {
    let rig = rig();
    let mut raw = raw_trade(&rig);
    for fixed in KNOWN_TRADE_FEATURES {
        assert!(raw.features.iter().any(|f| f == fixed), "{fixed} is not advertised");
    }
    assert_eq!(advertised_trade_venues(&raw.features), vec!["binance".to_string()]);
    raw.send(&Request::ArmingQuery);
    assert!(
        matches!(raw.recv::<Response>(), Response::ArmingTable(_)),
        "the Trade scope admits the trade queries"
    );

    // A daemon with no trade lane serves no Trade scope: the same coarse refusal as a wrong key, and
    // nothing trade-shaped in its Welcome.
    let data_only = LoopbackGateway::start(ScriptedData::new(&[]), ScriptedRest::new());
    let mut raw = Raw::connect(data_only.addr());
    let (_, features) = raw.hello();
    assert!(features.iter().all(|f| !f.starts_with("gw_trade") && !f.starts_with("trade_venue=")));
    match raw.authenticate(trade_keys().key_for(Scope::Trade), Scope::Trade) {
        Response::AuthDenied { reason } => assert_eq!(reason, "bad mac"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_write_key_cannot_open_a_lease_it_is_the_trade_scope_alone() {
    let rig = rig();
    let mut raw = Raw::open(rig.gw.addr(), &keys(), Scope::Write);
    raw.send(&open_request("c1", "binance", AccountTier::Demo));
    match raw.recv::<Response>() {
        Response::Error(text) => {
            assert!(text.contains("TradeOpen") && text.contains("trade"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.lane.starts(), 0);
}

#[test]
fn an_open_is_answered_with_the_epoch_the_lease_generation_and_the_stated_heartbeat() {
    let rig = rig();
    let (session, _lane) = open(&rig, "tradehub-1");
    let opened = session.opened().expect("the open reply is kept");
    assert_ne!(opened.gw_epoch, 0);
    assert!(opened.lease_epoch >= 1);
    assert_eq!(opened.heartbeat_ms, 2_000, "production states the protocol's own period");
    assert_eq!((opened.received_through_cseq, opened.event_ring_floor), (0, Some(0)));
    assert_eq!(opened.project_token, "", "no project state directory was handed to this daemon");
}

#[test]
fn every_refusal_is_a_word_and_leaves_the_connection_positional() {
    let rig = rig_with(
        ScriptedTrade::new(&["binance", "okx"]).with_tier(
            "okx",
            None,
            Err(ArmingBlock::AccountInactive),
        ),
        quick_config(),
        TradeKit::default(),
    );
    let mut raw = raw_trade(&rig);
    let mut refusal = |request: Request| -> Response {
        raw.send(&request);
        let response = raw.recv::<Response>();
        raw.send(&Request::ArmingQuery);
        assert!(
            matches!(raw.recv::<Response>(), Response::ArmingTable(_)),
            "the connection is still positional after {response:?}"
        );
        response
    };

    // A venue this build carries no adapter for.
    match refusal(open_request("c1", "bybit", AccountTier::Demo)) {
        Response::TradeRefused { block_word, text } => {
            assert_eq!(block_word, ArmingBlock::FeatureAbsent.word());
            assert!(text.contains("bybit") && text.contains("binance"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    // An account whose rows state no tier: the block account_tier names.
    match refusal(open_request("c1", "okx", AccountTier::Demo)) {
        Response::TradeRefused { block_word, .. } => {
            assert_eq!(block_word, ArmingBlock::AccountInactive.word());
        }
        other => panic!("{other:?}"),
    }
    // The tier the client believes is re-derived, never trusted.
    match refusal(open_request("c1", "binance", AccountTier::Live)) {
        Response::TradeRefused { block_word, text } => {
            assert_eq!(block_word, ArmingBlock::AdapterFailed.word());
            assert!(text.contains("Demo") && text.contains("Live"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    // A paper account opens no lease.
    match refusal(open_request("c1", "binance", AccountTier::Paper)) {
        Response::TradeRefused { block_word, .. } => {
            assert_eq!(block_word, ArmingBlock::PaperTier.word());
        }
        other => panic!("{other:?}"),
    }
    // Malformed requests are positional errors.
    assert!(matches!(
        refusal(Request::TradeOpen {
            client_id: "c1".into(),
            venue: "binance".into(),
            account: Some("lower case".into()),
            tier: AccountTier::Demo,
            resume: None,
            takeover: false,
        }),
        Response::Error(text) if text.contains("account label")
    ));
    assert!(matches!(
        refusal(open_request("", "binance", AccountTier::Demo)),
        Response::Error(text) if text.contains("client id")
    ));
    assert!(matches!(
        refusal(open_request(&"x".repeat(500), "binance", AccountTier::Demo)),
        Response::Error(text) if text.contains("client id")
    ));
    assert_eq!(rig.lane.starts(), 0, "nothing was started by a refusal");
}

/// **A store that cannot be READ at a `TradeOpen` is a transient plain error, never a refusal of the
/// account** (review 15, finding 3). `Response::TradeRefused` is what a client counts: three in a row on
/// a redial close its lease for good, and the adapter would run on with no tradehub. The store being
/// unreadable says nothing about the account, so the daemon answers `Response::Error` (which the client
/// retries on its dial schedule, uncounted) and keeps `TradeRefused` for the definitive answers: an
/// account that is inactive, has no row, or conflicts.
#[test]
fn an_unreadable_account_store_is_a_retryable_error_and_an_inactive_account_stays_a_refusal() {
    let rig = rig_with(
        ScriptedTrade::new(&["binance", "okx"])
            .with_tier("binance", None, Err(ArmingBlock::AccountNotInStore))
            .with_tier("okx", None, Err(ArmingBlock::AccountInactive)),
        quick_config(),
        TradeKit::default(),
    );
    let mut raw = raw_trade(&rig);
    raw.send(&open_request("c1", "binance", AccountTier::Demo));
    match raw.recv::<Response>() {
        Response::Error(text) => {
            assert!(text.contains("could not be read"), "the reason is named: {text}");
            assert!(text.contains("open again"), "…and it says to retry: {text}");
        }
        other => panic!("an unreadable store must not be an account refusal: {other:?}"),
    }
    raw.send(&open_request("c1", "okx", AccountTier::Demo));
    match raw.recv::<Response>() {
        Response::TradeRefused { block_word, .. } => {
            assert_eq!(
                block_word,
                ArmingBlock::AccountInactive.word(),
                "a definitive answer stays one"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.lane.starts(), 0);
}

#[test]
fn a_second_client_is_refused_naming_the_holder_and_a_takeover_displaces_it() {
    let rig = rig();
    let (first, _l1) = open(&rig, "tradehub-A");
    let holder = trade_keys().key_id(Scope::Trade).expect("the trade key has an id");

    let mut raw = raw_trade(&rig);
    raw.send(&open_request("tradehub-B", "binance", AccountTier::Demo));
    match raw.recv::<Response>() {
        Response::LeaseHeld { holder_key_id, since_ms } => {
            assert_eq!(holder_key_id, holder, "named by key id, never by key");
            assert!(since_ms > 0);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(first.link_state(), LinkState::Up, "the holder was not disturbed by the refusal");

    // Through the client: a takeover is explicit.
    let (events, _l2) = lane();
    let err = TradeLink::new(rig.gw.trade_link(), account("tradehub-B"), events)
        .with_timings(fast_trade_timings())
        .open()
        .expect_err("held");
    assert!(matches!(err, TradeOpenError::LeaseHeld { .. }), "{err:?}");
    let (events, _l3) = lane();
    let second = TradeLink::new(rig.gw.trade_link(), account("tradehub-B"), events)
        .with_timings(fast_trade_timings())
        .with_takeover(true)
        .open()
        .expect("a takeover is granted");
    assert!(
        wait_until(WAIT, || first.link_state() == LinkState::Closed),
        "the displaced holder was told and closed for good (it does not fight back)"
    );
    assert_eq!(second.link_state(), LinkState::Up);
    let reason = first.closed_reason().expect("a reason");
    assert!(reason.contains("taken over") || reason.contains("takeover"), "{reason}");
}

#[test]
fn a_reconnect_is_fenced_the_old_connection_is_read_to_its_end_before_the_new_open_is_answered() {
    // The first connection's reader is held inside a slow adapter start while its client has already
    // written three orders: they sit unread in the kernel. The reconnect must not be answered until they
    // are read, so the watermark it is handed counts them.
    let rig = rig_with(
        ScriptedTrade::new(&["binance"]).with_start(
            "binance",
            None,
            ScriptedStart::live().after(Duration::from_millis(500)),
        ),
        quick_config(),
        TradeKit::default(),
    );
    let mut old = raw_trade(&rig);
    old.send(&open_request("tradehub-1", "binance", AccountTier::Demo));
    assert!(matches!(old.recv::<Response>(), Response::TradeOpened(_)));
    let mut start = start_args();
    start.rid = 1;
    start.route_key = "binance".into();
    old.send(&TradeUp::AdapterStart(start));
    for cseq in 1..=3 {
        old.send(&TradeUp::Submit { cseq, order: order(&format!("c{cseq}")) });
    }
    assert!(
        wait_until(WAIT, || rig.lane.calls().iter().any(|c| matches!(c, TradeCall::Start { .. }))),
        "the slow start is under way"
    );

    let mut new = raw_trade(&rig);
    new.send(&open_request("tradehub-1", "binance", AccountTier::Demo));
    match new.recv::<Response>() {
        Response::TradeOpened(opened) => {
            assert_eq!(
                opened.received_through_cseq, 3,
                "everything the old connection sent was read"
            );
        }
        other => panic!("{other:?}"),
    }
    let adapter = rig.lane.adapter("binance", None).expect("the adapter outlived the connection");
    assert!(
        wait_until(WAIT, || adapter.submitted().len() == 3),
        "and EXECUTED: {:?}",
        adapter.submitted()
    );
}

#[test]
fn a_silent_client_cannot_hold_a_lease_and_the_daemon_heartbeats_meanwhile() {
    let mut config = quick_config();
    config.timings.trade_heartbeat = Duration::from_millis(100);
    config.timings.trade_read_floor = Duration::from_millis(300);
    let rig = rig_with(ScriptedTrade::new(&["binance"]), config, TradeKit::default());
    let mut ghost = raw_trade(&rig);
    ghost.send(&open_request("ghost", "binance", AccountTier::Demo));
    assert!(matches!(ghost.recv::<Response>(), Response::TradeOpened(_)));
    // The ghost says nothing. The daemon keeps writing a heartbeat at the period it stated...
    let mut beats = 0;
    while beats < 2 {
        match ghost.recv_within::<TradeDown>(Duration::from_secs(2)) {
            Ok(TradeDown::Heartbeat) => beats += 1,
            Ok(other) => panic!("unexpected {other:?}"),
            Err(e) => panic!("no heartbeat: {e}"),
        }
    }
    // ...and releases the lease at max(read floor, 3 x heartbeat): another client now gets it with no
    // takeover.
    assert!(
        wait_until(Duration::from_secs(3), || {
            let (events, _l) = lane();
            TradeLink::new(rig.gw.trade_link(), account("tradehub-2"), events)
                .with_timings(fast_trade_timings())
                .open()
                .is_ok()
        }),
        "a ghost client held the lease past its deadline"
    );
}

#[test]
fn a_trade_connection_is_exempt_from_the_idle_read_timeout() {
    let mut config = quick_config();
    config.timings.idle_read_timeout = Duration::from_millis(300);
    config.timings.trade_heartbeat = Duration::from_millis(200);
    config.timings.trade_read_floor = Duration::from_secs(5);
    let rig = rig_with(ScriptedTrade::new(&["binance"]), config, TradeKit::default());

    // The control: an rpc connection that says nothing IS reaped at the idle timeout.
    let mut idle_rpc = raw_trade(&rig);
    assert!(idle_rpc.closed_within(Duration::from_secs(3)), "the idle rpc connection survived");

    // The lease says nothing for several idle timeouts and is still there, heartbeating.
    let mut lease = raw_trade(&rig);
    lease.send(&open_request("c1", "binance", AccountTier::Demo));
    assert!(matches!(lease.recv::<Response>(), Response::TradeOpened(_)));
    let window = std::time::Instant::now();
    let mut beats = 0;
    while window.elapsed() < Duration::from_millis(1_400) {
        match lease.recv_within::<TradeDown>(Duration::from_millis(500)) {
            Ok(TradeDown::Heartbeat) => beats += 1,
            Ok(other) => panic!("unexpected {other:?}"),
            Err(e) => panic!("the lease went quiet or closed after {beats} heartbeats: {e}"),
        }
    }
    assert!(beats >= 4, "the lease went quiet after {beats} heartbeats");
    lease.send(&TradeUp::Ping);
    loop {
        match lease.recv::<TradeDown>() {
            TradeDown::Pong => break,
            TradeDown::Heartbeat => {}
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn the_daemons_stop_says_goodbye_to_a_lease_and_detaches_its_adapter() {
    let Rig { gw, lane: scripted } = rig();
    let (events, _core) = lane();
    let session = TradeLink::new(gw.trade_link(), account("tradehub-1"), events)
        .with_timings(fast_trade_timings())
        .open()
        .expect("opens");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        vike_gatewayhub_client::proto::AdapterReply::Live { .. }
    ));
    let adapter = scripted.adapter("binance", None).expect("the adapter");
    assert!(!adapter.is_detached());
    drop(gw);
    assert!(wait_until(WAIT, || adapter.is_detached()), "the adapter was not detached at the stop");
    assert!(
        wait_until(WAIT, || session.link_state() != LinkState::Up),
        "the client was not told the daemon stopped"
    );
}

#[test]
fn a_clients_final_detach_is_answered_bye_client_closed_and_ends_the_session_not_a_link_flap() {
    let rig = rig();
    let mut raw = raw_trade(&rig);
    raw.send(&open_request("tradehub-1", "binance", AccountTier::Demo));
    assert!(matches!(raw.recv::<Response>(), Response::TradeOpened(_)));
    let mut start = start_args();
    start.rid = 1;
    start.route_key = "binance".into();
    raw.send(&TradeUp::AdapterStart(start));
    loop {
        match raw.recv::<TradeDown>() {
            TradeDown::AdapterReply { .. } => break,
            TradeDown::Heartbeat => {}
            other => panic!("{other:?}"),
        }
    }
    let adapter = rig.lane.adapter("binance", None).expect("the adapter");

    // Phase one raises nothing on the daemon's adapter: a client that dies between the phases must not
    // leave a half-stopped adapter behind a lease that survives it.
    raw.send(&TradeUp::Detach { phase: vike_gatewayhub_client::proto::DetachPhase::Begin });
    std::thread::sleep(Duration::from_millis(100));
    assert!(!adapter.is_detached());

    raw.send(&TradeUp::Detach { phase: vike_gatewayhub_client::proto::DetachPhase::Final });
    loop {
        match raw.recv::<TradeDown>() {
            TradeDown::Bye(vike_gatewayhub_client::proto::TradeBye::ClientClosed) => break,
            TradeDown::Heartbeat => {}
            other => panic!("{other:?}"),
        }
    }
    assert!(wait_until(WAIT, || adapter.is_detached()), "a final detach tears the adapter down");

    // A new open is a NEW session: a new adapter.
    let (session, _lane) = open(&rig, "tradehub-1");
    session.start_adapter(start_args()).expect("started");
    assert_eq!(rig.lane.starts(), 2);
}

/// **The real client reads an unreadable account store as a daemon ERROR, not a refusal, over the real
/// wire** (review 15, finding 3): `TradeOpenError::Daemon` is what the link retries on its dial schedule
/// without counting it, `TradeOpenError::Refused` is what closes a lease after three. Once the store
/// reads, the very same open is accepted.
#[test]
fn the_real_client_retries_an_unreadable_store_and_is_accepted_once_it_reads() {
    let rig = rig_with(
        ScriptedTrade::new(&["binance"]).with_tier(
            "binance",
            None,
            Err(ArmingBlock::AccountNotInStore),
        ),
        quick_config(),
        TradeKit::default(),
    );
    let attempt = |rig: &Rig| {
        let (events, _lane) = lane();
        TradeLink::new(rig.gw.trade_link(), account("tradehub-A"), events)
            .with_timings(fast_trade_timings())
            .open()
    };
    match attempt(&rig) {
        Err(TradeOpenError::Daemon(text)) => {
            assert!(text.contains("could not be read"), "{text}");
        }
        Err(other) => {
            panic!("an unreadable store must reach the client as a daemon error: {other}")
        }
        Ok(_) => panic!("an account whose store cannot be read must not be admitted"),
    }
    rig.lane.set_tier("binance", None, Ok(AccountTier::Demo));
    assert!(attempt(&rig).is_ok(), "the store reads again: the same open is accepted");
}
