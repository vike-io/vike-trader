use super::*;
use std::assert_matches;
use vike_marketdata::test_support::flat_bar_unit_volume;
use vike_model::TradingState;
// `BodyCodec`/`project` come in via `super::*`; the frame reads and `Response` are extra here.
use vike_hub_protocol::frame::{read_frame, read_frame_raw};
use vike_tradehub_client::proto::Response;

/// The identity block is threaded through the projection verbatim (split-plane B3), and its
/// absence stays absent — an identity-less node publishes exactly the old shape.
#[test]
fn project_threads_the_identity_through() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let id = HubIdentityWire {
        name: "n".into(),
        strategy: "s".into(),
        params: "p".into(),
        live: false,
        build: "b".into(),
        advertise_addr: String::new(),
        priced_venues: None,
    };
    let wire = project(&snap, Some(&id), &[]);
    assert_eq!(wire.identity.as_ref().unwrap().strategy, "s");
    assert!(project(&snap, None, &[]).identity.is_none());
}

/// The armed core-held stops cross the projection whole (the row's `route_key` is the wire's `account`), and a
/// node that holds none writes no key at all: its frame is byte-identical to one from before the field.
#[test]
fn project_lists_the_armed_core_held_stops_and_writes_no_key_for_none() {
    let mut snap = CoreSnapshot::empty("polymarket", "TOK");
    let none = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!none.contains("core_held_stops"), "{none}");

    snap.core_held_stops = vec![vike_execution::OrderEmulationStopView {
        arm_id: "s0a3".into(),
        venue: "polymarket".into(),
        symbol: "TOK".into(),
        route_key: "polymarket#ALT".into(),
        side: -1,
        qty: 4.0,
        trigger_price: 0.35,
        held: true,
        rearms: 0,
    }];
    let wire = project(&snap, None, &[]);
    assert_eq!(
        wire.core_held_stops,
        vec![OrderEmulationStopViewWire {
            arm_id: "s0a3".into(),
            venue: "polymarket".into(),
            symbol: "TOK".into(),
            account: "polymarket#ALT".into(),
            side: -1,
            qty: 4.0,
            trigger_price: 0.35,
            held: true,
            rearms: 0,
        }]
    );
    let json = serde_json::to_string(&wire).expect("serializes");
    assert!(json.contains("\"held\":true") && !json.contains("rearms"), "{json}");
}

/// Every emulated order crosses the projection field for field (decision 0275): the account through its label,
/// the peg as its anchor's word; a node that emulates none writes no key.
#[test]
fn project_lists_the_emulated_orders_and_writes_no_key_for_none() {
    use vike_model::accounts::account_keys::AccountLabel;
    let mut snap = CoreSnapshot::empty("bybit", "BTCUSDT");
    let none = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!none.contains("emulated_orders"), "{none}");

    snap.emulated_orders = vec![vike_execution::OrderEmulationView {
        id: "c-9".into(),
        venue: "bybit".into(),
        account: Some(AccountLabel::parse("ALT").expect("a label")),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        kind: "chase",
        reduce_only: false,
        trigger: None,
        price: None,
        trail: None,
        peg: Some(vike_model::OrderPeg { anchor: vike_model::OrderPegAnchor::Mid, offset: -0.5 }),
        display_quantity: None,
        filled: 0.25,
        average_price: 60_000.0,
        child: Some("c-9-2".into()),
        waiting: Some("no fresh touch".into()),
        rearms: 2,
        held: false,
    }];
    assert_eq!(
        project(&snap, None, &[]).emulated_orders,
        vec![OrderEmulationViewWire {
            id: "c-9".into(),
            venue: "bybit".into(),
            account: Some("ALT".into()),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            kind: "chase".into(),
            reduce_only: false,
            trigger: None,
            price: None,
            trail: None,
            peg: Some(vike_tradehub_client::wire::OrderPegWire {
                anchor: "mid".into(),
                offset: -0.5
            }),
            display_quantity: None,
            filled: 0.25,
            average_price: 60_000.0,
            child: Some("c-9-2".into()),
            waiting: Some("no fresh touch".into()),
            rearms: 2,
            held: false,
        }]
    );
}

/// The projection carries the rendered fields the GUI needs: seq, venue/symbol, trading state,
/// equity, and each order (with its status rendered to a string).
#[test]
fn project_maps_the_rendered_fields() {
    let mut snap = CoreSnapshot::empty("polymarket", "TOK");
    snap.sequence = 7;
    snap.trading_state = TradingState::Reducing;
    snap.portfolio.equity_total = 1234.5;
    snap.orders.push(std::sync::Arc::new(OrderView {
        data_delay: None,
        order_client_id: "c-1".into(),
        venue: "polymarket".into(),
        account: None,
        symbol: "TOK".into(),
        side: 1,
        qty: 20.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(0.4),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: Some("v-9".into()),
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }));

    let wire = project(&snap, None, &[]);
    assert_eq!(wire.sequence, 7);
    assert_eq!(wire.venue, "polymarket");
    assert_eq!(wire.symbol, "TOK");
    assert_eq!(wire.trading_state, TradingState::Reducing);
    assert_eq!(wire.equity_total.to_bits(), 1234.5_f64.to_bits());
    assert_eq!(wire.orders.len(), 1);
    assert_eq!(wire.orders[0].order_client_id, "c-1");
    assert_eq!(wire.orders[0].status, "ACCEPTED", "status is rendered to its FSM word (`as_str`)");
    assert_eq!((&wire.orders[0].reason, &wire.orders[0].refusal_id), (&None, &None));

    // A refusal crosses with its catalog id beside its words.
    let refused = std::sync::Arc::make_mut(&mut snap.orders[0]);
    refused.status = vike_model::OrderStatus::Rejected;
    refused.reason = Some("too small".into());
    refused.refusal_id = Some("order.venue.rejected".into());
    let wire = project(&snap, None, &[]);
    assert_eq!(
        (wire.orders[0].reason.as_deref(), wire.orders[0].refusal_id.as_deref()),
        (Some("too small"), Some("order.venue.rejected"))
    );
}

/// A framed snapshot decodes back into a `Response::SnapshotFrame` whose wire matches the direct
/// projection — the "serialize once" bytes are a valid frame.
#[test]
fn frame_snapshot_round_trips_as_a_snapshot_frame() {
    let mut snap = CoreSnapshot::empty("sim", "BTCUSDT");
    snap.sequence = 3;
    let bytes = frame_snapshot(&snap, None, &[], &[], &NodeReadings::default(), BodyCodec::Json);
    let mut cursor = std::io::Cursor::new(bytes.as_slice());
    match read_frame::<_, Response>(&mut cursor).expect("decode frame") {
        Response::SnapshotFrame(w) => {
            assert_eq!(w.sequence, 3);
            // A frame is the projection plus what the publisher reads off the node: the uptime is
            // always stamped, and a publisher never handed a sentinel path says nothing about it.
            assert!(w.uptime_ms.is_some());
            assert_eq!(w.halt_sentinel, None);
            let mut expected = project(&snap, None, &[]);
            expected.uptime_ms = w.uptime_ms;
            assert!(w.published_ms > 0, "a frame says when it was framed");
            expected.published_ms = w.published_ms;
            // A publisher never handed the alert mount's count says none failed.
            expected.alert_deliveries_failed = Some(0);
            assert_eq!(*w, expected);
        }
        other => panic!("expected SnapshotFrame, got {other:?}"),
    }
}

/// The key-changed list is stamped into the frame beside `not_running` (route keys only), and a frame
/// without any carries none.
#[test]
fn frame_snapshot_stamps_the_key_changed_route_keys() {
    let snap = CoreSnapshot::empty("sim", "BTCUSDT");
    let framed = |keys: &[String]| match read_frame::<_, Response>(&mut std::io::Cursor::new(
        frame_snapshot(&snap, None, &[], keys, &NodeReadings::default(), BodyCodec::Json).to_vec(),
    ))
    .expect("decode frame")
    {
        Response::SnapshotFrame(w) => w.key_changed,
        other => panic!("expected SnapshotFrame, got {other:?}"),
    };
    assert!(framed(&[]).is_empty());
    assert_eq!(framed(&["binance#ALT".to_string()]), vec!["binance#ALT".to_string()]);
}

/// **A frame lists the accounts whose venue balance the node holds back, for the engines that RUN**: the lease
/// registry's gates report them (`TradeLeaseRegistry::set_balance_held`), the publisher stamps them beside the rest of its
/// own readings, a hold whose engine has no block in the frame is history and not listed, a publisher with no lease
/// registry lists none, and a frame that lists none carries no key (byte-identical to an older node's).
#[test]
fn frame_snapshot_stamps_the_balance_held_of_the_running_accounts_only() {
    use crate::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry;
    let mut snap = CoreSnapshot::empty("oanda", "EURUSD");
    snap.portfolio.venues = vec![vike_execution::VenueBlock {
        venue: "oanda".into(),
        route_key: "oanda".into(),
        ..Default::default()
    }];
    let source = Arc::new(TradeLeaseRegistry::default());
    let facts = NodeReadings::default();
    assert!(facts.readiness.set(source.clone()).is_ok());
    assert!(facts.link_health.set(source.clone()).is_ok());

    let framed = |facts: &NodeReadings| {
        let bytes = frame_snapshot(&snap, None, &[], &[], facts, BodyCodec::Json);
        let json = String::from_utf8_lossy(&bytes).into_owned();
        match read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.as_slice().to_vec()))
            .expect("decode frame")
        {
            Response::SnapshotFrame(w) => (w.balance_withheld, json),
            other => panic!("expected SnapshotFrame, got {other:?}"),
        }
    };
    let (none, json) = framed(&facts);
    assert!(none.is_empty() && !json.contains("balance_withheld"), "nothing held: no key: {json}");

    source.set_balance_held("oanda", Some("assumed USD"));
    source.set_balance_held("fxcm", Some("stated EUR not anchored"));
    let (held, json) = framed(&facts);
    assert_eq!(
        held,
        vec![BalanceWithheldWire { route_key: "oanda".into(), reason: "assumed USD".into() }],
        "fxcm has no engine block in this frame: its hold is not listed"
    );
    assert!(json.contains("\"balance_withheld\""), "{json}");

    let (none, _) = framed(&NodeReadings::default());
    assert!(none.is_empty(), "a publisher with no lease registry lists none");
}

/// **A frame lists the venue paths that are down, for the engines that RUN**: the lease worker's poll reports them
/// (`TradeLeaseRegistry::set_venue_link_down`), the publisher stamps them beside `balance_withheld`, an outage whose engine has
/// no block in the frame is history and not listed, an outage that ended is not listed, a publisher with no lease
/// registry lists none, and a frame that lists none carries no key (byte-identical to an older node's).
#[test]
fn frame_snapshot_stamps_the_down_venue_paths_of_the_running_accounts_only() {
    use crate::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry;
    use vike_tradehub_client::wire::VenueLinkDownWire;
    let mut snap = CoreSnapshot::empty("dukascopy", "EURUSD");
    snap.portfolio.venues = vec![vike_execution::VenueBlock {
        venue: "dukascopy".into(),
        route_key: "dukascopy".into(),
        ..Default::default()
    }];
    let source = Arc::new(TradeLeaseRegistry::default());
    let facts = NodeReadings::default();
    assert!(facts.readiness.set(source.clone()).is_ok());
    assert!(facts.link_health.set(source.clone()).is_ok());
    let down = |key: &str| VenueLinkDownWire {
        route_key: key.into(),
        why: "its output ended and the process exited".into(),
        failed_restarts: 0,
        retry_in_ms: 5_000,
        effect: String::new(),
        no_adapter: false,
    };

    let framed = |facts: &NodeReadings| {
        let bytes = frame_snapshot(&snap, None, &[], &[], facts, BodyCodec::Json);
        let json = String::from_utf8_lossy(&bytes).into_owned();
        match read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.as_slice().to_vec()))
            .expect("decode frame")
        {
            Response::SnapshotFrame(w) => (w.venue_link_down, json),
            other => panic!("expected SnapshotFrame, got {other:?}"),
        }
    };
    let (none, json) = framed(&facts);
    assert!(none.is_empty() && !json.contains("venue_link_down"), "all up: no key: {json}");

    source.set_venue_link_down("dukascopy", Some(down("dukascopy")));
    source.set_venue_link_down("fxcm", Some(down("fxcm")));
    let (listed, json) = framed(&facts);
    assert_eq!(
        listed,
        vec![down("dukascopy")],
        "fxcm has no engine block in this frame: its outage is not listed"
    );
    assert!(json.contains("\"venue_link_down\""), "{json}");

    source.set_venue_link_down("dukascopy", None);
    let (back, json) = framed(&facts);
    assert!(back.is_empty() && !json.contains("venue_link_down"), "an ended outage is not listed");

    source.set_venue_link_down("dukascopy", Some(down("dukascopy")));
    let (none, _) = framed(&NodeReadings::default());
    assert!(none.is_empty(), "a publisher with no lease registry lists none");
}

/// A lease registry whose hub links a test sets by hand.
#[derive(Default)]
struct Links {
    trade: Mutex<Vec<vike_tradehub_client::wire::TradeLinkDownWire>>,
    price: Mutex<Vec<vike_tradehub_client::wire::PriceLinkDownWire>>,
    path: Mutex<Vec<vike_tradehub_client::wire::VenueLinkDownWire>>,
    path_up: Mutex<Vec<String>>,
    dials_up: Mutex<Option<u32>>,
    refusal: Mutex<Option<String>>,
}

impl crate::server::symbol_admission::AccountReadiness for Links {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn start_refusal(&self, _: &str) -> Option<String> {
        self.refusal.lock().unwrap().clone()
    }
}

impl crate::server::symbol_admission::LinkHealth for Links {
    fn trade_link_down(&self) -> Vec<vike_tradehub_client::wire::TradeLinkDownWire> {
        self.trade.lock().unwrap().clone()
    }
    fn price_link_down(&self) -> Vec<vike_tradehub_client::wire::PriceLinkDownWire> {
        self.price.lock().unwrap().clone()
    }
    fn venue_link_down(&self) -> Vec<vike_tradehub_client::wire::VenueLinkDownWire> {
        self.path.lock().unwrap().clone()
    }
    fn venue_path_up(&self) -> Vec<String> {
        self.path_up.lock().unwrap().clone()
    }
    fn price_dials_up(&self) -> Option<u32> {
        *self.dials_up.lock().unwrap()
    }
}

fn lost_link(route_key: &str) -> vike_tradehub_client::wire::TradeLinkDownWire {
    vike_tradehub_client::wire::TradeLinkDownWire {
        route_key: route_key.into(),
        why: "the gatewayhub is stopping".into(),
        down_ms: 40,
        closed: false,
    }
}

fn lost_feed(venue: &str) -> vike_tradehub_client::wire::PriceLinkDownWire {
    vike_tradehub_client::wire::PriceLinkDownWire {
        venue: venue.into(),
        why: "connection refused".into(),
        down_ms: 40,
        of: 2,
    }
}

/// **A frame lists the lost links of the accounts that RUN, and every price link**: an account with no block in the
/// frame, or a stopped one that keeps its block (its lease is over on purpose), is not listed; a price feed is the
/// node's and names no account; a publisher with no lease registry lists none, and no key is written for none.
#[test]
fn frame_snapshot_stamps_the_lost_hub_links_of_the_accounts_that_run() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let block = |key: &str| vike_execution::VenueBlock {
        venue: vike_model::accounts::account_keys::venue_of_route_key(key).into(),
        route_key: key.into(),
        ..Default::default()
    };
    snap.portfolio.venues = vec![block("binance"), block("bybit")];
    let stopped = [ArmingNotRunningView {
        account: default_account_of("bybit"),
        tier: None,
        reason: "account inactive".into(),
        block: Some(vike_model::ArmingBlock::AccountInactive),
        refusal_id: None,
    }];
    let source = Arc::new(Links::default());
    let facts = NodeReadings::default();
    assert!(facts.readiness.set(source.clone()).is_ok());
    assert!(facts.link_health.set(source.clone()).is_ok());
    let framed = |facts: &NodeReadings| {
        let bytes = frame_snapshot(&snap, None, &stopped, &[], facts, BodyCodec::Json);
        let json = String::from_utf8_lossy(&bytes).into_owned();
        match read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.as_slice().to_vec()))
            .expect("decode frame")
        {
            Response::SnapshotFrame(w) => (w, json),
            other => panic!("expected SnapshotFrame, got {other:?}"),
        }
    };
    let (w, json) = framed(&facts);
    assert!(w.trade_link_down.is_empty() && w.price_link_down.is_empty());
    assert!(!json.contains("trade_link_down") && !json.contains("price_link_down"), "{json}");

    *source.trade.lock().unwrap() =
        vec![lost_link("binance"), lost_link("bybit"), lost_link("okx")];
    *source.price.lock().unwrap() = vec![lost_feed("okx")];
    let (w, json) = framed(&facts);
    assert_eq!(w.trade_link_down, vec![lost_link("binance")], "bybit is stopped, okx has no block");
    assert_eq!(w.price_link_down, vec![lost_feed("okx")]);
    assert!(json.contains("\"trade_link_down\"") && json.contains("\"price_link_down\""), "{json}");

    let (w, _) = framed(&NodeReadings::default());
    assert!(w.trade_link_down.is_empty() && w.price_link_down.is_empty(), "no registry, none");

    // The positive readings follow the same rule: only an account that runs, and a count that is not invented.
    *source.path_up.lock().unwrap() = ["binance", "bybit", "okx"].map(String::from).to_vec();
    *source.dials_up.lock().unwrap() = Some(0);
    let (w, json) = framed(&facts);
    assert_eq!(w.venue_path_up, ["binance"], "bybit is stopped, okx has no block");
    assert_eq!(w.price_dials_up, Some(0), "zero is a reading, and is written");
    assert!(json.contains("\"price_dials_up\":0"), "{json}");
    let (w, json) = framed(&NodeReadings::default());
    assert_eq!((w.venue_path_up.len(), w.price_dials_up), (0, None), "no registry, not said");
    assert!(!json.contains("venue_path_up") && !json.contains("price_dials_up"), "{json}");
}

