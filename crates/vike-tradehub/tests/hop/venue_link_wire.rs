//! **A venue path the gatewayhub's bridge lost reaches the node's lease registry, and with it the observe frame
//! `trade health` reads**, over a real socket against the gatewayhub client's own fake gatewayhub
//! (`crates/vike-gatewayhub-client/src/test_support/fake_trade_peer.rs`), so every claim is read off the FRAMES the peer
//! recorded and the registry the publisher stamps from (`TradeLeaseRegistry::venue_link_down`).
//!
//! * a dukascopy lease whose status says the sidecar is down lists the account, with the bridge's words; the same
//!   lease, once the status says it is up, lists nothing (the outage is not remembered); an oanda lease does the same
//!   for its transactions stream;
//! * a gatewayhub restart is no verdict: a daemon with no adapter yet does not end the watch, and the outage that
//!   follows it is still seen;
//! * a daemon that reports no path (one that predates the field, and alpaca, fxcm, ig and ibkr) keeps being asked, lists
//!   nothing and reads not known, until its adapter dies: that is listed like any other venue's;
//! * the lease ITSELF, the hop before the bridge: a link the gatewayhub's death cut and a lease closed for good are
//!   listed (`TradeLeaseRegistry::trade_link_down`), and a refused start keeps its cause (`TradeLeaseRegistry::start_refusal`).
//!
//! Each test leases its OWN account label, because the registries are per backend but the peers share a process.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_gatewayhub_client::proto::{
    AdapterReply, Response, TradeBye, TradeDown, TradeStatusReport, TradeUp, VenueLinkWire,
};
use vike_gatewayhub_client::trade::{TradeLeaseBuilder, TradeLinkState};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub::server::symbol_admission::AccountReadiness as _;
use vike_tradehub::server::symbol_admission::LinkHealth as _;
use vike_tradehub::venue_backend::gateway::VenueBackendGateway;
use vike_tradehub::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry;
use vike_tradehub_client::wire::VenueLinkDownWire;
use vike_tradehub_core::{AdapterPolicy, AdapterStartQuery, VenueBackend};

use vike_gatewayhub_client::test_support::fake_trade_peer;

use crate::hop_rig::config_for;
use fake_trade_peer::{
    Step, TradePeer, TradePeerConfig, default_script, fast_trade_timings, link_to,
};
use vike_log::poll::wait_until;

const ID: &str = "vike-tradehub-venue-link-test";
/// The first ask is made at once and the next one [`vike_tradehub::venue_backend::gateway::venue_link::ASK_EVERY`] later, on a
/// box that may run at many times its core count.
const WAIT: Duration = Duration::from_secs(30);

/// What the scripted daemon's status says about the path: `None` = it reports none, `adapter` = whether it calls the
/// adapter up.
#[derive(Clone)]
struct Says {
    link: Option<VenueLinkWire>,
    adapter_up: bool,
}

type Daemon = Arc<Mutex<Says>>;

fn daemon(link: Option<VenueLinkWire>) -> Daemon {
    Arc::new(Mutex::new(Says { link, adapter_up: true }))
}

fn sidecar_down() -> VenueLinkWire {
    VenueLinkWire {
        up: false,
        why: "JForex session disconnected".to_string(),
        failed_restarts: 1,
        retry_in_ms: 10_000,
        effect: String::new(),
    }
}

fn accounts(venue: &str) -> AccountDirectory {
    AdapterPolicy::default().with_account(venue, &AccountLabel::Default, AccountTier::Demo).accounts
}

