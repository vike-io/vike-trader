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
//!   that is away still is.

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
