use super::*;
use crate::test_strategy_files;
use vike_strategy_lab::makers::spread_maker::SpreadMaker;

#[cfg(test)]
mod data_only;
#[cfg(test)]
mod inputs;
#[cfg(test)]
mod maker_golden;
#[cfg(test)]
mod model_params;
#[cfg(test)]
mod paper_risk;
#[cfg(test)]
mod rhai;
#[cfg(test)]
mod sdk;
#[cfg(test)]
mod strategy;
#[cfg(test)]
mod strategy_params;
#[cfg(test)]
mod wired_set_sync;

/// The `[strategy]` table of a profile whose test is about the MOUNT, not the strategy: a mount
/// without a file is refused at load, so every such profile names the do-nothing file.
const NOTHING_TABLE: &str = "\n[strategy]\nfile = \"nothing.rhai\"\n";

/// The `[mounts.strategy]` table of a `[[mounts]]` row whose test is about the row, not the strategy.
const NOTHING_ROW_TABLE: &str = "[mounts.strategy]\nfile = \"nothing.rhai\"\n";

/// `grid.rs`: the retired registry's `grid` row (its key types and its route kind) written as
/// a strategy file's header. The body is never built here: a `.rs` file opens through the stand-in
/// of the sandboxed process ([`stand_in`]).
const GRID_RS: &str = "\
//! A test file: today's registry `grid` row as a header.
//!
//! ```toml
//! [strategy]
//! runs_on = \"live\"
//! why = \"a test file\"
//!
//! [params.anchor]
//! type = \"text\"
//! [params.anchor_price]
//! type = \"number\"
//! [params.step]
//! type = \"number\"
//! [params.rungs]
//! type = \"integer\"
//! [params.size]
//! type = \"number\"
//! [params.band]
//! type = \"number\"
//! [params.bounded01]
//! type = \"bool\"
//! [params.tick]
//! type = \"number\"
//! [params.symbol]
//! type = \"text\"
//! route = \"symbol\"
//! ```
pub fn build() {}
";

/// `dca_accumulate.rs`: today's registry `dca_accumulate` row as a header.
const DCA_ACCUMULATE_RS: &str = "\
//! A test file: today's registry `dca_accumulate` row as a header.
//!
//! ```toml
//! [strategy]
//! runs_on = \"live\"
//! why = \"a test file\"
//!
//! [params.side]
//! type = \"text_or_integer\"
//! [params.anchor]
//! type = \"text\"
//! [params.anchor_price]
//! type = \"number\"
//! [params.step]
//! type = \"number\"
//! [params.rungs]
//! type = \"integer\"
//! [params.size]
//! type = \"number\"
//! [params.tp]
//! type = \"number\"
//! [params.symbol]
//! type = \"text\"
//! route = \"symbol\"
//! ```
pub fn build() {}
";

/// `buy_hold.rs`: today's registry `buy_hold` row as a header.
const BUY_HOLD_RS: &str = "\
//! A test file: today's registry `buy_hold` row as a header.
//!
//! ```toml
//! [strategy]
//! runs_on = \"live\"
//! why = \"a test file\"
//!
//! [params.size]
//! type = \"number\"
//! [params.symbol]
//! type = \"text\"
//! route = \"symbol\"
//! ```
pub fn build() {}
";

/// `momentum.rs`: today's registry `momentum` row (a VENUE route and a VENUE MAP) as a header.
const MOMENTUM_RS: &str = "\
//! A test file: today's registry `momentum` row as a header.
//!
//! ```toml
//! [strategy]
//! runs_on = \"live\"
//!
//! [params.qty]
//! type = \"number\"
//! [params.threshold]
//! type = \"number\"
//! [params.tp]
//! type = \"number\"
//! [params.sl]
//! type = \"number\"
//! [params.time_limit_ms]
//! type = \"integer\"
//! [params.trailing]
//! type = \"number\"
//! [params.venue]
//! type = \"text\"
//! route = \"venue\"
//! [params.cooldown_ms]
//! type = \"integer\"
//! [params.venues]
//! type = \"table\"
//! route = \"venue_map\"
//! ```
pub fn build() {}
";

