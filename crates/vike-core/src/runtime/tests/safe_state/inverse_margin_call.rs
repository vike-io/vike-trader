//! **What the risk clock's margin call does with a deribit INVERSE engine, in numbers.** Until its coin
//! wallet is adopted (decision 0258) such an engine is valued on its configured seed, and the first tests run
//! that seed through `sweep_risk_clock`; the last ones adopt the wallet, as a reconcile pass does, and the
//! sweep then judges the wallet's dollars, and the drawdown latch the seed held in coin.
//!
//! The figures are the CI box's account row 6 as read on 2026-11-07: the seed every engine takes from the primary
//! mount (1000), `BTC-PERPETUAL` at about 75,728, a testnet wallet of 99.99788715 BTC. The default
//! `vike_model::MarginCallConfig` judges them, so a breach is `maintenance > equity · 1.1` with
//! `maintenance = 0.05 · |size|`, and at 1x (`|size| <= seed`) that takes a 95.46% move.
//!
//! Two shapes. An account with no mount row (row 6) is a price-only venue, whose feed dials mainnet while its
//! exec signs on the testnet, so `crates/vike-tradehub/src/venue_arming/deadman.rs`'s `price_source_tiers`
//! puts it on another network and the sweep judges NOTHING. An account a mount row names is left out of that
//! table, and the sweep judges the seed: the unit is right (all three terms are dollars) and the figure moves
//! with the engine's own PnL, but the coin wallet never enters.

use super::*;
use crate::runtime::test_support::{core_of, sim_engine_with};
use vike_model::AccountTier;

const SYMBOL: &str = "BTC-PERPETUAL";
const SEED: f64 = 1_000.0;
const ENTRY: f64 = 75_728.0;
const ROW_SIX_WALLET_BTC: f64 = 99.997_887_15;

#[derive(Clone, Copy)]
enum Shape {
    /// No mount row names the venue: price-only, its feed on another network than its exec.
    PriceOnly,
    /// A mount row names the venue: absent from `price_source`.
    Mounted,
}

/// A `(primary sim, extra deribit)` core at the daemon's default margin call, on a clock stopped at 1 s.
fn deribit_core(shape: Shape) -> CoreThread<RecordingClient> {
    let mut grid = indexmap::IndexMap::new();
    grid.insert(SYMBOL.to_string(), vike_model::ContractValue::inverse(1.0));
    let mut deribit = Engine::new(
        Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        SYMBOL,
    );
    deribit.tier = AccountTier::Demo;
    let (venues, price_source) = match shape {
        Shape::PriceOnly => (
            ["deribit".to_string()].into_iter().collect(),
            [("deribit".to_string(), AccountTier::Live)].into_iter().collect(),
        ),
        Shape::Mounted => Default::default(),
    };
    core_of(
        sim_engine_with(RecordingClient::default()),
        vec![(SEED, deribit)],
        CoreConfig {
            seed_cash: SEED,
            clock: Box::new(|| 1_000),
            margin_call: Some(vike_model::MarginCallConfig::default()),
            price_only: Some(PriceOnlyConfig {
                venues,
                sweep_interval: std::time::Duration::from_secs(1),
                max_price_age: std::time::Duration::from_secs(60),
                price_source,
            }),
            ..CoreConfig::default()
        },
    )
}

/// One sweep of the clock at `px`, and the engine's equity as that sweep read it.
fn sweep_at(core: &mut CoreThread<RecordingClient>, px: f64) -> f64 {
    core.extra_engines[0].1.price_board.set_quote("deribit", SYMBOL, px, px, 1);
    core.arm_boundary_timers(0);
    core.drive_due_timers(1_100);
    core.extra_engines[0].1.resolved_equity(SEED, &core.config.price_config)
}

/// A fresh core holding `size` from [`ENTRY`], swept once at `px`.
fn swept(shape: Shape, size: f64, px: f64) -> (CoreThread<RecordingClient>, f64) {
    let mut core = deribit_core(shape);
    set_position(&mut core.extra_engines[0].1, "deribit", SYMBOL, size, ENTRY);
    let equity = sweep_at(&mut core, px);
    (core, equity)
}

/// The close the sweep sent: its side and quantity.
fn close_of(core: &CoreThread<RecordingClient>) -> Option<(i32, f64)> {
    let sent = &core.extra_engines[0].1.client.submissions;
    assert!(sent.len() <= 1, "one close per sweep: {sent:?}");
    sent.first().map(|o| {
        assert!(o.reduce_only, "a margin call only reduces");
        (o.side, o.qty)
    })
}

fn warned(core: &CoreThread<RecordingClient>) -> bool {
    core.recent.iter().any(|e| e.contains("margin-call WARNING"))
}

