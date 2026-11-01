//! The node's balance gate: which bindings hold a venue's balance back, that the decorator holds exactly that and
//! forwards every other verb of the reconcile client as it is, that a hold is SHOWN to the operator surfaces in
//! short words, and that one that goes on is said again every ten minutes (a stepped clock), never between and
//! never when nothing is held.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_exec::recon::ReconClient;
use vike_log::capture::{CapturedEvent, captured};
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

use super::*;

/// **Only an assumed, a declined or an unanchored binding holds a balance back, and only for the venues whose
/// balance carries no currency** (oanda, ig, fxcm): an anchored engine, and every venue that fences itself
/// (ctrader, dukascopy, ibkr) or is not converting (bybit), is handed its balance whatever the binding says.
#[test]
fn only_a_binding_that_disagrees_with_the_cash_holds_it_back_and_only_for_no_currency_venues() {
    let why = "the engine is mounted on `EURUSD`, quoted in USD, and the account's money is EUR";
    let bindings = [
        Binding::Anchored,
        Binding::Assumed,
        Binding::Declined { ccy: "EUR", stated: true, why },
        Binding::Unanchored { ccy: "EUR" },
    ];
    for venue in ["oanda", "ig", "fxcm"] {
        assert_eq!(held_reason(venue, &bindings[0]), None, "{venue}: anchored, handed on");
        let assumed = held_reason(venue, &bindings[1]).expect("assumed: held");
        assert!(assumed.starts_with(venue) && assumed.contains("only ASSUMED"), "{assumed}");
        let declined = held_reason(venue, &bindings[2]).expect("declined: held");
        assert!(declined.contains(why) && declined.contains("not adopted"), "{declined}");
        let unanchored = held_reason(venue, &bindings[3]).expect("unanchored: held");
        assert!(
            unanchored.contains("EUR") && unanchored.contains("does not convert"),
            "{unanchored}"
        );
    }
    for venue in ["ctrader", "dukascopy", "ibkr", "bybit", "binance"] {
        for binding in &bindings {
            assert_eq!(held_reason(venue, binding), None, "{venue} {binding:?}");
            assert_eq!(hold(venue, binding), None, "{venue} {binding:?}");
        }
    }
}

/// **The operator is shown the reason in short words** (`balance held: <reason>`): an assumed USD, a stated
/// currency the ledger could not be anchored to (declined by the mount fence, or unanchored on an account that does
/// not convert), and a declined currency that was itself only assumed; an anchored binding shows nothing.
#[test]
fn a_hold_carries_the_short_words_the_operator_surfaces_show() {
    let why = "the engine is mounted on `EURUSD`";
    let words = |binding: Binding<'_>| hold("oanda", &binding).map(|hold| hold.reason);
    assert_eq!(words(Binding::Anchored), None);
    assert_eq!(words(Binding::Assumed).as_deref(), Some("assumed USD"));
    assert_eq!(
        words(Binding::Declined { ccy: "EUR", stated: true, why }).as_deref(),
        Some("stated EUR not anchored")
    );
    assert_eq!(
        words(Binding::Declined { ccy: "JPY", stated: false, why }).as_deref(),
        Some("assumed JPY not anchored")
    );
    assert_eq!(
        words(Binding::Unanchored { ccy: "GBP" }).as_deref(),
        Some("stated GBP not anchored")
    );
    // the log keeps its long sentence beside the short words
    let held = hold("oanda", &Binding::Assumed).expect("held");
    assert!(held.why.contains("only ASSUMED") && held.reason == "assumed USD", "{held:?}");
}

/// A reconcile client that records which verbs reached it and answers a balance of 25,000.
#[derive(Default)]
struct Recording {
    seen: Arc<Mutex<Vec<&'static str>>>,
}

impl Recording {
    fn note(&self, verb: &'static str) {
        self.seen.lock().unwrap().push(verb);
    }
}

