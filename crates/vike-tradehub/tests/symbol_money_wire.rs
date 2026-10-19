//! **The per-symbol money facts over the REAL lease** (`LeaseFacts` over a `TradeSession`, against a scripted
//! gatewayhub that speaks the real protocol): what the node keeps of the daemon's answers and when it keeps it.
//!
//! * **The symbol's own leverage** (decision 0182's per-symbol twin). Only a COMMIT (`TradeUp::SymbolFacts`
//!   with `probe: false`, which `LeaseFacts::admitted` and `tell_daemon` send) can bring a figure, because
//!   only a commit may ask the exchange anything. The figure is kept per `(route key, symbol)` (set on a
//!   figure, removed when a commit confirmed none, left alone by a commit that failed), the facts answer
//!   states it as `im_requirement = 1 / figure` so a refresh restates it, and a stopped engine forgets it.
//!
//! * **A symbol quoted in another currency than the account's** (decision 0183, phase 1). A node that says it
//!   converts (`TradeSession::convert_quotes`) is answered the quote currency and the rate; one that never did
//!   is refused by the daemon's fence. The node takes the answer only with a usable, fresh rate and only for an
//!   account the rule lets convert (`vike_tradehub_core::quote_conversion_tier_refusal`), reuses it for
//!   [`RATE_FRESH_FOR`] and no longer, and the timer re-reads a held position's rate and hands the fresh
//!   admission to the core's lane.
//!
//! The venue is named `binance` because it is the daemon the fake peer plays; the facts are asked as `oanda`,
//! the one venue the conversion exists for, because the rule names it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_exec::Command;
use vike_gatewayhub_client::proto::{FACTS_BUSY, TradeDown, TradeUp, WireSymbolFacts};
use vike_model::SymbolProperties;
use vike_tradehub::remote_backend::facts::LeaseFacts;
use vike_tradehub::server::symbol_admission::SymbolFacts;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../vike-gatewayhub-client/tests/support/fake_trade_peer.rs"]
mod fake_trade_peer;

use fake_trade_peer::{Step, TradePeer, TradePeerConfig, default_script, open};

const ROUTE: &str = "binance";
const VENUE: &str = "binance";
const SOL: &str = "SOLUSDT.P";

/// A symbol the scripted daemon quotes in another currency than the account's.
#[derive(Clone)]
struct Foreign {
    ccy: &'static str,
    /// What the daemon states one unit of `ccy` is worth in the account's currency (`None`: it states none).
    rate: Option<f64>,
    /// How old the rate is when the daemon answers, ms (negative: from the node's future).
    age_ms: i64,
}

/// What the scripted daemon does with a facts request.
#[derive(Default)]
struct Daemon {
    /// The leverage a COMMIT of each symbol confirms (absent: nothing).
    confirms: Mutex<HashMap<String, f64>>,
    /// Every facts request, in order: `(symbol, probe)`.
    asked: Mutex<Vec<(String, bool)>>,
    /// Whether each facts request said the node converts a quote currency (`TradeUp::SymbolFacts::quoted`).
    quoted_asks: Mutex<Vec<bool>>,
    /// The symbols quoted in another currency than the account's. Answered with their rate only to a node
    /// that asked `quoted`; to any other, with the fence's refusal.
    foreign: Mutex<HashMap<String, Foreign>>,
    /// Answer every commit "busy" (a transient failure, `Unavailable` at the node).
    busy: AtomicBool,
}

impl Daemon {
    fn asked(&self) -> Vec<(String, bool)> {
        self.asked.lock().unwrap().clone()
    }

    fn quote(&self, symbol: &str, ccy: &'static str, rate: Option<f64>, age_ms: i64) {
        self.foreign.lock().unwrap().insert(symbol.to_string(), Foreign { ccy, rate, age_ms });
    }

    fn confirm(&self, symbol: &str, figure: f64) {
        self.confirms.lock().unwrap().insert(symbol.to_string(), figure);
    }