/// **`adapter failed` and `venue unreachable` carry their cause, from the registry and not from the core's row** (the
/// second: "IBKR gateway not logged in yet", owner ruling 2026-11-16); a row with another block does not, so the last
/// refused start's words never hang on a stop that came later. Read off the row's BLOCK, never its words: a row
/// worded `adapter failed` that no block stopped carries nothing.
#[test]
fn only_a_refused_start_row_carries_the_cause_of_the_refused_start() {
    use vike_model::ArmingBlock;
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let row = |venue: &str, block: Option<ArmingBlock>| ArmingNotRunningView {
        account: default_account_of(venue),
        tier: None,
        reason: block.map_or("adapter failed", ArmingBlock::as_str).into(),
        block,
        refusal_id: None,
    };
    let not_running = [
        row("ibkr", Some(ArmingBlock::AdapterFailed)),
        row("oanda", Some(ArmingBlock::AccountInactive)),
        row("ibkr", Some(ArmingBlock::VenueUnreachable)),
        row("ibkr", None),
    ];
    let source = Arc::new(Links::default());
    *source.refusal.lock().unwrap() = Some("the lease is held by key-9".to_string());
    let facts = NodeReadings::default();
    assert!(facts.readiness.set(source.clone()).is_ok());
    assert!(facts.link_health.set(source).is_ok());
    let bytes = frame_snapshot(&snap, None, &not_running, &[], &facts, BodyCodec::Json);
    let Response::SnapshotFrame(w) =
        read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.as_slice().to_vec()))
            .expect("decode frame")
    else {
        panic!("expected SnapshotFrame");
    };
    assert_eq!(w.not_running[0].detail.as_deref(), Some("the lease is held by key-9"));
    assert_eq!(w.not_running[1].detail, None);
    assert_eq!(w.not_running[2].detail.as_deref(), Some("the lease is held by key-9"));
    assert_eq!(w.not_running[3].detail, None, "no block, whatever the words");
}

/// **A gatewayhub's refusal reaches the NOT RUNNING row with ONE id, its own**: the gatewayhub words a lease it refuses
/// (`AdapterReply::NotRunning`, the lease open's `Error`) as its rendered refusal, `error [gatewayhub.…]: …`, and the
/// node stamps those words onto the row's `detail` as they came, never wrapped in a refusal of its own; the row's
/// `reason` is the block's badge, with no id. Shortcut: the gatewayhub's rendered words are built here by hand (the id is
/// a stand-in, #3542 is not on main yet); read them off its enum once it lands.
#[test]
fn a_gatewayhub_refusal_reaches_the_not_running_row_with_one_id() {
    use vike_model::ArmingBlock;
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let not_running = [ArmingNotRunningView {
        account: default_account_of("ibkr"),
        tier: None,
        reason: ArmingBlock::AdapterFailed.as_str().into(),
        block: Some(ArmingBlock::AdapterFailed),
        refusal_id: None,
    }];
    let said = vike_utility::errors::render_refusal(
        "gatewayhub.trade.lease_held",
        "the lease is held by key-9",
    );
    let source = Arc::new(Links::default());
    *source.refusal.lock().unwrap() = Some(said.clone());
    let facts = NodeReadings::default();
    assert!(facts.readiness.set(source.clone()).is_ok());
    assert!(facts.link_health.set(source).is_ok());
    let bytes = frame_snapshot(&snap, None, &not_running, &[], &facts, BodyCodec::Json);
    let Response::SnapshotFrame(w) =
        read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.as_slice().to_vec()))
            .expect("decode frame")
    else {
        panic!("expected SnapshotFrame");
    };
    let row = &w.not_running[0];
    assert_eq!(row.detail.as_deref(), Some(said.as_str()), "the gatewayhub's words, as they came");
    let shown = format!("{}: {}", row.reason, row.detail.as_deref().unwrap_or_default());
    assert_eq!(shown.matches("error [").count(), 1, "one id, the gatewayhub's: {shown}");
    assert_eq!(
        row.reason,
        ArmingBlock::AdapterFailed.as_str(),
        "the badge is the block's word, no id"
    );
}

/// **The node's own NOT RUNNING sentence reaches every reader with ONE id, its own**: the row's `reason` crosses bare and
/// its id beside it (`refusal_id`), every surface shows them through `ArmingNotRunningViewWire::say` (the desktop through
/// `observe_decode::map_not_running`), and a block's badge crosses with none. The row is worded as the account watcher
/// words a restart with a position open (`crate::account_follow::restart_reason`): the wrapped words stay bare.
#[test]
fn a_not_running_sentence_of_the_nodes_own_reaches_every_reader_with_one_id() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let open = [("BTCUSDT".to_string(), 0.5)];
    let worded = crate::account_follow::restart_reason(vike_model::AccountTier::Live, &open);
    let not_running = [
        ArmingNotRunningView {
            account: default_account_of("bybit"),
            tier: Some(vike_model::AccountTier::Live),
            reason: worded.to_string(),
            block: None,
            refusal_id: vike_utility::errors::Declared::declared_id(&worded).map(str::to_owned),
        },
        ArmingNotRunningView {
            account: default_account_of("okx"),
            tier: None,
            reason: "no credentials".into(),
            block: Some(vike_model::ArmingBlock::NoCredentials),
            refusal_id: None,
        },
    ];
    let facts = NodeReadings::default();
    let bytes = frame_snapshot(&snap, None, &not_running, &[], &facts, BodyCodec::Json);
    let Response::SnapshotFrame(w) =
        read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.as_slice().to_vec()))
            .expect("decode frame")
    else {
        panic!("expected SnapshotFrame");
    };
    let row = &w.not_running[0];
    assert_eq!(row.reason, worded.to_string(), "the bare text");
    assert_eq!(row.refusal_id.as_deref(), Some("tradehub.not_running.position_open"));
    let shown = "error [tradehub.not_running.position_open]: tier changed, restarting at `live`; position \
                 open at venue: BTCUSDT 0.5";
    assert_eq!(row.say(), shown);
    assert_eq!(
        vike_tradehub_client::observe_decode::map_not_running(row).reason,
        shown,
        "the desktop reads the same line"
    );
    let badge = &w.not_running[1];
    assert_eq!((badge.refusal_id.as_deref(), badge.say()), (None, "no credentials".to_string()));
}

/// **A link that moves reaches a subscriber with no fold behind it**: a dead datahub stops the very ticks that keep
/// frames coming, so a transition frames the current snapshot again, down and back, with the `seq` where it was.
#[test]
fn a_hub_link_that_moves_reframes_without_a_sequence_change() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.sequence = 9;
    snap.portfolio.venues = vec![vike_execution::VenueBlock {
        venue: "binance".into(),
        route_key: "binance".into(),
        ..Default::default()
    }];
    let cell = Arc::new(ArcSwap::from_pointee(snap));
    let source = Arc::new(Links::default());
    let publisher = spawn(Arc::clone(&cell), None).with_symbol_sources(
        crate::server::symbol_admission::SymbolSources {
            readiness: Some(source.clone()),
            link_health: Some(source.clone()),
            ..Default::default()
        },
    );
    let subscription = publisher.subscribe();

    // Until a frame carries what `seen` asks of it; a frame that does not is the one before the transition.
    let frame_with = |seen: &dyn Fn(&CoreSnapshotWire) -> bool| -> CoreSnapshotWire {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let PublisherMailboxReceive::Frame(bytes) =
                subscription.recv_timeout(Duration::from_millis(100))
            {
                let decoded = read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.to_vec()));
                if let Ok(Response::SnapshotFrame(w)) = decoded
                    && seen(&w)
                {
                    return *w;
                }
            }
        }
        panic!("no frame showed the transition");
    };
    assert_eq!(frame_with(&|w| w.trade_link_down.is_empty()).sequence, 9);

    *source.trade.lock().unwrap() = vec![lost_link("binance")];
    assert_eq!(frame_with(&|w| !w.trade_link_down.is_empty()).sequence, 9, "lost");
    source.trade.lock().unwrap().clear();
    assert_eq!(frame_with(&|w| w.trade_link_down.is_empty()).sequence, 9, "back");

    *source.price.lock().unwrap() = vec![lost_feed("binance")];
    assert_eq!(frame_with(&|w| !w.price_link_down.is_empty()).sequence, 9, "a price lane too");

    // A venue path too: the lease worker writes it off its own ask, so no fold moves when it goes down or comes back.
    *source.path.lock().unwrap() = vec![vike_tradehub_client::wire::VenueLinkDownWire {
        route_key: "binance".into(),
        why: "closed mid-session".into(),
        failed_restarts: 0,
        retry_in_ms: 0,
        effect: String::new(),
        no_adapter: false,
    }];
    assert_eq!(frame_with(&|w| !w.venue_link_down.is_empty()).sequence, 9, "a venue path lost");
    source.path.lock().unwrap().clear();
    assert_eq!(frame_with(&|w| w.venue_link_down.is_empty()).sequence, 9, "a venue path back");
    publisher.shutdown();
}

/// **A link that comes back right after a frame that said it was down still reaches a subscriber.** The mark is of what
/// the last frame SAID: compared with the last LOOK instead, a frame framed for another reason (a fold, a NOT RUNNING
/// change) between two looks carried the loss, the link came back before the next look, the look saw no move, and the
/// subscriber held "down" until the core's next publish, which a dead datahub stops.
#[test]
fn a_link_that_comes_back_right_after_a_frame_for_another_reason_still_reframes() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.sequence = 9;
    snap.portfolio.venues = vec![vike_execution::VenueBlock {
        venue: "binance".into(),
        route_key: "binance".into(),
        ..Default::default()
    }];
    let cell = Arc::new(ArcSwap::from_pointee(snap));
    let source = Arc::new(Links::default());
    let publisher = spawn(Arc::clone(&cell), None).with_symbol_sources(
        crate::server::symbol_admission::SymbolSources {
            readiness: Some(source.clone()),
            link_health: Some(source.clone()),
            ..Default::default()
        },
    );
    let subscription = publisher.subscribe();
    let frame_with = |seen: &dyn Fn(&CoreSnapshotWire) -> bool| -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let PublisherMailboxReceive::Frame(bytes) =
                subscription.recv_timeout(Duration::from_millis(100))
                && let Ok(Response::SnapshotFrame(w)) =
                    read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.to_vec()))
                && seen(&w)
            {
                return true;
            }
        }
        false
    };
    assert!(frame_with(&|w| w.trade_link_down.is_empty()), "the first frame");
    // Several rounds, because a look of the mark can fall between the loss and the frame, which is the case that
    // always worked: a look comes every quarter of a second, the frame within a poll.
    for round in 0..5 {
        *source.trade.lock().unwrap() = vec![lost_link("binance")];
        publisher.set_key_changed(Vec::new());
        assert!(frame_with(&|w| !w.trade_link_down.is_empty()), "round {round}: the loss");
        source.trade.lock().unwrap().clear();
        assert!(frame_with(&|w| w.trade_link_down.is_empty()), "round {round}: it came back");
    }
    publisher.shutdown();
}

/// **A quiet node's pushed frame does not stand still at what the first frame of an outage said**: while a link stays
/// down the age moves the mark every step, so a subscriber's `down_ms` stays about as old as one step.
#[test]
fn a_link_that_stays_down_moves_the_mark_in_steps_of_its_age() {
    let source = Arc::new(Links::default());
    let facts = NodeReadings::default();
    assert!(facts.readiness.set(source.clone()).is_ok());
    assert!(facts.link_health.set(source.clone()).is_ok());
    let at = |down_ms: u64| {
        *source.trade.lock().unwrap() =
            vec![vike_tradehub_client::wire::TradeLinkDownWire { down_ms, ..lost_link("binance") }];
        facts.link_mark()
    };
    assert_eq!(at(10), at(LINK_AGE_STEP_MS - 1), "within a step: no new frame");
    assert_ne!(at(10), at(LINK_AGE_STEP_MS), "the next step: a new frame");
}

/// **A held divergence crosses the observe wire with its kind as the variant name, byte for byte** (B1.03:
/// the view holds the typed `ReconcileDivergenceKind`, the wire row its word, the text the row carried when the
/// view held `format!("{:?}")`).
#[test]
fn a_held_divergence_projects_its_kind_word_byte_for_byte() {
    let row = ReconcileAlertView {
        id: 7,
        kind: vike_model::ReconcileDivergenceKind::PositionDrift,
        detail: "long 2".to_string(),
        proposed_event_count: 1,
        account: "bybit#DEMO".to_string(),
        fill_symbol: None,
        fill_qty: 0.0,
    };
    assert_eq!(
        serde_json::to_string(&project_reconcile_alert(&row)).expect("the row encodes"),
        r#"{"id":7,"account":"bybit#DEMO","kind":"PositionDrift","detail":"long 2","proposed_event_count":1}"#
    );
}

/// **Pinned on the code from before the hub links were published: a healthy frame does not move by a byte.** A
/// running demo account with its lease up, an `adapter failed` row with no cause noted and another NOT RUNNING row:
/// no new key on any of them. The publisher's own clock and the core's counters block are the two things normalized
/// (the frame's length prefix is checked against the body it heads).
#[test]
fn pinned_a_healthy_frame_is_byte_identical() {
    use crate::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry;
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.sequence = 7;
    snap.portfolio.venues = vec![vike_execution::VenueBlock {
        venue: "binance".into(),
        route_key: "binance".into(),
        tier: Some(vike_model::AccountTier::Demo),
        balance_mode: vike_execution::BalanceMode::Authoritative,
        ..Default::default()
    }];
    let stopped = |venue: &str, reason: &str| ArmingNotRunningView {
        account: default_account_of(venue),
        tier: Some(vike_model::AccountTier::Demo),
        reason: reason.into(),
        block: None,
        refusal_id: None,
    };
    let not_running = [stopped("ibkr", "adapter failed"), stopped("oanda", "account inactive")];
    let facts = NodeReadings::default();
    let source = Arc::new(TradeLeaseRegistry::default());
    assert!(facts.readiness.set(source.clone()).is_ok());
    assert!(facts.link_health.set(source).is_ok());

    let bytes = frame_snapshot(&snap, None, &not_running, &[], &facts, BodyCodec::Json);
    let (prefix, body) = bytes.split_at(4);
    assert_eq!(
        u32::from_be_bytes(prefix.try_into().expect("four bytes")) as usize,
        body.len(),
        "the prefix heads the body"
    );
    let text = std::str::from_utf8(body).expect("JSON");
    let (head, tail) = text.split_once("\"uptime_ms\":").expect("the uptime is stamped");
    let rest = tail.trim_start_matches(|c: char| c.is_ascii_digit());
    // The three additions since the pin was taken: when it was framed, a clock reading, after it the core's health
    // counters (plan S14, the owner's wire change of 2026-11-23), all zero here, and the failed alert deliveries (D15),
    // the last key, 0 from a publisher with no alert mount.
    let (rest, stamp) = rest.split_once(",\"published_ms\":").expect("the frame is stamped");
    let (stamp, counters) = stamp.split_once(",\"counters\":").expect("the counters are published");
    assert!(stamp.parse::<i64>().is_ok_and(|ms| ms > 0), "{stamp}");
    let zero = serde_json::to_string(&CoreCountersWire::default()).expect("the block encodes");
    assert_eq!(
        counters,
        format!("{zero},\"alert_deliveries_failed\":0}}}}"),
        "the failed alert deliveries close the frame"
    );
    assert_eq!(format!("{head}\"uptime_ms\":0{rest}}}}}"), PINNED_HEALTHY_FRAME);
}

