//! The drawdown measure has TWO readers and they must not disagree.
//!
//! `CoreThread::sweep_drawdown_latch` folds its high-water-mark on the daemon's own equity curve
//! computed ENGINE-SIDE (`Σ seed_of(e) + Σ vike_exec::ExecutionEngine::resolved_own_pnl(e)`), while
//! `vike_alerting`'s `RuleTrigger::Drawdown` reads the same curve SNAPSHOT-SIDE
//! (`vike_core::Portfolio::drawdown_curve` = `capital_base + pnl_total`, folded from the four
//! published `VenueBlock` fields). Those are two different pieces of arithmetic over two different
//! data shapes, and if they drift an operator can be told "no drawdown" about a core that just
//! latched itself liquidate-only — or paged about one that did not.
//!
//! This file pins them BIT-identical, not merely close: same term order, same `py_sum` (Neumaier)
//! fold law, same venue order (primary first, then extras in registration order). It uses only
//! public API — `resolved_own_pnl` and `CoreSnapshot::build` — so it is a contract test, not a
//! white-box one. The BEHAVIOURAL tests (a third-party wallet movement must not move the latch; a
//! real own-book loss must trip it) live in `crates/vike-core/src/runtime/safe_state_tests.rs`,
//! which can drive the `pub(crate)` sweep synchronously.

use vike_core::snapshot::CoreSnapshot;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ExecutionEngine, MarginCallConfig, PositionEntry, PriceCfg, RiskGate,
    RiskLimits,
};

/// An engine on `venue`/`symbol` holding a LONG `qty` @ `avg_px`, with a live two-sided quote at
/// `bid`/`ask` on the board so the unrealized fold resolves rather than reading `Missing`.
fn eng(
    venue: &str,
    symbol: &str,
    mode: BalanceMode,
    qty: f64,
    avg_px: f64,
    bid: f64,
) -> ExecutionEngine<RecordingClient> {
    let mut account = Account::new(1.0, venue, None, mode);
    account.positions.insert(
        (venue.into(), symbol.into(), "BOTH".into()),
        PositionEntry { size: qty, avg_px, ..Default::default() },
    );
    // Non-trivial, non-round PnL terms on purpose: a fold-law difference between the two readers
    // shows up in the last ULP, and round numbers hide it.
    account.realized_pnl = 1_234.567_890_123_4;
    account.fees_paid = 7.891_011_121_3;
    // ⚠ NOT π (`-3.141_592_653_59`), which is what this was: `clippy::approx_constant` is
    // `deny`-by-default and CI's lint gate refused it. Any equally un-round decimal serves the
    // purpose — do not "restore" a recognisable constant here.
    account.funding_paid = -3.847_216_509_31;
    let mut e = ExecutionEngine::new(
        account,
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        symbol,
    );
    e.price_board.set_quote(venue, symbol, bid, bid + 0.5, 1);
    e
}

/// Adopt `wallet` USDT authoritatively — the venue's attested balance for the whole account.
fn adopt_wallet(e: &mut ExecutionEngine<RecordingClient>, venue: &str, wallet: f64) {
    e.account.apply_account_state(
        &vike_model::events::AccountState {
            venue: venue.into(),
            balances: vec![("USDT".to_string(), wallet)],
            ts: 0,
            route_key: None,
        },
        "USDT",
    );
    assert_eq!(e.account.balance_mode, BalanceMode::Authoritative);
}

