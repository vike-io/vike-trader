//! `[strategy.params]` strictness against the FILE's header (round-2 review, blocker 2), and the
//! effective-params echo.
//!
//! The `.rs` files here are header-only twins of today's registry rows (`super::GRID_RS` and its
//! siblings); they open through the stand-in of the sandboxed process (`super::stand_in`), so what
//! is proven is the daemon's half: the header's checks, the sentence around each finding, and what
//! reaches the operator from the file's own `refuse` and `echo`.

use super::*;

/// A hyperliquid/BTC profile naming `file` with `params` as its `[strategy.params]` table.
fn table_profile(file: &str, params: &str) -> String {
    format!(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nfile = \"{file}\"\n\n\
         [strategy.params]\n{params}\n"
    )
}

/// The resolve's refusal of `text` (paper).
fn refused(text: &str) -> String {
    match mount(text, false) {
        Err(e) => e,
        Ok(_) => panic!("must be refused:\n{text}"),
    }
}

// ---------------------------------------------------------------------------------------------
// `[strategy.params]` strictness.
// ---------------------------------------------------------------------------------------------

/// A params key the file does not declare is REFUSED, not ignored. `deny_unknown_fields` stops at
/// the `[strategy.params]` boundary (the field is a free-form `Params` table), so without this a
/// mistyped size knob mounts at the compiled default with only the OPTIONAL
/// `policy.max_notional_per_order` behind it.
#[test]
fn an_unread_params_key_is_refused_with_the_readable_set() {
    let err = refused(&table_profile("grid.rs", "step = 0.5\nsizee = 2.0"));
    assert!(err.contains("sizee"), "names the typo: {err}");
    assert!(err.contains("COMPILED DEFAULT"), "names the consequence: {err}");
    assert!(err.contains("band (a number"), "names what it CAN read, with its type: {err}");
    // ...and the same table with the key spelled right is accepted.
    assert!(mount(&table_profile("grid.rs", "step = 0.5\nsize = 2.0"), false).is_ok());
    // A file whose header declares NO params accepts no key at all, and says where to declare one.
    let err = refused(&table_profile("nothing.rhai", "size = 1.0"));
    assert!(err.contains("It reads: nothing: declare it in the file's header."), "{err}");
}

/// The strictness is the SAME on a paper profile as on a live one, and stated as its own claim:
/// a rehearsal that ran different parameters would conceal precisely what it exists to show.
#[test]
fn the_params_gate_does_not_depend_on_the_live_gate() {
    // The default (polymarket paper) venue, a placeholder token — a profile `validate_for_live`
    // refuses outright — is refused by the PARAMS rule all the same, on either daemon.
    for live in [false, true] {
        let profile = |params: &str| {
            format!(
                "token_id = \"TOK\"\n[strategy]\nfile = \"buy_hold.rs\"\n\n[strategy.params]\n{params}\n"
            )
        };
        let err = match mount(&profile("sizee = 1.0"), live) {
            Err(e) => e,
            Ok(_) => panic!("an unread key mounts nowhere"),
        };
        assert!(err.contains("sizee"), "the paper path is just as strict: {err}");
        // ...and so is the TYPE half, on the same paper profile.
        let err = match mount(&profile("size = \"1\""), live) {
            Err(e) => e,
            Ok(_) => panic!("a mistyped key mounts nowhere"),
        };
        assert!(err.contains("wrong TYPE"), "the paper path is just as strict: {err}");
    }
}

/// `[strategy.params]` is a TABLE by type: a scalar there does not load at all, under a `.rs` and a
/// `.rhai` file alike, instead of parsing into a value nobody reads.
#[test]
fn a_scalar_params_is_refused_at_load() {
    for file in ["buy_hold.rs", "thing.rhai"] {
        let err = ProfileDaemon::from_toml_str(&format!(
            "token_id = \"TOK\"\n[strategy]\nfile = \"{file}\"\nparams = 5\n"
        ))
        .expect_err("a scalar `[strategy.params]` must not load");
        assert!(err.contains("parse profile TOML"), "{file}: {err}");
    }
}

