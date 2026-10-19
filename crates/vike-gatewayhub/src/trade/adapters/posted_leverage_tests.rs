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
//! | [`default_accounts_of_three_venues_keep_their_own_figures_across_a_tradehub_only_restart`] | one record for every venue's default account: the last start's figure answers all three re-asks, a start of a venue that posts nothing wipes them |
//! | [`a_venue_that_posts_nothing_starting_does_not_wipe_another_venues_record`] | the same wipe, minimal: a default account of a non-posting venue erasing bybit's hand-over |
//! | [`two_default_accounts_starting_at_once_do_not_overwrite_each_other`] | two venues' starts racing between forgetting and recording, the later record replacing the earlier one's |
//! | [`a_symbol_admitted_on_demand_is_written_once_and_seen_holding_the_figure`] | a symbol admitted later keeping the venue's old leverage, a repeated admission writing again, a figure applied that the venue was never read holding |
//! | [`a_leverage_changed_on_the_venue_is_converged_and_a_position_is_never_moved`] | a venue-side change under the account's figure left in place, or a leverage moved under an open position |
//! | [`every_refusal_is_a_reason_and_a_venue_left_as_it_was`] | a hedge, isolated, above-maximum, unreadable, non-perpetual or unwritable symbol half applied, a write that never landed assumed to have, a bridge with no verb read as success |
//! | [`nothing_is_asked_of_a_venue_unless_the_whole_gate_opens`] | a venue call for a start that asked nothing, a closed tier, a withheld start, a cleared row, the mounted symbol, a spot symbol or a venue with another mechanism |
//! | [`two_accounts_and_two_venues_admitting_at_once_each_get_their_own_figure`] | one account's figure, symbol or write landing on another account or venue |

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    venue: &'static str,
    seen: Mutex<Vec<Option<f64>>>,
}

