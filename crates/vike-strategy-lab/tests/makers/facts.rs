//! The spread maker's MOUNT FACTS against what the daemon mounted: `spread_maker::maker_params`
//! lowers each profile shape's table on its mount's facts (the venue's default preset, the mount's
//! grid, the static fee lane's floor) to the maker the daemon's A-S lowering built for it, recorded
//! before plan 2b deleted that lowering (`maker_golden.txt` beside this file: the effective-params
//! line, the maker's whole params, the mount spec); then to the bag the mount's fee re-arm rebuilds
//! from (`vike_market_making::rearm_fee_floor`). The echo of that bag is the recorded maker line, and the
//! refusal is the maker's own words. A SPEC line names its mount by the instrument's user name (decision
//! 0290: hyperliquid's `BTC` perpetual is `BTCUSDC.P`, bybit's linear `BTCUSDT` is `BTCUSDT.P`, oanda's
//! `EUR_USD` is `EURUSD`), which picks the same fee lane the recording priced.

use vike_model::params::Params;
use vike_model::{FeeSchedule, maker_round_trip_fee};

use super::spread_maker::{self, MountFacts};

/// What each maker row of the golden printed, before plan 2b, through the daemon's A-S lowering.
const GOLDEN: &str = include_str!("maker_golden.txt");

/// Every maker row of the golden: `(case[row], its [strategy.params], it was named gueant_maker)`,
/// the tables of the cases `maker_golden.txt` was printed from (the row a registry strategy
/// mounted, `multi[2]`, is no maker).
const MAKER_ROWS: &[(&str, &str, bool)] = &[
    ("poly-default[0]", "", false),
    ("poly-tick[0]", "", false),
    ("poly-knobs[0]", "qty = 5.0\nhalf_spread = 0.02\nresolution_ts = 1800000000000", false),
    ("poly-named[0]", "resolution_ts = 1800000000000", false),
    ("poly-gueant[0]", "qty = 7.0", true),
    ("hl-default[0]", "", false),
    ("bybit-knobs[0]", "qty = 0.001", false),
    ("bybit-gueant[0]", "qty = 0.001\nhalf_spread = 0.3", true),
    ("binance-perp[0]", "", false),
    ("binance-spot[0]", "qty = 0.0002", false),
    ("oanda[0]", "qty = 1000.0", false),
    ("dukascopy[0]", "qty = 1000.0", false),
    ("multi[0]", "qty = 0.002", false),
    ("multi[1]", "qty = 3.0\nresolution_ts = 7", true),
];

/// The golden's `kind` line of `row`, after `GOLDEN <row> <kind> `.
fn golden(row: &str, kind: &str) -> &'static str {
    let prefix = format!("GOLDEN {row} {kind} ");
    GOLDEN
        .lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("the golden has no {kind} line for {row}"))
}

/// `key=value` of a golden SPEC line.
fn spec_field<'a>(spec: &'a str, key: &str) -> &'a str {
    let prefix = format!("{key}=");
    spec.split(' ')
        .find_map(|w| w.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("no {key} in {spec}"))
}

/// The row's table (`spread_model = "gueant"` for a `gueant_maker` row, the alias the file's key
/// replaces) and the facts its mount carried, read off the golden's SPEC line.
fn table_and_facts(
    row: &str,
    params: &str,
    gueant: bool,
    account: Option<f64>,
) -> (Params, MountFacts) {
    let mut table: Params = toml::from_str(params).unwrap_or_else(|e| panic!("{row}: {e}"));
    if gueant {
        table.insert("spread_model", "gueant");
    }
    let spec = golden(row, "SPEC");
    let (venue, symbol) = (spec_field(spec, "venue"), spec_field(spec, "symbol"));
    let facts = MountFacts {
        venue: venue.to_string(),
        tick_size: spec_field(spec, "tick").parse().unwrap_or_else(|e| panic!("{row}: {e}")),
        static_round_trip_fee_rate: vike_tradehub_core::maker_round_trip_fee_for(venue, symbol),
        account_round_trip_fee_rate: account,
    };
    (table, facts)
}