/// A key the file DOES read, at a type its reader cannot take, is refused — naming the key, what it
/// got and what the reader wants. These four inputs are the review's own examples, verbatim; each of
/// them used to mount the compiled default with the profile stating otherwise.
#[test]
fn a_mistyped_params_value_is_refused_naming_both_types() {
    for (file, params, key, got) in [
        ("grid.rs", "size  = \"2\"", "size", "string"),
        ("grid.rs", "rungs = 4.0", "rungs", "float"),
        ("grid.rs", "band  = true", "band", "boolean"),
        ("buy_hold.rs", "size  = \"3\"", "size", "string"),
    ] {
        let err = refused(&table_profile(file, params));
        assert!(err.contains("wrong TYPE"), "{params}: {err}");
        assert!(err.contains(&format!("`{key}`")), "names the key: {err}");
        assert!(err.contains(&format!("got {got}")), "names what it got: {err}");
        assert!(err.contains("COMPILED DEFAULT"), "names the consequence: {err}");
    }
    // The expected type is named too, and it is the one the reader really wants — `rungs` is
    // `Value::as_integer`, so the message must say integer and not merely "a number".
    let err = refused(&table_profile("grid.rs", "rungs = 4.0"));
    assert!(err.contains("wants an integer"), "{err}");
    let err = refused(&table_profile("grid.rs", "size  = \"2\""));
    assert!(err.contains("wants a number (integer or float)"), "{err}");

    // ...and every spelling the reader ACTUALLY accepts still mounts. A rule refusing `size = 2`
    // where `as_f64` happily takes it would break working profiles.
    for params in ["size = 2", "size = 2.0", "rungs = 4", "band = 3", "band = 3.5"] {
        assert!(mount(&table_profile("grid.rs", params), false).is_ok(), "`{params}` must mount");
    }
}

/// A value outside the range a header declares is refused in the same sentence as a wrong type.
#[test]
fn a_value_outside_the_declared_range_is_refused() {
    let dir = strategies();
    test_strategy_files::write_into(
        dir.path(),
        &[(
            "ladder.rs",
            "//! ```toml\n//! [strategy]\n//! [params.rungs]\n//! type = \"integer\"\n\
             //! range = [1, 500]\n//! ```\n",
        )],
    );
    let resolve = |rungs: &str| {
        ProfileDaemon::from_toml_str(&table_profile("ladder.rs", &format!("rungs = {rungs}")))
            .expect("loads")
            .resolve_mount_with(dir.path(), false, &stand_in(&[]))
    };
    let err = match resolve("900") {
        Err(e) => e,
        Ok(_) => panic!("outside its range"),
    };
    assert!(err.contains("`rungs` wants a value in [1, 500], got 900"), "{err}");
    assert!(err.contains("wrong TYPE") && err.contains("COMPILED DEFAULT"), "{err}");
    assert!(resolve("500").is_ok(), "the bounds are inclusive");
}

