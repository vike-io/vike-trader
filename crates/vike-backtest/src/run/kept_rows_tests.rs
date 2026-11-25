//! Decision 0290 in a backtest: a profile's symbols and a strategy's inputs are USER names, typed in any ASCII
//! case, each read under its STORE name (`KeptRows::store_name`, against the kept lists the door asked for), and a
//! name its venue's list names no instrument by is refused before the run, in words. And PM-2: a recorded polymarket
//! token's facts are its row.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use vike_data::{HistoryStore, HistoryStoreMemory, SeriesId};
use vike_marketdata::test_support::flat_bar_zero_volume;
use vike_marketdata::{OptionKind, TickQuote};
use vike_model::StoreKindId;
use vike_model::test_support::instruments::perp;
use vike_model::{
    AssetClass, Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct,
    ContractCfdUnderlying, ContractEquity, ContractFx, ContractOption, ContractOptionExerciseStyle,
    Instrument, InstrumentVenueIds, Margining, Strategy, SymbolProperties,
};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, TickEvent};

use super::*;
use crate::run::data_plan::{planned_series_ids, resolve_inputs};
use crate::run::{profile::ProfileBacktest, run_backtest};

const RECORDED: &str =
    "71321045679252212594626385532706912750332728571942532289631379312455583992563";
const EARLIER: &str =
    "10000000000000000000000000000000000000000000000000000000000000000000000000001";
const T: i64 = 1_794_124_800_000;

/// A store holding the facts the datahub's recorder writes beside a recorded token's rows (PM-2): tick 0.01, five
/// shares at least, a share step, classed a prediction market. `EARLIER` was recorded before facts were.
fn store_with_facts() -> Arc<HistoryStoreMemory> {
    let store = Arc::new(HistoryStoreMemory::new());
    let facts = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.01,
        min_qty: 5.0,
        asset_class: Some(AssetClass::PredictionMarket),
        ..Default::default()
    };
    store.append_symbol_properties("polymarket", RECORDED, &[(T, facts)], None).expect("stored");
    store
}

/// **PM-2: a recorded token's facts are its row**: no kept list names either token, and only the one recorded
/// before facts were written is named missing, loudly as before.
#[test]
fn a_recorded_tokens_facts_are_its_row_and_an_earlier_one_keeps_the_warning() {
    let store = store_with_facts();
    let series: Vec<(String, String)> =
        [RECORDED, EARLIER].iter().map(|t| ("polymarket".to_string(), (*t).to_string())).collect();
    let none: &[Instrument] = &[];
    let named = KeptRows::needing_rows(store.as_ref(), Ok(Some(none)), &series);
    assert_eq!(named, [("polymarket".to_string(), EARLIER.to_string())]);
    let kept = KeptRows::answered(Ok(Some(Vec::new())), &named);
    assert_eq!(kept.warnings.len(), 1, "{:?}", kept.warnings);
    assert!(kept.warnings[0].contains(EARLIER) && !kept.warnings[0].contains(RECORDED));
    // A store that serves no kept list keeps every series, as before.
    assert_eq!(KeptRows::needing_rows(store.as_ref(), Ok(None), &series), series);
}

/// Buys on the first quote with a limit at `price` for `qty` shares and counts its fills.
struct BuyOnce {
    qty: f64,
    price: f64,
    sent: bool,
    fills: Rc<RefCell<usize>>,
}

impl Strategy<OrderBrokerSimulator> for BuyOnce {
    fn on_quote_tick(&mut self, ctx: &mut OrderBrokerSimulator, _t: &TickQuote) {
        if !self.sent {
            self.sent = true;
            ctx.submit_limit(RECORDED, 1, self.qty, self.price, 0.0, true, None);
        }
    }

    fn on_fill(&mut self, _ctx: &mut OrderBrokerSimulator, _f: &vike_model::Fill) {
        *self.fills.borrow_mut() += 1;
    }
}

