use super::*;
use crate::spec::params_from_rows;
use std::assert_matches;
use std::sync::Arc;
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::BookLevel;
use vike_model::{Bar, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

const SCRIPT: &str = r#"
const QTY = 1.0;
fn on_bar() {
    let f = sma(5); let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

fn rhai(src: &str) -> StrategySpec {
    StrategySpec::rhai(src)
}

fn seeded_store() -> (tempfile::TempDir, StoreHandle, Vec<Bar>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    // deterministic oscillating walk so the cross triggers
    let mut px = 100.0f64;
    let mut seed = 0x1234_5678u64;
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            px = (px + ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0).max(1.0);
            Bar {
                ts: 60_000 * (i as i64 + 1),
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    (dir, Arc::new(store) as StoreHandle, bars)
}

fn slice() -> DataSlice {
    DataSlice::bars("binance", "BTCUSDT", "1m", TsRange::all())
}

#[test]
fn run_slice_backtests_over_the_store() {
    let (_dir, store, _bars) = seeded_store();
    let out = run_slice(&rhai(SCRIPT), &slice(), &store, EngineParams::default());
    let res = out.expect("should backtest");
    assert!(res.n_trades > 0, "the cross should trade over 400 bars");
}

#[test]
fn compile_error_maps_to_runerror_compile() {
    let (_dir, store, _b) = seeded_store();
    let out = run_slice(&rhai("fn on_bar( {"), &slice(), &store, EngineParams::default());
    assert_matches!(out, Err(RunError::Compile(_)));
}

#[test]
fn empty_slice_maps_to_runerror_data() {
    let (_dir, store, _b) = seeded_store();
    let missing = DataSlice::bars("binance", "NOPE", "1m", TsRange::all());
    let out = run_slice(&rhai(SCRIPT), &missing, &store, EngineParams::default());
    assert_matches!(out, Err(RunError::Data(_)));
}

#[test]
fn spawn_run_delivers_the_same_outcome() {
    let (_dir, store, _b) = seeded_store();
    let rx = spawn_run(rhai(SCRIPT), slice(), store.clone(), EngineParams::default());
    let out = rx.recv().expect("worker sends one outcome");
    assert!(out.unwrap().n_trades > 0);
}

// ---- native (registry) strategies -------------------------------------------------------

/// The native path runs a REGISTRY strategy — no Rhai anywhere — and its params table is
/// honored: `buy_hold` with `size = 2` buys 2 units once, so the run produces a position and
/// a non-flat curve.
#[test]
fn native_strategy_runs_from_the_registry_with_params() {
    let (_dir, store, _b) = seeded_store();
    let spec = StrategySpec::native(
        "buy_hold",
        params_from_rows(&[("size".into(), "2".into()), ("symbol".into(), "BTCUSDT".into())]),
    );
    let res = run_slice(&spec, &slice(), &store, EngineParams::default())
        .expect("buy_hold should run natively");
    assert!(!res.equity_curve.is_empty(), "a native run produces an equity curve");
}

#[test]
fn every_registered_native_strategy_resolves() {
    for name in crate::spec::native_strategies() {
        assert!(
            build_strategy(&StrategySpec::native_default(*name)).is_ok(),
            "{name} should build"
        );
    }
}

#[test]
fn unknown_native_name_is_a_strategy_error_not_a_compile_error() {
    let (_dir, store, _b) = seeded_store();
    let spec = StrategySpec::native_default("no_such_strategy");
    let out = run_slice(&spec, &slice(), &store, EngineParams::default());
    assert_matches!(out, Err(RunError::Strategy(_)), "got {out:?}");
}

/// **Fix-round-1 CRITICAL, pinned.** An absent sha must never resolve as if it named a real
/// artifact — this is the permanent guard every caller (Run/Sweep/Walk-Forward/Compare-all)
/// relies on regardless of whether it remembered to ask `run_blocked_reason` first. Both an
/// empty string (the studio-side `unwrap_or_default()` shape) and a whitespace-only one are
/// refused, and the refusal names the EMPTINESS rather than falling through to whatever the
/// loader would have said about `<name>-.so` — the two must stay DISTINGUISHABLE so this
/// guard cannot be deleted by mistake as "redundant" now that the loader HAS landed. (This
/// said "the generic 'loader not wired' message … once the loader lands"; that message is
/// gone and its arm is the real `dlopen` now. The requirement is unchanged and is why the
/// sibling test below pins the OTHER refusal by its own words.)
#[test]
fn a_plugin_spec_with_no_sha_is_refused_and_never_reaches_the_loader_arm() {
    let empty = StrategySpec::plugin("my_strat", "", crate::spec::empty_params());
    // `Box<dyn Strategy<SimBroker>>` is not `Debug`, so the Ok side cannot ride `expect_err`/
    // `{:?}` on the whole `Result` — match it out explicitly instead.
    match build_strategy(&empty) {
        Err(RunError::Strategy(msg)) => {
            assert!(msg.contains("no sha"), "{msg}");
            assert!(msg.contains("names no artifact"), "{msg}");
        }
        Ok(_) => panic!("an empty sha must be refused"),
        Err(other) => panic!("wrong RunError variant: {other:?}"),
    }

    let whitespace = StrategySpec::plugin("my_strat", "   ", crate::spec::empty_params());
    assert!(build_strategy(&whitespace).is_err(), "whitespace-only is not a sha either");

    // The overrides door delegates to `build_strategy`, so the same refusal is reachable
    // through it too — a sweep point can never smuggle an empty sha past this guard.
    let via_overrides = build_strategy_with(&empty, &[("fast".to_string(), 5.0)]);
    assert!(via_overrides.is_err(), "the overrides path must inherit the same refusal");
}

/// The complement: a NON-empty sha reaches the LOADER arm, and with no artifact on disk the
/// loader's own `Missing` refusal is what comes back — naming the path and telling the author
/// to build it. That proves the two refusals are genuinely distinct rather than one silently
/// swallowing the other.
///
/// ⚠ This test used to assert the message said "not wired". It is the one assertion the join
/// was supposed to invalidate, and it now pins the OPPOSITE property: reaching the loader is
/// the success condition, and "no artifact at this path" is the loader speaking rather than
/// this function guessing. A missing artifact must NEVER be a silent fall back to some other
/// strategy — the design's own list of red tests says so in as many words.
#[test]
fn a_plugin_spec_with_a_real_looking_sha_reaches_the_loader_and_reports_a_missing_artifact() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let spec = StrategySpec::plugin("my_strat", "a".repeat(64), crate::spec::empty_params());
    match build_strategy_in(&spec, dir.path()) {
        Err(RunError::Strategy(msg)) => {
            assert!(msg.contains("my_strat"), "the refusal must name the artifact: {msg}");
            assert!(msg.to_lowercase().contains("build"), "{msg}");
            assert!(
                !msg.contains("no sha"),
                "a real sha must not trip the emptiness refusal: {msg}"
            );
        }
        Ok(_) => panic!("no artifact exists at that path, so nothing can load"),
        Err(other) => panic!("wrong RunError variant: {other:?}"),
    }
}

/// [`plugins_dir`] resolves the PROJECT's `user_data/plugins`, which is where the builder
/// service's artifacts land and therefore the only place the host may look.
///
/// ⚠ **The leaf assertion alone could not fail for its stated reason**, and that is what this
/// test used to be: both branches of [`plugins_dir`] end in `user_data/plugins`, so a WALK
/// that resolved nothing — the fallback, a bare relative path — passed it identically to a
/// walk that found the project. The distinguishing property is ABSOLUTENESS plus the PROJECT
/// ROOT: only the resolved branch answers an absolute path under the project this test runs
/// in. Both are asserted, and the sibling below drives the FALLBACK branch through the same
/// function so the two are shown to differ rather than assumed to.
#[test]
fn the_plugin_lookup_directory_is_the_resolved_projects_user_data_plugins() {
    let dir = plugins_dir();
    let rendered = dir.to_string_lossy().replace('\\', "/");
    assert!(rendered.ends_with("user_data/plugins"), "{rendered}");
    // The resolved branch, distinguished from the fallback: an ABSOLUTE path whose parent
    // chain is the project this test runs in. A `cargo test -p` runs with the CWD at the
    // crate directory, so a walk that found nothing would answer the bare relative literal.
    assert!(
        dir.is_absolute(),
        "the walk must have RESOLVED a project, not fallen through to the relative default: \
             {rendered}"
    );
    let cwd = std::env::current_dir().expect("cwd");
    assert!(
        cwd.starts_with(dir.parent().and_then(std::path::Path::parent).expect("<project>")),
        "the resolved project must be an ANCESTOR of the working directory: {rendered} vs {}",
        cwd.display()
    );
}

/// ...and BOTH branches of [`plugins_dir_from`], driven through the production function.
///
/// ⚠ **The fallback assertion used to sit behind an `if let Some(..)`**, so on any box where
/// the resolver answered — which is every box — it asserted nothing at all, and the sibling
/// above claimed the fallback was "exercised separately below" when it was exercised by
/// nothing. It cannot be made unconditional through the filesystem: the walk climbs to `/`
/// and no test can promise that no ancestor of a temp directory carries a project marker.
/// `None` is the one input that reaches the fallback on every box, which is why
/// [`plugins_dir_from`] takes the working directory as a parameter.
#[test]
fn both_branches_of_the_plugin_directory_resolution_are_reachable_and_differ() {
    // THE FALLBACK, unconditionally: no working directory, so nothing to walk from.
    let fallback = plugins_dir_from(None);
    assert_eq!(fallback, PathBuf::from("user_data").join("plugins"));
    assert!(!fallback.is_absolute(), "the fallback is the bare relative default: {fallback:?}");

    // THE RESOLVED BRANCH, over a planted project — an ABSOLUTE answer under that project.
    let tmp = tempfile::tempdir().expect("scratch");
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    std::fs::create_dir_all(a.join("settings")).expect("plant marker a");
    std::fs::create_dir_all(b.join("settings")).expect("plant marker b");
    let ra = plugins_dir_from(Some(&a));
    let rb = plugins_dir_from(Some(&b));
    assert_eq!(ra, a.join("user_data").join("plugins"));
    assert!(ra.is_absolute());

    // The two branches genuinely differ, and the resolver is a FUNCTION OF ITS START PATH:
    // two projects cannot answer one directory, which is what stops a run loading a plugin
    // from one project while writing its run into another.
    assert_ne!(ra, fallback, "the two branches must not produce the same answer");
    assert_ne!(ra, rb, "two projects must not share one plugin directory");
}

#[test]
fn native_walkforward_and_sweep_use_the_registry_too() {
    let (_dir, store, _b) = seeded_store();
    let spec =
        StrategySpec::native("buy_hold", params_from_rows(&[("symbol".into(), "BTCUSDT".into())]));
    let rep = run_walkforward_slice(&spec, &slice(), &store, 4).unwrap();
    assert_eq!(rep.windows.len(), 4);

    let grid = vec![("size".to_string(), vec![1.0, 2.0, 3.0])];
    let sw = run_paramscan_slice(&spec, &slice(), &store, &grid).unwrap();
    assert_eq!(sw.entries.len(), 3, "a native sweep overrides strategy.params per point");
}

// ---- tick slices -------------------------------------------------------------------------

fn tick_store() -> (tempfile::TempDir, StoreHandle) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let quotes: Vec<QuoteTick> = (0..200)
        .map(|i| QuoteTick {
            ts: 1_000 * (i as i64 + 1),
            local_ts: 1_000 * (i as i64 + 1),
            bid: 100.0 + (i % 5) as f64,
            ask: 100.5 + (i % 5) as f64,
            bid_size: 5.0,
            ask_size: 5.0,
            symbol: "TKN".to_string(),
        })
        .collect();
    let trades: Vec<TradeTick> = (0..200)
        .map(|i| TradeTick {
            ts: 1_000 * (i as i64 + 1),
            local_ts: 1_000 * (i as i64 + 1),
            price: 100.25 + (i % 5) as f64,
            size: 1.0,
            is_buyer_maker: i % 2 == 0,
            symbol: "TKN".to_string(),
        })
        .collect();
    store.append_quotes("polymarket", "TKN", &quotes, None).unwrap();
    store.append_trades("polymarket", "TKN", &trades, None).unwrap();
    (dir, Arc::new(store) as StoreHandle)
}

#[test]
fn tick_series_lists_each_venue_symbol_once_across_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let q = vec![QuoteTick {
        ts: 1,
        local_ts: 1,
        bid: 1.0,
        ask: 1.1,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "TKN".into(),
    }];
    let t = vec![TradeTick {
        ts: 1,
        local_ts: 1,
        price: 1.05,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "TKN".into(),
    }];
    store.append_quotes("polymarket", "TKN", &q, None).unwrap();
    store.append_trades("polymarket", "TKN", &t, None).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar_at(0)], None).unwrap();

    let ticks = tick_series(&store).unwrap();
    assert_eq!(ticks, vec![("polymarket".to_string(), "TKN".to_string())]);
    // and the bar filter still sees only the bar series
    assert_eq!(
        bar_series(&store).unwrap(),
        vec![("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string())]
    );
}

/// One conflating-L2 snapshot, the shape `LiveDataSink::l2_snapshot` records.
fn depth_snapshot(ts: i64, symbol: &str) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: ts,
        seq: 0,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![BookLevel::new(100.0, 1.0)],
        asks: vec![BookLevel::new(100.5, 1.0)],
        symbol: symbol.to_string(),
    }
}

