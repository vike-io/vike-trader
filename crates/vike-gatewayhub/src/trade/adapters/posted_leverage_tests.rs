//! The lane's half of an account's leverage on a venue that POSTS one (decision 0182), over scripted
//! READERS and a fake clock: no venue network, and no write method exists on the reader to script.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`only_a_start_that_asks_and_whose_row_agrees_reaches_a_venue`] | a venue read for a start that asked nothing, a `live` row, a withheld start, a cleared row or a figure above the row |
//! | [`the_profile_carries_the_figure_and_keeps_everything_else`] | the figure replacing more than the leverage the bridge's one write reads |
//! | [`the_venue_is_seen_holding_the_figure_or_the_answer_is_an_error`] | a figure applied that the venue never held |
//! | [`a_closed_row_makes_no_reader_call_at_all`] | the demo-only fence leaking a read to a live row, aster or deribit |
//! | [`a_leverage_changed_on_the_venue_under_a_running_engine_is_seen_at_the_next_ask`] | a stale confirmation replayed to a restarted tradehub |
//! | [`the_bridge_is_handed_the_figure_only_when_the_probe_says_yes`] | an account that was refused still written, or one that asked nothing disturbed |
//! | [`a_figure_an_earlier_run_left_on_the_venue_confirms_nothing_the_start_did_not_write`] | a venue that merely holds the figure from an earlier run read back as confirmed while the start wrote something else |

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vike_bridge_core::leverage_read::HeldLeverage;
use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ExecOutcome, LiveExec, Resolution, VenueAdapter,
    VenueDeclaration,
};
use vike_bridge_core::venue_adapter_fixture::PLANTED_DECLARATION;
use vike_gatewayhub_client::proto::{AdapterRisk, Withhold};

use super::*;
use crate::trade::adapters::bridge_trade::BridgeTrade;
use crate::trade::adapters::registry::VenueRow;
use crate::trade::adapters::testutil::{NoopClient, snapshot_of};
use crate::trade::lane::TradeLane;

// ---- the doubles -----------------------------------------------------------------------------------

/// What a scripted venue answers, and what was asked of it.
#[derive(Clone)]
struct Script {
    account: Result<(), String>,
    max: Option<f64>,
    /// `held` reads in order; the last one repeats.
    reads: Arc<Mutex<VecDeque<HeldLeverage>>>,
    built: Arc<AtomicUsize>,
    probed_reads: Arc<AtomicUsize>,
    /// How many of the next `check_account` calls fail with a transient error (a timeout).
    flaky_checks: Arc<AtomicUsize>,
}

impl Script {
    fn holding(held: f64) -> Self {
        Script {
            account: Ok(()),
            max: Some(100.0),
            reads: Arc::new(Mutex::new(VecDeque::from([HeldLeverage {
                held,
                position_open: false,
            }]))),
            built: Arc::new(AtomicUsize::new(0)),
            probed_reads: Arc::new(AtomicUsize::new(0)),
            flaky_checks: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The next `n` account-mode reads time out.
    fn timing_out_first(self, n: usize) -> Self {
        self.flaky_checks.store(n, Ordering::SeqCst);
        self
    }

    fn then_holding(self, held: f64) -> Self {
        self.reads.lock().expect("script").push_back(HeldLeverage { held, position_open: false });
        self
    }

    fn now_holding(&self, held: f64) {
        *self.reads.lock().expect("script") =
            VecDeque::from([HeldLeverage { held, position_open: false }]);
    }
}

struct ScriptedReader(Script);

impl LeverageReader for ScriptedReader {
    fn check_account(&self) -> Result<(), String> {
        if self.0.flaky_checks.load(Ordering::SeqCst) > 0 {
            self.0.flaky_checks.fetch_sub(1, Ordering::SeqCst);
            return Err("account/info could not be read: timed out".to_string());
        }
        self.0.account.clone()
    }
    fn venue_max(&self) -> Result<Option<f64>, String> {
        Ok(self.0.max)
    }
    fn held(&self) -> Result<HeldLeverage, String> {
        self.0.probed_reads.fetch_add(1, Ordering::SeqCst);
        let mut reads = self.0.reads.lock().expect("script");
        Ok(if reads.len() > 1 { reads.pop_front().expect("one") } else { reads[0] })
    }
}

/// The lane's reads over `script` and a fake clock that records every wait.
fn reads_over(script: &Script) -> (PostedReads, Arc<Mutex<Vec<Duration>>>) {
    let script = script.clone();
    let factory: ReaderFactory = Arc::new(
        move |_venue: &str,
              _creds: &AccountCredentials<'_>,
              _tier: AccountTier,
              _symbol: &str|
              -> Result<Box<dyn LeverageReader>, String> {
            script.built.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(ScriptedReader(script.clone())))
        },
    );
    let waits = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&waits);
    let pause = Arc::new(move |d: Duration| recorded.lock().expect("waits").push(d));
    (PostedReads::scripted(factory, pause), waits)
}

