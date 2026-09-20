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

/// The UNLABELLED account, NAMED. ⚠ Not the same thing as `None`, and the difference is the whole
/// of this file's two-engine fixtures: an ABSENT account is the mount naming none, which a
/// two-engine venue refuses as ambiguous; this is the mount naming the account the venue already
/// had, which routes to it at any engine count. `AccountLabel::parse` cannot produce it — `DEFAULT`
/// is reserved there precisely so an operator cannot spell it as a label — so it is the variant.
fn default_acct() -> AccountLabel {
    AccountLabel::Default
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
        strategy: Some(mount("default", Some(default_acct()), "m-default", seen)),
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
        "the mount naming `DEFAULT` resolves the unlabelled engine, the `ALT` mount resolves its own"
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
#[should_panic(expected = "names account `ALT` of venue `binance`")]
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
        noted("must NOT fall through to that venue's default account"),
        "…and says what it is refusing to do instead: {:?}",
        core.recent
    );
}

/// **A mount naming a VENUE this core runs no engine for is refused too, at BOTH doors** — the
/// Phase-0 repair `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md` asks
/// for, and the half that used to resolve ENGINE ZERO in silence.
///
/// The account arm has always refused; this arm returned `unwrap_or(0)`, defended as a historical
/// tolerance for paper/test cores. On a live daemon engine zero is whichever venue was mounted
/// first, so the tolerance meant a strategy whose profile named another venue placed real orders
/// on a book its author never chose — the account arm's catastrophe wearing a venue instead of a
/// label.
///
/// ⚠ **Both doors in ONE test, because the finding was that they DISAGREED.** Asserting only the
/// spawn panic would have passed for the whole period the runtime door was already refusing, and
/// vice versa.
///
/// ⚠ **Mutation proof (production code, not the harness):** put `.unwrap_or(0)` back on
/// `mount_engine_resolution`'s `None` arm — i.e. make the miss resolve engine zero again — and
/// the spawn half stops panicking and the runtime half mounts a slot. Both halves go red, each
/// for its stated reason.
#[test]
fn a_mount_naming_an_unmounted_venue_is_refused_at_both_doors() {
    // SPAWN: a mount on a venue with no engine must panic rather than resolve engine 0.
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let m = mount("default", None, "m-elsewhere", &seen);
    let mut foreign = m;
    foreign.venue = "okx".to_string();
    let primary = engine(CANON, 1_000.0);
    let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mount_engine_idx(&primary, &[], &foreign, "m-elsewhere")
    }))
    .expect_err("a mount on a venue this core runs no engine for must not resolve an engine");
    let text =
        text.downcast_ref::<String>().cloned().unwrap_or_else(|| "<non-string panic>".to_string());
    assert!(text.contains("m-elsewhere"), "names the MOUNT: {text}");
    assert!(text.contains("okx"), "names the VENUE it could not find: {text}");
    assert!(text.contains("binance"), "…and the engines that DO exist: {text}");
    assert!(
        text.contains("ENGINE ZERO"),
        "…and says what it is refusing to do instead, which is the whole change: {text}"
    );

    // RUNTIME: the same miss is a NOTE and no slot, because a live daemon must stay up.
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount("default", None, "m-default", &seen)),
        ..CoreConfig::default()
    };
    let mut core = core_of(config, engine(CANON, 1_000.0), Vec::new());
    let before = core.mounts.len();
    core.mount_strategy_runtime(vike_exec::MountSpec {
        venue: "okx".into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        account: None,
        controller_id: Some("rt-okx".into()),
        name: Some("buy_hold".into()),
        rhai: None,
        params: serde_json::json!({}),
    });
    assert_eq!(core.mounts.len(), before, "the refused mount must create NO slot");
    let noted = |needle: &str| core.recent.iter().any(|l| l.contains(needle));
    assert!(noted("MOUNT REFUSED"), "recent: {:?}", core.recent);
    assert!(noted("names venue `okx`"), "names the venue: {:?}", core.recent);
    assert!(
        noted("ENGINE ZERO"),
        "…the SAME sentence the spawn door panics with — one resolution, one wording: {:?}",
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
        strategy: Some(mount_of(Box::new(Idle), Some(default_acct()), "m-default")),
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
        strategy: Some(mount("default", Some(default_acct()), "m-default", &seen)),
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

// ---------------------------------------------------------------------------------------------
// THE PANIC BUTTON — the operator's own verb, and the one that names an EXCHANGE rather than a
// book.
//
// Everything above is about an order that belongs to ONE account and had to learn to say so.
// `OrderIntent::MarketExit { venue: Some(v) }` is the opposite shape: the operator can name no
// account (a DOM click, a tradehub ticket, a CLI verb carries a venue string and nothing else),
// and naming the exchange IS naming every book on it. The verb lowered both of its legs as if the
// venue named one account instead.
// ---------------------------------------------------------------------------------------------

/// ⚠ **THE PANIC BUTTON REACHES THE BOOK IT READ** — a venue-scoped `MarketExit` on TWO ACCOUNTS OF
/// ONE EXCHANGE flattens each account's own position and cancels each account's own orders.
///
/// # The two legs, and both were mis-routed
///
/// * **FLATTEN.** `CoreThread::market_exit_flatten_legs` walks engine by engine and reads each
///   position out of a SPECIFIC engine — and then emitted an `OrderIntent::Flatten { venue,
///   symbol }`, which carries no account, so the index it had in hand died on the line that built
///   the leg. Downstream `vike_exec::RouteKey::sole_account_of` resolves a venue string to that
///   venue's DEFAULT account, so on this core the two positions produced BYTE-IDENTICAL intents
///   and both landed on engine 0: `Flatten` re-resolves the size at apply time, so the default
///   account was sold twice — the second leg REVERSING the position the first had just closed —
///   while `ALT` was never flattened at all.
/// * **MASS-CANCEL.** The exit's own cancel leg lowers as `MassCancel { venue: Some(v) }`, and
///   that arm resolved ONE engine through `CoreThread::route_of`, whose fallback is the same
///   default account. So the venue-scoped exit flattened BOTH books (badly) and cancelled NOTHING
///   on one of them, leaving `ALT`'s resting order live and free to fill straight after the
///   flatten and put the operator back in — from the button they pressed to get out.
///
/// # ⚠ How this differs from the test whose NAME says it already covers this
///
/// `apply.rs`'s `market_exit_walks_every_engine_and_routes_each_flatten_to_its_own` mounts `"sim"`
/// and `"bin"` — TWO EXCHANGES. Each leg's own venue STRING names its engine there, so the payload
/// route resolves it correctly with no index at all: that test proves the cross-VENUE walk and
/// could not have failed on this one. The distinction is the whole reason this file exists — both
/// engines here carry the canonical venue [`CANON`] and differ only in `route_key`, and both hold
/// the same [`SYMBOL`], so the two legs are indistinguishable as strings and only the engine index
/// can separate them.
///
/// The two positions are deliberately OPPOSITE and of DIFFERENT sizes: a leg that lands on the
/// wrong book is then a position INCREASE of the wrong quantity rather than a benign no-op, and
/// each assertion below names a side and a size that only its own account's book can produce.
#[test]
fn a_venue_scoped_market_exit_reaches_each_account_of_the_exchange() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let mut core = spread_core(&seen);
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");
    assert_eq!(core.engines_of_venue(CANON), vec![0, 1], "…and the exchange must own both of them");

    // ONE RESTING ORDER PER ACCOUNT, each minted by that account's own mount through the real
    // choke point — so "each account's own orders" is a fact about the core, not about the harness.
    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    let resting_default = core.eng(0).client.submissions[0].client_order_id.clone();
    let resting_alt = core.eng(1).client.submissions[0].client_order_id.clone();
    assert_ne!(resting_default, resting_alt, "two orders, or the cancel assertions say nothing");

    // …and ONE POSITION PER ACCOUNT, opposite sides and different sizes.
    set_position(&mut core.engine, 2.0, 100.0);
    set_position(&mut core.extra_engines[0].1, -3.0, 100.0);

    // THE BUTTON: an external command, which can name no account.
    core.apply_intent(OrderIntent::MarketExit { venue: Some(CANON.into()) }, 2);

    // ── the CANCEL leg: each account's own book, and only its own.
    assert_eq!(
        core.eng(0).client.cancels,
        vec![resting_default],
        "the default account's resting order is cancelled"
    );
    assert_eq!(
        core.eng(1).client.cancels,
        vec![resting_alt],
        "…and so is `ALT`'s, which the venue's default-account routing left resting behind a \
         flatten — free to fill and put the operator straight back in"
    );

    // ── the FLATTEN legs: `(side, qty)` per reduce-only close, per account. The resting limits are
    // filtered out, so what is left is exactly what this verb minted.
    assert_eq!(
        closes(core.eng(0)),
        vec![(-1, 2.0)],
        "the default account is closed ONCE, for its OWN long — two entries here is the defect: \
         the second leg re-resolves the same size and reverses the position"
    );
    assert_eq!(
        closes(core.eng(1)),
        vec![(1, 3.0)],
        "…and `ALT`'s own short is bought back on `ALT`'s book, at `ALT`'s size"
    );
}