/// **The instrument that used to vanish.** Recorded through `subscribe_depth` alone, it is a
/// real series on disk that `tick_series` cannot replay — and it appeared in NO list, so the
/// Studio showed the same nothing for "not recorded" and for "recorded, unrunnable".
/// [`depth_only_series`] is that instrument's one appearance.
#[test]
fn a_depth_only_instrument_is_not_replayable_but_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_depth("binance", "BTCUSDT", &[depth_snapshot(1, "BTCUSDT")], None).unwrap();

    assert!(
        tick_series(&store).unwrap().is_empty(),
        "depth is the CONFLATING lane — replay_ticks reads no depth, so no runnable row exists"
    );
    assert_eq!(
        depth_only_series(&store).unwrap(),
        vec![("binance".to_string(), "BTCUSDT".to_string())],
        "...and the instrument is NAMED rather than dropped in silence"
    );
}

/// The complement: an instrument with a REPLAYABLE lane is a tick row, so its depth series is
/// not a second entry anywhere. The disclosure reports what the filter COSTS, never a second
/// opinion about an instrument the picker already lists.
#[test]
fn depth_beside_a_replayable_lane_is_not_reported_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let t = vec![TradeTick {
        ts: 1,
        local_ts: 1,
        price: 100.25,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "BTCUSDT".into(),
    }];
    store.append_trades("binance", "BTCUSDT", &t, None).unwrap();
    store.append_depth("binance", "BTCUSDT", &[depth_snapshot(1, "BTCUSDT")], None).unwrap();

    assert_eq!(
        tick_series(&store).unwrap(),
        vec![("binance".to_string(), "BTCUSDT".to_string())],
        "the trade tape makes it runnable"
    );
    assert!(depth_only_series(&store).unwrap().is_empty(), "already listed — nothing is lost");
}