const PINNED_HEALTHY_FRAME: &str = "{\"SnapshotFrame\":{\"sequence\":7,\"venue\":\"binance\",\"symbol\":\"BTCUSDT\",\
    \"trading_state\":\"Active\",\"balance\":0.0,\"equity_total\":0.0,\"venues\":[{\"venue\":\"binance\",\
    \"balance\":0.0,\"realized_pnl\":0.0,\"fees_paid\":0.0,\"funding_paid\":0.0,\"equity\":0.0,\"unrealized\":0.0,\
    \"missing_prices\":0,\"margin_used\":0.0,\"free_buying_power\":0.0,\"trading_state\":\"Active\",\"positions\":[],\
    \"route_key\":\"binance\",\"tier\":\"demo\"}],\"orders\":[],\"positions\":[],\"held_exits\":[],\
    \"recent_events\":[],\"fault\":null,\"bars\":[],\"identity\":null,\"accounts_epoch\":0,\"not_running\":[{\
    \"venue\":\"ibkr\",\"route_key\":\"ibkr\",\"tier\":\"demo\",\"reason\":\"adapter failed\"},{\
    \"venue\":\"oanda\",\"route_key\":\"oanda\",\"tier\":\"demo\",\"reason\":\"account inactive\"}],\
    \"uptime_ms\":0}}";

/// The DEFAULT account of `venue`, as a NOT RUNNING row names it.
fn default_account_of(venue: &str) -> vike_model::accounts::account_keys::AccountNamed {
    vike_model::accounts::account_keys::AccountNamed::of(
        venue,
        vike_model::accounts::account_keys::AccountLabel::Default,
    )
}

/// **A labelled NOT RUNNING row keeps its wire bytes** (owner ruling 2026-11-12, variant 2B): the view names its
/// account through ONE typed value and computes the route key, and the row projected from it is the one the loose
/// `venue` / `account` / `route_key` fields projected, key for key: the label's text and the `venue#LABEL` key
/// (`pinned_a_healthy_frame_is_byte_identical` pins the default account's rows). An account this build cannot name
/// projects as it was received.
#[test]
fn pinned_a_labelled_not_running_row_is_byte_identical() {
    use vike_model::accounts::account_keys::{AccountLabel, AccountNamed};
    let row = |account| {
        serde_json::to_string(&project_not_running(&ArmingNotRunningView {
            account,
            tier: Some(vike_model::AccountTier::Demo),
            reason: "account inactive".into(),
            block: None,
            refusal_id: None,
        }))
        .expect("serializes")
    };
    let alt = AccountLabel::parse("ALT").expect("a label");
    assert_eq!(
        row(AccountNamed::of("binance", alt)),
        "{\"venue\":\"binance\",\"account\":\"ALT\",\"route_key\":\"binance#ALT\",\"tier\":\"demo\",\
         \"reason\":\"account inactive\"}"
    );
    assert_eq!(
        row(AccountNamed::from_wire("kalshi", Some("main"))),
        "{\"venue\":\"kalshi\",\"account\":\"main\",\"route_key\":\"kalshi#main\",\"tier\":\"demo\",\
         \"reason\":\"account inactive\"}"
    );
}

/// The frame over a BORROWED wire is byte for byte the frame of the owned `Response::SnapshotFrame`, in every form, so
/// keeping the wire for the observer stream moved no byte of any frame; and a publish's bytes in a form are that frame,
/// framed once.
#[test]
fn a_borrowed_frame_is_the_bytes_of_the_owned_one() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.sequence = 7;
    snap.recent_events = vec![Arc::from("FillEvent c-1 1@100")];
    let wire = project(&snap, None, &[]);
    let published = PublishedFrame::new(wire.clone(), 1);
    for codec in [BodyCodec::Json, BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
        let mut owned = Vec::new();
        write_reply_as(&mut owned, codec, &Response::SnapshotFrame(Box::new(wire.clone())))
            .expect("frames");
        assert_eq!(*frame_wire(&wire, codec), owned, "{codec:?}");
        assert_eq!(**published.bytes(codec), owned, "{codec:?}");
        assert!(Arc::ptr_eq(published.bytes(codec), published.bytes(codec)), "framed once");
    }
    // ...and the deflated msgpack frame a compressed subscriber of this build is written: that frame, deflated once.
    let deflated = published.deflated_msgpack();
    let decoded = vike_tradehub_client::observe_stream::decode_response(
        &read_frame_raw(&mut &deflated[..]).expect("one whole frame"),
        BodyCodec::Msgpack,
    );
    assert_eq!(decoded.expect("inflates"), Response::SnapshotFrame(Box::new(wire)));
    assert!(Arc::ptr_eq(deflated, published.deflated_msgpack()), "deflated once");
}

/// The event log numbers each NEW line of a ring once, by allocation: a ring that only rolled numbers nothing twice, two
/// equal lines are two, a ring that rolled past everything is all new, and `between` answers what it still holds.
#[test]
fn the_event_log_numbers_each_new_line_once() {
    let line = |text: &str| -> Arc<str> { Arc::from(text) };
    let (a, b, c, d) = (line("fill"), line("fill"), line("ack"), line("halt"));
    let mut log = EventLog::default();
    assert_eq!(log.note(&[Arc::clone(&a), Arc::clone(&b)]), 2, "two equal lines are two");
    assert_eq!(log.note(&[Arc::clone(&a), Arc::clone(&b)]), 2, "the same ring numbers nothing");
    assert_eq!(log.note(&[Arc::clone(&b), Arc::clone(&c)]), 3, "rolled by one, one new");
    assert_eq!(log.note(&[Arc::clone(&d)]), 4, "nothing of the last ring left: all of it is new");
    assert_eq!(log.between(0, 4), ["fill", "fill", "ack", "halt"]);
    assert_eq!(log.between(1, 3), ["fill", "ack"]);
    assert_eq!(log.between(3, 9), ["halt"], "past the end answers what there is");
}

/// The mailbox is bounded and drops the OLDEST frame when full — never the newest, never blocking.
#[test]
fn mailbox_drops_oldest_when_full() {
    let mb = PublisherMailbox::new(2);
    let f = |n: u8| Arc::new(vec![n]);
    mb.push(f(1));
    mb.push(f(2));
    mb.push(f(3)); // drops the oldest (1)
    match mb.recv_timeout(Duration::from_millis(0)) {
        PublisherMailboxReceive::Frame(b) => {
            assert_eq!(*b, vec![2], "oldest (1) was dropped, 2 is next")
        }
        _ => panic!("expected a frame"),
    }
    match mb.recv_timeout(Duration::from_millis(0)) {
        PublisherMailboxReceive::Frame(b) => assert_eq!(*b, vec![3]),
        _ => panic!("expected a frame"),
    }
    // empty now -> a zero timeout reports Timeout, not Closed.
    assert_matches!(mb.recv_timeout(Duration::from_millis(0)), PublisherMailboxReceive::Timeout);
    mb.close();
    assert_matches!(mb.recv_timeout(Duration::from_millis(0)), PublisherMailboxReceive::Closed);
}

// -----------------------------------------------------------------------------------------
// The bar lane (`project_bar_series`). ⚠ Until these landed, NOTHING in this workspace built a
// `CoreSnapshot` carrying bars and asserted what the projection emits: the only other
// `BarSeriesWire` sites are client-side hand-built round-trips, which never reach this filter.
// That is exactly how a structurally empty bar lane shipped with every test green.
// -----------------------------------------------------------------------------------------

fn series(closed: Vec<Bar>, forming: Option<Bar>) -> vike_execution::BarSeries {
    vike_execution::BarSeries { closed: Arc::new(closed), forming }
}

/// **The boot mounts' series are the core's rows' when the boot rows name none** (the daemon's own rows,
/// `wire_mount_rows`): read off the boot rows, a paper-only node's every order on its own mounted symbol was refused
/// `no price feed reaches`. A runtime mount is no boot series, a boot row that names its series keeps it, and a boot
/// row the core has not listed yet names none.
#[test]
fn the_boot_mount_series_are_read_off_the_cores_rows() {
    let boot = vec![boot_mount_row("polymarket__TOK__1m", "polymarket")];
    let runtime = mount_row("bybit", "ETHUSDT", "1m");
    let handle =
        publisher_of(vec![mount_row("polymarket", "TOK", "1m"), runtime], boot.clone(), None);
    assert_eq!(
        handle.boot_mount_series(),
        [("polymarket".to_string(), "TOK".to_string(), "1m".to_string())]
    );

    let stated = vike_tradehub_client::wire::MountViewWire {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        ..boot_mount_row("", "binance")
    };
    let handle = publisher_of(Vec::new(), vec![stated], None);
    assert_eq!(
        handle.boot_mount_series(),
        [("binance".to_string(), "BTCUSDT".to_string(), "1h".to_string())]
    );

    let handle = spawn_with_mounts(
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty("polymarket", "TOK"))),
        None,
        boot,
        Vec::new(),
    );
    assert!(handle.boot_mount_series().is_empty(), "a core that listed nothing names no series");
}

fn mount_row(venue: &str, symbol: &str, interval: &str) -> vike_execution::MountView {
    vike_execution::MountView {
        kind: vike_execution::MountViewKind::Mount,
        mount_id: format!("{venue}__{symbol}__{interval}"),
        venue: venue.into(),
        symbol: symbol.into(),
        interval: interval.into(),
        ready: true,
        position: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        notional: 0.0,
        budget: None,
        latched: false,
        params: None,
        currency: Default::default(),
        legs: None,
    }
}

/// ⚠ **THE LIVE CONFIGURATION, and the case nothing covered.** The daemon mounts bybit;
/// `vike_tradehub_core::assemble_runtime` makes its hardcoded binance paper engine the PRIMARY, so
/// `CoreSnapshot::venue` reads `"binance"` while the only series the core's bar cache holds is
/// the bybit feed's. The old projection compared the two and published `bars: []` — every
/// frame, forever, on a daemon that was otherwise healthy and publishing ~38 snapshots/second.
#[test]
fn a_bybit_mount_under_a_binance_primary_publishes_its_bars() {
    // `empty`'s arguments ARE the primary engine's (venue, symbol) — the untraded binance one.
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.mounts = vec![mount_row("bybit", "BTCUSDT", "1m")].into();
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(
            vec![flat_bar_unit_volume(1, 100.0), flat_bar_unit_volume(2, 101.0)],
            Some(flat_bar_unit_volume(3, 102.0)),
        ),
    );

    let wire = project(&snap, None, &[]);
    assert_eq!(wire.venue, "binance", "the primary-mirroring scalars are untouched by the fix");
    assert_eq!(
        wire.bars.len(),
        1,
        "the MOUNTED series must be published — this is the whole defect: a bybit-mounted, \
             binance-primary daemon published an empty bar lane"
    );
    let s = &wire.bars[0];
    assert_eq!(
        (s.venue.as_str(), s.symbol.as_str(), s.interval.as_str()),
        ("bybit", "BTCUSDT", "1m")
    );
    assert_eq!(s.closed.len(), 2);
    assert_eq!(
        s.forming.as_ref().expect("the forming candle rides along").c.to_bits(),
        102.0_f64.to_bits()
    );
}

/// A MULTI-mount daemon publishes EVERY series it holds, not one. This is the half that a
/// "filter to `mounts[0]`" repair would have left broken — it would have made the live box work
/// and kept the defect one mount later.
#[test]
fn every_mounted_series_is_published_not_just_one() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.mounts =
        vec![mount_row("bybit", "BTCUSDT", "1m"), mount_row("okx", "BTC-USDT", "5m")].into();
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(vec![flat_bar_unit_volume(1, 1.0)], None),
    );
    snap.bars.insert(
        ("okx".into(), "BTC-USDT".into(), "5m".into()),
        series(vec![flat_bar_unit_volume(1, 2.0)], None),
    );

    let wire = project(&snap, None, &[]);
    let mut got: Vec<(&str, &str)> =
        wire.bars.iter().map(|s| (s.venue.as_str(), s.symbol.as_str())).collect();
    got.sort_unstable();
    assert_eq!(got, vec![("bybit", "BTCUSDT"), ("okx", "BTC-USDT")]);
}

/// `mounts` ORDERS the frame, it never FILTERS it: a core with feeds and no strategy mount
/// still publishes its bars. A mount-gated projection would be the same empty chart wearing a
/// new argument.
#[test]
fn a_mount_less_core_still_publishes_its_bars() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(vec![flat_bar_unit_volume(1, 1.0)], None),
    );
    assert!(snap.mounts.is_empty(), "no mount rows at all");
    assert_eq!(project(&snap, None, &[]).bars.len(), 1);
}

/// The frame is bounded in BOTH dimensions, and a truncated one keeps the MOUNTED series. The
/// cap replaces the bounding the removed venue filter was doing by accident; the mounted-first
/// ordering is what makes a truncated frame still the useful one. Every unmounted series is
/// inserted FIRST here, so insertion order alone would have evicted the traded one.
#[test]
fn the_series_cap_bounds_the_frame_and_keeps_the_mounted_series_first() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    for i in 0..(MAX_BAR_SERIES + 4) {
        snap.bars.insert(
            ("binance".into(), format!("ALT{i}"), "1m".into()),
            series(vec![flat_bar_unit_volume(1, 1.0)], None),
        );
    }
    snap.mounts = vec![mount_row("bybit", "BTCUSDT", "1m")].into();
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(vec![flat_bar_unit_volume(1, 9.0)], None),
    );

    let wire = project(&snap, None, &[]);
    assert_eq!(wire.bars.len(), MAX_BAR_SERIES, "the frame carries at most the series cap");
    assert_eq!(
        (wire.bars[0].venue.as_str(), wire.bars[0].symbol.as_str()),
        ("bybit", "BTCUSDT"),
        "the mounted series sorts FIRST, so a capped frame still carries what is traded"
    );
}

/// Each series is still tail-sliced to [`CHART_BARS_CAP`] — the per-series bound is unchanged
/// by the filter's removal, and it keeps the TAIL (the recent candles), not the head.
#[test]
fn each_series_is_tail_sliced_to_the_chart_bars_cap() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let bars: Vec<Bar> =
        (0..(CHART_BARS_CAP as i64 + 10)).map(|i| flat_bar_unit_volume(i, i as f64)).collect();
    snap.bars.insert(("bybit".into(), "BTCUSDT".into(), "1m".into()), series(bars, None));

    let wire = project(&snap, None, &[]);
    assert_eq!(wire.bars[0].closed.len(), CHART_BARS_CAP);
    assert_eq!(wire.bars[0].closed[0].timestamp, 10, "the TAIL survives, not the head");
}

/// **The published block NAMES its account** — §6.2's snapshot mirror, and the successor to a
/// fence that stood here.
///
/// ⚠ **What this replaced, and why it is a restatement rather than a deletion.** Stage 1 gave
/// `vike_execution::VenueBlock` its `account` and `route_key`
/// (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §7.1)
/// and this projection forwarded NEITHER. A test called
/// `the_published_venue_block_carries_no_account_field_yet` pinned that absence, and its doc
/// said what would license the change: *"it fails the day somebody adds either field to
/// `VenueBlockWire` **without doing §6.2's consumer half**, which is exactly the shape of
/// change that would let a client believe it can address an account it cannot."*
///
/// §6.2's consumer half is done — `crates/vike-client/src/order_gate.rs`'s `mounted_keys` reads
/// `venues[].route_key` and its `judge` refuses an ambiguous venue before a
/// `preview_token` is minted — so the fence has been paid rather than climbed over, and the
/// assertion inverts. The fence FIRED on this change, which is the evidence it was worth
/// having: nothing else in the tree would have asked whether the consumer half existed.
#[test]
fn the_published_venue_block_names_its_account_and_route_key() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_execution::VenueBlock {
        venue: "binance".into(),
        account: vike_model::accounts::account_keys::AccountLabel::parse("ALT").ok(),
        route_key: "binance#ALT".into(),
        ..Default::default()
    }];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("the wire serializes");
    assert!(json.contains("\"account\":\"ALT\""), "the LABEL reaches the wire: {json}");
    assert!(json.contains("\"route_key\":\"binance#ALT\""), "…and the ROUTE KEY: {json}");
    assert!(json.contains("\"binance\""), "…and the venue is still published: {json}");
}

/// **The engine's conversion-rate table reaches the wire** (decision 0183, phase 1): one row per quote
/// currency with its rate, age and stale flag, and the currency the rates are in; an account with no symbol
/// quoted in another currency publishes neither key.
#[test]
fn the_published_venue_block_carries_the_conversion_rates() {
    let row = |currency_value: &str, rate: f64, as_of_ms: i64, stale: bool| {
        vike_execution::QuoteRateView {
            currency: vike_execution::Interned::from(currency_value),
            rate,
            as_of_ms,
            stale,
            held: stale,
        }
    };
    let mut snap = CoreSnapshot::empty("oanda", "EURUSD");
    snap.portfolio.venues = vec![vike_execution::VenueBlock {
        venue: "oanda".into(),
        route_key: "oanda".into(),
        quote_rates: vec![
            row("GBP", 1.35, 1_700_000_000_000, false),
            row("JPY", 1.0 / 150.0, 5, true),
        ],
        quote_rates_in: vike_execution::Interned::from("USD"),
        ..Default::default()
    }];
    let wire = project(&snap, None, &[]);
    let block = &wire.venues[0];
    assert_eq!(block.quote_rates_in, "USD");
    let got: Vec<(&str, f64, i64, bool)> = block
        .quote_rates
        .iter()
        .map(|r| (r.currency.as_str(), r.rate, r.as_of_ms, r.stale))
        .collect();
    assert_eq!(got, [("GBP", 1.35, 1_700_000_000_000, false), ("JPY", 1.0 / 150.0, 5, true)]);
    // a stale row that values an open position says so on the wire, a fresh one never does
    assert_eq!(block.quote_rates.iter().map(|r| r.held).collect::<Vec<_>>(), [false, true]);

    snap.portfolio.venues[0].quote_rates.clear();
    snap.portfolio.venues[0].quote_rates_in = vike_execution::Interned::default();
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("the wire serializes");
    assert!(!json.contains("quote_rates"), "an empty table writes no key: {json}");
}