/// The reduce-only closes one engine's client was handed, as `(side, qty)` — the resting limits
/// filtered out, so what is left is exactly what a market exit minted there.
fn closes(e: &ExecutionEngine<RecordingClient>) -> Vec<(i32, f64)> {
    e.client.submissions.iter().filter(|o| o.reduce_only).map(|o| (o.side, o.qty)).collect()
}

/// The same verb UNSCOPED, on the same core: `MarketExit { venue: None }` still reaches every
/// engine, which is the property that must SURVIVE the fix rather than be traded for it.
///
/// The panic button may never require an argument — naming the whole set IS naming the target — so
/// this is the guard against "fixing" the venue-scoped case by making the scope mandatory.
#[test]
fn an_unscoped_market_exit_still_reaches_every_engine() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let mut core = spread_core(&seen);

    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    set_position(&mut core.engine, 2.0, 100.0);
    set_position(&mut core.extra_engines[0].1, -3.0, 100.0);

    core.apply_intent(OrderIntent::MarketExit { venue: None }, 2);

    assert_eq!(core.eng(0).client.cancels.len(), 1, "the default account's order is cancelled");
    assert_eq!(core.eng(1).client.cancels.len(), 1, "…and so is `ALT`'s");
    assert_eq!(closes(core.eng(0)), vec![(-1, 2.0)], "the default account is flattened once");
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "…and `ALT` once, on its own side and size");
}