/// THE equality pin of the one-call fold: [`series_lists`] answers exactly what the three
/// separate enumerations answer, over a store holding all three shapes at once (a bar series,
/// a replayable tick lane, and a depth-only instrument). Without a fixture carrying all three
/// the assertion would pass on empty vectors and prove nothing.
#[test]
fn series_lists_equals_the_three_separate_enumerations() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar_at(0), bar_at(60_000)], None).unwrap();
    let t = vec![TradeTick {
        ts: 1,
        local_ts: 1,
        price: 100.25,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "ETHUSDT".into(),
    }];
    store.append_trades("binance", "ETHUSDT", &t, None).unwrap();
    store.append_depth("okx", "SOL-USDT", &[depth_snapshot(1, "SOL-USDT")], None).unwrap();

    let one = series_lists(&store).unwrap();
    assert_eq!(one.bars, bar_series(&store).unwrap());
    assert_eq!(one.ticks, tick_series(&store).unwrap());
    assert_eq!(one.depth_only, depth_only_series(&store).unwrap());
    // ...and the fixture actually exercises all three lists, so the equality above is not
    // three comparisons of empty vectors.
    assert_eq!(one.bars.len(), 1);
    assert_eq!(one.ticks, vec![("binance".to_string(), "ETHUSDT".to_string())]);
    assert_eq!(one.depth_only, vec![("okx".to_string(), "SOL-USDT".to_string())]);
}