/// ⚠ **The DEFAULT account publishes no `account` key and a route key equal to its venue** —
/// which is every node in production today, and the property that makes the two new fields
/// invisible to every existing consumer. Without this, the test above is satisfied by a
/// projection that stamps a label on blocks that have none.
#[test]
fn a_default_account_publishes_no_label_and_a_bare_route_key() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_execution::VenueBlock {
        venue: "binance".into(),
        account: None,
        route_key: "binance".into(),
        ..Default::default()
    }];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("the wire serializes");
    assert!(!json.contains("\"account\""), "no label key at all for the default: {json}");
    assert!(json.contains("\"route_key\":\"binance\""), "the key IS the venue: {json}");
}

/// ⚠ **THE MEASUREMENT the account gate is built on, pinned rather than argued.**
/// [`PublisherHandle::engine_venues`] answers `["binance", "binance"]` on a two-account node —
/// evidence that cannot tell two books apart, and correctly so for the question it answers —
/// while [`PublisherHandle::engine_route_keys`] answers `["binance", "binance#ALT"]` and can.
/// `crate::server::refusal::account_refusal` reads the second; `crate::server::refusal::venue_refusal` keeps
/// reading the first, because re-pointing it would change what an existing refusal MEANS.
///
/// The third assertion is the one a reader should not skip: both rosters are EMPTY on a cell
/// that has not published, and empty TOGETHER — which is what makes the account gate's
/// inherited "empty means UNKNOWN, never refuse" rule structural rather than a coincidence of
/// two separate reads.
#[test]
fn the_two_rosters_read_the_same_cell_and_differ_only_where_the_account_lives() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_execution::VenueBlock {
            venue: "binance".into(),
            account: None,
            route_key: "binance".into(),
            ..Default::default()
        },
        vike_execution::VenueBlock {
            venue: "binance".into(),
            account: vike_model::accounts::account_keys::AccountLabel::parse("ALT").ok(),
            route_key: "binance#ALT".into(),
            ..Default::default()
        },
    ];
    let cell = Arc::new(ArcSwap::from_pointee(snap));
    let handle = spawn(Arc::clone(&cell), None);
    assert_eq!(
        handle.engine_venues(),
        vec!["binance".to_string(), "binance".to_string()],
        "one entry per ENGINE, and the two are the SAME STRING — the defect, not a bug in this \
             accessor"
    );
    assert_eq!(
        handle.engine_route_keys(),
        vec!["binance".to_string(), "binance#ALT".to_string()],
        "…and the route keys tell them apart, which is the whole of what the gate needs"
    );
    // The third projection, the bracket gate's: the same blocks, in the same order, so its engine
    // is the one the route-key roster names.
    assert_eq!(
        handle.engine_blocks().iter().map(|b| b.route_key.as_str()).collect::<Vec<_>>(),
        vec!["binance", "binance#ALT"],
        "the engine blocks are the cell's own, one per engine"
    );

    // The pre-publish shape: a cell holding no venue block answers EMPTY on ALL THREE, which the
    // two gates read as UNKNOWN and the bracket's reads as nothing to judge.
    let bare = Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty("binance", "BTCUSDT")));
    let quiet = spawn(Arc::clone(&bare), None);
    assert!(quiet.engine_venues().is_empty(), "no venue roster before the first publish");
    assert!(quiet.engine_route_keys().is_empty(), "…and no route-key roster either");
    assert!(quiet.engine_blocks().is_empty(), "…and no engine block either");

    handle.shutdown();
    quiet.shutdown();
}

/// **The order a `Modify` names, and only that one.** `orders_named_by` answers the OPEN order
/// of the order client id a `Modify` carries — the ceiling's only way to its instrument — and nothing for a
/// terminal order, an unknown order client id, or any verb that names no order to resolve (a `Cancel` names a
/// order client id too, but the ceiling never sizes it, so this reads it as nothing). It reads the same cell
/// the rosters do.
///
/// ⚠ KILL PROOF: drop the `is_terminal` test in `named_orders`' `Modify` arm and the `FILLED` row
/// answers one order; drop the `Modify` match and the `Cancel` row answers one.
#[test]
fn orders_named_by_answers_the_open_order_a_modify_names_and_nothing_else() {
    use vike_model::OrderStatus;
    use vike_tradehub_client::wire::CommandWire;
    let order = |order_client_id: &str, status: OrderStatus| vike_execution::OrderView {
        data_delay: None,
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    let modify = |order_client_id: &str| CommandWire::Modify {
        order_client_id: order_client_id.into(),
        new_qty: Some(1.0),
        new_price: Some(1.0),
        new_trigger_price: None,
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.orders = vec![
        std::sync::Arc::new(order("open", OrderStatus::Accepted)),
        std::sync::Arc::new(order("part", OrderStatus::PartiallyFilled)),
        std::sync::Arc::new(order("done", OrderStatus::Filled)),
        std::sync::Arc::new(order("quit", OrderStatus::Canceled)),
    ];
    let cell = Arc::new(ArcSwap::from_pointee(snap.clone()));
    let handle = spawn(Arc::clone(&cell), None);

    let named = |cmd: &CommandWire| -> Vec<String> {
        handle.orders_named_by(cmd).into_iter().map(|o| o.order_client_id).collect()
    };
    assert_eq!(named(&modify("open")), vec!["open".to_string()]);
    assert_eq!(named(&modify("part")), vec!["part".to_string()], "partially filled is still open");
    assert!(named(&modify("done")).is_empty(), "a FILLED order is terminal: nothing to resolve");
    assert!(
        named(&modify("never-published")).is_empty(),
        "an unknown order client id resolves to nothing"
    );
    assert!(
        named(&CommandWire::Cancel("open".into())).is_empty(),
        "only a Modify and a Submit ask for an order to be resolved"
    );

    // A `Submit` is the other verb that carries an id of its own, and it is asked in ANY status:
    // a terminal order is exactly the case `order_client_id_reuse_refusal` exists for.
    let submit = |order_client_id: &str| {
        CommandWire::Submit(vike_tradehub_client::wire::OrderRequestWire {
            order_client_id: order_client_id.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: None,
            time_in_force: None,
            gtd_expiry: None,
            display_quantity: None,
            release: None,
            trail: None,
            limit_offset: None,
            peg: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
        })
    };
    assert_eq!(named(&submit("open")), vec!["open".to_string()]);
    assert_eq!(
        named(&submit("done")),
        vec!["done".to_string()],
        "a FILLED order still owns its id"
    );
    assert!(named(&submit("never-published")).is_empty(), "a free id names nothing");

    // The core's list lets an ended order go an hour after it ended (`vike_core::snapshot::ENDED_ORDER_PUBLISH_MS`);
    // the publisher kept its id and status when it saw it ended, so the id is still that order's at the door, refused
    // in the words of HOW it ended. ⚠ KILL PROOF: drop the ended lookup of `PublisherHandle::orders_named_by` and the
    // reuse passes the door.
    let deadline = Instant::now() + Duration::from_secs(20);
    while handle.shared.facts.ended.lock().unwrap_or_else(PoisonError::into_inner).len() < 2 {
        assert!(Instant::now() < deadline, "the publisher never saw the orders end");
        std::thread::sleep(Duration::from_millis(5));
    }
    snap.sequence += 1;
    snap.orders.retain(|o| !o.status.is_terminal());
    cell.store(Arc::new(snap));
    for (id, status, words) in
        [("done", OrderStatus::Filled, "FILLED"), ("quit", OrderStatus::Canceled, "CANCELED")]
    {
        let reused = handle.orders_named_by(&submit(id));
        assert_eq!(reused.len(), 1, "{id}: the order the list let go still owns its id");
        assert_eq!((reused[0].order_client_id.as_str(), reused[0].status), (id, status));
        let refusal = crate::server::refusal::order_client_id_reuse_refusal(
            &submit(id),
            &reused,
            &Default::default(),
        )
        .expect("refused at the door")
        .text;
        assert!(
            refusal.contains(&format!(
                "order client id {id} is already used by an order that is {words}"
            )),
            "{refusal}"
        );
        assert!(named(&modify(id)).is_empty(), "{id}: a Modify of it still names nothing");
    }
    assert!(named(&submit("never-published")).is_empty(), "a free id still names nothing");

    handle.shutdown();
}

/// **The orders a give-back names**: every order in the typed symbol that has not ended (case ignored, spaces
/// trimmed), of ANY account, and nothing for an ended order, another symbol or a symbol nobody holds. Until the
/// `UnadmitSymbol` arm the door's give-back gate (`plan_unadmit`) was handed no orders by the session, so it could
/// not refuse a symbol with an order resting in it (the v0.1.97 live finding).
///
/// ⚠ KILL PROOF: drop the `UnadmitSymbol` arm of `named_orders` and the first two rows answer nothing.
#[test]
fn orders_named_by_answers_the_unended_orders_in_the_symbol_a_give_back_names() {
    use vike_model::OrderStatus;
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_tradehub_client::wire::CommandWire;
    let order =
        |order_client_id: &str, symbol: &str, account: Option<&str>, status: OrderStatus| {
            vike_execution::OrderView {
                data_delay: None,
                order_client_id: order_client_id.into(),
                venue: "bybit".into(),
                account: account.map(|a| AccountLabel::parse(a).expect("a legal label")),
                symbol: symbol.into(),
                side: 1,
                qty: 1.0,
                order_type: vike_model::OrderType::Limit,
                price: Some(1.0),
                trigger_price: None,
                status,
                venue_order_id: None,
                filled_qty: 0.0,
                average_fill_price: 0.0,
                refusal_id: None,
                reason: None,
                pending_price: None,
                trail: None,
                limit_offset: None,
                attributes: None,
                trail_percent: None,
                adjust: None,
                release: None,
                display_quantity: None,
            }
        };
    let mut snap = CoreSnapshot::empty("bybit", "BTCUSDT");
    snap.orders = vec![
        std::sync::Arc::new(order("default-rests", "ETHUSDT", None, OrderStatus::Accepted)),
        std::sync::Arc::new(order(
            "labelled-rests",
            "ETHUSDT",
            Some("DEMO"),
            OrderStatus::Accepted,
        )),
        std::sync::Arc::new(order("conditional", "ethusdt", Some("DEMO"), OrderStatus::Emulated)),
        std::sync::Arc::new(order("liquidated", "ETHUSDT", None, OrderStatus::Liquidated)),
        std::sync::Arc::new(order("done", "ETHUSDT", Some("DEMO"), OrderStatus::Filled)),
        std::sync::Arc::new(order("elsewhere", "SOLUSDT", Some("DEMO"), OrderStatus::Accepted)),
    ];
    let handle = spawn(Arc::new(ArcSwap::from_pointee(snap)), None);
    let give_back = |symbol: &str| CommandWire::UnadmitSymbol {
        venue: "bybit".into(),
        account: Some("DEMO".into()),
        symbol: symbol.into(),
    };
    let named = |cmd: &CommandWire| -> Vec<String> {
        handle.orders_named_by(cmd).into_iter().map(|o| o.order_client_id).collect()
    };
    assert_eq!(
        named(&give_back(" ethusdt ")),
        ["default-rests", "labelled-rests", "conditional", "liquidated"],
        "every order of the symbol that has not ended, of any account: the gate keeps the account's by route key"
    );
    assert_eq!(named(&give_back("SOLUSDT")), ["elsewhere"]);
    assert!(named(&give_back("XRPUSDT")).is_empty(), "a symbol nobody holds names nothing");
    handle.shutdown();
}

/// The projection publishes the engine's symbols (primary first) and its tier, as its word.
#[test]
fn the_published_venue_block_names_its_symbols_and_tier() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_execution::VenueBlock {
        venue: "binance".into(),
        route_key: "binance".into(),
        symbol: "BTCUSDT".into(),
        extra_symbols: vec!["ETHUSDT".to_string()].into(),
        tier: Some(vike_model::AccountTier::Demo),
        ..Default::default()
    }];
    let wire = project(&snap, None, &[]);
    assert_eq!(wire.venues[0].symbols, vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
    assert_eq!(wire.venues[0].tier.as_deref(), Some("demo"));
}

/// ⚠ **Each of the three tiers reaches the wire as ITSELF**, in its one spelling
/// (`AccountTier::as_str`). The test above names one, so a swapped pair (a LIVE engine published as
/// demo) would pass it. A GUI states this word to a person who is about to send an order.
#[test]
fn every_engine_tier_is_published_as_itself() {
    for tier in vike_model::AccountTier::ALL {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![vike_execution::VenueBlock {
            venue: "binance".into(),
            route_key: "binance".into(),
            symbol: "BTCUSDT".into(),
            tier: Some(tier),
            ..Default::default()
        }];
        let wire = project(&snap, None, &[]);
        assert_eq!(wire.venues[0].tier.as_deref(), Some(tier.as_str()), "{tier:?}");
    }
}

/// Decision 0156: the accounts the node does NOT run ride every frame, each with its route key,
/// its row's tier WORD, the reason and the block's KEY, so a GUI says why one is not running instead of
/// drawing it as anything. The list is the publisher's (`vike_tradehub_core::RuntimeAssembly::not_running`), not the
/// core's: the core publishes none. An empty list writes no key, so a node with every account
/// running publishes the bytes it always did.
#[test]
fn the_accounts_the_node_does_not_run_ride_the_frame_with_their_reason() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!json.contains("\"not_running\""), "CONTROL: none, no key: {json}");
    let sub = vike_model::accounts::account_keys::AccountLabel::parse("SUB").expect("a label");
    let not_running = [
        vike_execution::ArmingNotRunningView {
            account: default_account_of("okx"),
            tier: Some(vike_model::AccountTier::Live),
            reason: "no credentials".into(),
            block: Some(vike_model::ArmingBlock::NoCredentials),
            refusal_id: None,
        },
        vike_execution::ArmingNotRunningView {
            account: vike_model::accounts::account_keys::AccountNamed::of("binance", sub),
            tier: None,
            reason: "no account row".into(),
            block: Some(vike_model::ArmingBlock::NoAccountRow),
            refusal_id: None,
        },
    ];
    let wire = project(&snap, None, &not_running);
    let seen: Vec<_> = wire
        .not_running
        .iter()
        .map(|n| {
            (
                n.venue.as_str(),
                n.account.as_deref(),
                n.route_key.as_str(),
                n.tier.as_deref(),
                n.reason.as_str(),
                n.block_word.map(vike_model::ArmingBlock::word),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("okx", None, "okx", Some("live"), "no credentials", Some("no_credentials")),
            ("binance", Some("SUB"), "binance#SUB", None, "no account row", Some("no_account_row")),
        ]
    );
    let json = serde_json::to_string(&wire).expect("serializes");
    assert!(json.contains(r#""block_word":"no_credentials""#), "the block's key: {json}");
    // A row no block stopped (a restart at another tier) writes no key, as an older node's row.
    let restarting = [vike_execution::ArmingNotRunningView {
        reason: "tier changed, restarting at `live`".into(),
        block: None,
        ..not_running[0].clone()
    }];
    let json = serde_json::to_string(&project(&snap, None, &restarting)).expect("serializes");
    assert!(!json.contains("block_word"), "no block, no key: {json}");
}

/// ⚠ **"Not said" stays "not said" on the wire.** A block that names no symbol and no tier (an
/// engine the snapshot could not describe) publishes an EMPTY list and no tier: never "trades
/// nothing", never a default of `Live`. Neither key reaches the frame either, so an older client
/// reads exactly the bytes it always read. `VenueBlock::trades` answers `false` for such a block,
/// which is the reader's trap: it has to test for the empty list first.
///
/// The second block pins the one case the wire cannot express: extras with NO primary. They go
/// with the missing primary, because a list whose first entry is not the primary would be read as
/// one.
#[test]
fn a_block_that_names_no_symbol_or_tier_publishes_neither() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_execution::VenueBlock {
            venue: "binance".into(),
            route_key: "binance".into(),
            ..Default::default()
        },
        vike_execution::VenueBlock {
            venue: "okx".into(),
            route_key: "okx".into(),
            extra_symbols: vec!["ETH-USDT".to_string()].into(),
            ..Default::default()
        },
    ];
    let wire = project(&snap, None, &[]);
    // Both blocks are published, or the loop below would pass over nothing.
    assert_eq!(wire.venues.len(), 2);
    for block in &wire.venues {
        assert!(block.symbols.is_empty(), "{}: {:?}", block.venue, block.symbols);
        assert_eq!(block.tier, None, "{}", block.venue);
    }
    let json = serde_json::to_string(&wire).expect("serializes");
    assert!(!json.contains("\"symbols\""), "{json}");
    assert!(!json.contains("\"tier\""), "{json}");
}