/// **A venue-scoped exit a LABELLED MOUNT issued stays on that mount's own book** — the other half
/// of the scope, and the thing a blanket "always fan over every account" would have broken.
///
/// An operator's exit names an exchange and means every account of it. A STRATEGY's does not: the
/// mount named its account at assemble, so `EngineRoute::Mount` carries it, and closing the
/// operator's other book because a strategy asked to get out of its own would be the mirror-image
/// defect. `CoreThread::exit_scope_engines` keeps the one-engine answer wherever the caller
/// actually named a book.
#[test]
fn a_labelled_mounts_own_market_exit_does_not_reach_the_other_account() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let mut core = spread_core(&seen);

    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    set_position(&mut core.engine, 2.0, 100.0);
    set_position(&mut core.extra_engines[0].1, -3.0, 100.0);

    // Mount 1 is the `ALT` mount (`spread_core`'s `extra_mounts`), so this is `ALT` asking to get
    // out of `binance` — its own `binance`.
    core.apply_intent_routed(
        OrderIntent::MarketExit { venue: Some(CANON.into()) },
        2,
        CancelIntent::Unspecified,
        EngineRoute::Mount(1),
    );

    assert!(
        core.eng(0).client.cancels.is_empty(),
        "the default account's book is not the strategy's to cancel"
    );
    assert_eq!(
        closes(core.eng(0)),
        Vec::<(i32, f64)>::new(),
        "…nor its position the strategy's to close"
    );
    assert_eq!(core.eng(1).client.cancels.len(), 1, "`ALT` cancels its own");
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "…and flattens its own short, at its own size");
}