impl VenueAdapter for Capture {
    fn venue(&self) -> &'static str {
        self.venue
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

static CAPTURE: Capture = Capture { venue: "bybit", seen: Mutex::new(Vec::new()) };
static CAPTURE_REG: [VenueRow; 1] = [VenueRow::Adapter(&CAPTURE)];
/// A second bridge for the test that starts accounts beside `the_bridge_is_handed_…` (the tests of one
/// binary run on threads, and a shared record would interleave).
static CAPTURE_B: Capture = Capture { venue: "bybit", seen: Mutex::new(Vec::new()) };
static CAPTURE_B_REG: [VenueRow; 1] = [VenueRow::Adapter(&CAPTURE_B)];
/// One lane, four venues: the three that post a leverage and one that posts nothing (`oanda`), for the
/// tests of the hand-over record, which is shared by every venue of the lane.
static MULTI_BYBIT: Capture = Capture { venue: "bybit", seen: Mutex::new(Vec::new()) };
static MULTI_OKX: Capture = Capture { venue: "okx", seen: Mutex::new(Vec::new()) };
static MULTI_BINANCE: Capture = Capture { venue: "binance", seen: Mutex::new(Vec::new()) };
static MULTI_OANDA: Capture = Capture { venue: "oanda", seen: Mutex::new(Vec::new()) };
static MULTI_REG: [VenueRow; 4] = [
    VenueRow::Adapter(&MULTI_BYBIT),
    VenueRow::Adapter(&MULTI_OKX),
    VenueRow::Adapter(&MULTI_BINANCE),
    VenueRow::Adapter(&MULTI_OANDA),
];

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

fn lane_over(
    registry: &'static [VenueRow],
    rows: &[vike_secrets::Account],
    reads: PostedReads,
) -> BridgeTrade {
    let mut policy = vike_tradehub_core::AdapterPolicy::default();
    for r in rows {
        policy = policy.with_account_row(r.clone());
    }
    BridgeTrade::with_snapshot(registry, HashMap::new(), snapshot_of(&policy))
        .with_posted_reads(reads)
}

fn lane(rows: &[vike_secrets::Account], reads: PostedReads) -> BridgeTrade {
    lane_over(&CAPTURE_REG, rows, reads)
}

/// [`lane`] after a start that handed the bridge `figure` (bybit's default account's hand-over, recorded
/// directly so the scripted venue's reads are left for the read-back under test).
fn lane_handed(rows: &[vike_secrets::Account], reads: PostedReads, figure: f64) -> BridgeTrade {
    let lane = lane(rows, reads);
    lane.handed_over_for_test(&target("bybit", AccountTier::Demo, None), Ok(figure));
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
    other.handed_over_for_test(&t, Ok(4.0));
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

// ---- one lane, several venues' default accounts -----------------------------------------------------

/// The reads of several venues at once: each venue is read from ITS OWN script (they hold different
/// figures). `on_build` runs whenever a reader is built, which is where a test that races two starts
/// makes them meet.
fn reads_by_venue(
    scripts: &[(&str, &Script)],
    on_build: impl Fn() + Send + Sync + 'static,
) -> PostedReads {
    let scripts: HashMap<String, Script> =
        scripts.iter().map(|(venue, script)| ((*venue).to_string(), (*script).clone())).collect();
    let factory: ReaderFactory = Arc::new(
        move |venue: &str,
              _creds: &AccountCredentials<'_>,
              _tier: AccountTier,
              _symbol: &str|
              -> Result<Box<dyn LeverageReader>, String> {
            on_build();
            let script = scripts.get(venue).ok_or_else(|| format!("{venue} is not scripted"))?;
            script.built.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(ScriptedReader(script.clone())))
        },
    );
    PostedReads::scripted(factory, Arc::new(|_: Duration| {}))
}

/// The default account of bybit (10x), okx (5x) and binance (10x), and of `oanda`, which posts nothing.
fn default_accounts() -> [vike_secrets::Account; 4] {
    let default = AccountLabel::Default;
    [
        row(1, "bybit", AccountTier::Demo, &default, Some(10.0)),
        row(2, "okx", AccountTier::Demo, &default, Some(5.0)),
        row(3, "binance", AccountTier::Demo, &default, Some(10.0)),
        row(4, "oanda", AccountTier::Demo, &default, None),
    ]
}

/// The daemon's start of `venue`'s default account asking `ask`; returns the target and start a later
/// re-ask on the running adapter presents.
fn start_default(lane: &BridgeTrade, venue: &str, ask: Option<f64>) -> (TradeTarget, AdapterStart) {
    let (t, s) = (target(venue, AccountTier::Demo, None), start(ask, None));
    let (tx, _rx) = vike_exec::event_channel(8);
    drop(lane.start_adapter(&t, &s, tx, None).expect("the planted bridge comes up"));
    (t, s)
}

/// **Three venues' default accounts hold different figures, and a tradehub-only restart re-asks each.**
/// The lease ends and the session does not, so every `AdapterStart` finds its adapter running and only
/// asks the lane for the figure again (`start_adapter_locked`): the record the original start wrote,
/// for THAT venue's account, must still answer, whatever else started on the lane since.
#[test]
fn default_accounts_of_three_venues_keep_their_own_figures_across_a_tradehub_only_restart() {
    let rows = default_accounts();
    let (bybit, okx, binance) =
        (Script::holding(10.0), Script::holding(5.0), Script::holding(10.0));
    let reads = reads_by_venue(&[("bybit", &bybit), ("okx", &okx), ("binance", &binance)], || {});
    let lane = lane_over(&MULTI_REG, &rows, reads);

    // The first boot: each start is followed by its own read-back.
    let asks = [("bybit", 10.0), ("okx", 5.0), ("binance", 10.0)];
    let started: Vec<_> = asks
        .iter()
        .map(|(venue, figure)| {
            let (t, s) = start_default(&lane, venue, Some(*figure));
            assert_eq!(lane.leverage_cap(&t, &s), Some(Ok(*figure)), "{venue}: the first boot");
            (t, s)
        })
        .collect();

    let reask = |when: &str| {
        for ((t, s), (venue, figure)) in started.iter().zip(asks) {
            assert_eq!(lane.leverage_cap(t, s), Some(Ok(figure)), "{venue}: {when}");
        }
    };
    reask("re-asked after a tradehub-only restart");

    // A venue that posts nothing starts its default account meanwhile: the three records stay.
    start_default(&lane, "oanda", None);
    reask("re-asked after oanda's default account started");
}

/// A default account of a venue that posts nothing clears only its OWN record: bybit's hand-over,
/// recorded by its start, still answers its read-back after oanda's start (which asks nothing).
#[test]
fn a_venue_that_posts_nothing_starting_does_not_wipe_another_venues_record() {
    let rows = default_accounts();
    let bybit = Script::holding(10.0);
    let lane = lane_over(&MULTI_REG, &rows, reads_by_venue(&[("bybit", &bybit)], || {}));
    let (t, s) = start_default(&lane, "bybit", Some(10.0));

    let (oanda, oanda_start) = start_default(&lane, "oanda", None);

    assert_eq!(lane.leverage_cap(&t, &s), Some(Ok(10.0)));
    assert_eq!(lane.leverage_cap(&oanda, &oanda_start), None, "oanda asked nothing");
}

/// Two venues' default accounts start at the same moment, each between clearing its record and
/// writing it: the later write must not replace the earlier one's.
#[test]
fn two_default_accounts_starting_at_once_do_not_overwrite_each_other() {
    let rows = default_accounts();
    let (bybit, okx) = (Script::holding(10.0), Script::holding(5.0));
    // Each start meets the other inside its probe (a reader is built there), i.e. after its record was
    // cleared and before it is written. The wait is bounded: a start that never gets there still ends.
    let racing = Arc::new(AtomicBool::new(true));
    let arrived = Arc::new(AtomicUsize::new(0));
    let reads = reads_by_venue(&[("bybit", &bybit), ("okx", &okx)], {
        let (racing, arrived) = (Arc::clone(&racing), Arc::clone(&arrived));
        move || {
            if racing.load(Ordering::SeqCst) {
                arrived.fetch_add(1, Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_secs(5);
                while arrived.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                    std::thread::yield_now();
                }
            }
        }
    });
    let lane = lane_over(&MULTI_REG, &rows, reads);

    let asks = [("bybit", 10.0), ("okx", 5.0)];
    let started: Vec<_> = std::thread::scope(|scope| {
        let lane = &lane;
        let handles: Vec<_> = asks
            .iter()
            .map(|(venue, figure)| scope.spawn(move || start_default(lane, venue, Some(*figure))))
            .collect();
        handles.into_iter().map(|h| h.join().expect("a start ended")).collect()
    });
    racing.store(false, Ordering::SeqCst);

    assert_eq!(arrived.load(Ordering::SeqCst), 2, "both starts met inside their probes");
    for ((t, s), (venue, figure)) in started.iter().zip(asks) {
        assert_eq!(lane.leverage_cap(t, s), Some(Ok(figure)), "{venue}");
    }
}

// ---- a symbol admitted on demand -------------------------------------------------------------------

impl Script {
    /// The account holds an open position on the symbol, at whatever leverage the venue holds now.
    fn with_open_position(self) -> Self {
        for read in self.reads.lock().expect("script").iter_mut() {
            read.position_open = true;
        }
        self
    }
}

/// One write the bridge's verb saw: (the writing account's label text, symbol, figure).
type Write = (Option<String>, String, f64);

/// A bridge whose verb of interest is `set_symbol_leverage`: it logs every write as (the writing
/// account's label text, symbol, figure) and, unless told otherwise, makes the scripted venue HOLD what it
/// was written, so a write the venue accepted is what the next read sees.
struct Writer {
    venue: &'static str,
    /// What the venue holds for each symbol: the scripts its readers read.
    held: HashMap<String, Script>,
    /// `false`: the write is accepted and never lands (a key without trade permission).
    lands: bool,
    /// The write itself fails with this.
    fails: Option<String>,
    writes: Mutex<Vec<Write>>,
}

impl Writer {
    fn new(venue: &'static str, symbols: &[(&str, Script)]) -> Self {
        Writer {
            venue,
            held: symbols
                .iter()
                .map(|(symbol, script)| ((*symbol).to_string(), script.clone()))
                .collect(),
            lands: true,
            fails: None,
            writes: Mutex::new(Vec::new()),
        }
    }

