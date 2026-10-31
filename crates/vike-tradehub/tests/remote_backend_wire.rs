//! **`RemoteBackend` over a real socket**: the venue port driven against the gatewayhub client's own
//! fake gatewayhub (`crates/vike-gatewayhub-client/tests/support/fake_trade_peer.rs`, included by
//! path as its header asks), so every claim below is read off the FRAMES the peer recorded, not off
//! what the backend says about itself.
//!
//! What it holds, one test each:
//!
//! * the arming table is asked ONCE, under `Scope::Trade`, and every later read is memory;
//! * **`TradeOpen` is answered before `AdapterStart` is sent** (drift register R2: the gatewayhub
//!   claims the account's lock at the open, so a second trading process is refused before a venue
//!   client exists), and the `AdapterStart` frame carries exactly what the fold asked for;
//! * a lease the gatewayhub refuses is a word, a lease another client holds is a refusal the root
//!   turns into a refused mount, and a daemon that is gone is `Unavailable`;
//! * the bound the daemon answers is held to the account's own tier;
//! * a refusal's word is read exhaustively: an unknown one, or one that names a running block, is
//!   `adapter failed`, never armed;
//! * the preflight crosses as ONE question per read (the plan, then each clock, credential and network read) and the
//!   answers come back as the fold's four outcomes, a declared gap with its reason and the network with its numbers;
//!   the report they build is held equal to the in-process one by `tests/preflight_parity.rs`.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_gatewayhub_client::proto::{
    AdapterReply, ArmingRow, ArmingTable, ClockLeg, ClockRiskClass, ClockRiskInfo, CredentialLeg,
    LegResult, NetReading, PreflightLists, PreflightStep, Request, Response, TradeDown, TradeUp,
    VenueFacts,
};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub::server::symbol_admission::SymbolFacts;
use vike_tradehub_core::preflight::{CredentialGap, ServerTimeGap};
use vike_tradehub_core::{
    AdapterPolicy, AdapterStartQuery, BackendError, IdentityRecord, NetReading as FoldNetReading,
    PreflightQuery, VenueBackend, Withhold,
};

#[path = "../../vike-gatewayhub-client/tests/support/fake_trade_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_trade_peer;

use fake_trade_peer::{Seen, Step, TradePeer, TradePeerConfig, link_to};

const ID: &str = "vike-tradehub-test";

fn accounts(rows: &[(&str, AccountTier)]) -> vike_secrets::account_directory::AccountDirectory {
    rows.iter()
        .fold(AdapterPolicy::default(), |p, (venue, tier)| {
            p.with_account(venue, &AccountLabel::Default, *tier)
        })
        .accounts
}

fn facts(takes_recon_trigger: bool) -> VenueFacts {
    VenueFacts {
        adapter_present: true,
        addresses_accounts: true,
        takes_recon_trigger,
        grid_source: vike_gatewayhub_client::proto::GridSource::NoGrid,
        process_exclusive: None,
        clock_decl_word: "wired".to_string(),
    }
}

fn binance_row(tier: AccountTier) -> ArmingRow {
    ArmingRow {
        venue: "binance".to_string(),
        label: None,
        tier: Some(tier),
        effective: Some(tier),
        block_word: ArmingBlock::None.word().to_string(),
        account_ids: vec![3],
        book: None,
    }
}

fn config(takes_recon_trigger: bool) -> TradePeerConfig {
    TradePeerConfig::new().with_rpc(move |req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::new(ArmingTable {
            facts: BTreeMap::from([("binance".to_string(), facts(takes_recon_trigger))]),
            rows: vec![binance_row(AccountTier::Demo)],
            key_changed: Vec::new(),
        }))],
        Request::PreflightQuery { .. } => {
            vec![Response::PreflightLegs { legs: Vec::new(), plan: None }]
        }
        other => vec![Response::Error(format!("unscripted: {other:?}"))],
    })
}

fn connect(peer: &TradePeer) -> RemoteBackend {
    RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[("binance", AccountTier::Demo)]),
        ID.to_string(),
        Duration::from_secs(5),
    )
}

/// The query for binance's engine, with no decoration.
fn query<'a>(
    legs: &'a [String],
    label: &'a AccountLabel,
    withhold: Option<Withhold>,
) -> AdapterStartQuery<'a> {
    AdapterStartQuery {
        venue: "binance",
        account: label,
        tier: AccountTier::Demo,
        symbol: "BTCUSDT",
        declared_legs: legs,
        recon_enabled: true,
        risk_profile: None,
        market_slippage: Some(0.002),
        halt_admit: vike_model::HaltAdmit::default(),
        withhold,
        account_leverage: None,
        route_key: "binance",
        properties_rec: None,
    }
}

