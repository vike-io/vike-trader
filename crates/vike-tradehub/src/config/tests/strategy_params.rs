//! `[strategy.params]` strictness (round-2 review, blocker 2) and the effective-params echo.

use super::*;

// ---------------------------------------------------------------------------------------------
// `[strategy.params]` strictness (round-2 review, blocker 2).
// ---------------------------------------------------------------------------------------------

/// The maker names take NO params here, because there is nowhere for them to go: the maker is
/// built from the profile's own fields, so a `[strategy.params]` table would be silently
/// dropped — and a dropped `qty` is a live order at the compiled default.
#[test]
fn the_maker_names_refuse_a_params_table() {
    for name in AS_MAKER_NAMES {
        let err = DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\nqty = 0.005\ngamma = 0.3\n"
        ))
        .unwrap_err();
        assert!(err.contains("takes no `[strategy.params]`"), "{name}: {err}");
        assert!(err.contains("gamma"), "{name} names the offending keys: {err}");
        assert!(err.contains("tick_size"), "{name} names where the knobs DO live: {err}");
        // An EMPTY table is fine — it configures nothing and asks for nothing.
        assert!(DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n[strategy.params]\n"
            ))
            .is_ok());
    }
}

/// A params key the strategy does not read is REFUSED at load, not ignored. `deny_unknown_fields`
/// stops at the `[strategy.params]` boundary (the field is a free-form `toml::Value`), so
/// without this a mistyped size knob mounts at the compiled default with only the OPTIONAL
/// `policy.max_notional_per_order` behind it.
#[test]
fn an_unread_params_key_is_refused_with_the_readable_set() {
    let err = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nstep = 0.5\nsizee = 2.0\n",
    )
    .unwrap_err();
    assert!(err.contains("sizee"), "names the typo: {err}");
    assert!(err.contains("COMPILED DEFAULT"), "names the consequence: {err}");
    assert!(err.contains("band"), "names what it CAN read: {err}");
    // ...and the same table with the key spelled right is accepted.
    assert!(
        DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nstep = 0.5\nsize = 2.0\n",
        )
        .is_ok()
    );
}

/// The strictness is the SAME on a paper profile as on a live one, and stated as its own claim:
/// a rehearsal that ran different parameters would conceal precisely what it exists to show.
#[test]
fn the_params_gate_does_not_depend_on_the_live_gate() {
    // The default (polymarket paper) venue, a placeholder token — a profile `validate_for_live`
    // refuses outright — is refused by the PARAMS rule at load all the same.
    let err = DaemonProfile::from_toml_str(
        "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsizee = 1.0\n",
    )
    .unwrap_err();
    assert!(err.contains("sizee"), "the paper path is just as strict: {err}");
    // ...and so is the TYPE half, on the same paper profile.
    let err = DaemonProfile::from_toml_str(
        "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsize = \"1\"\n",
    )
    .unwrap_err();
    assert!(err.contains("wrong TYPE"), "the paper path is just as strict: {err}");
}

/// A key the strategy DOES read, at a type its reader cannot take, is refused at load — naming
/// the key, what it got and what the reader wants. These four inputs are the review's own
/// examples, verbatim; each of them used to mount the compiled default with the profile stating
/// otherwise, which is the "a live order at a size nobody typed" consequence the key check was
/// added for, reached through the single most ordinary TOML slip there is.
#[test]
fn a_mistyped_params_value_is_refused_naming_both_types() {
    let profile = |name: &str, params: &str| {
        DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
        ))
    };
    for (name, params, key, got) in [
        ("grid", "size  = \"2\"", "size", "string"),
        ("grid", "rungs = 4.0", "rungs", "float"),
        ("grid", "band  = true", "band", "boolean"),
        ("buy_hold", "size  = \"3\"", "size", "string"),
    ] {
        let err = profile(name, params).unwrap_err();
        assert!(err.contains("wrong TYPE"), "{params}: {err}");
        assert!(err.contains(&format!("`{key}`")), "names the key: {err}");
        assert!(err.contains(&format!("got {got}")), "names what it got: {err}");
        assert!(err.contains("COMPILED DEFAULT"), "names the consequence: {err}");
    }
    // The expected type is named too, and it is the one the reader really wants — `rungs` is
    // `Value::as_integer`, so the message must say integer and not merely "a number".
    let err = profile("grid", "rungs = 4.0").unwrap_err();
    assert!(err.contains("wants an integer"), "{err}");
    let err = profile("grid", "size  = \"2\"").unwrap_err();
    assert!(err.contains("wants a number (integer or float)"), "{err}");

    // ...and every spelling the reader ACTUALLY accepts still loads. A rule refusing `size = 2`
    // where `as_f64` happily takes it would break working profiles, which is the failure mode
    // the type table is read off the source to avoid.
    for params in ["size = 2", "size = 2.0", "rungs = 4", "band = 3", "band = 3.5"] {
        assert!(profile("grid", params).is_ok(), "`{params}` must still load");
    }
}