    fn never_landing(mut self) -> Self {
        self.lands = false;
        self
    }

    fn failing(mut self, why: &str) -> Self {
        self.fails = Some(why.to_string());
        self
    }
}

impl VenueAdapter for Writer {
    fn venue(&self) -> &'static str {
        self.venue
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        AdapterOutcome::paper()
    }
    fn set_symbol_leverage(
        &self,
        inputs: &AdapterInputs<'_>,
        symbol: &str,
        leverage: f64,
    ) -> Result<(), String> {
        self.writes.lock().expect("writes").push((
            inputs.account.text().map(str::to_string),
            symbol.to_string(),
            leverage,
        ));
        if let Some(why) = &self.fails {
            return Err(why.clone());
        }
        if self.lands
            && let Some(script) = self.held.get(symbol)
        {
            script.now_holding(leverage);
        }
        Ok(())
    }
}

/// A lane over `writers` (one registry row each) whose readers read the writers' own scripts, and a fake
/// clock that records every wait. `on_build` runs whenever a reader is built.
struct World {
    lane: BridgeTrade,
    writers: HashMap<&'static str, &'static Writer>,
    waits: Arc<Mutex<Vec<Duration>>>,
    built: Arc<AtomicUsize>,
}

impl World {
    fn writes(&self, venue: &str) -> Vec<Write> {
        self.writers[venue].writes.lock().expect("writes").clone()
    }