/// A bridge that records the leverage its profile carried and comes up live.
struct Capture {
    seen: Mutex<Vec<Option<f64>>>,
}

impl VenueAdapter for Capture {
    fn venue(&self) -> &'static str {
        "bybit"
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        self.seen.lock().expect("seen").push(req.risk_profile.and_then(|p| p.max_leverage));
        AdapterOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(NoopClient),
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon: None,
            identity: None,
        }
    }
}

static CAPTURE: Capture = Capture { seen: Mutex::new(Vec::new()) };
static CAPTURE_REG: [VenueRow; 1] = [VenueRow::Adapter(&CAPTURE)];
/// A second bridge for the test that starts accounts beside `the_bridge_is_handed_…` (the tests of one
/// binary run on threads, and a shared record would interleave).
static CAPTURE_B: Capture = Capture { seen: Mutex::new(Vec::new()) };
static CAPTURE_B_REG: [VenueRow; 1] = [VenueRow::Adapter(&CAPTURE_B)];

// ---- fixtures --------------------------------------------------------------------------------------

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// An ACTIVE row of `venue` at `tier` stating `leverage`.
fn row(
    id: i64,
    venue: &str,
    tier: AccountTier,
    label: &AccountLabel,
    leverage: Option<f64>,
) -> vike_secrets::Account {
    vike_secrets::Account {
        id,
        venue: venue.to_string(),
        tier,
        label: label.text().map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: leverage.and_then(vike_secrets::AccountLeverage::new),
    }
}

fn accounts_of(rows: &[vike_secrets::Account]) -> AccountDirectory {
    let mut policy = vike_tradehub_core::AdapterPolicy::default();
    for r in rows {
        policy = policy.with_account_row(r.clone());
    }
    policy.accounts.clone()
}

fn start(venue_ask: Option<f64>, risk: Option<f64>) -> AdapterStart {
    AdapterStart {
        rid: 1,
        symbol: "BTCUSDT".to_string(),
        declared_legs: Vec::new(),
        recon_enabled: false,
        takes_trigger: false,
        risk: risk.map(|max_leverage| AdapterRisk { max_leverage: Some(max_leverage) }),
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: venue_ask,
        route_key: "bybit".to_string(),
    }
}

fn target(venue: &str, tier: AccountTier, account: Option<&str>) -> TradeTarget {
    TradeTarget { venue: venue.to_string(), account: account.map(str::to_string), tier }
}

fn lane(rows: &[vike_secrets::Account], reads: PostedReads) -> BridgeTrade {
    let mut policy = vike_tradehub_core::AdapterPolicy::default();
    for r in rows {
        policy = policy.with_account_row(r.clone());
    }
    BridgeTrade::with_snapshot(&CAPTURE_REG, HashMap::new(), snapshot_of(&policy))
        .with_posted_reads(reads)
}

/// [`lane`] after a start that handed the bridge `figure` (the default account's hand-over, recorded
/// directly so the scripted venue's reads are left for the read-back under test).
fn lane_handed(rows: &[vike_secrets::Account], reads: PostedReads, figure: f64) -> BridgeTrade {
    let lane = lane(rows, reads);
    lane.handed_over_for_test(None, Ok(figure));
    lane
}

// ---- the gate --------------------------------------------------------------------------------------

