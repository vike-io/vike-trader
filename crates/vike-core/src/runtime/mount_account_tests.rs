//! **A strategy mount names its ACCOUNT, and its orders and reads go there** — the gate over the
//! last unaddressable half of multi-account support.
//!
//! # What was wrong
//!
//! Everything below the mount could already address a second account: labelled credentials, the
//! per-account ceilings, `vike_mount::make_engine_accounts`' fan-out, inbound routing by route key,
//! per-account reconcile, the GUI. A STRATEGY could not — `StrategyMount` carried
//! `(venue, symbol, interval)` and nothing else — so the extra engines were mounted, were reachable
//! INBOUND, and were unaddressable OUTBOUND. Every strategy lane resolved its engine with
//! `engine_idx_for_route_key(RouteKey::sole_account_of(<a venue string>))`, which by construction
//! can only ever answer with a venue's DEFAULT account.
//!
//! # The two halves, and why the second is the dangerous one
//!
//! * **WRITES.** [`CoreThread::apply_strategy_intent`] is the ONE choke point every strategy-minted
//!   intent lowers through, and it now passes `EngineRoute::Mount(idx)` rather than letting the
//!   payload's venue string decide. The account never becomes a string, so a misroute has no
//!   spelling.
//! * **READS.** `Broker::position`/`equity`/`multiplier`/`lot_size` resolve through the same
//!   `CoreThread::mount_engine` index. A mount that traded account `ALT` while SIZING against the
//!   default account's position would be a worse defect than the one the field fixes, and no
//!   order-routing test would catch it — so the read half is asserted here beside the write half,
//!   on the same core, in the same test.
//!
//! # ⚠ THE HEADLINE CONFIGURATION: two accounts, ONE symbol
//!
//! That is the spread the deleted symbol-collision rule refused (`vike_config::venue_accounts`), so
//! every test here mounts both accounts on [`SYMBOL`] — which also makes the assertions strictly
//! harder, since a first-match-by-`(venue, symbol)` lookup would find the WRONG engine rather than
//! none at all.
//!
//! White-box, in-crate: `mount_engine`, `route_of`, `apply_strategy_intent` and `eng` are private
//! to the runtime module and are exactly what is under test. `use super::*` re-exports it, the
//! sibling-test-module idiom of `safe_state_tests.rs` / `route_key_tests.rs`.

use super::*;
use std::sync::Mutex as TestMutex;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, RiskGate, RiskLimits};
use vike_model::account_keys::AccountLabel;

/// The canonical exchange both accounts belong to. A roster id, so every capability table resolves
/// a real row for either engine.
const CANON: &str = "binance";
/// ⚠ ONE symbol for BOTH accounts — the spread configuration. See the module doc.
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// One engine on [`CANON`]/[`SYMBOL`] whose ROUTING key is `route_key` — the shape
/// `vike_mount::make_engine_for_account` produces (canonical venue everywhere, `route_key`
/// decorated).
fn engine(route_key: &str, seed: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(seed, CANON, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        CANON,
        SYMBOL,
    );
    e.route_key = route_key.to_string();
    e.collect_applied_fills = true;
    e
}

/// What each mount reported about ITS OWN book from inside one dispatch: `(label, position,
/// equity)`. A named alias because the shared cell is three types deep and the raw spelling says
/// nothing at the three sites that pass it.
type Seen = Arc<TestMutex<Vec<(&'static str, f64, f64)>>>;

/// A strategy that submits one tagged limit on `on_feed_status` and records what its broker told it
/// about its own book at that moment — the two halves (write, read) captured from inside ONE
/// dispatch, which is the only place they can be observed together.
struct Prober {
    label: &'static str,
    seen: Seen,
}

impl Strategy<LiveBroker> for Prober {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        self.seen.lock().unwrap().push((self.label, broker.position, broker.equity));
        broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
    }
}

fn mount(
    label: &'static str,
    account: Option<AccountLabel>,
    controller_id: &str,
    seen: &Seen,
) -> StrategyMount {
    StrategyMount {
        account,
        symbols: Vec::new(),
        controller_id: Some(controller_id.to_string()),
        underlying_symbol: None,
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        strategy: Box::new(Prober { label, seen: Arc::clone(seen) }),
    }
}