    fn waited(&self) -> Duration {
        self.waits.lock().expect("waits").iter().sum()
    }
}

fn world(
    rows: &[vike_secrets::Account],
    writers: Vec<Writer>,
    on_build: impl Fn() + Send + Sync + 'static,
) -> World {
    // Leaked: a registry row is `&'static`, and a test process is short.
    let writers: Vec<&'static Writer> =
        writers.into_iter().map(|w| &*Box::leak(Box::new(w))).collect();
    let registry: &'static [VenueRow] = Box::leak(
        writers.iter().map(|w| VenueRow::Adapter(*w)).collect::<Vec<_>>().into_boxed_slice(),
    );
    let by_venue: HashMap<&'static str, &'static Writer> =
        writers.iter().map(|w| (w.venue, *w)).collect();
    let built = Arc::new(AtomicUsize::new(0));
    let factory: ReaderFactory = {
        let (by_venue, built) = (by_venue.clone(), Arc::clone(&built));
        Arc::new(
            move |venue: &str,
                  _creds: &AccountCredentials<'_>,
                  _tier: AccountTier,
                  symbol: &str|
                  -> Result<Box<dyn LeverageReader>, String> {
                on_build();
                built.fetch_add(1, Ordering::SeqCst);
                let writer =
                    by_venue.get(venue).ok_or_else(|| format!("{venue} is not scripted"))?;
                let script = writer
                    .held
                    .get(symbol)
                    .ok_or_else(|| format!("{venue}: the venue lists no perpetual {symbol}"))?;
                Ok(Box::new(ScriptedReader(script.clone())))
            },
        )
    };
    let waits = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&waits);
    let pause = Arc::new(move |d: Duration| recorded.lock().expect("waits").push(d));
    let lane = lane_over(registry, rows, PostedReads::scripted(factory, pause));
    World { lane, writers: by_venue, waits, built }
}

/// **A perpetual admitted on demand is written ONCE and then SEEN holding the account's figure**, and
/// admitting it again finds the venue holding it and writes nothing (the venue is read, never assumed).
#[test]
fn a_symbol_admitted_on_demand_is_written_once_and_seen_holding_the_figure() {
    let default = AccountLabel::Default;
    let rows = [row(1, "bybit", AccountTier::Demo, &default, Some(10.0))];
    let eth = Script::holding(2.0);
    let w = world(&rows, vec![Writer::new("bybit", &[("ETHUSDT", eth.clone())])], || {});
    let (t, ask) = (target("bybit", AccountTier::Demo, None), start(Some(10.0), None));

    assert_eq!(w.lane.symbol_leverage(&t, &ask, "ETHUSDT"), Some(Ok(10.0)));
    assert_eq!(
        w.writes("bybit"),
        [(None, "ETHUSDT".to_string(), 10.0)],
        "one write, through the bridge's own verb, naming the admitted symbol and the figure"
    );
    assert_eq!(w.waited(), Duration::ZERO, "the first read after the write saw it");

    // The same symbol admitted again (a refresh, a boot re-admission, a retell after a restart).
    assert_eq!(w.lane.symbol_leverage(&t, &ask, "ETHUSDT"), Some(Ok(10.0)));
    assert_eq!(w.writes("bybit").len(), 1, "the venue already holds it: nothing is written");

    // The ceilings lowered the ask below the row's figure: the lane writes the LOWER number.
    eth.now_holding(2.0);
    assert_eq!(w.lane.symbol_leverage(&t, &start(Some(4.0), None), "ETHUSDT"), Some(Ok(4.0)));
    assert_eq!(w.writes("bybit").last(), Some(&(None, "ETHUSDT".to_string(), 4.0)));
}

