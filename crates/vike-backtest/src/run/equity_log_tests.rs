//! A bar run's event log over a real store: the rebuild from the store's bars is the run's curve to the bit, and every
//! refusal [`rebuild_curve`]'s doc lists answers in words; a tick run keeps its curve.

use super::*;
use crate::run::run_backtest_logged;
use vike_marketdata::test_support::quote;
use vike_store::HistoryStoreDataFusion;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// A one-symbol 1-minute profile over [`seeded`]'s tape, run by a strategy that opens and closes trades on it.
fn toml(kind: &str) -> String {
    format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "{kind}"
interval = "1m"
from = "0"
to = "100000000"

[engine]
cash = 1000.0

[strategy]
file = "trend/momentum.rs"
[strategy.params]
qty = 1.0
time_limit_ms = 180000
venue = "{VENUE}"
commission = 0.1
"#
    )
}

/// 400 deterministic oscillating bars, a minute apart, every close carrying all its digits.
fn tape() -> Vec<Bar> {
    tape_from(0x1234_5678)
}

/// [`tape`]'s walk from another `seed`: another series of the same shape.
fn tape_from(mut seed: u64) -> Vec<Bar> {
    let mut px = 100.0f64;
    (0..400)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            px = (px + ((seed >> 32) as f64 / f64::from(u32::MAX) - 0.5) * 2.0).max(1.0);
            Bar::ohlcv(60_000 * (i + 1), px, px, px, px, 1.0)
        })
        .collect()
}

fn seeded() -> (tempfile::TempDir, Arc<dyn HistoryStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    store.append_bars(VENUE, SYMBOL, "1m", &tape(), None).unwrap();
    (dir, Arc::new(store))
}

fn bits(curve: &[f64]) -> Vec<u64> {
    curve.iter().map(|e| e.to_bits()).collect()
}

/// The run of profile `text`, its log stamped as the saver stamps it (build `"aaaaaaa"`).
fn logged_of(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    text: &str,
) -> (TradingRecord, EquityLog) {
    let profile =
        ProfileBacktest::from_toml_str(text).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let (result, log) = run_backtest_logged(&profile, Arc::clone(store), None).unwrap();
    let mut log = log.expect("a bar run over a store logs");
    assert!(stamp(&mut log.recipe, store.as_ref(), Some("aaaaaaa")), "the store pins every bar");
    (result, log)
}

/// [`logged_of`] the bar profile.
fn logged(store: &Arc<dyn HistoryStore + Send + Sync>) -> (TradingRecord, EquityLog) {
    logged_of(store, &toml("bar"))
}

#[test]
fn a_bar_runs_log_rebuilds_its_curve_from_the_stored_bars_to_the_bit() {
    let (_dir, store) = seeded();
    let (result, log) = logged(&store);
    let events = decode_ledger_events(&log.events).unwrap();
    assert!(events.len() > 1, "the run must fill, or the log proves nothing: {events:?}");
    let recipe = &log.recipe;
    assert_eq!((recipe.format, recipe.steps, recipe.build.as_deref()), (1, 400, Some("aaaaaaa")));
    assert_eq!(
        (recipe.cash, recipe.currency.as_str(), recipe.decide.as_str()),
        (1000.0, "USD", "sequential")
    );
    assert_eq!(recipe.symbols, [SYMBOL]);
    assert_eq!(recipe.values, [ContractValue::linear(1.0)]);
    assert!(
        recipe.series[0].days.as_ref().is_some_and(|d| !d.is_empty()),
        "the store stamps its days"
    );
    // What the manifest keeps reads back to the same recipe, every number to the bit.
    let json = serde_json::to_string(recipe).unwrap();
    assert_eq!(&serde_json::from_str::<EquityLogRecipe>(&json).unwrap(), recipe);

    let (equity, timestamp) =
        rebuild_curve(&toml("bar"), recipe, &log.events, &store, Some("aaaaaaa")).unwrap();
    assert_eq!(bits(&equity), bits(&result.equity_curve), "the rebuilt curve, to the bit");
    assert_eq!(timestamp, result.equity_timestamp);
    println!(
        "SIZE: {} steps, {} log bytes, {} curve bytes",
        recipe.steps,
        log.events.len(),
        24 + 16 * recipe.steps
    );
}

