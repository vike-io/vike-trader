//! The `[engine]` knobs: cash gate, margin, volume limit, caps, multipliers, leverage, sessions.

use super::*;

#[test]
fn cash_gate_reaches_the_profile() {
    let toml = BAR_TOML.replace("# engine", "# engine\ncash_gate = true");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(p.engine.cash_gate);
}

/// `cash_gate`'s ONLY readers are `StrategyEngine::run`'s `granular` mask and its
/// `fill_step_gated` branch — both inside the BAR loop — and `fill_step_gated`'s single caller
/// is that branch. `run_ticks` never consults it, so a tick profile setting it would parse,
/// validate, reach `StrategyEngineConfig` and change nothing: the defect this whole surface exists to
/// refuse, which is why it now joins the lane-asymmetric refusals.
#[test]
fn cash_gate_is_bar_mode_only() {
    let toml = TICK_TOML.replace("snap_to_properties = true", "cash_gate = true");
    refused_at_load(&toml, HarnessError::Validation, &["cash_gate", "bar-mode only"]);
}

/// The companion: an explicit `false` on a tick profile configures nothing and must keep
/// validating — the refusal is on the value that would mislead, not on the key's presence.
#[test]
fn cash_gate_false_on_a_tick_profile_is_accepted() {
    let toml = TICK_TOML.replace("snap_to_properties = true", "cash_gate = false");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("an explicit false configures nothing")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(!p.engine.cash_gate);
}

/// `toml` with an `[engine.in_bar]` table of `rules` ahead of its `[strategy]`.
fn with_in_bar(toml: &str, rules: &str) -> String {
    toml.replace("[strategy]\n", &format!("[engine.in_bar]\n{rules}\n\n[strategy]\n"))
}

#[test]
fn in_bar_rules_reach_the_profile_each_off_unless_stated() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(p.engine.in_bar.is_none(), "absent, every rule off: byte-identical");
    let toml = with_in_bar(BAR_TOML, "path_walk = true\nprice_order = true");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    let rules = p.engine.in_bar.expect("stated");
    let stated =
        vike_simulator::InBarRules { path_walk: true, price_order: true, ..Default::default() };
    assert_eq!(rules, stated);
}

/// `StrategyEngine::run` alone reads `StrategyEngineConfig::in_bar`: a tick profile setting a rule would change nothing.
#[test]
fn in_bar_rules_are_bar_mode_only() {
    let toml = with_in_bar(TICK_TOML, "queue_order = true");
    refused_at_load(&toml, HarnessError::Validation, &["engine.in_bar", "bar-mode only"]);
    let off = with_in_bar(TICK_TOML, "queue_order = false");
    ProfileBacktest::from_toml_str(&off)
        .expect("a rule stated off configures nothing")
        .with_stand_in(vike_strategy_lab::stand_in);
}

/// The shared-cash lane fills each bar whole and walks no path, so the rules that walk one would be set and ignored.
#[test]
fn the_walking_in_bar_rules_do_not_go_with_the_shared_cash_lane() {
    let gated = BAR_TOML.replace("# engine", "# engine\ncash_gate = true");
    let toml = with_in_bar(&gated, "path_walk = true");
    refused_at_load(&toml, HarnessError::Validation, &["engine.in_bar.path_walk", "cash_gate"]);
    let queue = with_in_bar(&gated, "queue_order = true");
    ProfileBacktest::from_toml_str(&queue)
        .expect("the queue order is the book's, any lane's")
        .with_stand_in(vike_strategy_lab::stand_in);
    // this close is no walk: the shared-cash phase meets it at the head of the next step
    let close = with_in_bar(&gated, "fill_at_close = true");
    ProfileBacktest::from_toml_str(&close)
        .expect("the close goes with the shared-cash lane")
        .with_stand_in(vike_strategy_lab::stand_in);
}

#[test]
fn cash_gate_defaults_off() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(!p.engine.cash_gate, "the default must stay byte-identical to before this key");
}

