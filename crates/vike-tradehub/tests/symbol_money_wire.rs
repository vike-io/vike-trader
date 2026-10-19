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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// The ledger currency the daemon says a quoted rate is expressed in (`WireSymbolFacts::account_ccy`), as
    /// ibkr's does; absent: it states none, as oanda's does.
    ledger: Mutex<Option<&'static str>>,
    /// How long the daemon takes over each facts answer, ms (a slow venue).
    delay_ms: AtomicU64,
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
                        account_ccy: daemon.ledger.lock().unwrap().map(str::to_string),
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
            let slow = Duration::from_millis(daemon.delay_ms.load(Ordering::SeqCst));
            vec![Step::Sleep(slow), Step::Send(TradeDown::SymbolFactsReply { rid: *rid, facts })]
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

/// **cTrader is opened by the same rule as OANDA** (a practice account with an account-wide ceiling armed and its
/// currency anchored), in the same orientation (USDJPY on a USD account: a yen is a FRACTION of a dollar), with
/// the SAME refusals in words (no ceiling, no anchor); and a venue the rule does not name (bybit) is refused the
/// same symbol whatever the daemon answers.
#[test]
fn ctrader_is_opened_by_the_same_rule_and_a_venue_with_no_rate_source_is_not() {
    let ctrader_lease = |daemon: &Arc<Daemon>, route: &str, capped: bool, anchored: bool| {
        let peer = gateway(daemon);
        let (session, lane) = open(&peer, "symbol-money-wire-ctrader");
        session.convert_quotes(true);
        let facts = LeaseFacts::default();
        facts.bind_exposure_cap(move |_| capped);
        facts.register(route, session);
        facts.anchor_currency(route, anchored.then_some("USD"));
        (facts, peer, lane)
    };
    let daemon = Arc::new(Daemon::default());
    daemon.quote(USDJPY, "JPY", Some(1.0 / 150.0), 0);

    let (facts, _peer, _lane) = ctrader_lease(&daemon, "ctrader", true, true);
    let jpy = facts.admission("ctrader", "ctrader", USDJPY).expect("a priced yen pair");
    assert_eq!((jpy.quote_ccy.as_deref(), jpy.quote_rate), (Some("JPY"), Some(1.0 / 150.0)));
    assert!(jpy.quote_rate < Some(1.0), "a yen is a fraction of a dollar, never 150");

    let (facts, _peer, _lane) = ctrader_lease(&daemon, "ctrader", false, true);
    let why = facts.admission("ctrader", "ctrader", USDJPY).expect_err("no ceiling");
    assert!(why.contains("quoted in JPY") && why.contains("exposure ceiling"), "{why}");

    let (facts, _peer, _lane) = ctrader_lease(&daemon, "ctrader", true, false);
    let why = facts.admission("ctrader", "ctrader", USDJPY).expect_err("no anchor");
    assert!(why.contains("own currency is not known") && why.contains("NOT admitted"), "{why}");

    let (facts, _peer, _lane) = ctrader_lease(&daemon, "bybit", true, true);
    let why = facts.admission("bybit", "bybit", USDJPY).expect_err("no rate source");
    assert!(
        why.contains("practice accounts of oanda, ctrader, ibkr, dukascopy, ig, fxcm only")
            && why.contains("bybit"),
        "{why}"
    );
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

/// **The timer reads the symbol that waited longest first**, not the alphabetically first: when the venue's own
/// budget runs out part-way down the list (IG answers 12 lookups a minute per account), the symbols that go
/// without are the ones read last time, so the budget rotates. An alphabetical order made the last name lose
/// every tick, its rate went stale and opening orders were denied.
#[test]
fn the_rate_timer_reads_the_longest_waiting_symbol_first() {
    let daemon = Arc::new(Daemon::default());
    for symbol in ["CCCGBP", "AAAGBP", "BBBGBP"] {
        daemon.quote(symbol, "GBP", Some(1.35), 0);
    }
    let (facts, _peer, _lane) = converting_lease(&daemon, true);
    // read (and admitted) in this order, which is neither alphabetical nor reverse
    for symbol in ["CCCGBP", "AAAGBP", "BBBGBP"] {
        facts.admission(OANDA, OANDA, symbol).expect("priced");
        facts.admitted(OANDA, OANDA, symbol);
        std::thread::sleep(Duration::from_millis(5));
    }
    let names = |facts: &LeaseFacts| -> Vec<String> {
        facts.rated_symbols().into_iter().map(|(_, symbol)| symbol).collect()
    };
    assert_eq!(names(&facts), ["CCCGBP", "AAAGBP", "BBBGBP"], "oldest read first");

    // a tick that only reaches the first symbol (the venue's budget ran out after it) reads it again...
    let mut sent = Vec::new();
    let only_first = |_: &str, symbol: &str| symbol == "CCCGBP";
    assert_eq!(facts.refresh_held_rates(&only_first, &mut |c| sent.push(c)), 1);
    // ...and it goes to the back, so the next tick starts with the symbols that went without
    assert_eq!(names(&facts), ["AAAGBP", "BBBGBP", "CCCGBP"]);
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

// ---- ibkr: the rate's ledger currency, the restored position the boot missed, one thread per venue --------

const IBKR: &str = "ibkr";
const SAP: &str = "SAP.SMART.EUR";

/// `route`'s lease registered in `facts` against `daemon`'s peer, converting, anchored to USD. The peer and the
/// lane must stay alive with the test.
fn lease_as(
    facts: &LeaseFacts,
    route: &str,
    daemon: &Arc<Daemon>,
    client: &str,
) -> (TradePeer, fake_trade_peer::Lane) {
    let peer = gateway(daemon);
    let (session, lane) = open(&peer, client);
    session.convert_quotes(true);
    facts.register(route, session);
    facts.anchor_currency(route, Some("USD"));
    (peer, lane)
}

/// **A rate that says it is expressed in another currency than the one the engine was anchored to is NOT used**,
/// whatever its number: the anchor may have come from the journal when the start could not read the account's
/// currency, and an account whose currency changed meanwhile would otherwise be handed rates in a unit its
/// engine does not count in. A rate that states the anchor's own currency is admitted; **an ibkr rate that states
/// NONE is refused too** (finding 3 of the second IBKR currency review: ibkr's gatewayhub always stamps its rate,
/// so a bare one is an answer nobody checked against the anchor), while oanda's, which carries none by design
/// (its `Daemon` here states no ledger), is admitted as before: every oanda test above prices a cross with none.
#[test]
fn a_rate_expressed_in_another_ledger_currency_than_the_anchor_is_refused() {
    for (ledger, refusal) in [
        (Some("EUR"), Some("expressed in EUR")),
        (Some("USD"), None),
        (None, Some("does not say which currency it is expressed in")),
    ] {
        let daemon = Arc::new(Daemon::default());
        daemon.quote(SAP, "EUR", Some(1.10), 0);
        *daemon.ledger.lock().unwrap() = ledger;
        let facts = LeaseFacts::default();
        facts.bind_exposure_cap(|_| true);
        let (_peer, _lane) = lease_as(&facts, IBKR, &daemon, "ledger-currency");
        let got = facts.admission(IBKR, IBKR, SAP);
        match refusal {
            None => {
                let a = got.unwrap_or_else(|why| panic!("{ledger:?}: {why}"));
                assert_eq!((a.quote_ccy.as_deref(), a.quote_rate), (Some("EUR"), Some(1.10)));
            }
            Some(words) => {
                let why = got.expect_err("not the anchor's currency");
                assert!(
                    why.contains(words) && why.contains("anchored to USD"),
                    "{ledger:?}: {why}"
                );
                assert!(why.contains("NOT admitted"), "{why}");
            }
        }
    }
}

/// **The timer re-admits a restored foreign-quoted position the boot missed, and from then on rates it the
/// ordinary way.** The journal said the engine held `SAP.SMART.EUR`; the boot's pass and its retries failed (the
/// data lane refused the dial), so the registry holds no answer for it and the ordinary pass never lists it. A
/// held one is healed at the next tick (lookup, `AdmitSymbol`, the daemon told), a one nothing is held in is
/// left alone.
#[test]
fn the_timer_re_admits_a_restored_position_the_boot_missed() {
    let daemon = Arc::new(Daemon::default());
    daemon.quote(SAP, "EUR", Some(1.10), 0);
    *daemon.ledger.lock().unwrap() = Some("USD");
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(|_| true);
    let (_peer, _lane) = lease_as(&facts, IBKR, &daemon, "missed-restore");
    facts.note_journaled_foreign(IBKR, &[(SAP.to_string(), "EUR".to_string())]);
    assert!(facts.rated_symbols().is_empty(), "the boot rated nothing");

    let mut sent = Vec::new();
    let nothing_held = |_: &str, _: &str| false;
    assert_eq!(facts.refresh_held_rates(&nothing_held, &mut |c| sent.push(c)), 0);
    assert!(daemon.asked().is_empty(), "a symbol nothing is held in is not asked about");

    let held = |route: &str, symbol: &str| route == IBKR && symbol == SAP;
    assert_eq!(facts.refresh_held_rates(&held, &mut |c| sent.push(c)), 1);
    match &sent[..] {
        [Command::AdmitSymbol(a)] => {
            assert_eq!((a.route_key.as_str(), a.symbol.as_str()), (IBKR, SAP));
            assert_eq!((a.quote_ccy.as_deref(), a.quote_rate), (Some("EUR"), Some(1.10)));
        }
        other => panic!("one fresh admission, got {other:?}"),
    }
    assert_eq!(facts.rated_symbols(), [(IBKR.to_string(), SAP.to_string())], "rated from now on");
}

const ASML: &str = "ASML.SMART.EUR";

/// **The timer tries ONE owed symbol per venue per tick and backs each failing one off** (finding 5 of the second
/// IBKR currency review): two restored positions the boot missed whose rate cannot be had (the daemon's rate is
/// stale on arrival, a failure of the moment that is asked again each try) cost ONE ask per tick, not one each; a
/// symbol that just failed is not tried again before its wait has run, and the wait doubles; and once the rate
/// can be had both heal, one per tick, and cost nothing more.
#[test]
fn the_timer_tries_one_owed_symbol_per_tick_and_backs_each_failure_off() {
    let daemon = Arc::new(Daemon::default());
    for name in [SAP, ASML] {
        daemon.quote(name, "EUR", Some(1.10), 200_000);
    }
    *daemon.ledger.lock().unwrap() = Some("USD");
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(|_| true);
    facts.set_owed_retry_first(Duration::from_millis(500));
    let (_peer, _lane) = lease_as(&facts, IBKR, &daemon, "owed-backoff");
    facts.note_journaled_foreign(
        IBKR,
        &[(SAP.to_string(), "EUR".to_string()), (ASML.to_string(), "EUR".to_string())],
    );
    let held = |_: &str, _: &str| true;
    let mut sent = Vec::new();
    let mut tick = |facts: &LeaseFacts| facts.refresh_held_rates(&held, &mut |c| sent.push(c));
    let asked = |daemon: &Daemon| daemon.asked().len();

    tick(&facts);
    assert_eq!(asked(&daemon), 1, "one owed symbol on the first tick, not both");
    tick(&facts);
    assert_eq!(asked(&daemon), 2, "the other one on the next");
    tick(&facts);
    assert_eq!(asked(&daemon), 2, "both are in their backoff: nothing is asked");

    std::thread::sleep(Duration::from_millis(550));
    tick(&facts);
    tick(&facts);
    assert_eq!(asked(&daemon), 4, "after the first wait each is tried again, one per tick");
    tick(&facts);
    std::thread::sleep(Duration::from_millis(550));
    tick(&facts);
    assert_eq!(asked(&daemon), 4, "the second wait is twice the first: 550 ms is not enough");

    // The rate can be had now: once their waits have run the symbols are re-admitted, one per tick, and are rated
    // from then on (the ordinary pass keeps them, so no owed state is left for them).
    for name in [SAP, ASML] {
        daemon.quote(name, "EUR", Some(1.10), 0);
    }
    std::thread::sleep(Duration::from_millis(600));
    tick(&facts);
    assert_eq!(facts.rated_symbols().len(), 1, "one symbol healed on the first tick");
    tick(&facts);
    assert_eq!(facts.rated_symbols().len(), 2, "the other on the next");
}

/// **One venue's slow rate read does not delay another's**: the tick reads each venue's symbols on a thread of
/// its own, so two venues that each take 600 ms to answer cost the tick about 600 ms, not 1200. (Serial, an IBKR
/// gateway that takes seconds per quiet pair would push oanda's re-read past the engine's 120 s window.)
#[test]
fn a_slow_venue_does_not_delay_another_venues_rate_read() {
    let ibkr = Arc::new(Daemon::default());
    ibkr.quote(SAP, "EUR", Some(1.10), 0);
    *ibkr.ledger.lock().unwrap() = Some("USD");
    let oanda = Arc::new(Daemon::default());
    oanda.quote(EURGBP, "GBP", Some(1.35), 0);
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(|_| true);
    let (_peer_a, _lane_a) = lease_as(&facts, IBKR, &ibkr, "slow-ibkr");
    let (_peer_b, _lane_b) = lease_as(&facts, OANDA, &oanda, "slow-oanda");
    facts.admission(IBKR, IBKR, SAP).expect("priced");
    facts.admitted(IBKR, IBKR, SAP);
    facts.admission(OANDA, OANDA, EURGBP).expect("priced");
    facts.admitted(OANDA, OANDA, EURGBP);
    assert_eq!(facts.rated_symbols().len(), 2);

    for daemon in [&ibkr, &oanda] {
        daemon.delay_ms.store(600, Ordering::SeqCst);
    }
    let mut sent = Vec::new();
    let began = std::time::Instant::now();
    let count = facts.refresh_held_rates(&|_, _| true, &mut |c| sent.push(c));
    let took = began.elapsed();
    assert_eq!(count, 2, "both venues were re-read");
    assert!(took < Duration::from_millis(1_100), "one thread per venue: {took:?} for 2 x 600 ms");
    let mut symbols: Vec<&str> = sent
        .iter()
        .map(|c| match c {
            Command::AdmitSymbol(a) => a.symbol.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    // Sent as each venue's thread finishes: two that finish together arrive in either order.
    symbols.sort_unstable();
    assert_eq!(symbols, [EURGBP, SAP]);
}

/// A tick over a held ibkr symbol (rated) and a held oanda cross, ibkr answering after `ibkr_ms` and oanda at once.
fn two_venues_held(ibkr_ms: u64) -> (Arc<LeaseFacts>, Vec<(TradePeer, fake_trade_peer::Lane)>) {
    let ibkr = Arc::new(Daemon::default());
    ibkr.quote(SAP, "EUR", Some(1.10), 0);
    *ibkr.ledger.lock().unwrap() = Some("USD");
    let oanda = Arc::new(Daemon::default());
    oanda.quote(EURGBP, "GBP", Some(1.35), 0);
    let facts = LeaseFacts::default();
    facts.bind_exposure_cap(|_| true);
    let peers = vec![
        lease_as(&facts, IBKR, &ibkr, "held-ibkr"),
        lease_as(&facts, OANDA, &oanda, "held-oanda"),
    ];
    facts.admission(IBKR, IBKR, SAP).expect("priced");
    facts.admitted(IBKR, IBKR, SAP);
    facts.admission(OANDA, OANDA, EURGBP).expect("priced");
    facts.admitted(OANDA, OANDA, EURGBP);
    ibkr.delay_ms.store(ibkr_ms, Ordering::SeqCst);
    (Arc::new(facts), peers)
}

/// **A venue's rates reach the engine the moment its thread finishes, not when the slowest venue's has**
/// (finding 2 of the second IBKR currency review): with ibkr taking 800 ms, oanda's fresh admission is handed to
/// `send` at once, 600 ms or more BEFORE ibkr's. Joined before sending, oanda's rate waited for ibkr's read and
/// an oanda row could cross the engine's 120 s limit behind a silent IBKR link.
#[test]
fn a_venues_rates_are_sent_the_moment_its_thread_finishes() {
    let (facts, _peers) = two_venues_held(800);
    let mut sent: Vec<(std::time::Instant, String)> = Vec::new();
    let count = facts.refresh_held_rates(&|_, _| true, &mut |c| match c {
        Command::AdmitSymbol(a) => sent.push((std::time::Instant::now(), a.symbol.clone())),
        other => panic!("{other:?}"),
    });
    assert_eq!(count, 2);
    let at = |symbol: &str| sent.iter().find(|(_, s)| s == symbol).expect("sent").0;
    assert!(
        at(SAP) - at(EURGBP) >= Duration::from_millis(600),
        "oanda was sent {:?} before ibkr (ibkr takes 800 ms)",
        at(SAP) - at(EURGBP)
    );
}

/// **The ticks keep a fixed rate**: a tick that takes 250 ms with the timer set to 500 ms delivers every 500 ms,
/// not every 750 ms (the wait is the remainder of the period, not a whole period after the tick).
#[test]
fn the_rate_timer_keeps_a_fixed_rate_whatever_a_tick_costs() {
    let daemon = Arc::new(Daemon::default());
    daemon.quote(EURGBP, "GBP", Some(1.35), 0);
    daemon.delay_ms.store(250, Ordering::SeqCst);
    let (facts, _peer, _lane) = converting_lease(&daemon, true);
    facts.admission(OANDA, OANDA, EURGBP).expect("priced");
    facts.admitted(OANDA, OANDA, EURGBP);
    let facts = Arc::new(facts);
    let (tx, rx) = std::sync::mpsc::channel();
    facts.spawn_rate_refresher(
        Duration::from_millis(500),
        |_, symbol| symbol == EURGBP,
        move |_| {
            let _ = tx.send(std::time::Instant::now());
        },
    );
    let arrivals: Vec<std::time::Instant> = (0..4)
        .map(|_| rx.recv_timeout(Duration::from_secs(20)).expect("a tick delivered"))
        .collect();
    let mean = (arrivals[3] - arrivals[0]) / 3;
    assert!(
        mean < Duration::from_millis(625),
        "a tick every {mean:?}: the period stretched by the tick's own cost (750 ms) instead of holding 500 ms"
    );
}

/// **The timer does not run inside the boot window**: a tick there would take the boot bound for its asks and could
/// mark a route key `missed` for the boot's remaining symbols. It ticks as soon as the window closes.
#[test]
fn the_rate_timer_waits_out_the_boot_window() {
    let daemon = Arc::new(Daemon::default());
    daemon.quote(EURGBP, "GBP", Some(1.35), 0);
    let (facts, _peer, _lane) = converting_lease(&daemon, true);
    facts.admission(OANDA, OANDA, EURGBP).expect("priced");
    facts.admitted(OANDA, OANDA, EURGBP);
    facts.set_boot_wait(Duration::from_secs(5));
    let facts = Arc::new(facts);
    let window = facts.begin_boot();
    let (tx, rx) = std::sync::mpsc::channel();
    facts.spawn_rate_refresher(
        Duration::from_millis(30),
        |_, symbol| symbol == EURGBP,
        move |command| {
            let _ = tx.send(command);
        },
    );
    assert!(
        rx.recv_timeout(Duration::from_millis(700)).is_err(),
        "no tick ran while the boot window was open"
    );
    drop(window);
    let first =
        rx.recv_timeout(Duration::from_secs(20)).expect("a tick ran once the window closed");
    assert!(matches!(first, Command::AdmitSymbol(a) if a.symbol == EURGBP));
}
