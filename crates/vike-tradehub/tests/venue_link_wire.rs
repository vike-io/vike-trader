//! **A venue path the gatewayhub's bridge lost reaches the node's lease registry, and with it the observe frame the
//! doctor reads**, over a real socket against the gatewayhub client's own fake gatewayhub
//! (`crates/vike-gatewayhub-client/tests/support/fake_trade_peer.rs`), so every claim is read off the FRAMES the peer
//! recorded and the registry the publisher stamps from (`LeaseFacts::venue_link_down`).
//!
//! * a dukascopy lease whose status says the sidecar is down lists the account, with the bridge's words; the same
//!   lease, once the status says it is up, lists nothing (the outage is not remembered);
//! * a gatewayhub restart is no verdict: a daemon with no adapter yet does not end the watch, and the outage that
//!   follows it is still seen;
//! * a daemon that reports no path (one that predates the field) is asked once and never again, and lists nothing;
//! * every other venue is asked nothing, as before (`late_currency_wire.rs` pins the same for its own watch).
//!
//! Each test leases its OWN account label, because the registries are per backend but the peers share a process.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_gatewayhub_client::proto::{
    AdapterReply, ArmingRow, ArmingTable, GridSource, Request, Response, TradeDown,
    TradeStatusReport, TradeUp, VenueFacts, WireVenueLink,
};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub::server::symbol_admission::SymbolFacts as _;
use vike_tradehub_client::wire::WireVenueLinkDown;
use vike_tradehub_core::{AdapterPolicy, AdapterStartQuery, VenueBackend};

#[path = "../../vike-gatewayhub-client/tests/support/fake_trade_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_trade_peer;

use fake_trade_peer::{
    Step, TradePeer, TradePeerConfig, default_script, fast_trade_timings, link_to,
};

const ID: &str = "vike-tradehub-venue-link-test";
/// The first ask is made at once and the next one [`vike_tradehub::remote_backend::venue_link::ASK_EVERY`] later, on a
/// box that may run at many times its core count.
const WAIT: Duration = Duration::from_secs(30);

/// What the scripted daemon's status says about the path: `None` = it reports none, `adapter` = whether it calls the
/// adapter up.
#[derive(Clone)]
struct Says {
    link: Option<WireVenueLink>,
    adapter_up: bool,
}

type Daemon = Arc<Mutex<Says>>;

fn daemon(link: Option<WireVenueLink>) -> Daemon {
    Arc::new(Mutex::new(Says { link, adapter_up: true }))
}

