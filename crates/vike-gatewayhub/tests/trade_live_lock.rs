//! **The live-account lock is claimed under the PRODUCTION table spelling** (wave 2, W11c).
//!
//! `trade_hub.rs`'s lock tests hand the hub a HAND-MADE arming table, so they stayed green while the
//! table the daemon really serves (`BridgeTrade`, over `registry::trade::REGISTRY`) spelled its
//! blocks in a word the session no longer understood: `block_runs` compared against
//! `ArmingBlock::as_str()` ("" for a running row) while the table wrote `ArmingBlock::word()`
//! ("none"), `armed_live()` was false for every live row, and NOTHING claimed `LIVE-<route>.lock`.
//!
//! These tests drive the real lane's table into `TradeHub::open`, so a producer and a consumer that
//! disagree about the spelling fail here, and the refusal paths are decoded the way the tradehub
//! decodes them (`ArmingBlock::from_word`).
#![cfg(feature = "trade")]

use std::sync::Arc;
use std::time::Duration;

use vike_boot::live_lock::LiveLock;
use vike_config::ArmingBlock;
use vike_gatewayhub::budget::ledger::Budget;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::testkit::trade::{ScriptedStart, ScriptedTrade};
use vike_gatewayhub::trade::TradeConfig;
use vike_gatewayhub::trade::adapters::bridge_trade::BridgeTrade;
use vike_gatewayhub::trade::adapters::snapshot::TradeSnapshot;
use vike_gatewayhub::trade::lane::TradeLane;
use vike_gatewayhub::trade::session::{OpenRefusal, OpenRequest, TradeHub};
use vike_gatewayhub_client::proto::{AdapterReply, DetachPhase, TradeDown, TradeUp};
use vike_model::AccountTier;

#[path = "trade_roster/support/account_book.rs"]
mod account_book;
#[path = "trade_roster/support/arming_fixtures.rs"]
mod arming_fixtures;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use trade_rig::start_args;

fn cfg(state_dir: &std::path::Path) -> TradeConfig {
    TradeConfig {
        heartbeat: Duration::from_millis(100),
        read_floor: Duration::from_millis(300),
        ring_events: 64,
        dedup_coids: 64,
        command_queue: 64,
        control_queue: 64,
        event_channel: 64,
        max_leases: 8,
        orphan_ttl: Duration::from_secs(600),
        recon_progress: Duration::from_millis(100),
        recon_page_bytes: 4096,
        fence_drain: Duration::from_millis(500),
        account_ccy_retry_after: vike_gatewayhub::trade::ACCOUNT_CCY_RETRY_AFTER,
        state_dir: Some(state_dir.to_path_buf()),
        halt_path: None,
    }
}

fn request(client_id: &str, tier: AccountTier) -> OpenRequest {
    OpenRequest {
        client_id: client_id.to_string(),
        venue: "binance".to_string(),
        account: None,
        tier,
        resume: None,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    }
}

/// The REAL lane over the real registry: a binance DEFAULT account at `tier` holding a complete key set.
fn production_lane(tier: AccountTier) -> BridgeTrade {
    let policy =
        account_book::keyed_policy("binance", tier, arming_fixtures::armed_fields("binance"));
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
}

fn hub_over(lane: impl TradeLane + 'static, dir: &std::path::Path) -> Arc<TradeHub> {
    TradeHub::new(Arc::new(lane), cfg(dir), Arc::new(Budget::new()))
}

fn held(dir: &std::path::Path) -> bool {
    LiveLock::acquire(dir, "binance")
        .map(drop)
        .is_err_and(|e| e.kind() == std::io::ErrorKind::AddrInUse)
}