fn joined(rows: Vec<(&'static str, String)>) -> String {
    rows.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

/// The facts document a LIVE mount hands the file (`build(params, facts)`, `echo(params, facts)`):
/// `facts` with its grid stated.
fn live(facts: &MountFacts) -> vike_model::params::MountFacts {
    vike_model::params::MountFacts {
        venue: facts.venue.clone(),
        tick_size: Some(facts.tick_size),
        static_round_trip_fee_rate: facts.static_round_trip_fee_rate,
        account_round_trip_fee_rate: facts.account_round_trip_fee_rate,
        ..vike_model::params::MountFacts::default()
    }
}

#[test]
fn the_facts_lower_every_recorded_shape_to_the_maker_the_daemon_mounted() {
    assert_eq!(GOLDEN.lines().filter(|l| l.contains(" MAKER Spread")).count(), MAKER_ROWS.len());
    for &(row, params, gueant) in MAKER_ROWS {
        let (table, facts) = table_and_facts(row, params, gueant, None);
        let ours = spread_maker::maker_params(&table, &facts);
        let maker = spread_maker::SpreadMaker::from_params(&ours).expect("reads");
        assert_eq!(format!("{:?}", maker.params()), golden(row, "MAKER"), "{row}");
        assert_eq!(joined(spread_maker::echo(&table, &live(&facts))), golden(row, "EP"), "{row}");
        assert_eq!(spread_maker::refuse(&ours), None, "{row}");
    }
}

/// the CI box's bybit DEMO mount, row for row (`mount_param` `qty = 0.001`, `half_spread = 0.0005`; the
/// mount's `tick_size` 0.1): the line the daemon prints when the mount opens is the file's `echo`, and
/// it must be the maker that RUNS, the crypto preset's unbounded, constant-horizon A-S on the mount's
/// grid under bybit's 4 bp break-even floor (the golden's `bybit-knobs` line). Asked over the params
/// alone it printed the bare reader's outcome-token defaults on a perpetual (the second line, read off
/// the CI box's journal on 2026-11-19), which pointed a health check at a Polymarket misconfiguration while
/// the maker was refusing at its break-even floor (`spread_maker`'s
/// `the_fee_floor_refusal_is_not_double_reported`).
#[test]
fn the_mount_line_is_the_maker_that_runs_not_the_bare_table() {
    let table: Params = toml::from_str("qty = 0.001\nhalf_spread = 0.0005").expect("a table");
    let mount = vike_model::params::MountFacts {
        symbol: "BTCUSDT.P".into(),
        venue: "bybit".into(),
        interval: "1m".into(),
        tick_size: Some(0.1),
        static_round_trip_fee_rate: vike_tradehub_core::maker_round_trip_fee_for(
            "bybit",
            "BTCUSDT.P",
        ),
        account_round_trip_fee_rate: None,
    };
    assert_eq!(joined(spread_maker::echo(&table, &mount)), golden("bybit-knobs[0]", "EP"));
    // A backtest states no grid, so its maker IS the bare table, and its line stays the reader's.
    let backtest = vike_model::params::MountFacts {
        symbol: "BTCUSDT.P".into(),
        interval: "1m".into(),
        ..vike_model::params::MountFacts::default()
    };
    assert_eq!(
        joined(spread_maker::echo(&table, &backtest)),
        "qty=0.001 tick_size=0 min_half_spread_ticks=0 max_half_spread_ticks=0 \
         round_trip_fee_rate=None kappa_default=50 tau_hold_ms=3600000 price_domain=UnitInterval \
         variance_mode=LocalCapped horizon_mode=TimeToResolution spread_model=AvellanedaStoikov \
         gamma=0.1 resolution_ts=None"
    );
}

/// The re-arm at the account's own rate: wherever `vike_market_making::rearm_fee_floor` rebuilds the maker,
/// the facts' account rate gives the same bag; wherever it keeps the mounted maker, the facts give
/// the mounted bag.
#[test]
fn the_account_rate_rearms_the_floor_as_the_mount_does() {
    let schedules = [
        FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 },
        FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 3.0 },
        FeeSchedule::ProbabilityScaled {
            taker_rate: 0.072,
            maker_rate: 0.0,
            maker_rebate_share: 0.2,
        },
    ];
    for &(row, params, gueant) in MAKER_ROWS {
        let (table, facts) = table_and_facts(row, params, gueant, None);
        // The mounted bag (the golden's maker, by the test above).
        let bag = spread_maker::maker_params(&table, &facts);
        let spec = golden(row, "SPEC");
        let (venue, symbol) = (spec_field(spec, "venue"), spec_field(spec, "symbol"));
        for schedule in schedules {
            let rearmed = vike_market_making::rearm_fee_floor(&bag, Some(schedule), venue, symbol)
                .unwrap_or_else(|| bag.clone());
            let (table, facts) =
                table_and_facts(row, params, gueant, maker_round_trip_fee(schedule));
            assert_eq!(spread_maker::maker_params(&table, &facts), rearmed, "{row} {schedule:?}");
        }
        // No engine of the account answered: the mount keeps the maker it was built with.
        assert_eq!(vike_market_making::rearm_fee_floor(&bag, None, venue, symbol), None);
    }
}