    fn confirm_nothing(&self, symbol: &str) {
        self.confirms.lock().unwrap().remove(symbol);
    }
}

fn facts_of(leverage: Option<f64>) -> WireSymbolFacts {
    WireSymbolFacts {
        properties: SymbolProperties {
            tick_size: 0.01,
            step_size: 0.001,
            ..SymbolProperties::default()
        },
        leverage,
        ..WireSymbolFacts::default()
    }
}

/// A fake gatewayhub whose symbol facts are the daemon's: a look carries no figure, a commit the one the
/// daemon confirms for the symbol.
fn gateway(daemon: &Arc<Daemon>) -> TradePeer {
    let base = default_script();
    let daemon = Arc::clone(daemon);
    TradePeer::start(TradePeerConfig::new().with_script(move |frame| match frame {
        TradeUp::SymbolFacts { rid, symbol, probe, quoted } => {
            daemon.asked.lock().unwrap().push((symbol.clone(), *probe));
            daemon.quoted_asks.lock().unwrap().push(*quoted);
            let foreign = daemon.foreign.lock().unwrap().get(symbol).cloned();
            let facts = if !*probe && daemon.busy.load(Ordering::SeqCst) {
                Err(FACTS_BUSY.to_string())
            } else if let Some(foreign) = foreign {
                if *quoted {
                    Ok(WireSymbolFacts {
                        quote_ccy: Some(foreign.ccy.to_string()),
                        quote_rate: foreign.rate,
                        rate_as_of_ms: Some(vike_model::now_ms() - foreign.age_ms),
                        ..facts_of(None)
                    })
                } else {
                    Err(format!(
                        "oanda: {symbol} is quoted in {} but this account's currency is USD",
                        foreign.ccy
                    ))
                }
            } else {
                let leverage = if *probe {
                    None
                } else {
                    daemon.confirms.lock().unwrap().get(symbol).copied()
                };
                Ok(facts_of(leverage))
            };
            vec![Step::Send(TradeDown::SymbolFactsReply { rid: *rid, facts })]
        }
        other => base(other),
    }))
}

/// A registry with the fake daemon's lease registered under [`ROUTE`]. The lane and the peer stay alive
/// with it.
fn lease(daemon: &Arc<Daemon>) -> (LeaseFacts, TradePeer, fake_trade_peer::Lane) {
    let (facts, peer, lane, _session) = lease_keeping(daemon);
    (facts, peer, lane)
}

/// [`lease`], also handing back the session the registry holds (to see what the daemon was told, and to
/// restart the daemon under it).
fn lease_keeping(
    daemon: &Arc<Daemon>,
) -> (LeaseFacts, TradePeer, fake_trade_peer::Lane, vike_gatewayhub_client::trade::TradeSession) {
    let peer = gateway(daemon);
    let (session, lane) = open(&peer, "symbol-money-wire");
    let facts = LeaseFacts::default();
    facts.register(ROUTE, session.clone());
    (facts, peer, lane, session)
}

/// Restart the daemon behind `session` and wait until the lease knows the NEW one was told nothing of `symbol`.
fn replace_the_daemon(
    peer: &TradePeer,
    session: &vike_gatewayhub_client::trade::TradeSession,
    symbol: &str,
) {
    peer.restart();
    assert!(
        TradePeer::wait_until(Duration::from_secs(5), || !session.has_told(symbol)),
        "the new daemon has been told nothing"
    );
}