/// How many fills a run over a quote tape of the recorded token books for one limit buy, reading the recorded
/// facts as a run with `engine.snap_to_properties` does (`crate::run::tick_replay::properties_source`).
fn fills_of(qty: f64, price: f64) -> usize {
    let store: Arc<dyn HistoryStore + Send + Sync> = store_with_facts();
    let fills = Rc::new(RefCell::new(0));
    let config = StrategyEngineConfig {
        cash: 1_000.0,
        default_venue: Some("polymarket".to_string()),
        properties: Some(crate::run::tick_replay::properties_source(store, Arc::default())),
        ..Default::default()
    };
    let strategy = BuyOnce { qty, price, sent: false, fills: Rc::clone(&fills) };
    let tape: Vec<TickEvent> = (0..20)
        .map(|i| {
            TickEvent::Quote(TickQuote {
                timestamp: T + 1_000 + i * 100,
                local_timestamp: 0,
                bid: 0.49,
                ask: 0.51,
                bid_size: 1_000.0,
                ask_size: 1_000.0,
                symbol: RECORDED.into(),
            })
        })
        .collect();
    let mut engine =
        StrategyEngine::new(vec![(RECORDED.to_string(), Vec::new())], strategy, config);
    engine.run_ticks(&[(RECORDED.to_string(), tape)]);
    fills.take()
}

/// **PM-2: a run over recorded rows refuses what the venue refuses**: the 3-share buy at 0.523 is under the recorded
/// five-share minimum and books nothing, while five shares fill.
#[test]
fn a_run_over_recorded_rows_refuses_an_order_under_the_recorded_minimum() {
    assert_eq!(fills_of(3.0, 0.523), 0, "three shares are under the venue's minimum of five");
    assert!(fills_of(5.0, 0.523) > 0, "five shares are the venue's minimum and fill");
}

/// The kept lists the tests' datahub serves, each row as its bridge keeps it: bybit's spot pair and perpetual of one
/// coin, oanda's pair, dukascopy's share CFD (a row stating no base), a hyperliquid builder-dex coin and deribit's
/// coin-settled option.
fn lists() -> Vec<Instrument> {
    let share = Contract::Cfd(ContractCfd {
        underlying_kind: ContractCfdUnderlying::Share,
        underlying: None,
        product: ContractCfdProduct::Cfd,
        expiry: ContractCfdExpiry::Unknown,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    });
    let coin_option = Contract::Option(ContractOption {
        right: OptionKind::Call,
        strike: 90_000.0,
        expiry_ms: 0,
        style: ContractOptionExerciseStyle::Unknown,
        margining: Margining::Inverse,
        settle: "BTC".into(),
        value_currency: None,
        underlying: None,
    });
    let dex = InstrumentVenueIds { coin: Some("xyz:TSLA".into()), ..InstrumentVenueIds::default() };
    // named as the datahub's write stores them (`vike_catalog::spelled`)
    vike_catalog::spelled(vec![
        Instrument::new("bybit", "BTCUSDT", "BTC", "USDT", Contract::CryptoSpot),
        Instrument::new("bybit", "BTCUSDT.P", "BTC", "USDT", perp(Margining::Linear, "USDT")),
        Instrument::new("oanda", "EUR_USD", "EUR", "USD", Contract::Fx(ContractFx::default())),
        Instrument::new("dukascopy", "AAPL.US/USD", "", "USD", share),
        Instrument::new("hyperliquid", "xyz:TSLA", "TSLA", "USDC", perp(Margining::Linear, "USDC"))
            .with_wire(dex),
        Instrument::new("deribit", "BTC-27MAR26-90000-C", "BTC", "USD", coin_option),
    ])
}

/// A bar profile buying and holding `symbol` of `venue`, as the profile names it.
fn profile(venue: &str, symbol: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "{venue}"
symbols = ["{symbol}"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000000.0

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1000.0
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategy_lab::stand_in)
}