#[test]
fn a_word_the_maker_refuses_is_refused_in_its_own_words() {
    for (key, word) in
        [("spread_model", "gueant_lehale"), ("preset", "cryptoo"), ("style", "diagonal")]
    {
        let mut p = Params::new();
        p.insert(key, word);
        let want = spread_maker::SpreadMaker::from_params(&p).err().map(|e| e.to_string());
        assert!(want.is_some(), "{key} = {word} must be refused");
        assert_eq!(spread_maker::refuse(&p), want);
        assert!(spread_maker::echo(&p, &vike_model::params::MountFacts::default()).is_empty());
    }
    let mut p = Params::new();
    p.insert("spread_model", 3_i64);
    let want = spread_maker::SpreadMaker::from_params(&p).err().map(|e| e.to_string());
    assert_eq!(spread_maker::refuse(&p), want);
    assert!(want.expect("refused").contains("must be a string"));
}

/// The facts document reaches the maker through `build(params, facts)`: a BACKTEST's (`symbol` and
/// `interval` alone) leaves the table as it is, so a maker backtest is its table's; a LIVE mount's
/// (its `tick_size` stated) lowers the table as `maker_params` does.
#[test]
fn a_backtests_facts_leave_the_table_and_a_live_mounts_lower_it() {
    use vike_model::Strategy;
    use vike_simulator::OrderBrokerSimulator;

    let mut table = Params::new();
    table.insert("qty", 0.01);
    let built = |facts: &vike_model::params::MountFacts| {
        format!("{:?}", spread_maker::build::<OrderBrokerSimulator>(&table, facts).params())
    };
    let of = |bag: &Params| {
        let maker = spread_maker::SpreadMaker::from_params(bag).expect("reads");
        format!("{:?}", Strategy::<OrderBrokerSimulator>::params(&maker))
    };
    let backtest = vike_model::params::MountFacts {
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        ..vike_model::params::MountFacts::default()
    };
    assert_eq!(built(&backtest), of(&table), "a backtest's facts");
    let live = vike_model::params::MountFacts {
        venue: "hyperliquid".into(),
        tick_size: Some(1.0),
        static_round_trip_fee_rate: Some(0.0003),
        ..backtest.clone()
    };
    let lowered = spread_maker::maker_params(
        &table,
        &MountFacts {
            venue: "hyperliquid".into(),
            tick_size: 1.0,
            static_round_trip_fee_rate: Some(0.0003),
            account_round_trip_fee_rate: None,
        },
    );
    assert_eq!(built(&live), of(&lowered), "a live mount's facts");
    assert_ne!(built(&live), built(&backtest), "the live facts change the bag");
}