fn core_of(
    config: CoreConfig,
    primary: ExecutionEngine<RecordingClient>,
    extras: Vec<(f64, ExecutionEngine<RecordingClient>)>,
) -> CoreThread<RecordingClient> {
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&primary.venue, &primary.symbol)));
    assemble_core(primary, extras, config, market, snapshot, Arc::new(AtomicU64::new(0)))
}

/// The headline core: the DEFAULT account's engine plus `binance#ALT`, and two mounts on ONE
/// symbol — one on each account.
fn spread_core(seen: &Seen) -> CoreThread<RecordingClient> {
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount("default", None, "m-default", seen)),
        extra_mounts: vec![mount("alt", Some(alt()), "m-alt", seen)],
        ..CoreConfig::default()
    };
    core_of(config, engine(CANON, 1_000.0), vec![(2_000.0, engine("binance#ALT", 2_000.0))])
}

/// ⚠ **THE HEADLINE.** Two mounts, DIFFERENT accounts, the SAME symbol: both run, they resolve
/// DISTINCT engines, and each one's order reaches its own engine's client.
///
/// This is the arming the symbol-collision rule refused — long BTC on one wallet, short BTC on the
/// other — and it is also the configuration in which a `(venue, symbol)` first-match lookup fails
/// SILENTLY rather than loudly: both engines claim `binance`/`BTCUSDT`, so the wrong answer is a
/// well-formed one.
#[test]
fn two_mounts_on_one_symbol_and_two_accounts_each_reach_their_own_engine() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let mut core = spread_core(&seen);

    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");
    assert_eq!(
        core.mount_engine,
        vec![0, 1],
        "the account-less mount resolves the DEFAULT engine, the `ALT` mount resolves its own"
    );

    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);

    // BOTH mounts ran — a lane that fanned out to one of them would leave this at 1.
    let seen = seen.lock().unwrap().clone();
    let labels: Vec<&str> = seen.iter().map(|(l, ..)| *l).collect();
    assert_eq!(labels, vec!["default", "alt"], "both mounts must be driven: {seen:?}");

    // …and each order landed on its OWN account's client. One submission each, never two on one.
    assert_eq!(core.eng(0).client.submissions.len(), 1, "the default account got exactly its own");
    assert_eq!(core.eng(1).client.submissions.len(), 1, "…and `ALT` got exactly its own");
    assert_eq!(core.eng(0).client.submissions[0].symbol, SYMBOL);
    assert_eq!(core.eng(1).client.submissions[0].symbol, SYMBOL);
}

/// **The READ half, on the same core.** Each mount's `Broker` equity is its OWN engine's, so a
/// labelled mount cannot size against the default account's book while trading its own.
///
/// The two engines are seeded with DIFFERENT capital precisely so this can be asserted at all: with
/// equal seeds the wrong answer and the right one are the same number.
#[test]
fn a_labelled_mount_trades_and_reads_its_own_account() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let mut core = spread_core(&seen);
    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    let equity_of = |label: &str| seen.iter().find(|(l, ..)| *l == label).expect("mount ran").2;
    assert!(
        (equity_of("default") - 1_000.0).abs() < 1e-9,
        "the default mount reads the DEFAULT account's equity: {seen:?}"
    );
    assert!(
        (equity_of("alt") - 2_000.0).abs() < 1e-9,
        "…and the `ALT` mount reads ALT's, not the default account's: {seen:?}"
    );
}

