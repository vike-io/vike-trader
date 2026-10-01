use super::*;

fn bar(venue: &str, symbol: &str, interval: &str) -> SeriesId {
    SeriesId::per_symbol("bar", venue, symbol, Some(interval.to_string()))
}

fn grouped(kind: &str, venue: &str, group: &str) -> SeriesId {
    SeriesId::grouped(kind, venue, group)
}

fn all() -> Vec<SeriesId> {
    vec![
        bar("hyperliquid", "BTC", "1h"),
        bar("hyperliquid", "BTC", "1m"),
        bar("hyperliquid", "ETH", "1h"),
        bar("binance", "BTCUSDT", "1h"),
        grouped("book", "polymarket", "btc-5m"),
        SeriesId::per_symbol("book", "polymarket", "0x1234", None),
    ]
}

/// The headline: a `kind`+`venue` selector sweeps the subtree and NOTHING outside it.
#[test]
fn a_selector_intersects_the_stores_own_enumeration() {
    let sel = SeriesSelector::new("bar", "hyperliquid");
    let got = select_series(&all(), &sel);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(got.iter().all(|id| id.venue == "hyperliquid"));
}

/// A typo matches NOTHING. It never matches something adjacent, because the match is over ids
/// the store produced rather than over a path this code built.
#[test]
fn a_typo_matches_nothing_rather_than_something_adjacent() {
    let mut sel = SeriesSelector::new("bar", "hyperliquld");
    assert!(select_series(&all(), &sel).is_empty());
    sel = SeriesSelector::new("bar", "hyperliquid");
    sel.symbol = Some("BTCUSDT".to_string()); // binance's symbol, hyperliquid's venue
    assert!(select_series(&all(), &sel).is_empty());
}

/// `--symbol` and `--group` select DISJOINT layouts, and naming neither spans both — the
/// alternative `SeriesId` actually encodes.
#[test]
fn symbol_and_group_select_disjoint_layouts_and_neither_spans_both() {
    let mut sel = SeriesSelector::new("book", "polymarket");
    assert_eq!(select_series(&all(), &sel).len(), 2, "both layouts");

    sel.group = Some("btc-5m".to_string());
    let g = select_series(&all(), &sel);
    assert_eq!(g.len(), 1);
    assert!(g[0].group.is_some());

    sel.group = None;
    sel.symbol = Some("0x1234".to_string());
    let s = select_series(&all(), &sel);
    assert_eq!(s.len(), 1);
    assert!(s[0].group.is_none(), "a named --symbol must not select the grouped twin");
}

/// An omitted `--interval` is a wildcard, and a named one is exact.
#[test]
fn interval_is_a_wildcard_when_omitted() {
    let mut sel = SeriesSelector::new("bar", "hyperliquid");
    sel.symbol = Some("BTC".to_string());
    assert_eq!(select_series(&all(), &sel).len(), 2, "both intervals");
    sel.interval = Some("1h".to_string());
    assert_eq!(select_series(&all(), &sel).len(), 1);
}

/// A fully-pinned selector is not a sweep; ANY wildcarded dimension is.
#[test]
fn a_sweep_is_any_wildcarded_dimension() {
    let mut sel = SeriesSelector::new("bar", "binance");
    assert!(sel.is_sweep());
    sel.symbol = Some("BTCUSDT".to_string());
    assert!(sel.is_sweep(), "interval still wildcarded");
    sel.interval = Some("1h".to_string());
    assert!(!sel.is_sweep());

    // A grouped series is fully named by kind/venue/group — it HAS no interval dimension.
    let mut g = SeriesSelector::new("book", "polymarket");
    g.group = Some("btc-5m".to_string());
    assert!(!g.is_sweep());

    // ...and so is a per-symbol TICK series, whose leaf carries no `interval=` segment. This is
    // the case a flag COUNT gets wrong, and getting it wrong would demand more of the CLI than
    // the GUI's own Delete asks for the identical act.
    let mut t = SeriesSelector::new("trade", "binance");
    t.symbol = Some("BTCUSDT".to_string());
    assert!(!t.is_sweep());

    // An unknown kind reads as a sweep — the conservative direction.
    let mut u = SeriesSelector::new("nope", "binance");
    u.symbol = Some("BTCUSDT".to_string());
    assert!(u.is_sweep());
}