/// An order on a labelled account publishes its label, and a default-account order publishes no
/// key at all (its bytes are what they always were).
#[test]
fn a_published_order_names_its_account_only_when_labelled() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let order = |account| vike_execution::OrderView {
        data_delay: None,
        order_client_id: "c-1".into(),
        venue: "binance".into(),
        account,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(99.0),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    snap.orders = vec![std::sync::Arc::new(order(
        vike_model::accounts::account_keys::AccountLabel::parse("SUB").ok(),
    ))];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(json.contains("\"account\":\"SUB\""), "{json}");
    snap.orders = vec![std::sync::Arc::new(order(None))];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!json.contains("\"account\""), "{json}");
}

/// Decision 0140: an order's recorded delay label rides the wire as its WORD, and an unlabelled
/// order publishes no key at all (an old reader's bytes, and the golden below, are unchanged).
#[test]
fn a_published_order_carries_its_delay_label_as_a_word_and_an_unlabelled_one_no_key() {
    let mut snap = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
    let order = |data_delay| vike_execution::OrderView {
        data_delay,
        order_client_id: "c-1".into(),
        venue: "ibkr".into(),
        account: None,
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(190.0),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    for (label, word) in [
        (vike_model::StreamDelay::DELAYED, "delayed"),
        (vike_model::StreamDelay::Frozen, "frozen"),
        (vike_model::StreamDelay::RealTime, "real-time"),
    ] {
        snap.orders = vec![std::sync::Arc::new(order(Some(label)))];
        let wire = project(&snap, None, &[]);
        assert_eq!(wire.orders[0].data_delay.as_deref(), Some(word));
        let json = serde_json::to_string(&wire).expect("serializes");
        assert!(json.contains(&format!("\"data_delay\":\"{word}\"")), "{json}");
    }
    snap.orders = vec![std::sync::Arc::new(order(None))];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!json.contains("data_delay"), "{json}");
}

/// A resting trailing order publishes its trail and limit offset (its activation is its trigger price); any other
/// kind publishes neither key, a `trail` it carries for a backtest included, so its bytes are the frame from before.
#[test]
fn a_published_trailing_order_carries_its_trail_and_offset_and_no_other_kind_does() {
    let mut snap = CoreSnapshot::empty("okx", "BTC-USDT-SWAP");
    let order = |order_type| vike_execution::OrderView {
        data_delay: None,
        order_client_id: "c-1".into(),
        venue: "okx".into(),
        account: None,
        symbol: "BTC-USDT-SWAP".into(),
        side: -1,
        qty: 1.0,
        order_type,
        price: None,
        trigger_price: Some(70_000.0),
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: Some(50.0),
        limit_offset: Some(5.0),
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    snap.orders = vec![std::sync::Arc::new(order(vike_model::OrderType::TrailingLimit))];
    let wire = project(&snap, None, &[]);
    let o = &wire.orders[0];
    assert_eq!((o.trail, o.limit_offset, o.trigger_price), (Some(50.0), Some(5.0), Some(70_000.0)));
    snap.orders = vec![std::sync::Arc::new(order(vike_model::OrderType::Stop))];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!json.contains("\"trail\"") && !json.contains("\"limit_offset\""), "{json}");
}

// -----------------------------------------------------------------------------------------
// The wire golden. ⚠ `wire_tests.rs` (vike-tradehub-client) only ROUND-TRIPS a hand-built
// `CoreSnapshotWire`, and the tests above assert one field at a time, so until this one NOTHING pinned
// the BYTES the projection emits for a fully populated `CoreSnapshot`. It stands in front of the
// read-model move (`CoreSnapshot` and its view types leaving vike-core): that move must not change
// one byte of what a node publishes, and a re-spelled field, a reordered struct or a leaked
// core-only field all show here as a diff of this literal.
// -----------------------------------------------------------------------------------------

/// A bar whose five numbers differ, so a swapped `o`/`h`/`l`/`c`/`v` mapping cannot hide.
fn ohlcv(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
    Bar { open: o, high: h, low: l, close: c, volume: v, ..flat_bar_unit_volume(ts, 0.0) }
}

/// A snapshot with EVERY projected field populated, each `Option` set where the wire carries it,
/// and the core-only fields (`marks`, `recon`, margin mode, mark source, ...) set to non-default
/// values so a projection that started leaking one would change the bytes.
fn fully_populated_snapshot() -> CoreSnapshot {
    use vike_execution::{MountView, MountViewKind};
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_model::events::PositionSide;

    let position =
        |symbol: &str, side: PositionSide, size: f64, avg_px: f64| vike_execution::PositionView {
            venue: "binance".into(),
            symbol: symbol.into(),
            position_side: side,
            size,
            average_price: avg_px,
            unrealized: size * 1.5,
            mark_source: Some(vike_execution::PriceSource::Mark),
            leverage: 5.0,
            liquidation_price: avg_px - 20.0,
            margin_mode: vike_model::MarginMode::Isolated,
            isolated_margin: Some(55.5),
        };
    let block = |account: Option<AccountLabel>, route_key: &str, balance: f64, positions| {
        vike_execution::VenueBlock {
            venue: "binance".into(),
            account,
            route_key: route_key.into(),
            symbol: "BTCUSDT".into(),
            extra_symbols: vec!["ETHUSDT".to_string()].into(),
            tier: Some(vike_model::AccountTier::Paper),
            balance,
            realized_pnl: 12.25,
            fees_paid: 1.5,
            funding_paid: -0.75,
            equity: balance + 37.5,
            unrealized: 37.5,
            missing_prices: 2,
            margin_used: 300.0,
            free_buying_power: balance - 300.0,
            margin_ratio: 0.03,
            trading_state: TradingState::Active,
            contract_value_default: vike_model::ContractValue::linear(2.0),
            positions,
            ..Default::default()
        }
    };
    let order = |order_client_id: &str, account, status, price, trigger, venue_order_id| {
        vike_execution::OrderView {
            data_delay: None,
            order_client_id: order_client_id.into(),
            venue: "binance".into(),
            account,
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 2.0,
            order_type: vike_model::OrderType::StopLimit,
            price,
            trigger_price: trigger,
            status,
            venue_order_id,
            filled_qty: 0.5,
            average_fill_price: 101.25,
            refusal_id: None,
            reason: None,
            pending_price: None,
            trail: None,
            limit_offset: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            release: None,
            display_quantity: None,
        }
    };
    let held = |order_client_id: &str, price, trigger, parent: Option<&str>| OrderHeldView {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 2.0,
        order_type: vike_model::OrderType::StopLimit,
        price,
        trigger_price: trigger,
        parent_order_id: parent.map(Into::into),
        account: None,
        release: None,
        waiting: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        display_quantity: None,
    };

    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.sequence = 4242;
    snap.trading_state = TradingState::Reducing;
    snap.balance = 9876.5;
    snap.portfolio.equity_total = 20123.25;
    let mut alt = block(
        AccountLabel::parse("ALT").ok(),
        "binance#ALT",
        5000.25,
        vec![position("BTCUSDT", PositionSide::Short, -1.25, 101.0)],
    );
    alt.tier = Some(vike_model::AccountTier::Live);
    alt.trading_state = TradingState::Halted;
    snap.portfolio.venues = vec![
        block(
            None,
            "binance",
            10000.5,
            vec![
                position("BTCUSDT", PositionSide::Both, 0.75, 100.5),
                position("ETHUSDT", PositionSide::Long, 3.0, 2500.0),
            ],
        ),
        alt,
    ];
    snap.positions = vec![
        position("BTCUSDT", PositionSide::Both, 0.75, 100.5),
        position("ETHUSDT", PositionSide::Long, 3.0, 2500.0),
    ];
    snap.marks = vec![("binance".into(), "BTCUSDT".into(), 101.5)];
    snap.orders = vec![
        std::sync::Arc::new(order(
            "c-1",
            AccountLabel::parse("SUB").ok(),
            vike_model::OrderStatus::PartiallyFilled,
            Some(99.5),
            Some(100.0),
            Some("v-77".into()),
        )),
        std::sync::Arc::new(order(
            "c-2",
            None,
            vike_model::OrderStatus::Canceled,
            None,
            None,
            None,
        )),
    ];
    snap.held_exits = vec![
        held("c-1-sl", Some(98.5), Some(99.0), Some("c-1")),
        held("c-1-tp", Some(110.0), None, None),
    ];
    // Two mounted series and one unmounted one, the unmounted one FIRST in the cache: the frame
    // must lead with the mounted pair, and the residual row (no venue, no symbol) mounts nothing.
    snap.bars.insert(
        ("okx".into(), "BTC-USDT".into(), "5m".into()),
        series(vec![ohlcv(60, 1.0, 2.0, 0.5, 1.5, 7.0)], None),
    );
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(
            vec![
                ohlcv(1, 100.0, 103.5, 99.25, 102.0, 12.5),
                ohlcv(2, 102.0, 104.0, 101.5, 103.25, 8.75),
            ],
            Some(ohlcv(3, 103.25, 105.0, 103.0, 104.5, 3.5)),
        ),
    );
    snap.bars.insert(
        ("binance".into(), "ETHUSDT".into(), "1h".into()),
        series(vec![ohlcv(3600, 2500.0, 2510.0, 2490.0, 2505.0, 410.0)], None),
    );
    let mount = |venue: &str, symbol: &str, interval: &str| MountView {
        budget: Some(vike_execution::MountBudget {
            max_loss: Some(50.0),
            max_notional: Some(1000.0),
            loss_action: vike_model::MountLossAction::Flatten,
            max_leverage: None,
        }),
        position: 0.75,
        realized_pnl: 4.5,
        unrealized_pnl: 1.25,
        notional: 75.0,
        latched: true,
        ..mount_row(venue, symbol, interval)
    };
    snap.mounts = vec![
        mount("bybit", "BTCUSDT", "1m"),
        mount("binance", "ETHUSDT", "1h"),
        MountView {
            kind: MountViewKind::Residual,
            mount_id: String::new(),
            ..mount_row("", "", "")
        },
    ]
    .into();
    snap.recent_events = vec!["fill c-1 0.5@101.25".into(), "cancel c-2".into()];
    snap.fault = Some("handler panicked: boom".into());
    snap.counters.market_updates_dropped_conflated = 9;
    snap.counters.commands_rejected_queue_full = 3;
    snap.reconcile.last_pass_timestamp = 1_700_000_000_000;
    snap.accounts_epoch = 0x0123_4567_89AB_CDEF;
    // Each holds the venue's figure, as a live account does, so it writes no `balance_is_own_book`.
    for block in &mut snap.portfolio.venues {
        block.balance_mode = vike_execution::BalanceMode::Authoritative;
    }
    snap
}

/// **The JSON a node publishes for a fully populated snapshot, byte for byte.** Two venue blocks
/// (one labelled `ALT`), positions, two orders (one with every `Option` set, one with none), two
/// held exits, a closed-plus-forming bar series with mounted-first series order, a fault, recent
/// events, an accounts epoch and an identity. A change to the wire schema, to a field's spelling or
/// order, or to what the projection copies changes this literal and must be reviewed as a
/// wire-contract change; a refactor that does not mean to change the wire leaves it untouched.
#[test]
fn a_fully_populated_snapshot_projects_to_this_exact_json() {
    let id = HubIdentityWire {
        name: "the build runner".into(),
        strategy: "spread_maker".into(),
        params: "qty=0.5".into(),
        live: true,
        build: "v0.1.51 abc1234".into(),
        advertise_addr: "<host>:7879".into(),
        priced_venues: None,
    };
    let json = serde_json::to_string(&project(&fully_populated_snapshot(), Some(&id), &[]))
        .expect("the wire serializes");
    assert_eq!(json, WIRE_SNAPSHOT_GOLDEN);
}

/// The literal `a_fully_populated_snapshot_projects_to_this_exact_json` compares against: what
/// `serde_json::to_string` printed for `fully_populated_snapshot` on the first run, read by eye
/// afterwards. The mount rows do not appear in it by design (`project` reads them only to put the
/// MOUNTED bar series first, which is why `bybit` and `binance` lead and `okx` trails), nor do
/// `marks`, `recon`, `margin_ratio` or `margin_mode`, which stay core-side; every position carries the
/// `mark_source` that priced it (the fixture's are all the mark).
const WIRE_SNAPSHOT_GOLDEN: &str = r##"{"sequence":4242,"venue":"binance","symbol":"BTCUSDT","trading_state":"Reducing","balance":9876.5,"equity_total":20123.25,"venues":[{"venue":"binance","balance":10000.5,"realized_pnl":12.25,"fees_paid":1.5,"funding_paid":-0.75,"equity":10038.0,"unrealized":37.5,"missing_prices":2,"margin_used":300.0,"free_buying_power":9700.5,"trading_state":"Active","positions":[{"venue":"binance","symbol":"BTCUSDT","position_side":"BOTH","size":0.75,"average_price":100.5,"unrealized":1.125,"leverage":5.0,"liquidation_price":80.5,"mark_source":"mark"},{"venue":"binance","symbol":"ETHUSDT","position_side":"LONG","size":3.0,"average_price":2500.0,"unrealized":4.5,"leverage":5.0,"liquidation_price":2480.0,"mark_source":"mark"}],"route_key":"binance","symbols":["BTCUSDT","ETHUSDT"],"tier":"paper"},{"venue":"binance","balance":5000.25,"realized_pnl":12.25,"fees_paid":1.5,"funding_paid":-0.75,"equity":5037.75,"unrealized":37.5,"missing_prices":2,"margin_used":300.0,"free_buying_power":4700.25,"trading_state":"Halted","positions":[{"venue":"binance","symbol":"BTCUSDT","position_side":"SHORT","size":-1.25,"average_price":101.0,"unrealized":-1.875,"leverage":5.0,"liquidation_price":81.0,"mark_source":"mark"}],"account":"ALT","route_key":"binance#ALT","symbols":["BTCUSDT","ETHUSDT"],"tier":"live"}],"orders":[{"order_client_id":"c-1","venue":"binance","account":"SUB","symbol":"BTCUSDT","side":-1,"qty":2.0,"order_type":"stop_limit","price":99.5,"trigger_price":100.0,"status":"PARTIALLY_FILLED","venue_order_id":"v-77","filled_qty":0.5,"average_fill_price":101.25},{"order_client_id":"c-2","venue":"binance","symbol":"BTCUSDT","side":-1,"qty":2.0,"order_type":"stop_limit","price":null,"trigger_price":null,"status":"CANCELED","venue_order_id":null,"filled_qty":0.5,"average_fill_price":101.25}],"positions":[{"venue":"binance","symbol":"BTCUSDT","position_side":"BOTH","size":0.75,"average_price":100.5,"unrealized":1.125,"leverage":5.0,"liquidation_price":80.5,"mark_source":"mark"},{"venue":"binance","symbol":"ETHUSDT","position_side":"LONG","size":3.0,"average_price":2500.0,"unrealized":4.5,"leverage":5.0,"liquidation_price":2480.0,"mark_source":"mark"}],"held_exits":[{"order_client_id":"c-1-sl","venue":"binance","symbol":"BTCUSDT","side":-1,"qty":2.0,"order_type":"stop_limit","price":98.5,"trigger_price":99.0,"parent_order_id":"c-1"},{"order_client_id":"c-1-tp","venue":"binance","symbol":"BTCUSDT","side":-1,"qty":2.0,"order_type":"stop_limit","price":110.0,"trigger_price":null,"parent_order_id":null}],"recent_events":["fill c-1 0.5@101.25","cancel c-2"],"fault":"handler panicked: boom","bars":[{"venue":"bybit","symbol":"BTCUSDT","interval":"1m","closed":[{"timestamp":1,"o":100.0,"h":103.5,"l":99.25,"c":102.0,"v":12.5},{"timestamp":2,"o":102.0,"h":104.0,"l":101.5,"c":103.25,"v":8.75}],"forming":{"timestamp":3,"o":103.25,"h":105.0,"l":103.0,"c":104.5,"v":3.5}},{"venue":"binance","symbol":"ETHUSDT","interval":"1h","closed":[{"timestamp":3600,"o":2500.0,"h":2510.0,"l":2490.0,"c":2505.0,"v":410.0}],"forming":null},{"venue":"okx","symbol":"BTC-USDT","interval":"5m","closed":[{"timestamp":60,"o":1.0,"h":2.0,"l":0.5,"c":1.5,"v":7.0}],"forming":null}],"identity":{"name":"the build runner","strategy":"spread_maker","params":"qty=0.5","live":true,"build":"v0.1.51 abc1234","advertise_addr":"<host>:7879"},"accounts_epoch":81985529216486895,"counters":{"market_updates_dropped_conflated":9,"commands_rejected_queue_full":3,"events_dropped_order_already_closed":0,"closing_events_dropped_order_still_live":0,"events_dropped_order_unknown":0,"events_dropped_number_not_finite":0,"restored_orders_adopted":0,"restored_orders_gone":0,"restored_orders_filled_while_down":0,"tags_repointed_order_still_live":0}}"##;