#[test]
fn margin_keys_reach_the_profile() {
    let toml = BAR_TOML.replace(
        "# engine",
        "# engine\nmaint_margin = 0.005\nliquidation_buffer = 0.2\n\
             venue_style_liquidation = true",
    );
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.maint_margin, 0.005);
    assert_eq!(p.engine.liquidation_buffer, 0.2);
    assert!(p.engine.venue_style_liquidation);
}

/// `liquidation_buffer`'s engine default is 0.10, NOT 0.0 — a serde `Default::default()` here would
/// silently change every profile that omits the key.
#[test]
fn liquidation_buffer_defaults_to_the_engine_value() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.liquidation_buffer, 0.10);
    assert_eq!(p.engine.maint_margin, 0.0);
    assert!(!p.engine.venue_style_liquidation);
}

#[test]
fn a_negative_maint_margin_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\nmaint_margin = -0.01");
    refused_at_load(&toml, HarnessError::Validation, &["maint_margin"]);
}

#[test]
fn volume_limit_reaches_the_profile() {
    let toml = BAR_TOML.replace("# engine", "# engine\nvolume_limit = 0.05");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.volume_limit, Some(0.05));
}

#[test]
fn volume_limit_defaults_to_none() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.volume_limit, None);
}

/// A participation cap above 1.0 claims more than the whole bar traded.
#[test]
fn a_volume_limit_over_one_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\nvolume_limit = 1.5");
    refused_at_load(&toml, HarnessError::Validation, &["volume_limit"]);
}

#[test]
fn position_capabilities_reach_the_profile() {
    let toml = BAR_TOML.replace(
        "# engine",
        "# engine\nmax_open_positions = 10\nmax_open_long = 6\nmax_open_short = 4",
    );
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.max_open_positions, 10);
    assert_eq!(p.engine.max_open_long, 6);
    assert_eq!(p.engine.max_open_short, 4);
}

/// 0 is the engine's "unlimited", which is what every profile did before these keys.
#[test]
fn position_capabilities_default_to_unlimited() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.max_open_positions, 0);
    assert_eq!(p.engine.max_open_long, 0);
    assert_eq!(p.engine.max_open_short, 0);
}

/// [`BAR_TOML`] on ibkr futures named by their bare roots, whose unit the catalog does not fix by name
/// (`vike_catalog::contract_value` answers `None`): a profile's multiplier is the run's only source there.
fn ibkr_futures_toml() -> String {
    BAR_TOML
        .replace("venue = \"binance\"", "venue = \"ibkr\"")
        .replace("[\"BTCUSDT\", \"ETHUSDT\"]", "[\"ES\", \"NQ\"]")
}

#[test]
fn multipliers_reach_the_profile() {
    let toml = ibkr_futures_toml().replace(
        "# engine",
        "# engine\nmultiplier = 1.0\n\n[engine.multipliers]\n\
             ES = 50.0\nNQ = 20.0",
    );
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.multiplier, 1.0);
    assert_eq!(p.engine.multipliers.get("ES"), Some(&50.0));
    assert_eq!(p.engine.multipliers.get("NQ"), Some(&20.0));
}

/// **A multiplier the catalog contradicts is refused by name** (decision 0170's catalog amendment): one unit of
/// binance `BTCUSDT` is one coin, which its paper book and its live mount book, so a run at 50 would book every
/// notional, fee and PnL fifty times live's. Both the global key and a per-symbol row are judged, and the
/// refusal names the key, the symbol, the venue and the catalog's unit.
#[test]
fn a_multiplier_the_catalog_contradicts_is_refused_by_name() {
    let global = BAR_TOML.replace("# engine", "# engine\nmultiplier = 50.0");
    refused_at_load(
        &global,
        HarnessError::Validation,
        &["engine.multiplier sizes BTCUSDT on binance at 50", "catalog values one unit of it at 1"],
    );
    let row = BAR_TOML.replace("# engine", "# engine\n\n[engine.multipliers]\nETHUSDT = 20.0");
    refused_at_load(
        &row,
        HarnessError::Validation,
        &["engine.multipliers sizes ETHUSDT on binance at 20", "state 1"],
    );
}

