//! A mount names its account: its orders and reads route by ACCOUNT, never by first match.

use super::*;
use vike_marketdata::QuoteTick;

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
    let mut core = core_of(engine(CANON, 1_000.0), Vec::new(), config);

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

/// **A mount naming an account this core runs no engine for is NOT MOUNTED, and the core still
/// comes up** — the backstop `resolve_mount_engines` exists to be.
///
/// It used to PANIC, and under systemd a panic at assembly is a crash loop that re-arms every live
/// account every few seconds for a fault in ONE mount. The asymmetry that stays is the refusal
/// itself: `unwrap_or(0)` here would be the catastrophe stated as three characters — a strategy
/// whose operator named `ALT` trading the DEFAULT account, silently — so the bad mount is
/// tombstoned (never routed to engine zero), `error!`-logged and noted, and the GOOD mount beside
/// it keeps its slot and its engine. The composition root
/// (`crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`) refuses
/// this case first, with a message an operator can act on; this is what no future root can reach
/// past.
#[test]
fn a_mount_naming_an_unmounted_account_is_not_mounted_and_the_core_survives() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 1_000.0,
        // The `ALT` mount, and NO `binance#ALT` engine beside the default one…
        strategy: Some(mount("alt", Some(alt()), "m-alt", &seen)),
        // …and a healthy mount behind it that must not be taken down with it.
        extra_mounts: vec![mount("default", None, "m-default", &seen)],
        ..CoreConfig::default()
    };
    let mut core = core_of(engine(CANON, 1_000.0), Vec::new(), config);

    assert!(core.mounts[0].is_none(), "the unresolvable mount is tombstoned, not mounted");
    assert!(core.mounts[1].is_some(), "the other mount keeps its slot");
    assert_eq!(core.mount_ids.len(), 2, "slots are never resized: the index stays an identity");
    assert_eq!(core.mount_engine[1], 0);
    let noted = |needle: &str| core.recent.iter().any(|l| l.contains(needle));
    assert!(noted("MOUNT REFUSED: `m_alt`"), "names the MOUNT: {:?}", core.recent);

    // The survivor runs; the refused strategy is never driven, so it places nothing anywhere.
    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    let seen = seen.lock().unwrap().clone();
    let labels: Vec<&str> = seen.iter().map(|(l, ..)| *l).collect();
    assert_eq!(labels, vec!["default"], "only the healthy mount ran: {seen:?}");
    assert_eq!(core.eng(0).client.submissions.len(), 1, "…and its order reached its own engine");
}

/// …and the refusal carries what an operator needs to fix it: the mount id, the account, the
/// venue, the route key that was looked for, and the keys that would arm it. Asserted separately
/// from the test above because that one is about the SURVIVAL and this one's whole job is that the
/// sentence is actionable (the resolution's `Err` starts mid-sentence; the door prefixes the id).
#[test]
fn the_unmounted_account_refusal_says_what_to_do() {
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount("alt", Some(alt()), "m-alt", &seen)),
        ..CoreConfig::default()
    };
    let core = core_of(engine(CANON, 1_000.0), Vec::new(), config);
    let text =
        core.recent.iter().find(|l| l.contains("MOUNT REFUSED")).cloned().unwrap_or_default();
    assert!(text.contains("m_alt"), "names the MOUNT: {text}");
    assert!(text.contains("ALT"), "names the ACCOUNT: {text}");
    assert!(text.contains("binance"), "names the VENUE: {text}");
    assert!(text.contains("binance#ALT"), "names the ROUTE KEY it looked for: {text}");
    assert!(text.contains("--label ALT"), "names the row to write: {text}");
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
    let mut core = core_of(engine(CANON, 1_000.0), Vec::new(), config);
    let before = core.mounts.len();

    core.mount_strategy_runtime(vike_exec::MountSpec {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        account: Some(alt()),
        controller_id: Some("rt-alt".into()),
        name: Some("buy_hold".into()),
        rhai: None,
        params: vike_model::params::Params::new(),
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
/// spawn refusal would have passed for the whole period the runtime door was already refusing, and
/// vice versa.
///
/// ⚠ **Mutation proof (production code, not the harness):** put `.unwrap_or(0)` back on
/// `mount_engine_resolution`'s `None` arm — i.e. make the miss resolve engine zero again — and
/// the spawn half mounts a slot and so does the runtime half. Both halves go red, each
/// for its stated reason.
#[test]
fn a_mount_naming_an_unmounted_venue_is_refused_at_both_doors() {
    // SPAWN: a mount on a venue with no engine must be refused rather than resolve engine 0.
    let seen = Arc::new(TestMutex::new(Vec::new()));
    let mut foreign = mount("default", None, "m-elsewhere", &seen);
    foreign.venue = "okx".to_string();
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(foreign),
        extra_mounts: vec![mount("default", None, "m-default", &seen)],
        ..CoreConfig::default()
    };
    let core = core_of(engine(CANON, 1_000.0), Vec::new(), config);
    assert!(core.mounts[0].is_none(), "the engine-less mount must not take a slot (engine zero)");
    assert!(core.mounts[1].is_some(), "…and its neighbour is untouched");
    let text =
        core.recent.iter().find(|l| l.contains("MOUNT REFUSED")).cloned().unwrap_or_default();
    assert!(text.contains("m_elsewhere"), "names the MOUNT: {text}");
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
    let mut core = core_of(engine(CANON, 1_000.0), Vec::new(), config);
    let before = core.mounts.len();
    core.mount_strategy_runtime(vike_exec::MountSpec {
        venue: "okx".into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        account: None,
        controller_id: Some("rt-okx".into()),
        name: Some("buy_hold".into()),
        rhai: None,
        params: vike_model::params::Params::new(),
    });
    assert_eq!(core.mounts.len(), before, "the refused mount must create NO slot");
    let noted = |needle: &str| core.recent.iter().any(|l| l.contains(needle));
    assert!(noted("MOUNT REFUSED"), "recent: {:?}", core.recent);
    assert!(noted("names venue `okx`"), "names the venue: {:?}", core.recent);
    assert!(
        noted("ENGINE ZERO"),
        "…the SAME sentence the spawn door notes — one resolution, one wording: {:?}",
        core.recent
    );
}

/// What each [`Hearer`] saw from inside a market hook: `(label, hook, position, equity)`.
type Heard = Arc<TestMutex<Vec<(&'static str, &'static str, f64, f64)>>>;

/// Records `(label, hook, its broker's position, its broker's equity)` on every bar and quote.
struct Hearer {
    label: &'static str,
    heard: Heard,
}

impl Strategy<LiveBroker> for Hearer {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        self.heard.lock().unwrap().push((self.label, "bar", broker.position, broker.equity));
    }

    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.heard.lock().unwrap().push((self.label, "quote", broker.position, broker.equity));
    }
}

