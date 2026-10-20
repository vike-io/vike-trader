#![cfg(feature = "trade")]
//! **The HALT sentinel is checked at the tradehub's submit boundary, exactly as `ExecActor` checks it.**
//!
//! While the sentinel file exists an order that OPENS risk is refused with a terminal `OrderRejected`
//! (and never reaches the gatewayhub), a REDUCING submit is admitted so a halt can never trap an operator
//! in a position (`vike_exec::halt::halt_admits_submit` owns that rule), a modify is refused with the
//! non-terminal advisory, and a cancel or a confirm is never gated. The check is one `exists()` per call,
//! so a sentinel that appears or disappears mid-run is seen at the very next call.

use std::time::Duration;

use vike_exec::halt::HALT_REJECT_REASON;
use vike_exec::{CancelIntent, ExecutionClient};
use vike_gatewayhub_client::proto::TradeUp;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_trade_peer.rs"]
mod fake_trade_peer;

use fake_trade_peer::{
    TradePeer, TradePeerConfig, cancel_advisories, modify_advisories, open_with, order, rejects,
    wait_ups,
};

const WAIT: Duration = Duration::from_secs(5);

fn reducing(coid: &str) -> vike_model::OrderRequest {
    vike_model::OrderRequest { reduce_only: true, ..order(coid) }
}

struct Halted {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl Halted {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a scratch dir");
        let path = dir.path().join("HALT");
        Self { _dir: dir, path }
    }

    fn engage(&self) {
        std::fs::write(&self.path, b"").expect("touch the sentinel");
    }

    fn release(&self) {
        std::fs::remove_file(&self.path).expect("rm the sentinel");
    }
}

fn session_watching(
    peer: &TradePeer,
    halt: &Halted,
) -> (vike_gatewayhub_client::trade::TradeSession, fake_trade_peer::Lane) {
    let path = halt.path.clone();
    open_with(peer, "tradehub-1", move |link| link.with_halt_path(path))
}

fn commands_seen(peer: &TradePeer) -> Vec<TradeUp> {
    peer.ups().into_iter().filter(|f| fake_trade_peer::cseq_of(f).is_some()).collect()
}

#[test]
fn an_opening_submit_under_halt_is_a_terminal_reject_and_never_reaches_the_gatewayhub() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let halt = Halted::new();
    let (session, mut lane) = session_watching(&peer, &halt);
    let mut exec = session.exec_client();
    halt.engage();
    exec.submit(&order("open-1"));
    let got = lane.collect_until(WAIT, 1, |_| true);
    let rejected = rejects(&got);
    assert_eq!(rejected.len(), 1, "{got:?}");
    assert_eq!((rejected[0].0.as_str(), rejected[0].1.as_str()), ("open-1", HALT_REJECT_REASON));
    std::thread::sleep(Duration::from_millis(250));
    assert!(commands_seen(&peer).is_empty(), "the refused order was not sent");
}

#[test]
fn a_reducing_submit_is_admitted_so_a_halt_cannot_trap_an_operator_in_a_position() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let halt = Halted::new();
    let (session, mut lane) = session_watching(&peer, &halt);
    let mut exec = session.exec_client();
    halt.engage();
    exec.submit(&reducing("exit-1"));
    let sent = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::Submit { .. }));
    assert_eq!(sent.len(), 1);
    assert!(rejects(&lane.drain()).is_empty());
}

#[test]
fn a_modify_is_refused_with_an_advisory_and_cancels_and_confirms_are_never_gated() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let halt = Halted::new();
    let (session, mut lane) = session_watching(&peer, &halt);
    let mut exec = session.exec_client();
    halt.engage();
    exec.modify(&order("m1"), Some(3.0), None);
    exec.cancel_with_intent("k1", CancelIntent::RiskOff);
    exec.cancel("k2");
    exec.confirm("q1");
    let got = lane.collect_until(WAIT, 1, |_| true);
    let advisories = modify_advisories(&got);
    assert_eq!(advisories, [("m1".to_string(), HALT_REJECT_REASON.to_string())]);
    assert!(cancel_advisories(&got).is_empty(), "a cancel is never refused by a halt");
    let sent =
        wait_ups(&peer, WAIT, 3, |f| matches!(f, TradeUp::Cancel { .. } | TradeUp::Confirm { .. }));
    assert_eq!(sent.len(), 3);
    assert!(!commands_seen(&peer).iter().any(|f| matches!(f, TradeUp::Modify { .. })));
}

#[test]
fn a_batch_is_gated_order_by_order_and_the_survivors_travel_as_one_frame() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let halt = Halted::new();
    let (session, mut lane) = session_watching(&peer, &halt);
    let mut exec = session.exec_client();
    halt.engage();
    exec.submit_batch(&[order("open-a"), reducing("exit-b"), order("open-c")]);
    let sent = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::SubmitBatch { .. }));
    let TradeUp::SubmitBatch { orders, .. } = &sent[0] else { panic!("{sent:?}") };
    let ids: Vec<&str> = orders.iter().map(|o| o.client_order_id.as_str()).collect();
    assert_eq!(ids, ["exit-b"]);
    let got = lane.collect_until(WAIT, 2, |_| true);
    let mut rejected: Vec<String> = rejects(&got).into_iter().map(|(c, _)| c).collect();
    rejected.sort();
    assert_eq!(rejected, ["open-a", "open-c"], "one terminal for each refused order");
}

#[test]
fn the_sentinel_is_read_at_every_call_so_a_release_and_a_late_engage_are_seen_at_once() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let halt = Halted::new();
    let (session, mut lane) = session_watching(&peer, &halt);
    let mut exec = session.exec_client();

    exec.submit(&order("free-1")); // no sentinel yet
    halt.engage();
    exec.submit(&order("held-2")); // refused
    halt.release();
    exec.submit(&order("free-3")); // admitted again
    let sent: Vec<String> = wait_ups(&peer, WAIT, 2, |f| matches!(f, TradeUp::Submit { .. }))
        .into_iter()
        .filter_map(|f| match f {
            TradeUp::Submit { order, .. } => Some(order.client_order_id),
            _ => None,
        })
        .collect();
    assert_eq!(sent, ["free-1", "free-3"]);
    let rejected = rejects(&lane.collect_until(WAIT, 1, |_| true));
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].0, "held-2");
}

#[test]
fn a_session_with_no_sentinel_path_refuses_nothing() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = fake_trade_peer::open(&peer, "tradehub-1");
    let mut exec = session.exec_client();
    exec.submit(&order("plain-1"));
    let sent = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::Submit { .. }));
    assert_eq!(sent.len(), 1);
    assert!(rejects(&lane.drain()).is_empty());
}