/// ...and a multiplier EQUAL to the catalog's stays accepted, stated globally or per symbol.
#[test]
fn a_multiplier_equal_to_the_catalogs_is_accepted() {
    let toml = BAR_TOML
        .replace("# engine", "# engine\nmultiplier = 1.0\n\n[engine.multipliers]\nBTCUSDT = 1.0");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("the catalog's own unit")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.multipliers.get("BTCUSDT"), Some(&1.0));
}

#[test]
fn multiplier_defaults_to_one_and_the_table_is_empty() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.multiplier, 1.0);
    assert!(p.engine.multipliers.is_empty());
}

#[test]
fn a_zero_multiplier_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\nmultiplier = 0.0");
    refused_at_load(&toml, HarnessError::Validation, &["multiplier"]);
}

/// The per-symbol table gets its own negative-value test, distinct from the global
/// `multiplier` scalar above — `validate` is first-failure-wins, so this sets exactly one bad
/// per-symbol value and leaves the global scalar at its valid default. The key is a symbol
/// the slice ACTUALLY loads, so the value rule is what fails rather than the key rule below.
#[test]
fn a_zero_per_symbol_multiplier_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\n\n[engine.multipliers]\nBTCUSDT = 0.0");
    refused_at_load(&toml, HarnessError::Validation, &["multipliers", "BTCUSDT", "> 0"]);
}

/// A `multipliers` key naming a symbol the run does not load is silently inert today:
/// `StrategyEngine::new` matches by EXACT name and otherwise falls back to the global
/// `multiplier`, so a typo'd row leaves every position sized at the global while the profile
/// reads as contract-sized. The slice is named back so the typo is visible in the message.
#[test]
fn a_multiplier_for_a_symbol_outside_the_slice_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\n\n[engine.multipliers]\nES = 50.0");
    refused_at_load(
        &toml,
        HarnessError::Validation,
        &["engine.multipliers.ES", "BTCUSDT", "ETHUSDT"],
    );
}

/// ...and the companion, so the rule above cannot be satisfied by refusing everything: a row
/// naming a symbol the slice DOES load still validates.
#[test]
fn a_multiplier_for_a_symbol_in_the_slice_is_accepted() {
    let toml =
        ibkr_futures_toml().replace("# engine", "# engine\n\n[engine.multipliers]\nNQ = 50.0");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("the symbol is in the slice")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.multipliers.get("NQ"), Some(&50.0));
}

#[test]
fn leverage_keys_reach_the_profile() {
    let toml = BAR_TOML.replace("# engine", "# engine\nleverage = 5.0\nclamp_to_leverage = true");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.leverage, Some(5.0));
    assert!(p.engine.clamp_to_leverage);
}

#[test]
fn leverage_defaults_to_none() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.leverage, None);
    assert!(!p.engine.clamp_to_leverage);
}

#[test]
fn a_non_positive_leverage_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\nleverage = 0.0");
    refused_at_load(&toml, HarnessError::Validation, &["leverage"]);
}

/// `clamp_to_leverage` with no `leverage` set has nothing to clamp to — the brief's step 4
/// adds this rule but its own step 1 test list omits a case for it, so this closes that gap.
#[test]
fn clamp_to_leverage_without_leverage_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\nclamp_to_leverage = true");
    refused_at_load(&toml, HarnessError::Validation, &["clamp_to_leverage"]);
}