/// The two sets are complements over the tick lanes, not two independently maintained lists:
/// every `kind=` either replays or is the one disclosed exclusion, and nothing is both.
#[test]
fn the_replayable_and_disclosed_tick_kinds_do_not_overlap() {
    assert!(
        !REPLAYABLE_TICK_KINDS.contains(&UNREPLAYABLE_TICK_KIND),
        "a kind cannot both replay and be disclosed as unreplayable"
    );
}

fn bar_at(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A `SliceKind::Ticks` slice routes to `replay_ticks` (NOT `load_bars`): the store below has
/// no bar series at all, so a bar-path run could not possibly produce this result.
#[test]
fn tick_slice_replays_recorded_ticks() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TsRange::all());
    let spec =
        StrategySpec::native("buy_hold", params_from_rows(&[("symbol".into(), "TKN".into())]));
    let res =
        run_slice(&spec, &slice, &store, EngineParams::default()).expect("tick replay should run");
    assert!(!res.equity_curve.is_empty(), "the tick replay stamps an equity curve");
}

#[test]
fn tick_slice_with_no_recorded_ticks_is_a_data_error() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["MISSING".to_string()], TsRange::all());
    let out = run_slice(&rhai("fn on_bar() {}"), &slice, &store, EngineParams::default());
    assert_matches!(out, Err(RunError::Data(_)), "got {out:?}");
}

#[test]
fn a_bar_only_runner_rejects_a_tick_slice_instead_of_panicking() {
    let (_dir, store) = tick_store();
    let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TsRange::all());
    assert_matches!(
        run_walkforward_slice(&rhai(SCRIPT), &slice, &store, 4),
        Err(RunError::Data(_))
    );
}

// ---- multi-symbol ------------------------------------------------------------------------

#[test]
fn multi_symbol_bar_slice_registers_every_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let mk = |sym: &str, base: f64| -> Vec<Bar> {
        (0..50)
            .map(|i| {
                let c = base + (i % 7) as f64;
                Bar {
                    symbol: Some(sym.to_string()),
                    open: c,
                    high: c,
                    low: c,
                    close: c,
                    ..bar_at(60_000 * (i as i64 + 1))
                }
            })
            .collect()
    };
    store.append_bars("binance", "BTCUSDT", "1m", &mk("BTCUSDT", 100.0), None).unwrap();
    store.append_bars("binance", "ETHUSDT", "1m", &mk("ETHUSDT", 50.0), None).unwrap();
    let store: StoreHandle = Arc::new(store);

    let slice = DataSlice::multi_bars(
        "binance",
        vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        "1m",
        TsRange::all(),
    );
    let series = load_slice_bars(&slice, &store).unwrap();
    assert_eq!(series.len(), 2);
    assert_eq!(series[0].0, "BTCUSDT");
    assert_eq!(series[1].0, "ETHUSDT");

    let res = run_slice(
        &StrategySpec::native_default("rotation_top_k"),
        &slice,
        &store,
        EngineParams::default(),
    )
    .expect("a two-symbol run should backtest");
    assert!(!res.equity_curve.is_empty());
}

/// Misaligned per-symbol bar counts are `StrategyEngine::new`'s own assert — surfaced as a
/// `RunError` here rather than panicking the worker thread.
#[test]
fn misaligned_multi_symbol_slice_is_a_data_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let bars_a: Vec<Bar> = (0..20).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
    let bars_b: Vec<Bar> = (0..5).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
    store.append_bars("binance", "AAA", "1m", &bars_a, None).unwrap();
    store.append_bars("binance", "BBB", "1m", &bars_b, None).unwrap();
    let store: StoreHandle = Arc::new(store);

    let slice = DataSlice::multi_bars(
        "binance",
        vec!["AAA".to_string(), "BBB".to_string()],
        "1m",
        TsRange::all(),
    );
    assert_matches!(load_slice_bars(&slice, &store), Err(RunError::Data(_)));
}