fn adapter_starts(peer: &TradePeer) -> Vec<vike_gatewayhub_client::proto::AdapterStart> {
    peer.ups()
        .into_iter()
        .filter_map(|f| match f {
            TradeUp::AdapterStart(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// **The table is asked ONCE and under the Trade scope**; the mount's view of it and the plan loop's
/// are two readings of that one snapshot, and the peer saw one `ArmingQuery`.
#[test]
fn the_arming_table_is_asked_once_under_the_trade_scope() {
    let peer = TradePeer::start(config(false));
    let backend = connect(&peer);
    let plan = backend.view(accounts(&[("binance", AccountTier::Demo)]), HashSet::new());
    let mount = backend
        .view(accounts(&[("binance", AccountTier::Demo)]), HashSet::from(["binance".to_string()]));
    for b in [&backend, &plan, &mount, &backend] {
        b.arming_table().expect("it answers");
    }
    let asks = peer.rpcs().iter().filter(|r| matches!(r, Request::ArmingQuery)).count();
    assert_eq!(asks, 1, "one ArmingQuery for the whole assembly");
    assert_eq!(peer.auths(), vec![(vike_hub_proto::auth::Scope::Trade, true)]);

    let row = |b: &RemoteBackend| {
        b.arming_table()
            .unwrap()
            .row_of_account("binance", &AccountLabel::Default)
            .expect("the daemon's row")
            .arming
            .clone()
    };
    assert_eq!(row(&plan).block, ArmingBlock::None);
    assert_eq!(row(&mount).block, ArmingBlock::NoCredentials, "the mount view applies data_only");
    assert!(backend.gateway_down().is_none());
}

/// **THE ORDER (R2): the lease is opened and answered before the adapter start is sent**, and the
/// start carries what the fold asked for: the symbol, the declared legs, the route key, the reconcile
/// verdict, the slippage band, and no withhold.
#[test]
fn the_lease_opens_before_the_adapter_starts() {
    let peer = TradePeer::start(config(false));
    let backend = connect(&peer);
    let (events, _lane) = fake_trade_peer::lane();
    let legs = vec!["ETHUSDT".to_string()];
    let label = AccountLabel::Default;
    let bind = backend
        .start_adapter(query(&legs, &label, None), &events, None)
        .expect("a healthy daemon starts the adapter");

    let log = peer.seen();
    let open_at = log.iter().position(|s| matches!(s, Seen::Open { .. })).expect("a TradeOpen");
    let start_at = log
        .iter()
        .position(|s| matches!(s, Seen::Up { frame: TradeUp::AdapterStart(_), .. }))
        .expect("an AdapterStart");
    assert!(open_at < start_at, "TradeOpen is answered BEFORE AdapterStart is sent: {log:?}");
    let Seen::Open { client_id, venue, account, tier, resume, takeover, .. } = &log[open_at] else {
        unreachable!()
    };
    assert_eq!(client_id, ID);
    assert_eq!((venue.as_str(), account.as_deref(), *tier), ("binance", None, AccountTier::Demo));
    assert_eq!((resume.is_none(), *takeover), (true, false), "no resume, no takeover in W11");

    let start = adapter_starts(&peer).pop().expect("one AdapterStart");
    assert_eq!(start.symbol, "BTCUSDT");
    assert_eq!(start.declared_legs, vec!["ETHUSDT".to_string()]);
    assert_eq!(start.route_key, "binance");
    assert!(start.recon_enabled && !start.takes_trigger && start.withhold.is_none());
    assert_eq!(start.market_slippage, Some(0.002));
    assert_eq!(bind.bound_tier, AccountTier::Demo);
    assert!(bind.recon.is_some(), "the daemon built a reconcile handle");
    assert!(bind.confirmed_book.is_none(), "the gatewayhub records the confirmed identity itself");
}

/// The client the fold drives is the REMOTE one: a submit through it is a frame the peer read.
#[test]
fn the_bound_client_is_the_remote_one() {
    let peer = TradePeer::start(config(false));
    let backend = connect(&peer);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let mut bind =
        backend.start_adapter(query(&[], &label, None), &events, None).expect("the adapter starts");
    bind.client.submit(&fake_trade_peer::order("coid-1"));
    let got = fake_trade_peer::wait_ups(&peer, Duration::from_secs(3), 1, |u| {
        matches!(u, TradeUp::Submit { .. })
    });
    assert_eq!(got.len(), 1, "the submit reached the gatewayhub");
}

/// **`AdapterStart.takes_trigger` states the venue's FACT; the link holds the manager's trigger for
/// EVERY venue** (W13b finding 7). The fact is what the daemon does with the bridge's own resync
/// supervisor; whether this client may ask for a pass when the gatewayhub's event stream has a hole
/// (`ReplayUnavailable`, a lost frame) or the daemon asks for one (`ReconcileNow`) does not depend on it.
/// (`gateway_restart_wire.rs` holds the gap path of a non-taker.)
#[test]
fn the_start_states_whether_the_bridge_takes_a_trigger_and_the_link_holds_the_trigger_either_way() {
    for takes in [false, true] {
        let peer = TradePeer::start(config(takes));
        let backend = connect(&peer);
        let (events, _lane) = fake_trade_peer::lane();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let label = AccountLabel::Default;
        let _bind = backend
            .start_adapter(query(&[], &label, None), &events, Some(tx))
            .expect("the adapter starts");
        let start = adapter_starts(&peer).pop().expect("one AdapterStart");
        assert_eq!(start.takes_trigger, takes, "the fact the daemon is told");
        assert!(peer.send(&TradeDown::ReconcileNow { reason: "test".to_string() }));
        assert!(
            rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "takes_recon_trigger = {takes}: the manager is poked"
        );
    }
}

/// A `data_only` venue's start says so, a preflight stop says so and wins, and neither is silent.
#[test]
fn a_withhold_travels_with_the_start() {
    let peer = TradePeer::start(config(false));
    let label = AccountLabel::Default;
    let (events, _lane) = fake_trade_peer::lane();
    let data_only = connect(&peer)
        .view(accounts(&[("binance", AccountTier::Demo)]), HashSet::from(["binance".to_string()]));
    let _ = data_only.start_adapter(query(&[], &label, None), &events, None);
    let _ =
        data_only.start_adapter(query(&[], &label, Some(Withhold::PreflightFailed)), &events, None);
    let seen: Vec<_> = adapter_starts(&peer).into_iter().map(|s| s.withhold).collect();
    assert_eq!(
        seen,
        vec![
            Some(vike_gatewayhub_client::proto::Withhold::DataOnly),
            Some(vike_gatewayhub_client::proto::Withhold::PreflightFailed)
        ],
        "data_only travels; a preflight stop, when both apply, is the one that names the cause"
    );
}

/// **A start the daemon answers `NotRunning` is a block, in the bridge's words in the log**; a word
/// this build does not know, or one that names a block which RUNS, is `adapter failed`.
#[test]
fn a_refusal_is_read_exhaustively_and_never_arms() {
    for (word, want) in [
        ("sdk_absent", ArmingBlock::SdkAbsent),
        ("sidecar_held_elsewhere", ArmingBlock::SidecarHeldElsewhere),
        ("a_cause_from_the_future", ArmingBlock::AdapterFailed),
        ("none", ArmingBlock::AdapterFailed),
        ("orders_held", ArmingBlock::AdapterFailed),
    ] {
        let word_owned = word.to_string();
        let peer = TradePeer::start(config(false).with_script(move |frame| match frame {
            TradeUp::AdapterStart(s) => vec![Step::Send(TradeDown::AdapterReply {
                rid: s.rid,
                reply: Box::new(AdapterReply::NotRunning {
                    block_word: word_owned.clone(),
                    refusal_text: "the bridge's own refusal words".to_string(),
                }),
            })],
            _ => Vec::new(),
        }));
        let backend = connect(&peer);
        let (events, _lane) = fake_trade_peer::lane();
        let label = AccountLabel::Default;
        let (got, logged) = vike_log::capture::captured(|| {
            backend.start_adapter(query(&[], &label, None), &events, None)
        });
        assert_eq!(got.unwrap_err(), BackendError::NotRunning(want), "word `{word}`");
        assert!(
            logged.iter().any(|e| e.message.contains("the bridge's own refusal words")),
            "`{word}`: the bridge's words reach THIS journal: {logged:?}"
        );
    }
}

/// **The bound the daemon answers is held to the account's own tier** (defence in depth: the
/// gatewayhub's interlock held it too). A client bound at `live` for a `demo` account is a live order
/// path nobody asked for; the account does NOT RUN and the lease is released.
#[test]
fn a_bound_tier_other_than_the_accounts_is_refused() {
    let peer = TradePeer::start(config(false).with_script(|frame| match frame {
        TradeUp::AdapterStart(s) => vec![Step::Send(TradeDown::AdapterReply {
            rid: s.rid,
            reply: Box::new(AdapterReply::Live {
                bound_tier: AccountTier::Live,
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
        _ => Vec::new(),
    }));
    let backend = connect(&peer);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let got = backend.start_adapter(query(&[], &label, None), &events, None);
    assert_eq!(got.unwrap_err(), BackendError::NotRunning(ArmingBlock::AdapterFailed));
    let got = fake_trade_peer::wait_ups(&peer, Duration::from_secs(3), 1, |u| {
        matches!(u, TradeUp::Detach { .. })
    });
    assert_eq!(got.len(), 1, "the lease of a refused account is released: {:?}", peer.ups());
}

/// A lease the daemon refuses for the account's own state is that state's word.
#[test]
fn a_refused_lease_is_the_daemons_word() {
    let peer = TradePeer::start(config(false));
    peer.refuse_open(Some(Response::TradeRefused {
        block_word: ArmingBlock::PreflightFailed.word().to_string(),
        text: "the tier does not match".to_string(),
    }));
    let backend = connect(&peer);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let got = backend.start_adapter(query(&[], &label, None), &events, None);
    assert_eq!(got.unwrap_err(), BackendError::NotRunning(ArmingBlock::PreflightFailed));
    assert!(adapter_starts(&peer).is_empty(), "no adapter start is sent over a refused lease");
}

/// **A lease another client holds refuses the whole mount**: the account does not run, and the
/// refusal text (the holder named, the remedy given) is what the root turns into a refused live
/// mount. No adapter start was sent for it.
#[test]
fn a_lease_another_client_holds_is_a_refusal_the_root_can_read() {
    let peer = TradePeer::start(config(false));
    peer.refuse_open(Some(Response::LeaseHeld {
        holder_key_id: "nk-other".to_string(),
        since_ms: 1_700_000_000_000,
    }));
    let backend = connect(&peer);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let got = backend.start_adapter(query(&[], &label, None), &events, None);
    assert_eq!(got.unwrap_err(), BackendError::NotRunning(ArmingBlock::AdapterFailed));
    let refusals = backend.lease_refusals();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    assert!(refusals[0].contains("nk-other") && refusals[0].contains("binance"), "{refusals:?}");
    assert!(adapter_starts(&peer).is_empty());
}

/// **A gatewayhub that is gone after the table was fetched is `Unavailable`** — which the fold turns
/// into `gatewayhub unavailable` for the account — and never a paper client.
#[test]
fn a_gatewayhub_that_vanishes_after_the_table_is_unavailable() {
    let peer = TradePeer::start(config(false));
    let backend = connect(&peer);
    drop(peer);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    match backend.start_adapter(query(&[], &label, None), &events, None) {
        Err(BackendError::Unavailable(why)) => assert!(why.contains("binance"), "{why}"),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

/// The boot bound the two silent-venue tests hand to `RemoteBackend::connect`.
const BOOT_BOUND: Duration = Duration::from_secs(1);

/// How long a silent peer says nothing (`Step::Sleep`, cut short when the peer drops): far beyond anything asserted.
const SILENT_PEER: Duration = Duration::from_secs(120);

/// The lease timings of the silent-venue tests, pinned instead of left to the defaults: a request would wait a
/// minute (the stand-in for the lease's 300 s, which a bounded start must NOT wait), and a detach flushes within
/// two seconds. A bounded start returns after the boot bound plus, at worst, that flush.
fn silent_venue_timings() -> vike_gatewayhub_client::trade::TradeTimings {
    vike_gatewayhub_client::trade::TradeTimings {
        request_reply: Duration::from_secs(60),
        detach_flush: Duration::from_secs(2),
        ..Default::default()
    }
}

/// The longest a bounded start of a silent venue may take on a LOADED box: the boot bound, the detach flush it
/// ends with, and ten seconds for the box (a loaded CI run measured 3.85 s against a 1 s bound). Still a sixth of
/// the minute a start that ignored the bound would wait, so the margin hides nothing.
const BOUNDED_START_CEILING: Duration = Duration::from_secs(13);

/// **ONE venue whose adapter start never answers does not hold the boot**: the start returns after the
/// bound (`config.gatewayhub_connect_wait_ms`, one second here, not the lease's wait), the account is
/// `NotRunning(AdapterFailed)` (never `Unavailable`, whose late arm would restart the tradehub into the
/// same silent venue), and the NEXT account's start binds. The fold's start loop is serial and the
/// control door listens only after it returns, so a bounded start is a door that listens.
#[test]
fn a_silent_venue_is_bounded_and_the_next_account_still_starts() {
    let default = fake_trade_peer::default_script();
    let peer = TradePeer::start(config(false).with_script(move |frame| match frame {
        vike_gatewayhub_client::proto::TradeUp::AdapterStart(s) if s.symbol == "HANGUSDT" => {
            vec![Step::Sleep(SILENT_PEER)]
        }
        other => default(other),
    }));
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[("binance", AccountTier::Demo)]),
        ID.to_string(),
        BOOT_BOUND,
    )
    .with_timings(silent_venue_timings());
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;

    let mut hung = query(&[], &label, None);
    hung.symbol = "HANGUSDT";
    let started = std::time::Instant::now();
    let got = backend.start_adapter(hung, &events, None);
    let took = started.elapsed();
    assert_eq!(got.unwrap_err(), BackendError::NotRunning(ArmingBlock::AdapterFailed));
    assert!(took >= Duration::from_millis(900), "it waited the bound: {took:?}");
    assert!(
        took < BOUNDED_START_CEILING,
        "and not the lease's wait or the peer's silence: {took:?}"
    );

    let bind = backend
        .start_adapter(query(&[], &label, None), &events, None)
        .expect("the next account is not held by the silent one");
    assert_eq!(bind.bound_tier, AccountTier::Demo);
}

/// **An account the watcher starts HOT (after boot) is a start like a boot one**: the `refreshed`
/// backend waits the boot bound for the adapter start, not the lease's 300 s, and the lease it
/// opens is registered in the SAME `LeaseFacts` the control door reads (`symbol_facts`), so
/// any-symbol finds it.
#[test]
fn a_refreshed_backend_keeps_the_boot_bound_and_the_shared_lease_facts() {
    let default = fake_trade_peer::default_script();
    let peer = TradePeer::start(config(false).with_script(move |frame| match frame {
        vike_gatewayhub_client::proto::TradeUp::AdapterStart(s) if s.symbol == "HANGUSDT" => {
            vec![Step::Sleep(SILENT_PEER)]
        }
        other => default(other),
    }));
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[("binance", AccountTier::Demo)]),
        ID.to_string(),
        BOOT_BOUND,
    )
    .with_timings(silent_venue_timings());
    let hot = backend.refreshed(accounts(&[("binance", AccountTier::Demo)]), HashSet::new());
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;

    let mut hung = query(&[], &label, None);
    hung.symbol = "HANGUSDT";
    let started = std::time::Instant::now();
    let got = hot.start_adapter(hung, &events, None);
    let took = started.elapsed();
    assert_eq!(got.unwrap_err(), BackendError::NotRunning(ArmingBlock::AdapterFailed));
    assert!(took >= Duration::from_millis(900), "it waited the boot bound: {took:?}");
    assert!(took < BOUNDED_START_CEILING, "the boot bound, not the lease's wait: {took:?}");

    hot.start_adapter(query(&[], &label, None), &events, None).expect("a healthy daemon binds");
    for facts in [hot.symbol_facts(), backend.symbol_facts()] {
        assert!(
            facts.admission("binance", "binance", "SOLUSDT.P").is_ok(),
            "the hot-started lease is visible to the control door"
        );
    }
}

/// **A gatewayhub that never answers is waited for, then given up on — boundedly.** One second of
/// wait on an address nothing listens on: the start returns after about that, every demo account is
/// `gatewayhub unavailable`, and the table still carries the paper account.
#[test]
fn an_absent_gatewayhub_is_waited_for_boundedly() {
    let started = std::time::Instant::now();
    let backend = RemoteBackend::connect(
        link_to(&fake_trade_peer::dead_addr()),
        accounts(&[("binance", AccountTier::Demo), ("bybit", AccountTier::Paper)]),
        ID.to_string(),
        Duration::from_secs(1),
    );
    let waited = started.elapsed();
    assert!(waited >= Duration::from_secs(1), "it waited the bound: {waited:?}");
    // Ten seconds of margin for a loaded box on top of the one-second wait (a loaded run took 3.85 s for a 1 s bound).
    assert!(waited < Duration::from_secs(11), "…and not much more: {waited:?}");
    assert!(backend.gateway_down().is_some());
    let table = backend.arming_table().expect("it answers");
    let at =
        |v: &str| table.row_of_account(v, &AccountLabel::Default).expect("a row").arming.clone();
    assert_eq!(at("binance").block, ArmingBlock::GatewayUnavailable);
    assert_eq!(at("bybit").block, ArmingBlock::PaperTier);
}

/// The identity record rides the open lease, after the start; without a lease it is `Unavailable`
/// (the fold logs a warning and the start is unaffected).
#[test]
fn the_identity_record_rides_the_open_lease() {
    let peer = TradePeer::start(config(false));
    let backend = connect(&peer);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let q = IdentityRecord {
        venue: "binance",
        account: &label,
        bound_tier: AccountTier::Demo,
        confirmed: None,
    };
    assert!(matches!(backend.record_identity(&q, None), Err(BackendError::Unavailable(_))));
    let bind = backend.start_adapter(query(&[], &label, None), &events, None).expect("starts");
    backend.record_identity(&q, bind.recon.as_deref()).expect("the daemon records it");
    assert!(peer.ups().iter().any(|u| matches!(u, TradeUp::IdentityRecord { .. })));
}

/// A daemon that answers the four preflight questions of a binance demo box from fixed legs.
fn legs_config(
    clock: LegResult,
    credentials: Vec<LegResult>,
    network: LegResult,
    plan: PreflightLists,
) -> TradePeerConfig {
    config(false).with_rpc(move |req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::new(ArmingTable {
            facts: BTreeMap::from([("binance".to_string(), facts(false))]),
            rows: vec![binance_row(AccountTier::Demo)],
            key_changed: Vec::new(),
        }))],
        Request::PreflightQuery { step: PreflightStep::Plan, .. } => {
            vec![Response::PreflightLegs { legs: Vec::new(), plan: Some(plan.clone()) }]
        }
        Request::PreflightQuery { step: PreflightStep::Clock, .. } => {
            vec![Response::PreflightLegs { legs: vec![clock.clone()], plan: None }]
        }
        Request::PreflightQuery { step: PreflightStep::Credential, .. } => {
            vec![Response::PreflightLegs { legs: credentials.clone(), plan: None }]
        }
        Request::PreflightQuery { step: PreflightStep::Network, .. } => {
            vec![Response::PreflightLegs { legs: vec![network.clone()], plan: None }]
        }
        other => vec![Response::Error(format!("unscripted: {other:?}"))],
    })
}

fn binance_plan() -> PreflightLists {
    PreflightLists {
        clock_risks: BTreeMap::from([(
            "binance".to_string(),
            ClockRiskInfo {
                class: ClockRiskClass::SignedTimestamp,
                remedy: "the daemon's own remedy sentence".to_string(),
            },
        )]),
        wired_clock_reads: 1,
        credential_venues: vec!["binance".to_string()],
    }
}

/// The preflight steps the peer was asked, in order, with the venues each named.
fn preflight_asked(peer: &TradePeer) -> Vec<(PreflightStep, Vec<String>)> {
    peer.rpcs()
        .into_iter()
        .filter_map(|r| match r {
            Request::PreflightQuery { venues, step } => Some((step, venues)),
            _ => None,
        })
        .collect()
}

/// **The preflight is ONE QUESTION PER READ and the fold owns the schedule**: the plan crosses first
/// (the venue lists and each wired venue's policy, with the daemon's remedy), then every clock read,
/// credential read and network reading is its own question, so a resample is a second question. The
/// clock stamp is the venue's own, and the worst of a venue's accounts decides its credential leg.
#[test]
fn the_preflight_crosses_as_one_question_per_read_and_reads_back() {
    const STAMP: i64 = 1_700_000_000_123;
    let peer = TradePeer::start(legs_config(
        LegResult::Clock {
            venue: "binance".to_string(),
            leg: ClockLeg::ServerTime { server_ms: STAMP, local_ms: STAMP + 2, rtt_ms: 12 },
            reason: None,
            at_stake: None,
        },
        vec![
            LegResult::Credential {
                venue: "binance".to_string(),
                label: None,
                leg: CredentialLeg::Accepted,
            },
            LegResult::Credential {
                venue: "binance".to_string(),
                label: Some("ALT".to_string()),
                leg: CredentialLeg::Rejected("-2015 invalid key".to_string()),
            },
        ],
        LegResult::Network { reading: NetReading::Alive, rounds: Some(7), since_ms: None },
        binance_plan(),
    ));
    let backend = connect(&peer);
    let venues = vec!["binance".to_string()];
    let plan = backend
        .preflight_plan(&PreflightQuery { clock_venues: &venues })
        .expect("the plan comes back");
    assert_eq!(plan.credential_venues, vec!["binance".to_string()]);
    assert_eq!(plan.wired_clock_reads, 1);
    let policy = &plan.clock_policies["binance"];
    assert_eq!(policy.fail_ms, Some(vike_tradehub_core::preflight::DEFAULT_CLOCK_FAIL_MS));
    assert_eq!(policy.remedy, "the daemon's own remedy sentence");
    assert_eq!(preflight_asked(&peer), vec![(PreflightStep::Plan, venues.clone())], "no read yet");

    // The stamp is the venue's own, and every read is a question of its own.
    assert_eq!(backend.server_time_ms("binance", AccountTier::Demo), Ok(STAMP));
    assert_eq!(backend.server_time_ms("binance", AccountTier::Demo), Ok(STAMP));
    assert_eq!(
        backend.authed_read("binance"),
        Err(CredentialGap::Rejected("-2015 invalid key".to_string())),
        "the refused account decides"
    );
    assert_eq!(backend.net_reading(), FoldNetReading::Alive { rounds: 7 });
    assert_eq!(
        preflight_asked(&peer),
        vec![
            (PreflightStep::Plan, venues.clone()),
            (PreflightStep::Clock, venues.clone()),
            (PreflightStep::Clock, venues.clone()),
            (PreflightStep::Credential, venues.clone()),
            (PreflightStep::Network, Vec::new()),
        ],
        "a resample is a second question; nothing is cached"
    );
}

/// **A declared gap carries its reason across, and the network its numbers.**
#[test]
fn a_declared_gap_and_the_network_numbers_cross() {
    let peer = TradePeer::start(legs_config(
        LegResult::Clock {
            venue: "binance".to_string(),
            leg: ClockLeg::UnmeasuredRisk,
            reason: Some("binance publishes no clock in this test".to_string()),
            at_stake: Some("its signature binds the timestamp".to_string()),
        },
        Vec::new(),
        LegResult::Network {
            reading: NetReading::Dead("dns".to_string()),
            rounds: None,
            since_ms: Some(1_699_999_990_123),
        },
        binance_plan(),
    ));
    let backend = connect(&peer);
    let venues = vec!["binance".to_string()];
    backend.preflight_plan(&PreflightQuery { clock_venues: &venues }).expect("a plan");
    assert_eq!(
        backend.server_time_ms("binance", AccountTier::Demo),
        Err(ServerTimeGap::UnmeasuredRisk {
            reason: "binance publishes no clock in this test",
            at_stake: "its signature binds the timestamp",
        })
    );
    assert_eq!(backend.net_reading(), FoldNetReading::Dead { since_ms: 1_699_999_990_123 });
}

/// **A preflight the daemon cannot answer is a WARN row for every venue, never a green one and
/// never a dropped leg**: the plan still lists the armed venues, and each read says it did not
/// answer - whether the PLAN failed or only a later READ did.
#[test]
fn a_preflight_query_that_fails_leaves_every_leg_unanswered() {
    let peer = TradePeer::start(config(false).with_rpc(|req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::new(ArmingTable {
            facts: BTreeMap::from([("binance".to_string(), facts(false))]),
            rows: vec![binance_row(AccountTier::Demo)],
            key_changed: Vec::new(),
        }))],
        _ => vec![Response::Error("the preflight lane is down".to_string())],
    }));
    let backend = connect(&peer);
    let venues = vec!["binance".to_string()];
    let plan = backend.preflight_plan(&PreflightQuery { clock_venues: &venues }).expect("a plan");
    assert_eq!(
        plan.credential_venues,
        vec!["binance".to_string()],
        "the armed venue still gets a row"
    );
    assert_eq!(plan.wired_clock_reads, 1, "the facts say binance's clock is wired");
    assert!(plan.clock_policies.is_empty(), "no policy was told");
    assert!(matches!(
        backend.server_time_ms("binance", AccountTier::Demo),
        Err(ServerTimeGap::Unreachable(why)) if why.contains("preflight lane is down")
    ));
    assert!(matches!(
        backend.authed_read("binance"),
        Err(CredentialGap::Unanswered { detail, .. }) if detail.contains("preflight lane is down")
    ));
    assert_eq!(backend.net_reading(), FoldNetReading::Unknown);
    assert_eq!(
        preflight_asked(&peer).len(),
        1,
        "a plan that failed latches: the reads are not asked of a gatewayhub that did not answer"
    );
}

/// A READ the daemon fails after a good plan is a gap for THAT read only.
#[test]
fn a_read_that_fails_after_a_good_plan_is_a_gap_for_that_read() {
    let peer = TradePeer::start(config(false).with_rpc(|req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::new(ArmingTable {
            facts: BTreeMap::from([("binance".to_string(), facts(false))]),
            rows: vec![binance_row(AccountTier::Demo)],
            key_changed: Vec::new(),
        }))],
        Request::PreflightQuery { step: PreflightStep::Plan, .. } => {
            vec![Response::PreflightLegs { legs: Vec::new(), plan: Some(binance_plan()) }]
        }
        _ => vec![Response::Error("the read lane is down".to_string())],
    }));
    let backend = connect(&peer);
    let venues = vec!["binance".to_string()];
    backend.preflight_plan(&PreflightQuery { clock_venues: &venues }).expect("a plan");
    assert!(matches!(
        backend.server_time_ms("binance", AccountTier::Demo),
        Err(ServerTimeGap::Unreachable(why)) if why.contains("read lane is down")
    ));
    assert!(matches!(backend.authed_read("binance"), Err(CredentialGap::Unanswered { .. })));
    assert_eq!(backend.net_reading(), FoldNetReading::Unknown);
}