#[test]
fn another_build_serves_the_same_curve_and_refuses_another_one() {
    let (_dir, store) = seeded();
    let (result, log) = logged(&store);
    let rebuilt = rebuild_curve(&toml("bar"), &log.recipe, &log.events, &store, Some("bbbbbbb"));
    assert_eq!(bits(&rebuilt.unwrap().0), bits(&result.equity_curve), "the fold did not move");

    let moved = EquityLogRecipe { curve_fnv: "0000000000000000".into(), ..log.recipe.clone() };
    let why =
        rebuild_curve(&toml("bar"), &moved, &log.events, &store, Some("bbbbbbb")).unwrap_err();
    assert!(why.contains("saved by build aaaaaaa") && why.contains("bbbbbbb"), "{why}");
    let why =
        rebuild_curve(&toml("bar"), &moved, &log.events, &store, Some("aaaaaaa")).unwrap_err();
    assert!(why.contains("differs from the one the run answered"), "{why}");

    let other = EquityLogRecipe { format: 2, ..log.recipe.clone() };
    let why = rebuild_curve(&toml("bar"), &other, &log.events, &store, None).unwrap_err();
    assert!(why.contains("format 2"), "{why}");
    let why =
        rebuild_curve(&toml("bar"), &log.recipe, &log.events[..20], &store, None).unwrap_err();
    assert!(why.contains("cut short"), "{why}");
}

/// Buy `size` every tenth bar and sell it five bars later.
const ROUND_TRIPS: &str = r#"
let size = param("size", 1.0);
fn on_bar(strategy) {
    let i = index();
    if i % 10 == 0 { strategy.order("in", OrderSide::Long).size(size); }
    if i % 10 == 5 { strategy.order("out", OrderSide::Short).size(size); }
}"#;

/// **A portfolio whose mounts list its slots in another order than `[data]` rebuilds its curve to the bit**: the
/// mounts name ETHUSDT then BTCUSDT while `[data]` lists BTCUSDT first, so the engine holds the slots in mount order
/// (`ProfileBacktest::slot_series`); the recipe records them so, each series beside its own bars, and the rebuild reads
/// them so.
#[test]
fn a_portfolio_whose_mounts_reorder_its_slots_rebuilds_its_curve_to_the_bit() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    store.append_bars(VENUE, SYMBOL, "1m", &tape(), None).unwrap();
    store.append_bars(VENUE, "ETHUSDT", "1m", &tape_from(0x9abc_def0), None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    // `[data]` lists BTCUSDT first, the mounts ETHUSDT first.
    let both = toml("bar").replace("[\"BTCUSDT\"]", "[\"BTCUSDT\", \"ETHUSDT\"]");
    let head = both.split("[strategy]").next().unwrap();
    let text = format!(
        "{head}[[mounts]]\nsymbols = [\"ETHUSDT\"]\n[mounts.strategy]\nfile = \"test/eth.rhai\"\n\
         [mounts.strategy.params]\nsize = 2.0\n\n[[mounts]]\nsymbols = [\"BTCUSDT\"]\n\
         [mounts.strategy]\nfile = \"test/btc.rhai\"\n"
    );
    let mut profile = ProfileBacktest::from_toml_str(&text).unwrap();
    for strategy in profile.strategies_mut() {
        strategy.source = Some(crate::run::StrategySource::of(ROUND_TRIPS.to_string()));
    }
    let (result, log) = run_backtest_logged(&profile, Arc::clone(&store), None).unwrap();
    let mut log = log.expect("a portfolio's bar run over a store logs");
    assert!(stamp(&mut log.recipe, store.as_ref(), Some("aaaaaaa")), "the store pins every bar");
    let events = decode_ledger_events(&log.events).unwrap();
    for si in 0..2 {
        assert!(events.iter().any(|e| e.si == si), "slot {si} must fill: {events:?}");
    }
    assert_eq!(log.recipe.symbols, ["ETHUSDT", SYMBOL], "the engine's slots, in mount order");
    let named: Vec<&str> = log.recipe.series.iter().map(|s| s.symbol.as_str()).collect();
    assert_eq!(named, ["ETHUSDT", SYMBOL], "each series recorded beside its own bars");

    let (equity, timestamp) =
        rebuild_curve(&text, &log.recipe, &log.events, &store, Some("aaaaaaa")).unwrap();
    assert_eq!(bits(&equity), bits(&result.equity_curve), "the rebuilt curve, to the bit");
    assert_eq!(timestamp, result.equity_timestamp);
}

