//! The per-mount readiness gate, and the one-price law the decision lanes share with the display.

use super::*;
use crate::runtime::test_support::mount_of;

// ---- portfolio-observer PR-4 T5: per-mount readiness gate -------------------------------

/// A minimal maker-shaped strategy for the readiness-gate tests: submits a market buy on
/// EVERY quote tick, unconditionally, and counts its own calls. "Did an order reach the
/// engine" is then a direct proxy for "did `drain_broker` actually drain this dispatch's
/// buffered submit" (Pending must discard it; Ready must not) — and the call counter
/// independently proves the hook itself still ran while Pending (warmup/observation must be
/// unaffected by the gate; only the ORDER OUTPUT is gated).
struct AlwaysSubmitStrategy {
    calls: Arc<AtomicUsize>,
}
impl Strategy<LiveBroker> for AlwaysSubmitStrategy {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
}

fn maker_mount(calls: Arc<AtomicUsize>) -> StrategyMount {
    mount_of("sim", "BTCUSDT", "1m", Box::new(AlwaysSubmitStrategy { calls }))
}

/// gate ON, a maker mount, NO board price for its symbol -> a quote-tick dispatch that would
/// normally submit is DISCARDED (the engine's order registry stays empty) while Pending, even
/// though the strategy hook itself still ran. Feed the board a price and re-probe: the mount
/// flips Ready, and the NEXT submit DOES land in the registry.
#[test]
fn pending_mount_discards_submits_until_symbol_prices() {
    let calls = Arc::new(AtomicUsize::new(0));
    let config = CoreConfig {
        readiness_gate: true,
        strategy: Some(maker_mount(Arc::clone(&calls))),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    assert_eq!(
        core.mount_states,
        vec![MountState::Pending],
        "readiness_gate: true must seed every mount Pending"
    );

    // no board price yet -> a dispatch that would normally submit must be discarded.
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the hook must still fire while Pending");
    assert!(
        core.engine.registry.is_empty(),
        "a Pending mount's buffered submit must never reach the engine"
    );

    // the boundary probe with STILL no board price must not flip it.
    core.maintain_mount_readiness(0);
    assert_eq!(core.mount_states[0], MountState::Pending, "no price yet -> still Pending");

    // the symbol prices -> the boundary probe flips Pending -> Ready.
    core.engine.price_board.set_quote("sim", "BTCUSDT", 99.0, 101.0, 0);
    core.maintain_mount_readiness(0);
    assert_eq!(
        core.mount_states[0],
        MountState::Ready,
        "a priced symbol must flip Pending -> Ready"
    );

    // a submit now DOES land.
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 1, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(calls.load(Ordering::Relaxed), 2, "the hook ran again on the Ready dispatch");
    assert_eq!(core.engine.registry.len(), 1, "a Ready mount's submit must reach the engine");
}

/// gate OFF (the default): a maker submits immediately, exactly as today — no Pending
/// suppression, and the boundary probe is never even reachable (every mount starts Ready).
#[test]
fn readiness_gate_off_is_byte_identical() {
    assert!(!CoreConfig::default().readiness_gate, "sanity: the gate defaults to off");
    let calls = Arc::new(AtomicUsize::new(0));
    let config =
        CoreConfig { strategy: Some(maker_mount(Arc::clone(&calls))), ..CoreConfig::default() };
    let mut core = core_with(config);
    assert_eq!(
        core.mount_states,
        vec![MountState::Ready],
        "readiness_gate: false must seed every mount Ready"
    );

    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        core.engine.registry.len(),
        1,
        "gate off: a submit lands immediately, exactly like before this feature existed"
    );
}

// ---- one-price law: the decision lanes read the SAME resolver-priced equity the display does --

/// Capture-only strategy for the one-price-law test: records `ctx.equity` on every quote tick.
struct EquityCapture {
    seen: Arc<Mutex<Vec<f64>>>,
}
impl Strategy<LiveBroker> for EquityCapture {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.seen.lock().unwrap().push(broker.equity);
    }
}

/// A position with NO mark recorded but a live quote on the board: the strategy's `ctx.equity`
/// must now equal the snapshot's resolver-priced equity bit-for-bit (previously it was
/// `equity_all`'s silent-zero, differing by the FULL unrealized amount) — and must NOT equal the
/// mark-only law, proving the decision lane genuinely switched sources.
#[test]
fn strategy_ctx_equity_matches_snapshot_for_quote_only_position() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let config = CoreConfig {
        strategy: Some(mount_of(
            "sim",
            "BTCUSDT",
            "1m",
            Box::new(EquityCapture { seen: Arc::clone(&seen) }),
        )),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    // bid/ask on the board, NO mark slot — the resolver values the long at the BID (104)
    core.engine.price_board.set_quote("sim", "BTCUSDT", 104.0, 106.0, 1);

    core.drive_strategy_tick("sim", "BTCUSDT", 105.0, 2, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let ctx_equity = *seen.lock().unwrap().first().expect("hook captured equity");

    let snap = crate::snapshot::build(
        1,
        &core.engine,
        &core.extra_engines,
        core.config.seed_cash,
        core.config.price_cfg,
        vike_exec::MarginCallConfig::default().mm_requirement,
        &core.recent,
        &core.bars,
        std::sync::Arc::new([]),
        &core.fault,
        0,
        0,
        vike_exec::ReconBlock::default(),
        Vec::new(),
    );
    assert_eq!(
        ctx_equity.to_bits(),
        snap.portfolio.venues[0].equity.to_bits(),
        "decision-lane equity must equal the displayed snapshot equity bit-for-bit"
    );
    // the tick path marked the account at the tick price (105), so the mark-only law differs:
    // the two lanes genuinely read different stores, and the decision lane now reads the board's.
    assert!(
        ctx_equity != core.engine.account.equity_all(core.config.seed_cash),
        "quote-priced ctx.equity must diverge from the mark-only legacy law here"
    );
}

/// Drawdown latch on a loss visible ONLY through the quote lane (no mark ever recorded): the
/// resolver-priced sweep must latch Reducing. Under the old `equity_all` source both sweeps read
/// the bare seed (unmarked position = silent zero) and the crash was invisible by construction.
#[test]
fn drawdown_latch_acts_on_resolver_priced_equity() {
    let mut core = test_core();
    core.config.seed_cash = 1_000.0;
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    // flat quote first: equity 1000 seeds the high-water-mark
    core.engine.price_board.set_quote("sim", "BTCUSDT", 100.0, 100.5, 1);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "no drawdown at the HWM seed");
    // the bid crashes to 40 (long valued at bid): equity 1000 + 10·(40-100) = 400 → 60% > 20%
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 2);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Reducing,
        "a quote-lane crash must trip the latch (mark-only equity never saw it)"
    );
    assert!(
        core.recent.iter().any(|e| e.starts_with("DRAWDOWN LATCH")),
        "the latch warning lands in the ring: {:?}",
        core.recent
    );
}
