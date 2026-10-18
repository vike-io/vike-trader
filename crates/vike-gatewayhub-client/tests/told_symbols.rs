#![cfg(feature = "trade")]
//! **What the daemon's reconcile client has been TOLD about the symbols the engine admitted**
//! (decisions 0173 and 0174), as the node can know it: the lease remembers every granted
//! `SymbolFacts` request (the daemon tells its reconcile client on each grant), says so on the
//! `ReconClient` it hands the reconcile manager, forgets all of it when the gatewayhub restarts
//! (a new daemon knows nothing), and can ask for them again.
//!
//! The list is what the core's restore step reads to decide whether the venue's silence about an
//! order on an admitted symbol means "gone": it may only claim what the daemon's client really
//! reads, so it is bounded as the client's own list is, and it never holds a refused symbol.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_exec::MAX_EXTRA_SYMBOLS;
use vike_exec::recon::ReconClient;
use vike_gatewayhub_client::proto::{TradeDown, TradeUp, WireSymbolFacts};
use vike_model::SymbolProperties;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_trade_peer.rs"]
mod fake_trade_peer;

use fake_trade_peer::{Step, TradePeer, TradePeerConfig, default_script, open};

const WAIT: Duration = Duration::from_secs(5);
/// The one symbol the default fake daemon grants facts for.
const SOL: &str = "SOLUSDT.P";

fn facts() -> WireSymbolFacts {
    WireSymbolFacts {
        properties: SymbolProperties {
            tick_size: 0.01,
            step_size: 1.0,
            ..SymbolProperties::default()
        },
        contract_value: None,
    }
}

fn facts_asked(peer: &TradePeer) -> usize {
    peer.ups().iter().filter(|f| matches!(f, TradeUp::SymbolFacts { .. })).count()
}

/// A daemon that grants every `*.P` symbol and refuses everything else, and refuses ALL while
/// `refuse_all` is set.
fn grants_perps(refuse_all: Arc<AtomicBool>) -> TradePeerConfig {
    let base = default_script();
    TradePeerConfig::new().with_script(move |frame| match frame {
        TradeUp::SymbolFacts { rid, symbol, .. } => vec![Step::Send(TradeDown::SymbolFactsReply {
            rid: *rid,
            facts: if !refuse_all.load(Ordering::SeqCst) && symbol.ends_with(".P") {
                Ok(facts())
            } else {
                Err(format!("binance: no instrument {symbol}"))
            },
        })],
        other => base(other),
    })
}

fn told(session: &vike_gatewayhub_client::trade::TradeSession) -> Option<Vec<String>> {
    session.recon_client().admitted_symbols()
}

#[test]
fn a_granted_commit_is_told_and_a_refused_one_is_not() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-told-1");
    assert_eq!(
        told(&session),
        Some(Vec::new()),
        "a list, empty: a claim of nothing, not an absence"
    );
    assert!(!session.has_told(SOL));

    assert!(session.admit_symbol(SOL).is_ok());
    assert!(session.admit_symbol("NOPE").is_err());

    assert_eq!(told(&session), Some(vec![SOL.to_string()]));
    assert!(session.has_told(SOL));
    assert!(!session.has_told("NOPE"), "a refusal tells the daemon's client nothing");
    // A repeat is not a second slot.
    assert!(session.admit_symbol(SOL).is_ok());
    assert_eq!(told(&session), Some(vec![SOL.to_string()]));
}

/// **A venue whose daemon client ignores `admit_symbol` claims nothing, whatever the lease told it.**
/// okx is an `Admits` venue but its reconcile client reads the mounted instrument only: a mirrored
/// claim would let the restore planner retire a resting order on an admitted instrument as "gone".
/// The planner's HOLD on `Some([])` is `vike-tradehub-core`'s `recon_admitted_scope`.
#[test]
fn a_venue_whose_client_reads_only_the_mounted_symbol_claims_nothing() {
    let mut config = TradePeerConfig::new();
    config.features.push(vike_gatewayhub_client::proto::trade_venue_feature("okx"));
    config.route_key = "okx".to_string();
    let peer = TradePeer::start(config);
    let (events, _lane) = fake_trade_peer::lane();
    let account = vike_gatewayhub_client::trade::TradeAccount {
        venue: "okx".to_string(),
        route_key: "okx".to_string(),
        ..fake_trade_peer::account("tradehub-told-okx")
    };
    let session = vike_gatewayhub_client::trade::TradeLink::new(peer.link(), account, events)
        .with_timings(fake_trade_peer::fast_trade_timings())
        .open()
        .expect("the fake peer opens the lease");
    assert!(session.admit_symbol(SOL).is_ok());
    assert!(session.has_told(SOL), "the lease did tell the daemon");
    assert_eq!(told(&session), Some(Vec::new()), "but okx's client read nothing of it");
}