#[test]
fn the_shape_rules_refuse_what_an_operator_types_wrong() {
    let mut sel = SeriesSelector::new("", "binance");
    assert!(sel.validate_shape().unwrap_err().contains("--kind is required"));

    sel = SeriesSelector::new("bar", "");
    assert!(sel.validate_shape().unwrap_err().contains("--venue is required"));

    sel = SeriesSelector::new("bar", "binance");
    sel.symbol = Some("BTC".into());
    sel.group = Some("g".into());
    assert!(sel.validate_shape().unwrap_err().contains("ALTERNATIVES"));

    sel = SeriesSelector::new("book", "polymarket");
    sel.group = Some("g".into());
    sel.interval = Some("1h".into());
    assert!(sel.validate_shape().unwrap_err().contains("--interval does not apply"));

    sel = SeriesSelector::new("bar", "binance");
    sel.symbol = Some("BTC*".into());
    assert!(sel.validate_shape().unwrap_err().contains("glob"));

    sel = SeriesSelector::new("bar", "binance");
    sel.symbol = Some("  ".into());
    assert!(sel.validate_shape().unwrap_err().contains("EMPTY value"));
}

/// The store-side rules read `STORE_KINDS`, never a roster copied into a client.
#[test]
fn the_layout_rules_come_from_the_kind_table() {
    let sel = SeriesSelector::new("nope", "binance");
    let err = sel.validate_against_kinds().unwrap_err();
    assert!(err.contains("unknown kind"), "{err}");
    assert!(err.contains("bar"), "the message names the whole set: {err}");

    let mut iv = SeriesSelector::new("trade", "binance");
    iv.interval = Some("1h".into());
    assert!(iv.validate_against_kinds().unwrap_err().contains("only `bar`"));

    // `bar` declares no grouped form, so `--group` on it selects nothing and says so.
    let mut g = SeriesSelector::new("bar", "binance");
    g.group = Some("x".into());
    assert!(g.validate_against_kinds().unwrap_err().contains("no GROUPED form"));

    SeriesSelector::new("bar", "binance").validate_against_kinds().unwrap();
}

fn plan(commits: Vec<&str>, produced_by: Option<&str>) -> RemovalPlan {
    RemovalPlan {
        selector: SeriesSelector::new("bar", "hyperliquid"),
        produced_by: produced_by.map(str::to_string),
        series: vec![PlannedSeries::new(
            bar("hyperliquid", "BTC", "1h"),
            SeriesCoverage { rows: 10, bytes: 99, parts: 1, dates: 1, ..Default::default() },
            commits.into_iter().map(str::to_string).collect(),
            produced_by,
        )],
    }
}

/// **The MIXED-SERIES case, which is why this is an assertion.** A filter would have skipped
/// this series and deleted the rest; the assertion refuses the whole run and names the key.
#[test]
fn one_foreign_key_refuses_the_whole_run_and_names_it() {
    let p = plan(vec!["panel_bars:hl:BTC:1", "binance:BTCUSDT:1h:0-1"], Some("panel_bars:"));
    let refusals = p.verdict().unwrap_err();
    assert_eq!(refusals.len(), 1);
    assert!(refusals[0].contains("binance:BTCUSDT:1h:0-1"), "{}", refusals[0]);
    assert!(p.lines().iter().any(|l| l.contains("REFUSED")), "{:?}", p.lines());
}

/// A series recording no keys cannot satisfy an assertion — and IS deletable without one.
#[test]
fn an_empty_commit_log_refuses_under_an_assertion_and_passes_without_one() {
    assert!(plan(vec![], Some("panel_bars:")).verdict().is_err());
    plan(vec![], None).verdict().expect("no assertion, no refusal");
    assert!(
        plan(vec![], None).lines().iter().any(|l| l.contains("none recorded")),
        "the absence is DISCLOSED even when it is not a refusal"
    );
}

/// A REMOVED producer's key classifies as unknown and is reported in those words — the case
/// that motivated the literal-prefix spelling, and a place a refusal would be wrong.
#[test]
fn a_removed_producers_key_is_reported_not_refused() {
    let p = plan(vec!["panel_bars:hl:BTC:1", "panel_bars:hl:BTC:2"], Some("panel_bars:"));
    p.verdict().expect("every key carries the prefix");
    let lines = p.lines();
    assert!(lines.iter().any(|l| l.contains(NO_DECLARED_PRODUCER)), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("×2")), "keys fold to one counted group: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("SATISFIED")), "{lines:?}");
}