/// **A look never carries a figure and a commit keeps the one the daemon confirmed**: the Preview's lookup
/// asks the daemon with `probe: true` (which the daemon answers without touching the exchange), so the facts
/// answer states no margin rate; the commit (`admitted`) brings the figure, and from then on every answer
/// states `1 / 10`, a cached one included.
#[test]
fn a_look_confirms_nothing_and_a_commit_keeps_the_figure() {
    let daemon = Arc::new(Daemon::default());
    daemon.confirm(SOL, 10.0);
    let (facts, _peer, _lane) = lease(&daemon);

    let looked = facts.admission(ROUTE, VENUE, SOL).expect("the look is answered");
    assert_eq!(looked.im_requirement, None, "a look asks the exchange nothing");
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), None);
    assert_eq!(daemon.asked(), [(SOL.to_string(), true)], "one probe, no commit yet");

    facts.admitted(ROUTE, VENUE, SOL);
    assert_eq!(daemon.asked(), [(SOL.to_string(), true), (SOL.to_string(), false)]);
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), Some(10.0));

    let again = facts.admission(ROUTE, VENUE, SOL).expect("answered from the kept facts");
    assert_eq!(again.im_requirement, Some(1.0 / 10.0), "the answer restates the symbol's rate");
    assert_eq!(daemon.asked().len(), 2, "the kept answer cost no round trip");
}

/// **A commit that confirmed nothing forgets the figure**, so a refresh stops stating a rate the venue is no
/// longer seen holding (the engine's row can only be replaced, never cleared: a repeat `None` leaves the old
/// per-symbol row, which `vike-exec`'s `quote_margin` tests pin). Such a commit only runs for a symbol the
/// daemon has NOT been told (a settled one is not committed again), which is a NEW daemon: a gatewayhub restart.
#[test]
fn a_commit_that_confirms_nothing_removes_the_figure() {
    let daemon = Arc::new(Daemon::default());
    daemon.confirm(SOL, 10.0);
    let (facts, peer, _lane, session) = lease_keeping(&daemon);
    facts.admitted(ROUTE, VENUE, SOL);
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), Some(10.0));

    replace_the_daemon(&peer, &session, SOL);
    daemon.confirm_nothing(SOL);
    assert!(facts.tell_daemon(ROUTE, VENUE, SOL).is_ok(), "the commit itself took");
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), None);
    let after = facts.admission(ROUTE, VENUE, SOL).expect("answered");
    assert_eq!(after.im_requirement, None);
}

/// **A symbol the daemon holds a confirmed leverage for is not committed again** (the review's F2): the commit
/// takes seconds and could move the leverage under an open position, and the figure is fixed for the engine's
/// life, so a second admission (the facts refresh every ten minutes of an actively traded symbol) and the
/// retry's `tell_daemon` ask the daemon NOTHING. A symbol that confirmed nothing is committed again (it may
/// confirm now), except by `tell_daemon`, whose question (was the daemon told?) is already answered.
#[test]
fn a_settled_symbol_is_not_committed_again_and_an_unconfirmed_one_is() {
    let daemon = Arc::new(Daemon::default());
    daemon.confirm(SOL, 10.0);
    let (facts, _peer, _lane) = lease(&daemon);
    facts.admitted(ROUTE, VENUE, SOL);
    assert_eq!(daemon.asked(), [(SOL.to_string(), false)]);
    facts.admitted(ROUTE, VENUE, SOL);
    assert!(facts.tell_daemon(ROUTE, VENUE, SOL).is_ok());
    assert_eq!(daemon.asked().len(), 1, "settled: no second commit, no third");
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), Some(10.0));

    let eth = "ETHUSDT.P";
    facts.admitted(ROUTE, VENUE, eth);
    facts.admitted(ROUTE, VENUE, eth);
    let commits = daemon.asked().iter().filter(|(s, probe)| s == eth && !probe).count();
    assert_eq!(commits, 2, "nothing confirmed: the next admission asks again");
    assert!(facts.tell_daemon(ROUTE, VENUE, eth).is_ok(), "the daemon was told");
    let commits = daemon.asked().iter().filter(|(s, probe)| s == eth && !probe).count();
    assert_eq!(commits, 2, "and tell_daemon does not repeat a commit that took");
}