/// **Outbound routing picks the engine by ACCOUNT, never by first `(venue, symbol)` match.**
///
/// Asserted at the seam rather than through a lane, because the claim is about the RESOLUTION:
/// `route_of(Mount(i), venue)` must answer that mount's own engine for a venue both engines carry.
/// A first-match implementation returns `Some(0)` for both mounts and every lane test above would
/// still pass for mount 0.
#[test]
fn the_route_is_by_account_not_by_first_venue_symbol_match() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let core = spread_core(&seen);

    assert_eq!(core.route_of(EngineRoute::Mount(0), CANON), Some(0));
    assert_eq!(
        core.route_of(EngineRoute::Mount(1), CANON),
        Some(1),
        "the `ALT` mount must NOT resolve the first engine that claims (binance, BTCUSDT)"
    );
    // …and the first-match lookup really is ambiguous here, which is what makes the assertion above
    // load-bearing rather than incidental: with two engines claiming the pair it declines to answer.
    assert_eq!(
        core.engine_idx_for_venue_symbol(CANON, SYMBOL),
        None,
        "two engines claim this pair, so the symbol cannot name one"
    );
    // An EXTERNAL command (a ticket, a CLI verb) still routes by the payload — it can name no
    // account — and lands on the venue's default engine, exactly as it always has.
    assert_eq!(core.route_of(EngineRoute::Payload, CANON), Some(0));
    // A mount's route defers to the payload for a FOREIGN venue: an account is a fact about the
    // mount's own exchange and says nothing about another one.
    assert_eq!(core.route_of(EngineRoute::Mount(1), "okx"), None);
}

/// **A mount naming NO account is byte-identical to today** — a FROZEN BASELINE rather than a
/// behavioural assertion, because "unchanged" is the claim and a behavioural test can only ever
/// check the properties somebody thought to name.
///
/// The baseline is the whole per-mount engine resolution of a single-account core: one engine, one
/// mount, `mount_engine == [0]`, `multi_account == false` (which is what keeps `route_event`'s
/// disambiguation and `mirror_venue_price` inert), and the route key still the bare venue id, which
/// is what a deployment's `LIVE-<route_key>.lock` filename is derived from.
#[test]
fn an_account_less_mount_is_the_single_account_core_unchanged() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount("default", None, "m-default", &seen)),
        ..CoreConfig::default()
    };
    let mut core = core_of(config, engine(CANON, 1_000.0), Vec::new());

    assert_eq!(core.mount_engine, vec![0]);
    assert!(!core.multi_account, "one account per venue: every multi-account branch stays inert");
    assert_eq!(core.engine.route_key, CANON, "the sentinel filename does not move");
    assert_eq!(core.route_of(EngineRoute::Mount(0), CANON), Some(0));
    assert_eq!(
        core.route_of(EngineRoute::Mount(0), CANON),
        core.route_of(EngineRoute::Payload, CANON),
        "with one account the mount route and the payload route are the SAME answer, which is what \
         'byte-identical' means here"
    );

    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    assert_eq!(core.eng(0).client.submissions.len(), 1);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!((seen[0].2 - 1_000.0).abs() < 1e-9, "…reading the one account it has: {seen:?}");
}

/// **A mount naming an account this core runs no engine for PANICS, naming venue, account and
/// mount** — the backstop `mount_engine_idx` exists to be.
///
/// It is the one place in this file that is a refusal rather than a degrade, and the asymmetry is
/// the point: `unwrap_or(0)` here would be the catastrophe stated as three characters — a strategy
/// whose operator named `ALT` trading the DEFAULT account, silently. The composition root
/// (`vike_run::refuse_unarmed_mount_accounts`) refuses this case first, with a message an operator
/// can act on; this is what no future root can reach past.
#[test]
#[should_panic(expected = "names account ALT of binance")]
fn a_mount_naming_an_unmounted_account_panics_rather_than_falling_through() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 1_000.0,
        // The `ALT` mount, and NO `binance#ALT` engine beside the default one.
        strategy: Some(mount("alt", Some(alt()), "m-alt", &seen)),
        ..CoreConfig::default()
    };
    let _ = core_of(config, engine(CANON, 1_000.0), Vec::new());
}

/// …and the panic message carries what an operator needs to fix it: the mount id, the account, the
/// venue, the route key that was looked for, and the keys that would arm it. Asserted separately
/// from the `should_panic` above because that attribute matches ONE substring and the message's
/// whole job is to be actionable.
#[test]
fn the_unmounted_account_panic_says_what_to_do() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let m = mount("alt", Some(alt()), "m-alt", &seen);
    let primary = engine(CANON, 1_000.0);
    let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mount_engine_idx(&primary, &[], &m, "m-alt")
    }))
    .expect_err("a mount naming an unmounted account must not resolve an engine");
    let text =
        text.downcast_ref::<String>().cloned().unwrap_or_else(|| "<non-string panic>".to_string());
    assert!(text.contains("m-alt"), "names the MOUNT: {text}");
    assert!(text.contains("ALT"), "names the ACCOUNT: {text}");
    assert!(text.contains("binance"), "names the VENUE: {text}");
    assert!(text.contains("binance#ALT"), "names the ROUTE KEY it looked for: {text}");
    assert!(text.contains("policy.accounts.binance.ALT"), "names the line to write: {text}");
    assert!(
        text.contains("must NOT fall through"),
        "…and says why it refuses rather than defaulting: {text}"
    );
}