/// The daemon: every `AdapterStart` is answered Live, and every `TradeStatus` with what `says` holds now.
fn script(says: &Daemon) -> fake_trade_peer::Script {
    let says = Arc::clone(says);
    let base = default_script();
    Arc::new(move |frame| match frame {
        TradeUp::AdapterStart(start) => vec![Step::Send(TradeDown::AdapterReply {
            rid: start.rid,
            reply: Box::new(AdapterReply::Running {
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
                reconcile: false,
                leverage_cap: None,
                account_currency: None,
                account_currency_assumed: false,
                native_time_release: false,
                broker_code_len: 0,
            }),
        })],
        TradeUp::TradeStatus { rid } => {
            let now = says.lock().unwrap().clone();
            vec![Step::Send(TradeDown::TradeStatusIs {
                rid: *rid,
                status: TradeStatusReport {
                    adapter_up: now.adapter_up,
                    venue_link: now.link,
                    ..TradeStatusReport::default()
                },
            })]
        }
        other => base(other),
    })
}

/// A started lease: the peer, the backend (kept alive, and read for the registry) and the core lane.
struct Rig {
    backend: VenueBackendGateway,
    _lane: fake_trade_peer::Lane,
    peer: TradePeer,
    route: String,
}

impl Rig {
    fn down(&self) -> Vec<VenueLinkDownWire> {
        self.backend.symbol_facts().venue_link_down()
    }
}

fn start_lease(venue: &str, mount: &str, label: &str, says: &Daemon) -> Rig {
    let route = format!("{venue}#{label}");
    let peer = TradePeer::start(config_for(venue, &route, Some(script(says))));
    let backend = VenueBackendGateway::connect(
        link_to(&peer.address_string()),
        accounts(venue),
        ID.to_string(),
        WAIT,
    )
    .with_timings(fast_trade_timings());
    backend.symbol_facts().bind_exposure_cap(|_| true);
    let (events, lane) = fake_trade_peer::lane();
    let account = AccountLabel::parse(label).expect("a legal label");
    let q = AdapterStartQuery {
        venue,
        account: &account,
        tier: AccountTier::Demo,
        symbol: mount,
        declared_legs: &[],
        reconcile_enabled: false,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: &route,
        properties_rec: None,
    };
    backend.start_adapter(q, &events, None).expect("the account starts");
    Rig { backend, _lane: lane, peer, route }
}

fn status_asks(peer: &TradePeer) -> usize {
    peer.ups().iter().filter(|f| matches!(f, TradeUp::TradeStatus { .. })).count()
}

fn expected(route: &str) -> VenueLinkDownWire {
    VenueLinkDownWire {
        route_key: route.to_string(),
        why: "JForex session disconnected".to_string(),
        failed_restarts: 1,
        retry_in_ms: 10_000,
        effect: String::new(),
        no_adapter: false,
    }
}

/// **The lease itself: a link the gatewayhub's death cut is listed with the link's words and its age, and is not listed
/// once the redial lands.** `trade health`'s `gateway-link` row reads this list, through a real signal off a real socket.
#[test]
fn a_lost_trade_link_is_listed_and_the_outage_ends_with_the_redial() {
    let rig = start_lease("binance", "BTCUSDT", "LINKE", &daemon(None));
    let links = || rig.backend.symbol_facts().trade_link_down();
    assert!(links().is_empty(), "a lease that opened is up: {:?}", links());

    rig.peer.refuse_open(Some(Response::Error("down for the test".to_string())));
    rig.peer.kill_lease();
    assert!(wait_until(WAIT, || !links().is_empty()), "the loss reaches the registry");
    let down = &links()[0];
    assert_eq!(down.route_key, rig.route);
    assert!(!down.closed, "it redials");
    assert!(!down.why.is_empty(), "the link's own words: {down:?}");

    rig.peer.refuse_open(None);
    assert!(wait_until(WAIT, || links().is_empty()), "the redial ends it: {:?}", links());
}

/// **A lease closed for good is listed closed, with why**: the case that read RUNNING for ever, because nothing but the
/// re-teaching worker ever looked at it. The registry reads it off the session, so it is there however it closed.
#[test]
fn a_lease_taken_over_is_listed_closed_with_the_words() {
    let rig = start_lease("binance", "BTCUSDT", "LINKF", &daemon(None));
    let links = || rig.backend.symbol_facts().trade_link_down();
    assert!(rig.peer.send(&TradeDown::Bye(TradeBye::Takeover { by_key_id: "key-9".to_string() })));
    assert!(wait_until(WAIT, || links().iter().any(|l| l.closed)), "{:?}", links());
    let closed = &links()[0];
    assert_eq!(closed.route_key, rig.route);
    assert!(closed.why.contains("taken over by key-9"), "{closed:?}");
}