#[test]
fn slice_label_and_symbol_accessor() {
    let s = DataSlice::bars("binance", "BTCUSDT", "1m", TsRange::all());
    assert_eq!(s.symbol(), "BTCUSDT");
    assert_eq!(s.label(), "binance · BTCUSDT · 1m");
    let t = DataSlice::ticks("polymarket", vec!["A".into(), "B".into()], TsRange::all());
    assert_eq!(t.label(), "polymarket · A+1 · ticks");
}

// ---- compare / walk-forward / sweep ------------------------------------------------------

const NOOP_SCRIPT: &str = "fn on_bar() {}";

#[test]
fn compare_all_slice_runs_every_strategy_and_carries_names() {
    let (_dir, store, _b) = seeded_store();
    let strategies =
        vec![("noop".to_string(), rhai(NOOP_SCRIPT)), ("cross".to_string(), rhai(SCRIPT))];
    let results = compare_all_slice(&strategies, &slice(), &store);
    assert_eq!(results.len(), 2);
    let noop = results.iter().find(|(n, _)| n == "noop").unwrap();
    assert_eq!(noop.1.as_ref().unwrap().n_trades, 0);
    let cross = results.iter().find(|(n, _)| n == "cross").unwrap();
    assert!(cross.1.as_ref().unwrap().n_trades > 0);
}

/// A Rhai row and a NATIVE row compare side by side in one pass — the compare pane's widening.
#[test]
fn compare_all_slice_mixes_rhai_and_native_rows() {
    let (_dir, store, _b) = seeded_store();
    let strategies = vec![
        ("cross".to_string(), rhai(SCRIPT)),
        (
            "hold".to_string(),
            StrategySpec::native(
                "buy_hold",
                params_from_rows(&[("symbol".into(), "BTCUSDT".into())]),
            ),
        ),
        ("nope".to_string(), StrategySpec::native_default("not_a_strategy")),
    ];
    let results = compare_all_slice(&strategies, &slice(), &store);
    assert_eq!(results.len(), 3);
    assert!(results.iter().find(|(n, _)| n == "cross").unwrap().1.is_ok());
    assert!(results.iter().find(|(n, _)| n == "hold").unwrap().1.is_ok());
    let bad = results.iter().find(|(n, _)| n == "nope").unwrap();
    assert!(bad.1.is_err(), "an unknown native name surfaces as a failed row, not a panic");
}

#[test]
fn compare_all_slice_stringifies_a_per_strategy_failure_without_dropping_the_row() {
    let (_dir, store, _b) = seeded_store();
    let strategies =
        vec![("broken".to_string(), rhai("fn on_bar( {")), ("ok".to_string(), rhai(NOOP_SCRIPT))];
    let results = compare_all_slice(&strategies, &slice(), &store);
    assert_eq!(results.len(), 2, "a failing strategy stays in the output, not dropped");
    let broken = results.iter().find(|(n, _)| n == "broken").unwrap();
    assert!(broken.1.is_err());
    let ok = results.iter().find(|(n, _)| n == "ok").unwrap();
    assert!(ok.1.is_ok());
}

#[test]
fn spawn_compare_all_delivers_the_same_outcome_as_the_sync_call() {
    let (_dir, store, _b) = seeded_store();
    let strategies = vec![("cross".to_string(), rhai(SCRIPT))];
    let rx = spawn_compare_all(strategies.clone(), slice(), store.clone());
    let got = rx.recv().expect("worker sends one outcome");
    let want = compare_all_slice(&strategies, &slice(), &store);
    assert_eq!(got.len(), want.len());
    assert_eq!(got[0].0, want[0].0);
    assert_eq!(got[0].1.as_ref().unwrap().n_trades, want[0].1.as_ref().unwrap().n_trades);
}

#[test]
fn walkforward_slice_stitches_oos_windows() {
    let (_dir, store, _b) = seeded_store();
    let rep = run_walkforward_slice(&rhai(SCRIPT), &slice(), &store, 4).unwrap();
    assert_eq!(rep.windows.len(), 4);
    assert!(!rep.oos_equity_curve.is_empty());
    assert!((0.0..=1.0).contains(&rep.wf_consistency));
}

#[test]
fn walkforward_compile_error_maps_to_compile() {
    let (_dir, store, _b) = seeded_store();
    assert_matches!(
        run_walkforward_slice(&rhai("fn on_bar( {"), &slice(), &store, 4),
        Err(RunError::Compile(_))
    );
}

