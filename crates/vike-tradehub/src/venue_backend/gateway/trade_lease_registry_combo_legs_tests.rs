//! A combo's LEGS take the admission's commit to the gatewayhub (`TradeLeaseRegistry::tell_combo_legs`), over the gatewayhub
//! client's own fake gatewayhub (`vike_gatewayhub_client::test_support::fake_trade_peer`): the daemon's reconcile client
//! is told each leg once, a restarted gatewayhub is told it again from this node's record of what it told, a commit the
//! gatewayhub could not answer is asked again on the reaper's tick (bounded), and a leg nothing needs is forgotten.

use std::sync::atomic::{AtomicUsize, Ordering};

use vike_gatewayhub_client::proto::{
    PROPERTIES_BUSY, SYMBOL_FORGOTTEN, SymbolPropertiesRefusal, SymbolPropertiesRefusalCode,
    TradeDown, TradeUp,
};
use vike_gatewayhub_client::test_support::fake_trade_peer::{
    Lane, Step, TradePeer, TradePeerConfig, default_script, open, symbol_facts,
};
use vike_log::poll::wait_until;

use super::*;

/// The legs of the captured testnet combo (`BTC-FS-25DEC26_20JUL26`).
const LEGS: [&str; 2] = ["BTC-25DEC26", "BTC-20JUL26"];
/// The combo that named them.
const COMBO: &str = "combo-1";

/// A daemon that answers each commit BUSY while `busy` counts down (`usize::MAX`: for ever), refuses one leg's facts (a
/// coin-settled option), grants every other commit and forgets what it is asked to.
fn leg_peer(busy: Arc<AtomicUsize>) -> TradePeer {
    let rest = default_script();
    TradePeer::start(TradePeerConfig::new().with_script(move |frame| match frame {
        TradeUp::SymbolProperties { rid, forget: true, .. } => {
            vec![Step::Send(TradeDown::SymbolPropertiesReply {
                rid: *rid,
                properties: Err(SymbolPropertiesRefusal {
                    code: SymbolPropertiesRefusalCode::Forgotten,
                    reason: SYMBOL_FORGOTTEN.to_string(),
                }),
            })]
        }
        TradeUp::SymbolProperties { rid, symbol, .. } => {
            let busy_now = busy
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| match n {
                    0 => None,
                    usize::MAX => Some(usize::MAX),
                    n => Some(n - 1),
                })
                .is_ok();
            let facts = if busy_now {
                Err(SymbolPropertiesRefusal {
                    code: SymbolPropertiesRefusalCode::Busy,
                    reason: PROPERTIES_BUSY.to_string(),
                })
            } else if symbol == "BTC-27MAR26-100000-C" {
                Err(SymbolPropertiesRefusal {
                    code: SymbolPropertiesRefusalCode::Verdict,
                    reason: format!("deribit: {symbol} is a coin-settled option"),
                })
            } else {
                Ok(symbol_facts(10.0, None))
            };
            vec![Step::Send(TradeDown::SymbolPropertiesReply { rid: *rid, properties: facts })]
        }
        other => rest(other),
    }))
}

/// The symbols the daemon was COMMITTED (a `SymbolProperties` request that is neither a look nor a forget), in order.
fn commits(peer: &TradePeer) -> Vec<String> {
    peer.ups()
        .into_iter()
        .filter_map(|frame| match frame {
            TradeUp::SymbolProperties { symbol, probe: false, forget: false, .. } => Some(symbol),
            _ => None,
        })
        .collect()
}

/// The symbols the daemon was told to FORGET, in order.
fn forgets(peer: &TradePeer) -> Vec<String> {
    peer.ups()
        .into_iter()
        .filter_map(|frame| match frame {
            TradeUp::SymbolProperties { symbol, forget: true, .. } => Some(symbol),
            _ => None,
        })
        .collect()
}

/// A registry over one lease of `peer` under the route key `binance`, the lease, and the core lane it delivers to
/// (kept alive by the caller).
fn registry(peer: &TradePeer) -> (Arc<TradeLeaseRegistry>, TradeLease, Lane) {
    let (session, lane) = open(peer, "node");
    let facts = Arc::new(TradeLeaseRegistry::default());
    facts.register("binance", session.clone());
    (facts, session, lane)
}