/// **A lease this node ends itself is no loss**: a teardown closes it, and that must not read as a closed lease. Unless
/// the node said why the engine lost it (the re-teaching worker's refusal of a client bound at another tier): that
/// engine stays published with no way to the venue, so the lease reads closed, in the node's words.
#[test]
fn a_lease_this_node_ends_itself_is_listed_only_when_it_said_why() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (events, _lane) = fake_trade_peer::lane();
    let account = fake_trade_peer::account(ID);
    let route = account.route_key.clone();
    let session = TradeLeaseBuilder::new(peer.link(), account, events)
        .with_timings(fast_trade_timings())
        .open()
        .expect("the lease opens");
    let facts = TradeLeaseRegistry::default();
    facts.register(&route, session.clone());
    assert!(facts.trade_link_down().is_empty(), "{:?}", facts.trade_link_down());

    session.detach();
    assert_eq!(session.link_state(), TradeLinkState::Closed);
    assert!(facts.trade_link_down().is_empty(), "{:?}", facts.trade_link_down());

    facts.set_trade_link_lost(&route, Some("bound at another tier"));
    let listed = facts.trade_link_down();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(listed[0].closed, "{listed:?}");
    assert_eq!(listed[0].why, "bound at another tier");
}

/// **`adapter failed` keeps its cause beside the account**, in the words of the refusal, and the next start that comes
/// up leaves no cause behind.
#[test]
fn a_refused_start_keeps_its_cause_until_the_next_start() {
    let refusing = Arc::new(Mutex::new(true));
    let says = daemon(None);
    let script: fake_trade_peer::Script = {
        let (refusing, inner) = (Arc::clone(&refusing), script(&says));
        Arc::new(move |frame| match frame {
            TradeUp::AdapterStart(start) if *refusing.lock().unwrap() => {
                vec![Step::Send(TradeDown::AdapterReply {
                    rid: start.rid,
                    reply: Box::new(AdapterReply::NotRunning {
                        block_word: "adapter_failed".to_string(),
                        refusal_text: "the venue refused the login".to_string(),
                    }),
                })]
            }
            other => inner(other),
        })
    };
    let route = "binance#LINKH".to_string();
    let peer = TradePeer::start(config_for("binance", &route, Some(script)));
    let backend = VenueBackendGateway::connect(
        link_to(&peer.address_string()),
        accounts("binance"),
        ID.to_string(),
        WAIT,
    )
    .with_timings(fast_trade_timings());
    backend.symbol_facts().bind_exposure_cap(|_| true);
    let (events, _lane) = fake_trade_peer::lane();
    let account = AccountLabel::parse("LINKH").expect("a legal label");
    let start = || AdapterStartQuery {
        venue: "binance",
        account: &account,
        tier: AccountTier::Demo,
        symbol: "BTCUSDT",
        declared_legs: &[],
        reconcile_enabled: false,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: &route,
        properties_rec: None,
    };
    assert!(backend.start_adapter(start(), &events, None).is_err(), "the refusal");
    assert_eq!(
        backend.symbol_facts().start_refusal(&route).as_deref(),
        Some("the venue refused the login")
    );

    *refusing.lock().unwrap() = false;
    backend.start_adapter(start(), &events, None).expect("the account starts");
    assert_eq!(backend.symbol_facts().start_refusal(&route), None, "a new start, no old cause");
}