/// A params key that names a market this mount does not trade is refused at load, NAMING BOTH.
///
/// ⚠ The reviewer's own probe, verbatim, is the first row: `symbol = "MOUNTED_SYMBOL"` at the
/// top level and `[strategy.params] symbol = "A_COMPLETELY_DIFFERENT_SYMBOL"`. MEASURED on the CI box
/// before this refusal existed, that profile LOADED, announced
/// `size=3 symbol=A_COMPLETELY_DIFFERENT_SYMBOL`, and filled on `MOUNTED_SYMBOL` — a startup
/// line naming one instrument while the orders hit another.
///
/// The venue half is the same defect on the same rule: a `momentum` mount's `venue`/`venues`
/// are read by `ControllerHarness` and then discarded by the core's `resolve_intent_venue`.
///
/// MUTATION: delete the `misrouted_params` block from [`DaemonProfile::validate_strategy`] and
/// every row below goes red — the profiles all parse, all type-check, and all mount.
#[test]
fn a_params_key_naming_another_market_is_refused_naming_both() {
    // (venue, symbol, strategy, params, the two names the message must carry)
    for (venue, symbol, name, params, named, mounted) in [
        (
            "polymarket",
            "MOUNTED_SYMBOL",
            "buy_hold",
            "size = 3\nsymbol = \"A_COMPLETELY_DIFFERENT_SYMBOL\"",
            "A_COMPLETELY_DIFFERENT_SYMBOL",
            "MOUNTED_SYMBOL",
        ),
        ("hyperliquid", "BTC", "grid", "symbol = \"ETH\"", "ETH", "BTC"),
        ("hyperliquid", "BTC", "dca_accumulate", "symbol = \"ETH\"", "ETH", "BTC"),
        // The VENUE half — `momentum` has no `symbol` key at all, so this row also proves the
        // rule is not "symbol only".
        ("hyperliquid", "BTC", "momentum", "venue = \"binance\"", "binance", "hyperliquid"),
        // ...and one ROW of the routing table, which is a `(symbol, venue)` pair.
        (
            "hyperliquid",
            "BTC",
            "momentum",
            "venues = { ETH = \"binance\" }",
            "binance",
            "hyperliquid",
        ),
    ] {
        let err = DaemonProfile::from_toml_str(&format!(
            "venue = \"{venue}\"\nsymbol = \"{symbol}\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
        ))
        .unwrap_err();
        assert!(err.contains(named), "{name}/{params}: names what the profile said: {err}");
        assert!(err.contains(mounted), "{name}/{params}: names what is mounted: {err}");
        assert!(
            err.contains("configures NOTHING"),
            "{name}/{params}: names the consequence: {err}"
        );
    }

    // ...and the AGREEING spellings still load, or the rule would be refusing correct profiles.
    // `symbol` restating the mount is a no-op; an ABSENT one is the working default; an EMPTY
    // one is a mount that cannot trade, which `resolved_params` reports rather than refuses.
    for params in ["symbol = \"BTC\"", "size = 1.0", "symbol = \"\""] {
        assert!(
            DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"buy_hold\"\n\n\
                     [strategy.params]\n{params}\n"
            ))
            .is_ok(),
            "`{params}` names this mount's own market and must still load"
        );
    }
    for params in ["venue = \"hyperliquid\"", "qty = 1.0", "venues = { BTC = \"hyperliquid\" }"] {
        assert!(
            DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"momentum\"\n\n\
                     [strategy.params]\n{params}\n"
            ))
            .is_ok(),
            "`{params}` names this mount's own market and must still load"
        );
    }
}