/// `market_making/spread_maker.rs`: a maker-shaped header (two of its knobs), in a subfolder.
const SPREAD_MAKER_RS: &str = "\
//! A test file: two of the maker's knobs as a header.
//!
//! ```toml
//! [strategy]
//! runs_on = \"live\"
//! why = \"a test file\"
//!
//! [params.qty]
//! type = \"number\"
//! [params.half_spread]
//! type = \"number\"
//! ```
pub fn build() {}
";

/// `funding_capture.rs`: a file that runs in a backtest only, its header saying why.
const FUNDING_CAPTURE_RS: &str = "\
//! ```toml
//! [strategy]
//! runs_on = \"backtest\"
//! why = \"it reads Bar::funding, which no live feed delivers\"
//! ```
pub fn build() {}
";

/// A strategies folder holding every test file: the `.rhai` files of
/// [`crate::test_strategy_files::standard`] and the header-only `.rs` twins above.
fn strategies() -> tempfile::TempDir {
    let dir = test_strategy_files::standard();
    test_strategy_files::write_into(
        dir.path(),
        &[
            ("grid.rs", GRID_RS),
            ("dca_accumulate.rs", DCA_ACCUMULATE_RS),
            ("buy_hold.rs", BUY_HOLD_RS),
            ("momentum.rs", MOMENTUM_RS),
            ("funding_capture.rs", FUNDING_CAPTURE_RS),
            ("market_making/spread_maker.rs", SPREAD_MAKER_RS),
        ],
    );
    dir
}

/// The stand-in of a `.rs` file's sandboxed process: it opens every file as a proxy that places
/// nothing, its `echo` answering `echo`.
fn stand_in(
    echo: &'static [(&'static str, &'static str)],
) -> impl Fn(&SandboxOpen<'_>) -> Result<SandboxOpened, String> {
    move |_| {
        Ok(SandboxOpened {
            strategy: Box::new(SpreadMaker::new(1.0, 0.5)),
            echo: echo.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            remote: None,
            sdk: None,
        })
    }
}

/// `text` loaded and resolved over [`strategies`], `.rs` files through [`stand_in`] answering no
/// echo: the load's refusal or the resolve's, whichever comes first.
fn mount(text: &str, live: bool) -> Result<MountResolvedStrategy, String> {
    let dir = strategies();
    ProfileDaemon::from_toml_str(text)?.resolve_mount_with(dir.path(), live, &stand_in(&[]))
}

/// A hyperliquid/BTC profile naming `file`.
fn file_profile(file: &str) -> Result<ProfileDaemon, String> {
    ProfileDaemon::from_toml_str(&format!(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nfile = \"{file}\"\n"
    ))
}

#[test]
fn minimal_profile_uses_polymarket_defaults() {
    let p = ProfileDaemon::from_toml_str(&format!("symbol = \"TOK\"{NOTHING_TABLE}"))
        .expect("minimal profile parses");
    assert_eq!(p.venue(), "polymarket");
    assert_eq!(p.mount_symbol(), "TOK");
    assert_eq!(p.strategy_file(), Some("nothing.rhai"));
    assert_eq!(p.daemon.summary_ms, 5_000);
    assert_eq!(p.daemon.shutdown_deadline_ms, 5_000);

    // The polymarket mount defaults flow through untouched.
    let spec = p.to_mount_spec();
    assert_eq!(spec.venue, "polymarket");
    assert_eq!(spec.symbol, "TOK");
    assert_eq!(spec.interval, "1m");
    assert_eq!(spec.interval_ms, 60_000);
    assert_eq!(spec.tick_size.to_bits(), 0.01_f64.to_bits());
    // The ONE paper starting balance, polymarket included (the owner's ruling of 2026-11-16).
    assert_eq!(spec.seed_cash.to_bits(), vike_tradehub_core::PAPER_SEED_CASH.to_bits());
}

/// **A mount without a strategy file is refused at load, naming the folder**: no strategy is compiled
/// into the program, so there is nothing to default to. A `[strategy]` table naming no file (with
/// params or without) is the same refusal.
#[test]
fn a_mount_without_a_strategy_file_is_refused_naming_the_folder() {
    for profile in [
        "symbol = \"TOK\"",
        "symbol = \"TOK\"\n[strategy]\n",
        "symbol = \"TOK\"\n[strategy]\n[strategy.params]\nsize = 2.0\n",
        "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\n",
    ] {
        let err = ProfileDaemon::from_toml_str(profile).expect_err("no file, no mount");
        assert!(err.contains("names no strategy file"), "{profile:?}: {err}");
        assert!(err.contains("`strategies/` folder"), "names the folder: {err}");
        assert!(err.contains("file = "), "spells the fix: {err}");
        assert!(err.contains("no default"), "says why there is no fallback: {err}");
    }
}