/// **The mount rows the `StrategyStatus` overlay reads carry the budget latch** (decision 0239): each REAL mount row
/// of the published cell hands out its `latched` flag, the residual row is left out, and the rows an un-latch (or an
/// order, `server/tests/paper_demo_login_door.rs`) is judged against are the same real ones, for those verbs alone.
#[test]
fn the_live_mount_rows_carry_the_budget_latch() {
    use vike_tradehub_client::wire::CommandWire;
    let snap = fully_populated_snapshot();
    let handle = spawn(Arc::new(ArcSwap::from_pointee(snap)), None);
    let live = handle.live_mount_params();
    assert_eq!(live.len(), 2, "the residual row is no mount: {live:?}");
    assert!(live.iter().all(|m| m.latched), "both fixture mounts are latched: {live:?}");
    assert_eq!(live[0].mount_id, "bybit__BTCUSDT__1m");

    let rows = handle
        .mounts_named_by(&CommandWire::UnlatchMount { mount_id: "bybit__BTCUSDT__1m".into() });
    assert_eq!(rows.len(), 2, "the real mount rows, for that verb");
    assert!(rows.iter().all(|m| m.latched && m.kind == vike_execution::MountViewKind::Mount));
    assert!(
        handle.mounts_named_by(&CommandWire::Cancel("c".into())).is_empty(),
        "any other verb clones nothing"
    );
    handle.shutdown();
}

/// **The frame says how a client mints an id for each account** (decision 0270): a block's account code and its
/// client shape cross as `VenueBlockWire::{account_code, order_client_id_shape}`, field for field; a block that says neither
/// writes neither key.
#[test]
fn the_frame_carries_each_accounts_code_and_id_shape() {
    use vike_model::orders::client_id::{OrderClientIdAccountCode, OrderClientIdShape};
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let told = vike_execution::VenueBlock {
        venue: "binance".into(),
        route_key: "binance".into(),
        account_code: Some(OrderClientIdAccountCode::of_id(41)),
        order_client_id_shape: Some(OrderClientIdShape {
            account: false,
            u_width: 12,
            max_gen: 9,
            tag: false,
        }),
        ..Default::default()
    };
    let untold = vike_execution::VenueBlock {
        venue: "bybit".into(),
        route_key: "bybit".into(),
        ..Default::default()
    };
    snap.portfolio.venues = vec![told, untold];
    let wire = project(&snap, None, &[]);
    assert_eq!(wire.venues[0].account_code.as_deref(), Some("15"));
    assert_eq!(
        wire.venues[0].order_client_id_shape,
        Some(vike_model::orders::client_id::OrderClientIdShape {
            account: false,
            u_width: 12,
            max_gen: 9,
            tag: false
        })
    );
    assert_eq!(
        (wire.venues[1].account_code.as_deref(), wire.venues[1].order_client_id_shape),
        (None, None)
    );
    let text = serde_json::to_string(&wire.venues[1]).expect("serialises");
    assert!(!text.contains("account_code") && !text.contains("order_client_id_shape"), "{text}");
}

/// **The mount rows carry each mount's strategy code** (decision 0270): a runtime mount's from the record the door
/// stamped (`write_live_mount`), as its four characters; a mount with none says none.
#[test]
fn a_runtime_mount_row_carries_its_strategy_code() {
    let spec = |cid: &str, code: Option<i64>| vike_execution::MountSpec {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        account: None,
        controller_id: Some(cid.into()),
        file: Some("buy_hold.rhai".into()),
        name: None,
        rhai: None,
        params: vike_model::params::Params::new(),
        order_client_id_code: code
            .and_then(vike_model::orders::client_id::OrderClientIdStrategyCode::new),
    };
    let records = [
        vike_core::mount_topology::MountRecord::stamped(spec("rt_coded", Some(363)), 1),
        vike_core::mount_topology::MountRecord::stamped(spec("rt_plain", None), 1),
    ];
    let live = |id: &str| MountLiveParams {
        mount_id: id.into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        typed_params: None,
        latched: false,
    };
    let blank = || vike_tradehub_client::wire::MountViewWire {
        order_client_id_code: None,
        refusal_id: None,
        live: false,
        strategy: String::new(),
        params: String::new(),
        venue: String::new(),
        symbol: String::new(),
        interval: String::new(),
        mount_id: String::new(),
        typed_params: None,
        asset_class: None,
        latched: false,
        route_key: String::new(),
        tier: None,
        not_running: false,
    };
    let mut row = blank();
    write_live_mount(&mut row, live("rt_coded"), &records);
    assert_eq!(row.order_client_id_code.as_deref(), Some("00A3"));
    let mut row = blank();
    write_live_mount(&mut row, live("rt_plain"), &records);
    assert_eq!(row.order_client_id_code, None);
}

/// A refused order's reason crosses the wire (`OrderViewWire::reason`), and an order without one
/// puts nothing there.
#[test]
fn project_carries_a_refused_orders_reason() {
    let mut snap = CoreSnapshot::empty("ctrader", "EURUSD");
    let order = |order_client_id: &str, reason: Option<&str>| OrderView {
        data_delay: None,
        order_client_id: order_client_id.into(),
        venue: "ctrader".into(),
        account: None,
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.1),
        trigger_price: None,
        status: vike_model::OrderStatus::Rejected,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: reason.map(str::to_owned),
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    snap.orders.push(std::sync::Arc::new(order(
        "c-1",
        Some("INVALID_REQUEST: more digits than symbol allows"),
    )));
    snap.orders.push(std::sync::Arc::new(order("c-2", None)));

    let wire = project(&snap, None, &[]);
    assert_eq!(
        wire.orders[0].reason.as_deref(),
        Some("INVALID_REQUEST: more digits than symbol allows")
    );
    assert_eq!(wire.orders[1].reason, None);
}

/// An account that stopped and started again has two blocks under one route key in the core's list (the list only
/// grows): the frame carries the newest, in its place, so a consumer that takes the first block of a key reads the
/// running engine and not the stopped one. A stopped account that did not start again keeps its block, and blocks
/// that name no route key are never merged.
#[test]
fn a_restarted_accounts_stale_block_is_not_published_beside_its_new_one() {
    let block = |venue: &str, key: &str, equity: f64| vike_execution::VenueBlock {
        venue: venue.into(),
        route_key: key.into(),
        equity,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        block("binance", "binance", 1.0),
        block("binance", "binance#ALT", 2.0), // stopped, then started again below
        block("okx", "okx", 3.0),             // stopped for good: still shown
        block("binance", "binance#ALT", 4.0),
        block("bybit", "", 5.0),
        block("bybit", "", 6.0),
    ];
    let wire = project(&snap, None, &[]);
    let seen: Vec<(String, f64)> =
        wire.venues.iter().map(|v| (v.route_key.clone(), v.equity)).collect();
    assert_eq!(
        seen,
        vec![
            ("binance".to_string(), 1.0),
            ("okx".to_string(), 3.0),
            ("binance#ALT".to_string(), 4.0),
            (String::new(), 5.0),
            (String::new(), 6.0),
        ]
    );
}

/// **The published `equity_total` is the sum of the rows beside it** (drill `n078-B1`, the CI box, 2026-10-19): the
/// core sums every engine it ever held, so one deactivate and activate of binance's demo account added the
/// stopped spot engine's wallet (4984.80) to the new engine's, and the REPL's total no longer matched its rows.
/// The frame sums the newest block of each route key, as its rows are; a stopped account that did not start
/// again keeps its block, and a node with nothing superseded publishes the core's own figure bit for bit.
#[test]
fn a_restarted_accounts_stopped_engine_is_not_added_to_the_published_total() {
    let block = |venue: &str, key: &str, equity: f64| vike_execution::VenueBlock {
        venue: venue.into(),
        route_key: key.into(),
        equity,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        block("binance", "binance", 4984.80324635), // stopped by the deactivate, then started again below
        block("okx", "okx", 4999.625219769523),
        block("binance", "binance", 4984.80324635),
        block("binance", "binance#PERP", 4996.00829587), // the lane twin, then stopped for good: still shown
    ];
    snap.portfolio.equity_total =
        vike_math::summation::compensated_sum(snap.portfolio.venues.iter().map(|v| v.equity));
    let wire = project(&snap, None, &[]);
    assert_eq!(wire.venues.len(), 3, "the stale block is not a row");
    let rows = vike_math::summation::compensated_sum(wire.venues.iter().map(|v| v.equity));
    assert_eq!(wire.equity_total.to_bits(), rows.to_bits(), "the total is the rows' sum");
    assert!(
        wire.equity_total < snap.portfolio.equity_total - 4984.0,
        "the stopped spot engine's wallet is not counted again"
    );

    // CONTROL: nothing superseded, so the core's figure goes out untouched (here deliberately not the rows' sum).
    snap.portfolio.venues.remove(0);
    snap.portfolio.equity_total = 1234.5;
    assert_eq!(project(&snap, None, &[]).equity_total.to_bits(), 1234.5_f64.to_bits());
}

/// ⚠ **The published `equity_total` is one currency, the primary's, also when a block was superseded**
/// (decision 0183, item 13): the frame's total re-sums the newest block of each key, and a block in another
/// currency is not in it. The block's group rides the wire as `ledger_currency`, EMPTY (and so absent from the
/// frame) for the dollar group.
#[test]
fn the_published_total_is_the_primary_currency_alone_and_each_block_names_its_group() {
    let block = |venue: &str, key: &str, currency: &str, equity: f64| vike_execution::VenueBlock {
        venue: venue.into(),
        route_key: key.into(),
        ledger_currency: currency.into(),
        equity,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        block("binance", "binance", "", 100.0), // stopped, started again below: a superseded block
        block("oanda", "oanda", "EUR", 7_000.0),
        block("binance", "binance", "", 110.0),
    ];
    snap.portfolio.equity_total = 100.0 + 110.0; // as `build` folds it: the primary group, every engine
    let wire = project(&snap, None, &[]);
    assert_eq!(wire.venues.len(), 2);
    assert_eq!(
        wire.equity_total.to_bits(),
        110.0_f64.to_bits(),
        "the dollar block alone, not +7000"
    );
    assert_eq!(wire.venues[0].ledger_currency, "EUR");
    assert_eq!(wire.venues[1].ledger_currency, "", "the dollar group is the absent key");
    let json = serde_json::to_string(&wire.venues[1]).expect("serializes");
    assert!(!json.contains("ledger_currency"), "{json}");

    // CONTROL: nothing superseded, so the core's own figure goes out untouched.
    snap.portfolio.venues.remove(0);
    snap.portfolio.equity_total = 110.0;
    assert_eq!(project(&snap, None, &[]).equity_total.to_bits(), 110.0_f64.to_bits());
}

