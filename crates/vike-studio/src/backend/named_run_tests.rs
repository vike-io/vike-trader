use super::*;
use vike_data::TsRange;

fn bar_slice() -> DataSlice {
    DataSlice {
        venue: "binance".into(),
        symbols: vec!["BTCUSDT".into()],
        interval: "1d".into(),
        range: TsRange { start: Some(0), end: Some(86_400_000 * 3) },
        kind: SliceKind::Bars,
    }
}

fn native(name: &str) -> StrategySpec {
    StrategySpec::native_default(name)
}

/// The ordinary case: a native strategy over a bounded bar slice converts.
#[test]
fn a_native_strategy_over_a_bounded_bar_slice_converts() {
    let named = to_named_run_spec(&native("buy_hold"), &bar_slice()).expect("converts");
    assert_eq!(named.strategy, "buy_hold");
    assert_eq!(named.symbol, "BTCUSDT");
    assert_eq!(named.start, 0);
    assert!(named.params.is_empty());
}

/// **THE STRUCTURAL REFUSAL, in the UI's own words.** A Rhai spec cannot become a named run,
/// and the message says why rather than reporting an empty roster or a compile error.
#[test]
fn a_script_cannot_become_a_named_run_and_the_refusal_says_so() {
    let err = to_named_run_spec(&StrategySpec::rhai("fn on_bar(){}"), &bar_slice())
        .expect_err("a script must be refused");
    let msg = err.to_string();
    assert!(msg.contains("carries no source"), "{msg}");
    assert!(msg.contains("Native"), "…and it names the way forward: {msg}");
}

/// The plugin twin: a runtime-loaded strategy is not on the SERVER's compiled-in roster either,
/// so it is refused the same way and for the same reason — never silently substituted.
#[test]
fn a_plugin_cannot_become_a_named_run_and_the_refusal_says_so() {
    let spec = StrategySpec::Plugin {
        name: "my_strat".to_string(),
        sha: "a".repeat(64),
        params: toml::Value::Table(toml::map::Map::new()),
    };
    let err = to_named_run_spec(&spec, &bar_slice()).expect_err("a plugin must be refused");
    let msg = err.to_string();
    assert!(msg.contains("not on that roster"), "{msg}");
    assert!(msg.contains("Native"), "…and it names the way forward: {msg}");
}

/// Every OTHER bound refuses BY NAME too — one row per dimension the request would have named.
#[test]
fn every_bound_refuses_by_name_rather_than_narrowing_the_run() {
    // a tick slice — the window ceiling is counted in bars
    let mut ticks = bar_slice();
    ticks.kind = SliceKind::Ticks;
    assert!(
        to_named_run_spec(&native("buy_hold"), &ticks).unwrap_err().to_string().contains("BAR"),
    );
    // a symbol LIST
    let mut many = bar_slice();
    many.symbols.push("ETHUSDT".into());
    assert!(
        to_named_run_spec(&native("buy_hold"), &many)
            .unwrap_err()
            .to_string()
            .contains("ONE symbol")
    );
    // an OPEN window — the shape that means "the whole store"
    let mut open = bar_slice();
    open.range.end = None;
    assert!(
        to_named_run_spec(&native("buy_hold"), &open)
            .unwrap_err()
            .to_string()
            .contains("BOTH ends")
    );
    // ...and an over-wide one, which comes back from the SHARED validator naming its constant
    let mut wide = bar_slice();
    wide.interval = "1s".into();
    wide.range.end = Some(86_400_000 * 30);
    assert!(
        to_named_run_spec(&native("buy_hold"), &wide)
            .unwrap_err()
            .to_string()
            .contains("NAMED_RUN_MAX_BARS")
    );
}