#[test]
fn only_a_start_that_asks_and_whose_row_agrees_reaches_a_venue() {
    let default = AccountLabel::Default;
    let demo_bybit = accounts_of(&[row(1, "bybit", AccountTier::Demo, &default, Some(10.0))]);
    let t = target("bybit", AccountTier::Demo, None);
    let ask = |dir: &AccountDirectory, t: &TradeTarget, label: &AccountLabel, s: &AdapterStart| {
        asked_figure(dir, t, label, s)
    };

    assert_eq!(ask(&demo_bybit, &t, &default, &start(Some(10.0), None)), Some(10.0));
    assert_eq!(
        ask(&demo_bybit, &t, &default, &start(Some(4.0), None)),
        Some(4.0),
        "a ceiling lowered the ask below the stated figure: the lane asks the lower"
    );
    assert_eq!(ask(&demo_bybit, &t, &default, &start(None, Some(2.0))), None, "an older tradehub");
    assert_eq!(
        ask(&demo_bybit, &t, &default, &start(Some(12.0), None)),
        None,
        "the row states 10 and the start asks 12: the lane's reading wins"
    );
    assert_eq!(ask(&demo_bybit, &t, &default, &start(Some(2.5), None)), None, "not whole");
    assert_eq!(ask(&demo_bybit, &t, &default, &start(Some(0.0), None)), None);
    assert_eq!(ask(&demo_bybit, &t, &default, &start(Some(f64::NAN), None)), None);

    // A withheld (data-only) start is not to use the account's credentials, even to read.
    let mut withheld = start(Some(10.0), None);
    withheld.withhold = Some(Withhold::DataOnly);
    assert_eq!(ask(&demo_bybit, &t, &default, &withheld), None);

    // The row cleared since the node read it.
    let cleared = accounts_of(&[row(1, "bybit", AccountTier::Demo, &default, None)]);
    assert_eq!(ask(&cleared, &t, &default, &start(Some(10.0), None)), None);

    // Another account of the venue states nothing.
    let two = accounts_of(&[
        row(1, "bybit", AccountTier::Demo, &default, Some(10.0)),
        row(2, "bybit", AccountTier::Demo, &alt(), None),
    ]);
    assert_eq!(
        ask(
            &two,
            &target("bybit", AccountTier::Demo, Some("ALT")),
            &alt(),
            &start(Some(10.0), None)
        ),
        None
    );

    // The three listed venues are asked at demo; the table decides, not this file.
    for venue in ["okx", "binance"] {
        let dir = accounts_of(&[row(1, venue, AccountTier::Demo, &default, Some(10.0))]);
        let t = target(venue, AccountTier::Demo, None);
        assert_eq!(ask(&dir, &t, &default, &start(Some(10.0), None)), Some(10.0), "{venue}");
    }
}

#[test]
fn a_closed_row_makes_no_reader_call_at_all() {
    let default = AccountLabel::Default;
    let script = Script::holding(10.0);
    let (reads, waits) = reads_over(&script);
    let cases = [
        // (venue, tier): a live row of a demo-only venue, aster (closed), deribit (no number),
        // hyperliquid (a different mechanism, its own path).
        ("bybit", AccountTier::Live),
        ("okx", AccountTier::Live),
        ("binance", AccountTier::Live),
        ("aster", AccountTier::Demo),
        ("deribit", AccountTier::Demo),
        ("hyperliquid", AccountTier::Demo),
    ];
    for (venue, tier) in cases {
        let lane = lane(&[row(1, venue, tier, &default, Some(10.0))], reads.clone());
        let got = lane.leverage_cap(&target(venue, tier, None), &start(Some(10.0), None));
        // hyperliquid answers by its own rule (a public read this test cannot reach: it has no key
        // set, so it is an error); every other row is silence.
        if venue == "hyperliquid" {
            assert!(matches!(got, Some(Err(_))), "{venue}: its own path, not the posting reader");
        } else {
            assert_eq!(got, None, "{venue} at {tier:?}");
        }
    }
    assert_eq!(script.built.load(Ordering::SeqCst), 0, "no reader was ever built");
    assert!(waits.lock().expect("waits").is_empty());
}

// ---- the profile -----------------------------------------------------------------------------------

#[test]
fn the_profile_carries_the_figure_and_keeps_everything_else() {
    let bare = profile_with(None, 10.0);
    assert_eq!(bare.max_leverage, Some(10.0));
    assert_eq!(bare, ProfileRisk { max_leverage: Some(10.0), ..Default::default() });
    let had = ProfileRisk {
        max_leverage: Some(3.0),
        max_notional_per_order: Some(5.0),
        ..Default::default()
    };
    let with = profile_with(Some(had), 10.0);
    assert_eq!(with.max_leverage, Some(10.0), "the account's figure replaces the global number");
    assert_eq!(with.max_notional_per_order, Some(5.0));
}

// ---- the read-back ---------------------------------------------------------------------------------