/// **A RUNTIME mount naming an unarmed account is REFUSED, not panicked** — the same rule at the
/// other door. A daemon must stay up when an operator sends a bad mount command; the note names the
/// venue, the account and the route key, and no mount slot is created.
#[test]
fn a_runtime_mount_naming_an_unmounted_account_is_refused_by_name() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount("default", None, "m-default", &seen)),
        ..CoreConfig::default()
    };
    let mut core = core_of(config, engine(CANON, 1_000.0), Vec::new());
    let before = core.mounts.len();

    core.mount_strategy_runtime(vike_exec::MountSpec {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        account: Some(alt()),
        controller_id: Some("rt-alt".into()),
        name: Some("buy_hold".into()),
        rhai: None,
        params: serde_json::json!({}),
    });

    assert_eq!(core.mounts.len(), before, "the refused mount must create NO slot");
    let noted = |needle: &str| core.recent.iter().any(|l| l.contains(needle));
    assert!(noted("MOUNT REFUSED"), "recent: {:?}", core.recent);
    assert!(noted("names account `ALT`"), "names the account: {:?}", core.recent);
    assert!(noted("binance#ALT"), "names the route key it looked for: {:?}", core.recent);
    assert!(
        noted("will NOT be mounted on that venue's default account"),
        "…and says what it is refusing to do instead: {:?}",
        core.recent
    );
}

// ---------------------------------------------------------------------------------------------
// THE ORDERS THE CORE MINTS ITSELF — the half the mount's own `EngineRoute::Mount` never covered.
//
// Everything above is about an order a STRATEGY asked for, which lowers through the one choke
// point `apply_strategy_intent` and carries its mount index. Three orders do not: a fired
// conditional (armed once, released later, off a bar), a margin-call liquidation (produced by a
// per-engine sweep) and a mount-budget flatten. Each was routed by the payload's VENUE string —
// the venue's DEFAULT account — while the book it was protecting belonged to another account.
//
// The failure they share is the worst-shaped one available: a protective order that acts on the
// wrong book does not fail, it SUCCEEDS at the wrong thing, leaving the exposure it was meant to
// close wide open and opening a second one nobody asked for. On the spread this file is about —
// long on one account, short on the other — the wrong book is the one holding the opposite side.
// ---------------------------------------------------------------------------------------------

/// A strategy that arms ONE protective stop on its first `on_feed_status` and does nothing else.
struct Stopper {
    price: f64,
}

impl Strategy<LiveBroker> for Stopper {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_stop(-1, 1.0, self.price);
    }
}

/// A mount that trades nothing at all — the DEFAULT-account mount in the tests below, present so
/// the assertion "the default account's client is empty" is about ROUTING rather than about there
/// being no strategy on that account.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

fn mount_of(
    strategy: Box<dyn Strategy<LiveBroker> + Send>,
    account: Option<AccountLabel>,
    controller_id: &str,
) -> StrategyMount {
    StrategyMount {
        account,
        symbols: Vec::new(),
        controller_id: Some(controller_id.to_string()),
        underlying_symbol: None,
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        strategy,
    }
}

/// A closed bar whose LOW takes out a sell stop at 90 — the trigger in the tests below.
fn crashing_bar() -> Bar {
    Bar {
        ts: 1,
        open: 95.0,
        high: 95.0,
        low: 89.0,
        close: 89.0,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn set_position(engine: &mut ExecutionEngine<RecordingClient>, size: f64, avg_px: f64) {
    engine.account.positions.insert(
        (CANON.into(), SYMBOL.into(), "BOTH".into()),
        vike_exec::PositionEntry { size, avg_px, ..Default::default() },
    );
}

fn stop_core(stop_px: f64) -> CoreThread<RecordingClient> {
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount_of(Box::new(Idle), None, "m-default")),
        extra_mounts: vec![mount_of(Box::new(Stopper { price: stop_px }), Some(alt()), "m-alt")],
        ..CoreConfig::default()
    };
    core_of(config, engine(CANON, 1_000.0), vec![(2_000.0, engine("binance#ALT", 2_000.0))])
}

