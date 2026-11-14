//! The PAPER TWO-LEG REHEARSAL of a live multi-symbol mount (one broker plan, step 12): `pairs_zscore`
//! resolved from a daemon profile the way `profile.rs`'s `resolve_mounts` resolves it, the symbols it
//! trades beside the mount's own declared as the mount's legs (`MountSpec::declare_strategy_legs` over
//! `Strategy::symbols`), mounted on the production paper builder and fed both legs' closed bars. The spread
//! opens on the bar that completes the pair, which is the LEG's, and each leg's market order reaches and
//! fills on its OWN paper book. Nothing here dials anything.
//!
//! The profile names a `.rs` strategy FILE, whose header the resolve judges the params against; the
//! sandboxed process that runs such a file is stood in for by an opener that builds the shipped file's own
//! `PairsZScore` (`strategies/pairs/pairs_zscore.rs`, compiled in-process by `vike_strategies_check::takers`)
//! from the judged params, in this process.

use vike_core::MountLeg;
use vike_execution::BarUpdate;
use vike_marketdata::Bar;

use crate::config::{ProfileDaemon, SandboxOpened};

const VENUE: &str = "bybit";
/// The mount's own symbol, `symbol_a` (the dependent leg).
const A: &str = "BTCUSDT";
/// The declared leg, `symbol_b` (the hedge).
const B: &str = "ETHUSDT";
const MINUTE: i64 = 60_000;

/// `pairs/pairs_zscore.rs`'s header: the keys this test's profile hands it, the two leg keys routed.
const PAIRS_ZSCORE_HEADER: &str = "\
//! ```toml
//! [strategy]
//! runs_on = \"live\"
//! routes = \"multi_leg\"
//! multi_leg_why = \"a pairs file: its two leg keys name the legs it trades\"
//!
//! [params.symbol_a]
//! type = \"text\"
//! route = \"symbol\"
//! [params.symbol_b]
//! type = \"text\"
//! route = \"symbol\"
//! [params.period]
//! type = \"number\"
//! [params.entry_z]
//! type = \"number\"
//! [params.exit_z]
//! type = \"number\"
//! [params.taker_fee]
//! type = \"number\"
//! [params.half_spread_bps]
//! type = \"number\"
//! [params.notional]
//! type = \"number\"
//! ```
";

fn bar(symbol: &str, ts: i64, close: f64) -> BarUpdate {
    BarUpdate {
        venue: VENUE.into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        bar: Bar::ohlcv(ts, close, close, close, close, 1.0),
    }
}