/// **The venue is converged, not trusted**: a leverage the operator changed on the exchange since is
/// written back to the account's figure at the next admission, but never under an open position at
/// another number.
#[test]
fn a_leverage_changed_on_the_venue_is_converged_and_a_position_is_never_moved() {
    let default = AccountLabel::Default;
    let rows = [row(1, "bybit", AccountTier::Demo, &default, Some(10.0))];
    let eth = Script::holding(2.0);
    let w = world(&rows, vec![Writer::new("bybit", &[("ETHUSDT", eth.clone())])], || {});
    let (t, ask) = (target("bybit", AccountTier::Demo, None), start(Some(10.0), None));
    assert_eq!(w.lane.symbol_leverage(&t, &ask, "ETHUSDT"), Some(Ok(10.0)));

    eth.now_holding(3.0);
    assert_eq!(w.lane.symbol_leverage(&t, &ask, "ETHUSDT"), Some(Ok(10.0)), "written back");
    assert_eq!(w.writes("bybit").len(), 2);

    // A position opened meanwhile, and the operator moved the leverage under it: refused, not moved.
    let eth_open = Script::holding(3.0).with_open_position();
    let w = world(&rows, vec![Writer::new("bybit", &[("ETHUSDT", eth_open)])], || {});
    let Some(Err(why)) = w.lane.symbol_leverage(&t, &ask, "ETHUSDT") else {
        panic!("a position at another leverage must be refused");
    };
    assert!(why.contains("open position is held at 3x"), "{why}");
    assert!(w.writes("bybit").is_empty());
}

/// **Every refusal and failure is a reason and a venue left exactly as it was**: not plain cross or a
/// hedge account, a position at another leverage, a figure above the symbol's maximum, a read that
/// fails, a symbol the venue lists no perpetual for (spot, dated), a write the venue refuses, and a
/// write that is accepted and never lands (the whole bounded poll, then an error naming both numbers).
#[test]
fn every_refusal_is_a_reason_and_a_venue_left_as_it_was() {
    let default = AccountLabel::Default;
    let rows = [row(1, "bybit", AccountTier::Demo, &default, Some(10.0))];
    let (t, ask) = (target("bybit", AccountTier::Demo, None), start(Some(10.0), None));

    let mut hedge = Script::holding(2.0);
    hedge.account = Err("the venue answered 2 position rows for ETHUSDT: hedge mode".to_string());
    let mut low = Script::holding(2.0);
    low.max = Some(5.0);
    let refused: [(&str, Script, &str); 4] = [
        ("hedge or not plain cross", hedge, "hedge mode"),
        (
            "a position at another leverage",
            Script::holding(3.0).with_open_position(),
            "open position is held at 3x",
        ),
        ("above the symbol's maximum", low, "above the venue's maximum of 5x"),
        ("a read that fails", Script::holding(2.0).timing_out_first(1), "could not be read"),
    ];
    for (what, script, reason) in refused {
        let w = world(&rows, vec![Writer::new("bybit", &[("ETHUSDT", script)])], || {});
        let got = w.lane.symbol_leverage(&t, &ask, "ETHUSDT");
        let Some(Err(why)) = got else { panic!("{what}: must be refused, got {got:?}") };
        assert!(why.contains(reason), "{what}: {why}");
        assert!(w.writes("bybit").is_empty(), "{what}: nothing is written");
        assert_eq!(w.waited(), Duration::ZERO, "{what}: nothing is polled");
    }

    // A symbol the venue lists no perpetual for: the reader cannot be built, so nothing is read or written.
    let w = world(&rows, vec![Writer::new("bybit", &[("ETHUSDT", Script::holding(2.0))])], || {});
    let got = w.lane.symbol_leverage(&t, &ask, "ETHUSDT-25JUL26");
    assert!(matches!(&got, Some(Err(why)) if why.contains("lists no perpetual")), "{got:?}");
    assert!(w.writes("bybit").is_empty());

    // The venue refuses the write: its words, no poll.
    let writer = Writer::new("bybit", &[("ETHUSDT", Script::holding(2.0))])
        .failing("10005 permission denied");
    let w = world(&rows, vec![writer], || {});
    assert_eq!(
        w.lane.symbol_leverage(&t, &ask, "ETHUSDT"),
        Some(Err("the venue was not written: 10005 permission denied".to_string()))
    );
    assert_eq!((w.writes("bybit").len(), w.waited()), (1, Duration::ZERO));

    // The write is accepted and never lands: ONE write, the whole schedule, an error with both numbers.
    let writer = Writer::new("bybit", &[("ETHUSDT", Script::holding(2.0))]).never_landing();
    let w = world(&rows, vec![writer], || {});
    let got = w.lane.symbol_leverage(&t, &ask, "ETHUSDT");
    let Some(Err(why)) = got else { panic!("never held must be an error: {got:?}") };
    assert!(
        why.contains("still holds 2x") && why.contains("10x") && why.contains("just now"),
        "{why}"
    );
    assert_eq!(w.writes("bybit").len(), 1, "one write and no retry");
    assert_eq!(w.waited(), Duration::from_millis(5000));

    // A bridge that has no per-symbol verb (the trait's default) is an error, never a silent success.
    let (reads, _) = reads_over(&Script::holding(2.0));
    let got = lane(&rows, reads).symbol_leverage(&t, &ask, "ETHUSDT");
    assert!(
        matches!(&got, Some(Err(why)) if why.contains("writes no per-symbol leverage")),
        "{got:?}"
    );
}

