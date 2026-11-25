//! **A running account the operator RENAMES keeps its session** (`AccountSessions::open`'s tier check of a label that
//! names no row any more): the tradehub keeps the account's one engine under the label it started with
//! (`vike-tradehub`'s `Watcher::follow_renames`), so when its link drops it reopens THAT label. The session's
//! adapter still holds the row's keys, so the reopen is judged by what the ROW answers now, under its new label;
//! a label that names no row and no running session still opens nothing. After a gatewayhub restart there is no
//! session to remember the row, so the tradehub names it (`TradeOpen.row`, [`AccountSessions::open_at_row`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{AdapterReply, AdapterStart, TradeDown, TradeUp};
use vike_model::ArmingBlock;
use vike_model::{AccountTier, HaltAdmit, OrderRequest};

use super::{AccountSessions, TradeLeaseOpenRefusal, TradeLeaseOpenRequest, word};
use crate::budget::ledger::Budget;
use crate::testkit::trade::{ExecutionCall, ScriptedAdapter, TradeCall, TradeLaneScript};
use crate::testkit::trade_rig::hub_config;
use crate::trade::lease::{TradeLeaseHold, TradeLeaseHoldFlow, TradeLeaseHoldNext};

const WAIT: Duration = Duration::from_secs(5);

fn hub_over(lane: &TradeLaneScript) -> Arc<AccountSessions> {
    AccountSessions::new(Arc::new(lane.clone()), hub_config(), Arc::new(Budget::new()))
}

/// The tradehub's open of binance's `OLD` account (the label its engine started under).
fn open_old() -> TradeLeaseOpenRequest {
    TradeLeaseOpenRequest {
        client_id: "tradehub-1".to_string(),
        venue: "binance".to_string(),
        account: Some("OLD".to_string()),
        tier: AccountTier::Demo,
        resume: None,
        takeover: false,
        key_id: Some("nk-test".to_string()),
        closer: Arc::new(|| {}),
    }
}

fn start() -> AdapterStart {
    AdapterStart {
        rid: 7,
        symbol: "BTCUSDT".to_string(),
        declared_legs: Vec::new(),
        reconcile_enabled: true,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        read_account_currency: false,
        route_key: "binance#OLD".to_string(),
    }
}

/// `lease`'s adapter start is answered `Live`.
fn started(lease: &Arc<TradeLeaseHold>) {
    assert_eq!(lease.handle(TradeUp::AdapterStart(start())), TradeLeaseHoldFlow::Continue);
    loop {
        match lease.next(WAIT) {
            TradeLeaseHoldNext::Frame(TradeDown::AdapterReply { rid: 7, reply }) => {
                assert!(matches!(*reply, AdapterReply::Running { .. }), "{reply:?}");
                return;
            }
            TradeLeaseHoldNext::Frame(_) => {}
            other => panic!("expected the adapter reply, got {other:?}"),
        }
    }
}

/// Row 2 runs as `OLD`, with one order resting at its adapter.
fn running_old(
    lane: &TradeLaneScript,
    hub: &AccountSessions,
) -> (Arc<TradeLeaseHold>, ScriptedAdapter) {
    let lease =
        hub.open(open_old()).unwrap_or_else(|e| panic!("the open is accepted: {e:?}")).lease;
    started(&lease);
    let order = OrderRequest {
        order_client_id: "resting-1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        price: Some(100.0),
        timestamp: 1_700_000_000_000,
        ..Default::default()
    };
    assert_eq!(
        lease.handle(TradeUp::Submit { command_sequence: 1, order }),
        TradeLeaseHoldFlow::Continue
    );
    let adapter = lane.adapter("binance", Some("OLD")).expect("the adapter was started");
    let deadline = Instant::now() + WAIT;
    while adapter.submitted().is_empty() {
        assert!(Instant::now() < deadline, "the order never reached the adapter");
        std::thread::sleep(Duration::from_millis(5));
    }
    (lease, adapter)
}

/// The link drops: what the connection's guard does when its reader leaves (`server::trade`'s `EndLease`).
fn link_drops(lease: &Arc<TradeLeaseHold>) {
    lease.close();
    lease.finish();
}

fn cancels(adapter: &ScriptedAdapter) -> Vec<ExecutionCall> {
    let calls = adapter.calls().into_iter();
    calls
        .filter(|c| matches!(c, ExecutionCall::Cancel { .. } | ExecutionCall::CancelBatch { .. }))
        .collect()
}