/// The token the fake peer states in every `TradeOpened` (`fake_trade_peer.rs`).
const PEER_PROJECT_TOKEN: &str = "project-token-of-the-tests";

/// A project state directory whose `gateway-id` holds `token` (`None`: the file is absent).
fn project_dir(token: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temp project state dir");
    if let Some(token) = token {
        // A trailing newline, as an operator's `echo` would leave: the comparison trims.
        std::fs::write(
            dir.path().join(vike_gatewayhub::trade::state_dir::PROJECT_TOKEN_FILE),
            format!("{token}\n"),
        )
        .expect("write the token");
    }
    dir
}

/// **The project token is compared (review 15, finding 7).** A tradehub whose project state directory holds
/// the gatewayhub's token starts its account; one whose token DIFFERS, or whose directory holds none, or
/// which resolved no project at all, is refused: the account is NOT RUNNING (`adapter failed`), no
/// `AdapterStart` is sent and the lease is detached. The HALT sentinel, the journals and the live-account
/// locks hang off the project, so two processes on two projects must not trade.
#[test]
fn the_gatewayhubs_project_token_must_be_the_tradehubs() {
    let label = AccountLabel::Default;
    let try_start = |state_dir: Option<std::path::PathBuf>| {
        let peer = TradePeer::start(config(false));
        let backend = connect(&peer).with_project_state_dir(state_dir);
        let (events, _lane) = fake_trade_peer::lane();
        let result = backend.start_adapter(query(&[], &label, None), &events, None);
        (result.map(|bind| bind.bound_tier), adapter_starts(&peer).len())
    };

    let same = project_dir(Some(PEER_PROJECT_TOKEN));
    let (started, sent) = try_start(Some(same.path().to_path_buf()));
    assert_eq!((started.ok(), sent), (Some(AccountTier::Demo), 1), "one project: it starts");

    let other = project_dir(Some("another-projects-token"));
    let absent = project_dir(None);
    for (why, dir) in [
        ("a different token", Some(other.path().to_path_buf())),
        ("no token file", Some(absent.path().to_path_buf())),
        ("no project at all", None),
    ] {
        let (started, sent) = try_start(dir);
        assert!(
            matches!(started, Err(BackendError::NotRunning(ArmingBlock::AdapterFailed))),
            "{why}: the account is NOT RUNNING, got {started:?}"
        );
        assert_eq!(sent, 0, "{why}: nothing was started at the gatewayhub");
    }
}