/// **The annualization bug.** `oos_sharpe` is scaled by `sqrt(periods_per_year)`, and that
/// factor must be the observation count the SLICE's own bar step produces — 252 · 1440 for the
/// 1m fixture — not the daily 252 this plane hardcoded while the harness plane derived its
/// own. The two doors disagreed by `sqrt(1440)` on exactly this interval, which is the one the
/// CI roundtrip fixture uses, and no test compared them.
#[test]
fn walkforward_annualizes_off_the_slice_interval_not_a_bare_252() {
    let (_dir, store, _b) = seeded_store();
    assert_eq!(slice().interval, "1m", "the fixture interval is what makes 1440 the ratio");
    let rep = run_walkforward_slice(&rhai(SCRIPT), &slice(), &store, 4).unwrap();

    // `oos_sharpe` IS `sharpe(oos_equity_curve, factor)` — both come out of the same stitch —
    // so recomputing the curve at each candidate factor says which one the run actually used,
    // bit for bit, with no tolerance to argue about.
    let at_slice = sharpe(&rep.oos_equity_curve, periods_per_year_for_interval("1m"));
    let at_daily = sharpe(&rep.oos_equity_curve, DAILY_PERIODS_PER_YEAR);
    assert_eq!(
        rep.oos_sharpe.to_bits(),
        at_slice.to_bits(),
        "a 1m slice must annualize at 252 * 1440, i.e. one observation per BAR"
    );
    assert!(
        at_daily != 0.0,
        "precondition: a flat OOS curve would make every comparison below vacuous"
    );
    assert_ne!(rep.oos_sharpe.to_bits(), at_daily.to_bits(), "...and NOT at the daily anchor");

    // The SIZE of the correction, pinned because it is what a reader needs in order to reason
    // about an `oos_sharpe` printed by this door BEFORE the fix.
    let ratio = rep.oos_sharpe / at_daily;
    assert!((ratio - 1440.0_f64.sqrt()).abs() < 1e-9, "expected ~37.9x, got {ratio}");
}

/// **The `n_splits` floor.** Zero windows is not an empty result, it is a run that never
/// happened wearing the shape of a strategy that made nothing — so it is refused rather than
/// reported. The second half drives `walk_forward_strategy` at zero directly, because a guard
/// is only worth having if the thing it prevents is real.
#[test]
fn zero_splits_is_refused_rather_than_reported_as_an_all_zeros_run() {
    let (_dir, store, bars) = seeded_store();
    let err = run_walkforward_slice(&rhai(SCRIPT), &slice(), &store, 0)
        .expect_err("n_splits = 0 must not produce a report");
    let RunError::Data(msg) = &err else { panic!("expected a Data refusal, got {err:?}") };
    assert!(msg.contains("n_splits"), "the refusal must name the parameter, got {msg:?}");

    // What the guard prevents. Both scalars are arbitrary here: with no splits the window
    // closure is never called, nothing is stitched, and `sharpe` of an empty curve is 0 at any
    // annualization.
    // ⚠ `|_, _|`, not `|_|`. This test arrived on one branch while the runner's closure grew a
    // second parameter (the TRAIN half) on another, and the mismatch was invisible to both:
    // each side compiled alone, and only the merged tree names it.
    let unguarded = walk_forward_strategy(&bars, 0, WalkMode::Anchored, 1_000.0, 1.0, |_, _| {
        unreachable!("zero splits means the window closure is never called")
    });
    assert!(unguarded.windows.is_empty(), "no windows");
    assert!(unguarded.oos_equity_curve.is_empty(), "nothing stitched");
    assert_eq!(unguarded.oos_return, 0.0, "reads as a strategy that made nothing");
    assert_eq!(unguarded.oos_sharpe, 0.0, "...at a Sharpe of zero");
    assert_eq!(unguarded.wf_consistency, 0.0, "...over `windows.len().max(1)` windows");
}