#[test]
fn the_venue_is_seen_holding_the_figure_or_the_answer_is_an_error() {
    let default = AccountLabel::Default;
    let rows = [row(1, "bybit", AccountTier::Demo, &default, Some(10.0))];
    let t = target("bybit", AccountTier::Demo, None);

    // Nothing asked: no answer, no reader.
    let script = Script::holding(10.0);
    let (reads, _) = reads_over(&script);
    assert_eq!(lane(&rows, reads).leverage_cap(&t, &start(None, None)), None);
    assert_eq!(script.built.load(Ordering::SeqCst), 0);

    // The venue holds the ask at once: Ok, no wait. (Stated equals current: no write is sent by anyone
    // here, so there is nothing to wait for.)
    let script = Script::holding(10.0);
    let (reads, waits) = reads_over(&script);
    assert_eq!(
        lane_handed(&rows, reads, 10.0).leverage_cap(&t, &start(Some(10.0), None)),
        Some(Ok(10.0))
    );
    assert!(waits.lock().expect("waits").is_empty());

    // The bridge's write lands on the third look.
    let script = Script::holding(2.0).then_holding(2.0).then_holding(2.0).then_holding(10.0);
    let (reads, waits) = reads_over(&script);
    assert_eq!(
        lane_handed(&rows, reads, 10.0).leverage_cap(&t, &start(Some(10.0), None)),
        Some(Ok(10.0))
    );
    assert_eq!(waits.lock().expect("waits").len(), 2, "it waited between looks and stopped");

    // The write never lands (a read-only key): an error after the whole bounded schedule, never Ok.
    let script = Script::holding(2.0);
    let (reads, waits) = reads_over(&script);
    let got = lane_handed(&rows, reads, 10.0).leverage_cap(&t, &start(Some(10.0), None));
    let Some(Err(why)) = got else { panic!("never held must be an error: {got:?}") };
    assert!(why.contains("still holds 2x"), "{why}");
    let total: Duration = waits.lock().expect("waits").iter().sum();
    assert_eq!(total, Duration::from_millis(5000));

    // A refused account says why and waits for nothing.
    let mut isolated = Script::holding(10.0);
    isolated.account = Err("the account is in isolated margin".to_string());
    let (reads, waits) = reads_over(&isolated);
    assert_eq!(
        lane_handed(&rows, reads, 10.0).leverage_cap(&t, &start(Some(10.0), None)),
        Some(Err("the account is in isolated margin".to_string()))
    );
    assert!(waits.lock().expect("waits").is_empty());

    // A figure above the symbol's maximum.
    let mut low = Script::holding(2.0);
    low.max = Some(5.0);
    let (reads, _) = reads_over(&low);
    let Some(Err(why)) = lane_handed(&rows, reads, 10.0).leverage_cap(&t, &start(Some(10.0), None))
    else {
        panic!("above the maximum must be refused");
    };
    assert!(why.contains("above the venue's maximum of 5x"), "{why}");
}

#[test]
fn a_leverage_changed_on_the_venue_under_a_running_engine_is_seen_at_the_next_ask() {
    let default = AccountLabel::Default;
    let rows = [row(1, "bybit", AccountTier::Demo, &default, Some(10.0))];
    let t = target("bybit", AccountTier::Demo, None);
    let script = Script::holding(10.0);
    let (reads, _) = reads_over(&script);
    let lane = lane_handed(&rows, reads, 10.0);
    let same = start(Some(10.0), None);
    assert_eq!(lane.leverage_cap(&t, &same), Some(Ok(10.0)), "the first ask");
    // The operator lowers the leverage on the exchange; the tradehub reconnects and the daemon
    // answers the SAME start from the running adapter: the venue is read again, not replayed.
    script.now_holding(5.0);
    let again = lane.leverage_cap(&t, &same);
    assert!(matches!(again, Some(Err(_))), "a stale Ok must not survive: {again:?}");
    // …and raising it above the ask is not trusted either (the engine would be tighter than the venue,
    // but the confirmation is "exactly this figure").
    script.now_holding(20.0);
    assert!(matches!(lane.leverage_cap(&t, &same), Some(Err(_))));
}