/// **A commit that times out, or fails, is "unconfirmed"** (the review's F4): the symbol stays at the engine's
/// own rate (no figure kept, so no second `AdmitSymbol`), the failure is `tell_daemon`'s `Err` for the boot's
/// retry, and the next commit that runs confirms it.
#[test]
fn a_first_commit_that_fails_leaves_the_symbol_unconfirmed_until_the_next_one() {
    let daemon = Arc::new(Daemon::default());
    daemon.confirm(SOL, 10.0);
    let (facts, _peer, _lane) = lease(&daemon);
    daemon.busy.store(true, Ordering::SeqCst);
    facts.admitted(ROUTE, VENUE, SOL);
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), None);
    assert!(facts.tell_daemon(ROUTE, VENUE, SOL).is_err());
    assert_eq!(facts.admission(ROUTE, VENUE, SOL).expect("answered").im_requirement, None);

    daemon.busy.store(false, Ordering::SeqCst);
    assert!(facts.tell_daemon(ROUTE, VENUE, SOL).is_ok());
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), Some(10.0), "the next commit confirms it");
}

/// **A commit that ran off the sender's thread is stated at the next refresh** (the Telegram poller's
/// `FailFast`): the order it rode went out at the engine's own rate, and the symbol's next `refresh` answers at
/// once, from the facts already kept, with the margin rate the commit confirmed; the one after is not due.
#[test]
fn a_detached_commits_figure_is_stated_by_the_next_refresh() {
    let daemon = Arc::new(Daemon::default());
    daemon.confirm(SOL, 10.0);
    let (facts, _peer, _lane) = lease(&daemon);
    facts.admission(ROUTE, VENUE, SOL).expect("the look");
    assert_eq!(facts.refresh(ROUTE, VENUE, SOL), None, "not admitted yet");

    facts.admitted_detached(ROUTE, VENUE, SOL);
    let stated = facts.refresh(ROUTE, VENUE, SOL).expect("the figure is stated at once");
    assert_eq!(stated.im_requirement, Some(1.0 / 10.0));
    assert_eq!(facts.refresh(ROUTE, VENUE, SOL), None, "stated once, and the facts are not due");

    // A commit that confirmed nothing leaves nothing to state.
    daemon.confirm_nothing("ETHUSDT.P");
    facts.admission(ROUTE, VENUE, "ETHUSDT.P").expect("the look");
    facts.admitted_detached(ROUTE, VENUE, "ETHUSDT.P");
    assert_eq!(facts.refresh(ROUTE, VENUE, "ETHUSDT.P"), None);
}

/// **A commit that FAILED says nothing**: the figure the last commit left stands (a busy daemon is not a
/// verdict about the venue), and the failure is `tell_daemon`'s `Err`, which the boot's retry counts. (The
/// commit that fails is one to a NEW daemon: a settled symbol is not committed again.)
#[test]
fn a_failed_commit_leaves_the_figure_as_it_was() {
    let daemon = Arc::new(Daemon::default());
    daemon.confirm(SOL, 10.0);
    let (facts, peer, _lane, session) = lease_keeping(&daemon);
    facts.admitted(ROUTE, VENUE, SOL);

    replace_the_daemon(&peer, &session, SOL);
    daemon.busy.store(true, Ordering::SeqCst);
    daemon.confirm_nothing(SOL);
    assert!(facts.tell_daemon(ROUTE, VENUE, SOL).is_err(), "the daemon was busy");
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), Some(10.0), "nothing was learned");
}

/// **A figure that is no leverage is no figure** (a daemon bug must not make the engine refuse the whole
/// admission with `bad-im-requirement`): below 1x, zero, negative and not-a-number are dropped at the door
/// of the map, and the facts answer states no rate.
#[test]
fn a_figure_that_is_no_leverage_is_not_kept() {
    for bad in [0.5, 0.0, -2.0, f64::NAN, f64::INFINITY] {
        let daemon = Arc::new(Daemon::default());
        daemon.confirm(SOL, bad);
        let (facts, _peer, _lane) = lease(&daemon);
        facts.admitted(ROUTE, VENUE, SOL);
        assert_eq!(facts.confirmed_leverage(ROUTE, SOL), None, "{bad}");
        assert_eq!(facts.admission(ROUTE, VENUE, SOL).expect("answered").im_requirement, None);
    }
}