/// A params table that describes NO order at all is refused at load — carrying the resolution,
/// so the operator can see which knob left the ladder empty.
///
/// ⚠ Both dead configurations were MEASURED, as zero broker calls over a scripted market,
/// before this refusal existed (`crates/vike-strategy/tests/param_gates.rs`'s `DEAD` ledger).
/// They are the quietest failure this daemon has: the profile parses, every key is spelled,
/// typed and routed right, the mount line prints a full configuration — and nothing is ever
/// submitted, on any market, at any price.
///
/// MUTATION: delete the `unarmable_params` block from [`DaemonProfile::validate_strategy`] and
/// every row below goes red — the profiles all parse, all type-check, all route and all mount.
#[test]
fn a_ladder_that_can_never_rest_a_rung_is_refused() {
    let profile = |name: &str, params: &str| {
        DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
        ))
    };
    // (strategy, params, the resolution the message must show)
    for (name, params, shown) in [
        // A FIXED anchor with the price left at its compiled `0`: every long rung prices at or
        // below zero and is skipped, and the anchor is stamped anyway so it never re-arms.
        ("dca_accumulate", "anchor = \"fixed\"", "anchor_price=0"),
        // A 0..1 grid at the compiled `step = 1.0`: one rung spacing spans the whole domain.
        ("grid", "bounded01 = true", "step=1"),
        // ...and the degenerate ladder, the same defect through the other guard. `rungs = -5`
        // is here because `read_rungs` CLAMPS it to zero — the input the echo test used to
        // demonstrate that clamp with, now refused before there is a mount line to read.
        ("grid", "rungs = 0", "rungs=0"),
        ("grid", "rungs = -5", "rungs=0"),
        ("dca_accumulate", "size = 0.0", "size=0"),
    ] {
        let err = profile(name, params).unwrap_err();
        assert!(err.contains("NO rung"), "{name}/{params}: names what is wrong: {err}");
        assert!(err.contains("never place an order"), "{name}/{params}: {err}");
        assert!(err.contains(shown), "{name}/{params}: carries the resolution: {err}");
        assert!(
            !err.contains("wrong TYPE") && !err.contains("does not read"),
            "{name}/{params}: this is the case the other three PASS: {err}"
        );
    }

    // ...and the near-misses must still load, or this is a rule about suspicious VALUES rather
    // than about an empty ladder — the over-refusal `unarmable_params`' doc argues against.
    for (name, params) in [
        // A SHORT ladder anchored at zero steps AWAY from it and rests real rungs.
        ("dca_accumulate", "anchor = \"fixed\"\nside = \"short\"\nstep = 0.05"),
        // A bounded grid whose step FITS inside the walls.
        ("grid", "bounded01 = true\nstep = 0.05"),
        // A fixed anchor with a real price on it.
        ("dca_accumulate", "anchor = \"fixed\"\nanchor_price = 40.0"),
        // ...and the ordinary tables, which is what makes every refusal above a contrast.
        ("grid", "rungs = 4\nstep = 0.5"),
        ("dca_accumulate", "rungs = 4\nstep = 0.5"),
    ] {
        assert!(profile(name, params).is_ok(), "`{name}` / `{params}` must still load");
    }
    // The empty table — every knob at its compiled default — is armable for both names, so the
    // refusal can never be reached by simply naming one of them.
    for name in ["grid", "dca_accumulate"] {
        assert!(profile(name, "").is_ok(), "{name}'s own defaults must mount");
    }
}

/// The cross-crate link that keeps the route rule from being SKIPPED on a future name — the twin
/// of [`every_not_enumerated_registry_row_is_refused_here`], one table over.
///
/// [`vike_strategy::misrouted_params`] judges only [`vike_strategy::ParamRoutes::SingleLeg`]
/// names: a
/// `MultiLeg` row's keys name LEGS (so "must equal the mount" is false about them) and a
/// `NotEnumerated` row's key set is unknown. Both abstentions are correct TODAY only because no
/// such name is `Capability::Live` except the two maker aliases, whose params table this daemon
/// refuses outright. Flip `funding_carry` or `pairs_zscore` to live without first giving the
/// mount real legs and the route check would silently stop applying to it — a strategy naming a
/// leg the mount cannot route, mounting clean. This fails first instead.
#[test]
fn every_live_name_this_daemon_mounts_is_route_checked_or_refused_outright() {
    let mut checked = 0;
    for name in vike_strategy::PORTABLE_STRATEGIES {
        if !matches!(vike_strategy::capability(name), vike_strategy::Capability::Live) {
            continue; // refused by NAME at `validate_strategy`'s first gate
        }
        match vike_strategy::param_routes(name) {
            Some(vike_strategy::ParamRoutes::SingleLeg(_)) => checked += 1,
            _ => assert!(
                AS_MAKER_NAMES.contains(name),
                "`{name}` is LIVE-mountable here and its PARAM_ROUTES row is not SingleLeg, so \
                     `misrouted_params` abstains on it — yet this daemon does not refuse its \
                     params table either. Give the mount real legs (`vike_mount::MountSpec::legs` + \
                     `MultiPaperExecutionClient`) before flipping the LIVE_CAPABLE row"
            ),
        }
    }
    assert!(checked > 0, "no live name is route-checked — this gate is vacuous");
}

