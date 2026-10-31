//! **`RemoteBackend` against the gatewayhub's REAL server and the PRODUCTION table spelling** (wave 2,
//! W11c).
//!
//! `remote_backend_wire.rs` drives the backend against a fake peer whose words the TEST writes, so a
//! gatewayhub that spells a refusal differently from what `ArmingBlock::from_word` reads stays invisible
//! there (the review found exactly that: the gatewayhub shipped `as_str()` words, the tradehub read
//! `word()`, every refusal decoded as `adapter failed` with a false "newer gatewayhub?" error, and the
//! live-account lock was never claimed). Here the other end is `vike-gatewayhub`'s own server over the
//! scripted trade lane, and the arming table is the one `BridgeTrade` really produces.
//!
//! * every refusal path crosses as a word the tradehub decodes to the SAME block;
//! * a live-account lock another process holds is a LEASE refusal: the whole live mount is refused
//!   (`lease_refusals`), as the old in-process lock did;
//! * a refusal no retry can change (a daemon that carries no adapter for the venue) is a failed adapter,
//!   NOT "the gatewayhub is unavailable" (which the late arm waits out and restarts for); a gatewayhub
//!   that is away still is;
//! * an operator's READ of a running account at its venue (`SymbolFacts::venue_read`, the node verb
//!   `vike-cli trade venue`) goes over the account's own lease to the real daemon: the daemon's gate and its
//!   `not offered` words come back as the operator's sentence, an account with no lease is told so without the
//!   daemon being asked, and a daemon that predates the read is told in words with nothing sent.

use std::time::Duration;

use vike_boot::live_lock::LiveLock;
use vike_config::ArmingBlock;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::gateway::{LoopbackGateway, TradeKit};
use vike_gatewayhub::testkit::rest::ScriptedRest;
use vike_gatewayhub::testkit::trade::{ScriptedStart, ScriptedTrade};
use vike_gatewayhub::trade::adapters::bridge_trade::BridgeTrade;
use vike_gatewayhub::trade::adapters::snapshot::TradeSnapshot;
use vike_gatewayhub::trade::lane::TradeLane as _;
use vike_gatewayhub_client::proto::ArmingTable;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub_core::{AdapterStartQuery, BackendError, VenueBackend as _};

#[path = "support/account_book.rs"]
mod account_book;

const ID: &str = "vike-tradehub-w11c";

/// What a complete binance key set looks like (the fields arm the bridge's `resolve`).
const KEYS: &[(&str, &str)] = &[("API_KEY", "k"), ("API_SECRET", "s")];

fn policy(tier: AccountTier) -> vike_tradehub_core::AdapterPolicy {
    account_book::keyed_policy("binance", tier, KEYS)
}

/// The table the PRODUCTION lane (`BridgeTrade` over the real registry) serves for `policy`.
fn production_table(policy: &vike_tradehub_core::AdapterPolicy) -> ArmingTable {
    BridgeTrade::with_snapshot(
        REGISTRY,
        std::collections::HashMap::new(),
        TradeSnapshot {
            accounts: policy.accounts.clone(),
            venue_settings: policy.venue_settings.clone(),
            venue_settings_unread: None,
            preflight_failed: policy.preflight_failed.clone(),
        },
    )
    .arming_table()
}

fn gateway(trade: ScriptedTrade, state_dir: Option<std::path::PathBuf>) -> LoopbackGateway {
    LoopbackGateway::start_trade_with(
        ScriptedData::new(&["binance"]),
        ScriptedRest::new(),
        trade,
        ServerConfig::PRODUCTION,
        TradeKit { state_dir, halt_path: None },
    )
}

fn backend(gw: &LoopbackGateway, policy: &vike_tradehub_core::AdapterPolicy) -> RemoteBackend {
    RemoteBackend::connect(
        gw.trade_link(),
        policy.accounts.clone(),
        ID.to_string(),
        Duration::from_secs(5),
    )
}