impl ReconClient for Recording {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.note("orders");
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        self.note("fills");
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.note("positions");
        Ok(Vec::new())
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        self.note("balance");
        Ok(Some(25_000.0))
    }
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        self.note("fees");
        Ok(None)
    }
    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        self.note("identity");
        Ok(Some("ACC-1".to_string()))
    }
    fn admit_symbol(&self, _symbol: &str) {
        self.note("admit");
    }
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        self.note("admitted");
        Some(vec!["GBPUSD".to_string()])
    }
}

/// What the gate said to the lease registry, in order: `Some(reason)` = held, `None` = withdrawn.
type Board = Arc<Mutex<Vec<Option<String>>>>;

/// Which verbs reached the inner client ([`Recording::seen`]).
type Seen = Arc<Mutex<Vec<&'static str>>>;

/// The assumed-USD hold of an oanda engine.
fn assumed() -> Hold {
    hold("oanda", &Binding::Assumed).expect("an assumed binding holds")
}

/// A spec for oanda's account `ALT` whose `shown` records into the returned board.
fn spec(hold: Option<Hold>, untold: Option<Untold>) -> (GateSpec, Board) {
    let board: Board = Arc::default();
    let log = Arc::clone(&board);
    let shown: Shown = Box::new(move |reason: Option<&str>| {
        log.lock().unwrap().push(reason.map(str::to_string));
    });
    (
        GateSpec {
            venue: "oanda".to_string(),
            account: "ALT".to_string(),
            hold,
            untold,
            lift: None,
            shown,
        },
        board,
    )
}

/// The raw gate of an ASSUMED binding with its lift flag (the flag `LeaseFacts` raises when the venue names the
/// currency the engine assumed), built at the returned instant.
fn liftable() -> (BalanceGate, Seen, Board, Arc<AtomicBool>, Instant) {
    let inner = Recording::default();
    let seen = Arc::clone(&inner.seen);
    let (mut spec, board) = spec(Some(assumed()), None);
    let lift = Arc::new(AtomicBool::new(false));
    spec.lift = Some(Arc::clone(&lift));
    let now = Instant::now();
    (BalanceGate::new(Box::new(inner), spec, now), seen, board, lift, now)
}

/// The production entry: the gate boxed (or the inner client itself when nothing is held).
fn gated(hold: Option<Hold>, untold: Option<Untold>) -> (Box<dyn ReconClient>, Seen, Board) {
    let inner = Recording::default();
    let seen = Arc::clone(&inner.seen);
    let (spec, board) = spec(hold, untold);
    (gate(Box::new(inner), spec), seen, board)
}

/// The gate itself, built at `now`, so a test can step the clock ([`BalanceGate::fetch_balance_at`]).
fn raw(hold: Option<Hold>, untold: Option<Untold>, now: Instant) -> (BalanceGate, Seen, Board) {
    let inner = Recording::default();
    let seen = Arc::clone(&inner.seen);
    let (spec, board) = spec(hold, untold);
    (BalanceGate::new(Box::new(inner), spec, now), seen, board)
}

/// The `warn` events among `events`.
fn warns(events: &[CapturedEvent]) -> Vec<&CapturedEvent> {
    events.iter().filter(|event| event.level == tracing::Level::WARN).collect()
}

/// **A held balance is `Ok(None)` and the venue is never asked; every other verb crosses unchanged**, the
/// default `fetch_mass_status` included (it composes the three forwarded fetches, in the driver's order).
#[test]
fn a_held_balance_is_not_reported_and_not_asked_and_every_other_verb_is_forwarded() {
    let (client, seen, _board) = gated(Some(assumed()), None);
    assert_eq!(client.fetch_balance(), Ok(None));
    assert!(seen.lock().unwrap().is_empty(), "the venue was not asked: {:?}", seen.lock().unwrap());

    assert_eq!(client.fetch_account_identity(), Ok(Some("ACC-1".to_string())));
    assert_eq!(client.fetch_fee_rates(), Ok(None));
    client.admit_symbol("GBPUSD");
    assert_eq!(client.admitted_symbols(), Some(vec!["GBPUSD".to_string()]));
    assert!(client.fetch_order_status_reports(0).unwrap().is_empty());
    assert!(client.fetch_fill_reports(0).unwrap().is_empty());
    assert!(client.fetch_position_status_reports().unwrap().is_empty());
    client.fetch_mass_status(0).expect("the default composition");
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "identity",
            "fees",
            "admit",
            "admitted",
            "orders",
            "fills",
            "positions", // each verb, once
            "orders",
            "fills",
            "positions", // fetch_mass_status
        ]
    );
}