/// **Nothing is asked of a venue unless the whole gate opens**: no figure asked by the node (an older
/// tradehub, or none stated), a closed tier, a withheld start, a row that no longer states it, the
/// MOUNTED symbol (the start's own write and read-back), binance's SPOT symbol, and a venue with another
/// mechanism. No reader is built and nothing is written; the positive control proves the same world
/// does answer when it should.
#[test]
fn nothing_is_asked_of_a_venue_unless_the_whole_gate_opens() {
    let default = AccountLabel::Default;
    let bybit_rows = [row(1, "bybit", AccountTier::Demo, &default, Some(10.0))];
    let eth = [("ETHUSDT", Script::holding(2.0))];
    let t = target("bybit", AccountTier::Demo, None);
    let silent =
        |rows: &[vike_secrets::Account], t: &TradeTarget, s: &AdapterStart, symbol: &str| {
            let venue: &'static str = Box::leak(t.venue.clone().into_boxed_str());
            let w = world(rows, vec![Writer::new(venue, &eth)], || {});
            assert_eq!(w.lane.symbol_leverage(t, s, symbol), None, "{venue} {symbol}");
            assert_eq!(w.built.load(Ordering::SeqCst), 0, "no reader was built");
            assert!(w.writes(venue).is_empty());
        };
    silent(&bybit_rows, &t, &start(None, Some(3.0)), "ETHUSDT");
    let mut withheld = start(Some(10.0), None);
    withheld.withhold = Some(Withhold::DataOnly);
    silent(&bybit_rows, &t, &withheld, "ETHUSDT");
    let cleared = [row(1, "bybit", AccountTier::Demo, &default, None)];
    silent(&cleared, &t, &start(Some(10.0), None), "ETHUSDT");
    let live = [row(1, "bybit", AccountTier::Live, &default, Some(10.0))];
    silent(&live, &target("bybit", AccountTier::Live, None), &start(Some(10.0), None), "ETHUSDT");
    // The row states 10 and the start asks 12: the lane's own reading wins.
    silent(&bybit_rows, &t, &start(Some(12.0), None), "ETHUSDT");
    // The mounted symbol is the start's, in any spelling of its case.
    silent(&bybit_rows, &t, &start(Some(10.0), None), "BTCUSDT");
    silent(&bybit_rows, &t, &start(Some(10.0), None), "btcusdt");
    // Hyperliquid's number is a cap read by its own path, not a posted one.
    let hl = [row(1, "hyperliquid", AccountTier::Demo, &default, Some(10.0))];
    silent(&hl, &target("hyperliquid", AccountTier::Demo, None), &start(Some(10.0), None), "ETH");

    // binance: a bare symbol is SPOT and gets no figure; the perpetual of the same name does.
    let binance_rows = [row(2, "binance", AccountTier::Demo, &default, Some(10.0))];
    let bt = target("binance", AccountTier::Demo, None);
    silent(&binance_rows, &bt, &start(Some(10.0), None), "ETHUSDT");
    let w = world(
        &binance_rows,
        vec![Writer::new("binance", &[("ETHUSDT.P", Script::holding(2.0))])],
        || {},
    );
    assert_eq!(w.lane.symbol_leverage(&bt, &start(Some(10.0), None), "ETHUSDT.P"), Some(Ok(10.0)));
    assert!(is_spot_symbol("binance", "ETHUSDT") && !is_spot_symbol("binance", "ETHUSDT.P"));
    assert!(!is_spot_symbol("bybit", "ETHUSDT") && !is_spot_symbol("okx", "ETH-USDT-SWAP"));
}