fn query<'a>(label: &'a AccountLabel, tier: AccountTier) -> AdapterStartQuery<'a> {
    AdapterStartQuery {
        venue: "binance",
        account: label,
        tier,
        symbol: "BTCUSDT",
        declared_legs: &[],
        recon_enabled: true,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: "binance",
        properties_rec: None,
    }
}

fn events() -> vike_exec::EventSender {
    vike_exec::event_channel(64).0.routed("binance")
}

fn start(
    backend: &RemoteBackend,
    tier: AccountTier,
) -> Result<vike_tradehub_core::AdapterBind, BackendError> {
    let label = AccountLabel::Default;
    backend.start_adapter(query(&label, tier), &events(), None)
}

/// **The table the gatewayhub really serves reads back as RUNNING.** A live account with a complete key
/// set is `none` on the wire and the tradehub reads it as armed at `live`.
#[test]
fn the_production_table_reads_back_as_a_running_live_account() {
    let policy = policy(AccountTier::Live);
    let gw = gateway(
        ScriptedTrade::new(&["binance"])
            .with_tier("binance", None, Ok(AccountTier::Live))
            .with_arming(production_table(&policy)),
        None,
    );
    let backend = backend(&gw, &policy);
    let table = backend.arming_table().expect("it answers");
    let row = table.row_of_account("binance", &AccountLabel::Default).expect("the row");
    assert_eq!(row.arming.effective, Some(AccountTier::Live));
    assert_eq!(row.arming.block, ArmingBlock::None, "a clear row decodes to None");
}

/// A refusal the gatewayhub spelled in a word this build reads must not log the "unknown word, a newer
/// gatewayhub?" error (it is `error` level and an operator acts on it).
fn assert_no_unknown_word_error(logged: &[vike_log::capture::CapturedEvent]) {
    assert!(
        !logged.iter().any(|e| e.message.contains("does not know")),
        "a known word was read as unknown: {logged:?}"
    );
}

/// Every refusal path, over the real server: the block the gatewayhub names is the block the tradehub
/// reports, and it is NOT the "unknown word" fallback (`adapter failed`) for a block that is not that.
#[test]
fn a_refusal_for_the_accounts_state_decodes_to_the_same_block() {
    let policy = policy(AccountTier::Demo);
    for block in
        [ArmingBlock::TierConflict, ArmingBlock::AccountInactive, ArmingBlock::NoAccountRow]
    {
        let gw = gateway(
            ScriptedTrade::new(&["binance"])
                .with_tier("binance", None, Err(block))
                .with_arming(production_table(&policy)),
            None,
        );
        let backend = backend(&gw, &policy);
        let (got, logged) = vike_log::capture::captured(|| start(&backend, AccountTier::Demo));
        match got {
            Err(BackendError::NotRunning(got)) => assert_eq!(got, block, "TradeRefused"),
            other => panic!("{block:?}: {other:?}"),
        }
        assert_no_unknown_word_error(&logged);
        assert!(backend.lease_refusals().is_empty(), "an account's refusal is not a lease refusal");
    }
}

/// `AdapterStart` the bridge refuses: the refusal's block crosses and decodes unchanged.
#[test]
fn a_start_refusal_decodes_to_the_same_block() {
    let policy = policy(AccountTier::Demo);
    let gw = gateway(
        ScriptedTrade::new(&["binance"]).with_arming(production_table(&policy)).with_start(
            "binance",
            None,
            ScriptedStart::refuse(ArmingBlock::NoCredentials, "binance: no demo key set is stored"),
        ),
        None,
    );
    let backend = backend(&gw, &policy);
    let (got, logged) = vike_log::capture::captured(|| start(&backend, AccountTier::Demo));
    match got {
        Err(BackendError::NotRunning(got)) => assert_eq!(got, ArmingBlock::NoCredentials),
        other => panic!("{other:?}"),
    }
    assert_no_unknown_word_error(&logged);
}