/// The `token_id` spelling of the mount symbol is the SAME field, so the route rule must read it
/// through [`DaemonProfile::mount_symbol`] and not off `symbol` alone.
///
/// Not a restatement: this daemon's shipped profiles all say `token_id`, so a rule that compared
/// against `self.symbol` would be `None` there and — depending on which way it fell — either
/// refuse every Polymarket profile or check nothing on the ones that actually run.
#[test]
fn the_route_rule_reads_the_mount_symbol_under_either_spelling() {
    let err = DaemonProfile::from_toml_str(
        "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\n\
             symbol = \"OTHER\"\n",
    )
    .unwrap_err();
    assert!(err.contains("TOK") && err.contains("OTHER"), "names both: {err}");
    assert!(
        DaemonProfile::from_toml_str(
            "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\n\
                 symbol = \"TOK\"\n",
        )
        .is_ok(),
        "the `token_id` spelling must satisfy the rule when it agrees"
    );
}

/// The knob that is refused is the knob that would have been WRONG — proven end to end rather
/// than trusted: the same table with the value spelled right mounts the value the operator
/// typed, and the refused one would have mounted the compiled default.
#[test]
fn the_refused_value_is_the_one_that_would_have_silently_defaulted() {
    let g = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nsize = 2.0\n",
    )
    .expect("the correctly-typed profile loads");
    assert!(g.effective_params(&g.to_mount_config()).contains("size=2"), "mounts what it says");
    // The mistyped twin resolves to the compiled default — which is why it is refused at load
    // rather than mounted and logged.
    let quoted: toml::Value = toml::from_str("size = \"2\"").unwrap();
    let resolved = vike_strategy::resolved_params("grid", &quoted).expect("grid enumerates");
    assert_eq!(
        resolved.iter().find(|(k, _)| *k == "size").map(|(_, v)| v.as_str()),
        Some("1"),
        "`size = \"2\"` really does read as the compiled default"
    );
}

/// The cross-crate link that keeps [`AS_MAKER_NAMES`] honest: a registry row that declines to
/// enumerate its keys ([`ParamKeys::NotEnumerated`]) is one `unknown_params` cannot check, so
/// this daemon owes it a stricter rule of its own — and the only such rule is the maker refusal.
/// A future `NotEnumerated` row therefore fails HERE rather than mounting unchecked params.
#[test]
fn every_not_enumerated_registry_row_is_refused_here() {
    for (name, keys) in vike_strategy::PARAM_KEYS {
        if matches!(keys, ParamKeys::NotEnumerated(_)) {
            assert!(
                AS_MAKER_NAMES.contains(name),
                "`{name}` declines to enumerate its params keys, so `unknown_params` reports \
                     nothing about it — but this daemon has no rule of its own for it either, so \
                     any key would mount unchecked. Either enumerate the row, or give this daemon a \
                     rule (the maker names are refused a params table outright)."
            );
        }
    }
    // ...and the converse: every maker name really is a NotEnumerated row, so the refusal is
    // covering a real gap rather than being an unexplained special case.
    for name in AS_MAKER_NAMES {
        assert!(
            matches!(vike_strategy::param_keys(name), Some(ParamKeys::NotEnumerated(_))),
            "`{name}` is refused a params table here but the registry enumerates its keys — \
                 one of the two is now wrong"
        );
    }
}