/// With nothing held and nothing owed the balance is the inner client's, and so is everything else; the gate is
/// not even built, so nothing is shown.
#[test]
fn a_gate_with_nothing_held_hands_the_balance_on_and_shows_nothing() {
    let (client, seen, board) = gated(None, None);
    assert_eq!(client.fetch_balance(), Ok(Some(25_000.0)));
    assert_eq!(*seen.lock().unwrap(), ["balance"]);
    assert!(board.lock().unwrap().is_empty(), "nothing held: nothing shown");
}

/// **A held gate shows its reason to the operator surfaces the moment it is built**, once, and the passes that
/// follow add nothing to the board (a hold that goes on is not re-announced there).
#[test]
fn a_held_gate_shows_its_reason_at_once_and_once() {
    let (client, _seen, board) = gated(Some(assumed()), None);
    assert_eq!(*board.lock().unwrap(), [Some("assumed USD".to_string())]);
    for _ in 0..3 {
        assert_eq!(client.fetch_balance(), Ok(None));
    }
    assert_eq!(board.lock().unwrap().len(), 1, "passes do not re-announce the hold");

    let declined =
        hold("oanda", &Binding::Declined { ccy: "EUR", stated: true, why: "mounted on EURUSD" })
            .expect("held");
    let (_client, _seen, board) = gated(Some(declined), None);
    assert_eq!(*board.lock().unwrap(), [Some("stated EUR not anchored".to_string())]);
}

/// **The hold is said at `warn` when it begins and again every ten minutes while it lasts, never between**: the
/// first sight is the construction's own line, a pass inside the interval says nothing, the pass at ten minutes
/// says it once with the account, the venue, the reason and the operator step, and a long silence between passes
/// is one line, not one per missed interval.
#[test]
fn a_hold_that_goes_on_is_said_again_every_ten_minutes_and_not_between() {
    let t0 = Instant::now();
    let at = |minutes: u64| t0 + Duration::from_secs(60 * minutes);
    let ((gate, _seen, _board), made) = captured(|| raw(Some(assumed()), None, t0));
    let first = warns(&made);
    assert_eq!(first.len(), 1, "the hold is said when it begins: {made:?}");
    assert!(first[0].message.contains("RECONCILE BALANCE HELD BACK"), "{:?}", first[0]);
    assert_eq!(first[0].field("account"), Some("ALT"));
    assert_eq!(first[0].field("venue"), Some("oanda"));

    let pass = |minutes: u64| captured(|| gate.fetch_balance_at(at(minutes)));
    for quiet in [0, 1, 5, 9] {
        let (balance, events) = pass(quiet);
        assert_eq!(balance, Ok(None));
        assert!(warns(&events).is_empty(), "minute {quiet} is inside the interval: {events:?}");
    }
    let (_, events) = pass(10);
    let reminder = warns(&events);
    assert_eq!(reminder.len(), 1, "ten minutes on: said again: {events:?}");
    let line = &reminder[0].message;
    assert!(
        line.contains("oanda account ALT: balance held: assumed USD")
            && line.contains("`account.active` off and then on"),
        "{line}"
    );
    assert_eq!(reminder[0].field("account"), Some("ALT"));
    for quiet in [11, 19] {
        assert!(warns(&pass(quiet).1).is_empty(), "minute {quiet}");
    }
    assert_eq!(warns(&pass(20).1).len(), 1, "and ten minutes after that");
    assert_eq!(warns(&pass(65).1).len(), 1, "a long gap between passes is ONE line");
    assert!(warns(&pass(66).1).is_empty(), "the interval counts from the last line");
}