// ---------------------------------------------------------------------------------------------
// STAGE 1 — THE NODE REFUSES ON AMBIGUITY
// (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md`)
// ---------------------------------------------------------------------------------------------
//
// §4.2's table, ROW 1: an EXTERNAL command (a DOM click, a tradehub ticket, a CLI verb — every one
// of them `EngineRoute::Payload`, none of them able to name an account) that names a venue this
// process runs SEVERAL accounts of is REFUSED rather than routed to that venue's default book.
//
// §4.5's law decides WHICH verbs:
//
//   > A risk-REDUCING venue verb that names no account fans out to EVERY account of that venue. A
//   > risk-INCREASING one refuses.
//
// Both halves are gated below, on one harness, because the halves are only meaningful against each
// other: a build that refused the reducing verbs would take out the panic button (decision 0041's
// verdict 4), and one that fanned out the increasing verbs would place N orders in N books the
// sender never named — strictly worse than the single misroute Stage 1 exists to close.

/// A two-account core with NO strategy mount at all — the shape an operator ticket arrives at. The
/// mounts in [`spread_core`] would let a test accidentally route by `EngineRoute::Mount` and prove
/// nothing about the external path.
fn ticket_core() -> CoreThread<RecordingClient> {
    core_of(
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
        engine(CANON, 1_000.0),
        vec![(2_000.0, engine("binance#ALT", 2_000.0))],
    )
}

/// …and its SINGLE-account twin, identical in every way the assertions below can see.
fn single_account_core() -> CoreThread<RecordingClient> {
    core_of(
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
        engine(CANON, 1_000.0),
        Vec::new(),
    )
}

fn open_order(venue: &str) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        client_order_id: String::new(),
        venue: venue.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    }
}

/// Every order that reached a venue client, across every engine of the core.
fn submitted_anywhere(core: &CoreThread<RecordingClient>) -> usize {
    (0..=core.extra_engines.len()).map(|i| core.eng(i).client.submissions.len()).sum()
}

fn refusal_note(core: &CoreThread<RecordingClient>) -> Option<String> {
    core.recent.iter().find(|l| l.contains("REFUSED")).map(|l| l.to_string())
}

/// ⚠ **THE HEADLINE REFUSAL.** An account-less submit naming a venue with two accounts reaches
/// NEITHER book, mints no coid, and says which two books it could have meant.
#[test]
fn an_account_less_submit_to_a_two_account_venue_is_refused_by_name() {
    let mut core = ticket_core();
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order(CANON))), 1);

    assert!(coids.is_empty(), "nothing was minted: {coids:?}");
    assert_eq!(submitted_anywhere(&core), 0, "and nothing reached EITHER venue client");

    let note = refusal_note(&core).expect("the recent-events ring must say so");
    assert!(note.contains("binance"), "names the venue: {note}");
    assert!(note.contains("binance#ALT"), "names the OTHER candidate: {note}");
    assert!(note.contains("2 accounts"), "…and how many there are: {note}");
    assert!(note.contains("Nothing was sent"), "{note}");
}

/// **A SINGLE-account node is unchanged** — the claim that makes Stage 1 safe to ship. The same
/// intent, the same clock, the same engine: a minted coid and one submission, and nothing refused.
#[test]
fn a_single_account_node_routes_the_same_submit_exactly_as_before() {
    let mut core = single_account_core();
    assert!(!core.multi_account, "one account: every Stage-1 branch stays inert");
    assert_eq!(core.ambiguous_accounts(EngineRoute::Payload, CANON), None);

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order(CANON))), 1);
    assert_eq!(coids.len(), 1, "one coid minted, as always");
    assert_eq!(core.eng(0).client.submissions.len(), 1, "…and one order at the venue");
    assert_eq!(refusal_note(&core), None, "nothing was refused");
}