/// What the reaper reads off the published engines: `binance` is known, holds `held` and lists `live` as live orders.
fn published(held: &[&str], live: &[&str]) -> crate::readmit::ReadmitHoldings {
    crate::readmit::ReadmitHoldings {
        known: ["binance".to_string()].into(),
        held: held
            .iter()
            .map(|symbol| crate::readmit::ReadmitTarget {
                route_key: "binance".into(),
                venue: "binance".into(),
                symbol: (*symbol).into(),
                flat: false,
            })
            .collect(),
        live_orders: live.iter().map(|order_client_id| (*order_client_id).to_string()).collect(),
        ..Default::default()
    }
}

/// Pretend `leg` was last named by a combo `ago` ago (and its commit owed since then when `owed`).
fn named_ago(facts: &TradeLeaseRegistry, leg: &str, ago: Duration, owed: bool) {
    let then = Instant::now().checked_sub(ago).unwrap();
    let mut kept = facts.combo_legs.lock().unwrap();
    let entry = kept.get_mut(&("binance".to_string(), leg.to_string())).expect("a kept leg");
    entry.named = then;
    if owed {
        entry.owed = Some(then);
    }
}

fn kept(facts: &TradeLeaseRegistry, leg: &str) -> Option<ComboLeg> {
    facts.combo_legs.lock().unwrap().get(&("binance".to_string(), leg.to_string())).cloned()
}

/// **An accepted combo's legs are committed to the daemon once each, and a restarted gatewayhub is told them again.**
/// The core writes a combo's legs into its engine with no `AdmitSymbol`, so the daemon's reconcile client never read a
/// leg, and after a gatewayhub restart (the bridge's memory of the leg's fills gone) a held leg was an
/// `OrphanLocalPosition` on every pass. Now the core's hook hands the legs to the registry's thread, the commit tells
/// the daemon (the lease records it), a second combo on the same legs asks nothing, and the re-teaching after a
/// restart tells the new daemon the legs with every admitted symbol. A leg the daemon refuses is not recorded, and an
/// account with no lease (a `paper` engine) asks nothing. None of it is an admission of the registry.
#[test]
fn a_combos_legs_are_committed_once_and_told_again_to_a_restarted_gatewayhub() {
    let peer = leg_peer(Arc::default());
    let (facts, session, _lane) = registry(&peer);
    let legs: Vec<String> = LEGS.map(String::from).to_vec();

    let tell = facts.combo_leg_teller();
    tell("binance", COMBO, legs.as_slice());
    assert!(
        wait_until(Duration::from_secs(5), || legs.iter().all(|leg| session.has_told(leg))),
        "the hook's thread commits the legs"
    );
    // A second combo on the same legs.
    facts.tell_combo_legs("binance", &["combo-2".to_string()], &legs);
    assert_eq!(commits(&peer), LEGS, "one commit per leg, whatever the combos");
    assert!(!facts.admitted_on_venue("binance", LEGS[0]), "a leg takes no place of the registry's");
    assert_eq!(kept(&facts, LEGS[0]).expect("kept").combos, [COMBO, "combo-2"]);

    facts.tell_combo_legs("binance", &[], &["BTC-27MAR26-100000-C".to_string()]);
    assert!(!session.has_told("BTC-27MAR26-100000-C"), "a refused leg is not recorded");
    assert_eq!(kept(&facts, "BTC-27MAR26-100000-C").expect("kept").owed, None, "nor asked again");
    facts.tell_combo_legs("binance#PAPER", &[], &["BTC-27JUN26".to_string()]);
    assert_eq!(commits(&peer).len(), 3, "an account with no lease asks nothing");

    peer.restart();
    assert!(
        wait_until(Duration::from_secs(8), || !session.has_told(LEGS[0])
            && session.link_state() == TradeLinkState::Up),
        "the reconnect to the new epoch owes the legs"
    );
    assert!(session.retell_symbols().is_empty(), "the new daemon grants them");
    assert!(
        legs.iter().all(|leg| session.has_told(leg)),
        "the restarted gatewayhub reads the legs"
    );
    assert_eq!(commits(&peer)[3..], LEGS, "told again, from this node's record");
}