/// **A live account another process holds refuses the WHOLE live mount.** The gatewayhub claims
/// `LIVE-binance.lock` at the open; a claim that fails comes back as a LEASE refusal, which the
/// backend records into `lease_refusals` (the root refuses the mount on any entry) with the lock's own
/// sentence in it. And the same account, free, opens and HOLDS the claim at the gatewayhub.
#[test]
fn a_live_lock_another_process_holds_refuses_the_whole_mount_in_the_locks_words() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let policy = policy(AccountTier::Live);
    let trade = ScriptedTrade::new(&["binance"])
        .with_tier("binance", None, Ok(AccountTier::Live))
        .with_arming(production_table(&policy));
    let gw = gateway(trade.clone(), Some(dir.path().to_path_buf()));
    let backend = backend(&gw, &policy);

    let other = LiveLock::acquire(dir.path(), "binance").expect("held by another process");
    match start(&backend, AccountTier::Live) {
        Err(BackendError::NotRunning(_)) => {}
        other => panic!("{other:?}"),
    }
    let refusals = backend.lease_refusals();
    assert_eq!(refusals.len(), 1, "one lease refusal: {refusals:?}");
    assert!(
        refusals[0].contains("another live process already trades the binance account"),
        "the lock's own words reach the operator: {}",
        refusals[0]
    );
    assert_eq!(trade.starts(), 0, "no venue client was built behind the refused claim");

    drop(other);
    let bind = start(&backend, AccountTier::Live).expect("the claim is free, the account starts");
    assert_eq!(bind.bound_tier, AccountTier::Live);
    assert!(
        LiveLock::acquire(dir.path(), "binance")
            .is_err_and(|e| e.kind() == std::io::ErrorKind::AddrInUse),
        "the gatewayhub holds LIVE-binance.lock for the running live account"
    );
}

/// **Version skew is a failed adapter, not an outage.** A daemon that carries no trade adapter for the
/// venue answers the arming query (so the late arm's probe would pass) but cannot open the lease; the
/// account is NOT RUNNING `adapter failed`, which the late arm does not wait for (it waits only on
/// `gatewayhub unavailable`) and so does not restart the process for, cycle after cycle.
#[test]
fn a_daemon_without_the_venue_is_a_failed_adapter_and_not_an_outage() {
    let policy = policy(AccountTier::Demo);
    let gw = gateway(ScriptedTrade::new(&["bybit"]).with_arming(production_table(&policy)), None);
    let backend = backend(&gw, &policy);
    match start(&backend, AccountTier::Demo) {
        Err(BackendError::NotRunning(got)) => assert_eq!(got, ArmingBlock::AdapterFailed),
        other => panic!("a fixed refusal must not read as an outage: {other:?}"),
    }
}

/// A gatewayhub that went away after the table was fetched IS an outage: the account waits for it.
#[test]
fn a_gatewayhub_that_went_away_is_an_outage() {
    let policy = policy(AccountTier::Demo);
    let gw = gateway(ScriptedTrade::new(&["binance"]).with_arming(production_table(&policy)), None);
    let backend = backend(&gw, &policy);
    drop(gw);
    match start(&backend, AccountTier::Demo) {
        Err(BackendError::Unavailable(_)) => {}
        other => panic!("{other:?}"),
    }
}

#[path = "../../vike-gatewayhub-client/tests/support/fake_trade_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_trade_peer;