/// **A sidecar the bridge lost is listed with the bridge's words, and is not listed once it is back.**
#[test]
fn a_down_sidecar_is_listed_and_the_outage_ends_with_the_report_that_it_is_up() {
    let says = daemon(Some(sidecar_down()));
    let rig = start_lease("dukascopy", "EURUSD", "LINKA", &says);
    assert!(
        wait_until(WAIT, || rig.down() == vec![expected(&rig.route)]),
        "the outage reaches the registry: {:?} / {:?}",
        rig.down(),
        rig.peer.ups()
    );

    says.lock().unwrap().link = Some(VenueLinkWire::working());
    assert!(
        wait_until(WAIT, || rig.down().is_empty()),
        "the report that the path is up ends the outage: {:?}",
        rig.down()
    );
}

/// **A gatewayhub restart is no verdict**: a daemon that has no adapter yet reports no path because it has no
/// adapter, which neither clears the outage nor ends the watch; the outage it reports once the adapter is back is
/// seen, whatever happened to the lease in between.
#[test]
fn a_daemon_with_no_adapter_gives_no_verdict_and_the_watch_survives_it() {
    let says = daemon(Some(VenueLinkWire::working()));
    let rig = start_lease("dukascopy", "EURUSD", "LINKB", &says);
    assert!(wait_until(WAIT, || status_asks(&rig.peer) >= 1), "{:?}", rig.peer.ups());
    assert!(rig.down().is_empty());

    *says.lock().unwrap() = Says { link: None, adapter_up: false };
    let asked = status_asks(&rig.peer);
    assert!(
        wait_until(WAIT, || status_asks(&rig.peer) > asked),
        "a daemon without an adapter is still asked"
    );
    *says.lock().unwrap() = Says { link: Some(sidecar_down()), adapter_up: true };
    assert!(
        wait_until(WAIT, || rig.down() == vec![expected(&rig.route)]),
        "the outage after it is seen: {:?}",
        rig.peer.ups()
    );
}

/// **A daemon that reports no path keeps being asked**, because the next answer may be that its adapter is gone. It is
/// not read as "up", and it lists nothing.
#[test]
fn a_daemon_that_reports_no_path_keeps_being_asked_and_lists_nothing() {
    let says = daemon(None);
    let rig = start_lease("dukascopy", "EURUSD", "LINKC", &says);
    assert!(wait_until(WAIT, || status_asks(&rig.peer) >= 2), "{:?}", rig.peer.ups());
    assert!(rig.down().is_empty());
    assert!(rig.backend.symbol_facts().venue_path_up().is_empty());
}

/// **A bridge panic on a venue that reports no session of its own (alpaca), or none yet (fxcm, ig and ibkr while their
/// bridge cannot vouch either way).** Those leases were asked nothing, so `HEALTH: OK` stood beside the UNKNOWN row with
/// every order refused. A live adapter stays not known; a dead
/// one is listed after about 30 s of real asks, as on any other venue, and its return ends it. Nothing here reaches a
/// venue: the daemon is the fake peer.
#[test]
fn a_dead_adapter_is_listed_on_a_venue_that_reports_no_session() {
    const PATIENCE: Duration = Duration::from_secs(120);
    let says = daemon(None);
    let rigs: Vec<Rig> = [
        ("alpaca", "AAPL", "LINKL"),
        ("fxcm", "EURUSD", "LINKM"),
        ("ig", "CS.D.EURUSD.CFD.IP", "LINKN"),
        ("ibkr", "AAPL.SMART.USD", "LINKO"),
    ]
    .into_iter()
    .map(|(venue, mount, label)| start_lease(venue, mount, label, &says))
    .collect();
    for rig in &rigs {
        assert!(wait_until(WAIT, || status_asks(&rig.peer) >= 2), "{}", rig.route);
        assert!(rig.down().is_empty(), "{}", rig.route);
        assert!(rig.backend.symbol_facts().venue_path_up().is_empty(), "{}", rig.route);
    }

    *says.lock().unwrap() = Says { link: None, adapter_up: false };
    for rig in &rigs {
        assert!(
            wait_until(PATIENCE, || rig.down().first().is_some_and(|d| d.no_adapter)),
            "{}: {:?}",
            rig.route,
            rig.peer.ups()
        );
        assert_eq!(rig.down()[0].route_key, rig.route);
        assert!(rig.backend.symbol_facts().venue_path_up().is_empty(), "{}", rig.route);
    }

    *says.lock().unwrap() = Says { link: None, adapter_up: true };
    for rig in &rigs {
        assert!(wait_until(WAIT, || rig.down().is_empty()), "{}: {:?}", rig.route, rig.down());
        assert!(rig.backend.symbol_facts().venue_path_up().is_empty(), "{}", rig.route);
    }
}