/// The echo (blocker 2's second half): what actually mounted must be readable. Logging the NAME
/// alone left an operator unable to tell which numbers were running.
#[test]
fn the_effective_params_line_reports_what_mounted() {
    // The maker reports the RESOLVED knobs, including the venue-selected domain the profile
    // never states — the one that decides whether it quotes at all.
    let hl = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005",
    )
    .expect("parses");
    let line = hl.effective_params(&hl.to_mount_config());
    assert!(line.contains("qty=0.005"), "{line}");
    assert!(line.contains("price_domain=Unbounded"), "{line}");
    assert_eq!(hl.strategy_name(), "spread_maker", "the default mount names itself");

    // Every other strategy reports the RESOLVED knobs the same way — the whole set, not the
    // subset the profile happened to mention, because a knob nobody typed is still a knob the
    // strategy is running. Each row is `key=<what the reader landed on>` and nothing else: this
    // line answers "what did the mount resolve", not "what is in force" — see
    // `effective_params`' own doc.
    let g = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nstep = 0.5\n",
    )
    .expect("parses");
    assert_eq!(
        g.effective_params(&g.to_mount_config()),
        "anchor=first anchor_price=0 step=0.5 rungs=3 size=1 band=10 bounded01=false \
             tick=0.001 symbol=(from the feed)"
    );
    assert_eq!(g.strategy_name(), "grid");

    // An empty table is not "nothing configured" — it is every knob at its compiled default,
    // and the line now SAYS what those are.
    let b = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"buy_hold\"\n",
    )
    .expect("parses");
    assert_eq!(b.effective_params(&b.to_mount_config()), "size=1 symbol=(from the feed)");
}

/// The maker line must carry the knobs that DECIDE THE POSTED WIDTH — and must not carry the
/// dead one. It formatted nine fields and none of these four, so an operator could read `gamma`
/// off the startup line while the two numbers that actually bound `δ` were invisible; meanwhile
/// it printed `half_spread`, which A-S never consumes on this daemon.
#[test]
fn the_maker_line_reports_the_knobs_that_set_the_posted_width() {
    let hl = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005",
    )
    .expect("parses");
    let line = hl.effective_params(&hl.to_mount_config());
    for knob in [
        "min_half_spread_ticks=2",
        "max_half_spread_ticks=60",
        "kappa_default=50",
        "tau_hold_ms=3600000",
    ] {
        assert!(line.contains(knob), "the width knob `{knob}` is missing from: {line}");
    }
    assert!(
        !line.contains("half_spread="),
        "the DEAD fixed-spread seed must not be logged beside the live knobs: {line}"
    );
    // The break-even fee floor is reported as the `Option` it is: a `Some` on a venue whose fee
    // shape has a flat rate (hyperliquid, 1.5 bps maker ⇒ a 3 bps round trip)...
    assert!(line.contains("round_trip_fee_rate=Some(0.0003"), "{line}");
    // ...and a NONE that an operator can SEE on one that has not (polymarket's p(1−p) curve),
    // because "no bar is armed" and "the fee is zero" must never read the same.
    let pm =
        DaemonProfile::from_toml_str("venue = \"polymarket\"\nsymbol = \"TOK\"").expect("parses");
    let pm_line = pm.effective_params(&pm.to_mount_config());
    assert!(pm_line.contains("round_trip_fee_rate=None"), "{pm_line}");
}

/// The defect the round-2 repair introduced, pinned so it cannot return: the echo reported the
/// RAW table, so a value the reader coerced printed as what was TYPED. A diagnostic that
/// affirmatively misstates the mount is worse than no diagnostic — this repo deleted
/// `Policy::max_total_exposure` over the same principle.
///
/// Every case below is type-CORRECT input, so [`vike_strategy::mistyped_params`] passes it and
/// only the echo can tell the truth about it.
#[test]
fn the_echo_reports_the_resolution_and_not_the_input() {
    let line = |name: &str, params: &str| {
        let p = DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
        ))
        .unwrap_or_else(|e| panic!("`{params}` must LOAD (it is well-typed): {e}"));
        p.effective_params(&p.to_mount_config())
    };
    // ⚠ The CLAMP row (`rungs = -5`, which `read_rungs` floors to zero) is no longer HERE: a
    // grid that rests nothing is refused at load by `vike_strategy::unarmable_params`, so there
    // is no mount line to inspect. The clamp is still pinned, in the refusal MESSAGE, by
    // `a_ladder_that_can_never_rest_a_rung_is_refused` below — the resolution the operator
    // needs to see is carried either way, which is the property this test is really about.
    //
    // An unrecognised STRING silently falling back — the anchor price is then never used.
    let l = line("grid", "anchor = \"fixd\"\nanchor_price = 42.0");
    assert!(l.contains("anchor=first"), "{l}");
    // ...and the same shape on a direction knob, where the fallback is a SIDE.
    let l = line("dca_accumulate", "side = \"shrot\"");
    assert!(l.contains("side=long"), "{l}");
    // A default the profile never mentions at all: the controller harness's venue tag.
    let l = line("momentum", "qty = 2.0");
    assert!(l.contains("venue=sim"), "a live mount under the tag `sim`, and now it says so: {l}");
    assert!(l.contains("tp=(unarmed)"), "an un-armed barrier leg says so: {l}");
}