/// What the VENUE's account is worth in dollars at `px`: the coin wallet and the coin PnL of `size` from
/// [`ENTRY`], at that price.
fn wallet_usd(wallet_btc: f64, size: f64, px: f64) -> f64 {
    (wallet_btc + size * (1.0 / ENTRY - 1.0 / px)) * px
}

fn close_to(got: f64, want: f64) -> bool {
    (got - want).abs() < 1e-6
}

/// Account row 6's shape: no mount row, so the feed's network is not the account's and no sweep judges it. A
/// 99% loss on the seed sends nothing and warns of nothing; the skip is said once, in the ring.
#[test]
fn an_account_with_no_mount_row_is_not_judged_at_all() {
    let (core, equity) = swept(Shape::PriceOnly, SEED, ENTRY * 0.01);
    assert!(close_to(equity, 10.0), "{equity}");
    assert_eq!(close_of(&core), None, "breached on the seed, and nothing sent");
    assert!(!warned(&core));
    let skip = "price-only risk sweeps SKIP the deribit engine";
    assert_eq!(core.recent.iter().filter(|e| e.contains(skip)).count(), 1, "{:?}", core.recent);
}

/// At 1x, the most the buying-power lane admits, a mount-row engine needs a 94.74% move to warn and 95.46% to
/// fire, either way round: `(signed size, move, equity, close, warned)`.
#[test]
fn at_one_x_the_seed_takes_a_95_percent_move_to_fire() {
    let rows = [
        (SEED, -0.50, 500.0, None, false),
        (SEED, -0.947, 53.0, None, false),
        (SEED, -0.9474, 52.6, None, true),
        (SEED, -0.9545, 45.5, None, true),
        (SEED, -0.9546, 45.4, Some((-1, 92.0)), false),
        (SEED, -0.99, 10.0, Some((-1, 800.0)), false),
        (-SEED, 0.50, 500.0, None, false),
        (-SEED, 0.9545, 45.5, None, true),
        (-SEED, 0.9546, 45.4, Some((1, 92.0)), false),
        (-SEED, 1.0, 0.0, Some((1, SEED)), false),
        (-SEED, 2.0, -1_000.0, Some((1, SEED)), false),
    ];
    for (size, change, want_equity, want_close, want_warned) in rows {
        let (core, equity) = swept(Shape::Mounted, size, ENTRY * (1.0 + change));
        assert!(close_to(equity, want_equity), "{size} {change}: equity {equity}");
        match (close_of(&core), want_close) {
            (None, None) => {}
            (Some((side, qty)), Some((want_side, want_quantity))) => {
                assert_eq!(side, want_side, "{size} {change}");
                assert!(close_to(qty, want_quantity), "{size} {change}: qty {qty}");
            }
            (got, want) => panic!("{size} {change}: close {got:?}, wanted {want:?}"),
        }
        assert_eq!(warned(&core), want_warned, "{size} {change}: {:?}", core.recent);
    }
}

/// A profile's `[risk] max_leverage` is the asked-for way past 1x on a mount-row engine (the account row has no
/// leverage on this venue): at 10x the same seed fires at a 5.46% move, whatever the wallet holds.
#[test]
fn at_ten_x_the_seed_fires_at_five_and_a_half_percent() {
    let size = 10.0 * SEED;
    let (core, equity) = swept(Shape::Mounted, size, ENTRY * (1.0 - 0.0545));
    assert!(close_to(equity, 455.0), "{equity}");
    assert_eq!(close_of(&core), None);
    assert!(warned(&core), "{:?}", core.recent);

    let (core, equity) = swept(Shape::Mounted, size, ENTRY * (1.0 - 0.0546));
    assert!(close_to(equity, 454.0), "{equity}");
    let (side, qty) = close_of(&core).expect("maintenance 500 above 454 · 1.1");
    assert_eq!(side, -1);
    assert!(close_to(qty, 920.0), "the excess 46 at 0.05 a dollar: {qty}");
}

/// FIRES ON A HEALTHY ACCOUNT while the wallet is not adopted: a 1x short on a coin wallet of the same size is a
/// dollar hedge, worth 1000 at any price. At a doubled price the seed's equity is 0, and the sweep closes the
/// whole hedge. (Adopted, it holds: `with_the_wallet_adopted_a_hedged_short_holds_its_dollars`.)
#[test]
fn a_hedged_short_is_closed_though_its_wallet_has_not_moved() {
    let wallet = SEED / ENTRY;
    let px = ENTRY * 2.0;
    let (core, equity) = swept(Shape::Mounted, -SEED, px);
    assert!(close_to(wallet_usd(wallet, -SEED, px), SEED), "the hedge holds its dollars");
    assert!(close_to(equity, 0.0), "{equity}");
    assert_eq!(close_of(&core), Some((1, SEED)));
}