/// Rename, then the link drops, then the lease reopens: on the SAME session (its adapter, the row's keys), one
/// lease, no second adapter, the resting order untouched; and the re-sent start is answered by the running adapter.
#[test]
fn a_renamed_running_account_reopens_its_lease_after_a_link_drop() {
    let lane = TradeLaneScript::new(&["binance"]).with_row(2, "binance", Some("OLD"));
    let hub = hub_over(&lane);
    let (lease, adapter) = running_old(&lane, &hub);

    lane.rename_row(2, Some("NEW"));
    link_drops(&lease);
    assert_eq!(hub.leases(), 0, "the link dropped");

    let reopened = hub
        .open(open_old())
        .unwrap_or_else(|e| panic!("the renamed account's lease must reopen: {e:?}"))
        .lease;
    assert_eq!(hub.leases(), 1, "one lease on row 2");
    assert_eq!(hub.sessions(), 1, "the same session, no second one");
    started(&reopened);
    assert_eq!(lane.starts(), 1, "no second adapter on row 2's keys");
    assert!(!adapter.is_detached(), "the adapter that holds the order kept running");
    assert_eq!(
        cancels(&adapter),
        Vec::<ExecutionCall>::new(),
        "the reopen cancelled a resting order"
    );
    hub.end_attached_leases();
    hub.shutdown();
}

/// The row's answer under its new label is the reopen's: a row deactivated since the rename is refused for what
/// it is (`account inactive`), and a label that names no row with no running session behind it opens nothing.
#[test]
fn a_reopen_by_the_row_takes_the_rows_answer_and_a_bare_gone_label_opens_nothing() {
    let lane = TradeLaneScript::new(&["binance"]).with_row(2, "binance", Some("OLD"));
    let hub = hub_over(&lane);
    let (lease, adapter) = running_old(&lane, &hub);
    lane.rename_row(2, Some("NEW"));
    lane.set_tier("binance", Some("NEW"), Err(ArmingBlock::AccountInactive));
    link_drops(&lease);
    match hub.open(open_old()).map(|_| ()) {
        Err(TradeLeaseOpenRefusal::Refused { block_word, .. }) => {
            assert_eq!(block_word, word(ArmingBlock::AccountInactive));
        }
        other => panic!("a deactivated row reopens nothing: {other:?}"),
    }
    assert_eq!(cancels(&adapter), Vec::<ExecutionCall>::new(), "a refused reopen cancels nothing");
    hub.shutdown();

    let lane = TradeLaneScript::new(&["binance"]).with_row(2, "binance", Some("OLD"));
    lane.rename_row(2, Some("NEW"));
    let hub = hub_over(&lane);
    match hub.open(open_old()).map(|_| ()) {
        Err(TradeLeaseOpenRefusal::Refused { block_word, .. }) => {
            assert_eq!(block_word, word(ArmingBlock::NoAccountRow));
        }
        other => panic!("a label that names no row opens no session: {other:?}"),
    }
    assert_eq!(hub.sessions(), 0);
    hub.shutdown();
}

/// The label of every adapter start the lane was asked for, in order: the account the lane read the keys of.
fn started_accounts(lane: &TradeLaneScript) -> Vec<Option<String>> {
    let calls = lane.calls().into_iter();
    calls
        .filter_map(|c| match c {
            TradeCall::Start { target, .. } => Some(target.account),
            _ => None,
        })
        .collect()
}

/// **A gatewayhub restart after a rename**: the daemon comes back with no session and no memory of the row, and the
/// tradehub's re-teach opens the label its engine started under, naming the row. The open is judged by that row
/// and the adapter starts on ITS keys: the lane is asked for the account under the label the row carries now. One
/// session, one lease, one adapter, nothing cancelled.
#[test]
fn a_gatewayhub_restart_after_a_rename_reopens_and_starts_on_the_rows_keys() {
    let lane = TradeLaneScript::new(&["binance"]).with_row(2, "binance", Some("OLD"));
    lane.rename_row(2, Some("NEW"));
    let hub = hub_over(&lane);
    let opened = hub
        .open_at_row(open_old(), Some(2))
        .unwrap_or_else(|e| panic!("the re-teach of a renamed account must open: {e:?}"));
    started(&opened.lease);
    assert_eq!(
        started_accounts(&lane),
        [Some("NEW".to_string())],
        "the adapter starts on row 2's keys, under the label the row carries now"
    );
    assert_eq!((hub.sessions(), hub.leases()), (1, 1), "one session, one lease");
    let adapter = lane.adapter("binance", Some("NEW")).expect("the adapter was started");
    assert_eq!(cancels(&adapter), Vec::<ExecutionCall>::new(), "the re-teach cancelled nothing");
    hub.end_attached_leases();
    hub.shutdown();
}