/// **The load's words are the refusal's, under its catalog id**: the profile API's `String` is the variant's
/// text, and a person reading it rendered reads the id beside it.
#[test]
fn a_mount_without_a_strategy_file_is_refused_under_its_id() {
    let refusal = crate::refusals::MountProfileRefusal::NoStrategyFile;
    let err = ProfileDaemon::from_toml_str("symbol = \"TOK\"").expect_err("no file, no mount");
    assert_eq!(err, refusal.to_string());
    assert_eq!(refusal.id().word(), "tradehub.profile.strategy.no_file");
    assert_eq!(refusal.rendered(), format!("error [tradehub.profile.strategy.no_file]: {err}"));
    let row = "[[mounts]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\n";
    let err = ProfileDaemon::from_toml_str(row).expect_err("a row with no file");
    let named = crate::refusals::MountProfileRefusal::RowRefused {
        index: 0,
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        why: refusal.to_string(),
    };
    assert_eq!(err, named.to_string(), "the row's refusal wraps the file's, naming the row");
}

#[test]
fn overrides_apply_to_the_mount_and_the_daemon_settings() {
    let toml = r#"
venue = "polymarket"
symbol = "OUTCOME"
interval = "5m"
interval_ms = 300000
tick_size = 0.01
seed_cash = 250.0

[strategy]
file = "buy_hold.rhai"

[strategy.params]
size = 50.0

[daemon]
summary_ms = 2000
shutdown_deadline_ms = 3000
"#;
    let p = ProfileDaemon::from_toml_str(toml).expect("full profile parses");
    assert_eq!(p.daemon.summary_ms, 2_000);
    assert_eq!(p.summary_interval(), Duration::from_millis(2_000));
    assert_eq!(p.shutdown_deadline(), Duration::from_millis(3_000));

    let spec = p.to_mount_spec();
    assert_eq!(spec.symbol, "OUTCOME");
    assert_eq!(spec.interval, "5m");
    assert_eq!(spec.interval_ms, 300_000);
    assert_eq!(spec.seed_cash.to_bits(), 250.0_f64.to_bits());
    assert_eq!(spec.tick_size.to_bits(), 0.01_f64.to_bits());
    let params = &p.strategy.as_ref().expect("a [strategy] table").params;
    assert_eq!(params.get("size").and_then(|v| v.as_f64()), Some(50.0));
}

/// **A mount's own leverage cap** (the `mount.max_leverage` column's key) loads in both spellings and
/// rides the `[[mounts]]` lowering; a figure under 1x or not finite is refused by name, and a top-level
/// one beside `[[mounts]]` is the both-spellings refusal.
#[test]
fn a_mounts_own_leverage_cap_loads_in_both_spellings_and_is_refused_under_one() {
    let single = |line: &str| format!("symbol = \"TOK\"\n{line}{NOTHING_TABLE}");
    let p = ProfileDaemon::from_toml_str(&single("max_leverage = 3.0")).expect("loads");
    assert_eq!(p.max_leverage, Some(3.0));
    assert_eq!(ProfileDaemon::from_toml_str(&single("")).expect("loads").max_leverage, None);

    let multi = ProfileDaemon::from_toml_str(&format!(
        "[[mounts]]\nsymbol = \"TOK\"\nmax_leverage = 2.5\n{NOTHING_ROW_TABLE}\n\
         [[mounts]]\nsymbol = \"TOK2\"\n{NOTHING_ROW_TABLE}"
    ))
    .expect("loads");
    let rows = multi.mount_rows();
    assert_eq!((rows[0].max_leverage, rows[1].max_leverage), (Some(2.5), None));

    for bad in ["0.5", "0.0", "-1.0", "inf", "nan"] {
        let err = ProfileDaemon::from_toml_str(&single(&format!("max_leverage = {bad}")))
            .expect_err("not a leverage cap");
        assert!(err.contains("max_leverage") && err.contains("at least 1"), "{bad}: {err}");
    }
    let both = ProfileDaemon::from_toml_str(&format!(
        "max_leverage = 3.0\n[[mounts]]\nsymbol = \"TOK\"\n{NOTHING_ROW_TABLE}"
    ))
    .expect_err("both spellings");
    assert!(both.contains("BOTH spellings") && both.contains("max_leverage"), "{both}");
}