/// A params key that names a market this mount does not trade is refused, NAMING BOTH.
///
/// ⚠ The reviewer's own probe, verbatim, is the first row: `symbol = "MOUNTED_SYMBOL"` at the
/// top level and `[strategy.params] symbol = "A_COMPLETELY_DIFFERENT_SYMBOL"`. MEASURED on the CI box
/// before this refusal existed, that profile LOADED, announced
/// `size=3 symbol=A_COMPLETELY_DIFFERENT_SYMBOL`, and filled on `MOUNTED_SYMBOL` — a startup
/// line naming one instrument while the orders hit another.
///
/// The venue half is the same defect on the same rule: a `momentum` mount's `venue`/`venues`
/// are read by `ControllerHarness` and then discarded by the core's `resolve_intent_venue`.
#[test]
fn a_params_key_naming_another_market_is_refused_naming_both() {
    // (venue, symbol, file, params, the two names the message must carry)
    for (venue, symbol, file, params, named, mounted) in [
        (
            "polymarket",
            "MOUNTED_SYMBOL",
            "buy_hold.rs",
            "size = 3\nsymbol = \"A_COMPLETELY_DIFFERENT_SYMBOL\"",
            "A_COMPLETELY_DIFFERENT_SYMBOL",
            "MOUNTED_SYMBOL",
        ),
        ("hyperliquid", "BTC", "grid.rs", "symbol = \"ETH\"", "ETH", "BTC"),
        ("hyperliquid", "BTC", "dca_accumulate.rs", "symbol = \"ETH\"", "ETH", "BTC"),
        // The VENUE half — `momentum` has no `symbol` key at all, so this row also proves the
        // rule is not "symbol only".
        ("hyperliquid", "BTC", "momentum.rs", "venue = \"binance\"", "binance", "hyperliquid"),
        // ...and one ROW of the routing table, which is a `(symbol, venue)` pair.
        (
            "hyperliquid",
            "BTC",
            "momentum.rs",
            "venues = { ETH = \"binance\" }",
            "binance",
            "hyperliquid",
        ),
    ] {
        let err = refused(&format!(
            "venue = \"{venue}\"\nsymbol = \"{symbol}\"\n[strategy]\nfile = \"{file}\"\n\n\
             [strategy.params]\n{params}\n"
        ));
        assert!(err.contains(named), "{file}/{params}: names what the profile said: {err}");
        assert!(err.contains(mounted), "{file}/{params}: names what is mounted: {err}");
        assert!(
            err.contains("configures NOTHING"),
            "{file}/{params}: names the consequence: {err}"
        );
    }

    // ...and the AGREEING spellings still mount, or the rule would be refusing correct profiles.
    // `symbol` restating the mount is a no-op; an ABSENT one is the working default; an EMPTY
    // one is a mount that cannot trade, which the file's echo reports rather than refuses.
    for params in ["symbol = \"BTC\"", "size = 1.0", "symbol = \"\""] {
        assert!(
            mount(&table_profile("buy_hold.rs", params), false).is_ok(),
            "`{params}` names this mount's own market and must still mount"
        );
    }
    for params in ["venue = \"hyperliquid\"", "qty = 1.0", "venues = { BTC = \"hyperliquid\" }"] {
        assert!(
            mount(&table_profile("momentum.rs", params), false).is_ok(),
            "`{params}` names this mount's own market and must still mount"
        );
    }
}

/// A `multi_leg` header's route keys name LEGS, so "must equal the mount" is false about them:
/// they are not judged, and the strategy declares each leg itself (`vike_model::Strategy::symbols`).
#[test]
fn a_multi_leg_files_route_keys_are_not_judged_against_the_mount() {
    let dir = strategies();
    test_strategy_files::write_into(
        dir.path(),
        &[(
            "pairs.rs",
            "//! ```toml\n//! [strategy]\n//! routes = \"multi_leg\"\n\
             //! multi_leg_why = \"a pairs file: its leg key names the second leg\"\n//! [params.symbol_b]\n\
             //! type = \"text\"\n//! route = \"symbol\"\n//! ```\n",
        )],
    );
    ProfileDaemon::from_toml_str(&table_profile("pairs.rs", "symbol_b = \"ETH\""))
        .expect("loads")
        .resolve_mount_with(dir.path(), false, &stand_in(&[]))
        .unwrap_or_else(|e| panic!("a leg is not a misroute: {e}"));
}