/// A bar rewritten in the window (the store answers the newest write) changes the closes the run marked and the day's
/// stamp: refused, naming the series and the restamped day. Split rows other than the ones the run applied: refused.
#[test]
fn changed_bars_or_splits_are_refused_in_words() {
    let (_dir, store) = seeded();
    let (_, log) = logged(&store);
    let splits = EquityLogSeries {
        splits: vec![StockSplit::new(VENUE, SYMBOL, "1970-01-02", 2.0).unwrap()],
        ..log.recipe.series[0].clone()
    };
    let split = EquityLogRecipe { series: vec![splits], ..log.recipe.clone() };
    let why =
        rebuild_curve(&toml("bar"), &split, &log.events, &store, Some("aaaaaaa")).unwrap_err();
    assert!(why.contains("share splits of binance BTCUSDT 1m changed"), "{why}");

    let mut bar = tape()[100].clone();
    bar.close += 0.5;
    store.append_bars(VENUE, SYMBOL, "1m", &[bar], None).unwrap();
    let why =
        rebuild_curve(&toml("bar"), &log.recipe, &log.events, &store, Some("aaaaaaa")).unwrap_err();
    assert!(
        why.contains("stored bars of binance BTCUSDT 1m are not the ones the run marked"),
        "{why}"
    );
    assert!(why.contains("restamped binance BTCUSDT 1m on 1 day(s) from 1970-01-01"), "{why}");
}

/// **A profile that names a file is refused before anything is read**: the rebuild runs in the serving daemon over a
/// profile a sandboxed child wrote, so `data.file` and `[engine.resolution] path` naming a file that EXISTS (this
/// crate's manifest, which a CSV read would refuse in its own words) answer the one refusal, and no read of the file.
#[test]
fn a_profile_that_names_a_file_is_refused_before_it_is_read() {
    let (_dir, store) = seeded();
    let (_, log) = logged(&store);
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    assert!(std::path::Path::new(path).is_file(), "the premise: the named file exists");
    let file = toml("bar")
        .replace("interval = \"1m\"\n", &format!("interval = \"1m\"\nfile = {path:?}\n"));
    let resolution = toml("bar").replace(
        "[strategy]\n",
        &format!(
            "[engine.resolution]\nkind = \"binary_outcome\"\nwindow_secs = 300\npath = {path:?}\n\n\
             [strategy]\n"
        ),
    );
    for (key, text) in [("`data.file`", file), ("`[engine.resolution] path`", resolution)] {
        ProfileBacktest::from_toml_str(&text).expect("the profile itself reads");
        let why =
            rebuild_curve(&text, &log.recipe, &log.events, &store, Some("aaaaaaa")).unwrap_err();
        // The refusal alone, not a read of the file reported in other words.
        assert!(why.starts_with("its profile names the file"), "{why}");
        assert!(why.contains(key) && why.contains("never rebuilt from a file"), "{why}");
    }
}

/// A tick run's curve is sampled per event, not per bar: it answers no log, and its saver keeps the curve.
#[test]
fn a_tick_run_answers_no_log() {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    store
        .append_quotes(VENUE, SYMBOL, &[quote(0, 10.0, 11.0), quote(1000, 12.0, 13.0)], None)
        .unwrap();
    let tick = toml("tick").replace("1m", "1d");
    let profile =
        ProfileBacktest::from_toml_str(&tick).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    let (_, log) = run_backtest_logged(&profile, Arc::new(store), None).unwrap();
    assert!(log.is_none());
}