/// **A pump venue's lease answers for its own session**: asked like the others, a report of an up stream is the positive
/// reading, a stream that dropped moves it to the down list in the bridge's words, and an adapter that is gone stops the
/// reading at once. `trade health`'s per-account `gateway-link` rows read these lists.
#[test]
fn a_binance_stream_reads_up_then_down_then_up_in_the_bridges_words() {
    let says = daemon(Some(VenueLinkWire::working()));
    let rig = start_lease("binance", "BTCUSDT", "LINKG", &says);
    let up = || rig.backend.symbol_facts().venue_path_up();
    assert!(wait_until(WAIT, || up() == vec![rig.route.clone()]), "{:?}", rig.peer.ups());
    assert!(rig.down().is_empty());

    says.lock().unwrap().link = Some(VenueLinkWire {
        why: "reset by peer".to_string(),
        retry_in_ms: 30_000,
        effect: "orders still go out".to_string(),
        ..VenueLinkWire::default()
    });
    assert!(wait_until(WAIT, || !rig.down().is_empty()), "{:?}", rig.peer.ups());
    assert_eq!(rig.down()[0].why, "reset by peer");
    assert!(up().is_empty(), "a path reported down is not also up");

    says.lock().unwrap().link = Some(VenueLinkWire::working());
    assert!(wait_until(WAIT, || rig.down().is_empty() && !up().is_empty()), "{:?}", up());
}

/// **A bridge panics on a venue that reports its path: the lease stays up, `adapter_up` stays false, and nothing restarts
/// it.** Through the real worker and a real socket, that reaches the list `trade health` reads, as no live adapter, and an
/// adapter that comes back ends it. About 30 s of real asks.
#[test]
fn an_adapter_that_stays_dead_under_a_live_lease_is_listed_and_its_return_ends_it() {
    const PATIENCE: Duration = Duration::from_secs(120);
    let says = daemon(Some(VenueLinkWire::working()));
    let rig = start_lease("dukascopy", "EURUSD", "LINKK", &says);
    let up = || rig.backend.symbol_facts().venue_path_up();
    assert!(wait_until(WAIT, || !up().is_empty()), "{:?}", rig.peer.ups());

    *says.lock().unwrap() = Says { link: None, adapter_up: false };
    assert!(
        wait_until(PATIENCE, || rig.down().first().is_some_and(|d| d.no_adapter)),
        "{:?}",
        rig.peer.ups()
    );
    assert_eq!(rig.down()[0].route_key, rig.route);
    assert!(up().is_empty(), "a dead adapter is not a path that is up");
    assert!(rig.backend.symbol_facts().trade_link_down().is_empty(), "the lease is up");

    *says.lock().unwrap() = Says { link: Some(VenueLinkWire::working()), adapter_up: true };
    assert!(wait_until(WAIT, || rig.down().is_empty()), "{:?}", rig.down());
}

/// **A daemon that reports no path is never read as a session that is up**: it is asked, and the account stays not known.
#[test]
fn a_report_with_no_path_is_not_a_reading_of_up() {
    let rig = start_lease("binance", "BTCUSDT", "LINKI", &daemon(None));
    assert!(wait_until(WAIT, || status_asks(&rig.peer) >= 1), "{:?}", rig.peer.ups());
    std::thread::sleep(Duration::from_millis(300));
    assert!(rig.backend.symbol_facts().venue_path_up().is_empty());
    assert!(rig.down().is_empty());
}