/// ...and ABSENT `[strategy]` still means the A-S maker, which is the back-compat property.
#[test]
fn no_strategy_table_still_resolves_the_as_maker() {
    let p = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("parses");
    assert!(p.strategy.is_none());
    assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
}

/// The three rejection classes, each with its OWN message. This is the honest-gate test: a
/// strategy that would mount and never trade must fail at profile LOAD, not at 3am.
#[test]
fn unmountable_strategies_are_rejected_at_load_with_their_reason() {
    // (a) a typo.
    let err = strategy_profile("grud").unwrap_err();
    assert!(err.contains("unknown strategy"), "typo message: {err}");
    assert!(err.contains("grid"), "names what it could have meant: {err}");

    // (b) simulator-only: it backtests, but this daemon does not link the simulator.
    let err = strategy_profile("rotation_top_k").unwrap_err();
    assert!(err.contains("simulator-only"), "sim-only message: {err}");

    // (c) resolves, but its input never arrives live — the SILENT NO-OP class.
    let err = strategy_profile("funding_capture").unwrap_err();
    assert!(err.contains("cannot trade"), "not-live message: {err}");
    assert!(err.contains("Bar::funding"), "names the missing input: {err}");

    let err = strategy_profile("pairs_zscore").unwrap_err();
    assert!(err.contains("TWO-LEG"), "names why a two-leg mount cannot route: {err}");
}

/// Every `NotLive` row in the shared table is refused here — table-driven, so a future row
/// cannot be added to the registry and silently stay mountable by this daemon.
#[test]
fn every_not_live_registry_row_is_refused_by_the_profile() {
    for (name, verdict) in vike_strategy::LIVE_CAPABLE {
        if verdict.blocker().is_none() {
            continue;
        }
        assert!(
            strategy_profile(name).is_err(),
            "{name} is declared not-live-capable but the profile accepted it"
        );
    }
}

/// The mount SPEC a `[strategy]` profile lowers to is the SAME projection the A-S path uses —
/// one derivation, so the two can never disagree about venue/symbol/interval/seed_cash or the
/// paper fee model. And its `legs` stay EMPTY: a multi-leg paper rehearsal would book both legs
/// under one symbol (`build_paper_strategy_core_with`'s tripwire), so no profile may declare one
/// until `MultiPaperExecutionClient` is wired.
#[test]
fn the_mount_spec_matches_the_maker_lowering_and_declares_no_legs() {
    let p = strategy_profile("grid").expect("parses");
    let cfg = p.to_mount_config();
    let spec = p.to_mount_spec();
    assert_eq!(spec.venue, cfg.venue);
    assert_eq!(spec.symbol, cfg.token_id);
    assert_eq!(spec.interval, cfg.interval);
    assert_eq!(spec.interval_ms, cfg.interval_ms);
    assert_eq!(spec.seed_cash.to_bits(), cfg.seed_cash.to_bits());
    assert_eq!(spec.maker_fee.to_bits(), cfg.maker_fee.to_bits());
    assert_eq!(spec.taker_fee.to_bits(), cfg.taker_fee.to_bits());
    assert!(spec.legs.is_empty(), "no profile may declare a mount leg yet");
}

#[test]
fn hyperliquid_lowers_to_the_crypto_dollar_scale_mount() {
    // A hyperliquid profile lowers into `MakerMountConfig::crypto` (the $-scale A-S domain) so the
    // maker can quote a $64k asset; a polymarket profile keeps the [0,1] domain. The crypto mount's
    // signature here is its min-half-spread FLOOR (>0) — absent (0.0) on the [0,1] default.
    let hl = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\ntoken_id = \"BTC\"\ntick_size = 1.0\nqty = 0.005",
    )
    .expect("parses");
    let cfg = hl.to_mount_config();
    assert_eq!(cfg.venue, "hyperliquid");
    assert!(
        cfg.as_params.min_half_spread_ticks > 0.0,
        "hyperliquid must lower to the crypto $-scale mount (min-half-spread floor set)"
    );

    // polymarket (the default venue) keeps the [0,1] domain — no crypto floor.
    let poly = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("parses");
    assert_eq!(
        poly.to_mount_config().as_params.min_half_spread_ticks,
        0.0,
        "polymarket keeps the [0,1] default (no crypto floor)"
    );
}