/// **The same, one verb later**: a daemon that opens the lease but does not advertise `gw_adapter` makes the
/// adapter start a local refusal (`TradeError::Unsupported`, nothing sent). That is deploy skew too — a
/// failed adapter, not an outage the late arm would restart over.
#[test]
fn a_daemon_that_lacks_the_adapter_verb_is_a_failed_adapter_and_not_an_outage() {
    use vike_gatewayhub_client::proto::{FEATURE_GW_ADAPTER, Request, Response};
    let mut config = fake_trade_peer::TradePeerConfig::new().with_rpc(|req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::default())],
        other => vec![Response::Error(format!("unscripted: {other:?}"))],
    });
    config.features.retain(|f| f != FEATURE_GW_ADAPTER);
    let peer = fake_trade_peer::TradePeer::start(config);
    let policy = policy(AccountTier::Demo);
    let backend = RemoteBackend::connect(
        peer.link(),
        policy.accounts.clone(),
        ID.to_string(),
        Duration::from_secs(5),
    );
    match start(&backend, AccountTier::Demo) {
        Err(BackendError::NotRunning(got)) => assert_eq!(got, ArmingBlock::AdapterFailed),
        other => panic!("a missing verb must not read as an outage: {other:?}"),
    }
}

/// The reads the scripted lane was asked, in order.
fn venue_reads_asked(trade: &ScriptedTrade) -> Vec<vike_model::VenueReadKind> {
    use vike_gatewayhub::testkit::trade::TradeCall;
    trade
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            TradeCall::VenueRead { target, kind } => {
                assert_eq!(target.venue, "binance", "the read names the lease's own venue");
                Some(kind)
            }
            _ => None,
        })
        .collect()
}

/// **A running account is READ at its venue over its OWN lease, through the real daemon.** The start opened the
/// lease and registered it; `SymbolFacts::venue_read` (what `Request::VenueRead` calls) sends `TradeUp::VenueRead`
/// down it, and the daemon's bridge answers. The read starts no second adapter, and the daemon's own gate holds
/// the next read of the same account and kind apart (its sentence reaches the operator as it was written), while
/// the other kind is a read of its own and meets the daemon's `not offered` words when the bridge has none.
#[test]
fn a_running_account_is_read_at_its_venue_over_its_own_lease() {
    use vike_model::{VenueBalance, VenueRead, VenueReadKind};
    use vike_tradehub::server::symbol_admission::SymbolFacts as _;
    let policy = policy(AccountTier::Demo);
    let answer = VenueRead::Balance(VenueBalance {
        asset: "pUSD".into(),
        balance: 20.0,
        allowances: Vec::new(),
    });
    let trade = ScriptedTrade::new(&["binance"])
        .with_arming(production_table(&policy))
        .with_venue_read("binance", VenueReadKind::Balance, Ok(answer.clone()));
    let gw = gateway(trade.clone(), None);
    let backend = backend(&gw, &policy);
    let bind = start(&backend, AccountTier::Demo).expect("the account starts");
    assert_eq!(trade.starts(), 1);
    let facts = backend.symbol_facts();

    assert_eq!(facts.venue_read("binance", VenueReadKind::Balance), Ok(answer));
    assert_eq!(trade.starts(), 1, "a read starts no adapter");

    let held = facts.venue_read("binance", VenueReadKind::Balance).expect_err("held apart");
    assert!(held.contains("was read a moment ago") && held.contains("held 2 s apart"), "{held}");
    let none = facts.venue_read("binance", VenueReadKind::OpenOrders).expect_err("no such read");
    assert!(none.starts_with("not offered by binance"), "the daemon's own words: {none}");
    assert_eq!(
        venue_reads_asked(&trade),
        [VenueReadKind::Balance, VenueReadKind::OpenOrders],
        "the refused repeat never reached the lane"
    );
    drop(bind);
}