/// The `token_id` spelling of the mount symbol is the SAME field, so the route rule must read it
/// through [`ProfileDaemon::mount_symbol`] and not off `symbol` alone.
///
/// Not a restatement: this daemon's shipped profiles all say `token_id`, so a rule that compared
/// against `self.symbol` would be `None` there and — depending on which way it fell — either
/// refuse every Polymarket profile or check nothing on the ones that actually run.
#[test]
fn the_route_rule_reads_the_mount_symbol_under_either_spelling() {
    let profile = |symbol: &str| {
        format!(
            "token_id = \"TOK\"\n[strategy]\nfile = \"buy_hold.rs\"\n\n[strategy.params]\n\
             symbol = \"{symbol}\"\n"
        )
    };
    let err = refused(&profile("OTHER"));
    assert!(err.contains("TOK") && err.contains("OTHER"), "names both: {err}");
    assert!(
        mount(&profile("TOK"), false).is_ok(),
        "the `token_id` spelling must satisfy the rule when it agrees"
    );
}

/// **The file's own whole-table refusal (its `refuse` export, run in its sandboxed process) reaches
/// the operator VERBATIM** — the daemon's half of the ladder refusal: a table that is spelled, typed
/// and routed right and still describes NO order passes every check above and is refused when the
/// file opens, in the file's words, with nothing added or dropped. The other three checks pass these
/// tables (a ladder refusal is never a type or key refusal).
///
/// The words themselves are the grid and dca files', pinned where the files are compiled
/// (`crates/vike-strategies-check/tests/takers/mount_lines.rs`'s
/// `a_ladder_that_can_never_rest_a_rung_is_refused`).
#[test]
fn a_ladder_that_can_never_rest_a_rung_is_refused() {
    // (the table, the resolution the file's refusal shows): the stand-in refuses exactly these.
    const DEAD: &[(&str, &str)] = &[
        ("anchor = \"fixed\"", "anchor_price=0"),
        ("bounded01 = true", "step=1"),
        ("rungs = 0", "rungs=0"),
        ("rungs = -5", "rungs=0"),
        ("size = 0.0", "size=0"),
    ];
    let dir = strategies();
    let opener = |request: &SandboxOpen<'_>| {
        let table: Vec<String> = request.params.iter().map(|(k, v)| format!("{k} = {v}")).collect();
        match DEAD.iter().find(|(dead, _)| table.join("\n") == *dead) {
            Some((_, shown)) => Err(format!(
                "the ladder rests NO rung, so this mount would never place an order ({shown})"
            )),
            None => stand_in(&[])(request),
        }
    };
    let resolve = |file: &str, params: &str| {
        ProfileDaemon::from_toml_str(&table_profile(file, params))
            .expect("every key spelled, typed and routed right")
            .resolve_mount_with(dir.path(), false, &opener)
    };
    for (file, params, shown) in [
        ("dca_accumulate.rs", "anchor = \"fixed\"", "anchor_price=0"),
        ("grid.rs", "bounded01 = true", "step=1"),
        ("grid.rs", "rungs = 0", "rungs=0"),
        ("grid.rs", "rungs = -5", "rungs=0"),
        ("dca_accumulate.rs", "size = 0.0", "size=0"),
    ] {
        let err = match resolve(file, params) {
            Err(e) => e,
            Ok(_) => panic!("{file}/{params}: the file refused this table"),
        };
        assert_eq!(
            err,
            format!("the ladder rests NO rung, so this mount would never place an order ({shown})"),
            "{file}/{params}: the file's refusal reaches the operator verbatim"
        );
    }
    // ...and a table the file does not refuse mounts.
    for (file, params) in [("grid.rs", "rungs = 4\nstep = 0.5"), ("dca_accumulate.rs", "")] {
        assert!(resolve(file, params).is_ok(), "`{file}` / `{params}` must mount");
    }
}