/// SILENT ON A SICK ONE: the same wallet long. The venue's account has 20 dollars left at 49% down, and it runs
/// out at 50%; the seed's equity says 510 and the sweep neither closes nor warns.
#[test]
fn a_long_on_a_wallet_the_size_of_the_seed_runs_out_and_the_sweep_is_silent() {
    let wallet = SEED / ENTRY;
    let px = ENTRY * 0.51;
    let (core, equity) = swept(Shape::Mounted, SEED, px);
    assert!(close_to(wallet_usd(wallet, SEED, px), 20.0), "{}", wallet_usd(wallet, SEED, px));
    assert!(close_to(wallet_usd(wallet, SEED, ENTRY * 0.5), 0.0), "gone at half the entry");
    assert!(close_to(equity, 510.0), "{equity}");
    assert_eq!(close_of(&core), None);
    assert!(!warned(&core), "{:?}", core.recent);
}

/// The one way past 1x with no leverage asked for: a foreign fill (a trade made at the venue, on the mounted
/// symbol) folds with no gate in front of it. Past 22 seeds the next sweep closes part of it with the price
/// unmoved, on a wallet that holds about 250 times the position.
#[test]
fn a_foreign_fill_past_22_seeds_is_cut_on_a_wallet_many_times_its_size() {
    let foreign = |qty: f64| FillEvent {
        trade_id: TradeId::prefixed("venue-ui-", "1"),
        order_client_id: String::new(),
        venue: "deribit".into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_price: ENTRY,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: Some(ENTRY),
        position_side: vike_model::events::PositionSide::Both,
    };
    let run = |qty: f64| {
        let mut core = deribit_core(Shape::Mounted);
        core.dispatch(Ingest::Event(Event::Fill(foreign(qty))));
        assert_eq!(core.extra_engines[0].1.account.net_qty(SYMBOL), qty, "folded, no gate");
        let equity = sweep_at(&mut core, ENTRY);
        assert!(close_to(equity, SEED), "the price has not moved: {equity}");
        core
    };

    let at_the_line = run(22.0 * SEED);
    assert_eq!(close_of(&at_the_line), None, "maintenance 1100 is not above 1000 · 1.1");
    assert!(warned(&at_the_line));

    let past_it = run(30.0 * SEED);
    let (side, qty) = close_of(&past_it).expect("maintenance 1500 breaches a seed of 1000");
    assert_eq!(side, -1);
    assert!(close_to(qty, 10_000.0), "the excess 500 at 0.05 a dollar: {qty}");
    let wallet = wallet_usd(ROW_SIX_WALLET_BTC, 30.0 * SEED, ENTRY);
    assert!(wallet > 250.0 * 30.0 * SEED, "{wallet}");
}

// ---- decision 0258: the coin wallet adopted, the seed held in coin --------------------------------------

/// The wallet a reconcile pass reports, in the frame the node builds from it (the engine's own quote label).
fn adopt_wallet(core: &mut CoreThread<RecordingClient>, wallet_btc: f64) {
    let e = &mut core.extra_engines[0].1;
    let label = e.quote_asset.clone();
    e.account.apply_account_state(
        &vike_model::events::AccountState {
            venue: "deribit".into(),
            balances: vec![(label.clone(), wallet_btc)],
            timestamp: 0,
            route_key: None,
        },
        &label,
    );
    assert_eq!(e.account.balance_mode, BalanceMode::Authoritative, "the coin wallet is adopted");
}

/// The hedge of `a_hedged_short_is_closed_though_its_wallet_has_not_moved`, its wallet adopted: the sweep reads
/// the 1000 dollars the account holds at a doubled price (the seed read 0 and closed the hedge), and sends nothing.
#[test]
fn with_the_wallet_adopted_a_hedged_short_holds_its_dollars() {
    let mut core = deribit_core(Shape::Mounted);
    adopt_wallet(&mut core, SEED / ENTRY);
    set_position(&mut core.extra_engines[0].1, "deribit", SYMBOL, -SEED, ENTRY);
    for px in [ENTRY * 2.0, ENTRY * 0.5, ENTRY * 1.1] {
        let equity = sweep_at(&mut core, px);
        assert!(close_to(equity, SEED), "at {px}: {equity}");
    }
    assert_eq!(close_of(&core), None);
    assert!(!warned(&core), "{:?}", core.recent);
}

