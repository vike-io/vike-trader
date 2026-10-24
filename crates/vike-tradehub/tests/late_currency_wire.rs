//! **A currency the venue states AFTER the engine started without it, over a real socket** (decision 0183, the
//! node half): the lease of an fxcm account the venue gave no word on (its login failed, so the gatewayhub told an
//! assumed USD) keeps asking the same `AdapterStart`, and the reply that finally states a currency asks the account
//! watcher to start the account again. Driven against the gatewayhub client's own fake gatewayhub
//! (`crates/vike-gatewayhub-client/tests/support/fake_trade_peer.rs`), so every claim is read off the FRAMES the
//! peer recorded and the request the lease worker left (`late_currency::take_remount`).
//!
//! * a late pound is asked for, once, after the asks that heard nothing, and the asks stop;
//! * a currency the venue stated AT the start is never watched (the start moved the mount, as it always did);
//! * an adapter the daemon does not call up is not asked, and a currency no pair follows asks for nothing;
//! * a gatewayhub restart whose new daemon states the currency is judged by the same rule;
//! * every other venue is asked nothing, as before.
//!
//! Each test leases its OWN account label, because the request set is process-wide and the tests share a process.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_gatewayhub_client::proto::{
    AdapterReply, ArmingRow, ArmingTable, GridSource, Request, Response, TradeDown,
    TradeStatusReport, TradeUp, VenueFacts,
};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub::remote_backend::late_currency::take_remount;
use vike_tradehub_core::{AdapterBind, AdapterPolicy, AdapterStartQuery, VenueBackend};

#[path = "../../vike-gatewayhub-client/tests/support/fake_trade_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_trade_peer;

use fake_trade_peer::{
    Step, TradePeer, TradePeerConfig, default_script, fast_trade_timings, link_to,
};

const ID: &str = "vike-tradehub-late-currency-test";
const WAIT: Duration = Duration::from_secs(8);
/// The asks of a test that watches them: every 150 ms.
const FAST: &[Duration] = &[Duration::from_millis(150)];
/// The asks of a test that must not see one: an hour.
const NEVER: &[Duration] = &[Duration::from_secs(3_600)];