/// ⚠ **A labelled mount's PROTECTIVE STOP fires into its own account.**
///
/// `conditional_books` is keyed by `(venue, symbol)` — an EXCHANGE fact — so both accounts of a
/// venue arm into ONE book and a `FiredConditional` names no account. The release used to lower
/// through `apply_intent`, i.e. by the payload's venue, which resolves the venue's DEFAULT engine:
/// `ALT`'s stop-loss sold into the default account's book. `ALT`'s position stayed open and
/// unprotected while a naked short opened beside it, with no error on either side.
/// `CoreThread::cond_engine` records the arm's engine at ARM time and the fire spends it.
#[test]
fn a_labelled_mounts_protective_stop_fires_into_its_own_account() {
    let mut core = stop_core(90.0);

    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    // The arm sits in the ONE book both accounts share, tagged to the engine that armed it —
    // which is the only place the account survives between the arm and the fire.
    assert_eq!(core.cond_engine.values().copied().collect::<Vec<usize>>(), vec![1]);

    // …and the stop is taken out.
    let bar = crashing_bar();
    core.fire_conditionals_bar(CANON, SYMBOL, &bar);

    assert_eq!(
        core.eng(1).client.submissions.len(),
        1,
        "the fired stop reaches the account that armed it"
    );
    assert!(
        core.eng(0).client.submissions.is_empty(),
        "…and NOT the venue's default account, which armed nothing and holds nothing"
    );
    assert!(
        core.cond_engine.is_empty(),
        "the arm left the book, so its account entry left with it (the map's bound)"
    );
}

/// The same lane on a SINGLE-account core, as a frozen baseline: the index recorded at the arm is
/// the answer the payload lookup already gave, so the fire is byte-identical where it always
/// worked.
#[test]
fn an_account_less_mounts_stop_fires_exactly_where_it_always_did() {
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount_of(Box::new(Stopper { price: 90.0 }), None, "m-default")),
        ..CoreConfig::default()
    };
    let mut core = core_of(config, engine(CANON, 1_000.0), Vec::new());

    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    assert_eq!(core.cond_engine.values().copied().collect::<Vec<usize>>(), vec![0]);

    let bar = crashing_bar();
    core.fire_conditionals_bar(CANON, SYMBOL, &bar);
    assert_eq!(core.eng(0).client.submissions.len(), 1);
}

/// A DISARM and a scoped mass-cancel take the arm's account entry with them — the bound on
/// `cond_engine`, asserted rather than promised in a comment.
#[test]
fn an_arm_that_leaves_its_book_takes_its_account_entry_with_it() {
    let mut core = stop_core(90.0);

    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    let arm_id = core.cond_engine.keys().next().expect("one arm").clone();
    core.apply_intent(OrderIntent::DisarmConditional { arm_id }, 1);
    assert!(core.cond_engine.is_empty(), "a disarmed arm leaves no entry behind");

    // …and again through the scoped mass-cancel, which empties a whole book at once.
    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    assert_eq!(core.cond_engine.len(), 1, "re-armed");
    core.apply_intent(
        OrderIntent::MassCancel { venue: Some(CANON.into()), symbol: Some(SYMBOL.into()) },
        2,
    );
    assert!(core.cond_engine.is_empty(), "clearing a book clears its arms' account entries");
}