/// A backend that was never told its project asks nothing (the scripted peers of the other tests carry a
/// token of their own and no project).
#[test]
fn a_backend_with_no_project_check_starts_whatever_token_the_peer_states() {
    let peer = TradePeer::start(config(false));
    let backend = connect(&peer);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    assert!(backend.start_adapter(query(&[], &label, None), &events, None).is_ok());
}

// ---- ibkr: what the start asks for and what the ledger is anchored to (decision 0183) --------------------

/// A fake gatewayhub whose ibkr adapter comes up stating `stated` as the account's currency (`None`: it could
/// not read it right now).
fn ibkr_gateway(stated: Option<&'static str>) -> TradePeer {
    gateway_of("ibkr", stated)
}

/// [`ibkr_gateway`] for any venue.
fn gateway_of(venue: &'static str, stated: Option<&'static str>) -> TradePeer {
    gateway_replying(venue, AccountTier::Demo, stated, false)
}

/// [`gateway_of`] for a lease bound at `tier`, whose reply says `assumed` about the currency it states (the
/// gatewayhub's own USD default, `account_ccy_assumed` on the wire).
fn gateway_replying(
    venue: &'static str,
    tier: AccountTier,
    stated: Option<&'static str>,
    assumed: bool,
) -> TradePeer {
    let default = fake_trade_peer::default_script();
    let mut daemon = config(false);
    // The daemon plays that venue's: it serves its trade lane and names that account's ring.
    daemon.route_key = venue.to_string();
    daemon.features.push(vike_gatewayhub_client::proto::trade_venue_feature(venue));
    TradePeer::start(daemon.with_script(move |frame| match frame {
        TradeUp::AdapterStart(s) => vec![Step::Send(TradeDown::AdapterReply {
            rid: s.rid,
            reply: Box::new(AdapterReply::Live {
                bound_tier: tier,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
                recon: false,
                leverage_cap: None,
                account_ccy: stated.map(str::to_string),
                account_ccy_assumed: assumed,
            }),
        })],
        other => default(other),
    }))
}

fn ibkr_backend(
    peer: &TradePeer,
    ceiling: bool,
    hints: vike_tradehub::remote_backend::restore::BootHints,
) -> RemoteBackend {
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[("ibkr", AccountTier::Demo)]),
        ID.to_string(),
        Duration::from_secs(5),
    )
    .with_boot(hints);
    backend.symbol_facts().bind_exposure_cap(move |_| ceiling);
    backend
}

/// One ibkr start on a fresh fake gatewayhub that states `stated`: the currency the engine is anchored to.
fn anchored_by(
    stated: Option<&'static str>,
    ceiling: bool,
    hints: vike_tradehub::remote_backend::restore::BootHints,
    mounted: &str,
) -> Option<String> {
    let peer = ibkr_gateway(stated);
    ibkr_start(&ibkr_backend(&peer, ceiling, hints), mounted)
}

fn ibkr_start(backend: &RemoteBackend, mounted: &str) -> Option<String> {
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let query = AdapterStartQuery {
        venue: "ibkr",
        account: &label,
        tier: AccountTier::Demo,
        symbol: mounted,
        declared_legs: &[],
        recon_enabled: false,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: "ibkr",
        properties_rec: None,
    };
    backend.start_adapter(query, &events, None).expect("the ibkr adapter comes up").account_ccy
}

fn journaled_usd() -> vike_tradehub::remote_backend::restore::BootHints {
    use vike_tradehub::remote_backend::restore::{BootHints, JournaledFx};
    BootHints::none().with_fx(
        "ibkr",
        JournaledFx {
            account_ccy: "USD".to_string(),
            symbols: vec![("SAP.SMART.EUR".to_string(), "EUR".to_string())],
        },
    )
}

/// **An ibkr start asks the gatewayhub to read the account's currency only when the account will convert**: a
/// demo account with an exposure ceiling armed. The read is a dial of the data lane's scarce budget (four an
/// hour), so an account with no ceiling asks nothing at its start and its `AdapterStart` frame is the old frame,
/// byte for byte (`read_account_ccy` is not written while false).
#[test]
fn an_ibkr_start_asks_for_the_currency_only_with_a_ceiling() {
    use vike_tradehub::remote_backend::restore::BootHints;
    for ceiling in [true, false] {
        let peer = ibkr_gateway(None);
        let backend = ibkr_backend(&peer, ceiling, BootHints::none());
        // none stated, no journal: USD (assumed) for the account that converts, nothing for the one that does not
        assert_eq!(
            ibkr_start(&backend, "AAPL.SMART.USD").as_deref(),
            ceiling.then_some("USD"),
            "ceiling {ceiling}"
        );
        let starts = adapter_starts(&peer);
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].read_account_ccy, ceiling, "ceiling {ceiling}");
        let wire = serde_json::to_string(&starts[0]).unwrap();
        assert_eq!(wire.contains("read_account_ccy"), ceiling, "{wire}");
    }
}

/// **A start whose gatewayhub could not read the currency anchors to the journal's**, for an account that
/// converts: the restored engine's rates are expressed in it, and an engine assembled without it could not take
/// a fresh rate for a held position (the restart finding: the data lane's dial is most likely refused exactly
/// then). Said at `warn`. The venue's own word wins over the journal's, the mounted contract's money still has
/// to agree, and an account with no ceiling takes nothing from the journal.
#[test]
fn a_start_that_could_not_read_the_currency_anchors_to_the_journals() {
    use vike_tradehub::remote_backend::restore::BootHints;
    // unread + a journal + a ceiling: anchored to the journal's currency, and the line says so
    let (anchor, logged) =
        vike_log::capture::captured(|| anchored_by(None, true, journaled_usd(), "AAPL.SMART.USD"));
    assert_eq!(anchor.as_deref(), Some("USD"));
    // the journal names the default, so it only records the previous run's assumption (review 2, MEDIUM-1): the
    // same assumed USD, said as one (a journal naming another money is the journal's word, said as that)
    assert!(
        logged.iter().any(|e| e.message.contains("the venue stated no currency for the account")
            && e.message.contains("assumed USD")),
        "{logged:?}"
    );
    // the mounted contract counts money in another currency than the journal's: not anchored
    assert_eq!(anchored_by(None, true, journaled_usd(), "SAP.SMART.EUR"), None);
    // no ceiling: the journal is not consulted
    assert_eq!(anchored_by(None, false, journaled_usd(), "AAPL.SMART.USD"), None);
    // no journal, nothing stated: USD, the owner's default (it was "nothing to anchor to" until the ruling)
    assert_eq!(
        anchored_by(None, true, BootHints::none(), "AAPL.SMART.USD").as_deref(),
        Some("USD")
    );
    // the venue's word wins over the journal's
    assert_eq!(
        anchored_by(Some("EUR"), true, journaled_usd(), "SAP.SMART.EUR").as_deref(),
        Some("EUR")
    );
}

/// One start of `venue`'s default demo account mounted on `mounted` against `peer`, the exposure ceiling `ceiling`
/// and the boot's `hints`: the currency the engine is to be anchored to.
fn start_anchor(
    peer: &TradePeer,
    venue: &'static str,
    mounted: &str,
    ceiling: bool,
    hints: vike_tradehub::remote_backend::restore::BootHints,
) -> Option<String> {
    start_bind(peer, venue, mounted, ceiling, hints).account_ccy
}

/// [`start_anchor`], the whole bind: the currency AND whether it is only ASSUMED.
fn start_bind(
    peer: &TradePeer,
    venue: &'static str,
    mounted: &str,
    ceiling: bool,
    hints: vike_tradehub::remote_backend::restore::BootHints,
) -> vike_tradehub_core::AdapterBind {
    start_bind_at(peer, venue, AccountTier::Demo, mounted, ceiling, hints)
}

/// [`start_bind`] for an account at `tier` (the peer must bind that tier).
fn start_bind_at(
    peer: &TradePeer,
    venue: &'static str,
    tier: AccountTier,
    mounted: &str,
    ceiling: bool,
    hints: vike_tradehub::remote_backend::restore::BootHints,
) -> vike_tradehub_core::AdapterBind {
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[(venue, tier)]),
        ID.to_string(),
        Duration::from_secs(5),
    )
    .with_boot(hints);
    backend.symbol_facts().bind_exposure_cap(move |_| ceiling);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let query = AdapterStartQuery {
        venue,
        account: &label,
        tier,
        symbol: mounted,
        declared_legs: &[],
        recon_enabled: false,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: venue,
        properties_rec: None,
    };
    backend.start_adapter(query, &events, None).expect("the adapter comes up")
}

/// **A USD the gatewayhub filled in for a silent venue is the venue's SILENCE, so the journal outranks it**
/// (venue > journal > USD assumed, whoever assumed it): with a journal that names a currency the mount is
/// quoted in, the journal's word is the anchor and is not flagged; with none, the gatewayhub's assumption stands,
/// flagged assumed. (FXCM's lane has always assumed USD this way; every converting venue's does now.)
#[test]
fn a_gatewayhubs_assumed_usd_is_the_venues_silence_and_the_journal_outranks_it() {
    use vike_tradehub::remote_backend::restore::{BootHints, JournaledFx};
    let journal = |ccy: &str| {
        BootHints::none().with_fx(
            "oanda",
            JournaledFx {
                account_ccy: ccy.to_string(),
                symbols: vec![("EURGBP".into(), "GBP".into())],
            },
        )
    };
    let seen = |mount: &str, hints| {
        let peer = gateway_replying("oanda", AccountTier::Demo, Some("USD"), true);
        let bind = start_bind(&peer, "oanda", mount, true, hints);
        (bind.account_ccy, bind.account_ccy_assumed)
    };
    assert_eq!(seen("EURGBP", journal("GBP")), (Some("GBP".to_string()), false), "the journal's");
    assert_eq!(seen("EURUSD", BootHints::none()), (Some("USD".to_string()), true), "the lane's");
    // a journal that only names the default records the earlier assumption: still the assumption (MEDIUM-1)
    assert_eq!(
        seen("EURUSD", journal("usd")),
        (Some("USD".to_string()), true),
        "the same assumption"
    );
}

/// **A currency the venue STATES is never turned into "USD, assumed", whatever the mount is quoted in** (review 2,
/// HIGH-2, and review 1's dukascopy HIGH-1): the gatewayhub tells EUR, unflagged, for the default account mounted on
/// the USD-quoted `EURUSD`. Every venue whose lane rates its mount (oanda, ctrader, fxcm: decision 0183's
/// mounted-symbol amendment; dukascopy and ig since the non-USD change) anchors the STATED EUR, the mount declared
/// foreign-quoted: the stated word again, never a default, and not the "assuming USD" line, because nothing was
/// assumed. (The lane used to hide a stated EUR from the node on fxcm; the node then defaulted. A venue outside the
/// mount list keeps the engine's mount fence and reports NO anchor: `leverage_tests.rs` holds that row.)
#[test]
fn a_stated_eur_on_a_usd_quoted_mount_is_anchored_or_declined_never_defaulted_to_usd() {
    use vike_tradehub::remote_backend::restore::BootHints;
    for (venue, mount) in [
        ("oanda", "EURUSD"),
        ("ctrader", "EURUSD"),
        ("dukascopy", "EURUSD"),
        ("fxcm", "EURUSD"),
        ("ig", "CS.D.EURUSD.MINI.IP"),
    ] {
        let peer = gateway_replying(venue, AccountTier::Demo, Some("EUR"), false);
        let (bind, logged) = vike_log::capture::captured(|| {
            start_bind(&peer, venue, mount, true, BootHints::none())
        });
        assert_eq!(
            (bind.account_ccy.as_deref(), bind.account_ccy_assumed),
            (Some("EUR"), false),
            "{venue}"
        );
        assert!(
            logged.iter().all(|e| !e.message.contains("assuming")),
            "{venue}: nothing was assumed: {logged:?}"
        );
    }
}