fn sidecar_down() -> WireVenueLink {
    WireVenueLink {
        up: false,
        why: "JForex session disconnected".to_string(),
        failed_restarts: 1,
        retry_in_ms: 10_000,
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
            reply: Box::new(AdapterReply::Live {
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
                recon: false,
                leverage_cap: None,
                account_ccy: None,
                account_ccy_assumed: false,
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

fn config_for(venue: &str, route_key: &str, script: fake_trade_peer::Script) -> TradePeerConfig {
    let venue = venue.to_string();
    let mut cfg = TradePeerConfig::new();
    cfg.route_key = route_key.to_string();
    cfg.features.push(vike_gatewayhub_client::proto::trade_venue_feature(&venue));
    let arming_venue = venue.clone();
    let mut cfg = cfg.with_rpc(move |req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::new(ArmingTable {
            facts: std::collections::BTreeMap::from([(
                arming_venue.clone(),
                VenueFacts {
                    adapter_present: true,
                    addresses_accounts: true,
                    takes_recon_trigger: false,
                    grid_source: GridSource::NoGrid,
                    process_exclusive: None,
                    clock_decl_word: "wired".to_string(),
                },
            )]),
            rows: vec![ArmingRow {
                venue: arming_venue.clone(),
                label: None,
                tier: Some(AccountTier::Demo),
                effective: Some(AccountTier::Demo),
                block_word: ArmingBlock::None.word().to_string(),
                account_ids: vec![1],
                book: None,
            }],
            key_changed: Vec::new(),
        }))],
        other => vec![Response::Error(format!("unscripted: {other:?}"))],
    });
    cfg.script = script;
    cfg
}

/// A started lease: the peer, the backend (kept alive, and read for the registry) and the core lane.
struct Rig {
    backend: RemoteBackend,
    _lane: fake_trade_peer::Lane,
    peer: TradePeer,
    route: String,
}

impl Rig {
    fn down(&self) -> Vec<WireVenueLinkDown> {
        self.backend.symbol_facts().venue_link_down()
    }
}

fn start_lease(venue: &str, mount: &str, label: &str, says: &Daemon) -> Rig {
    let route = format!("{venue}#{label}");
    let peer = TradePeer::start(config_for(venue, &route, script(says)));
    let backend =
        RemoteBackend::connect(link_to(&peer.addr_string()), accounts(venue), ID.to_string(), WAIT)
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
        recon_enabled: false,
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

fn expected(route: &str) -> WireVenueLinkDown {
    WireVenueLinkDown {
        route_key: route.to_string(),
        why: "JForex session disconnected".to_string(),
        failed_restarts: 1,
        retry_in_ms: 10_000,
    }
}

/// **A sidecar the bridge lost is listed with the bridge's words, and is not listed once it is back.**
#[test]
fn a_down_sidecar_is_listed_and_the_outage_ends_with_the_report_that_it_is_up() {
    let says = daemon(Some(sidecar_down()));
    let rig = start_lease("dukascopy", "EURUSD", "LINKA", &says);
    assert!(
        TradePeer::wait_until(WAIT, || rig.down() == vec![expected(&rig.route)]),
        "the outage reaches the registry: {:?} / {:?}",
        rig.down(),
        rig.peer.ups()
    );

    says.lock().unwrap().link = Some(WireVenueLink::working());
    assert!(
        TradePeer::wait_until(WAIT, || rig.down().is_empty()),
        "the report that the path is up ends the outage: {:?}",
        rig.down()
    );
}

/// **A gatewayhub restart is no verdict**: a daemon that has no adapter yet reports no path because it has no
/// adapter, which neither clears the outage nor ends the watch; the outage it reports once the adapter is back is
/// seen, whatever happened to the lease in between.
#[test]
fn a_daemon_with_no_adapter_gives_no_verdict_and_the_watch_survives_it() {
    let says = daemon(Some(WireVenueLink::working()));
    let rig = start_lease("dukascopy", "EURUSD", "LINKB", &says);
    assert!(TradePeer::wait_until(WAIT, || status_asks(&rig.peer) >= 1), "{:?}", rig.peer.ups());
    assert!(rig.down().is_empty());

    *says.lock().unwrap() = Says { link: None, adapter_up: false };
    let asked = status_asks(&rig.peer);
    assert!(
        TradePeer::wait_until(WAIT, || status_asks(&rig.peer) > asked),
        "a daemon without an adapter is still asked"
    );
    *says.lock().unwrap() = Says { link: Some(sidecar_down()), adapter_up: true };
    assert!(
        TradePeer::wait_until(WAIT, || rig.down() == vec![expected(&rig.route)]),
        "the outage after it is seen: {:?}",
        rig.peer.ups()
    );
}

/// **A daemon that reports no path is asked once, and never again**: a daemon that predates the field (or a lane that
/// names none) must not be read as "up" and must not be polled for ever.
#[test]
fn a_daemon_that_reports_no_path_is_asked_once_and_lists_nothing() {
    let says = daemon(None);
    let rig = start_lease("dukascopy", "EURUSD", "LINKC", &says);
    assert!(TradePeer::wait_until(WAIT, || status_asks(&rig.peer) >= 1), "{:?}", rig.peer.ups());
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(status_asks(&rig.peer), 1, "the watch is over after the first answer");
    assert!(rig.down().is_empty());
}

/// **Every other venue is asked nothing**: an oanda lease keeps its single start and no status request, however long
/// it lives.
#[test]
fn a_venue_whose_exec_is_not_an_owned_child_is_never_asked() {
    let says = daemon(Some(sidecar_down()));
    let rig = start_lease("oanda", "EURUSD", "LINKD", &says);
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(status_asks(&rig.peer), 0, "{:?}", rig.peer.ups());
    assert!(rig.down().is_empty(), "{}", rig.route);
}