/// **The block says when its ledger currency is only an assumed USD** (decision 0189, A52): the engine's
/// `VenueBlock::currency_assumed` rides the frame as `currency_assumed`, absent while false, per block (a stated account
/// beside an assumed one stays stated).
#[test]
fn a_block_whose_currency_is_only_assumed_says_so_on_the_wire_and_others_write_no_key() {
    let block = |venue: &str, assumed: bool| vike_execution::VenueBlock {
        venue: venue.into(),
        route_key: venue.into(),
        currency_assumed: assumed,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![block("oanda", true), block("binance", false)];
    let wire = project(&snap, None, &[]);
    assert!(wire.venues[0].currency_assumed && !wire.venues[1].currency_assumed);
    let on = serde_json::to_string(&wire.venues[0]).expect("serializes");
    let off = serde_json::to_string(&wire.venues[1]).expect("serializes");
    assert!(on.contains(r#""currency_assumed":true"#), "{on}");
    assert!(!off.contains("currency_assumed"), "{off}");
}

/// A block whose `balance` is the engine's own book says so on the wire, and one holding the venue's figure writes no key.
#[test]
fn a_block_that_keeps_its_own_book_says_so_on_the_wire_and_others_write_no_key() {
    let block = |venue: &str, mode| vike_execution::VenueBlock {
        venue: venue.into(),
        route_key: venue.into(),
        balance_mode: mode,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        block("deribit", vike_execution::BalanceMode::Delta),
        block("binance", vike_execution::BalanceMode::Authoritative),
    ];
    let wire = project(&snap, None, &[]);
    assert!(wire.venues[0].balance_is_own_book && !wire.venues[1].balance_is_own_book);
    let off = serde_json::to_string(&wire.venues[1]).expect("serializes");
    assert!(!off.contains("balance_is_own_book"), "{off}");
}

/// ⚠ **The frame STATES the primary group; a reader never derives it from the first block.** The node's
/// primary is the core's first engine, the stopped primary's tombstone included, but the frame carries only
/// the newest block of each account: `[binance USDT (primary), oanda EUR, bybit USDT]` after binance stopped
/// and started again publishes `[oanda, bybit, binance']`, whose first block is in EUR while `equity_total`
/// is the dollar group's. The dollar group is the absent key; a EUR primary is stated.
#[test]
fn the_frame_states_the_primary_group_after_a_primary_restart() {
    let block = |venue: &str, key: &str, currency: &str, equity: f64| vike_execution::VenueBlock {
        venue: venue.into(),
        route_key: key.into(),
        ledger_currency: currency.into(),
        equity,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        block("binance", "binance", "", 100.0), // the PRIMARY, stopped; its successor is last
        block("oanda", "oanda", "EUR", 7_000.0),
        block("bybit", "bybit", "", 50.0),
        block("binance", "binance", "", 110.0),
    ];
    snap.portfolio.equity_total = 100.0 + 50.0 + 110.0;
    let wire = project(&snap, None, &[]);
    assert_eq!(
        wire.venues.iter().map(|v| v.route_key.as_str()).collect::<Vec<_>>(),
        ["oanda", "bybit", "binance"],
        "PREMISE: the first block of the frame is the EUR one"
    );
    assert_eq!(wire.venues[0].ledger_currency, "EUR");
    assert_eq!(wire.primary_currency, "", "the node's primary is the dollar group");
    assert!(!serde_json::to_string(&wire).expect("serializes").contains("primary_currency"));
    assert_eq!(wire.equity_total.to_bits(), (50.0_f64 + 110.0).to_bits(), "the dollar blocks");

    // A EUR primary is stated, whichever block leads the frame.
    snap.portfolio.venues = vec![
        block("oanda", "oanda", "EUR", 7_000.0), // the PRIMARY, stopped
        block("binance", "binance", "", 100.0),
        block("oanda", "oanda", "EUR", 7_100.0),
    ];
    snap.portfolio.equity_total = 7_000.0 + 7_100.0;
    let wire = project(&snap, None, &[]);
    assert_eq!(
        wire.venues[0].ledger_currency, "",
        "PREMISE: the first block of the frame is the dollar one"
    );
    assert_eq!(wire.primary_currency, "EUR");
    assert!(
        serde_json::to_string(&wire).expect("serializes").contains(r#""primary_currency":"EUR""#)
    );
    assert_eq!(wire.equity_total.to_bits(), 7_100.0_f64.to_bits(), "the EUR block alone");
}

/// The door reads [`PublisherHandle::engine_blocks`], not the frame, and takes the first block of a route key: after
/// a restart that block must be the NEW engine's, with its own `extra_symbols`, not the stopped one's.
#[test]
fn engine_blocks_carry_the_newest_block_of_a_restarted_account() {
    let block = |key: &str, equity: f64, extra: &[&str]| vike_execution::VenueBlock {
        venue: "hyperliquid".into(),
        route_key: key.into(),
        equity,
        extra_symbols: extra.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues = vec![
        block("hyperliquid#LIVE", 1.0, &["SOL"]), // stopped, then started again below
        block("okx", 2.0, &[]),                   // stopped for good: still there
        block("hyperliquid#LIVE", 3.0, &[]),
    ];
    let handle = spawn(Arc::new(ArcSwap::from_pointee(snap)), None);
    let seen: Vec<_> = handle
        .engine_blocks()
        .into_iter()
        .map(|b| (b.route_key, b.equity, b.extra_symbols.len()))
        .collect();
    assert_eq!(seen, vec![("okx".to_string(), 2.0, 0), ("hyperliquid#LIVE".to_string(), 3.0, 0)]);
    handle.shutdown();
}

// -----------------------------------------------------------------------------------------
// The mount rows as they stand now (`PublisherHandle::mount_rows_now`).
// -----------------------------------------------------------------------------------------

/// A boot row as `wire_mount_rows` builds it: an id and an account, no key, no typed params.
fn boot_mount_row(mount_id: &str, route_key: &str) -> vike_tradehub_client::wire::MountViewWire {
    vike_tradehub_client::wire::MountViewWire {
        order_client_id_code: None,
        refusal_id: None,
        strategy: "boot strategy".into(),
        params: "boot params".into(),
        live: true,
        venue: String::new(),
        symbol: String::new(),
        interval: String::new(),
        mount_id: mount_id.into(),
        typed_params: None,
        asset_class: Some("CryptoPerp".into()),
        latched: false,
        route_key: route_key.into(),
        tier: None,
        not_running: false,
    }
}

/// What the core writes when a runtime mount lands, under `dir`.
fn record_runtime_mount(dir: &std::path::Path, id: &str, account: &str, stamp: i64) {
    let spec = vike_execution::MountSpec {
        venue: "bybit".into(),
        symbol: "ETHUSDT".into(),
        interval: "1m".into(),
        account: vike_model::accounts::account_keys::AccountLabel::parse(account).ok(),
        controller_id: Some(id.into()),
        file: Some("grid_dca/grid.rs".into()),
        name: None,
        rhai: None,
        params: vike_model::params::Params::new(),
        order_client_id_code: None,
    };
    vike_core::mount_topology::upsert(
        dir,
        vike_core::mount_topology::MountRecord::stamped(spec, stamp),
    )
    .expect("record the runtime mount");
}

/// A built frame whose core lists `mounts`.
fn publisher_of(
    mounts: Vec<vike_execution::MountView>,
    boot: Vec<vike_tradehub_client::wire::MountViewWire>,
    topology: Option<std::path::PathBuf>,
) -> PublisherHandle {
    let mut snap = CoreSnapshot::empty("bybit", "BTCUSDT");
    snap.sequence = 1;
    snap.mounts = mounts.into();
    spawn_with_mounts(Arc::new(ArcSwap::from_pointee(snap)), None, boot, Vec::new())
        .with_mount_topology_dir(topology)
}

/// **A mount added while the node runs has a row, naming the account it trades, and a boot mount unmounted meanwhile
/// has none.** Matched by id: by position, the boot row's strategy and its demo account were written onto the runtime
/// mount that took its place, so a `bybit#LIVE` strategy read as `bybit#DEMO`, and the mount itself had no row at all.
#[test]
fn a_runtime_mount_has_a_row_and_an_unmounted_boot_mount_has_none() {
    let dir = tempfile::tempdir().expect("temp state dir");
    record_runtime_mount(dir.path(), "rt-x", "LIVE", 5);
    let rt = vike_execution::MountView {
        mount_id: "rt_x".into(),
        latched: true,
        ..mount_row("bybit", "ETHUSDT", "1m")
    };
    let kept = mount_row("bybit", "SOLUSDT", "1m");
    let boot = vec![
        boot_mount_row("bybit__BTCUSDT__1m", "bybit#DEMO"),
        boot_mount_row(&kept.mount_id, "bybit#DEMO"),
    ];
    let handle = publisher_of(vec![rt, kept], boot.clone(), Some(dir.path().to_path_buf()));

    let rows = handle.mount_rows_now(boot);
    assert_eq!(
        rows.iter()
            .map(|r| (r.mount_id.as_str(), r.symbol.as_str(), r.route_key.as_str()))
            .collect::<Vec<_>>(),
        [("bybit__SOLUSDT__1m", "SOLUSDT", "bybit#DEMO"), ("rt_x", "ETHUSDT", "bybit#LIVE"),],
        "the boot row that lost its mount is gone, the one that kept it first, the runtime mount after: {rows:?}"
    );
    assert_eq!(rows[0].strategy, "boot strategy", "a boot row keeps its own text");
    assert_eq!(rows[1].strategy, "grid", "a runtime row says what was mounted");
    assert!(rows[1].params.starts_with("venue=bybit symbol=ETHUSDT interval=1m"), "{rows:?}");
    assert_eq!(rows[1].asset_class, None, "a mount command names no product");
    assert!(rows[1].latched, "the core's own state rides a runtime row too");
    handle.shutdown();
}

/// **A row goes only when the core has SAID its mount is gone.** A placeholder frame lists no mount because nothing is
/// built yet, and the frame of a core that faulted in its publish lists none because it could not build them: both keep
/// the boot rows, because an omission reads as "that mount is gone", a claim about a live book. So does a row that
/// carries no id to be matched by (the identity fallback).
#[test]
fn a_boot_row_stays_until_the_core_has_said_its_mount_is_gone() {
    let boot = || vec![boot_mount_row("bybit__BTCUSDT__1m", "bybit#DEMO")];
    let cell_of = |seq, fault: Option<&str>| {
        let mut snap = CoreSnapshot::empty("bybit", "BTCUSDT");
        snap.sequence = seq;
        snap.fault = fault.map(str::to_string);
        Arc::new(ArcSwap::from_pointee(snap))
    };
    for cell in [cell_of(0, None), cell_of(7, Some("panic during snapshot publish"))] {
        let handle = spawn_with_mounts(cell, None, boot(), Vec::new());
        assert_eq!(handle.mount_rows_now(boot()), boot());
        handle.shutdown();
    }

    let handle = spawn_with_mounts(cell_of(7, None), None, boot(), Vec::new());
    assert!(handle.mount_rows_now(boot()).is_empty(), "a built frame without it says it is gone");
    let idless = vec![boot_mount_row("", "")];
    assert_eq!(handle.mount_rows_now(idless.clone()), idless);
    handle.shutdown();
}

/// **A boot mount the start DEACTIVATED keeps its row** (owner 2026-11-14): the core does not hold it because its
/// account's venue is not connected, and the account is on the NOT RUNNING list, so the row stays (the overlay then
/// marks it not running); a boot mount whose account RUNS and that the core does not hold is gone, as before.
#[test]
fn a_deactivated_boot_mount_keeps_its_row_while_its_account_does_not_run() {
    let mut snap = CoreSnapshot::empty("bybit", "BTCUSDT");
    snap.sequence = 7;
    let boot = vec![
        boot_mount_row("fxcm__EURUSD__1m", "fxcm"),
        boot_mount_row("bybit__ETHUSDT__1m", "bybit"),
    ];
    let handle =
        spawn_with_mounts(Arc::new(ArcSwap::from_pointee(snap)), None, boot.clone(), Vec::new());
    handle.set_not_running(vec![vike_execution::ArmingNotRunningView {
        account: default_account_of("fxcm"),
        tier: Some(vike_model::AccountTier::Demo),
        reason: "FX market closed: login at 2026-11-15T21:00:00Z".into(),
        block: Some(vike_model::ArmingBlock::FxMarketClosed),
        refusal_id: None,
    }]);
    let rows = handle.mount_rows_now(boot);
    assert_eq!(rows.iter().map(|r| r.mount_id.as_str()).collect::<Vec<_>>(), ["fxcm__EURUSD__1m"]);
    handle.shutdown();
}

/// **A `.rs` mount the start deactivated for its BUILD keeps its row, NOT RUNNING with the reason, and reads its
/// file's params line once it runs** (the owner's ruling of 2026-11-14). Its account runs, so only the build state
/// keeps the row the core does not hold yet; a failed build's row carries the compiler's words.
#[test]
fn a_boot_mount_waiting_for_its_build_keeps_its_row_with_the_reason_and_reads_its_params_once_it_runs()
 {
    let boot = vec![boot_mount_row("bybit__BTCUSDT__1m", "bybit")];
    let builds = crate::config::MountBuilds::default();
    let waiting = publisher_of(Vec::new(), boot.clone(), None).with_mount_builds(builds.clone());
    builds.set("bybit__BTCUSDT__1m", Err("NOT RUNNING: building market_making/maker.rs".into()));
    let rows = waiting.mount_rows_now(boot.clone());
    assert_eq!(rows.len(), 1, "the row stays while the core does not hold it: {rows:?}");
    assert!(rows[0].not_running, "{rows:?}");
    assert_eq!(rows[0].params, "NOT RUNNING: building market_making/maker.rs");

    builds.set("bybit__BTCUSDT__1m", Err("error[E0425]: cannot find value `x`".into()));
    let rows = waiting.mount_rows_now(boot.clone());
    assert_eq!(
        (rows[0].not_running, rows[0].params.as_str()),
        (true, "error[E0425]: cannot find value `x`")
    );
    waiting.shutdown();

    let running = publisher_of(vec![mount_row("bybit", "BTCUSDT", "1m")], boot.clone(), None)
        .with_mount_builds(builds.clone());
    builds.set("bybit__BTCUSDT__1m", Ok("qty=0.001 half_spread=0.0005".into()));
    let rows = running.mount_rows_now(boot.clone());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(!rows[0].not_running, "{rows:?}");
    assert_eq!(rows[0].params, "qty=0.001 half_spread=0.0005");
    running.shutdown();

    // A mount that never waited for a build is as before: gone when its account runs and the core does not hold it.
    let plain = publisher_of(Vec::new(), boot.clone(), None);
    assert!(plain.mount_rows_now(boot).is_empty());
    plain.shutdown();
}

/// **A record the start's resurrect had refused does not speak for the boot mount it collided with, and a later
/// re-mount of that id does.** A profile mount holds id X on the demo account; the file still holds X on the live
/// account from before the restart, a mount the core refused as a duplicate. Reading it would call a demo mount live.
/// When the operator unmounts X and mounts it on the live account, the new stamp makes the record the truth.
#[test]
fn a_refused_record_does_not_name_the_account_and_a_later_remount_does() {
    let dir = tempfile::tempdir().expect("temp state dir");
    record_runtime_mount(dir.path(), "bybit__BTCUSDT__1m", "LIVE", 5);
    let boot = vec![boot_mount_row("bybit__BTCUSDT__1m", "bybit#DEMO")];
    let handle = publisher_of(
        vec![mount_row("bybit", "BTCUSDT", "1m")],
        boot.clone(),
        Some(dir.path().to_path_buf()),
    );
    assert_eq!(handle.mount_rows_now(boot.clone())[0].route_key, "bybit#DEMO");

    record_runtime_mount(dir.path(), "bybit__BTCUSDT__1m", "LIVE", 6);
    let now = handle.mount_rows_now(boot);
    assert_eq!(now[0].route_key, "bybit#LIVE", "{now:?}");
    assert_eq!(now[0].strategy, "grid", "{now:?}");
    handle.shutdown();
}

/// **A runtime mount with no record still has a row, and it does not read paper while its orders may reach a venue.**
/// Its account is unknown (no project, or a failed write): it names none, and `live` says whether any account of the
/// venue trades at one, because the core's row does not say which it is on.
#[test]
fn a_runtime_mount_with_no_record_has_a_row_that_names_no_account() {
    let block = |tier| vike_execution::VenueBlock {
        venue: "bybit".into(),
        route_key: "bybit#A".into(),
        tier: Some(tier),
        ..Default::default()
    };
    for (tier, live) in
        [(vike_model::AccountTier::Demo, true), (vike_model::AccountTier::Paper, false)]
    {
        let mut snap = CoreSnapshot::empty("bybit", "BTCUSDT");
        snap.sequence = 1;
        snap.mounts = vec![mount_row("bybit", "ETHUSDT", "1m")].into();
        snap.portfolio.venues = vec![block(tier)];
        let handle =
            spawn_with_mounts(Arc::new(ArcSwap::from_pointee(snap)), None, Vec::new(), Vec::new());
        let rows = handle.mount_rows_now(Vec::new());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].mount_id, "bybit__ETHUSDT__1m");
        assert_eq!(rows[0].strategy, "unknown");
        assert_eq!(rows[0].route_key, "");
        assert_eq!(rows[0].live, live, "{tier:?}: {rows:?}");
        handle.shutdown();
    }
}

// -----------------------------------------------------------------------------------------
// What the publisher stamps onto a frame (`NodeReadings`): uptime, the HALT sentinel, order ages.
// -----------------------------------------------------------------------------------------

/// The uptime is counted from the PROCESS's start (`process_started`), as of the instant the frame
/// is built: 90 s after it reads 90 000 ms, not a figure measured from the publisher's spawn.
#[test]
fn a_frame_carries_the_uptime_counted_from_the_process_start() {
    let snap = CoreSnapshot::empty("sim", "BTCUSDT");
    let mut wire = project(&snap, None, &[]);
    assert_eq!(wire.uptime_ms, None, "`project` is pure: the stamp is the publisher's");
    NodeReadings::default().stamp(&mut wire, &snap, process_started() + Duration::from_secs(90));
    assert_eq!(wire.uptime_ms, Some(90_000));
}

/// **The frame carries the process's ERROR and WARN lines, one row per target**, and says nothing where nothing counts
/// them: no `vike_log::init` ran in this test binary, so the stamp leaves the field `None`, never an empty "no line".
#[test]
fn a_frame_carries_the_line_counts_per_target_and_none_where_nothing_counts() {
    let snap = CoreSnapshot::empty("sim", "BTCUSDT");
    let mut wire = project(&snap, None, &[]);
    NodeReadings::default().stamp(&mut wire, &snap, Instant::now());
    assert_eq!(wire.log_lines, None, "this process counts nothing");

    let counts = vike_log::line_counts::counted(|| {
        tracing::error!(target: "vike_tradehub::publish", "one");
        tracing::warn!(target: "vike_tradehub::publish", "two");
        tracing::error!(target: "panic", "three");
    });
    let rows = project_log_lines(&counts);
    let read: Vec<(&str, u64, u64, bool)> = rows
        .iter()
        .map(|r| (r.target.as_str(), r.errors, r.warnings, r.latest_error_ms > 0))
        .collect();
    assert_eq!(
        read,
        [("panic", 1, 0, true), ("vike_tradehub::publish", 1, 1, true)],
        "one row per target, in target order, each ERROR row with its time"
    );
}

/// **The frame carries the alert mount's failed deliveries as they stand at the stamp**: `0` from a publisher never
/// handed the count (no alerting mounted, nothing failed), then the shared count, read again at every stamp.
#[test]
fn a_frame_carries_the_failed_alert_deliveries_and_zero_with_no_mount() {
    let snap = CoreSnapshot::empty("sim", "BTCUSDT");
    let readings = NodeReadings::default();
    let mut wire = project(&snap, None, &[]);
    assert_eq!(
        wire.alert_deliveries_failed, None,
        "`project` is pure: the stamp is the publisher's"
    );
    readings.stamp(&mut wire, &snap, Instant::now());
    assert_eq!(wire.alert_deliveries_failed, Some(0));

    let failed = Arc::new(AtomicU64::new(2));
    readings.alert_deliveries_failed.set(Arc::clone(&failed)).expect("the first count");
    readings.stamp(&mut wire, &snap, Instant::now());
    assert_eq!(wire.alert_deliveries_failed, Some(2));
    failed.fetch_add(1, Ordering::Relaxed);
    readings.stamp(&mut wire, &snap, Instant::now());
    assert_eq!(wire.alert_deliveries_failed, Some(3));
}

/// **The frame carries the journal watch's last measure as it stands at the stamp**: nothing from a publisher never
/// handed the cell (no journal watched), nothing while the watch has not measured, then each measure.
#[test]
fn a_frame_carries_the_journal_watchs_last_measure_and_nothing_without_one() {
    let snap = CoreSnapshot::empty("sim", "BTCUSDT");
    let readings = NodeReadings::default();
    let mut wire = project(&snap, None, &[]);
    readings.stamp(&mut wire, &snap, Instant::now());
    assert_eq!(wire.journal, None, "no journal watched");

    let cell = Arc::new(Mutex::new(None));
    readings.journal.set(Arc::clone(&cell)).expect("the first cell");
    readings.stamp(&mut wire, &snap, Instant::now());
    assert_eq!(wire.journal, None, "not measured yet");
    let measured = JournalWire { total_bytes: 3, segments: 1, bytes_per_hour: Some(2), over: true };
    *cell.lock().unwrap() = Some(measured);
    readings.stamp(&mut wire, &snap, Instant::now());
    assert_eq!(wire.journal, Some(measured));
}

/// The sentinel is `None` ("the node did not say") until a path is handed over, then follows the
/// file in both directions: `touch` shows on the next frame and `rm` clears it, nothing latches.
#[test]
fn the_halt_sentinel_is_unsaid_without_a_path_and_follows_the_file_with_one() {
    let snap = CoreSnapshot::empty("sim", "BTCUSDT");
    let stamped = |facts: &NodeReadings| {
        let mut wire = project(&snap, None, &[]);
        facts.stamp(&mut wire, &snap, Instant::now());
        wire.halt_sentinel
    };
    assert_eq!(stamped(&NodeReadings::default()), None, "no path handed: silence, never `false`");

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("HALT");
    let facts = NodeReadings::default();
    facts.halt_path.set(path.clone()).expect("the first path is taken");
    assert_eq!(stamped(&facts), Some(false));
    std::fs::write(&path, b"").expect("touch the sentinel");
    assert_eq!(stamped(&facts), Some(true));
    std::fs::remove_file(&path).expect("rm the sentinel");
    assert_eq!(stamped(&facts), Some(false));
}

/// An order's age counts from the instant the node FIRST saw it, grows with later frames, belongs
/// only to an order that is not terminal, and its bookkeeping goes when the order does.
#[test]
fn an_orders_age_counts_from_the_first_sight_and_ends_with_the_order() {
    use vike_model::OrderStatus;
    let order = |order_client_id: &str, status: OrderStatus| vike_execution::OrderView {
        data_delay: None,
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    let facts = NodeReadings::default();
    let ages = |snap: &CoreSnapshot, at: Instant| -> Vec<Option<u64>> {
        let mut wire = project(snap, None, &[]);
        facts.stamp(&mut wire, snap, at);
        wire.orders.iter().map(|o| o.age_ms).collect()
    };
    let t0 = Instant::now();
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.orders = vec![
        std::sync::Arc::new(order("c-1", OrderStatus::Accepted)),
        std::sync::Arc::new(order("c-2", OrderStatus::Filled)),
    ];

    // First sight: the resting order is new, the terminal one carries no age at all.
    assert_eq!(ages(&snap, t0), vec![Some(0), None]);
    // A later frame counts from the FIRST sight, not from itself.
    let hour = t0 + Duration::from_secs(3600);
    assert_eq!(ages(&snap, hour), vec![Some(3_600_000), None]);
    // A second order that appears now starts at zero while the first keeps growing.
    snap.orders.push(std::sync::Arc::new(order("c-3", OrderStatus::PartiallyFilled)));
    assert_eq!(ages(&snap, hour), vec![Some(3_600_000), None, Some(0)]);
    assert_eq!(
        ages(&snap, hour + Duration::from_secs(60)),
        vec![Some(3_660_000), None, Some(60_000)]
    );
    // An order that ends loses its age and its entry (the registry itself is never pruned).
    std::sync::Arc::make_mut(&mut snap.orders[0]).status = OrderStatus::Filled;
    assert_eq!(ages(&snap, hour + Duration::from_secs(61)), vec![None, None, Some(61_000)]);
    let kept: Vec<String> = facts.first_seen.lock().unwrap().keys().cloned().collect();
    assert_eq!(kept, vec!["c-3".to_string()]);
}

/// **With no subscriber the publisher frames nothing, and the first to come is sent the CURRENT state.** A node
/// publishing to nobody leaves no frame (a frame is the whole snapshot's JSON, made at every publish); a subscriber that
/// comes later is sent the snapshot as it stands then, never an older one, and an order resting since before it came is
/// as old as its first publish, not as the first frame. A subscriber that leaves takes the publisher back to framing
/// nothing, and the next one is sent the state of its own moment.
#[test]
fn with_no_subscriber_nothing_is_framed_and_a_late_one_is_sent_the_current_state() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.orders = vec![std::sync::Arc::new(vike_execution::OrderView {
        data_delay: None,
        order_client_id: "c-1".into(),
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    })];
    let cell = Arc::new(ArcSwap::from_pointee(snap.clone()));
    let publisher = spawn(Arc::clone(&cell), None);
    let first = |subscription: &PublisherSubscription| -> CoreSnapshotWire {
        match subscription.recv_timeout(Duration::from_secs(10)) {
            PublisherMailboxReceive::Frame(bytes) => {
                match read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.to_vec())) {
                    Ok(Response::SnapshotFrame(wire)) => *wire,
                    other => panic!("expected SnapshotFrame, got {other:?}"),
                }
            }
            other => panic!("no frame: {other:?}"),
        }
    };
    let publish = |snap: &mut CoreSnapshot, sequence: u64| {
        snap.sequence = sequence;
        cell.store(Arc::new(snap.clone()));
        std::thread::sleep(POLL_INTERVAL * 3);
    };
    for sequence in 1..=5 {
        publish(&mut snap, sequence);
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(publisher.latest_frame().is_none(), "a publish nobody hears is not framed");

    let late = publisher.subscribe();
    let wire = first(&late);
    assert_eq!(wire.sequence, 5, "the state as it stands");
    let age = wire.orders[0].age_ms.expect("a resting order is aged");
    assert!(age >= 300, "the age counts from the order's first publish, not this frame: {age} ms");

    drop(late);
    publish(&mut snap, 6);
    let deadline = Instant::now() + Duration::from_secs(10);
    while publisher.latest_frame().is_some() {
        assert!(Instant::now() < deadline, "the publish after the last subscriber left was framed");
        std::thread::sleep(Duration::from_millis(5));
    }
    let next = publisher.subscribe();
    assert_eq!(first(&next).sequence, 6, "the state of its own moment");
    publisher.shutdown();
}

/// Through a REAL publisher: a handle handed a sentinel path answers every `Request::Snapshot` with
/// the file's state at that moment, and one never handed a path says nothing.
#[test]
fn a_publisher_frames_the_sentinel_it_was_handed_and_stays_silent_otherwise() {
    let framed = |handle: &PublisherHandle| match read_frame::<_, Response>(
        &mut std::io::Cursor::new(handle.snapshot_frame_now(BodyCodec::Json).as_slice().to_vec()),
    )
    .expect("decode frame")
    {
        Response::SnapshotFrame(w) => *w,
        other => panic!("expected SnapshotFrame, got {other:?}"),
    };
    let cell = || Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty("sim", "BTCUSDT")));

    let silent = spawn(cell(), None);
    let frame = framed(&silent);
    assert_eq!((frame.halt_sentinel, frame.uptime_ms.is_some()), (None, true));
    silent.shutdown();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("HALT");
    let watching = spawn(cell(), None).with_halt_sentinel_path(path.clone());
    assert_eq!(framed(&watching).halt_sentinel, Some(false));
    std::fs::write(&path, b"").expect("touch the sentinel");
    assert_eq!(framed(&watching).halt_sentinel, Some(true));
    watching.shutdown();
}