/// **The claim, asked of what the daemon really serves.** A running live row of the production table
/// carries `block_word` "none"; the session must read it as RUNNING and claim `LIVE-binance.lock`.
#[test]
fn the_production_table_arms_the_live_lock_claim_and_the_final_detach_frees_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let lane = production_lane(AccountTier::Live);
    let table = lane.arming_table();
    let row = table.rows.iter().find(|r| r.venue == "binance").expect("binance has a row");
    assert_eq!(row.effective, Some(AccountTier::Live), "the fixture arms binance live");
    assert_eq!(
        ArmingBlock::from_word(&row.block_word),
        Some(ArmingBlock::None),
        "the production table spells a running row in the wire word: {:?}",
        row.block_word
    );

    // The production table over a scripted lane: the claim is read from the table alone, and a scripted
    // adapter can start where the real binance bridge would dial the venue.
    let hub = hub_over(
        ScriptedTrade::new(&["binance"])
            .with_tier("binance", None, Ok(AccountTier::Live))
            .with_arming(table),
        dir.path(),
    );
    let opened = hub.open(request("tradehub-1", AccountTier::Live)).expect("a live account opens");
    assert!(
        held(dir.path()),
        "LIVE-binance.lock is held while the session lives: a second process-level claim must be \
         refused with the lock's own AddrInUse"
    );
    // A flap keeps the claim; the session's, not the connection's. ⚠ Only a session that RUNS an adapter
    // outlives its lease: the reaper (every `REAPER_TICK`) retires a leaseless session with no adapter and
    // no supervisor at once, so without this start the claim is dropped whenever a tick lands between the
    // flap and the check (the CI flake under load).
    assert!(matches!(next_reply(&opened.lease), AdapterReply::Live { .. }));
    opened.lease.close();
    opened.lease.finish();
    assert!(held(dir.path()), "a lease flap keeps the claim");
    let again = hub.open(request("tradehub-1", AccountTier::Live)).expect("reopens");
    again.lease.handle(TradeUp::Detach { phase: DetachPhase::Final });
    again.lease.close();
    again.lease.finish();
    assert!(!held(dir.path()), "the claim was not released at the final detach");
}

/// A demo account of the production table claims nothing (the other side of the same predicate).
#[test]
fn a_demo_account_of_the_production_table_claims_no_lock() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let hub = hub_over(production_lane(AccountTier::Demo), dir.path());
    let _opened = hub.open(request("tradehub-1", AccountTier::Demo)).expect("demo opens");
    assert!(!held(dir.path()), "a demo account claims no LIVE-*.lock");
}

/// **A lock another process holds refuses the WHOLE live mount**, which on the wire is a LEASE refusal
/// (the tradehub records `LeaseHeld` into `lease_refusals`; a `TradeRefused` is only that account's), in
/// the lock's own words.
#[test]
fn a_live_lock_another_process_holds_is_a_lease_refusal_in_the_locks_own_words() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let other = LiveLock::acquire(dir.path(), "binance").expect("held elsewhere");
    let hub = hub_over(production_lane(AccountTier::Live), dir.path());
    match hub.open(request("tradehub-1", AccountTier::Live)) {
        Err(OpenRefusal::LeaseHeld { holder_key_id, .. }) => {
            assert!(
                holder_key_id.contains("another live process already trades the binance account"),
                "{holder_key_id}"
            );
        }
        Err(other) => panic!("a lock refusal is a LeaseHeld, got {other:?}"),
        Ok(opened) => panic!("the open was accepted: {:?}", opened.reply),
    }
    assert_eq!(hub.sessions(), 0, "a refused open leaves no session behind");
    drop(other);
    assert!(hub.open(request("tradehub-1", AccountTier::Live)).is_ok());
}

// ---- the refusal words, decoded the way the tradehub decodes them -------------------------------------

fn scripted_live(tier: Result<AccountTier, ArmingBlock>) -> ScriptedTrade {
    ScriptedTrade::new(&["binance"]).with_tier("binance", None, tier)
}