/// **A window open at its end** (`data.to` empty, "from X to now") reads every bar the store holds, so a live series
/// that collected more bars since, or a backfill before the first, is NO change to the run: the rebuild cuts the window
/// to the bars the run marked and answers its curve to the bit. A bar rewritten INSIDE them is still refused.
#[test]
fn an_open_ended_run_rebuilds_after_the_store_grew_around_it() {
    let (_dir, store) = seeded();
    let open = toml("bar").replace("to = \"100000000\"", "to = \"\"");
    assert_ne!(open, toml("bar"), "the fixture's text moved");
    let (result, log) = logged_of(&store, &open);
    let series = &log.recipe.series[0];
    assert_eq!((series.to, series.first_bar, series.last_bar), (None, 60_000, 60_000 * 400));

    let later: Vec<Bar> =
        (401..=450).map(|i| Bar::ohlcv(60_000 * i, 90.0, 90.0, 90.0, 90.0, 1.0)).collect();
    store.append_bars(VENUE, SYMBOL, "1m", &later, None).unwrap();
    store
        .append_bars(VENUE, SYMBOL, "1m", &[Bar::ohlcv(30_000, 90.0, 90.0, 90.0, 90.0, 1.0)], None)
        .unwrap();
    let (equity, timestamp) =
        rebuild_curve(&open, &log.recipe, &log.events, &store, Some("aaaaaaa")).unwrap();
    assert_eq!(
        bits(&equity),
        bits(&result.equity_curve),
        "the grown store rebuilds the run's curve"
    );
    assert_eq!(timestamp, result.equity_timestamp);

    let mut inside = tape()[250].clone();
    inside.close += 0.25;
    store.append_bars(VENUE, SYMBOL, "1m", &[inside], None).unwrap();
    let why = rebuild_curve(&open, &log.recipe, &log.events, &store, Some("aaaaaaa")).unwrap_err();
    assert!(why.contains("not the ones the run marked"), "{why}");
}

/// `record` answers NO log, and the run keeps its curve, when its log does not rebuild the run's own curve: a cash
/// move the log names and the engine never made (what a mutator the log does not cover reads as), or a curve that is
/// not the one the bars and the log give.
#[test]
fn a_log_that_does_not_rebuild_its_run_is_not_answered() {
    let (_dir, store) = seeded();
    let profile = ProfileBacktest::from_toml_str(&toml("bar"))
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let (bars, _) = crate::run::load_profile_bars(&profile, &store, None, &mut Vec::new()).unwrap();
    let params = vike_simulator::StrategyEngineConfig { cash: 1000.0, ..Default::default() };
    let strategy = profile.strategy.as_ref().unwrap().build().unwrap();
    let mut engine = vike_simulator::StrategyEngine::new(bars, strategy, params);
    let start = engine.core.start_ledger_log();
    let mut result = engine.run();
    assert!(record(&profile, &engine.core, start.clone(), &result).is_some(), "its own log");

    let kept = result.equity_curve[200];
    result.equity_curve[200] = kept + 1.0;
    assert!(record(&profile, &engine.core, start.clone(), &result).is_none(), "another curve");
    result.equity_curve[200] = kept;

    result.equity_timestamp[200] += 1;
    assert!(record(&profile, &engine.core, start.clone(), &result).is_none(), "another step time");
    result.equity_timestamp[200] -= 1;
    let last = result.equity_timestamp.pop().unwrap();
    assert!(record(&profile, &engine.core, start.clone(), &result).is_none(), "a step time short");
    result.equity_timestamp.push(last);
    assert!(record(&profile, &engine.core, start.clone(), &result).is_some(), "its own log again");

    let moved = vike_simulator::LedgerEventKind::Funding { amount: 1.0 };
    let log = engine.core.ledger_events.as_mut().unwrap();
    log.push(LedgerEvent { step: 10, si: 0, kind: moved });
    assert!(
        record(&profile, &engine.core, start, &result).is_none(),
        "a move the engine never made"
    );
}