/// A fake gatewayhub whose adapter comes up WITH a reconcile client, states `stated` (`assumed`: its own USD
/// default) and answers every balance request with 25,000: whatever the node does NOT hand to the engine is the
/// node's decision, the gatewayhub's own gate (`TradeLane::balance_withheld`) is not modelled here.
fn balance_gateway(venue: &'static str, stated: Option<&'static str>, assumed: bool) -> TradePeer {
    use vike_gatewayhub_client::proto::{ReconKind, ReconPayload};
    let default = fake_trade_peer::default_script();
    let mut daemon = config(false);
    daemon.route_key = venue.to_string();
    daemon.features.push(vike_gatewayhub_client::proto::trade_venue_feature(venue));
    TradePeer::start(daemon.with_script(move |frame| match frame {
        TradeUp::AdapterStart(s) => vec![Step::Send(TradeDown::AdapterReply {
            rid: s.rid,
            reply: Box::new(AdapterReply::Live {
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
                recon: true,
                leverage_cap: None,
                account_ccy: stated.map(str::to_string),
                account_ccy_assumed: assumed,
            }),
        })],
        TradeUp::Recon { rid, kind: ReconKind::Balance } => {
            vec![Step::Send(TradeDown::ReconReply {
                rid: *rid,
                page: 0,
                more: false,
                payload: ReconPayload::Balance(Some(25_000.0)),
                read: None,
            })]
        }
        other => default(other),
    }))
}

/// One start of `venue`'s default demo account mounted on `mounted` against `peer` WITH its reconcile client, the
/// exposure ceiling `ceiling` and the boot's `hints`: what the engine's reconcile balance is, as the core's
/// reconcile manager would ask it (`None` = "not reported").
fn balance_of(
    peer: &TradePeer,
    venue: &'static str,
    mounted: &str,
    ceiling: bool,
    hints: vike_tradehub::remote_backend::restore::BootHints,
) -> Result<Option<f64>, String> {
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[(venue, AccountTier::Demo)]),
        ID.to_string(),
        Duration::from_secs(5),
    )
    .with_boot(hints);
    backend.symbol_facts().bind_exposure_cap(move |_| ceiling);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let query = AdapterStartQuery {
        venue,
        account: &label,
        tier: AccountTier::Demo,
        symbol: mounted,
        declared_legs: &[],
        recon_enabled: true,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: venue,
        properties_rec: None,
    };
    let bind = backend.start_adapter(query, &events, None).expect("the adapter comes up");
    bind.recon.expect("the gatewayhub has a reconcile client").fetch_balance()
}

/// **The NODE decides whether a balance is handed to the engine, from how the engine was bound** (third review,
/// HIGH-B and S7): an oanda, ig or fxcm balance is the account's cash with no currency beside it, so it is adopted
/// only into a ledger anchored to the currency the venue STATED. Held back (`None`): an assumed USD (silent
/// venue, a failed read or the gatewayhub's own default), a stated currency the mount fence declines to anchor (a
/// venue whose lane does not rate its mount: dukascopy, ig) and a stated non-USD currency on an account that does
/// not convert. Handed on: a stated currency the ledger is anchored to (the good path: no residue; since decision
/// 0183's mounted-symbol amendment that includes EUR over the USD-quoted `EURUSD` of oanda, ctrader and fxcm, whose
/// mount is declared foreign-quoted), and a stated USD on any account.
#[test]
fn a_balance_is_adopted_only_into_a_ledger_anchored_to_the_currency_the_venue_stated() {
    use vike_tradehub::remote_backend::restore::BootHints;
    let seen = |venue: &'static str, stated, assumed, mount: &str, ceiling| {
        balance_of(
            &balance_gateway(venue, stated, assumed),
            venue,
            mount,
            ceiling,
            BootHints::none(),
        )
    };
    let (held, adopted) = (Ok(None), Ok(Some(25_000.0)));
    for venue in ["oanda", "fxcm"] {
        // the venue states USD and the ledger is anchored to it: adopted (S7, no residue)
        assert_eq!(seen(venue, Some("USD"), false, "EURUSD", true), adopted, "{venue}: stated USD");
        // ...and so is a stated USD with no ceiling (nothing to disagree with)
        assert_eq!(
            seen(venue, Some("USD"), false, "EURUSD", false),
            adopted,
            "{venue}: no ceiling"
        );
        // a stated EUR on a EUR-quoted mount anchors the ledger to EUR: adopted
        assert_eq!(
            seen(venue, Some("EUR"), false, "GBPEUR", true),
            adopted,
            "{venue}: EUR anchored"
        );
        // the venue said nothing (the node assumes USD), or the gatewayhub assumed it: held
        assert_eq!(seen(venue, None, false, "EURUSD", true), held, "{venue}: silent");
        assert_eq!(seen(venue, Some("USD"), true, "EURUSD", true), held, "{venue}: lane-assumed");
        // a stated EUR over the USD-quoted `EURUSD`: FLIPPED by decision 0183's mounted-symbol amendment (this row
        // was `held`, the mount fence's decline: S3 and its oanda twin). The ledger is anchored to EUR and the
        // mount declared foreign-quoted, so the venue's cash is the ledger's and nothing is held.
        assert_eq!(
            seen(venue, Some("EUR"), false, "EURUSD", true),
            adopted,
            "{venue}: a declared mount"
        );
        // a stated EUR on an account that does not convert: nothing anchors it, held
        assert_eq!(seen(venue, Some("EUR"), false, "EURUSD", false), held, "{venue}: no ceiling");
    }
    // ig: the same rule (a USD account; a non-USD one that converts does not run at all)
    assert_eq!(
        seen("ig", Some("USD"), false, "CS.D.EURUSD.MINI.IP", true),
        adopted,
        "ig: stated USD"
    );
    assert_eq!(seen("ig", None, false, "CS.D.EURUSD.MINI.IP", true), held, "ig: silent");
    // the venues that fence themselves are handed on whatever the binding says (their gateway-side clients
    // withhold what they must): ctrader's parse_balance, dukascopy's missing leg, ibkr's own client
    for (venue, mount) in
        [("ctrader", "EURUSD"), ("dukascopy", "EURUSD"), ("ibkr", "AAPL.SMART.USD")]
    {
        assert_eq!(seen(venue, None, false, mount, true), adopted, "{venue}: silent, pinned");
        assert_eq!(
            seen(venue, Some("EUR"), false, mount, true),
            adopted,
            "{venue}: declined, pinned"
        );
    }
}

/// One start of `venue`'s default demo account mounted on `mounted` against `peer` WITH its reconcile client (as
/// [`balance_of`]), handing back the backend (its lease registry is what the operator surfaces read) and the bind.
fn start_with_recon(
    peer: &TradePeer,
    venue: &'static str,
    mounted: &str,
    ceiling: bool,
    hints: vike_tradehub::remote_backend::restore::BootHints,
) -> (RemoteBackend, vike_tradehub_core::AdapterBind) {
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[(venue, AccountTier::Demo)]),
        ID.to_string(),
        Duration::from_secs(5),
    )
    .with_boot(hints);
    backend.symbol_facts().bind_exposure_cap(move |_| ceiling);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let query = AdapterStartQuery {
        venue,
        account: &label,
        tier: AccountTier::Demo,
        symbol: mounted,
        declared_legs: &[],
        recon_enabled: true,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: venue,
        properties_rec: None,
    };
    let bind = backend.start_adapter(query, &events, None).expect("the adapter comes up");
    (backend, bind)
}

/// **The operator is told what the node holds back, through the REAL start**: after a start whose binding holds
/// the balance, the lease registry (the source of every frame's `balance_held`, and so of `trade equity`, `trade
/// status`, `trade doctor` and the Telegram `/equity`) lists the account with the short reason; an anchored start
/// lists nothing, and a venue that fences itself lists nothing whatever its binding. The held cases are exactly the
/// ones whose balance [`a_balance_is_adopted_only_into_a_ledger_anchored_to_the_currency_the_venue_stated`] sees
/// withheld.
#[test]
fn a_held_balance_is_listed_for_the_operator_through_the_real_start() {
    use vike_tradehub::remote_backend::restore::BootHints;
    let held = |venue: &'static str, stated, assumed, mount: &str, ceiling| {
        let peer = balance_gateway(venue, stated, assumed);
        let (backend, _bind) = start_with_recon(&peer, venue, mount, ceiling, BootHints::none());
        backend.symbol_facts().balance_held()
    };
    let row = |venue: &str, reason: &str| vec![(venue.to_string(), reason.to_string())];
    for venue in ["oanda", "fxcm"] {
        let nothing = Vec::<(String, String)>::new();
        assert_eq!(held(venue, Some("USD"), false, "EURUSD", true), nothing, "{venue}: stated USD");
        assert_eq!(
            held(venue, Some("EUR"), false, "GBPEUR", true),
            nothing,
            "{venue}: EUR anchored"
        );
        assert_eq!(held(venue, None, false, "EURUSD", true), row(venue, "assumed USD"), "{venue}");
        assert_eq!(
            held(venue, Some("USD"), true, "EURUSD", true),
            row(venue, "assumed USD"),
            "{venue}: lane-assumed"
        );
        // FLIPPED by decision 0183's mounted-symbol amendment (was `stated EUR not anchored`, the mount fence's
        // decline): the ledger is anchored to the stated EUR and the mount declared, so no balance is held.
        assert_eq!(
            held(venue, Some("EUR"), false, "EURUSD", true),
            nothing,
            "{venue}: a declared mount"
        );
        assert_eq!(
            held(venue, Some("EUR"), false, "EURUSD", false),
            row(venue, "stated EUR not anchored"),
            "{venue}: an account that does not convert"
        );
    }
    assert_eq!(held("ig", None, false, "CS.D.EURUSD.MINI.IP", true), row("ig", "assumed USD"));
    for (venue, mount) in
        [("ctrader", "EURUSD"), ("dukascopy", "EURUSD"), ("ibkr", "AAPL.SMART.USD")]
    {
        assert!(held(venue, None, false, mount, true).is_empty(), "{venue} fences itself");
    }
}

/// **ibkr's untold-stock hold is listed once a reconcile pass finds a journaled foreign symbol owed**, not before
/// (the gate learns of it when it is asked), and a start with none owed lists nothing.
#[test]
fn an_ibkr_untold_stock_hold_is_listed_once_a_pass_finds_it() {
    use vike_tradehub::remote_backend::restore::BootHints;
    let peer = balance_gateway("ibkr", Some("USD"), false);
    let (backend, bind) = start_with_recon(&peer, "ibkr", "AAPL.SMART.USD", true, journaled_usd());
    assert!(backend.symbol_facts().balance_held().is_empty(), "nothing is known before a pass");
    let recon = bind.recon.expect("the gatewayhub has a reconcile client");
    assert_eq!(recon.fetch_balance(), Ok(None), "SAP.SMART.EUR is journaled and untold");
    assert_eq!(
        backend.symbol_facts().balance_held(),
        vec![("ibkr".to_string(), "untold foreign stock".to_string())]
    );

    let peer = balance_gateway("ibkr", Some("USD"), false);
    let (backend, bind) =
        start_with_recon(&peer, "ibkr", "AAPL.SMART.USD", true, BootHints::none());
    let recon = bind.recon.expect("the gatewayhub has a reconcile client");
    assert_eq!(recon.fetch_balance(), Ok(Some(25_000.0)));
    assert!(backend.symbol_facts().balance_held().is_empty(), "nothing owed, nothing held");
}