/// ⚠ **A labelled account's MARGIN CALL liquidates its own book.**
///
/// The sweep is already per-engine — it reads `ALT`'s account, at `ALT`'s seed, through `ALT`'s
/// price board — but the reduce-only close it produced lowered through `apply_intent` and was
/// routed by the position's VENUE string, which resolves the default account. Against a flat
/// default account that is a reduce-only order with nothing to reduce (refused at the venue, `ALT`
/// never liquidated — protection that silently does nothing); against this file's spread it is a
/// close of the other leg of the operator's own position.
#[test]
fn a_labelled_accounts_margin_call_liquidates_its_own_book() {
    let cfg =
        vike_exec::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };
    let mut core = core_of(
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
        engine(CANON, 1_000.0),
        vec![(100.0, engine("binance#ALT", 100.0))],
    );

    // `ALT` is long 10 at 100 on a seed of 100 and the market is at 40: equity 100 − 600 = −500
    // against a maintenance requirement of 10·40·0.05 = 20. The DEFAULT account is flat.
    set_position(&mut core.extra_engines[0].1, 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote(CANON, SYMBOL, 40.0, 40.5, 1);

    core.sweep_margin_call(&cfg, 3);

    assert_eq!(
        core.eng(1).client.submissions.len(),
        1,
        "the liquidation reaches the engine whose account actually breached"
    );
    assert!(core.eng(1).client.submissions[0].reduce_only, "…as a reduce-only close");
    assert!(
        core.eng(0).client.submissions.is_empty(),
        "…and the healthy default account is never asked to close a position it does not hold"
    );
}

/// ⚠ **A labelled mount's BUDGET FLATTEN closes its own book.**
///
/// Everything else in `CoreThread::latch_mount` is mount-scoped — the attributed coids it cancels,
/// the attributed size it closes, the mount id it stamps on the journal record — but the flatten
/// itself was routed by the payload's venue, so a labelled mount's breach closed a position on the
/// default account and left its own untouched: the latch reporting success having bounded nothing.
#[test]
fn a_labelled_mounts_budget_flatten_closes_its_own_book() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let mut core = spread_core(&seen);

    // The `ALT` mount (index 1) is 100 down on an attributed long, against a budget of 10.
    core.mount_attr[1].size = 1.0;
    core.mount_attr[1].avg_px = 100.0;
    core.mount_attr[1].realized_pnl = -100.0;
    core.mount_budget[1] =
        Some(MountBudget { max_loss: Some(10.0), max_notional: None, flatten_on_breach: true });

    core.sweep_mount_budgets(1);

    assert!(core.mount_latched[1], "the breaching mount latched");
    assert_eq!(
        core.eng(1).client.submissions.len(),
        1,
        "the flatten reaches the account the latched mount actually trades"
    );
    assert!(core.eng(1).client.submissions[0].reduce_only);
    assert!(
        core.eng(0).client.submissions.is_empty(),
        "…and not the default account, whose book the latch has no business closing"
    );
}

/// **A second account's PAPER book is filled by the bar clock too.**
///
/// `ExecutionClient::on_bar` is the paper exchange's fill clock (a real adapter's default impl
/// ignores it), and it was delivered only to the engine the venue string resolves. A second
/// account that fell back to `vike_paper::PaperExecutionClient` at mount — credentials absent, the
/// live gate doing its job — therefore held a book nothing ever filled: its strategy's orders
/// rested forever, never filling and never terminalizing, with no error anywhere.
#[test]
fn every_account_of_a_venue_gets_the_bar_that_fills_its_paper_book() {
    let core = core_of(
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
        engine(CANON, 1_000.0),
        vec![(2_000.0, engine("binance#ALT", 2_000.0))],
    );
    assert_eq!(
        core.engines_of_venue(CANON),
        vec![0, 1],
        "both accounts of the exchange, default account first"
    );
    assert!(core.engines_of_venue("okx").is_empty(), "a venue this core runs no engine for");

    // …and the CALL SITE, which is the half a helper test cannot see: a real closed bar through
    // the real `dispatch` must reach BOTH accounts' clients. `RecordingClient::bars` exists for
    // this: `on_bar`'s trait default is a no-op, so a client that never received the bar is
    // otherwise indistinguishable from one that received it and had nothing to fill.
    let mut core = core;
    core.dispatch(Ingest::BarClose(Box::new(BarUpdate {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        bar: crashing_bar(),
    })));
    assert_eq!(core.eng(0).client.bars.len(), 1, "the default account's book is clocked");
    assert_eq!(
        core.eng(1).client.bars.len(),
        1,
        "…and so is the second account's, which is the whole finding"
    );

    // …and on a single-account core it is exactly the one engine the venue lookup answered with,
    // which is what makes the loop at the `on_bar` site byte-identical there.
    let solo = core_of(
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
        engine(CANON, 1_000.0),
        Vec::new(),
    );
    assert_eq!(solo.engines_of_venue(CANON), vec![0]);
}