/// **Each account keeps its own figure, and a stopped engine takes it away**: the next engine under the
/// route key starts with no per-symbol margin rate, so the node must not state one it never confirmed.
#[test]
fn a_figure_belongs_to_one_accounts_engine_and_dies_with_it() {
    let daemon = Arc::new(Daemon::default());
    daemon.confirm(SOL, 10.0);
    let (facts, _peer, _lane) = lease(&daemon);
    facts.admitted(ROUTE, VENUE, SOL);
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), Some(10.0));
    assert_eq!(facts.confirmed_leverage("binance#ALT", SOL), None, "another account's engine");

    facts.engine_stopped(ROUTE);
    assert_eq!(facts.confirmed_leverage(ROUTE, SOL), None);
}

// ---- a symbol quoted in another currency than the account's ----------------------------------------------

const OANDA: &str = "oanda";
const EURGBP: &str = "EURGBP";
const USDJPY: &str = "USDJPY";

/// A registry with the lease registered under the route key `oanda` (a route key's first segment is the venue
/// the rule is judged for), whose session SAYS it converts a quote currency (as `RemoteBackend::start_adapter`
/// says it for OANDA's practice account) and whose account-wide ceiling is `capped`.
fn converting_lease(
    daemon: &Arc<Daemon>,
    capped: bool,
) -> (LeaseFacts, TradePeer, fake_trade_peer::Lane) {
    let peer = gateway(daemon);
    let (session, lane) = open(&peer, "symbol-money-wire-quoted");
    session.convert_quotes(true);
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(move |_| capped);
    facts.register(OANDA, session);
    facts.anchor_currency(OANDA, Some("USD"));
    (facts, peer, lane)
}

/// **A cross is admitted with its rate, in the orientation the daemon states it** (account units per ONE unit
/// of the quote currency): a pound is worth MORE than a dollar and a yen a fraction of one, and the node
/// forwards the rate as stated, stamps nothing of its own, and reuses it without a round trip inside its
/// freshness. A symbol the daemon does not quote in another currency carries no currency and no rate.
#[test]
fn a_cross_is_admitted_with_its_rate_as_stated_to_an_account_that_converts() {
    let daemon = Arc::new(Daemon::default());
    daemon.quote(EURGBP, "GBP", Some(1.35), 1_000);
    daemon.quote(USDJPY, "JPY", Some(1.0 / 150.0), 0);
    let (facts, _peer, _lane) = converting_lease(&daemon, true);

    let gbp = facts.admission(OANDA, OANDA, EURGBP).expect("a priced cross");
    assert_eq!((gbp.quote_ccy.as_deref(), gbp.quote_rate), (Some("GBP"), Some(1.35)));
    let age = vike_model::now_ms() - gbp.rate_as_of_ms.expect("stamped by the daemon");
    assert!((0..30_000).contains(&age), "a rate asked a moment ago: {age} ms");
    let jpy = facts.admission(OANDA, OANDA, USDJPY).expect("a priced cross");
    assert_eq!(jpy.quote_rate.map(f64::to_bits), Some((1.0_f64 / 150.0).to_bits()));
    assert!(jpy.quote_rate < Some(1.0), "a yen is a fraction of a dollar, never 150");

    let asks = daemon.asked().len();
    assert_eq!(
        facts.admission(OANDA, OANDA, EURGBP).expect("kept"),
        gbp,
        "reused inside the window"
    );
    assert_eq!(daemon.asked().len(), asks, "no round trip for a rate that is still fresh");
    assert!(
        daemon.quoted_asks.lock().unwrap().iter().all(|asked| *asked),
        "every request said this node converts"
    );

    let plain = facts.admission(OANDA, OANDA, "GBPUSD").expect("quoted in the account's currency");
    assert_eq!((plain.quote_ccy, plain.quote_rate, plain.rate_as_of_ms), (None, None, None));
}