/// What the scripted daemon's replies say about the account's currency: `(code, assumed)`.
type Says = Arc<Mutex<(Option<&'static str>, bool)>>;

fn says(ccy: Option<&'static str>, assumed: bool) -> Says {
    Arc::new(Mutex::new((ccy, assumed)))
}

fn now_says(says: &Says, ccy: Option<&'static str>, assumed: bool) {
    *says.lock().unwrap() = (ccy, assumed);
}

fn accounts(venue: &str) -> AccountDirectory {
    AdapterPolicy::default().with_account(venue, &AccountLabel::Default, AccountTier::Demo).accounts
}

/// The daemon: every `AdapterStart` is answered with what `says` holds now, and `TradeStatus` says the adapter is
/// up unless `adapter_up` is false.
fn script(says: &Says, adapter_up: bool) -> fake_trade_peer::Script {
    let says = Arc::clone(says);
    let base = default_script();
    Arc::new(move |frame| match frame {
        TradeUp::AdapterStart(start) => {
            let (ccy, assumed) = *says.lock().unwrap();
            vec![Step::Send(TradeDown::AdapterReply {
                rid: start.rid,
                reply: Box::new(AdapterReply::Live {
                    bound_tier: AccountTier::Demo,
                    grid: None,
                    contract_value: None,
                    margin_mode: None,
                    leg_grids: Vec::new(),
                    recon: false,
                    leverage_cap: None,
                    account_ccy: ccy.map(str::to_string),
                    account_ccy_assumed: assumed,
                }),
            })]
        }
        TradeUp::TradeStatus { rid } if !adapter_up => {
            vec![Step::Send(TradeDown::TradeStatusIs {
                rid: *rid,
                status: TradeStatusReport::default(),
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

/// A started lease: the peer, the backend (kept alive), the bind and the core lane (kept alive, or the link would
/// call the core gone).
struct Rig {
    bind: AdapterBind,
    _backend: RemoteBackend,
    _lane: fake_trade_peer::Lane,
    peer: TradePeer,
    route: String,
}

/// Start `venue`'s account `label` (on the venue's wired pair `mount`) against a daemon that answers as `says`
/// says, asking again on `pauses`.
fn start_lease(
    venue: &str,
    mount: &str,
    label: &str,
    says: &Says,
    adapter_up: bool,
    pauses: &'static [Duration],
) -> Rig {
    let route = format!("{venue}#{label}");
    let peer = TradePeer::start(config_for(venue, &route, script(says, adapter_up)));
    let backend =
        RemoteBackend::connect(link_to(&peer.addr_string()), accounts(venue), ID.to_string(), WAIT)
            .with_timings(fast_trade_timings())
            .with_late_currency_pauses(pauses);
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
    let bind = backend.start_adapter(q, &events, None).expect("the account starts");
    Rig { bind, _backend: backend, _lane: lane, peer, route }
}

fn adapter_starts(peer: &TradePeer) -> usize {
    peer.ups().iter().filter(|f| matches!(f, TradeUp::AdapterStart(_))).count()
}

fn status_asks(peer: &TradePeer) -> usize {
    peer.ups().iter().filter(|f| matches!(f, TradeUp::TradeStatus { .. })).count()
}

/// Every symbol an `AdapterStart` named, in order.
fn start_symbols(peer: &TradePeer) -> Vec<String> {
    peer.ups()
        .into_iter()
        .filter_map(|f| match f {
            TradeUp::AdapterStart(s) => Some(s.symbol),
            _ => None,
        })
        .collect()
}

/// **A login that succeeds late is heard of, and the account watcher is asked to start the account again.** The
/// first start is answered with the gatewayhub's assumed USD (its login failed), the asks that follow hear the same
/// and request nothing, the one that hears a STATED pound requests the restart of exactly this account, and the asks
/// then stop (the watch is over). Every ask names the first start's symbol, and every one is preceded by a status
/// request (the adapter must be called up).
#[test]
fn a_currency_the_venue_states_late_asks_the_account_watcher_to_start_the_account_again() {
    let daemon = says(Some("USD"), true);
    let rig = start_lease("fxcm", "EURUSD", "LATEA", &daemon, true, FAST);
    assert_eq!(
        (rig.bind.account_ccy.as_deref(), rig.bind.account_ccy_assumed),
        (Some("USD"), true),
        "the start was told an assumed USD"
    );
    assert_eq!(rig.bind.mount_symbol, None, "and stands on the wired pair");

    assert!(
        TradePeer::wait_until(WAIT, || adapter_starts(&rig.peer) >= 3),
        "the same start is asked again while the venue says nothing: {:?}",
        rig.peer.ups()
    );
    assert!(!take_remount(&rig.route), "an assumed answer asks for nothing");

    now_says(&daemon, Some("GBP"), false);
    assert!(
        TradePeer::wait_until(WAIT, || take_remount(&rig.route)),
        "the account is asked to be started again: {:?}",
        rig.peer.ups()
    );

    // The watch is over: no ask follows (one may already have been in flight).
    std::thread::sleep(Duration::from_millis(400));
    let settled = adapter_starts(&rig.peer);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(adapter_starts(&rig.peer), settled, "nothing is asked once the venue has spoken");
    assert!(!take_remount(&rig.route), "and it was asked for once");
    assert!(
        start_symbols(&rig.peer).iter().all(|s| s == "EURUSD"),
        "every ask is the first start, never a different one: {:?}",
        start_symbols(&rig.peer)
    );
    assert!(
        status_asks(&rig.peer) >= adapter_starts(&rig.peer) - 1,
        "each ask follows a status request"
    );
}

/// **A currency the venue STATED at the start is never watched**: a pound moved the mount to the pound pair as it
/// always did, a dollar left it on the wired pair, and in both nothing is asked again however long the lease lives,
/// so a stated currency that later changes is not this module's to heal (the door refuses a rate in any other
/// currency, as before).
#[test]
fn a_currency_stated_at_the_start_is_never_watched() {
    let daemon = says(Some("GBP"), false);
    let rig = start_lease("fxcm", "EURUSD", "LATEB", &daemon, true, FAST);
    assert_eq!(rig.bind.mount_symbol.as_deref(), Some("EURGBP"), "the start moved the mount");
    assert_eq!(start_symbols(&rig.peer), ["EURUSD", "EURGBP"]);

    now_says(&daemon, Some("EUR"), false);
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(adapter_starts(&rig.peer), 2, "no ask: {:?}", start_symbols(&rig.peer));
    assert!(!take_remount(&rig.route));

    // A dollar the venue STATED stays on the wired pair, which is the mount the watch is armed on: only the
    // venue's word at the start keeps it out.
    let dollar = says(Some("USD"), false);
    let rig = start_lease("fxcm", "EURUSD", "LATEJ", &dollar, true, FAST);
    assert_eq!(
        (rig.bind.account_ccy.as_deref(), rig.bind.account_ccy_assumed, &rig.bind.mount_symbol),
        (Some("USD"), false, &None),
        "stated, and not moved"
    );
    now_says(&dollar, Some("GBP"), false);
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(adapter_starts(&rig.peer), 1, "no ask: {:?}", start_symbols(&rig.peer));
    assert_eq!(status_asks(&rig.peer), 0);
    assert!(!take_remount(&rig.route));
}

/// **A request an older lease of the account left is forgotten by the next start**: the request belongs to the
/// engine whose worker made it, and the account watcher must not stop a fresh one for it.
#[test]
fn a_request_an_older_lease_left_is_forgotten_by_the_next_start() {
    let route = "fxcm#LATEI";
    vike_tradehub::remote_backend::late_currency::request_remount(route);
    let daemon = says(Some("USD"), true);
    let rig = start_lease("fxcm", "EURUSD", "LATEI", &daemon, true, NEVER);
    assert_eq!(rig.route, route);
    assert!(!take_remount(route), "the start forgot it");
}

/// **An adapter the daemon does not call up is not asked**: a question must not revive a dead one. The lease
/// asks for the daemon's status at every pause and sends no start, and a late pound asks for nothing.
#[test]
fn an_adapter_the_daemon_does_not_call_up_is_not_asked() {
    let daemon = says(Some("USD"), true);
    let rig = start_lease("fxcm", "EURUSD", "LATEC", &daemon, false, FAST);
    now_says(&daemon, Some("GBP"), false);
    assert!(
        TradePeer::wait_until(WAIT, || status_asks(&rig.peer) >= 3),
        "the status is asked at every pause: {:?}",
        rig.peer.ups()
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(adapter_starts(&rig.peer), 1, "no start but the first");
    assert!(!take_remount(&rig.route));
}

/// **A currency no pair follows asks for nothing, and ends the watch**: the venue states USD (the assumption was
/// right; the balance hold is the toggle's) or any money without a row.
#[test]
fn a_currency_no_pair_follows_ends_the_watch_and_asks_for_nothing() {
    for (label, ccy) in [("LATED", "USD"), ("LATEE", "EUR")] {
        let daemon = says(Some("USD"), true);
        let rig = start_lease("fxcm", "EURUSD", label, &daemon, true, FAST);
        assert!(TradePeer::wait_until(WAIT, || adapter_starts(&rig.peer) >= 2), "{ccy}");
        now_says(&daemon, Some(ccy), false);
        assert!(TradePeer::wait_until(WAIT, || adapter_starts(&rig.peer) >= 3), "{ccy}");
        std::thread::sleep(Duration::from_millis(400));
        let settled = adapter_starts(&rig.peer);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(adapter_starts(&rig.peer), settled, "{ccy}: the watch is over");
        assert!(!take_remount(&rig.route), "{ccy}: nothing to start again on");
    }
}

/// **A gatewayhub restart whose new daemon CAN log in is judged by the same rule.** The reteach worker sends the
/// stored start again and used to throw the reply's currency away. A restart that still states nothing asks for
/// nothing; the next one, which states the pound, asks for the account to be started again.
#[test]
fn a_gatewayhub_restart_that_states_the_currency_asks_for_the_account_to_be_started_again() {
    let daemon = says(Some("USD"), true);
    let rig = start_lease("fxcm", "EURUSD", "LATEF", &daemon, true, NEVER);

    rig.peer.restart();
    assert!(TradePeer::wait_until(WAIT, || adapter_starts(&rig.peer) >= 2), "{:?}", rig.peer.ups());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!take_remount(&rig.route), "the restarted daemon still states nothing");

    now_says(&daemon, Some("GBP"), false);
    rig.peer.restart();
    assert!(
        TradePeer::wait_until(WAIT, || take_remount(&rig.route)),
        "the restarted daemon states the pound: {:?}",
        rig.peer.ups()
    );
}

/// **Any other venue is asked nothing**: an oanda lease told an assumed USD keeps its single start however long it
/// lives (it has no pair to follow a currency with), and so does an fxcm lease on a mount somebody chose.
#[test]
fn a_lease_with_no_pair_to_follow_is_never_asked() {
    let daemon = says(Some("USD"), true);
    let oanda = start_lease("oanda", "EURUSD", "LATEG", &daemon, true, FAST);
    let chosen = start_lease("fxcm", "USDJPY", "LATEH", &daemon, true, FAST);
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(adapter_starts(&oanda.peer), 1, "oanda: {:?}", start_symbols(&oanda.peer));
    assert_eq!(
        adapter_starts(&chosen.peer),
        1,
        "fxcm on a chosen mount: {:?}",
        start_symbols(&chosen.peer)
    );
    assert_eq!(status_asks(&oanda.peer) + status_asks(&chosen.peer), 0);
}