#[test]
fn pairs_zscore_trades_both_legs_each_on_its_own_paper_book() {
    // A short window and a low entry so seven closes open the spread; no fee, spread or carry, so the
    // cost gate passes any reversion edge.
    let profile = ProfileDaemon::from_toml_str(&format!(
        "venue = \"{VENUE}\"\nsymbol = \"{A}\"\n[strategy]\nfile = \"pairs/pairs_zscore.rs\"\n\
         [strategy.params]\nsymbol_a = \"{A}\"\nsymbol_b = \"{B}\"\nperiod = 3\nentry_z = 1.0\n\
         exit_z = 0.1\ntaker_fee = 0.0\nhalf_spread_bps = 0.0\nnotional = 1100.0\n"
    ))
    .expect("pairs_zscore loads as a live mount");
    let files =
        crate::test_strategy_files::folder(&[("pairs/pairs_zscore.rs", PAIRS_ZSCORE_HEADER)]);
    let strategy = profile
        .resolve_mount_with(files.path(), true, &|open| {
            Ok(SandboxOpened {
                strategy: Box::new(
                    vike_strategies_check::takers::pairs_zscore::PairsZScore::from_params(
                        open.params,
                    ),
                ),
                echo: Vec::new(),
                remote: None,
            })
        })
        .expect("resolves")
        .into_strategy();
    let mut spec = profile.to_mount_spec();
    spec.declare_strategy_legs(strategy.symbols()).expect("both legs in the door's spelling");
    assert_eq!(
        spec.legs,
        vec![MountLeg::same_venue(B)],
        "the leg is declared, the own symbol is not"
    );

    // A sentinel this test owns and never creates: the paper books' HALT stays off.
    let sentinel = std::env::temp_dir()
        .join(format!("vike-tradehub-two-leg-rehearsal-{}", std::process::id()))
        .join("HALT");
    let opts = vike_tradehub_core::StrategyCorePaperConfig {
        halt: vike_tradehub_core::StrategyCorePaperHalt::Pinned(sentinel),
        ..Default::default()
    };
    let core = vike_tradehub_core::build_paper_strategy_core_with(strategy, &spec, opts);
    let cell = core.handle.snapshot_cell();
    let bars = core.handle.bar_sender();
    // Three warmup bars, then four pairs; A jumps on the last, so the spread A - B is rich (z = 1.41):
    // SELL A, BUY B, decided on B's bar (A's closes first). The eighth pair fills both at its open.
    let closes_a = [100.0, 100.0, 100.0, 100.0, 100.0, 100.0, 110.0, 110.0];
    for (i, close_a) in closes_a.into_iter().enumerate() {
        let ts = i as i64 * MINUTE;
        bars.close(bar(A, ts, close_a)).unwrap();
        bars.close(bar(B, ts, 100.0)).unwrap();
    }
    core.handle.shutdown_and_join();

    let snap = cell.load();
    let held = |symbol: &str| {
        snap.positions.iter().filter(|p| p.symbol == symbol).map(|p| p.size).sum::<f64>()
    };
    let book = || {
        let orders: Vec<_> = snap
            .orders
            .iter()
            .map(|o| (o.symbol.as_str(), o.side, o.qty, &o.status, o.filled_qty))
            .collect();
        format!("positions {:?}, orders {orders:?}, ring {:?}", snap.positions, snap.recent_events)
    };
    // 1100 of A at 110 is 10; beta 1 hedges it with 10 of B.
    assert!((held(A) + 10.0).abs() < 1e-9, "A sold on its own book: {}", book());
    assert!((held(B) - 10.0).abs() < 1e-9, "B bought on its own book: {}", book());
    let own_fills = core.fills.lock().unwrap();
    assert_eq!(own_fills.len(), 1, "one fill on the mount's own book: {own_fills:?}");
    assert_eq!((own_fills[0].symbol.as_str(), own_fills[0].side), (A, -1), "{own_fills:?}");
}

/// **A leg the control door writes another way is refused, and no leg is declared** (the C-D4 follow-up): a strategy
/// declaring `ethusdt.s` beside a bybit mount used to declare it as written, a perpetual named `ethusdt.s` to bybit's
/// `split_spot`, and the daemon now refuses the mount naming `ETHUSDT.S`. A leg in the door's spelling still declares.
#[test]
fn a_leg_the_door_spells_another_way_is_refused_and_declares_nothing() {
    let profile = ProfileDaemon::from_toml_str(&format!(
        "venue = \"{VENUE}\"\nsymbol = \"{A}\"\n[strategy]\nfile = \"nothing.rhai\"\n"
    ))
    .expect("a plain bybit mount loads");
    let mut spec = profile.to_mount_spec();
    let refused = spec.declare_strategy_legs(vec!["ethusdt.s".to_string()]);
    assert!(spec.legs.is_empty(), "{:?}", spec.legs);
    assert!(format!("{refused:?}").contains("is written `ETHUSDT.S` on bybit"), "{refused:?}");
    let mut spec = profile.to_mount_spec();
    let declared = spec.declare_strategy_legs(vec!["ETHUSDT.S".to_string()]);
    assert_eq!(format!("{declared:?}"), format!("{:?}", Ok::<(), String>(())));
    assert_eq!(spec.legs, vec![MountLeg::same_venue("ETHUSDT.S")]);
}