/// A key a declared producer DOES build resolves to that producer's path.
#[test]
fn a_declared_producers_key_names_the_file_that_builds_it() {
    let p = plan(vec!["demo-tape:v1:BTCUSDT:1h"], None);
    assert_eq!(p.series[0].key_groups[0].prefix, "demo-tape:v");
    assert_eq!(p.series[0].key_groups[0].producers, vec!["crates/vike-data/src/demo.rs"]);
}

/// A day-chunked venue series — the OANDA S5 lane commits one key per UTC day,
/// `oanda:EUR_USD:5s:{from}-{to}`, thousands per series — folds into ONE counted group that names
/// the klines producer. Without the shape rule every key was its own group, each line marked
/// [`NO_DECLARED_PRODUCER`], and the origin check warned on a producer that IS declared.
#[test]
fn a_day_chunked_venue_series_folds_to_one_group_naming_the_klines_producer() {
    let day = 86_400_000_i64;
    let t0 = 1_441_756_800_000_i64;
    let keys: Vec<String> = (0..5)
        .map(|i| format!("oanda:EUR_USD:5s:{}-{}", t0 + i * day, t0 + (i + 1) * day - 1))
        .collect();
    let p = plan(keys.iter().map(String::as_str).collect(), None);
    let groups = &p.series[0].key_groups;
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0].prefix, "oanda:EUR_USD:5s:");
    assert_eq!(groups[0].count, 5);
    assert_eq!(groups[0].producers, vec!["crates/vike-backfill/src/klines.rs"]);
    assert!(
        !p.lines().iter().any(|l| l.contains(NO_DECLARED_PRODUCER)),
        "a declared producer must not render as undeclared: {:?}",
        p.lines()
    );
    // Two series' keys stay two groups: the group is per (venue, symbol, interval), not per venue.
    let two = plan(vec!["oanda:EUR_USD:5s:0-1", "oanda:USD_JPY:5s:0-1"], None);
    assert_eq!(two.series[0].key_groups.len(), 2);
}

/// ⚠ Without `--produced-by`, an unrecognised key IS its own group — which is honest and, for
/// the case this feature exists for, unreadable: a months-long backfill's per-window keys would
/// render one plan line each. The operator's own prefix is what folds them.
#[test]
fn the_asserted_prefix_folds_the_keys_no_declared_producer_claims() {
    let unasserted = plan(vec!["panel_bars:hl:BTC:1", "panel_bars:hl:BTC:2"], None);
    assert_eq!(unasserted.series[0].key_groups.len(), 2, "each unknown key is its own group");

    let asserted = plan(vec!["panel_bars:hl:BTC:1", "panel_bars:hl:BTC:2"], Some("panel_bars:"));
    assert_eq!(asserted.series[0].key_groups.len(), 1);
    assert_eq!(asserted.series[0].key_groups[0].count, 2);
    assert!(
        asserted.series[0].key_groups[0].producers.is_empty(),
        "folding under the operator's prefix must not INVENT a producer for it"
    );
}

/// Zero matches is a plan that says so, with the totals absent rather than zeroed into a table.
#[test]
fn nothing_matched_renders_as_nothing_to_delete() {
    let p = RemovalPlan {
        selector: SeriesSelector::new("bar", "binance"),
        produced_by: None,
        series: Vec::new(),
    };
    assert_eq!(p.matched(), 0);
    assert!(p.verdict().is_ok(), "an empty set satisfies every assertion");
    assert!(p.lines().iter().any(|l| l.contains("matched 0 series")), "{:?}", p.lines());
}

/// The id rendering resolves the grouped/per-symbol alternative rather than printing an empty
/// `symbol=` for half the store.
#[test]
fn an_id_renders_its_scope_rather_than_a_blank_symbol() {
    assert_eq!(describe_id(&bar("hyperliquid", "BTC", "1h")), "bar/hyperliquid/symbol=BTC/1h");
    assert_eq!(
        describe_id(&grouped("book", "polymarket", "btc-5m")),
        "book/polymarket/group=btc-5m"
    );
}