#[test]
fn an_empty_symbol_is_rejected() {
    let err = ProfileDaemon::from_toml_str("symbol = \"\"").unwrap_err();
    assert!(err.contains("symbol"), "error must name the symbol: {err}");
}

#[test]
fn a_profile_with_no_symbol_at_all_is_rejected() {
    let err = ProfileDaemon::from_toml_str("venue = \"polymarket\"").unwrap_err();
    assert!(err.contains("symbol"), "names what is missing: {err}");
}

/// **PM-3**: a mount has ONE name, `symbol` (a polymarket outcome token id too). The retired `token_id` key is
/// refused at load by name, alone or beside `symbol`, never read as nothing.
#[test]
fn the_retired_token_id_key_is_refused_by_name() {
    for toml in [
        format!("venue = \"polymarket\"\ntoken_id = \"TOK\"{NOTHING_TABLE}"),
        format!("symbol = \"BTC\"\ntoken_id = \"TOK\"{NOTHING_TABLE}"),
        format!("[[mounts]]\nvenue = \"polymarket\"\ntoken_id = \"TOK\"\n{NOTHING_ROW_TABLE}"),
    ] {
        let err = ProfileDaemon::from_toml_str(&toml).unwrap_err();
        assert!(err.contains("token_id"), "names the retired key: {err}");
    }
}

/// **The shipped profile shape's maker keys are REFUSED at load** (step 10, Q4): the old top-level
/// `qty`/`half_spread`/`resolution_ts_ms` stop being read, so a profile still setting one is refused
/// NAMING it and where it went, never mounted with the key ignored. The same shape with the keys moved
/// into the maker FILE's table loads and stays live-armable.
#[test]
fn the_shipped_profile_shape_is_refused_by_its_retired_maker_keys_and_loads_once_moved() {
    let token = "71321045679252212594626385532706912750332728571942532289631379312455583992563";
    let shipped = format!(
        "venue = \"polymarket\"\nsymbol = \"{token}\"\ninterval = \"1m\"\ninterval_ms = 60000\n\
         qty = 20.0\nhalf_spread = 0.01\ntick_size = 0.01\nseed_cash = 1000.0\n"
    );
    let err = ProfileDaemon::from_toml_str(&shipped).expect_err("the retired keys are refused");
    assert!(err.contains("this profile sets: qty, half_spread"), "names the keys: {err}");
    assert!(err.contains("[strategy.params]") && err.contains("resolution_ts"), "{err}");
    assert!(err.contains("[strategy] file = "), "names where the maker is selected now: {err}");
    let resolution =
        ProfileDaemon::from_toml_str(&format!("symbol = \"{token}\"\nresolution_ts_ms = 7\n"))
            .expect_err("resolution_ts_ms too");
    assert!(resolution.contains("this profile sets: resolution_ts_ms"), "{resolution}");

    let moved = format!(
        "venue = \"polymarket\"\nsymbol = \"{token}\"\ninterval = \"1m\"\ninterval_ms = 60000\n\
         tick_size = 0.01\nseed_cash = 1000.0\n[strategy]\nfile = \"market_making/spread_maker.rs\"\n\
         [strategy.params]\nqty = 20.0\nhalf_spread = 0.01\n"
    );
    let p = ProfileDaemon::from_toml_str(&moved).expect("the moved shape parses");
    let spec = p.to_mount_spec();
    assert_eq!(spec.symbol, token);
    assert_eq!(spec.seed_cash.to_bits(), 1_000.0_f64.to_bits());
    assert_eq!(p.strategy_name(), "spread_maker", "the file's stem is the name it is known by");
    // ...and it is still live-armable, which is the half a venue-based gate could have broken.
    //
    // ⚠ Since wave 2 (W15) the polymarket feed arm is compiled into every build: this crate's
    // `polymarket` feature is gone with the bridge edge it gated, and whether the VENUE can start is
    // the gatewayhub's answer (a `FeatureAbsent` row in its arming table), not a cfg here.
    assert!(
        p.validate_for_live(&crate::testutil::live_box()).is_ok(),
        "the moved live profile shape stays armable: {:?}",
        p.validate_for_live(&crate::testutil::live_box())
    );
}

