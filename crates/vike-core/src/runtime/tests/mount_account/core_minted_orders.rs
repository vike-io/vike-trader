//! The orders the core mints itself (stops, margin calls, flattens, bar fills) and the panic button.

use super::*;

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

fn mount_with_account(
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
pub(crate) fn crashing_bar() -> Bar {
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

pub(crate) fn set_position(engine: &mut ExecutionEngine<RecordingClient>, size: f64, avg_px: f64) {
    engine.account.positions.insert(
        (CANON.into(), SYMBOL.into(), "BOTH".into()),
        vike_exec::PositionEntry { size, avg_px, ..Default::default() },
    );
}

fn stop_core(stop_px: f64) -> CoreThread<RecordingClient> {
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(mount_with_account(Box::new(Idle), Some(default_acct()), "m-default")),
        extra_mounts: vec![mount_with_account(
            Box::new(Stopper { price: stop_px }),
            Some(alt()),
            "m-alt",
        )],
        ..CoreConfig::default()
    };
    core_of(engine(CANON, 1_000.0), vec![(2_000.0, engine("binance#ALT", 2_000.0))], config)
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
    assert_eq!(core.cond_engine.values().map(|a| a.engine).collect::<Vec<usize>>(), vec![1]);
    // ...and WHICH MOUNT armed it (mount 1 is `m-alt`), so the fire stays that strategy's order.
    assert_eq!(core.cond_engine.values().map(|a| a.mount).collect::<Vec<_>>(), vec![Some(1)]);

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
    assert_eq!(
        core.coid_mount.get(&core.eng(1).client.submissions[0].client_order_id),
        Some(&1),
        "the fired stop is attributed to the mount that armed it"
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
        strategy: Some(mount_with_account(Box::new(Stopper { price: 90.0 }), None, "m-default")),
        ..CoreConfig::default()
    };
    let mut core = core_of(engine(CANON, 1_000.0), Vec::new(), config);

    core.drive_strategy_feed_status(CANON, SYMBOL, FeedStatus::Disconnected);
    assert_eq!(core.cond_engine.values().map(|a| a.engine).collect::<Vec<usize>>(), vec![0]);

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
        OrderIntent::MassCancel {
            venue: Some(CANON.into()),
            symbol: Some(SYMBOL.into()),
            account: None,
        },
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
        engine(CANON, 1_000.0),
        vec![(100.0, engine("binance#ALT", 100.0))],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
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
        engine(CANON, 1_000.0),
        vec![(2_000.0, engine("binance#ALT", 2_000.0))],
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
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
        engine(CANON, 1_000.0),
        Vec::new(),
        CoreConfig { seed_cash: 1_000.0, ..CoreConfig::default() },
    );
    assert_eq!(solo.engines_of_venue(CANON), vec![0]);
}

/// **THE ACCOUNT-AGGREGATE EXPOSURE CEILING IS SPENT PER ACCOUNT, THROUGH THE REAL ROUTING** —
/// `vike_model::RiskLimits::max_account_exposure`, driven where the account-to-engine resolution
/// actually happens.
///
/// The exec-crate suite (`crates/vike-exec/tests/risk/risk_account_ceiling.rs`'s
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
        core_of(engine_capped(CANON), vec![(2_000.0, engine_capped("binance#ALT"))], config);
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
// `OrderIntent::MarketExit { venue: Some(v) }` naming NO account is the opposite shape: the
// operator named an exchange and no book of it (the shape every DOM click, tradehub ticket and
// CLI verb had before the reducing verbs could carry an account — see the section at the end),
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
/// `crates/vike-core/src/runtime/tests/apply/market_exit.rs`'s
/// `market_exit_walks_every_engine_and_routes_each_flatten_to_its_own` mounts `"sim"`
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
    core.apply_intent(OrderIntent::MarketExit { venue: Some(CANON.into()), account: None }, 2);

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
pub(crate) fn closes(e: &ExecutionEngine<RecordingClient>) -> Vec<(i32, f64)> {
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

    core.apply_intent(OrderIntent::MarketExit { venue: None, account: None }, 2);

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
        OrderIntent::MarketExit { venue: Some(CANON.into()), account: None },
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