/// **A node that never said it converts is refused the cross by the daemon's own fence**: nothing changes for
/// an account the rule does not open (every other venue, a live account), because the request never carries
/// `quoted`.
#[test]
fn a_node_that_never_said_it_converts_is_refused_the_cross_by_the_daemons_fence() {
    let daemon = Arc::new(Daemon::default());
    daemon.quote(EURGBP, "GBP", Some(1.35), 0);
    let (facts, _peer, _lane) = lease(&daemon);
    let why = facts.admission(ROUTE, "oanda", EURGBP).expect_err("the fence");
    assert!(why.contains("EURGBP is quoted in GBP"), "{why}");
    assert_eq!(*daemon.quoted_asks.lock().unwrap(), [false]);
}

/// **No rate, no admission, in words**: a daemon that states no rate, a rate that is not a number, a rate past
/// the engine's window and a rate from the node's future each leave the symbol NOT admitted. The first two are
/// answers about the symbol, so they are remembered (a flood of orders is not a flood of lookups); the last two
/// are about the moment, so the next ask goes back to the daemon.
#[test]
fn a_missing_stale_or_future_rate_is_refused_and_only_the_moment_is_asked_again() {
    for (rate, age_ms, words, remembered) in [
        (None, 0, "gave none", true),
        (Some(0.0), 0, "not a positive number", true),
        (Some(1.35), 200_000, "past the 120 s", false),
        (Some(1.35), -60_000, "clocks disagree", false),
    ] {
        let daemon = Arc::new(Daemon::default());
        daemon.quote(EURGBP, "GBP", rate, age_ms);
        let (facts, _peer, _lane) = converting_lease(&daemon, true);
        let why = facts.admission(OANDA, OANDA, EURGBP).expect_err(words);
        assert!(why.contains(words) && why.contains("NOT admitted"), "{words}: {why}");
        let before = daemon.asked().len();
        facts.admission(OANDA, OANDA, EURGBP).expect_err("still refused");
        assert_eq!(daemon.asked().len() == before, remembered, "{words}");
    }
}

/// **An account with no ceiling armed is refused the cross, in words, and still trades the pairs quoted in its
/// own currency** (the same answers from the daemon; the rule is the node's).
#[test]
fn an_account_with_no_ceiling_is_refused_the_cross_and_keeps_its_own_currency_pairs() {
    let daemon = Arc::new(Daemon::default());
    daemon.quote(EURGBP, "GBP", Some(1.35), 0);
    let (facts, _peer, _lane) = converting_lease(&daemon, false);
    let why = facts.admission(OANDA, OANDA, EURGBP).expect_err("no ceiling");
    assert!(why.contains("quoted in GBP") && why.contains("exposure ceiling"), "{why}");
    facts.admission(OANDA, OANDA, "GBPUSD").expect("quoted in the account's own currency");
}

/// **A cross is refused while the account's currency is not anchored**: the venue's account summary was not read
/// at the start (or the daemon predates the field), so the rate cannot be tied to a currency the ledger is in;
/// a pair quoted in the account's own currency is unaffected. A start that anchors it opens the cross, and a
/// stop takes the anchor away again.
#[test]
fn a_cross_is_refused_until_the_accounts_currency_is_anchored() {
    let daemon = Arc::new(Daemon::default());
    daemon.quote(EURGBP, "GBP", Some(1.35), 0);
    let (facts, _peer, _lane) = converting_lease(&daemon, true);
    facts.anchor_currency(OANDA, None);
    let why = facts.admission(OANDA, OANDA, EURGBP).expect_err("no currency");
    assert!(why.contains("own currency is not known") && why.contains("NOT admitted"), "{why}");
    facts.admission(OANDA, OANDA, "GBPUSD").expect("its own currency needs no anchor");

    // A start that anchors the currency (the venue's word, trimmed and upper-cased) opens the cross...
    let (facts, _peer, _lane) = converting_lease(&daemon, true);
    facts.anchor_currency(OANDA, Some(" usd "));
    assert!(facts.admission(OANDA, OANDA, EURGBP).is_ok(), "anchored: the cross is priced");
    // ...and a stop takes the anchor, and the rate kept under it, away.
    facts.engine_stopped(OANDA);
    let second = gateway(&daemon);
    let (session, _second_lane) = open(&second, "symbol-money-wire-restart");
    session.convert_quotes(true);
    facts.engine_starting(OANDA, true);
    facts.register(OANDA, session);
    let why = facts.admission(OANDA, OANDA, EURGBP).expect_err("the new start set no anchor");
    assert!(why.contains("own currency is not known"), "a stop took the anchor away: {why}");
}

