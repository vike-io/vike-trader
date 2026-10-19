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

/// **ONE venue whose adapter start never answers does not hold the boot**: the start returns after the
/// bound (`config.gatewayhub_connect_wait_ms`, one second here, not the lease's 300 s), the account is
/// `NotRunning(AdapterFailed)` (never `Unavailable`, whose late arm would restart the tradehub into the
/// same silent venue), and the NEXT account's start binds. The fold's start loop is serial and the
/// control door listens only after it returns, so a bounded start is a door that listens.
#[test]
fn a_silent_venue_is_bounded_and_the_next_account_still_starts() {
    let default = fake_trade_peer::default_script();
    let peer = TradePeer::start(config(false).with_script(move |frame| match frame {
        vike_gatewayhub_client::proto::TradeUp::AdapterStart(s) if s.symbol == "HANGUSDT" => {
            vec![Step::Sleep(Duration::from_secs(3))]
        }
        other => default(other),
    }));
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[("binance", AccountTier::Demo)]),
        ID.to_string(),
        Duration::from_secs(1),
    );
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;

    let mut hung = query(&[], &label, None);
    hung.symbol = "HANGUSDT";
    let started = std::time::Instant::now();
    let got = backend.start_adapter(hung, &events, None);
    let took = started.elapsed();
    assert_eq!(got.unwrap_err(), BackendError::NotRunning(ArmingBlock::AdapterFailed));
    assert!(took >= Duration::from_millis(900), "it waited the bound: {took:?}");
    assert!(took < Duration::from_secs(3), "and not the peer's three-second silence: {took:?}");

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
            vec![Step::Sleep(Duration::from_secs(3))]
        }
        other => default(other),
    }));
    let backend = RemoteBackend::connect(
        link_to(&peer.addr_string()),
        accounts(&[("binance", AccountTier::Demo)]),
        ID.to_string(),
        Duration::from_secs(1),
    );
    let hot = backend.refreshed(accounts(&[("binance", AccountTier::Demo)]), HashSet::new());
    let (events, _lane) = fake_trade_peer::lane();
    let label = AccountLabel::Default;

    let mut hung = query(&[], &label, None);
    hung.symbol = "HANGUSDT";
    let started = std::time::Instant::now();
    let got = hot.start_adapter(hung, &events, None);
    let took = started.elapsed();
    assert_eq!(got.unwrap_err(), BackendError::NotRunning(ArmingBlock::AdapterFailed));
    assert!(took < Duration::from_secs(3), "the boot bound, not the lease's wait: {took:?}");

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
    assert!(waited < Duration::from_secs(6), "…and not much more: {waited:?}");
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