/// **A leg the gatewayhub could not be told is asked again on the reaper's tick, and given up after the window.** The
/// first commit meets a busy gatewayhub: the leg is OWED, the next tick asks again and the daemon takes it. A gatewayhub
/// that stays busy is asked on every tick for [`COMBO_LEG_RETRY_FOR`] and then no more.
#[test]
fn a_leg_the_gatewayhub_could_not_be_told_is_asked_again_on_the_tick_until_the_window_ends() {
    let busy = Arc::new(AtomicUsize::new(1));
    let peer = leg_peer(Arc::clone(&busy));
    let (facts, session, _lane) = registry(&peer);
    let held = published(&LEGS, &[]);

    facts.tell_combo_legs("binance", &[COMBO.to_string()], &[LEGS[0].to_string()]);
    assert!(!session.has_told(LEGS[0]), "the busy gatewayhub took nothing");
    assert!(kept(&facts, LEGS[0]).expect("kept").owed.is_some(), "the commit is owed");
    facts.settle_combo_legs(&held);
    assert!(session.has_told(LEGS[0]), "the tick asked again and the daemon took it");
    assert_eq!(kept(&facts, LEGS[0]).expect("kept").owed, None);
    assert_eq!(commits(&peer), [LEGS[0], LEGS[0]]);

    busy.store(usize::MAX, Ordering::SeqCst);
    facts.tell_combo_legs("binance", &[COMBO.to_string()], &[LEGS[1].to_string()]);
    facts.settle_combo_legs(&held);
    assert_eq!(commits(&peer).len(), 4, "asked again while the window runs");
    named_ago(&facts, LEGS[1], COMBO_LEG_RETRY_FOR + Duration::from_secs(1), true);
    facts.settle_combo_legs(&held);
    facts.settle_combo_legs(&held);
    assert_eq!(commits(&peer).len(), 4, "past the window it is asked no more");
    assert_eq!(kept(&facts, LEGS[1]).expect("still kept for its release").owed, None);
    assert!(!session.has_told(LEGS[1]));
}

/// **A leg nothing needs any more is forgotten at the daemon; a leg still held as a position stays.** While a combo that
/// named the legs is live, or one named them within [`COMBO_LEG_QUIET`], both are read. Then the leg the engine holds
/// a position in stays read and the flat one is forgotten (its read and its place in the daemon's list go, and the
/// lease's record with it), once; a leg the door admitted as well is the admission's and is not forgotten here.
#[test]
fn a_leg_no_combo_and_no_position_needs_is_forgotten_and_a_held_one_stays() {
    let peer = leg_peer(Arc::default());
    let (facts, session, _lane) = registry(&peer);
    let legs: Vec<String> = LEGS.map(String::from).to_vec();
    facts.tell_combo_legs("binance", &[COMBO.to_string()], &legs);
    assert!(legs.iter().all(|leg| session.has_told(leg)));
    let near_held = published(&[LEGS[0]], &[]);

    facts.settle_combo_legs(&near_held);
    assert!(forgets(&peer).is_empty(), "a combo named them a moment ago");
    for leg in LEGS {
        named_ago(&facts, leg, COMBO_LEG_QUIET + Duration::from_secs(1), false);
    }
    facts.settle_combo_legs(&published(&[LEGS[0]], &[COMBO]));
    assert!(forgets(&peer).is_empty(), "the combo still rests");

    facts.settle_combo_legs(&near_held);
    assert_eq!(forgets(&peer), [LEGS[1]], "the flat leg no combo needs is forgotten");
    assert!(session.has_told(LEGS[0]) && !session.has_told(LEGS[1]));
    assert!(kept(&facts, LEGS[0]).is_some() && kept(&facts, LEGS[1]).is_none());
    facts.settle_combo_legs(&near_held);
    assert_eq!(forgets(&peer), [LEGS[1]], "once");

    // Flat now, but the door admitted it too: the admission owns the read.
    SymbolAdmitter::admitted(&*facts, "binance", "binance", LEGS[0]);
    facts.settle_combo_legs(&published(&[], &[]));
    assert_eq!(forgets(&peer), [LEGS[1]], "an admitted symbol is not forgotten as a leg");
    assert!(kept(&facts, LEGS[0]).is_none() && session.has_told(LEGS[0]));
}