/// **No hold, no line**: a gate that holds nothing (it is only watching ibkr's owed symbols, and none is owed) says
/// nothing at `warn` however long it lives, shows nothing, and hands the balance on.
#[test]
fn nothing_is_said_or_shown_while_nothing_is_held() {
    let t0 = Instant::now();
    let (gate, seen, board) = raw(None, Some(Box::new(Vec::<String>::new)), t0);
    for minutes in [0, 10, 20, 60, 600] {
        let (balance, events) =
            captured(|| gate.fetch_balance_at(t0 + Duration::from_secs(60 * minutes)));
        assert_eq!(balance, Ok(Some(25_000.0)));
        assert!(warns(&events).is_empty(), "minute {minutes}: {events:?}");
    }
    assert!(board.lock().unwrap().is_empty());
    assert_eq!(seen.lock().unwrap().len(), 5, "the venue was asked every time");
}

/// **ibkr's boot window**: the balance is not asked while the daemon's client is owed a journaled foreign symbol,
/// and is asked once every one was told.
#[test]
fn a_balance_waits_for_the_journaled_foreign_symbols_to_be_told() {
    let told = Arc::new(AtomicBool::new(false));
    let untold: Untold = {
        let told = Arc::clone(&told);
        Box::new(move || {
            if told.load(Ordering::SeqCst) { Vec::new() } else { vec!["SAP.SMART.EUR".to_string()] }
        })
    };
    let (client, seen, _board) = gated(None, Some(untold));
    assert_eq!(client.fetch_balance(), Ok(None), "owed a symbol");
    assert!(seen.lock().unwrap().is_empty());
    told.store(true, Ordering::SeqCst);
    assert_eq!(client.fetch_balance(), Ok(Some(25_000.0)), "told");
    // a held binding stays held whatever was told
    let (client, _, _) = gated(Some(assumed()), Some(Box::new(Vec::<String>::new)));
    assert_eq!(client.fetch_balance(), Ok(None));
}

/// **The dynamic hold is shown while it lasts and withdrawn when it ends**: the first pass that finds a symbol
/// owed shows `untold foreign stock` and says it at `warn` at once, the cadence of the reminders is the same ten
/// minutes, telling the daemon withdraws the row (and the balance flows), and a hold that begins again is said at
/// once afresh.
#[test]
fn the_untold_stock_hold_is_shown_said_again_and_withdrawn_when_it_ends() {
    let t0 = Instant::now();
    let at = |minutes: u64| t0 + Duration::from_secs(60 * minutes);
    let told = Arc::new(AtomicBool::new(false));
    let untold: Untold = {
        let told = Arc::clone(&told);
        Box::new(move || {
            if told.load(Ordering::SeqCst) { Vec::new() } else { vec!["SAP.SMART.EUR".to_string()] }
        })
    };
    let (gate, seen, board) = raw(None, Some(untold), t0);
    assert!(board.lock().unwrap().is_empty(), "nothing is shown before a pass finds a symbol owed");

    let pass = |minutes: u64| captured(|| gate.fetch_balance_at(at(minutes)));
    let (balance, events) = pass(1);
    assert_eq!(balance, Ok(None));
    let first = warns(&events);
    assert_eq!(first.len(), 1, "said at once when it begins: {events:?}");
    assert!(
        first[0].message.contains("balance held: untold foreign stock"),
        "{:?}",
        first[0].message
    );
    assert_eq!(*board.lock().unwrap(), [Some("untold foreign stock".to_string())]);

    assert!(warns(&pass(5).1).is_empty(), "inside the interval");
    assert_eq!(board.lock().unwrap().len(), 1, "shown once, not once per pass");
    assert_eq!(warns(&pass(11).1).len(), 1, "ten minutes after the first line");

    told.store(true, Ordering::SeqCst);
    let (balance, events) = pass(12);
    assert_eq!(balance, Ok(Some(25_000.0)), "told: the balance flows");
    assert!(warns(&events).is_empty(), "{events:?}");
    assert_eq!(*board.lock().unwrap(), [Some("untold foreign stock".to_string()), None]);
    assert_eq!(*seen.lock().unwrap(), ["balance"]);

    told.store(false, Ordering::SeqCst);
    assert_eq!(warns(&pass(13).1).len(), 1, "a hold that begins again is said at once");
    assert_eq!(board.lock().unwrap().len(), 3, "and shown again");
}