const SWEEP_SCRIPT: &str = r#"
const QTY = 1.0;
let fast = param("fast", 5.0);
fn on_bar() {
    let f = sma(fast.to_int()); let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

#[test]
fn sweep_slice_ranks_and_deflates() {
    let (_dir, store, _b) = seeded_store();
    let grid = vec![("fast".to_string(), vec![3.0, 5.0, 8.0])];
    let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
    assert_eq!(sw.entries.len(), 3, "one entry per grid point");
    assert!((0.0..=1.0).contains(&sw.dsr));
    assert_eq!(sw.best_index, 0);
    // ranked best-first by annualized sharpe (non-increasing)
    let s: Vec<f64> = sw
        .entries
        .iter()
        .map(|e| vike_analytics::metrics::sharpe(&e.result.equity_curve, 252.0))
        .collect();
    for w in s.windows(2) {
        if w[0].is_nan() || w[1].is_nan() {
            continue;
        }
        assert!(w[0] >= w[1]);
    }
}

/// DETERMINISM GATE for the bounded-pool sweep: the SAME grid run twice must produce the same
/// entries in the same rank order with BIT-identical equity, no matter which point finished
/// first. `map_bounded` reassembles by index and the ranking sort is stable, so ordering can
/// never depend on scheduling.
#[test]
fn parallel_sweep_is_deterministic_across_repeated_runs() {
    let (_dir, store, _b) = seeded_store();
    // Enough points that the bounded pool really interleaves them.
    let grid = vec![("fast".to_string(), vec![2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0])];
    let a = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
    let b = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
    assert_eq!(a.entries.len(), 8);
    assert_eq!(a.entries.len(), b.entries.len());
    for (x, y) in a.entries.iter().zip(&b.entries) {
        assert_eq!(x.overrides, y.overrides, "same point at the same rank");
        assert_eq!(
            x.result.final_equity.to_bits(),
            y.result.final_equity.to_bits(),
            "bit-identical equity across runs"
        );
    }
    assert_eq!(a.dsr.to_bits(), b.dsr.to_bits());
}

fn entry(curve: Vec<f64>) -> ParamscanEntry {
    let last = *curve.last().unwrap();
    ParamscanEntry {
        overrides: vec![],
        result: BacktestResult { equity_curve: curve, final_equity: last, ..Default::default() },
    }
}

/// Regression (mirrors the harness sweep's `nan_sharpe_ranks_last_never_first`): a NaN-Sharpe
/// entry constructed FIRST must rank LAST — with the old
/// `partial_cmp(..).unwrap_or(Equal)` sort it compared Equal to every finite point and,
/// grid-placed first, stayed at rank #1 (= `best_index` 0, feeding the DSR).
#[test]
fn nan_sharpe_entry_ranks_last_never_best_index() {
    // A NaN in the equity curve (a blown-up strategy) poisons mean/var -> NaN sharpe.
    let degenerate = entry(vec![1000.0, f64::NAN, 1000.0, 1000.0]);
    let good = entry(vec![1000.0, 1010.0, 1005.0, 1030.0]);
    assert!(sharpe(&degenerate.result.equity_curve, 252.0).is_nan(), "precondition");
    assert!(sharpe(&good.result.equity_curve, 252.0).is_finite(), "precondition");

    // NaN entry deliberately placed first, so a broken comparator would leave it ranked #1.
    let mut entries = vec![degenerate, good];
    rank_entries_by_sharpe(&mut entries);
    assert!(
        sharpe(&entries[0].result.equity_curve, 252.0).is_finite(),
        "finite-Sharpe point is best_index 0"
    );
    assert!(
        sharpe(&entries[1].result.equity_curve, 252.0).is_nan(),
        "NaN-Sharpe point ranks last, not first"
    );
}

/// [`rank_entries_by_sharpe`]'s annualization factor is ORDERING-INERT, which is the whole
/// reason it stays [`DAILY_PERIODS_PER_YEAR`] while [`run_walkforward_slice_with_params`]
/// derives its own from the slice. `metrics::sharpe` multiplies by `sqrt(periods_per_year)` —
/// ONE uniform positive scale applied to every entry — so no factor can reorder the ranking,
/// and a NaN score stays NaN under all of them. Gated here so that doc comment is not a claim
/// nothing checks, and so a later reader cannot derive the value and believe something moved.
#[test]
fn ranking_order_is_invariant_to_the_annualization_factor() {
    // `entry` stamps `final_equity` from the curve's last point, and these four differ, so the
    // equity bits identify WHICH entry landed at each rank.
    let curves = [
        vec![1000.0, 1010.0, 1005.0, 1030.0],
        vec![1000.0, 990.0, 1002.0, 995.0],
        vec![1000.0, f64::NAN, 1000.0, 1000.0],
        vec![1000.0, 1001.0, 1002.0, 1004.0],
    ];
    let order_at = |ppy: f64| -> Vec<u64> {
        let mut entries: Vec<ParamscanEntry> = curves.iter().cloned().map(entry).collect();
        entries.sort_by(|a, b| {
            cmp_scores_desc(
                sharpe(&a.result.equity_curve, ppy),
                sharpe(&b.result.equity_curve, ppy),
            )
        });
        entries.iter().map(|e| e.result.final_equity.to_bits()).collect()
    };

    let daily = order_at(DAILY_PERIODS_PER_YEAR);
    for interval in ["1m", "5m", "1h", "1d", "7d"] {
        assert_eq!(
            order_at(periods_per_year_for_interval(interval)),
            daily,
            "the {interval} factor must not reorder a sweep"
        );
    }

    // ...and the SHIPPED ranker produces exactly that order, with the NaN point still last.
    let mut entries: Vec<ParamscanEntry> = curves.iter().cloned().map(entry).collect();
    rank_entries_by_sharpe(&mut entries);
    let ranked: Vec<u64> = entries.iter().map(|e| e.result.final_equity.to_bits()).collect();
    assert_eq!(ranked, daily, "the shipped ranker agrees with the scale-free order");
    let last = &entries.last().expect("four entries").result.equity_curve;
    assert!(sharpe(last, DAILY_PERIODS_PER_YEAR).is_nan(), "the NaN point still sorts last");
}

#[test]
fn trial_columns_are_per_trial_returns_and_transpose_is_consistent() {
    let entries =
        vec![entry(vec![100.0, 110.0, 121.0, 133.0]), entry(vec![100.0, 90.0, 99.0, 108.0])];
    let (cols, t) = trial_columns(&entries);
    assert_eq!(cols.len(), 2, "N columns");
    assert!(cols.iter().all(|c| c.len() == t), "each column length T");
    for (j, e) in entries.iter().enumerate() {
        let r = vike_analytics::metrics::returns(&e.result.equity_curve);
        assert_eq!(cols[j], r[..t].to_vec(), "column j is trial j's returns");
    }
    let m = transpose(&cols, t);
    assert_eq!(m.len(), t, "matrix has T rows");
    assert!(m.iter().all(|row| row.len() == 2), "each row width N");
    for ti in 0..t {
        for j in 0..2 {
            assert_eq!(m[ti][j], cols[j][ti], "matrix[t][j] == columns[j][t]");
        }
    }
}

#[test]
fn sweep_populates_pbo_for_a_multi_point_grid() {
    let (_dir, store, _b) = seeded_store();
    let grid = vec![("fast".to_string(), vec![3.0, 5.0, 8.0])];
    let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
    assert!(
        (0.0..=1.0).contains(&sw.pbo) || sw.pbo.is_nan(),
        "pbo in [0,1] or NaN, got {}",
        sw.pbo
    );
}

#[test]
fn sweep_pbo_is_nan_for_a_single_point_grid() {
    let (_dir, store, _b) = seeded_store();
    let grid = vec![("fast".to_string(), vec![5.0])];
    let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
    assert!(sw.pbo.is_nan(), "single trial -> PBO not assessable, got {}", sw.pbo);
}

#[test]
fn sweep_dsr_is_effective_n_deflated() {
    let (_dir, store, _b) = seeded_store();
    // closely-spaced fast values -> strongly correlated equity curves -> effective_n < N,
    // so the effective-N DSR differs from the plain deflated_sharpe_ratio on the same inputs.
    let grid = vec![("fast".to_string(), vec![3.0, 4.0, 5.0, 6.0])];
    let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();

    let per_obs = |r: &BacktestResult| {
        vike_analytics::metrics::sharpe(&r.equity_curve, 252.0) / 252.0_f64.sqrt()
    };
    let trials: Vec<f64> = sw.entries.iter().map(|e| per_obs(&e.result)).collect();
    let best = &sw.entries[0].result;
    let n = vike_analytics::metrics::returns(&best.equity_curve).len();
    let skew = vike_analytics::metrics::returns_skewness(&best.equity_curve);
    let kurt = vike_analytics::metrics::returns_kurtosis(&best.equity_curve) + 3.0;
    let plain =
        vike_analytics::overfit::deflated_sharpe_ratio(per_obs(best), &trials, n, skew, kurt);

    assert!(
        (sw.dsr - plain).abs() > 1e-9,
        "effective-N DSR ({}) should differ from plain DSR ({})",
        sw.dsr,
        plain
    );
}

#[test]
fn pbo_discriminates_overfit_from_stable() {
    // Hand-built per-trial columns (length 48 >= N_SPLITS) fed straight through the transpose
    // into pbo_cscv — asserts the ORDERING (oracle-parity retired; we port the math), decoupled
    // from any strategy that might not reliably overfit the RNG seed.
    let t = 48usize;
    let const_col = |v: f64| vec![v; t];
    let half_col = |first: f64, second: f64| -> Vec<f64> {
        (0..t).map(|i| if i < t / 2 { first } else { second }).collect()
    };
    // Stable: one column dominates in every row -> IS-best is always OOS-best -> PBO ~ 0.
    let stable = vec![const_col(1.0), const_col(0.0), const_col(0.0), const_col(0.0)];
    // Overfit: the IS-best flips to OOS-worst across the timeline halves -> high PBO.
    let overfit = vec![half_col(1.0, 0.0), half_col(0.0, 1.0), const_col(0.5), const_col(0.5)];
    let stable_pbo = vike_analytics::overfit::pbo_cscv(&transpose(&stable, t), N_SPLITS);
    let overfit_pbo = vike_analytics::overfit::pbo_cscv(&transpose(&overfit, t), N_SPLITS);
    assert!(overfit_pbo > 0.0, "overfit matrix should register some overfit, got {overfit_pbo}");
    assert!(
        overfit_pbo > stable_pbo,
        "overfit PBO {overfit_pbo} should exceed stable PBO {stable_pbo}"
    );
}

/// split-plane B12: `bar_series`/`tick_series` are TRAIT enumerations — they must answer over
/// a [`StoreHandle`] whose backing store is not a `DataFusionHist` at all (the production case
/// is the RPC-backed `RemoteHistStore`; here the smallest seeded fake stands in, because
/// `vike_data::MemHistStore` can only ever list the seams it stores for real —
/// it cannot hold the `kind=bar` series this test needs). Every verb except `list_series`
/// refuses, so the test also PROVES the two
/// functions touch nothing but the catalog verb.
#[test]
fn series_enumeration_needs_only_the_trait() {
    use vike_data::{SeriesCoverage, SeriesId};

    /// `list_series` answers from the seeded list; every other verb is a hard error.
    struct CatalogOnlyStore(Vec<SeriesId>);
    fn refuse(method: &str) -> DataError {
        DataError::Query(format!("CatalogOnlyStore: {method} must not be called"))
    }
    impl HistStore for CatalogOnlyStore {
        fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
            Ok(self.0.clone())
        }
        fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
            Err(refuse("inventory"))
        }
        vike_data::hist_store_stubs!(refuse(refuse): all);
    }

    let store: StoreHandle = Arc::new(CatalogOnlyStore(vec![
        SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".to_string())),
        SeriesId::per_symbol("quote", "polymarket", "TKN", None),
        SeriesId::per_symbol("trade", "polymarket", "TKN", None),
    ]));

    assert_eq!(
        bar_series(store.as_ref()).unwrap(),
        vec![("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string())]
    );
    // quote + trade for the same (venue, symbol) still dedupe to ONE runnable tick row.
    assert_eq!(
        tick_series(store.as_ref()).unwrap(),
        vec![("polymarket".to_string(), "TKN".to_string())]
    );
}