/// **Two accounts and two venues admitting at once each get their own figure** (the keying lesson of
/// v0.1.76's hand-over record): bybit's default account (10x on ETHUSDT), bybit's ALT account (5x on
/// SOLUSDT) and okx's default account (3x on its swap) meet INSIDE their probes, and every write names the
/// account, symbol and figure of its own ask. The lane keeps nothing between asks, so there is nothing
/// to collide.
#[test]
fn two_accounts_and_two_venues_admitting_at_once_each_get_their_own_figure() {
    let default = AccountLabel::Default;
    let rows = [
        row(1, "bybit", AccountTier::Demo, &default, Some(10.0)),
        row(2, "bybit", AccountTier::Demo, &alt(), Some(5.0)),
        row(3, "okx", AccountTier::Demo, &default, Some(3.0)),
    ];
    let racing = Arc::new(AtomicBool::new(true));
    let arrived = Arc::new(AtomicUsize::new(0));
    let meet = {
        let (racing, arrived) = (Arc::clone(&racing), Arc::clone(&arrived));
        move || {
            if racing.load(Ordering::SeqCst) {
                arrived.fetch_add(1, Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_secs(5);
                while arrived.load(Ordering::SeqCst) < 3 && Instant::now() < deadline {
                    std::thread::yield_now();
                }
            }
        }
    };
    let w = world(
        &rows,
        vec![
            Writer::new(
                "bybit",
                &[("ETHUSDT", Script::holding(2.0)), ("SOLUSDT", Script::holding(2.0))],
            ),
            Writer::new("okx", &[("ETH-USDT-SWAP", Script::holding(1.0))]),
        ],
        meet,
    );

    let asks = [
        (target("bybit", AccountTier::Demo, None), start(Some(10.0), None), "ETHUSDT", 10.0),
        (target("bybit", AccountTier::Demo, Some("ALT")), start(Some(5.0), None), "SOLUSDT", 5.0),
        (target("okx", AccountTier::Demo, None), start(Some(3.0), None), "ETH-USDT-SWAP", 3.0),
    ];
    let answers: Vec<_> = std::thread::scope(|scope| {
        let lane = &w.lane;
        let handles: Vec<_> = asks
            .iter()
            .map(|(t, s, symbol, _)| scope.spawn(move || lane.symbol_leverage(t, s, symbol)))
            .collect();
        handles.into_iter().map(|h| h.join().expect("an ask ended")).collect()
    });
    racing.store(false, Ordering::SeqCst);

    assert_eq!(arrived.load(Ordering::SeqCst), 3, "all three asks met inside their probes");
    for (answer, (_, _, symbol, figure)) in answers.iter().zip(&asks) {
        assert_eq!(answer, &Some(Ok(*figure)), "{symbol}");
    }
    let mut bybit = w.writes("bybit");
    bybit.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        bybit,
        [
            (None, "ETHUSDT".to_string(), 10.0),
            (Some("ALT".to_string()), "SOLUSDT".to_string(), 5.0),
        ],
        "each account's write carries its own label, symbol and figure"
    );
    assert_eq!(w.writes("okx"), [(None, "ETH-USDT-SWAP".to_string(), 3.0)]);
}