/// [`profile`] as a door resolves it, against [`lists`] (`ProfileBacktest::read_kept_rows`).
fn read(venue: &str, symbol: &str) -> Result<ProfileBacktest, HarnessError> {
    let rows = lists();
    let mut p = profile(venue, symbol);
    p.hold_kept_rows(Ok(Some(rows)), Ok(None)).map(|()| p)
}

/// The refusal [`read`] answers.
fn refused(venue: &str, symbol: &str) -> String {
    match read(venue, symbol) {
        Err(HarnessError::Data(m)) => m,
        other => panic!("{symbol} on {venue}: expected a refusal before the run, got {other:?}"),
    }
}

/// The `kind=bar` series a run of `p` reads.
fn reads(p: &ProfileBacktest) -> Vec<SeriesId> {
    planned_series_ids(p, &[])
}

/// **bybit's perpetual and its spot pair are two names and two series** (audit BYB-1, BIN-7): `BTCUSDT.P` in any case
/// reads the perpetual's store series, `BTCUSDT` the spot pair's, each with its own kept row.
#[test]
fn a_bybit_perpetual_and_its_spot_pair_read_their_own_series() {
    for (typed, store, raw) in [
        ("BTCUSDT.P", "BTCUSDT.P", "BTCUSDT.P"),
        ("btcusdt.p", "BTCUSDT.P", "BTCUSDT.P"),
        ("BTCUSDT", "BTCUSDT", "BTCUSDT"),
        ("btcUSDT", "BTCUSDT", "BTCUSDT"),
    ] {
        let p = read("bybit", typed).unwrap_or_else(|e| panic!("{typed}: {e:?}"));
        assert_eq!(p.data.symbols, [store], "{typed}");
        let bar = SeriesId::per_symbol(StoreKindId::Bar, "bybit", store, Some("1d".into()));
        assert_eq!(reads(&p), [bar], "{typed}");
        assert_eq!(p.kept.row("bybit", store).map(|r| r.raw_symbol.as_str()), Some(raw), "{typed}");
        assert!(p.kept.warnings.is_empty(), "{typed}: {:?}", p.kept.warnings);
    }
}