/// **The echo (blocker 2's second half): what actually mounted must be readable**, and for a `.rs`
/// file it is the file's own `echo` rows, rendered `key=<what the reader landed on>` and nothing
/// else — the whole set, not the subset the profile mentioned, because a knob nobody typed is still a
/// knob the strategy runs. The rows below are today's pinned grid and buy-hold lines; the words come
/// from the files, pinned where the files are compiled
/// (`crates/vike-strategies-check/tests/takers/mount_lines.rs`'s
/// `the_effective_params_line_reports_what_mounted`).
#[test]
fn the_effective_params_line_reports_what_mounted() {
    const GRID_ECHO: &[(&str, &str)] = &[
        ("anchor", "first"),
        ("anchor_price", "0"),
        ("step", "0.5"),
        ("rungs", "3"),
        ("size", "1"),
        ("band", "10"),
        ("bounded01", "false"),
        ("tick", "0.001"),
        ("symbol", "(from the feed)"),
    ];
    const BUY_HOLD_ECHO: &[(&str, &str)] = &[("size", "1"), ("symbol", "(from the feed)")];
    let dir = strategies();
    let line = |file: &str, params: &str, echo: &'static [(&'static str, &'static str)]| {
        let p = ProfileDaemon::from_toml_str(&table_profile(file, params)).expect("loads");
        assert_eq!(p.strategy_name(), file.trim_end_matches(".rs"), "named by its stem");
        p.resolve_mount_with(dir.path(), false, &stand_in(echo))
            .expect("resolves")
            .effective_params()
            .to_string()
    };
    assert_eq!(
        line("grid.rs", "step = 0.5", GRID_ECHO),
        "anchor=first anchor_price=0 step=0.5 rungs=3 size=1 band=10 bounded01=false \
         tick=0.001 symbol=(from the feed)"
    );
    // An empty table is not "nothing configured" — it is every knob at its compiled default, and
    // the line SAYS what those are.
    assert_eq!(line("buy_hold.rs", "", BUY_HOLD_ECHO), "size=1 symbol=(from the feed)");
    // A file that answers no echo says so, rather than echoing the raw table — the misreport this
    // line exists to have stopped.
    assert_eq!(
        line("buy_hold.rs", "size = 2.0", &[]),
        "(unreported — \"buy_hold.rs\" exports no echo, so nothing here can state what it resolved)"
    );
}

// ---------------------------------------------------------------------------------------------
// The mount spec a strategy file's profile lowers to.
// ---------------------------------------------------------------------------------------------

/// The mount SPEC is strategy-free: two profiles that differ only by their file lower to the SAME
/// spec — one derivation, so they can never disagree about venue/symbol/interval/seed_cash or the
/// paper fee model. And its `legs` stay EMPTY: a profile declares no leg, the STRATEGY does
/// (`vike_model::Strategy::symbols`, read when the daemon resolves the mount).
#[test]
fn the_mount_spec_is_the_same_whatever_file_it_names_and_declares_no_legs() {
    let a = file_profile("grid.rs").expect("parses").to_mount_spec();
    let b = file_profile("buy_hold.rhai").expect("parses").to_mount_spec();
    assert_eq!(a.venue, b.venue);
    assert_eq!(a.symbol, b.symbol);
    assert_eq!(a.interval, b.interval);
    assert_eq!(a.interval_ms, b.interval_ms);
    assert_eq!(a.tick_size.to_bits(), b.tick_size.to_bits());
    assert_eq!(a.seed_cash.to_bits(), b.seed_cash.to_bits());
    assert_eq!(a.maker_fee.to_bits(), b.maker_fee.to_bits());
    assert_eq!(a.taker_fee.to_bits(), b.taker_fee.to_bits());
    assert!(a.legs.is_empty(), "a profile declares no mount leg; the strategy does");
}

/// A `$`-scale venue's unset mount defaults are its own: `100_000` of paper equity and a `1.0`
/// grid, where the `[0,1]` outcome-token venue (polymarket) keeps `1_000` and `0.01`
/// (`minimal_profile_uses_polymarket_defaults`).
#[test]
fn a_dollar_scale_venue_lowers_to_its_own_mount_defaults() {
    let spec = file_profile("grid.rs").expect("parses").to_mount_spec();
    assert_eq!(spec.venue, "hyperliquid");
    assert_eq!(spec.seed_cash.to_bits(), 100_000.0_f64.to_bits());
    assert_eq!(spec.tick_size.to_bits(), 1.0_f64.to_bits());
}