/// **An account with no lease is told so, by name, and the daemon is asked nothing**: the registry holds a lease
/// for a RUNNING account only, so a not-yet-started account, a labelled account beside a running default one and
/// a stopped engine all answer the same sentence, and none of them is read through another account's lease.
#[test]
fn an_account_with_no_lease_is_told_so_and_the_daemon_is_asked_nothing() {
    use vike_model::VenueReadKind;
    use vike_tradehub::server::symbol_admission::SymbolFacts as _;
    let policy = policy(AccountTier::Demo);
    let trade = ScriptedTrade::new(&["binance"]).with_arming(production_table(&policy));
    let gw = gateway(trade.clone(), None);
    let backend = backend(&gw, &policy);
    let facts = backend.symbol_facts();

    let before = facts.venue_read("binance", VenueReadKind::Balance).expect_err("not started");
    assert!(before.starts_with("binance:") && before.contains("no open trade lease"), "{before}");

    let bind = start(&backend, AccountTier::Demo).expect("the account starts");
    let labelled = facts.venue_read("binance#ALT", VenueReadKind::Balance).expect_err("no lease");
    assert!(labelled.starts_with("binance#ALT:") && labelled.contains("no open trade lease"));

    facts.engine_stopped("binance");
    let stopped = facts.venue_read("binance", VenueReadKind::Balance).expect_err("stopped");
    assert!(stopped.contains("no open trade lease"), "{stopped}");
    assert!(venue_reads_asked(&trade).is_empty(), "the daemon was asked nothing");
    drop(bind);
}

/// **A gatewayhub that predates the read is told in words and sent nothing.** It cannot decode the lease frame,
/// so the client refuses locally against a daemon that does not advertise `gw_trade_venue_read`; the node's
/// sentence is the client's, and the operator is told to update the gatewayhub (not that the venue offers no read).
#[test]
fn a_gatewayhub_that_predates_the_read_is_told_in_words_and_sent_nothing() {
    use vike_gatewayhub_client::proto::{FEATURE_GW_TRADE_VENUE_READ, TradeUp};
    use vike_model::VenueReadKind;
    use vike_tradehub::remote_backend::facts::LeaseFacts;
    use vike_tradehub::server::symbol_admission::SymbolFacts as _;
    let mut config = fake_trade_peer::TradePeerConfig::new();
    config.features.retain(|f| f != FEATURE_GW_TRADE_VENUE_READ);
    let peer = fake_trade_peer::TradePeer::start(config);
    let (session, _lane) = fake_trade_peer::open(&peer, "venue-read-old-daemon");
    let facts = LeaseFacts::default();
    facts.register("binance", session);

    let why = facts.venue_read("binance", VenueReadKind::Balance).expect_err("not advertised");
    assert_eq!(why, "the gatewayhub does not advertise venue reads (update it)");
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::VenueRead { .. })), "nothing was sent");
}

/// **A gatewayhub from the build that has the read but not the positions kind is told in words and sent
/// nothing, and its lease goes on.** That daemon decodes `VenueRead` but not the kind's name, and a trade frame
/// it cannot decode ENDS the lease, which would stop the account's trading; so the lease client asks for
/// positions only of a daemon that advertises `gw_trade_venue_positions`, and the older kinds still go.
#[test]
fn a_gatewayhub_with_the_read_but_not_the_positions_kind_is_told_in_words_and_sent_nothing() {
    use vike_gatewayhub_client::proto::{FEATURE_GW_TRADE_VENUE_POSITIONS, TradeUp};
    use vike_model::VenueReadKind;
    use vike_tradehub::remote_backend::facts::LeaseFacts;
    use vike_tradehub::server::symbol_admission::SymbolFacts as _;
    let mut config = fake_trade_peer::TradePeerConfig::new();
    config.features.retain(|f| f != FEATURE_GW_TRADE_VENUE_POSITIONS);
    let peer = fake_trade_peer::TradePeer::start(config);
    let (session, _lane) = fake_trade_peer::open(&peer, "venue-read-positions-old-daemon");
    let facts = LeaseFacts::default();
    facts.register("binance", session);

    let why = facts.venue_read("binance", VenueReadKind::Positions).expect_err("not advertised");
    assert_eq!(why, "the gatewayhub does not advertise position reads (update it)");
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::VenueRead { .. })), "nothing was sent");
    assert!(facts.venue_read("binance", VenueReadKind::Balance).is_ok(), "the lease goes on");
}