#[test]
fn the_latch_curve_and_the_published_curve_are_bit_identical() {
    let cfg = PriceCfg::default();
    // The the CI box shape (2026-08-17), scaled down: a paper `Delta` primary plus two live mounts, one
    // of which has adopted a SHARED account's whole 53647.10600813 USDT wallet.
    let primary = eng("sim", "BTCUSDT", BalanceMode::Delta, 10.0, 100.0, 93.75);
    let mut bybit = eng("bybit", "ETHUSDT", BalanceMode::Delta, -4.0, 2_500.0, 2_611.25);
    adopt_wallet(&mut bybit, "bybit", 53_647.106_008_13);
    let mut okx = eng("okx", "SOLUSDT", BalanceMode::Delta, 137.0, 18.25, 17.9375);
    adopt_wallet(&mut okx, "okx", 811.222_333_444_5);

    let seeds = [1_000.0_f64, 2_500.5, 333.25];
    let extras = vec![(seeds[1], bybit), (seeds[2], okx)];

    // ENGINE-SIDE, exactly as `CoreThread::sweep_drawdown_latch` computes it.
    let base_engine_side =
        vike_model::py_sum(std::iter::once(seeds[0]).chain(extras.iter().map(|(s, _)| *s)));
    let pnl_engine_side = vike_model::py_sum(
        std::iter::once(primary.resolved_own_pnl(&cfg))
            .chain(extras.iter().map(|(_, e)| e.resolved_own_pnl(&cfg))),
    );
    let latch_curve = base_engine_side + pnl_engine_side;

    // SNAPSHOT-SIDE, exactly as `vike_alerting`'s `RuleTrigger::Drawdown` reads it.
    let snap = CoreSnapshot::build(
        1,
        &primary,
        &extras,
        seeds[0],
        cfg,
        MarginCallConfig::default().mm_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        &[],
        &None,
        0,
        0,
        vike_core::snapshot::ReconBlock::default(),
        &[],
    );

    assert_eq!(
        snap.portfolio.capital_base.to_bits(),
        base_engine_side.to_bits(),
        "the published capital base must be the latch's, bit-for-bit ({} vs {})",
        snap.portfolio.capital_base,
        base_engine_side
    );
    assert_eq!(
        snap.portfolio.pnl_total().to_bits(),
        pnl_engine_side.to_bits(),
        "`Portfolio::pnl_total` folds the published VenueBlock fields; \
         `resolved_own_pnl` folds the Account directly. Same term order, same py_sum, so same \
         bits ({} vs {})",
        snap.portfolio.pnl_total(),
        pnl_engine_side
    );
    assert_eq!(
        snap.portfolio.drawdown_curve().to_bits(),
        latch_curve.to_bits(),
        "and therefore the same curve ({} vs {})",
        snap.portfolio.drawdown_curve(),
        latch_curve
    );

    // The contamination this exists to keep OUT: `equity_total` carries both adopted wallets, so it
    // is a completely different number from the curve. If these ever coincide the fixture has lost
    // its point.
    assert!(
        (snap.portfolio.equity_total - latch_curve).abs() > 40_000.0,
        "equity_total {} still carries the two adopted wallets (54458 combined) that the curve \
         {} excludes — if these ever coincide the fixture has lost its point",
        snap.portfolio.equity_total,
        latch_curve
    );
}

/// ⚠ **The TERM-ORDER half of the pin, on a fixture that can actually see it.** The realistic
/// fixture above catches a reader that folds a different SET of terms, a different venue ORDER, or a
/// plain `.sum()` instead of `py_sum` — but it does NOT catch a pure reassociation of the same four
/// values, because at those magnitudes `(realized − fees) + funding` and
/// `(realized + funding) − fees` land on the same `f64`. Measured: planting exactly that swap in
/// `Portfolio::pnl_total` left the test above GREEN. A doc that claims term order is pinned needs a
/// fixture where term order is observable, so this is that fixture.
///
/// The terms are chosen at the `2^53` boundary, and the test VERIFIES its own sensitivity before
/// asserting anything — an inert fixture here would be a gate that reports success about nothing.
#[test]
fn the_two_folds_agree_where_reassociating_the_terms_changes_the_last_bit() {
    let realized = 9_007_199_254_740_994.0_f64; // 2^53 + 2
    let fees = 1.0_f64;
    let funding = 2.0_f64;
    assert_ne!(
        ((realized - fees) + funding).to_bits(),
        ((realized + funding) - fees).to_bits(),
        "INERT FIXTURE: reassociating these terms must change the result, or this test gates \
         nothing. (realized−fees)+funding = {}, (realized+funding)−fees = {}",
        (realized - fees) + funding,
        (realized + funding) - fees
    );

    // No position on purpose: `unrealized` is 0 on both sides, so the ONLY thing under test is how
    // the three scalar terms are combined.
    let mut account = Account::new(1.0, "bybit", None, BalanceMode::Delta);
    account.realized_pnl = realized;
    account.fees_paid = fees;
    account.funding_paid = funding;
    let e = ExecutionEngine::new(
        account,
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "ETHUSDT",
    );
    let cfg = PriceCfg::default();
    let snap = CoreSnapshot::build(
        1,
        &e,
        &[],
        0.0,
        cfg,
        MarginCallConfig::default().mm_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        &[],
        &None,
        0,
        0,
        vike_core::snapshot::ReconBlock::default(),
        &[],
    );
    assert_eq!(
        snap.portfolio.pnl_total().to_bits(),
        e.resolved_own_pnl(&cfg).to_bits(),
        "the published fold must reproduce the engine fold BIT-for-bit, term order included: \
         {} vs {}",
        snap.portfolio.pnl_total(),
        e.resolved_own_pnl(&cfg)
    );
}