/// **The read-back confirms only what THIS start handed the bridge to write.** The venue holds 10x from an
/// earlier run, this start's first read times out (so the probe refuses and the bridge is started with the
/// profile untouched: it will write 3x), and the read-back's own probe then passes and sees the old 10x.
/// Without the hand-over record that is "confirmed" while the venue is about to be written 3x.
#[test]
fn a_figure_an_earlier_run_left_on_the_venue_confirms_nothing_the_start_did_not_write() {
    let default = AccountLabel::Default;
    let rows = [row(1, "bybit", AccountTier::Demo, &default, Some(10.0))];
    let t = target("bybit", AccountTier::Demo, None);
    let ask = start(Some(10.0), Some(3.0));
    let seen = || std::mem::take(&mut *CAPTURE_B.seen.lock().expect("seen"));
    let begin = |lane: &BridgeTrade| {
        let (tx, _rx) = vike_exec::event_channel(8);
        drop(lane.start_adapter(&t, &ask, tx, None).expect("the planted bridge comes up"));
        seen()
    };
    let lane_b = |reads: PostedReads| {
        let mut policy = vike_tradehub_core::AdapterPolicy::default();
        for r in &rows {
            policy = policy.with_account_row(r.clone());
        }
        BridgeTrade::with_snapshot(&CAPTURE_B_REG, HashMap::new(), snapshot_of(&policy))
            .with_posted_reads(reads)
    };
    seen();

    // The refused start: the bridge keeps the profile's 3x, and nothing is confirmed afterwards.
    let script = Script::holding(10.0).timing_out_first(1);
    let (reads, waits) = reads_over(&script);
    let l = lane_b(reads);
    assert_eq!(begin(&l), [Some(3.0)], "the probe refused: the profile went through untouched");
    let got = l.leverage_cap(&t, &ask);
    let Some(Err(why)) = got else { panic!("a venue that merely happens to hold it: {got:?}") };
    assert!(why.contains("did not write it"), "{why}");
    assert!(waits.lock().expect("waits").is_empty(), "no poll for a figure nobody wrote");

    // A start that passes records the hand-over, and the same read-back then confirms.
    let script = Script::holding(10.0);
    let (reads, _) = reads_over(&script);
    let l = lane_b(reads);
    assert_eq!(begin(&l), [Some(10.0)], "the probe passed: the bridge is handed the figure");
    assert_eq!(l.leverage_cap(&t, &ask), Some(Ok(10.0)));

    // A lane that never started the account confirms nothing, and a different figure than the one
    // handed over confirms nothing either.
    let (reads, _) = reads_over(&Script::holding(10.0));
    let never_started = lane_b(reads);
    let got = never_started.leverage_cap(&t, &ask);
    assert!(matches!(&got, Some(Err(why)) if why.contains("no record")), "{got:?}");
    let (reads, _) = reads_over(&Script::holding(10.0));
    let other = lane_b(reads);
    other.handed_over_for_test(None, Ok(4.0));
    let got = other.leverage_cap(&t, &ask);
    assert!(matches!(&got, Some(Err(why)) if why.contains("handed the bridge 4x")), "{got:?}");
}

// ---- the handover to the bridge --------------------------------------------------------------------

#[test]
fn the_bridge_is_handed_the_figure_only_when_the_probe_says_yes() {
    let default = AccountLabel::Default;
    let rows = [
        row(1, "bybit", AccountTier::Demo, &default, Some(10.0)),
        row(2, "bybit", AccountTier::Demo, &alt(), None),
    ];
    let seen = || std::mem::take(&mut *CAPTURE.seen.lock().expect("seen"));
    let begin = |lane: &BridgeTrade, t: &TradeTarget, s: &AdapterStart| {
        let (tx, _rx) = vike_exec::event_channel(8);
        drop(lane.start_adapter(t, s, tx, None).expect("the planted bridge comes up"));
        seen()
    };
    seen();

    // A clean venue: the bridge's profile carries the account's figure (over the profile's own 3x).
    let script = Script::holding(2.0);
    let (reads, _) = reads_over(&script);
    let l = lane(&rows, reads);
    let t = target("bybit", AccountTier::Demo, None);
    assert_eq!(begin(&l, &t, &start(Some(10.0), Some(3.0))), [Some(10.0)]);
    // …also when the start carried no profile at all.
    assert_eq!(begin(&l, &t, &start(Some(10.0), None)), [Some(10.0)]);

    // A refused probe: the profile goes through UNTOUCHED (3x here; none on the other), which is
    // today's start including the default write.
    let mut isolated = Script::holding(2.0);
    isolated.account = Err("the account is in isolated margin".to_string());
    let (reads, _) = reads_over(&isolated);
    let l = lane(&rows, reads);
    assert_eq!(begin(&l, &t, &start(Some(10.0), Some(3.0))), [Some(3.0)]);
    assert_eq!(begin(&l, &t, &start(Some(10.0), None)), [None]);

    // No ask: nothing changes, and no reader is built.
    let script = Script::holding(2.0);
    let (reads, _) = reads_over(&script);
    let l = lane(&rows, reads);
    assert_eq!(begin(&l, &t, &start(None, Some(3.0))), [Some(3.0)]);
    assert_eq!(script.built.load(Ordering::SeqCst), 0);

    // Account A states and B of the same venue does not: B's bridge gets the profile untouched.
    let b = target("bybit", AccountTier::Demo, Some("ALT"));
    let script = Script::holding(2.0);
    let (reads, _) = reads_over(&script);
    let l = lane(&rows, reads);
    assert_eq!(begin(&l, &b, &start(Some(10.0), Some(3.0))), [Some(3.0)]);
    assert_eq!(script.built.load(Ordering::SeqCst), 0, "B asked nothing of the venue");
}