/// **THE ACCOUNT-AGGREGATE EXPOSURE CEILING IS SPENT PER ACCOUNT, THROUGH THE REAL ROUTING** —
/// `vike_exec::RiskLimits::max_account_exposure`, driven where the account-to-engine resolution
/// actually happens.
///
/// The exec-crate suite (`crates/vike-exec/tests/risk/risk_lane_completion.rs`'s
/// `a_labelled_second_account_of_one_venue_has_its_own_budget`) can only show that two ENGINES
/// carry two budgets, because the fold reads one engine's own book and nothing above it. The
/// property an operator actually depends on is one layer up and lives here: the order a mount mints
/// is judged against the ceiling of the account THAT MOUNT NAMES. A misroute — the `(venue, symbol)`
/// first-match this module exists to have deleted — would spend the wrong account's budget, and
/// every assertion in the exec suite would still pass.
///
/// The DEFAULT account is loaded past its ceiling in a symbol NEITHER mount trades, so the refusal
/// can only come from the account aggregate: the per-symbol lane sees a flat `BTCUSDT` book on both
/// engines and both orders are the identical size. `ALT` holds nothing, so its order must be
/// ADMITTED — the arm that stops this passing against a ceiling armed at zero, or against a lane
/// that refuses everything once it is set.
#[test]
fn each_account_spends_its_own_share_of_the_account_exposure_ceiling() {
    /// Small enough that the loaded account is over it and the clean one is not, at the size the
    /// `Prober` mount submits (1 @ 100).
    const CEILING: f64 = 150.0;
    /// A symbol NEITHER mount trades, so what it loads is account exposure and nothing else.
    const OTHER: &str = "ETHUSDT";

    /// ⚠ **`Account::new`'s first argument is the contract MULTIPLIER, not cash** — the trap
    /// `crates/vike-core/src/runtime/apply.rs`'s combo tests also record. [`engine`] above passes
    /// the seed there, which is harmless for a test that judges no notional; every lane this
    /// ceiling touches multiplies by it, so a `1_000.0` multiplier would make a 1-lot order
    /// 100 000 of exposure and no sane ceiling could tell the two accounts apart. Hence `1.0`.
    fn engine_capped(route_key: &str) -> ExecutionEngine<RecordingClient> {
        let mut e = ExecutionEngine::new(
            Account::new(1.0, CANON, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits { max_account_exposure: Some(CEILING), ..RiskLimits::new() }),
            RecordingClient::default(),
            CANON,
            SYMBOL,
        );
        e.route_key = route_key.to_string();
        e.collect_applied_fills = true;
        e
    }

    let seen = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount("default", None, "m-default", &seen)),
        extra_mounts: vec![mount("alt", Some(alt()), "m-alt", &seen)],
        ..CoreConfig::default()
    };
    let mut core =
        core_of(config, engine_capped(CANON), vec![(2_000.0, engine_capped("binance#ALT"))]);
    assert_eq!(core.mount_engine, vec![0, 1], "precondition: one mount per account");

    // Load the DEFAULT account past its ceiling: 2 × 100 = 200 against 150. Priced on BOTH stores,
    // so no resolver arm can move the number under test.
    {
        let e = core.eng_mut(0);
        e.account.positions.insert(
            (CANON.into(), OTHER.into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        e.account.set_mark_from(CANON, OTHER, 100.0, vike_exec::MarkSource::VenueMark, 0);
        e.price_board.set_mark(CANON, OTHER, 100.0, 1);
    }

    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);

    let both_ran = seen.lock().unwrap().len();
    assert_eq!(both_ran, 2, "precondition: both mounts were driven, or neither result means much");
    assert!(
        core.eng(0).client.submissions.is_empty(),
        "the loaded account is over its ceiling and must refuse its own mount's order: {:?}",
        core.eng(0).client.submissions
    );
    assert_eq!(
        core.eng(1).client.submissions.len(),
        1,
        "…and `ALT`'s identical order must still reach the venue: one account's exposure may not \
         spend another account's ceiling, and a misroute here would show up as this order being \
         judged against the loaded book"
    );
}