/// **hyperliquid's daemon client reads every coin of the address, so the lease claims what it told**
/// (its reports span the account: a restored order on an admitted coin is judged by the report, not
/// held for ever beside a gone order). The coin keeps its case.
#[test]
fn hyperliquids_account_wide_client_claims_the_coins_it_was_told() {
    let base = default_script();
    let mut config = TradePeerConfig::new().with_script(move |frame| match frame {
        TradeUp::SymbolFacts { rid, .. } => {
            vec![Step::Send(TradeDown::SymbolFactsReply { rid: *rid, facts: Ok(facts()) })]
        }
        other => base(other),
    });
    config.features.push(vike_gatewayhub_client::proto::trade_venue_feature("hyperliquid"));
    config.route_key = "hyperliquid".to_string();
    let peer = TradePeer::start(config);
    let (events, _lane) = fake_trade_peer::lane();
    let account = vike_gatewayhub_client::trade::TradeAccount {
        venue: "hyperliquid".to_string(),
        route_key: "hyperliquid".to_string(),
        ..fake_trade_peer::account("tradehub-told-hl")
    };
    let session = vike_gatewayhub_client::trade::TradeLink::new(peer.link(), account, events)
        .with_timings(fake_trade_peer::fast_trade_timings())
        .open()
        .expect("the fake peer opens the lease");
    assert_eq!(told(&session), Some(Vec::new()), "nothing told yet: a claim of nothing");
    assert!(session.admit_symbol("kPEPE").is_ok());
    assert_eq!(told(&session), Some(vec!["kPEPE".to_string()]), "case kept, and claimed");
}