/// **ibkr's boot window** (third review, MEDIUM-D): a tradehub that boots with a foreign stock in the journal has
/// told the new daemon's reconcile client nothing yet (the boot re-admission runs after the start), and ibkr's
/// balance is the cash of the MOUNTED contract's currency. It is not asked while a journaled foreign symbol is
/// untold; with no such symbol it is asked as ever.
#[test]
fn an_ibkr_balance_is_not_asked_while_a_journaled_foreign_stock_is_untold() {
    use vike_tradehub::remote_backend::restore::BootHints;
    let peer = balance_gateway("ibkr", Some("USD"), false);
    assert_eq!(
        balance_of(&peer, "ibkr", "AAPL.SMART.USD", true, journaled_usd()),
        Ok(None),
        "SAP.SMART.EUR is journaled and nobody told the daemon's client"
    );
    let peer = balance_gateway("ibkr", Some("USD"), false);
    assert_eq!(
        balance_of(&peer, "ibkr", "AAPL.SMART.USD", true, BootHints::none()),
        Ok(Some(25_000.0)),
        "no journaled foreign stock: nothing to wait for"
    );
}

/// **A `live` account is never defaulted, through the REAL start**: nothing the gatewayhub states, assumes or
/// leaves silent, and nothing the journal carries, changes what a live account was told before the default existed
/// (a stated currency passes through, silence tells nothing, an assumption is passed along and not acted on); the
/// assembly then refuses to anchor it (`ledger_anchor`). A demo account with NO ceiling is the same.
#[test]
fn a_live_account_or_one_with_no_ceiling_is_never_defaulted_through_the_real_start() {
    use vike_tradehub::remote_backend::restore::{BootHints, JournaledFx};
    let journal = BootHints::none().with_fx(
        "oanda",
        JournaledFx {
            account_ccy: "USD".to_string(),
            symbols: vec![("EURGBP".into(), "GBP".into())],
        },
    );
    for (stated, assumed) in [(None, false), (Some("EUR"), false), (Some("USD"), true)] {
        let want = (stated.map(str::to_string), assumed);
        let live = gateway_replying("oanda", AccountTier::Live, stated, assumed);
        let bind =
            start_bind_at(&live, "oanda", AccountTier::Live, "EURUSD", true, journal.clone());
        assert_eq!(
            (bind.account_ccy, bind.account_ccy_assumed),
            want,
            "live, gatewayhub says {stated:?} assumed={assumed}: told exactly what it was"
        );
        let demo = gateway_replying("oanda", AccountTier::Demo, stated, assumed);
        let bind = start_bind(&demo, "oanda", "EURUSD", false, journal.clone());
        assert_eq!(
            (bind.account_ccy, bind.account_ccy_assumed),
            want,
            "no ceiling, gatewayhub says {stated:?} assumed={assumed}"
        );
    }
}

/// **Every converting venue anchors a missed read to the journal, under the same fences as a stated currency**
/// (`anchor_with_journal` states the rule; ibkr's rows are above): after one missed read at a restart the account that
/// converts is anchored to the currency its previous ledger was (so the core restores a default account's journaled
/// engine instead of skipping it), the line says that no symbol quoted in another currency is admitted on it until
/// the venue states its own, an account with no ceiling or no journal is anchored to nothing, and the ENGINE's
/// mount fence is the door's too: a journaled or stated currency the mounted symbol is not quoted in anchors
/// nothing (ig's other-than-USD never does).
#[test]
fn a_start_that_could_not_read_the_currency_anchors_every_converting_venue_to_the_journals() {
    use vike_tradehub::remote_backend::restore::{BootHints, JournaledFx};
    let hints = |venue: &str, ccy: &str| {
        BootHints::none().with_fx(
            venue,
            JournaledFx {
                account_ccy: ccy.to_string(),
                symbols: vec![("EURGBP".into(), "GBP".into())],
            },
        )
    };
    // (venue, the symbol its default account mounts, a symbol quoted in yen it could be mounted on)
    for (venue, usd_mount, yen_mount) in [
        ("oanda", "EURUSD", "USDJPY"),
        ("ctrader", "EURUSD", "USDJPY"),
        ("dukascopy", "EURUSD", "USDJPY"),
        ("fxcm", "EURUSD", "USDJPY"),
        ("ig", "CS.D.EURUSD.MINI.IP", "CS.D.USDJPY.MINI.IP"),
    ] {
        let anchor = |stated, mount: &str, ceiling, hints| {
            start_anchor(&gateway_of(venue, stated), venue, mount, ceiling, hints)
        };
        let (got, logged) =
            vike_log::capture::captured(|| anchor(None, usd_mount, true, hints(venue, "usd")));
        assert_eq!(got.as_deref(), Some("USD"), "{venue}: unread, journaled, converting");
        // a journal that names the default only records the previous run's assumption (review 2, MEDIUM-1): it is
        // said as the same assumption and locks nothing out, where a journal's other money would
        assert!(
            logged
                .iter()
                .any(|e| e.message.contains("the venue stated no currency for the account")
                    && e.message.contains("assumed USD")),
            "{venue}: {logged:?}"
        );
        assert!(
            logged.iter().all(|e| !e.message.contains("NO symbol quoted in another currency")),
            "{venue}: {logged:?}"
        );
        // a journal that names ANOTHER money the mount is quoted in is the journal's word: said as such, and no
        // symbol quoted in another currency is admitted on a rate that cannot confirm it (ig's included: its
        // journaled pound is an anchor like any venue's since the non-USD change)
        let gbp_mount = if venue == "ig" { "CS.D.EURGBP.MINI.IP" } else { "EURGBP" };
        let (got, logged) =
            vike_log::capture::captured(|| anchor(None, gbp_mount, true, hints(venue, "GBP")));
        assert_eq!(got.as_deref(), Some("GBP"), "{venue}: a GBP journal on a GBP-quoted mount");
        assert!(
            logged.iter().any(|e| e.message.contains("could not state the account's currency")
                && e.message.contains("anchored to GBP")
                && e.message.contains("NO symbol quoted in another currency is admitted")),
            "{venue}: {logged:?}"
        );
        assert_eq!(
            anchor(None, usd_mount, false, hints(venue, "USD")),
            None,
            "{venue}: no ceiling"
        );
        // nothing stated and nothing journaled: USD (the owner's default), never "no anchor"
        assert_eq!(
            anchor(None, usd_mount, true, BootHints::none()).as_deref(),
            Some("USD"),
            "{venue}: no journal"
        );
        assert_eq!(
            anchor(Some("USD"), usd_mount, true, hints(venue, "USD")).as_deref(),
            Some("USD")
        );
        // the mount fence of the engine, here too: the mounted symbol is not quoted in the journaled money.
        // FLIPPED for the venues whose lane rates the mount (decision 0183's mounted-symbol amendment: oanda,
        // ctrader, fxcm; dukascopy and ig since the non-USD change): the ledger is anchored all the same and the
        // mount declared foreign-quoted, where these rows were `None`.
        assert_eq!(
            anchor(None, usd_mount, true, hints(venue, "GBP")).as_deref(),
            Some("GBP"),
            "{venue}: a GBP journal"
        );
        assert_eq!(
            anchor(None, yen_mount, true, hints(venue, "USD")).as_deref(),
            Some("USD"),
            "{venue}: a yen mount"
        );
        assert_eq!(
            anchor(Some("USD"), yen_mount, true, BootHints::none()).as_deref(),
            Some("USD"),
            "{venue}: stated, yen mount"
        );
    }
}

/// **A daemon that predates the per-step preflight is NAMED** (review 15, finding 8): it answers the plan
/// with no venue lists, every leg reads `did not answer` (a WARN, never green), and the words say why —
/// a version skew, not "no venue lists".
#[test]
fn a_daemon_that_predates_the_preflight_steps_is_named_and_no_leg_is_green() {
    let peer = TradePeer::start(config(false));
    let backend = connect(&peer);
    let venues = vec!["binance".to_string()];
    let plan = backend
        .preflight_plan(&PreflightQuery { clock_venues: &venues })
        .expect("the plan degrades, it is not an error");
    assert!(plan.clock_policies.is_empty(), "no venue list came");
    match backend.server_time_ms("binance", AccountTier::Demo) {
        Err(ServerTimeGap::Unreachable(why)) => {
            assert!(why.contains("predates the per-step preflight"), "{why}");
        }
        other => panic!("a clock leg must be a WARN naming the version skew: {other:?}"),
    }
}

// ---- fxcm: the mount follows the account's money (measured on the demo account, 2026-10-20: GBP) ---------------

/// A fake gatewayhub whose fxcm adapter comes up at `tier` WITH a reconcile client (every balance request answered
/// with 25,000) and answers its Nth `AdapterStart` with the currency `says[N]` (`None`: it could not read it; the
/// last entry answers every later start).
fn fxcm_gateway(tier: AccountTier, says: Vec<Option<&'static str>>) -> TradePeer {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vike_gatewayhub_client::proto::{ReconKind, ReconPayload};
    let default = fake_trade_peer::default_script();
    let mut daemon = config(false);
    daemon.route_key = "fxcm".to_string();
    daemon.features.push(vike_gatewayhub_client::proto::trade_venue_feature("fxcm"));
    let asked = AtomicUsize::new(0);
    TradePeer::start(daemon.with_script(move |frame| match frame {
        TradeUp::AdapterStart(s) => {
            let stated = says[asked.fetch_add(1, Ordering::SeqCst).min(says.len() - 1)];
            vec![Step::Send(TradeDown::AdapterReply {
                rid: s.rid,
                reply: Box::new(AdapterReply::Live {
                    bound_tier: tier,
                    grid: None,
                    contract_value: None,
                    margin_mode: None,
                    leg_grids: Vec::new(),
                    recon: true,
                    leverage_cap: None,
                    account_ccy: stated.map(str::to_string),
                    account_ccy_assumed: false,
                }),
            })]
        }
        TradeUp::Recon { rid, kind: ReconKind::Balance } => {
            vec![Step::Send(TradeDown::ReconReply {
                rid: *rid,
                page: 0,
                more: false,
                payload: ReconPayload::Balance(Some(25_000.0)),
                read: None,
            })]
        }
        other => default(other),
    }))
}

/// One start of fxcm's default account at `tier`, mounted on `mounted`, with its reconcile client and the exposure
/// ceiling `ceiling`. The backend is returned beside the result: the bind's clients keep its leases open.
fn fxcm_start(
    peer: &TradePeer,
    tier: AccountTier,
    mounted: &str,
    ceiling: bool,
) -> (RemoteBackend, Result<vike_tradehub_core::AdapterBind, BackendError>) {
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[("fxcm", tier)]),
        ID.to_string(),
        Duration::from_secs(5),
    );
    backend.symbol_facts().bind_exposure_cap(move |_| ceiling);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let query = AdapterStartQuery {
        venue: "fxcm",
        account: &label,
        tier,
        symbol: mounted,
        declared_legs: &[],
        recon_enabled: true,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: "fxcm",
        properties_rec: None,
    };
    let started = backend.start_adapter(query, &events, None);
    (backend, started)
}