/// The end state the two older rules left UNSTATED: `clamp_to_leverage` is unavailable to any
/// profile carrying `[risk]`. Before the reorder, this exact profile was told to add
/// `engine.leverage` — and adding it was then refused by the next rule, a dead end with the
/// governing constraint named nowhere.
#[test]
fn clamp_to_leverage_with_a_risk_table_is_rejected() {
    let toml = BAR_TOML.replace(
        "# engine",
        "# engine\nclamp_to_leverage = true\n\n[risk]\nmax_notional_per_order = 1000.0",
    );
    match ProfileBacktest::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            let m = m.to_string();
            assert!(m.contains("clamp_to_leverage"), "{m}");
            assert!(m.contains("[risk]"), "{m}");
            // The misdirection this replaces: never send the operator to add a key the next
            // rule refuses.
            assert!(
                !m.contains("needs engine.leverage"),
                "the old rule-18 misdirection must not fire first: {m}"
            );
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// ...and with `engine.leverage` supplied as well, the SAME refusal must answer — the clamp
/// rule is what governs, whichever way the operator arrived at the combination.
#[test]
fn clamp_to_leverage_with_a_risk_table_and_leverage_is_rejected_the_same_way() {
    let toml = BAR_TOML.replace(
        "# engine",
        "# engine\nleverage = 5.0\nclamp_to_leverage = true\n\n[risk]\n\
             max_notional_per_order = 1000.0",
    );
    refused_at_load(&toml, HarnessError::Validation, &["clamp_to_leverage"]);
}

/// `OrderBrokerSimulator::build_risk_gate` matches `(&p.risk_limits, p.leverage)`; with the clamp OFF its
/// `(Some(l), _)` arm takes `risk_limits` UNCONDITIONALLY whenever `[risk]` is present — even
/// one that never sets `max_leverage` — and silently drops `engine.leverage`. Declaring both
/// must be a named refusal, not a silent precedence pick. (The clamp-ON case has the opposite
/// precedence and its own refusal, two tests above.) The check once read `"[risk]"` OR `"risk"`;
/// the `"[risk]"` alternative was dead (a message holding it holds `"risk"`), so it is now just
/// `"risk"`.
#[test]
fn engine_leverage_with_a_risk_table_is_rejected() {
    let toml = BAR_TOML
        .replace("# engine", "# engine\nleverage = 5.0\n\n[risk]\nmax_notional_per_order = 1000.0");
    refused_at_load(&toml, HarnessError::Validation, &["engine.leverage", "risk"]);
}

/// The companion to the rejection above: `engine.leverage` with NO `[risk]` table is exactly
/// the shape `OrderBrokerSimulator::build_risk_gate`'s `(None, Some(leverage))` arm consumes, and must keep
/// working.
#[test]
fn engine_leverage_alone_with_no_risk_table_still_works() {
    let toml = BAR_TOML.replace("# engine", "# engine\nleverage = 5.0");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("leverage alone, no [risk] table")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.leverage, Some(5.0));
    assert!(p.risk.is_none());
}

/// `engine.max_mount_leverage` (the run's mount leverage cap) reaches the profile BESIDE a `[risk]`
/// table, which `engine.leverage` may not: it is the mount's figure, no second margin source.
/// Absent, it is no cap.
#[test]
fn the_mount_leverage_cap_reaches_the_profile_beside_a_risk_table() {
    let toml = BAR_TOML.replace(
        "# engine",
        "# engine\nmax_mount_leverage = 3.0\n\n[risk]\nmax_notional_per_order = 1000.0",
    );
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("the cap beside [risk]")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.max_mount_leverage, Some(3.0));
    assert!(p.risk.is_some());
    let bare = ProfileBacktest::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(bare.engine.max_mount_leverage, None);
}

/// A cap the judge would read as OFF is refused at load, never run as no cap: below 1, and not a
/// number.
#[test]
fn a_mount_leverage_cap_that_is_no_finite_figure_of_at_least_one_is_rejected() {
    for figure in ["0.5", "nan", "inf"] {
        let key = format!("# engine\nmax_mount_leverage = {figure}");
        let toml = BAR_TOML.replace("# engine", &key);
        refused_at_load(&toml, HarnessError::Validation, &["engine.max_mount_leverage"]);
    }
}