/// **The rate timer** (one tick): for an ADMITTED symbol the registry holds a rate for, a held position's rate
/// is read again and the fresh admission goes to the core's lane; a symbol nothing is held in is left to the
/// order path; a re-read that fails sends nothing (the engine keeps the rate it has); and a stopped engine
/// takes its symbols out of the timer.
#[test]
fn the_rate_timer_re_reads_a_held_position_and_leaves_the_rest_alone() {
    let daemon = Arc::new(Daemon::default());
    daemon.quote(EURGBP, "GBP", Some(1.35), 0);
    let (facts, _peer, _lane) = converting_lease(&daemon, true);
    assert!(facts.rated_symbols().is_empty(), "nothing admitted yet");
    facts.admission(OANDA, OANDA, EURGBP).expect("priced");
    assert!(facts.rated_symbols().is_empty(), "a look admits nothing");
    facts.admitted(OANDA, OANDA, EURGBP);
    assert_eq!(facts.rated_symbols(), [(OANDA.to_string(), EURGBP.to_string())]);

    daemon.quote(EURGBP, "GBP", Some(1.36), 0);
    let mut sent = Vec::new();
    assert_eq!(facts.refresh_held_rates(&|_, _| false, &mut |c| sent.push(c)), 0, "not held");
    assert!(sent.is_empty());
    let held = |route: &str, symbol: &str| route == OANDA && symbol == EURGBP;
    assert_eq!(facts.refresh_held_rates(&held, &mut |c| sent.push(c)), 1);
    match &sent[..] {
        [Command::AdmitSymbol(a)] => {
            assert_eq!((a.route_key.as_str(), a.symbol.as_str()), (OANDA, EURGBP));
            assert_eq!((a.quote_ccy.as_deref(), a.quote_rate), (Some("GBP"), Some(1.36)));
        }
        other => panic!("one fresh admission, got {other:?}"),
    }

    daemon.quote(EURGBP, "GBP", None, 0);
    assert_eq!(facts.refresh_held_rates(&held, &mut |c| sent.push(c)), 0, "no rate, nothing sent");
    assert_eq!(sent.len(), 1);

    facts.engine_stopped(OANDA);
    assert!(facts.rated_symbols().is_empty(), "a stopped engine's symbols leave the timer");
}

/// **The rate thread** ticks while the symbol is held and ends with the registry (it holds a `Weak`).
#[test]
fn the_rate_thread_sends_while_held_and_ends_with_the_registry() {
    let daemon = Arc::new(Daemon::default());
    daemon.quote(EURGBP, "GBP", Some(1.35), 0);
    let (facts, _peer, _lane) = converting_lease(&daemon, true);
    facts.admission(OANDA, OANDA, EURGBP).expect("priced");
    facts.admitted(OANDA, OANDA, EURGBP);
    let facts = Arc::new(facts);
    let (tx, rx) = std::sync::mpsc::channel();
    facts.spawn_rate_refresher(
        Duration::from_millis(30),
        |_, symbol| symbol == EURGBP,
        move |command| {
            let _ = tx.send(command);
        },
    );
    let first = rx.recv_timeout(Duration::from_secs(10)).expect("a tick re-read the rate");
    assert!(matches!(first, Command::AdmitSymbol(a) if a.symbol == EURGBP));
    drop(facts);
    loop {
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(_) => {}
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => panic!("the thread outlived the registry"),
        }
    }
}