/// **An ASSUMED hold ends when the lift flag is raised, and not before** (A52 (b)): the flag is the venue naming the
/// very currency the engine assumed (`LeaseFacts::confirm_assumed_anchor`). Until then the gate holds and reminds
/// as any hold does; the pass after the flag hands the venue's cash on, withdraws the row from the operator
/// surfaces and says so once at `info`, and the passes after that say nothing more.
#[test]
fn an_assumed_hold_ends_when_the_venue_names_the_assumed_currency_and_not_before() {
    let (gate, seen, board, lift, t0) = liftable();
    let at = |secs: u64| t0 + Duration::from_secs(secs);
    assert_eq!(gate.fetch_balance_at(at(1)), Ok(None), "held until the flag");
    let (_, events) = captured(|| gate.fetch_balance_at(t0 + REMIND_EVERY));
    assert_eq!(warns(&events).len(), 1, "a liftable hold is reminded like any other: {events:?}");
    assert!(seen.lock().unwrap().is_empty(), "the venue was not asked while held");
    assert_eq!(*board.lock().unwrap(), [Some("assumed USD".to_string())]);

    lift.store(true, Ordering::SeqCst);
    let (balance, events) = captured(|| gate.fetch_balance_at(at(3)));
    assert_eq!(balance, Ok(Some(25_000.0)), "the venue's cash is adopted");
    assert_eq!(*board.lock().unwrap(), [Some("assumed USD".to_string()), None], "withdrawn");
    assert!(warns(&events).is_empty(), "{events:?}");
    assert!(
        events
            .iter()
            .any(|e| e.level == tracing::Level::INFO && e.message.contains("balance hold ended")),
        "said once: {events:?}"
    );

    let (balance, events) = captured(|| gate.fetch_balance_at(at(4)));
    assert_eq!(balance, Ok(Some(25_000.0)));
    assert!(events.is_empty(), "nothing more to say: {events:?}");
    assert_eq!(board.lock().unwrap().len(), 2, "withdrawn once");
    assert_eq!(*seen.lock().unwrap(), ["balance", "balance"]);
}

/// **A hold that has no lift flag never lifts**, whatever else is true: a declined or unanchored binding is the
/// cash being in ANOTHER currency than the label, which no statement of the assumed one can change.
#[test]
fn a_hold_without_a_lift_flag_never_lifts() {
    let declined =
        hold("oanda", &Binding::Declined { ccy: "EUR", stated: true, why: "mounted on EURUSD" })
            .expect("held");
    let (client, seen, _board) = gated(Some(declined), None);
    for _ in 0..3 {
        assert_eq!(client.fetch_balance(), Ok(None));
    }
    assert!(seen.lock().unwrap().is_empty());
}

/// The reminder names everything the operator needs: the venue, the account, the reason and the step.
#[test]
fn the_reminder_names_the_venue_the_account_the_reason_and_the_operator_step() {
    let line = held_reminder("ig", "DEFAULT", "stated GBP not anchored");
    assert!(
        line.starts_with("ig account DEFAULT: balance held: stated GBP not anchored."),
        "{line}"
    );
    assert!(line.contains("`account.active` off and then on"), "{line}");
    assert!(line.contains("every 10 minutes"), "{line}");
}