/// The symbols the fxcm gatewayhub was asked to start its adapter on, in order.
fn started_on(peer: &TradePeer) -> Vec<String> {
    adapter_starts(peer).into_iter().map(|s| s.symbol).collect()
}

/// **A pound account of fxcm is started AGAIN on a pound-quoted pair** (the demo account's currency is GBP, which
/// the first start learns from the venue's `BASE_CRNCY`; the wired `EURUSD` is quoted in dollars and its ledger
/// books it at par, so it could not carry a pound ledger: the node declined to anchor, held the venue's balance back
/// and refused every foreign-quoted pair). The gatewayhub tears the first adapter down for a different start of the
/// same lease and logs in again on `EURGBP`; the bind says where the session now stands, the ledger is anchored to
/// GBP, and the venue's balance is ADOPTED into it. Nothing but the symbol of the second `AdapterStart` differs.
#[test]
fn an_fxcm_pound_account_is_started_again_on_a_pound_quoted_pair() {
    let peer = fxcm_gateway(AccountTier::Demo, vec![Some("GBP")]);
    let ((backend, started), logged) =
        vike_log::capture::captured(|| fxcm_start(&peer, AccountTier::Demo, "EURUSD", true));
    let bind = started.expect("the pound account comes up");
    assert_eq!(
        started_on(&peer),
        ["EURUSD", "EURGBP"],
        "one start on the wired pair, one on the pound pair"
    );
    assert_eq!(bind.mount_symbol.as_deref(), Some("EURGBP"));
    assert_eq!(
        (bind.account_ccy.as_deref(), bind.account_ccy_assumed),
        (Some("GBP"), false),
        "anchored to the money the venue stated"
    );
    assert_eq!(
        bind.recon.expect("the gatewayhub has a reconcile client").fetch_balance(),
        Ok(Some(25_000.0)),
        "the balance is adopted into the pound ledger, not held back"
    );
    assert!(
        logged
            .iter()
            .any(|e| ["fxcm", "GBP", "EURUSD", "EURGBP"].iter().all(|w| e.message.contains(w))),
        "the move is said, naming both pairs and the money: {logged:?}"
    );
    drop(backend);
}

/// **Every other fxcm start is made ONCE and is exactly what it was**: a dollar account (the wired mount carries
/// it), an account the venue states nothing for (the node assumes USD, flagged, and holds the balance), any other
/// money (no pair is mounted for it, and since decision 0183's mounted-symbol amendment none needs to be: its
/// ledger is anchored to the stated money, the mount declared foreign-quoted, the balance adopted), a pound
/// account that does not convert (no ceiling) or runs at `live`, a pound account mounted on a pair somebody
/// chose (anchored the same way), and a pound account already mounted on a pound pair.
#[test]
fn every_other_fxcm_start_is_made_once_exactly_as_before() {
    use AccountTier::{Demo, Live};
    /// One account: what the venue says, what the node asked, and what the start must come to.
    struct Case {
        what: &'static str,
        tier: AccountTier,
        says: Option<&'static str>,
        mount: &'static str,
        ceiling: bool,
        /// The currency the engine is anchored to, and whether it is only assumed.
        anchor: (Option<&'static str>, bool),
        /// What the reconcile client hands the engine as the venue's balance.
        balance: Result<Option<f64>, String>,
    }
    let case = |what, tier, says, mount, ceiling, anchor, balance| Case {
        what,
        tier,
        says,
        mount,
        ceiling,
        anchor,
        balance,
    };
    let held = || Ok(None);
    let adopted = || Ok(Some(25_000.0));
    let cases = [
        case("USD", Demo, Some("USD"), "EURUSD", true, (Some("USD"), false), adopted()),
        case("silent", Demo, None, "EURUSD", true, (Some("USD"), true), held()),
        // FLIPPED by decision 0183's mounted-symbol amendment (these three rows were `(None, false)` and `held()`,
        // the mount fence's decline): fxcm's lane rates its mount, so the ledger is anchored to the stated money,
        // the mount is declared foreign-quoted (and ONE start is still enough: only the pound row moves a mount),
        // and the venue's cash is the ledger's.
        case("JPY", Demo, Some("JPY"), "EURUSD", true, (Some("JPY"), false), adopted()),
        case("EUR", Demo, Some("EUR"), "EURUSD", true, (Some("EUR"), false), adopted()),
        case("GBP, no ceiling", Demo, Some("GBP"), "EURUSD", false, (Some("GBP"), false), held()),
        case("GBP, live", Live, Some("GBP"), "EURUSD", true, (Some("GBP"), false), held()),
        case(
            "GBP, a chosen mount",
            Demo,
            Some("GBP"),
            "USDJPY",
            true,
            (Some("GBP"), false),
            adopted(),
        ),
        case(
            "GBP, a pound mount",
            Demo,
            Some("GBP"),
            "EURGBP",
            true,
            (Some("GBP"), false),
            adopted(),
        ),
    ];
    for c in cases {
        let peer = fxcm_gateway(c.tier, vec![c.says]);
        let (backend, started) = fxcm_start(&peer, c.tier, c.mount, c.ceiling);
        let bind = started.unwrap_or_else(|e| panic!("{}: {e:?}", c.what));
        assert_eq!(
            started_on(&peer),
            [c.mount],
            "{}: ONE start, on the mount it was asked for",
            c.what
        );
        assert_eq!(bind.mount_symbol, None, "{}: nothing moved", c.what);
        assert_eq!((bind.account_ccy.as_deref(), bind.account_ccy_assumed), c.anchor, "{}", c.what);
        assert_eq!(
            bind.recon.expect("a reconcile client").fetch_balance(),
            c.balance,
            "{}",
            c.what
        );
        drop(backend);
    }
}

/// **A second start moves the mount at most once, and whatever it states is judged against the mount it ended
/// on**: a venue that then states USD (or nothing, which the node assumes to be USD) on the pound pair was declined
/// by the mount fence like any other mismatch, with the balance held back; since decision 0183's mounted-symbol
/// amendment fxcm's lane rates its mount, so the ledger is anchored to what the second start states (the assumed
/// USD held back as before) and the pound mount is declared foreign-quoted. Either way the node does NOT try a
/// third start, and the bind still says where the session stands, because the engine must be mounted where the
/// adapter is.
#[test]
fn a_second_start_that_disagrees_is_anchored_and_the_mount_does_not_move_again() {
    let adopted: Result<Option<f64>, String> = Ok(Some(25_000.0));
    let held: Result<Option<f64>, String> = Ok(None);
    for (second, anchor, balance) in [
        (Some("USD"), (Some("USD"), false), adopted.clone()),
        (None, (Some("USD"), true), held),
        (Some("JPY"), (Some("JPY"), false), adopted),
    ] {
        let says = vec![Some("GBP"), second];
        let peer = fxcm_gateway(AccountTier::Demo, says.clone());
        let (backend, started) = fxcm_start(&peer, AccountTier::Demo, "EURUSD", true);
        let bind = started.expect("it comes up");
        assert_eq!(started_on(&peer), ["EURUSD", "EURGBP"], "{says:?}: no third start");
        assert_eq!(
            bind.mount_symbol.as_deref(),
            Some("EURGBP"),
            "{says:?}: the session stands there"
        );
        assert_eq!(
            (bind.account_ccy.as_deref(), bind.account_ccy_assumed),
            anchor,
            "{says:?}: anchored to what the second start says, the pound mount declared"
        );
        assert_eq!(bind.recon.expect("a reconcile client").fetch_balance(), balance, "{says:?}");
        drop(backend);
    }
}

/// **A second start the gatewayhub refuses is the account NOT RUNNING** (`adapter failed`, with the bridge's
/// words in the log), never an engine mounted on the first start's pair beside an adapter that is gone.
#[test]
fn a_refused_second_start_is_not_running() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let default = fake_trade_peer::default_script();
    let mut daemon = config(false);
    daemon.route_key = "fxcm".to_string();
    daemon.features.push(vike_gatewayhub_client::proto::trade_venue_feature("fxcm"));
    let asked = AtomicUsize::new(0);
    let peer = TradePeer::start(daemon.with_script(move |frame| match frame {
        TradeUp::AdapterStart(s) => {
            let reply = if asked.fetch_add(1, Ordering::SeqCst) == 0 {
                AdapterReply::Live {
                    bound_tier: AccountTier::Demo,
                    grid: None,
                    contract_value: None,
                    margin_mode: None,
                    leg_grids: Vec::new(),
                    recon: false,
                    leverage_cap: None,
                    account_ccy: Some("GBP".to_string()),
                    account_ccy_assumed: false,
                }
            } else {
                AdapterReply::NotRunning {
                    block_word: ArmingBlock::AdapterFailed.word().to_string(),
                    refusal_text: "fxcm: the login on EURGBP was refused".to_string(),
                }
            };
            vec![Step::Send(TradeDown::AdapterReply { rid: s.rid, reply: Box::new(reply) })]
        }
        other => default(other),
    }));
    let (_backend, started) = fxcm_start(&peer, AccountTier::Demo, "EURUSD", true);
    assert_eq!(started.unwrap_err(), BackendError::NotRunning(ArmingBlock::AdapterFailed));
    assert_eq!(started_on(&peer), ["EURUSD", "EURGBP"]);
}

/// **The SECOND start gets twice the first's bound, and only the second does** (review F1): the gatewayhub tears the
/// first adapter down before the new login (its exec thread, still inside its own login, is joined for up to its
/// full join wait), so a second answer that arrives after the first's bound but inside twice of it must still
/// count. A first answer that late is a silent venue and times out as it always did (`NotRunning(AdapterFailed)`).
#[test]
fn only_the_second_fxcm_start_gets_twice_the_bound() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // The bound is `min(wait, request_reply)`: 600 ms here, so 900 ms is inside twice the bound and past one.
    let (bound, late) = (Duration::from_millis(600), Duration::from_millis(900));
    let gateway = |slow_start: usize| {
        let default = fake_trade_peer::default_script();
        let mut daemon = config(false);
        daemon.route_key = "fxcm".to_string();
        daemon.features.push(vike_gatewayhub_client::proto::trade_venue_feature("fxcm"));
        let asked = AtomicUsize::new(0);
        TradePeer::start(daemon.with_script(move |frame| match frame {
            TradeUp::AdapterStart(s) => {
                let live = Step::Send(TradeDown::AdapterReply {
                    rid: s.rid,
                    reply: Box::new(AdapterReply::Live {
                        bound_tier: AccountTier::Demo,
                        grid: None,
                        contract_value: None,
                        margin_mode: None,
                        leg_grids: Vec::new(),
                        recon: false,
                        leverage_cap: None,
                        account_ccy: Some("GBP".to_string()),
                        account_ccy_assumed: false,
                    }),
                });
                if asked.fetch_add(1, Ordering::SeqCst) == slow_start {
                    vec![Step::Sleep(late), live]
                } else {
                    vec![live]
                }
            }
            other => default(other),
        }))
    };
    let start = |peer: &TradePeer| {
        let backend = RemoteBackend::connect(
            link_to(&peer.addr_string()),
            accounts(&[("fxcm", AccountTier::Demo)]),
            ID.to_string(),
            bound,
        );
        backend.symbol_facts().bind_exposure_cap(|_| true);
        let (events, _lane) = fake_trade_peer::lane();
        let label = AccountLabel::Default;
        let query = AdapterStartQuery {
            venue: "fxcm",
            account: &label,
            tier: AccountTier::Demo,
            symbol: "EURUSD",
            declared_legs: &[],
            recon_enabled: false,
            risk_profile: None,
            market_slippage: None,
            halt_admit: vike_model::HaltAdmit::default(),
            withhold: None,
            account_leverage: None,
            route_key: "fxcm",
            properties_rec: None,
        };
        let started = backend.start_adapter(query, &events, None);
        (backend, started)
    };
    // the second answer is 900 ms late: inside the doubled bound, so the account comes up on the pound pair
    let peer = gateway(1);
    let (_backend, started) = start(&peer);
    let bind = started.expect("a second start inside twice the bound comes up");
    assert_eq!(bind.mount_symbol.as_deref(), Some("EURGBP"));
    // the FIRST answer is 900 ms late: past its (single) bound, a silent venue, as before
    let peer = gateway(0);
    let (_backend, started) = start(&peer);
    assert_eq!(started.unwrap_err(), BackendError::NotRunning(ArmingBlock::AdapterFailed));
}