#[test]
fn settlement_period_reaches_the_profile() {
    let toml = BAR_TOML.replace("# engine", "# engine\nsettlement_period_ms = 86400000");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.settlement_period_ms, Some(86_400_000));
}

#[test]
fn settlement_period_defaults_to_none() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.settlement_period_ms, None);
}

#[test]
fn a_non_positive_settlement_period_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\nsettlement_period_ms = 0");
    refused_at_load(&toml, HarnessError::Validation, &["settlement_period_ms"]);
}

/// `[engine.quotes]` states the law a tagged plain limit fills by; absent, the strategy FILE's
/// header decides (`[strategy] optimistic_quotes`: on for the makers, off for a file that declares
/// nothing and where no file was read); a misspelt key in the table is refused.
#[test]
fn engine_quotes_states_the_maker_law_and_absent_the_file_header_decides() {
    let law = crate::run::profile::optimistic_quotes;
    let header =
        "//! ```toml\n//! [strategy]\n//! optimistic_quotes = true\n//! ```\nfn on_bar() {}\n";
    let maker = StrategySource::of(header.to_string());
    assert!(maker.optimistic_quotes, "the header's word is read");
    let declaring = |strategy: &ProfileStrategy, source: Option<StrategySource>| ProfileStrategy {
        source,
        ..strategy.clone()
    };
    let p = ProfileBacktest::from_toml_str(BAR_TOML).expect("parses");
    assert!(p.engine.quotes.is_none());
    let as_maker = declaring(p.strategy.as_ref().unwrap(), Some(maker.clone()));
    assert_eq!(law(p.engine.quotes, Some(&as_maker)), (true, false), "a maker's default");
    let plain = declaring(
        p.strategy.as_ref().unwrap(),
        Some(StrategySource::of("fn on_bar() {}".to_string())),
    );
    assert_eq!(law(p.engine.quotes, Some(&plain)), (false, false), "a file that declares nothing");
    assert_eq!(law(p.engine.quotes, p.strategy.as_ref()), (false, false), "no file read");
    let table = |body: &str| BAR_TOML.replace("[strategy]\n", &format!("{body}\n\n[strategy]\n"));
    let off = table("[engine.quotes]\noptimistic = false");
    let p = ProfileBacktest::from_toml_str(&off).expect("parses");
    let as_maker = declaring(p.strategy.as_ref().unwrap(), Some(maker));
    assert_eq!(law(p.engine.quotes, Some(&as_maker)), (false, true), "a stated key wins");
    let typo = table("[engine.quotes]\noptimistc = true");
    assert!(ProfileBacktest::from_toml_str(&typo).is_err(), "a misspelt key is refused");
}

/// The strategy FILE says when a bar's market order fills (`[strategy] fill`, decision 0285's amendment
/// of 2026-11-16): `close` turns `InBarRules::fill_at_close` on, `next_open` and a file that declares
/// nothing leave it off, and the profile's `[engine.in_bar] fill_at_close = true` (the labs' door, a
/// run with no file) fills at the close whatever the file says. Every lane takes it (review FIX, finding
/// 5): the shared-cash lane fills at the close too, and a tick replay places no bar order.
#[test]
fn the_strategy_files_fill_word_resolves_the_close_rule_and_the_profile_key_overrides() {
    use crate::run::profile::in_bar_rules;
    use vike_simulator::InBarRules;
    let file = |fill: &str| {
        StrategySource::of(format!(
            "//! ```toml\n//! [strategy]\n//! fill = \"{fill}\"\n//! ```\nfn on_bar() {{}}\n"
        ))
    };
    assert!(file("close").fill_at_close && !file("next_open").fill_at_close);
    let p = ProfileBacktest::from_toml_str(BAR_TOML).expect("parses");
    let with =
        |source: Option<StrategySource>| ProfileStrategy { source, ..p.strategy.clone().unwrap() };
    let close = |strategy: &ProfileStrategy, in_bar| in_bar_rules(in_bar, Some(strategy));
    let (rules, by_header) = close(&with(Some(file("close"))), None);
    assert!(rules.fill_at_close && by_header, "the header's close");
    assert_eq!(
        rules,
        InBarRules { fill_at_close: true, ..InBarRules::default() },
        "and nothing else"
    );
    assert_eq!(close(&with(Some(file("next_open"))), None), (InBarRules::default(), false));
    assert_eq!(close(&with(None), None), (InBarRules::default(), false), "no file read");
    let stated = InBarRules { fill_at_close: true, path_walk: true, ..InBarRules::default() };
    assert_eq!(
        close(&with(Some(file("next_open"))), Some(stated)),
        (stated, false),
        "the override"
    );
    assert_eq!(close(&with(Some(file("close"))), Some(stated)), (stated, false));
}