/// `TradeRefused` for the account's own state: `TierConflict` crosses as a word `from_word` knows.
#[test]
fn a_refusal_for_the_accounts_state_decodes_to_the_same_block() {
    let dir = tempfile::tempdir().expect("a temp dir");
    for block in
        [ArmingBlock::TierConflict, ArmingBlock::AccountInactive, ArmingBlock::NoAccountRow]
    {
        let hub = hub_over(scripted_live(Err(block)), dir.path());
        match hub.open(request("tradehub-1", AccountTier::Live)) {
            Err(OpenRefusal::Refused { block_word, text }) => {
                assert_eq!(ArmingBlock::from_word(&block_word), Some(block), "{block_word:?}");
                assert!(text.contains(block.as_str()), "the sentence keeps the badge: {text}");
            }
            other => panic!("{block:?}: {:?}", other.map(|o| o.reply)),
        }
    }
}

fn next_reply(lease: &Arc<vike_gatewayhub::trade::lease::Lease>) -> AdapterReply {
    use vike_gatewayhub::trade::lease::{Flow, Next};
    let mut s = start_args();
    s.rid = 7;
    s.route_key = "binance".into();
    assert_eq!(lease.handle(TradeUp::AdapterStart(s)), Flow::Continue);
    match lease.next(Duration::from_millis(500)) {
        Next::Frame(TradeDown::AdapterReply { rid: 7, reply }) => *reply,
        other => panic!("{other:?}"),
    }
}

/// `AdapterStart` the bridge refuses: the refusal's block crosses as a word `from_word` knows.
#[test]
fn a_start_refusal_decodes_to_the_same_block() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let lane = scripted_live(Ok(AccountTier::Demo)).with_start(
        "binance",
        None,
        ScriptedStart::refuse(ArmingBlock::NoCredentials, "binance: no demo key set is stored"),
    );
    let hub = hub_over(lane, dir.path());
    let opened = hub.open(request("tradehub-1", AccountTier::Demo)).expect("opens");
    match next_reply(&opened.lease) {
        AdapterReply::NotRunning { block_word, .. } => {
            assert_eq!(ArmingBlock::from_word(&block_word), Some(ArmingBlock::NoCredentials));
        }
        other => panic!("{other:?}"),
    }
}

/// The tier interlock's reply (a client bound at another tier) is `adapter failed` in the wire word.
#[test]
fn the_tier_interlock_reply_decodes_to_adapter_failed() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let lane = scripted_live(Ok(AccountTier::Demo)).with_start(
        "binance",
        None,
        ScriptedStart::live().bound_tier(AccountTier::Live),
    );
    let hub = hub_over(lane, dir.path());
    let opened = hub.open(request("tradehub-1", AccountTier::Demo)).expect("opens");
    match next_reply(&opened.lease) {
        AdapterReply::NotRunning { block_word, .. } => {
            assert_eq!(ArmingBlock::from_word(&block_word), Some(ArmingBlock::AdapterFailed));
        }
        other => panic!("{other:?}"),
    }
}

/// **The claim comes BEFORE any venue client** (`LiveLock::acquire`'s doc: three venue arms post
/// `set_leverage` while constructing their exec client): a refused claim leaves the lane never asked to
/// start an adapter, and a session with a held claim starts one only after the claim.
#[test]
fn a_refused_claim_never_reaches_the_lane_and_the_table_is_the_production_one() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let table = production_lane(AccountTier::Live).arming_table();
    let lane = ScriptedTrade::new(&["binance"])
        .with_tier("binance", None, Ok(AccountTier::Live))
        .with_arming(table);
    let other = LiveLock::acquire(dir.path(), "binance").expect("held elsewhere");
    let hub = hub_over(lane.clone(), dir.path());
    assert!(matches!(
        hub.open(request("tradehub-1", AccountTier::Live)),
        Err(OpenRefusal::LeaseHeld { .. })
    ));
    assert_eq!(lane.starts(), 0, "no venue client exists behind a refused claim");
    drop(other);
    let opened = hub.open(request("tradehub-1", AccountTier::Live)).expect("the claim is free");
    assert!(held(dir.path()));
    assert!(matches!(next_reply(&opened.lease), AdapterReply::Live { .. }));
    assert_eq!(lane.starts(), 1, "the adapter started once the claim was held");
}