/// **`reteach`'s `NotRunning` arm: a gatewayhub that restarted and refused the adapter does not read as up.** The lease
/// redials and stays up, so nothing else looks; it is listed with the refusal's own words, on a venue whose bridge
/// reports no path too, and a later restart that starts the adapter ends it.
#[test]
fn a_restarted_gatewayhub_that_refuses_the_adapter_is_listed_with_its_words_and_a_later_start_ends_it()
 {
    let refusing = Arc::new(Mutex::new(false));
    let says = daemon(None);
    let script: fake_trade_peer::Script = {
        let (refusing, inner) = (Arc::clone(&refusing), script(&says));
        Arc::new(move |frame| match frame {
            TradeUp::AdapterStart(start) if *refusing.lock().unwrap() => {
                vec![Step::Send(TradeDown::AdapterReply {
                    rid: start.rid,
                    reply: Box::new(AdapterReply::NotRunning {
                        block_word: "adapter_failed".to_string(),
                        refusal_text: "the venue refused the login".to_string(),
                    }),
                })]
            }
            other => inner(other),
        })
    };
    let route = "fxcm#LINKJ".to_string();
    let peer = TradePeer::start(config_for("fxcm", &route, Some(script)));
    let backend = VenueBackendGateway::connect(
        link_to(&peer.address_string()),
        accounts("fxcm"),
        ID.to_string(),
        WAIT,
    )
    .with_timings(fast_trade_timings());
    backend.symbol_facts().bind_exposure_cap(|_| true);
    let (events, _lane) = fake_trade_peer::lane();
    let account = AccountLabel::parse("LINKJ").expect("a legal label");
    let q = AdapterStartQuery {
        venue: "fxcm",
        account: &account,
        tier: AccountTier::Demo,
        symbol: "EURUSD",
        declared_legs: &[],
        reconcile_enabled: false,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: &route,
        properties_rec: None,
    };
    backend.start_adapter(q, &events, None).expect("the account starts");
    let listed = || backend.symbol_facts().venue_link_down();
    assert!(listed().is_empty());

    *refusing.lock().unwrap() = true;
    *says.lock().unwrap() = Says { link: None, adapter_up: false };
    peer.restart(); // the gatewayhub restarts under the lease
    assert!(wait_until(WAIT, || !listed().is_empty()), "{:?}", peer.ups());
    let down = &listed()[0];
    assert_eq!(down.route_key, route);
    assert!(down.no_adapter, "{down:?}");
    assert_eq!(down.why, "the venue refused the login");
    assert!(down.say().contains("NO LIVE ADAPTER"), "{}", down.say());

    *refusing.lock().unwrap() = false;
    *says.lock().unwrap() = Says { link: None, adapter_up: true };
    peer.restart(); // and again, which starts it
    assert!(wait_until(WAIT, || listed().is_empty()), "{:?}", listed());
}

/// **OANDA's transactions stream reaches the same list, in the bridge's words about orders**: its loss refuses no order,
/// so `trade health` must not say so; and the report that it is open again ends the outage.
#[test]
fn an_oanda_stream_that_is_down_is_listed_with_the_bridges_words_and_ends_with_its_return() {
    let link = VenueLinkWire {
        why: "HTTP 401 on stream open".to_string(),
        failed_restarts: 3,
        retry_in_ms: 30_000,
        effect: "orders still go out".to_string(),
        up: false,
    };
    let says = daemon(Some(link));
    let rig = start_lease("oanda", "EURUSD", "LINKE", &says);
    assert!(
        wait_until(WAIT, || rig.down().first().is_some_and(|d| d.effect == "orders still go out")),
        "the outage reaches the registry: {:?} / {:?}",
        rig.down(),
        rig.peer.ups()
    );
    assert!(!rig.down()[0].say().contains("refused"), "{}", rig.down()[0].say());

    says.lock().unwrap().link = Some(VenueLinkWire::working());
    assert!(wait_until(WAIT, || rig.down().is_empty()), "{:?}", rig.down());
}