/// The long of `a_long_on_a_wallet_the_size_of_the_seed_runs_out_and_the_sweep_is_silent`, its wallet adopted: the
/// sweep reads the 20 dollars left at 49% down (the seed read 510) and cuts the position.
#[test]
fn with_the_wallet_adopted_a_long_that_runs_out_is_cut() {
    let mut core = deribit_core(Shape::Mounted);
    adopt_wallet(&mut core, SEED / ENTRY);
    set_position(&mut core.extra_engines[0].1, "deribit", SYMBOL, SEED, ENTRY);
    let px = ENTRY * 0.51;
    let equity = sweep_at(&mut core, px);
    assert!(close_to(equity, wallet_usd(SEED / ENTRY, SEED, px)), "{equity}");
    assert!(close_to(equity, 20.0), "{equity}");
    let (side, qty) = close_of(&core).expect("maintenance 50 above 20 · 1.1");
    assert_eq!(side, -1);
    assert!(qty > 0.0 && qty <= SEED, "{qty}");
}

/// **The drawdown latch reads the hedge flat**: the deribit engine's seed (1000) is held in coin from the first
/// priced sweep, so at +10% its base is 1100 and the short's -100 nets it to 1000, the node's curve 2000 against a
/// peak of 2000. A long of the same dollars is doubly long the coin: -10% reads 800 on that engine, a 10% node
/// drawdown, and the 4% rule trips.
#[test]
fn the_drawdown_latch_reads_a_hedged_short_flat_and_a_long_twice() {
    let cases =
        [(-SEED, ENTRY * 1.1, false), (-SEED, ENTRY * 0.9, false), (SEED, ENTRY * 0.9, true)];
    for (size, px, trips) in cases {
        let mut core = deribit_core(Shape::Mounted);
        set_position(&mut core.extra_engines[0].1, "deribit", SYMBOL, size, ENTRY);
        core.extra_engines[0].1.price_board.set_quote("deribit", SYMBOL, ENTRY, ENTRY, 1);
        core.sweep_drawdown_latch(0.04);
        let peak = core.pnl_curve_peak.expect("the latch swept");
        assert!(close_to(peak, 2.0 * SEED), "{size}: the base at entry: {peak}");
        core.extra_engines[0].1.price_board.set_quote("deribit", SYMBOL, px, px, 2);
        core.sweep_drawdown_latch(0.04);
        assert!(
            close_to(core.extra_engines[0].0, SEED * px / ENTRY),
            "{size}: the seed held in coin"
        );
        let latched = core.engine.trading_state == TradingState::Reducing;
        assert_eq!(latched, trips, "{size} at {px}: {:?}", core.recent);
    }
}

/// **The owner's ruling of 2026-11-15 (decision 0258, second question "нет")**: with account row 6's 100-coin
/// demo wallet adopted, a strategy sizes from the seed the latch's sweep holds in coin (1000 at entry, 1100 at
/// +10%), never from the wallet, which the margin call still reads whole.
#[test]
fn with_the_wallet_adopted_a_strategy_sizes_from_the_seed_held_in_coin() {
    let mut core = deribit_core(Shape::Mounted);
    adopt_wallet(&mut core, ROW_SIX_WALLET_BTC);
    for (stamp, px, seed) in [(1, ENTRY, SEED), (2, ENTRY * 1.1, SEED * 1.1)] {
        core.extra_engines[0].1.price_board.set_quote("deribit", SYMBOL, px, px, stamp);
        core.sweep_drawdown_latch(0.25);
        let (deribit, config) = (core.engine(1), &core.config.price_config);
        let sizing = deribit.sizing_equity(core.seed_of(1), config);
        assert!(close_to(sizing, seed), "at {px}: {sizing}");
        let wallet = deribit.resolved_equity(core.seed_of(1), config);
        assert!(
            close_to(wallet, ROW_SIX_WALLET_BTC * px),
            "the margin call's figure at {px}: {wallet}"
        );
    }
}

/// **The same with the drawdown latch OFF** (no `max_drawdown`, as a profile may set): the risk clock still holds
/// the seed in coin, so the sizing cap moves with the coin (1000 at entry, 1100 at +10%), and the latch stays
/// off: no peak folded, the node Active, the seed slot the configured 1000.
#[test]
fn with_the_latch_off_the_sizing_seed_still_moves_with_the_coin() {
    let mut core = deribit_core(Shape::Mounted);
    assert_eq!(core.config.max_drawdown, None, "the latch is off");
    adopt_wallet(&mut core, ROW_SIX_WALLET_BTC);
    for (px, seed) in [(ENTRY, SEED), (ENTRY * 1.1, SEED * 1.1)] {
        let wallet = sweep_at(&mut core, px);
        assert!(
            close_to(wallet, ROW_SIX_WALLET_BTC * px),
            "the margin call's figure at {px}: {wallet}"
        );
        let sizing = core.engine(1).sizing_equity(core.seed_of(1), &core.config.price_config);
        assert!(close_to(sizing, seed), "at {px}: {sizing}");
    }
    assert_eq!(core.pnl_curve_peak, None, "the latch folded nothing");
    assert_eq!(core.engine.trading_state, TradingState::Active);
    assert_eq!(core.seed_of(1), SEED, "the slot, the latch's base, is not revalued");
}