/// …and an UNKNOWN venue keeps `unwrap_or(0)`'s historical answer rather than becoming §4.2's
/// `N = 0` refusal. Declared rather than defaulted: refusing there would change SINGLE-account
/// behaviour (every paper/sim engine behind a non-roster id), which is the one thing this stage
/// may not do — see `CoreThread::ambiguous_accounts`' own doc.
#[test]
fn an_unknown_venue_still_routes_to_engine_zero() {
    let mut core = single_account_core();
    assert_eq!(core.ambiguous_accounts(EngineRoute::Payload, "no-such-venue"), None);
    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order("no-such-venue"))), 1);
    assert_eq!(coids.len(), 1, "minted, not refused");
    assert_eq!(core.eng(0).client.submissions.len(), 1);
}

/// **The ambiguity is per VENUE, never per NODE** — §4.2: *"a box running forty-nine binance
/// accounts and one okx account keeps serving every account-less okx command unchanged, and refuses
/// only the binance ones."*
#[test]
fn a_single_account_venue_on_a_multi_account_node_is_untouched() {
    let okx = {
        let mut e = ExecutionEngine::new(
            Account::new(3_000.0, "okx", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "okx",
            SYMBOL,
        );
        e.route_key = "okx".to_string();
        e
    };
    let mut core = core_of(
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
        engine(CANON, 1_000.0),
        vec![(2_000.0, engine("binance#ALT", 2_000.0)), (3_000.0, okx)],
    );
    assert!(core.multi_account);
    assert!(core.ambiguous_accounts(EngineRoute::Payload, CANON).is_some(), "binance IS ambiguous");
    assert_eq!(core.ambiguous_accounts(EngineRoute::Payload, "okx"), None, "okx is NOT");

    let coids = core.apply_intent(OrderIntent::Submit(Box::new(open_order("okx"))), 1);
    assert_eq!(coids.len(), 1, "the okx ticket is served exactly as before");
    assert_eq!(core.eng(2).client.submissions.len(), 1);
    assert_eq!(core.eng(0).client.submissions.len(), 0, "…and neither binance book was touched");
    assert_eq!(core.eng(1).client.submissions.len(), 0);
}

/// **§4.2 row 1 reaches the BATCH verb, through a fast path that would otherwise have skipped it.**
///
/// `SubmitBatch`'s `all_primary` test asks whether every leg resolves to engine 0. An account-less
/// leg on a two-account venue SATISFIES that question — `route_of` answers with that venue's
/// DEFAULT engine, which IS engine 0 here — so without the ambiguity clause beside it the whole
/// batch took the single-call path onto engine 0 and never re-entered the arm where the refusal
/// lives. The guard is therefore load-bearing rather than defensive, and this is what pins it.
///
/// The batch is deliberately MIXED, because that is §4.2's per-VENUE scoping applied INSIDE one
/// command: the ambiguous leg is refused BY NAME while the okx leg is submitted exactly as before.
#[test]
fn an_ambiguous_batch_leg_is_refused_while_its_unambiguous_sibling_is_submitted() {
    let okx = {
        let mut e = ExecutionEngine::new(
            Account::new(3_000.0, "okx", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "okx",
            SYMBOL,
        );
        e.route_key = "okx".to_string();
        e
    };
    let mut core = core_of(
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
        engine(CANON, 1_000.0),
        vec![(2_000.0, engine("binance#ALT", 2_000.0)), (3_000.0, okx)],
    );

    let coids =
        core.apply_intent(OrderIntent::SubmitBatch(vec![open_order(CANON), open_order("okx")]), 1);

    assert_eq!(coids.len(), 1, "only the okx leg minted a coid: {coids:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 0, "the default binance book is untouched");
    assert_eq!(core.eng(1).client.submissions.len(), 0, "…and so is `ALT`");
    assert_eq!(core.eng(2).client.submissions.len(), 1, "the okx leg is served exactly as before");
    let note = refusal_note(&core).expect("the ring must say the binance leg was refused");
    assert!(note.contains("binance#ALT"), "…and name both candidates: {note}");
}

/// A BRACKET and a CONDITIONAL ARM are the same risk-INCREASING side and refuse the same way —
/// asserted together because the failure mode is "somebody guarded one arm and not its neighbour".
#[test]
fn every_risk_increasing_arm_refuses_and_mints_nothing() {
    let bracket = OrderIntent::Bracket(Box::new(vike_model::BracketSpec {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        take_profit: 110.0,
    }));
    let arm = OrderIntent::ArmConditional(ConditionalIntent {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        side: -1,
        qty: 1.0,
        price: Some(90.0),
        trail: None,
        trigger_by: None,
    });
    // ⚠ A COMBO's refusal is asserted with a spec that would otherwise be REJECTED anyway
    // (`binance` carries no `supports_combo` row), which is precisely what makes it worth
    // asserting: the ambiguity guard sits AHEAD of both `validate` and the capability arm, so a
    // combo that names no account refuses for the RIGHT reason and mints nothing rather than
    // reaching the synthesized-terminal path on a guessed engine.
    let combo = OrderIntent::Combo(Box::new(vike_model::ComboSpec {
        venue: CANON.into(),
        side: 1,
        qty: 1.0,
        legs: vec![
            vike_model::ComboLeg { symbol: SYMBOL.into(), ratio: 1 },
            vike_model::ComboLeg { symbol: "ETHUSDT".into(), ratio: -1 },
        ],
        net_limit: Some(1.0),
        time_in_force: vike_model::TimeInForce::default(),
    }));
    for (label, intent) in [("bracket", bracket), ("ArmConditional", arm), ("combo", combo)] {
        let mut core = ticket_core();
        let coids = core.apply_intent(intent, 1);
        assert!(coids.is_empty(), "{label}: nothing minted, got {coids:?}");
        assert_eq!(submitted_anywhere(&core), 0, "{label}: nothing reached a venue client");
        let note = refusal_note(&core)
            .unwrap_or_else(|| panic!("{label}: the ring must say so: {:?}", core.recent));
        assert!(note.contains("binance#ALT"), "{label} names both candidates: {note}");
    }
}

/// ⚠ **THE REDUCING SIDE, and the one that would have made this a trading halt.** §4.5:
///
///   > A risk-REDUCING venue verb that names no account fans out to EVERY account of that venue.
///
/// A standalone `Flatten` names a venue and a symbol and mints a `reduce_only` MARKET — it can only
/// CLOSE. It used to resolve ONE engine (`route_of(..).unwrap_or(0)`, the venue's default account),
/// so on this core "flatten my binance BTC" closed one book and left the other open — while
/// `MarketExit` on the same venue, which is this verb plus a mass-cancel, already fanned out.
#[test]
fn an_account_less_flatten_reaches_every_account_of_the_venue() {
    let mut core = ticket_core();
    set_position(&mut core.engine, 2.0, 100.0);
    set_position(&mut core.extra_engines[0].1, -3.0, 100.0);

    let coids =
        core.apply_intent(OrderIntent::Flatten { venue: CANON.into(), symbol: SYMBOL.into() }, 2);

    assert_eq!(coids.len(), 2, "one leg per account: {coids:?}");
    assert_eq!(closes(core.eng(0)), vec![(-1, 2.0)], "the default account is flattened once");
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "…and `ALT` once, on ITS OWN side and size");
    assert_eq!(refusal_note(&core), None, "a reducing verb is never refused for ambiguity");
}

/// …and the same verb on a SINGLE-account core is what it always was: one leg, on the one engine,
/// closing exactly the position it read.
#[test]
fn a_single_account_flatten_is_the_one_leg_it_always_was() {
    let mut core = single_account_core();
    set_position(&mut core.engine, 2.0, 100.0);

    let coids =
        core.apply_intent(OrderIntent::Flatten { venue: CANON.into(), symbol: SYMBOL.into() }, 2);

    assert_eq!(coids.len(), 1);
    assert_eq!(closes(core.eng(0)), vec![(-1, 2.0)]);
}

/// A venue-scoped MARKET-EXIT is the other reducing verb, and it must stay fanned out rather than
/// be swept into the refusal — the guard against a future "refuse whenever ambiguous" applied
/// uniformly, which is decision 0041's verdict 4 by name.
#[test]
fn the_panic_button_is_not_refused() {
    let mut core = ticket_core();
    set_position(&mut core.engine, 2.0, 100.0);
    set_position(&mut core.extra_engines[0].1, -3.0, 100.0);

    core.apply_intent(OrderIntent::MarketExit { venue: Some(CANON.into()) }, 2);

    assert_eq!(closes(core.eng(0)), vec![(-1, 2.0)], "the default account is flattened");
    assert_eq!(closes(core.eng(1)), vec![(1, 3.0)], "…and so is `ALT`");
    assert_eq!(refusal_note(&core), None, "the panic button may never be refused for ambiguity");
}

/// **§9 item 2 — the FOREIGN-VENUE FALLTHROUGH.** A labelled mount's cross-venue leg defers to the
/// payload (correctly: *"the mount's account is a fact about its OWN venue"*), and that deference
/// used to land on the foreign venue's DEFAULT account. The guard covers it by construction —
/// `routed_engine` answering `None` IS the fallthrough — so the mount's OWN venue stays routable
/// while the external path on the same venue refuses.
#[test]
fn a_mount_route_that_names_its_account_is_never_ambiguous() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let core = spread_core(&seen);
    assert_eq!(core.route_of(EngineRoute::Mount(1), "okx"), None, "the deference is unchanged");
    assert_eq!(
        core.ambiguous_accounts(EngineRoute::Mount(1), CANON),
        None,
        "the mount named its account, so its own venue is not ambiguous for it"
    );
    assert!(
        core.ambiguous_accounts(EngineRoute::Payload, CANON).is_some(),
        "…while the external path, which names nothing, IS"
    );
}

/// **The refusal names the accounts in ENGINE order**, so the first one is the account the command
/// used to reach — which is what an operator needs in order to tell whether they were relying on
/// the old behaviour.
#[test]
fn the_refusal_lists_the_default_account_first() {
    let core = ticket_core();
    let candidates =
        core.ambiguous_accounts(EngineRoute::Payload, CANON).expect("two accounts is ambiguous");
    assert_eq!(candidates, vec!["binance".to_string(), "binance#ALT".to_string()]);
}

/// `accounts_of_venue` is `engines_of_venue(..).len()`, pinned — the allocation-free twin exists
/// only because `route_event`'s last rung asks the question on the fold, and a second answer would
/// be a second truth.
#[test]
fn the_account_count_is_the_engine_set_it_claims_to_be() {
    let core = ticket_core();
    for venue in [CANON, "binance#ALT", "okx", "no-such-venue"] {
        assert_eq!(core.accounts_of_venue(venue), core.engines_of_venue(venue).len(), "{venue}");
    }
    let single = single_account_core();
    for venue in [CANON, "okx"] {
        assert_eq!(
            single.accounts_of_venue(venue),
            single.engines_of_venue(venue).len(),
            "{venue}"
        );
    }
}

/// **The journal records the RESOLVED account** (§9 item 12) — and records NOTHING for a default
/// account, so a single-account box's journal bytes do not move.
#[test]
fn the_write_ahead_route_key_is_absent_for_a_default_account() {
    let core = single_account_core();
    assert_eq!(core.journal_route_key(0, CANON), None, "the venue's sole account names itself");

    let two = ticket_core();
    assert_eq!(two.journal_route_key(0, CANON), None, "…and so does the DEFAULT account of two");
    assert_eq!(
        two.journal_route_key(1, CANON),
        Some("binance#ALT".to_string()),
        "a labelled account is what the record has to carry"
    );
}

/// **AN ACCOUNT-LESS MOUNT REFUSES A VENUE THIS CORE RUNS TWICE** — the hole `sole_account_of`
/// names but cannot close on its own.
///
/// That spelling ASSERTS *"this venue has one account in this process"*. On a box with an
/// `[accounts]` table the assertion is false, and the lookup nonetheless SUCCEEDS — the default
/// account's route key IS the bare venue — so the mount lands on the default account and looks
/// correct. A strategy executing on an account its author did not choose is the account arm's
/// catastrophe wearing an ABSENCE instead of a label.
///
/// ⚠ **Mutation proof (production code, not the harness):** delete the `carriers > 1` block from
/// `mount_engine_resolution`'s `None` arm and this test goes green again while the mount silently
/// resolves engine `binance` — which is exactly the state before the change.
#[test]
fn an_account_less_mount_is_refused_when_the_venue_has_two_engines() {
    let mounted = ["binance", "binance#ALT", "okx"];
    let reason = mount_engine_resolution(&mounted, "binance", None)
        .expect_err("two engines of one venue cannot be addressed by the venue alone");
    assert!(reason.contains("2 engines"), "the refusal must COUNT them: {reason}");
    assert!(reason.contains("binance#ALT"), "…and name the spellings it answers to: {reason}");
    assert!(
        reason.contains("policy.accounts"),
        "…and say where an account is armed, which is what the operator does next: {reason}"
    );
}

/// ⚠ **The complement, and it is what keeps the refusal off every single-account box** — which is
/// every box with no `[accounts]` table, i.e. nearly all of them. Without this the test above is
/// satisfied by a `None` arm that refuses unconditionally.
#[test]
fn an_account_less_mount_still_resolves_a_venue_with_one_engine() {
    let mounted = ["binance", "okx"];
    assert_eq!(
        mount_engine_resolution(&mounted, "binance", None).expect("one engine is unambiguous"),
        0
    );
    assert_eq!(
        mount_engine_resolution(&mounted, "okx", None).expect("one engine is unambiguous"),
        1
    );
}

/// Naming the ACCOUNT resolves what the venue alone could not — the refusal above tells the
/// operator to do this, so it has to work.
#[test]
fn naming_the_account_resolves_a_venue_with_two_engines() {
    let mounted = ["binance", "binance#ALT"];
    let alt = vike_model::account_keys::AccountLabel::parse("ALT").expect("a valid label");
    assert_eq!(
        mount_engine_resolution(&mounted, "binance", Some(&alt)).expect("the label addresses it"),
        1
    );
}

/// ⚠ **NAMING `DEFAULT` IS NOT THE SAME AS NAMING NOTHING, AND ON A TWO-ENGINE VENUE IT ROUTES.**
///
/// The design's sender table keeps these on separate rows — an ABSENT account is the sender naming
/// none, ambiguous at `N ≥ 2`; `DEFAULT` is the sender naming the unlabelled account the venue
/// already had, which is ambiguous at no engine count at all. This test exists because collapsing
/// the two is the natural way to write the refusal above (`account.filter(|l| !l.is_default())` is
/// already how `DEFAULT` reaches that arm to share its resolution) and it was how this change was
/// FIRST written — at which point the default account had no spelling that mounted it here: absence
/// refused, and `DEFAULT` folded into the same refusal, so the message told the operator to name an
/// account while refusing the name for the one they wanted. The eleven two-engine fixtures above
/// went red together and that is what they were saying.
#[test]
fn naming_default_resolves_the_unlabelled_engine_of_a_two_engine_venue() {
    let mounted = ["binance", "binance#ALT"];
    let default = vike_model::account_keys::AccountLabel::Default;
    assert_eq!(
        mount_engine_resolution(&mounted, "binance", Some(&default))
            .expect("`DEFAULT` names the unlabelled account and is never ambiguous"),
        0,
        "the unlabelled account's route key IS the bare venue, so it is engine zero here"
    );
    // …and the refusal is still armed for the row that IS ambiguous, on the same mounted set — so
    // this test cannot be passed by a `None` arm that simply stopped refusing.
    assert!(
        mount_engine_resolution(&mounted, "binance", None).is_err(),
        "absence must still refuse: it is a different row, not a different spelling of this one"
    );
}