/// A STRING param is refused rather than dropped, and the reserved source key gets its own
/// sentence — the belt, saying what it is.
#[test]
fn a_string_param_is_refused_and_a_src_param_says_what_it_is() {
    let mut table = toml::map::Map::new();
    table.insert("mode".into(), toml::Value::String("aggressive".into()));
    let err = to_named_run_spec(
        &StrategySpec::native("buy_hold", toml::Value::Table(table)),
        &bar_slice(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("numbers and flags only"), "{err}");

    let mut src = toml::map::Map::new();
    src.insert(vike_model::RESERVED_SRC_KEY.into(), toml::Value::String("fn on_bar(){}".into()));
    let err =
        to_named_run_spec(&StrategySpec::native("buy_hold", toml::Value::Table(src)), &bar_slice())
            .unwrap_err()
            .to_string();
    assert!(err.contains("carries no source"), "{err}");
}

/// Numeric and boolean knobs DO cross, and an integer stays an integer — the reason
/// `NamedParam` splits `Int` from `Num` at all (`Grid::from_params`' `rungs` reads
/// `Value::as_integer`, which answers `None` for a TOML float).
#[test]
fn an_integer_knob_stays_an_integer_across_the_conversion() {
    let mut table = toml::map::Map::new();
    table.insert("rungs".into(), toml::Value::Integer(4));
    table.insert("size".into(), toml::Value::Float(2.5));
    table.insert("live".into(), toml::Value::Boolean(true));
    let named =
        to_named_run_spec(&StrategySpec::native("grid", toml::Value::Table(table)), &bar_slice())
            .expect("converts");
    let by_key = |k: &str| named.params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(by_key("rungs"), Some(NamedParam::Int(4)));
    assert_eq!(by_key("size"), Some(NamedParam::Num(2.5)));
    assert_eq!(by_key("live"), Some(NamedParam::Flag(true)));
}

/// **No offloading backend may DEFAULT to the datahub's address**, because every verb either of
/// them sends is `Plane::Compute` and that daemon refuses those by plane — which sends the
/// reader looking for a key they do not need, rather than at the port they do.
///
/// ⚠ This test used to be `assert_ne!(DEFAULT_COMPUTE_ADDR, DEFAULT_REMOTE_ADDR)`, and it
/// PASSED for the whole time [`Backend::Remote`] was defaulting to 7878 — it asked whether the
/// two constants differed, which was exactly the defect wearing its own guard. The question is
/// not "are they different" but "does either name the DATA daemon", so it is asked of the real
/// `Backend` values against the authority, `vike_config::DEFAULT_DATAHUB_ADDR`, rather than of
/// a local constant that could drift with the thing it checks.
#[test]
fn no_offloading_backend_defaults_to_the_datahub() {
    for backend in [Backend::remote_default(), Backend::named_default()] {
        let addr = backend.addr();
        assert_ne!(
            addr,
            vike_config::DEFAULT_DATAHUB_ADDR,
            "{backend:?} defaults to the DATA daemon, which refuses every Run* verb by plane"
        );
        assert_eq!(addr, vike_config::DEFAULT_BACKTEST_ADDR, "{backend:?}");
    }
    // ...and the CONTROL: the address this must not be is a real, different one, so the
    // assertion above is not passing because both sides are empty or equal by construction.
    assert_ne!(vike_config::DEFAULT_DATAHUB_ADDR, vike_config::DEFAULT_BACKTEST_ADDR);
    // Every surviving variant DIALS - the one that did not is gone, so `addr()` has no
    // `None` arm left to prove. What is worth pinning is that both dial what they were told.
}

/// **The SEARCH refusal — 0064's decision 3, bound 1, which is the largest of that verb's
/// bounds and the only structural one.** It fires on the Named backend and on NOTHING else, so
/// adding it could not have narrowed what the Remote backend may do.
#[test]
fn a_search_is_refused_on_the_named_backend_and_nowhere_else() {
    let named = Backend::Named { addr: DEFAULT_COMPUTE_ADDR.to_string() };
    for action in ["sweep", "walk-forward"] {
        let msg = named_backend_search_refusal(&named, action)
            .unwrap_or_else(|| panic!("a {action} must be refused on the Named backend"));
        assert!(msg.contains(action), "the refusal names the action it stopped: {msg}");
        assert!(
            msg.contains("ONE parameter set"),
            "…and says what the bound IS, rather than reading as a missing feature: {msg}"
        );
    }
    // ...and the backend that DOES carry a search is untouched, which is what makes this a
    // bound on the new backend rather than a regression on the old one.
    //
    // ⚠ This was a LOOP over two backends until `Backend::Local` was deleted; with one
    // element left clippy's `single_element_loop` refuses it, and rightly - a loop over one
    // thing reads as a set and is not one.
    let other = Backend::Remote { addr: DEFAULT_COMPUTE_ADDR.to_string() };
    assert_eq!(named_backend_search_refusal(&other, "sweep"), None, "{other:?}");
}