/// A run serves its inputs' whole series (`vike_simulator::StrategyEngineConfig::input_whole`) to a
/// file whose header says `runs_on = "backtest"` and to nothing else: a paper or live file, a text with
/// no header, a run that read no file.
#[test]
fn only_a_backtest_only_file_is_served_the_whole_series() {
    let header = |runs_on: &str| {
        format!(
            "//! ```toml\n//! [strategy]\n//! runs_on = \"{runs_on}\"\n//! ```\nfn on_bar() {{}}\n"
        )
    };
    assert!(StrategySource::of(header("backtest")).backtest_only);
    let rust = vike_strategy::file::scan::StrategyFileLanguage::Rust;
    let (read, _) = StrategySource::read(header("backtest"), rust).expect("a header");
    assert!(read.backtest_only, "the door that reads a file reads the same word");
    for runs_on in ["paper", "live"] {
        assert!(!StrategySource::of(header(runs_on)).backtest_only, "{runs_on}");
    }
    assert!(!StrategySource::of("fn on_bar() {}".to_string()).backtest_only, "no header");
}

#[test]
fn session_gate_defaults_off() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(!p.engine.session_gate);
}

/// RULED (see `ProfileEngine::session_gate`'s doc): step 1 found no by-name `SessionCalendar`
/// lookup, so this task's deliverable is `session_gate` alone — no `[engine.sessions]` table.
/// The brief's own third test was written as a decision point that passed either way; this
/// replaces it with the real assertion for the scope that was actually chosen: the gate parses
/// and validates cleanly with no calendar configured.
///
/// ⚠ This comment used to add "fail-permissive always-open at runtime", and that was FALSE as
/// a general claim — `crate::run::tick_replay::replay_ticks` sets `default_venue` unconditionally
/// and `bar_engine_params` sets it under `snap_to_properties`, so each symbol resolves through
/// `vike_venues::session_for`, which hands back a real `FX_WEEK` calendar for the FX/CFD
/// venues. Arming the gate with no calendar table is a MODELLING choice, not a no-op. What
/// this test asserts is only what its name says: no calendar table is needed to LOAD it — and so
/// that the knob reaches the profile at all.
#[test]
fn session_gate_without_a_calendar_parses_and_validates_cleanly() {
    let toml = BAR_TOML.replace("# engine", "# engine\nsession_gate = true");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("no calendar table needed to arm the gate")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(p.engine.session_gate);
}

/// Every `DecideMode` word resolves in any case, absent is the default, and anything else is the one
/// unknown-mode refusal (whose roster renders from `DecideMode::WORDS`, `crate::run::refusals`'
/// `DECIDE_WORDS`).
#[test]
fn every_decide_word_resolves_and_another_is_refused() {
    let err = decide_mode(Some("Sideways")).expect_err("not a decide word");
    assert_eq!(
        err,
        HarnessError::from(ProfileRefusal::EngineDecideUnknown { other: "sideways".into() })
    );
    for word in DecideMode::WORDS {
        assert_eq!(
            decide_mode(Some(&word.to_ascii_uppercase())).ok().map(DecideMode::word),
            Some(word)
        );
    }
    assert_eq!(decide_mode(None).ok(), Some(DecideMode::default()));
}