/// **The route-key roster is one key per engine that RUNS** (v0.1.80 review b). The core's list only grows and a
/// stopped account keeps its block, but the core counts only live engines when it asks whether a venue has
/// several, so the bracket gate must not count a restarted account's old block (a duplicate key) or a stopped
/// lane twin's. A raw list refused every bracket of the venue for as long as either was published.
#[test]
fn the_route_key_roster_is_one_key_per_engine_that_runs() {
    use vike_execution::ArmingNotRunningView;
    let block = |key: &str| vike_execution::VenueBlock {
        venue: "binance".into(),
        route_key: key.into(),
        ..Default::default()
    };
    let bracket = vike_tradehub_client::wire::CommandWire::Bracket(
        vike_tradehub_client::wire::OrderBracketWire {
            order_client_id: None,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(1.0),
            stop_loss: 0.9,
            take_profit: 1.1,
            account: None,
        },
    );
    // The default account was restarted (its old block is first), and the lane twin was stopped (flat, row gone).
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![block("binance"), block("binance#PERP"), block("binance")];
    let stopped_twin = ArmingNotRunningView {
        account: vike_model::accounts::account_keys::AccountNamed::of(
            "binance",
            vike_model::accounts::account_keys::AccountLabel::parse("PERP").expect("a label"),
        ),
        tier: None,
        reason:
            "the perp lane is not running: state a leverage on the binance demo account to start it"
                .into(),
        block: None,
        refusal_id: None,
    };

    // Before the twin is listed as stopped it runs: two accounts, one key each, and the bracket is refused.
    let handle = spawn(Arc::new(ArcSwap::from_pointee(snap)), None);
    assert_eq!(handle.engine_route_keys(), vec!["binance#PERP".to_string(), "binance".to_string()]);
    assert!(
        crate::server::refusal::account_refusal(&bracket, &handle.engine_route_keys()).is_some()
    );

    // Listed as stopped it is no account of the exchange (as the core counts), and the bracket goes to the
    // venue's one account.
    handle.set_not_running(vec![stopped_twin]);
    assert_eq!(handle.engine_route_keys(), vec!["binance".to_string()]);
    assert_eq!(
        crate::server::refusal::account_refusal(&bracket, &handle.engine_route_keys()),
        None
    );
    // The blocks themselves are untouched: a stopped account's position is still shown.
    assert_eq!(handle.engine_blocks().len(), 2);
    handle.shutdown();
}

/// An order the core HOLDS until its release reaches the wire with its account and its release, the price
/// condition as a side and a level (the wire's condition reads the order's own symbol); a held bracket exit
/// keeps the bytes it always had (no `account`, no `release` key).
#[test]
fn a_time_held_order_projects_its_account_and_release() {
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_model::{OrderRelease, OrderReleasePrice, OrderReleasePriceSide};
    use vike_tradehub_client::wire::{OrderReleasePriceWire, OrderReleaseWire};
    let mut held = OrderHeldView {
        order_client_id: "h-1".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 2.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(90.0),
        trigger_price: None,
        parent_order_id: None,
        account: AccountLabel::parse("ALT").ok(),
        release: Some(OrderRelease {
            at: 4_097_217_600_000,
            price: Some(OrderReleasePrice {
                symbol: "BTCUSDT".into(),
                level: 85.0,
                side: OrderReleasePriceSide::Below,
                venue: None,
            }),
        }),
        waiting: Some("no price of BTCUSDT on binance on this node yet".into()),
        attributes: None,
        trail_percent: None,
        adjust: None,
        display_quantity: None,
    };
    let wire = project_held(&held);
    assert_eq!(wire.account.as_deref(), Some("ALT"));
    assert_eq!(
        wire.waiting.as_deref(),
        Some("no price of BTCUSDT on binance on this node yet"),
        "why it still waits rides beside the release"
    );
    assert_eq!(
        wire.release,
        Some(OrderReleaseWire {
            at: 4_097_217_600_000,
            price: Some(OrderReleasePriceWire::Below(85.0)),
            symbol: None,
            venue: None,
        })
    );
    assert_eq!(wire.release_venue, None, "a condition on the order's own venue names none");
    // A strategy's order held on ANOTHER symbol's price names that symbol on the wire.
    let mut on_eth = held.clone();
    on_eth.release = on_eth.release.map(|mut r| {
        r.price = r.price.map(|mut c| {
            c.symbol = "ETHUSDT".into();
            c
        });
        r
    });
    assert_eq!(project_held(&on_eth).release.and_then(|r| r.symbol).as_deref(), Some("ETHUSDT"));
    held.account = None;
    held.release = None;
    held.waiting = None;
    let exit = serde_json::to_string(&project_held(&held)).expect("serialises");
    assert!(
        !exit.contains("account") && !exit.contains("release") && !exit.contains("waiting"),
        "{exit}"
    );
}

/// A held order whose release reads ANOTHER venue's price (decision 0267's 2.13) names that venue as the row's
/// READING `release_venue`, and NEVER inside its `OrderReleaseWire` (an older client's `deny_unknown_fields` there would
/// fail its whole frame); its condition's symbol is named, being another than the order's own.
#[test]
fn a_held_order_read_on_another_venue_names_the_venue_beside_its_release() {
    use vike_model::{OrderRelease, OrderReleasePrice, OrderReleasePriceSide};
    let held = OrderHeldView {
        order_client_id: "h-x".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(190.0),
        trigger_price: None,
        parent_order_id: None,
        account: None,
        release: Some(OrderRelease {
            at: 4_097_217_600_000,
            price: Some(OrderReleasePrice {
                symbol: "BTCUSDT".into(),
                level: 100_000.0,
                side: OrderReleasePriceSide::Above,
                venue: Some("binance".into()),
            }),
        }),
        waiting: Some("binance's last BTCUSDT price is 75 s old".into()),
        attributes: None,
        trail_percent: None,
        adjust: None,
        display_quantity: None,
    };
    let wire = project_held(&held);
    assert_eq!(wire.release_venue.as_deref(), Some("binance"));
    let release = wire.release.clone().expect("the release");
    assert_eq!((release.venue, release.symbol.as_deref()), (None, Some("BTCUSDT")));
    let json = serde_json::to_value(&wire).expect("serialises");
    assert_eq!(json["release_venue"], "binance");
    assert!(json["release"].get("venue").is_none(), "{json}");
}

/// A resting order carries the attributes wave's readings (its attributes, its percent trail, its adjustment and the
/// release it rests at its venue with), each as its wire mirror; an order with none of them publishes none of the
/// keys, so its bytes are the frame from before.
#[test]
fn a_published_order_carries_its_attributes_percent_trail_adjustment_and_release() {
    use vike_model::{OrderAttributes, OrderRelease, OrderStopAdjust, OrderStopAdjustTo};
    use vike_tradehub_client::wire::OrderReleaseWire;
    let mut snap = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
    let order = |order_type| vike_execution::OrderView {
        data_delay: None,
        order_client_id: "c-1".into(),
        venue: "ibkr".into(),
        account: None,
        symbol: "AAPL.SMART.USD".into(),
        side: -1,
        qty: 1.0,
        order_type,
        price: Some(190.0),
        trigger_price: Some(180.0),
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    };
    let attributes = OrderAttributes {
        all_or_none: true,
        sweep_to_fill: false,
        discretionary_amount: Some(0.05),
    };
    let adjust = OrderStopAdjust {
        at: 200.0,
        to: OrderStopAdjustTo::StopLimit { stop: 185.0, limit: 184.0 },
    };
    snap.orders = vec![
        std::sync::Arc::new(vike_execution::OrderView {
            attributes: Some(attributes),
            release: Some(OrderRelease { at: 4_097_217_600_000, price: None }),
            ..order(vike_model::OrderType::Limit)
        }),
        std::sync::Arc::new(vike_execution::OrderView {
            trail_percent: Some(1.5),
            ..order(vike_model::OrderType::Trailing)
        }),
        std::sync::Arc::new(vike_execution::OrderView {
            adjust: Some(adjust),
            ..order(vike_model::OrderType::Stop)
        }),
    ];
    let wire = project(&snap, None, &[]);
    assert_eq!(
        wire.orders[0].attributes,
        Some(OrderAttributes {
            all_or_none: true,
            sweep_to_fill: false,
            discretionary_amount: Some(0.05)
        })
    );
    assert_eq!(
        wire.orders[0].release,
        Some(OrderReleaseWire { at: 4_097_217_600_000, price: None, symbol: None, venue: None })
    );
    assert_eq!(wire.orders[1].trail_percent, Some(1.5));
    assert_eq!(
        wire.orders[2].adjust,
        Some(OrderStopAdjust {
            at: 200.0,
            to: OrderStopAdjustTo::StopLimit { stop: 185.0, limit: 184.0 }
        })
    );
    snap.orders = vec![std::sync::Arc::new(order(vike_model::OrderType::Limit))];
    let plain = serde_json::to_value(&project(&snap, None, &[]).orders[0]).expect("serialises");
    for key in ["attributes", "trail_percent", "adjust", "release"] {
        assert!(plain.get(key).is_none(), "{key}: {plain}");
    }
}

/// A HELD order carries its terms as a resting order does (its attributes, its percent trail, its adjustment and its
/// display), and a resting order its display (zero: a HIDDEN order); a row without them publishes none of the keys.
#[test]
fn a_held_order_projects_its_terms_and_a_resting_order_its_display() {
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo};
    let attributes = OrderAttributes { all_or_none: true, ..Default::default() };
    let held = OrderHeldView {
        order_client_id: "h-1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Trailing,
        price: None,
        trigger_price: Some(180.0),
        parent_order_id: None,
        account: None,
        release: Some(vike_model::OrderRelease { at: 4_097_217_600_000, price: None }),
        waiting: None,
        attributes: Some(attributes),
        trail_percent: Some(1.5),
        adjust: Some(OrderStopAdjust { at: 200.0, to: OrderStopAdjustTo::Stop { stop: 195.0 } }),
        display_quantity: Some(0.0),
    };
    let wire = project_held(&held);
    let shown = OrderAttributes { all_or_none: true, ..Default::default() };
    assert_eq!(wire.attributes, Some(shown));
    assert_eq!(wire.trail_percent, Some(1.5));
    assert_eq!(
        wire.adjust,
        Some(OrderStopAdjust { at: 200.0, to: OrderStopAdjustTo::Stop { stop: 195.0 } })
    );
    assert_eq!(wire.display_quantity, Some(0.0));
    let plain = OrderHeldView {
        attributes: None,
        trail_percent: None,
        adjust: None,
        display_quantity: None,
        ..held
    };
    let json = serde_json::to_value(project_held(&plain)).expect("serialises");
    for key in ["attributes", "trail_percent", "adjust", "display_quantity"] {
        assert!(json.get(key).is_none(), "{key}: {json}");
    }

    let order = |display_qty| OrderView {
        data_delay: None,
        order_client_id: "c-1".into(),
        venue: "ibkr".into(),
        account: None,
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(190.0),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        average_fill_price: 0.0,
        refusal_id: None,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: display_qty,
    };
    assert_eq!(project_order(&order(Some(0.0))).display_quantity, Some(0.0));
    assert_eq!(project_order(&order(Some(2.0))).display_quantity, Some(2.0), "an iceberg's slice");
    let bare = serde_json::to_value(project_order(&order(None))).expect("serialises");
    assert!(bare.get("display_quantity").is_none(), "{bare}");
}

/// **The node says which source priced each position, and nothing for one it could not price** (top layers wave 3):
/// the core's `PositionView::mark_source` crosses as its word, so a client tells an unpriced position's 0.0 P/L
/// from a flat one, and an unpriced position writes no key at all.
#[test]
fn a_position_crosses_with_the_source_that_priced_it() {
    let priced = vike_execution::PositionView {
        mark_source: Some(vike_execution::PriceSource::LastTrade),
        ..vike_execution::testing::position_view("bybit", "BTCUSDT", 0.5)
    };
    let unpriced = vike_execution::PositionView {
        mark_source: None,
        ..vike_execution::testing::position_view("bybit", "WIFUSDT", 3.0)
    };
    assert_eq!(project_position(&priced).mark_source.as_deref(), Some("last_trade"));
    assert_eq!(project_position(&unpriced).mark_source, None);
    let bytes = serde_json::to_string(&project_position(&unpriced)).expect("encodes");
    assert!(!bytes.contains("mark_source"), "{bytes}");
}