// ---- ig: what the start asks for, which accounts run, and what a lease asks of the gatewayhub (decision 0183) ----

/// A fake gatewayhub whose ig adapter comes up stating `stated` as the account's currency (`None`: it could not
/// read it right now).
fn ig_gateway(stated: Option<&'static str>) -> TradePeer {
    let default = fake_trade_peer::default_script();
    let mut daemon = config(false);
    daemon.route_key = "ig".to_string();
    daemon.features.push(vike_gatewayhub_client::proto::trade_venue_feature("ig"));
    TradePeer::start(daemon.with_script(move |frame| match frame {
        TradeUp::AdapterStart(s) => vec![Step::Send(TradeDown::AdapterReply {
            rid: s.rid,
            reply: Box::new(AdapterReply::Live {
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
                recon: false,
                leverage_cap: None,
                account_ccy: stated.map(str::to_string),
                account_ccy_assumed: false,
            }),
        })],
        other => default(other),
    }))
}

/// One ig start against `peer`, the exposure ceiling `ceiling` (bound before the start, as the root binds it before
/// the engines are built).
fn ig_start(
    peer: &TradePeer,
    ceiling: bool,
) -> (RemoteBackend, Result<vike_tradehub_core::AdapterBind, BackendError>) {
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[("ig", AccountTier::Demo)]),
        ID.to_string(),
        Duration::from_secs(5),
    );
    backend.symbol_facts().bind_exposure_cap(move |_| ceiling);
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;
    let query = AdapterStartQuery {
        venue: "ig",
        account: &label,
        tier: AccountTier::Demo,
        symbol: "CS.D.EURUSD.MINI.IP",
        declared_legs: &[],
        recon_enabled: false,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: "ig",
        properties_rec: None,
    };
    let started = backend.start_adapter(query, &events, None);
    (backend, started)
}

/// **An ig start asks the gatewayhub to read the account's currency only when the account will convert**: a demo
/// account with an exposure ceiling armed. The read is a login and `GET /accounts` out of the account's request
/// allowance, so an account with no ceiling asks nothing at its start and its `AdapterStart` frame is the old
/// frame, byte for byte (`read_account_ccy` is not written while false); and its LEASE does not ask the
/// gatewayhub to convert (`quoted` is not sent), so the gatewayhub does no login, no `GET /accounts` and no
/// market read for an epic the node would refuse afterwards. With a ceiling and a stated currency the lease does.
#[test]
fn an_ig_start_asks_for_the_currency_and_the_conversion_only_with_a_ceiling() {
    for ceiling in [true, false] {
        let peer = ig_gateway(Some("USD"));
        let (backend, started) = ig_start(&peer, ceiling);
        let bind = started.expect("a USD ig account comes up");
        let starts = adapter_starts(&peer);
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].read_account_ccy, ceiling, "ceiling {ceiling}");
        let wire = serde_json::to_string(&starts[0]).unwrap();
        assert_eq!(wire.contains("read_account_ccy"), ceiling, "{wire}");
        // What the node judged from the gatewayhub's word, then what the lease asks of it.
        assert_eq!(bind.account_ccy.as_deref(), Some("USD"));
        let _ = backend.symbol_facts().admission("ig", "ig", "SOLUSDT.P");
        let quoted: Vec<bool> = peer
            .ups()
            .into_iter()
            .filter_map(|f| match f {
                TradeUp::SymbolFacts { quoted, .. } => Some(quoted),
                _ => None,
            })
            .collect();
        assert_eq!(quoted, [ceiling], "the lease converts exactly when the ceiling is armed");
    }
}

/// **A lease anchored to the ASSUMED USD asks the gatewayhub to convert** (owner's delegate, 2026-10-19: the USD
/// default must TRADE, inside the conversion's own gates, as FXCM's assumed USD does): the gatewayhub could not
/// state the currency, the node anchors the ledger to the ruling's USD (flagged assumed), and the lease is
/// anchored, so a foreign-quoted admission is asked for with the conversion on. The same ig account with NO
/// ceiling asks nothing (the gates are the conversion's own).
#[test]
fn an_ig_lease_anchored_to_the_assumed_usd_asks_the_gatewayhub_to_convert() {
    for ceiling in [true, false] {
        let peer = ig_gateway(None);
        let (backend, started) = ig_start(&peer, ceiling);
        let bind = started.expect("it comes up anchored to the default");
        assert_eq!(
            (bind.account_ccy.as_deref(), bind.account_ccy_assumed),
            if ceiling { (Some("USD"), true) } else { (None, false) },
            "ceiling {ceiling}"
        );
        let _ = backend.symbol_facts().admission("ig", "ig", "SOLUSDT.P");
        let quoted: Vec<bool> = peer
            .ups()
            .into_iter()
            .filter_map(|f| match f {
                TradeUp::SymbolFacts { quoted, .. } => Some(quoted),
                _ => None,
            })
            .collect();
        assert_eq!(quoted, [ceiling], "the assumed anchor is a confirmed one: ceiling {ceiling}");
    }
}

/// **A converting venue whose API states no currency is anchored to USD, ASSUMED** (owner ruling 2026-10-19,
/// repeated: "the account currency is what the venue API states; if the API does not state it, or the read
/// fails: USD"). The order of precedence is the venue's word, then the journal's, then USD; an assumption is
/// flagged on the bind (`account_ccy_assumed`, the flag FXCM's lane already sets) so the engine's assembly says it
/// at `warn`; and the default is applied only to an account that converts (a ceiling armed), so nothing opens
/// where the conversion is not armed.
#[test]
fn a_venue_that_states_no_currency_is_anchored_to_assumed_usd_after_the_venue_and_the_journal() {
    use vike_tradehub::remote_backend::restore::{BootHints, JournaledFx};
    let journal = |venue: &str, ccy: &str| {
        BootHints::none().with_fx(
            venue,
            JournaledFx {
                account_ccy: ccy.to_string(),
                symbols: vec![("EURGBP".into(), "GBP".into())],
            },
        )
    };
    let seen = |venue: &'static str, stated, mount: &str, ceiling, hints| {
        let bind = start_bind(&gateway_of(venue, stated), venue, mount, ceiling, hints);
        (bind.account_ccy, bind.account_ccy_assumed)
    };
    let some = |ccy: &str, assumed| (Some(ccy.to_string()), assumed);
    // (venue, the symbol its default account mounts, a symbol quoted in sterling)
    for (venue, usd_mount, gbp_mount) in [
        ("oanda", "EURUSD", "EURGBP"),
        ("ctrader", "EURUSD", "EURGBP"),
        ("dukascopy", "EURUSD", "EURGBP"),
        ("fxcm", "EURUSD", "EURGBP"),
        ("ig", "CS.D.EURUSD.MINI.IP", "CS.D.EURGBP.MINI.IP"),
    ] {
        // nothing stated, nothing journaled, a ceiling: USD, assumed
        assert_eq!(
            seen(venue, None, usd_mount, true, BootHints::none()),
            some("USD", true),
            "{venue}"
        );
        // ...but no ceiling (the conversion is not armed): nothing is defaulted
        assert_eq!(
            seen(venue, None, usd_mount, false, BootHints::none()),
            (None, false),
            "{venue}"
        );
        // the venue states USD: stated, never flagged (and the journal, whatever it says, is not consulted)
        assert_eq!(
            seen(venue, Some("USD"), usd_mount, true, journal(venue, "GBP")),
            some("USD", false),
            "{venue}"
        );
        // none stated, a journal that names the default: it only records the previous run's assumption (review 2,
        // MEDIUM-1), so it is the same assumed USD and trades like the first run
        assert_eq!(seen(venue, None, usd_mount, true, journal(venue, "usd")), some("USD", true));
        // the default is judged by the mount fence like any stated currency, and every venue's lane rates its mount
        // now (decision 0183's mounted-symbol amendment: oanda, ctrader, fxcm; dukascopy and ig since the
        // non-USD change): the assumed USD is anchored all the same and the sterling mount declared foreign-quoted
        // (these rows were `(None, false)` for dukascopy and ig, which kept the fence).
        assert_eq!(
            seen(venue, None, gbp_mount, true, BootHints::none()),
            some("USD", true),
            "{venue}"
        );
    }
    // venue > journal > USD on a sterling mount
    assert_eq!(
        seen("oanda", Some("GBP"), "EURGBP", true, journal("oanda", "USD")),
        some("GBP", false),
        "the venue's word beats the journal's"
    );
    assert_eq!(
        seen("oanda", None, "EURGBP", true, journal("oanda", "GBP")),
        some("GBP", false),
        "the journal's word beats the default (USD would not even anchor this mount)"
    );
}

/// **An ig account that armed the conversion and whose currency is not the one the mounted epic is quoted in RUNS,
/// anchored to its own money** (the non-USD change; it used to be `adapter failed`, said at `error`, because a
/// 10,000 GBP account seeded 10,000 into a ledger labelled dollars over a USD-quoted mount, ~35% off). The ledger is
/// the pound, the mount is declared foreign-quoted (the engine's assembly) and the node keeps it rated, so
/// nothing refuses the start and nothing is logged as a refusal; an ig account with NO ceiling reads nothing at its
/// start and is not judged (a gatewayhub that states a currency anyway changes nothing there), and a dollar account
/// runs.
#[test]
fn an_ig_account_in_another_currency_than_the_wire_runs_once_it_armed_the_conversion() {
    let peer = ig_gateway(Some("GBP"));
    let ((_backend, started), logged) = vike_log::capture::captured(|| ig_start(&peer, true));
    let bind = started.expect("a pound ig account that armed the conversion runs");
    assert_eq!((bind.account_ccy.as_deref(), bind.account_ccy_assumed), (Some("GBP"), false));
    assert!(
        logged.iter().all(|e| !e.message.contains("NOT RUNNING") && !e.message.contains("REFUSED")),
        "{logged:?}"
    );

    // no ceiling: nothing was asked, so nothing is judged
    let peer = ig_gateway(Some("GBP"));
    let (_backend, started) = ig_start(&peer, false);
    assert!(started.is_ok(), "an account that never converted runs as it always did");

    // the wire's own currency runs, whatever the spelling the venue used
    for stated in ["USD", "usd"] {
        let peer = ig_gateway(Some(stated));
        assert!(ig_start(&peer, true).1.is_ok(), "{stated}");
    }
}