/// A [`Hearer`] mount on [`CANON`]/[`SYMBOL`]/[`INTERVAL`] naming `account`.
fn hearer_mount(
    label: &'static str,
    account: AccountLabel,
    controller_id: &str,
    heard: &Heard,
) -> StrategyMount {
    StrategyMount {
        account: Some(account),
        symbols: Vec::new(),
        controller_id: Some(controller_id.to_string()),
        underlying_symbol: None,
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        strategy: Box::new(Hearer { label, heard: Arc::clone(heard) }),
    }
}

/// **The fan-out half, on two accounts.** Two mounts on ONE series, one per account, both hear every
/// closed bar and quote — and on those lanes each reads its OWN account's book. ALT holds a position
/// the default account does not and the two are seeded with different capital, so a fan-out that
/// resolved the engine once, for the first mount, shows up here as a wrong number rather than as a
/// missing call.
#[test]
fn two_accounts_on_one_series_each_hear_bars_and_ticks_against_their_own_book() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(hearer_mount("default", default_acct(), "m-default", &heard)),
        extra_mounts: vec![hearer_mount("alt", alt(), "m-alt", &heard)],
        ..CoreConfig::default()
    };
    let mut core =
        core_of(engine(CANON, 1_000.0), vec![(2_000.0, engine("binance#ALT", 2_000.0))], config);
    set_position(core.eng_mut(1), 2.0, 100.0);

    core.dispatch(Ingest::BarClose(Box::new(BarUpdate {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        bar: Bar {
            ts: 60_000,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        },
    })));
    core.dispatch(Ingest::Quote(Box::new(QuoteUpdate {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        quote: QuoteTick {
            ts: 60_500,
            local_ts: 0,
            bid: 99.0,
            ask: 101.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    })));

    let got = heard.lock().unwrap().clone();
    let hooks: Vec<(&str, &str)> = got.iter().map(|(l, h, ..)| (*l, *h)).collect();
    assert_eq!(
        hooks,
        vec![("default", "bar"), ("alt", "bar"), ("default", "quote"), ("alt", "quote")],
        "both accounts' mounts hear every bar and quote, in mount order: {got:?}"
    );
    // each mount's equity is ITS engine's, computed the way the lanes compute it; the fixture makes
    // the two differ, or the per-mount assertion below could not tell them apart
    let own_equity = |i: usize| core.eng(i).sizing_equity(core.seed_of(i), &core.config.price_cfg);
    let (eq_default, eq_alt) = (own_equity(0), own_equity(1));
    assert!(
        (eq_default - eq_alt).abs() > 1.0,
        "the two books must differ: {eq_default} vs {eq_alt}"
    );
    for (label, hook, position, equity) in &got {
        let (want_pos, want_eq) = if *label == "alt" { (2.0, eq_alt) } else { (0.0, eq_default) };
        assert!(
            (position - want_pos).abs() < 1e-9,
            "{label} on {hook} reads its own position: {got:?}"
        );
        assert!((equity - want_eq).abs() < 1e-9, "{label} on {hook} reads its own equity: {got:?}");
    }
}