/// `resolved_own_pnl` is MODE-BLIND: adopting a venue wallet flips `balance_mode`, rewrites
/// `balance` absolutely and moves `resolved_equity` by the full wallet — and must not move own PnL
/// by one bit. This is the property the whole fix rests on, so it is pinned on its own rather than
/// inferred from the curve test above.
#[test]
fn adopting_a_venue_wallet_moves_equity_by_the_wallet_and_own_pnl_by_nothing() {
    let cfg = PriceCfg::default();
    let mut e = eng("bybit", "ETHUSDT", BalanceMode::Delta, 10.0, 100.0, 93.75);
    let pnl_before = e.resolved_own_pnl(&cfg);
    let equity_before = e.resolved_equity(1_000.0, &cfg);

    adopt_wallet(&mut e, "bybit", 53_647.106_008_13);
    assert_eq!(
        e.resolved_own_pnl(&cfg).to_bits(),
        pnl_before.to_bits(),
        "own PnL is realized − fees + funding + unrealized; a wallet adoption writes none of them"
    );
    assert!(
        (e.resolved_equity(1_000.0, &cfg) - equity_before).abs() > 50_000.0,
        "…while equity moved by the whole wallet: {} → {}",
        equity_before,
        e.resolved_equity(1_000.0, &cfg)
    );

    // A THIRD PARTY withdrawing: same again, in the other direction.
    let pnl_at_wallet = e.resolved_own_pnl(&cfg);
    adopt_wallet(&mut e, "bybit", 30_000.0);
    assert_eq!(
        e.resolved_own_pnl(&cfg).to_bits(),
        pnl_at_wallet.to_bits(),
        "a −23647 withdrawal by somebody else is not this daemon's loss"
    );
}

/// On an all-`Delta` core — every paper mount, every backtest-shaped run — `seed + own_pnl` IS
/// `resolved_equity`, because `Delta`-mode `balance` is exactly `−fees_paid + funding_paid`
/// accumulated from the same fills. So the move off `resolved_equity` is a NO-OP wherever there is
/// no venue wallet to contaminate the measure, and bites exactly where one exists. Pinned because
/// the whole "this changes nothing you were relying on" claim rests on it.
#[test]
fn on_a_delta_account_seed_plus_own_pnl_is_exactly_resolved_equity() {
    let cfg = PriceCfg::default();
    let mut account = Account::new(1.0, "sim", None, BalanceMode::Delta);
    // Drive REAL fills so `balance`, `realized_pnl` and `fees_paid` move through the one fold path
    // rather than being assigned — the equality is a property of that fold, not of the fields.
    let fill = |tid: &str, side: i32, qty: f64, px: f64, fee: f64| vike_model::events::FillEvent {
        trade_id: vike_model::events::TradeId::new(tid).expect("test ids are non-empty"),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: fee,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    };
    account.apply_fill(&fill("a", 1, 7.0, 101.25, 0.708_75));
    account.apply_fill(&fill("b", -1, 3.0, 109.5, 0.328_5)); // realizes on the closed portion
    account.apply_fill(&fill("c", 1, 2.5, 97.125, 0.242_812_5));
    account.apply_funding(&vike_model::events::FundingEvent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        position_side: "BOTH".into(),
        funding_rate: 0.0001,
        amount: -1.234_5,
        mark_price: Some(100.0),
        ts: 0,
        route_key: None,
    });
    let mut e = ExecutionEngine::new(
        account,
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    e.price_board.set_quote("sim", "BTCUSDT", 93.75, 94.25, 1);

    let seed = 1_000.0;
    assert_eq!(
        (seed + e.resolved_own_pnl(&cfg)).to_bits(),
        e.resolved_equity(seed, &cfg).to_bits(),
        "seed + own_pnl == resolved_equity on a Delta account: {} vs {}",
        seed + e.resolved_own_pnl(&cfg),
        e.resolved_equity(seed, &cfg)
    );
    // Guard the fixture: the terms must actually be nonzero, or the equality is vacuous.
    assert_ne!(e.account.realized_pnl, 0.0);
    assert_ne!(e.account.fees_paid, 0.0);
    assert_ne!(e.account.funding_paid, 0.0);
}