/// **A LOOK is never claimed as read.** A Preview, the Telegram look and an order the door then
/// refuses all read a symbol's facts without admitting it (`symbol_facts`, a probe: the daemon tells
/// its reconcile client nothing), so the lease must not list the symbol: the core would otherwise
/// read the venue's silence about an order on it as "gone" and retire it. Nor does a restart owe it.
#[test]
fn a_previewed_symbol_is_not_told_and_a_restart_does_not_owe_it() {
    let peer = TradePeer::start(grants_perps(Arc::new(AtomicBool::new(false))));
    let (session, _lane) = open(&peer, "tradehub-told-preview");
    assert!(session.symbol_facts("ETHUSDT.P").is_ok(), "the look is answered");
    assert_eq!(told(&session), Some(Vec::new()));
    assert!(!session.has_told("ETHUSDT.P"));

    assert!(session.admit_symbol(SOL).is_ok());
    peer.restart();
    assert!(TradePeer::wait_until(WAIT, || told(&session) == Some(Vec::new())));
    assert!(session.retell_symbols().is_empty());
    assert_eq!(told(&session), Some(vec![SOL.to_string()]), "only the committed symbol is retold");
    let asked_after_restart: Vec<String> = peer
        .ups()
        .iter()
        .filter_map(|f| match f {
            TradeUp::SymbolFacts { symbol, probe: false, .. } => Some(symbol.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(asked_after_restart, [SOL, SOL], "ETHUSDT.P was only ever a look");
}
/// The daemon's reconcile client keeps at most `MAX_EXTRA_SYMBOLS`, ignoring the rest; a claim of a
/// seventeenth would be a symbol it does not read.
#[test]
fn the_list_is_bounded_as_the_daemons_own_is() {
    let peer = TradePeer::start(grants_perps(Arc::new(AtomicBool::new(false))));
    let (session, _lane) = open(&peer, "tradehub-told-cap");
    for i in 0..=MAX_EXTRA_SYMBOLS {
        assert!(session.admit_symbol(&format!("S{i}USDT.P")).is_ok());
    }
    let held = told(&session).expect("a list");
    assert_eq!(held.len(), MAX_EXTRA_SYMBOLS);
    assert!(!held.contains(&format!("S{MAX_EXTRA_SYMBOLS}USDT.P")), "the 17th is not claimed");
}

/// A gatewayhub restart is a new daemon: its reconcile client was told nothing, so the lease claims
/// nothing, remembers what it had told the old one, and `retell_symbols` tells the new one.
#[test]
fn a_gatewayhub_restart_forgets_the_claim_and_retell_restores_it() {
    let peer = TradePeer::start(grants_perps(Arc::new(AtomicBool::new(false))));
    let (session, _lane) = open(&peer, "tradehub-told-restart");
    assert!(session.admit_symbol(SOL).is_ok());
    assert!(session.admit_symbol("ETHUSDT.P").is_ok());
    assert_eq!(facts_asked(&peer), 2);

    peer.restart();
    assert!(
        TradePeer::wait_until(WAIT, || told(&session) == Some(Vec::new())),
        "the new daemon has been told nothing: {:?}",
        told(&session)
    );
    assert!(!session.has_told(SOL));

    assert_eq!(session.retell_symbols(), Vec::<(String, String)>::new(), "nothing refused");
    assert_eq!(told(&session), Some(vec![SOL.to_string(), "ETHUSDT.P".to_string()]));
    assert_eq!(facts_asked(&peer), 4, "one request per symbol, to the NEW daemon");
    // Nothing is owed any more: a second retell sends nothing.
    assert!(session.retell_symbols().is_empty());
    assert_eq!(facts_asked(&peer), 4);
}

/// A symbol the new daemon refuses stays owed and is NAMED with the daemon's reason; the claim does
/// not include it; a later retell, once the daemon answers, tells it.
#[test]
fn a_symbol_the_new_daemon_refuses_is_named_stays_unclaimed_and_is_retold_later() {
    let refuse = Arc::new(AtomicBool::new(false));
    let peer = TradePeer::start(grants_perps(Arc::clone(&refuse)));
    let (session, _lane) = open(&peer, "tradehub-told-refused");
    assert!(session.admit_symbol(SOL).is_ok());

    peer.restart();
    assert!(TradePeer::wait_until(WAIT, || told(&session) == Some(Vec::new())));
    refuse.store(true, Ordering::SeqCst);
    let failed = session.retell_symbols();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].0, SOL);
    assert!(failed[0].1.contains("no instrument"), "the daemon's own reason: {failed:?}");
    assert_eq!(told(&session), Some(Vec::new()), "not claimed");

    refuse.store(false, Ordering::SeqCst);
    assert!(session.retell_symbols().is_empty());
    assert_eq!(told(&session), Some(vec![SOL.to_string()]));
}

/// Two restarts in a row, the second before anything was retold, still owe the symbols of the first.
#[test]
fn what_is_owed_survives_a_second_restart() {
    let peer = TradePeer::start(grants_perps(Arc::new(AtomicBool::new(false))));
    let (session, _lane) = open(&peer, "tradehub-told-twice");
    assert!(session.admit_symbol(SOL).is_ok());

    peer.restart();
    assert!(TradePeer::wait_until(WAIT, || told(&session) == Some(Vec::new())));
    peer.restart();
    std::thread::sleep(Duration::from_millis(300));

    assert!(session.retell_symbols().is_empty());
    assert_eq!(told(&session), Some(vec![SOL.to_string()]));
}

/// **A silent daemon costs the caller's bound, not `request_reply`** (the tradehub's boot re-admission):
/// the look and the commit both come back `Unavailable` once the bound passes, and the daemon is never
/// recorded as told.
#[test]
fn a_facts_request_gives_up_at_the_callers_bound() {
    let base = default_script();
    let peer = TradePeer::start(TradePeerConfig::new().with_script(move |frame| match frame {
        TradeUp::SymbolFacts { .. } => Vec::new(),
        other => base(other),
    }));
    let (session, _lane) = open(&peer, "tradehub-told-silent");
    let bound = Duration::from_millis(150);
    let started = std::time::Instant::now();
    let look = session.symbol_facts_within(SOL, bound).expect_err("nobody answers");
    let commit = session.admit_symbol_within(SOL, bound).expect_err("nobody answers");
    assert!(matches!(look, vike_gatewayhub_client::trade::SymbolFactsError::Unavailable(_)));
    assert!(matches!(commit, vike_gatewayhub_client::trade::SymbolFactsError::Unavailable(_)));
    assert!(started.elapsed() < Duration::from_secs(3), "the bound, not request_reply");
    assert!(!session.has_told(SOL), "an unanswered commit tells nothing");
}