/// **oanda's pair is `EURUSD`** (audit OAN-1): the run reads the store's `EURUSD` series, and the venue's own
/// spelling is refused in words that name the user name.
#[test]
fn an_oanda_pair_reads_its_user_name_and_the_venues_spelling_is_refused() {
    let p = read("oanda", "eurusd").unwrap();
    assert_eq!(p.data.symbols, ["EURUSD"]);
    let store = HistoryStoreMemory::new();
    let bars: Vec<_> = (0..3).map(|i| flat_bar_zero_volume(i * 1000, 1.08)).collect();
    store.append_bars("oanda", "EURUSD", "1d", &bars, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    run_backtest(&p, store).expect("the run reads oanda's EURUSD series");

    let m = refused("oanda", "EUR_USD");
    assert!(m.contains("data symbol \"EUR_USD\" on oanda is refused before the run"), "{m}");
    assert!(m.contains("`EUR_USD` is named `EURUSD`") && m.contains("decision 0290"), "{m}");
}

/// **dukascopy's share CFD is `AAPL.CFD`** (audit DUK-2, X-1), in any case: its row states no base and its venue
/// spells it `AAPL.US/USD`.
#[test]
fn a_dukascopy_share_cfd_reads_its_user_name() {
    let p = read("dukascopy", "aapl.cfd").unwrap();
    assert_eq!(p.data.symbols, ["AAPL.CFD"]);
    assert_eq!(
        p.kept.row("dukascopy", "AAPL.CFD").map(|r| r.raw_symbol.as_str()),
        Some("AAPL.US/USD")
    );
    assert!(refused("dukascopy", "AAPLUSUSD").contains("`AAPLUSUSD`"));
}

/// **A builder-dex coin's `:` is the symbol's, not a lane's** (audit HL-4): `hyperliquid:XYZ:TSLAUSDC.P:1h` parses
/// as its venue, its symbol and its interval, and reads the store's `XYZ-TSLAUSDC.P`, a `:` being no path's.
#[test]
fn a_builder_dex_input_parses_and_reads_its_store_name() {
    let p = read("bybit", "BTCUSDT.P").unwrap();
    for name in ["hyperliquid:XYZ:TSLAUSDC.P:1h", "hyperliquid:xyz:tslausdc.p:1h"] {
        let inputs = resolve_inputs(&p, &[name.to_string()]).unwrap_or_else(|e| panic!("{e:?}"));
        let id = &inputs[0].id;
        assert_eq!(
            (id.venue.as_str(), id.symbol.as_str(), id.interval.as_deref(), id.source.as_deref()),
            ("hyperliquid", "XYZ-TSLAUSDC.P", Some("1h"), None),
            "{name}"
        );
    }
    // The venue's spelling of an input is refused before the run like a profile's symbol; a lane's series is the
    // operator's own import, read as it was filed.
    let Err(HarnessError::Validation(m)) = resolve_inputs(&p, &["oanda:EUR_USD:1h".to_string()])
    else {
        panic!("oanda's EUR_USD is no user name")
    };
    assert!(m.to_string().contains("`EUR_USD` is named `EURUSD`"), "{m}");
    let lane = resolve_inputs(&p, &["user-mine:bybit:USDT-USD:1d".to_string()]).unwrap();
    assert_eq!(lane[0].id.symbol, "USDT-USD");
}

/// **No list, no instrument** (decision 0290): a venue whose list the datahub keeps no row of is refused naming the
/// list to keep; DER-1, a symbol the order door refuses (deribit's coin-settled option) is refused in the door's
/// words; PM-1, a polymarket name is an outcome's token id or nothing. A store that serves no list at all (a test's,
/// a datahub too old to keep them) reads every name as typed, as before.
#[test]
fn a_name_no_kept_row_names_is_refused_before_the_run() {
    let m = refused("ig", "EURUSD");
    assert!(m.contains(&vike_catalog::no_kept_list_refusal("ig")), "the fix is named: {m}");
    assert!(m.contains("<project>/instruments/ig.json"), "the fix: {m}");
    assert!(refused("bybit", "ETHUSDT.P").contains("carries no row it names"));
    assert!(refused("deribit", "BTC-27MAR26-90000-C").contains("coin-settled option"));
    let m = refused("polymarket", "will-btc-hit-100k");
    assert!(m.contains("no polymarket outcome token id"), "{m}");
    let token = "67102598924726349989742049657609063097299442010642912942032681022413402597017";
    assert_eq!(read("polymarket", token).unwrap().data.symbols, [token]);

    let mut p = profile("bybit", "btcusdt.p");
    p.hold_kept_rows(Ok(None), Ok(None)).unwrap();
    assert_eq!(p.data.symbols, ["btcusdt.p"], "no list served: as typed");
    let kept = KeptRows::of(&lists(), &[]);
    assert_eq!(kept.store_name("user", "Sentiment").as_deref(), Ok("Sentiment"), "the user venue");
}

/// **A series the store lists reads under its store name too** (the Studio's slice picker names the store's series): a
/// builder-dex `XYZ-TSLAUSDC.P` is the row whose user name is `XYZ:TSLAUSDC.P`, as that user name reads it.
#[test]
fn a_store_name_reads_its_kept_row() {
    for typed in ["XYZ-TSLAUSDC.P", "XYZ:TSLAUSDC.P"] {
        let p = read("hyperliquid", typed).unwrap_or_else(|e| panic!("{typed}: {e:?}"));
        assert_eq!(p.data.symbols, ["XYZ-TSLAUSDC.P"], "{typed}");
        let row = p.kept.row("hyperliquid", "XYZ-TSLAUSDC.P").map(|r| r.raw_symbol.as_str());
        assert_eq!(row, Some("xyz:TSLA"), "{typed}");
    }
}

/// **An `engine.multipliers` key follows its data symbol to the store name** (decision 0290): the engine matches a row
/// by the exact series name, so the key the profile typed beside `btcusdt.p` would size nothing once the run reads
/// `BTCUSDT.P`. And the profile is judged again with the rows in hand: an ibkr stock's unit is its KEPT row's, so a
/// multiplier no parse could judge (no row then) is refused once the row is read.
#[test]
fn a_multiplier_key_follows_its_symbol_and_a_held_row_judges_its_unit() {
    let rows = lists();
    let mut p = profile("bybit", "btcusdt.p");
    p.engine.multipliers.insert("btcusdt.p".to_string(), 1.0);
    p.hold_kept_rows(Ok(Some(rows)), Ok(None)).unwrap();
    assert_eq!(p.data.symbols, ["BTCUSDT.P"]);
    let keys: Vec<(&str, f64)> =
        p.engine.multipliers.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(keys, [("BTCUSDT.P", 1.0)]);

    let equity = ContractEquity { exchange: None, currency: Some("USD".into()) };
    let mut rows = lists();
    rows.extend(vike_catalog::spelled(vec![Instrument::new(
        "ibkr",
        "AAPL.SMART.USD",
        "AAPL",
        "USD",
        Contract::Equity(equity),
    )]));
    let mut p = profile("ibkr", "AAPL");
    p.engine.multiplier = 100.0;
    let Err(HarnessError::Validation(m)) = p.hold_kept_rows(Ok(Some(rows)), Ok(None)) else {
        panic!("a multiplier against the kept row's unit must be refused");
    };
    assert!(m.to_string().contains("engine.multiplier sizes AAPL on ibkr at 100"), "{m}");
}

/// **A `[[mounts]]` slot follows its data symbol to the store name too**, as the profile typed it: the mount the
/// profile wrote beside `btcusdt.p` trades `BTCUSDT.P` once the run reads it, and the profile still loads (every slot
/// still a mount's).
#[test]
fn a_mount_slot_follows_its_symbol_to_the_store_name() {
    let text = r#"
[data]
venue = "bybit"
symbols = ["btcusdt.p"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000000.0

[[mounts]]
symbols = ["btcusdt.p"]
[mounts.strategy]
file = "trend/buy_hold.rs"
"#;
    let mut p = ProfileBacktest::from_toml_str(text).expect("a portfolio loads");
    p.hold_kept_rows(Ok(Some(lists())), Ok(None)).expect("its slot follows the store name");
    assert_eq!(p.data.symbols, ["BTCUSDT.P"]);
    assert_eq!(p.mounts[0].symbols, ["BTCUSDT.P"]);
}

/// A strategy that reads `names` beside its own bars and trades nothing.
struct ReadsInputs(Vec<String>);

impl Strategy<OrderBrokerSimulator> for ReadsInputs {
    fn inputs(&self) -> Vec<String> {
        self.0.clone()
    }
    fn on_bar(&mut self, _: &mut OrderBrokerSimulator, _: &vike_marketdata::Bar) {}
}

/// [`profile`] whose strategy reads `inputs`.
fn reading(venue: &str, symbol: &str, inputs: &[&str]) -> ProfileBacktest {
    let mut p = profile(venue, symbol);
    let names: Vec<String> = inputs.iter().map(|n| (*n).to_string()).collect();
    p.strategy.as_mut().unwrap().builder =
        Some(crate::run::strategy_builder::StrategyBuilder::new(move |_| {
            Ok(Box::new(ReadsInputs(names.clone())))
        }));
    p
}

/// **A door asks the venues its run reads, and reads them as the whole lists** (the sandboxed child's kept rows on
/// demand): a run of bybit's perpetual reading a hyperliquid input asks bybit and hyperliquid alone, and its store
/// names, rows, warnings and inputs are the ones the whole lists give; a symbol no list names is refused in the same
/// words; a param table, which may change what the strategy reads, asks every venue.
#[test]
fn a_door_asks_the_venues_its_run_reads_and_reads_them_as_the_whole_lists() {
    let store = HistoryStoreMemory::new();
    store.serve_kept_instruments(lists());
    let input = "hyperliquid:XYZ:TSLAUSDC.P:1d";
    let p = reading("bybit", "btcusdt.p", &[input]);
    assert_eq!(
        crate::run::data_plan::kept_venues(&p),
        Some(vec!["bybit".to_string(), "hyperliquid".to_string()])
    );
    let mut asked = p.clone();
    asked.read_kept_rows(&store).expect("the venues' rows");
    let mut whole = p.clone();
    whole.hold_kept_rows(Ok(Some(lists())), Ok(None)).expect("the whole lists");
    assert!(asked.kept.rows.iter().all(|r| r.venue == "bybit" || r.venue == "hyperliquid"));
    assert_eq!(asked.data.symbols, whole.data.symbols);
    assert_eq!(asked.kept.warnings, whole.kept.warnings);
    for (venue, name) in [("bybit", "BTCUSDT.P"), ("hyperliquid", "XYZ:TSLAUSDC.P")] {
        assert_eq!(asked.kept.store_name(venue, name), whole.kept.store_name(venue, name));
        let store_name = whole.kept.store_name(venue, name).expect("a store name");
        assert_eq!(asked.kept.row(venue, &store_name), whole.kept.row(venue, &store_name));
        assert_eq!(
            asked.kept.fiat_quote(venue, &store_name),
            whole.kept.fiat_quote(venue, &store_name)
        );
    }
    let ids = |p: &ProfileBacktest| {
        resolve_inputs(p, &[input.to_string()]).expect("resolved").into_iter().map(|i| i.id)
    };
    assert_eq!(ids(&asked).collect::<Vec<_>>(), ids(&whole).collect::<Vec<_>>());

    let refusal = |mut p: ProfileBacktest, whole_lists: bool| {
        let read = if whole_lists {
            p.hold_kept_rows(Ok(Some(lists())), Ok(None))
        } else {
            p.read_kept_rows(&store)
        };
        match read {
            Err(HarnessError::Data(m)) => m,
            other => panic!("expected a refusal, got {other:?}"),
        }
    };
    let missing = reading("bybit", "nosuchcoin", &[input]);
    assert_eq!(refusal(missing.clone(), false), refusal(missing, true));

    let mut scan = reading("bybit", "btcusdt.p", &[input]);
    scan.paramscan = Some(Default::default());
    assert_eq!(crate::run::data_plan::kept_venues(&scan), None, "a param table asks every venue");
}

/// **The tick a strategy reads off its kept row** ([`KeptRows::ticks`], the owner's ruling of 2026-11-19): each
/// series' row's tick under its slot, an account's leg as `SYMBOL@account`; a row that states no tick, and a series
/// no row covers, lend nothing. The row's other fields are not part of the answer.
#[test]
fn each_series_lends_its_kept_rows_tick_under_its_slot() {
    let row = |symbol: &str, base: &str, tick: f64| {
        let grid = SymbolProperties { tick_size: tick, step_size: 10.0, ..Default::default() };
        Instrument::new("binance", symbol, base, "USDT", Contract::CryptoSpot).with_properties(grid)
    };
    let rows = vike_catalog::spelled(vec![row("BTCUSDT", "BTC", 0.01), row("ETHUSDT", "ETH", 0.0)]);
    let pairs: Vec<(String, String)> = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
        .iter()
        .map(|s| ("binance".to_string(), (*s).to_string()))
        .collect();
    let kept = KeptRows::of(&rows, &pairs);
    let mut leg = ProfileDataSeries::new("binance", "BTCUSDT");
    leg.account = Some("binance/main".to_string());
    let series = [
        ProfileDataSeries::new("binance", "BTCUSDT"),
        leg,
        ProfileDataSeries::new("binance", "ETHUSDT"),
        ProfileDataSeries::new("binance", "SOLUSDT"),
    ];
    assert_eq!(
        kept.ticks(&series),
        [("BTCUSDT".to_string(), 0.01), ("BTCUSDT@binance/main".to_string(), 0.01)]
    );
    assert!(KeptRows::default().ticks(&series).is_empty(), "no list served: no tick lent");
}