/// A venue with no feed arm is refused BY VENUE, not by the token shape: the refusal names the real
/// reason. (This was stated for polymarket in a build without the `polymarket` feature until W15 made
/// that arm unconditional; fxcm is a roster venue that serves no market data, so it has no arm.)
#[test]
fn a_venue_with_no_feed_arm_is_refused_by_venue_not_by_token_shape() {
    let p = ProfileDaemon::from_toml_str(&format!(
        "venue = \"fxcm\"\nsymbol = \"EURUSD\"{NOTHING_TABLE}"
    ))
    .expect("parses");
    let err = p.validate_for_live(&crate::testutil::live_box()).unwrap_err();
    assert!(err.contains("not live-wired in this build"), "names the real reason: {err}");
}

#[test]
fn unknown_field_is_rejected() {
    // deny_unknown_fields catches a typo'd key rather than silently ignoring it.
    let err = ProfileDaemon::from_toml_str(&format!("symbol = \"TOK\"\nbogus = 1{NOTHING_TABLE}"))
        .unwrap_err();
    assert!(!err.is_empty());
    // ...in the `[strategy]` table too.
    let err = ProfileDaemon::from_toml_str(
        "symbol = \"TOK\"\n[strategy]\nfile = \"nothing.rhai\"\nfiel = \"x.rhai\"\n",
    )
    .unwrap_err();
    assert!(err.contains("fiel"), "{err}");
}

#[test]
fn partial_daemon_table_keeps_the_other_default() {
    // Only one of the two daemon knobs set — the other must keep its default.
    let p = ProfileDaemon::from_toml_str(&format!(
        "symbol = \"TOK\"\n[daemon]\nsummary_ms = 1000{NOTHING_TABLE}"
    ))
    .expect("partial daemon table parses");
    assert_eq!(p.daemon.summary_ms, 1_000);
    assert_eq!(p.daemon.shutdown_deadline_ms, 5_000);
}

/// **A mount symbol whose kept row's user name is written another way is refused at LOAD** (the C-D4 follow-up,
/// decision 0290): a lower-case `btcusdt.p` on bybit, a row of `[[mounts]]` the same, and fxcm's `eurusd`. The refusal
/// names the user name; a name already so written (bybit's perpetual `BTCUSDT.P` and its spot pair `BTCUSDT`,
/// hyperliquid's `KPEPEUSDC.P`, oanda's and ibkr's `EURUSD`) and a name the grammar does not read (polymarket's
/// made-up `TOK`) still load.
#[test]
fn a_mount_symbol_the_door_spells_another_way_is_refused_at_load() {
    crate::testutil::hold_door_lists();
    for profile in [
        format!("venue = \"bybit\"\nsymbol = \"btcusdt.p\"{NOTHING_TABLE}"),
        format!("[[mounts]]\nvenue = \"bybit\"\nsymbol = \"btcusdt.p\"\n{NOTHING_ROW_TABLE}"),
    ] {
        let why = ProfileDaemon::from_toml_str(&profile).unwrap_err();
        assert!(why.contains("`btcusdt.p` is written `BTCUSDT.P` on bybit"), "{why}");
    }
    let why = ProfileDaemon::from_toml_str(&format!(
        "venue = \"fxcm\"\nsymbol = \"eurusd\"{NOTHING_TABLE}"
    ))
    .unwrap_err();
    assert!(why.contains("`EURUSD`"), "{why}");
    for ok in [
        "venue = \"bybit\"\nsymbol = \"BTCUSDT.P\"",
        "venue = \"bybit\"\nsymbol = \"BTCUSDT\"",
        "venue = \"hyperliquid\"\nsymbol = \"KPEPEUSDC.P\"",
        "venue = \"oanda\"\nsymbol = \"EURUSD\"",
        "venue = \"ibkr\"\nsymbol = \"EURUSD\"",
        "symbol = \"TOK\"",
    ] {
        ProfileDaemon::from_toml_str(&format!("{ok}{NOTHING_TABLE}"))
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
}
