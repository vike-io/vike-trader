use super::*;

/// THE GATE that stops `[guards]`/`[sinks]` drifting back into declared-but-inert.
///
/// ⚠ It is an exhaustive DESTRUCTURE, not a list of assertions, and that is the whole point: a
/// new field on either struct makes this a COMPILE ERROR until its author classifies it as
/// wired (it lands in `CoreConfig`) or as declared-unwired (it is named by
/// [`RunProfile::apply_guards_and_sinks`]'s return). `vike_config::CONSUMPTION` gates exactly
/// this property for `config`/`preferences`/`flags`, and `POLICY_CONSUMERS` for `policy` — but
/// both key off `vike-config`-owned types, so `RunProfile` (a `vike-core` type, with
/// `deny_unknown_fields` and a `validate()` that rejects typos, i.e. the artifact that gives an
/// operator the MOST confidence) was the one settings surface with no gate at all. Four of its
/// sections were inert for their entire life and nothing could go red.
#[test]
fn every_guard_and_sink_field_is_wired_or_declared() {
    // Set EVERY field to a non-default, so each one is observable either in the CoreConfig it
    // lands in or in the unwired list it is named by.
    let guards = Guards {
        initial_trading_state: ProfileTradingState::Halted,
        submit_ack_timeout_ms: Some(1_234),
        submit_ack_confirm_grace_ms: Some(4_321),
        max_drawdown: Some(0.25),
        conditionals_on_ticks: true,
        freshness_ms: Some(9_000),
        margin_call: Some(ProfileMarginCall {
            mm_requirement: 0.5,
            warn_fraction: 0.8,
            buffer: 0.1,
        }),
    };
    let sinks = Sinks {
        gui: true,
        recorder: true,
        raw_capture_dir: Some("/tmp/caps".to_string()),
        journal: None, // wired ELSEWHERE (`journal_config_from_env`), so not this fn's business
        equity_sample_ms: Some(2_500),
    };

    // ⚠ Exhaustive destructure — the compile error a new field causes IS the gate.
    let Guards {
        initial_trading_state,
        submit_ack_timeout_ms,
        submit_ack_confirm_grace_ms,
        max_drawdown,
        conditionals_on_ticks,
        freshness_ms,
        margin_call,
    } = &guards;
    let Sinks { gui, recorder, raw_capture_dir, journal, equity_sample_ms } = &sinks;

    let profile = RunProfile {
        guards: guards.clone(),
        sinks: sinks.clone(),
        ..RunProfile::from_toml_str(samples::LIVE_TOML).expect("the shipped live sample must parse")
    };
    let mut cfg = crate::CoreConfig::default();
    let unwired = profile.apply_guards_and_sinks(&mut cfg).unwired;

    // ── WIRED: each of these must be observable in the CoreConfig the core is built from.
    assert_eq!(
        cfg.submit_ack_timeout,
        Some(Duration::from_millis(*submit_ack_timeout_ms.as_ref().unwrap()))
    );
    assert_eq!(
        cfg.submit_ack_confirm_grace,
        Duration::from_millis(*submit_ack_confirm_grace_ms.as_ref().unwrap())
    );
    assert_eq!(cfg.max_drawdown, *max_drawdown);
    assert_eq!(cfg.conditionals_on_ticks, *conditionals_on_ticks);
    assert!(cfg.margin_call.is_some(), "guards.margin_call must reach CoreConfig");
    assert_eq!(margin_call.is_some(), cfg.margin_call.is_some());
    assert_eq!(cfg.equity_sample, Some(Duration::from_millis(*equity_sample_ms.as_ref().unwrap())));

    // ── DECLARED-UNWIRED: each must be NAMED, so a binary can disclose it.
    assert!(unwired.contains(&"guards.initial_trading_state"), "{unwired:?}");
    assert_ne!(*initial_trading_state, ProfileTradingState::default(), "precondition");
    assert!(unwired.contains(&"guards.freshness_ms"), "{unwired:?}");
    assert!(freshness_ms.is_some(), "precondition");
    assert!(unwired.contains(&"sinks.gui"), "{unwired:?}");
    assert!(*gui, "precondition");
    assert!(unwired.contains(&"sinks.recorder"), "{unwired:?}");
    assert!(*recorder, "precondition");
    assert!(unwired.contains(&"sinks.raw_capture_dir"), "{unwired:?}");
    assert!(raw_capture_dir.is_some(), "precondition");

    // ── `journal` is wired, but through `journal_config_from_env`, not through this fn — so it
    //    must NOT appear in the unwired list, and must not be silently applied here either.
    assert!(journal.is_none(), "precondition: this profile arms no journal");
    assert!(
        !unwired.iter().any(|k| k.contains("journal")),
        "sinks.journal IS consumed (journal_config_from_env → CoreConfig::journal): {unwired:?}"
    );

    assert_eq!(unwired.len(), 5, "every field above is accounted for exactly once: {unwired:?}");
}

/// …and the quiet path: a profile that sets NO guard and NO sink names nothing as unwired, so a
/// binary's disclosure line cannot fire on every ordinary start. A row that fires on every
/// healthy mount is noise, and noise is what let the original defect sit for six weeks.
#[test]
fn a_profile_with_no_guards_declares_nothing_unwired() {
    let profile = RunProfile {
        guards: Guards::default(),
        sinks: Sinks::default(),
        ..RunProfile::from_toml_str(samples::LIVE_TOML).expect("the shipped live sample parses")
    };
    let mut cfg = crate::CoreConfig::default();
    assert!(profile.apply_guards_and_sinks(&mut cfg).unwired.is_empty());
    // …and a profile that declares nothing must not overwrite what the BINARY configured: the
    // `Option` guards are skipped, so the caller's own knobs survive.
    let base = crate::CoreConfig::default();
    assert_eq!(cfg.submit_ack_timeout, base.submit_ack_timeout);
    assert_eq!(cfg.submit_ack_confirm_grace, base.submit_ack_confirm_grace);
    assert_eq!(cfg.max_drawdown, base.max_drawdown);
    assert_eq!(cfg.equity_sample, base.equity_sample);
}

/// A `CoreConfig` shaped like the one the live root builds: the binary's own 30 s stage-1
/// timeout, and a confirm grace nobody has touched.
/// `crates/vike-tradehub/src/tradehub_cli.rs` constructs exactly this before calling
/// [`RunProfile::apply_guards_and_sinks`], which is the configuration the hazard below lives in
/// — a bare `CoreConfig::default()` has `submit_ack_timeout: None` and no watchdog at all, so
/// testing against one would test the case that cannot be hurt.
///
/// ⚠ The name is plural-flavoured because there WERE two: the GUI shell built the identical
/// literal until the desktop cut removed its local trading core
/// (`crates/vike-desktop/src/main.rs`, then spelled `vike-app`). One root now, same shape.
fn live_root_config() -> crate::CoreConfig {
    crate::CoreConfig {
        submit_ack_timeout: Some(Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS)),
        ..crate::CoreConfig::default()
    }
}

/// A profile carrying `guards`, otherwise the shipped live sample.
fn profile_with(guards: Guards) -> RunProfile {
    RunProfile {
        guards,
        ..RunProfile::from_toml_str(samples::LIVE_TOML).expect("the shipped live sample parses")
    }
}

/// The stage-1 timeout the live root arms, in its own `CoreConfig` literal
/// (`crates/vike-tradehub/src/tradehub_cli.rs`'s `submit_ack_timeout`). Restated here because
/// `vike-core` cannot read a binary above it; the citation is the link.
const LIVE_ROOT_TIMEOUT_SECS: u64 = 30;
/// The adapter's per-re-query REST timeout
/// (`crates/vike-bridge-core/src/http.rs`'s `blocking_agent_with_timeout`). Restated for the
/// same reason — `vike-core` sits BELOW `vike-bridge-core` and cannot name it in code.
const REQUERY_SECS: u64 = 5;
/// Sequential re-queries the worst-case confirm makes: 2 for Bybit (realtime → history).
const REQUERY_HOPS: u64 = 2;

/// THE PAIRING, pinned per key and in all four combinations.
///
/// ⚠ The case that motivated this test is row 2 — arm stage 1, say NOTHING about the grace.
/// That used to rewrite the grace to `Guards`' own stale `DEFAULT_CONFIRM_GRACE_MS` (5 s) over
/// `CoreConfig::default`'s 15 s, halving the confirm window on a LIVE order path from a profile
/// that never mentioned it. Row 3 is the mirror defect the same gate carried: a profile naming
/// ONLY the grace was silently dropped, because the write was gated on stage 1's key.
#[test]
fn the_confirm_grace_is_written_only_when_the_operator_names_it() {
    let default_grace = crate::CoreConfig::default().submit_ack_confirm_grace;
    let named_grace = Duration::from_millis(16_000);
    // Deliberately a value that still clears bound (a) against the default grace
    // (`2·15s = 30s > 20/2 + 2·5 = 20s`): a test fixture should not model the pairing the docs
    // tell an operator to avoid.
    let named_timeout = Duration::from_millis(20_000);

    // 1 — NEITHER named: both knobs stay exactly as the binary built them.
    let mut cfg = live_root_config();
    profile_with(Guards::default()).apply_guards_and_sinks(&mut cfg);
    assert_eq!(cfg.submit_ack_timeout, Some(Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS)));
    assert_eq!(cfg.submit_ack_confirm_grace, default_grace);

    // 2 — ONLY the timeout named: the timeout moves, THE GRACE DOES NOT.
    let mut cfg = live_root_config();
    profile_with(Guards {
        submit_ack_timeout_ms: Some(named_timeout.as_millis() as u64),
        ..Guards::default()
    })
    .apply_guards_and_sinks(&mut cfg);
    assert_eq!(cfg.submit_ack_timeout, Some(named_timeout));
    assert_eq!(
        cfg.submit_ack_confirm_grace, default_grace,
        "an unnamed grace must survive an armed stage 1 — this is the live-order hazard"
    );

    // 3 — ONLY the grace named: it is APPLIED (the roots' own stage 1 is armed regardless of
    //     what the profile says), and the timeout the binary set is untouched.
    let mut cfg = live_root_config();
    profile_with(Guards {
        submit_ack_confirm_grace_ms: Some(named_grace.as_millis() as u64),
        ..Guards::default()
    })
    .apply_guards_and_sinks(&mut cfg);
    assert_eq!(cfg.submit_ack_timeout, Some(Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS)));
    assert_eq!(
        cfg.submit_ack_confirm_grace, named_grace,
        "a named grace must reach the core even with no `submit_ack_timeout_ms` beside it"
    );

    // 4 — BOTH named: both are the operator's.
    let mut cfg = live_root_config();
    profile_with(Guards {
        submit_ack_timeout_ms: Some(named_timeout.as_millis() as u64),
        submit_ack_confirm_grace_ms: Some(named_grace.as_millis() as u64),
        ..Guards::default()
    })
    .apply_guards_and_sinks(&mut cfg);
    assert_eq!(cfg.submit_ack_timeout, Some(named_timeout));
    assert_eq!(cfg.submit_ack_confirm_grace, named_grace);

    // …and the accessor itself carries the same meaning, so no caller can reintroduce a
    // profile-side default by reading it: unnamed is `None`, not a number.
    assert_eq!(Guards::default().submit_ack_confirm_grace(), None);
}

/// HARD LOWER BOUND (a) — `2·grace > submit_ack_timeout/2 + N·requery` — evaluated on what
/// this workspace actually SHIPS, not on an example.
///
/// The bound is stated in prose on [`crate::CoreConfig::submit_ack_confirm_grace`] and argued
/// again at both roots' `submit_ack_timeout` literals, and prose cannot notice when one of its
/// terms moves. It nearly did: `CoreConfig::default`'s grace was bumped 5 s → 15 s and the
/// profile converter's copy was not, which is the defect the test above pins from the wiring
/// side. This one pins it from the ARITHMETIC side, so a future re-tune of either shipped
/// number reddens here rather than in a live phantom reject.
#[test]
fn the_shipped_confirm_grace_pairing_satisfies_the_lower_bound() {
    let requery = Duration::from_secs(REQUERY_SECS * REQUERY_HOPS);
    let bound = |timeout: Duration, grace: Duration| grace * 2 > timeout / 2 + requery;

    let timeout = Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS);
    let grace = crate::CoreConfig::default().submit_ack_confirm_grace;
    assert!(
        bound(timeout, grace),
        "the SHIPPED pairing violates HARD LOWER BOUND (a): 2·{grace:?} must exceed \
             {timeout:?}/2 + {requery:?} — the last-resort synthesized OrderRejected can fire \
             while the adapter's own re-query is still in flight, which is a phantom reject of a \
             live order. Raise `CoreConfig::default`'s `submit_ack_confirm_grace` or lower the \
             `submit_ack_timeout` both live roots arm."
    );

    // …and the bound can actually FAIL, so the assertion above is not vacuous: the 5 s the
    // deleted `DEFAULT_CONFIRM_GRACE_MS` used to write is exactly what it refuses.
    assert!(
        !bound(timeout, Duration::from_secs(5)),
        "5 s was the value the profile converter wrote over the default; if this passes the \
             bound has been re-derived and this whole test needs re-reading"
    );

    // Every shipped SAMPLE that arms stage 1 must clear the same bound — these are the files
    // an operator copies, and one of them shipping a violating pair is how the operator
    // acquires it.
    for (name, toml) in samples::ALL {
        let p = RunProfile::from_toml_str(toml).expect("shipped sample parses");
        let Some(t) = p.guards.submit_ack_timeout() else { continue };
        let g = p.guards.submit_ack_confirm_grace().unwrap_or(grace);
        assert!(bound(t, g), "sample `{name}` pairs {t:?} with {g:?}, violating bound (a)");
    }
}

/// The RUNTIME warning and this file's own pin must be the SAME bound.
///
/// ⚠ Two spellings of one arithmetic, held equal — the idiom
/// `crates/vike-bridge-core/tests/settings_dir_spellings.rs` established for the duplicated
/// settings-dir resolver. The alternative (the test simply calling
/// [`confirm_grace_clears_bound`]) would make the pin a re-assertion of the code it gates, and
/// this whole thread exists because a bound stated in two places drifted: `Guards`' own 5 s
/// constant against `CoreConfig::default`'s 15 s. So the closure above stays an INDEPENDENT
/// re-derivation from [`REQUERY_SECS`]/[`REQUERY_HOPS`], and this is what refuses to let the two
/// part company — including when only one side's `N` or requery budget is re-tuned.
#[test]
fn the_warning_predicate_agrees_with_the_bound_the_shipped_pairing_is_gated_on() {
    let requery = Duration::from_secs(REQUERY_SECS * REQUERY_HOPS);
    let bound = |timeout: Duration, grace: Duration| grace * 2 > timeout / 2 + requery;

    // A grid that straddles the boundary at several timeouts, so agreement is checked ON the
    // knife edge rather than only in the comfortable interior.
    for t_secs in [1_u64, 5, 20, 30, 45, 60, 90, 300] {
        let timeout = Duration::from_secs(t_secs);
        for g_ms in [0_u64, 1, 999, 5_000, 12_499, 12_500, 12_501, 15_000, 27_501, 60_000] {
            let grace = Duration::from_millis(g_ms);
            assert_eq!(
                bound(timeout, grace),
                confirm_grace_clears_bound(timeout, grace),
                "the runtime predicate and this file's pin disagree at {timeout:?}/{grace:?} — \
                     one of the two spellings of HARD LOWER BOUND (a) has been re-tuned alone"
            );
        }
    }
}

/// The number the warning tells an operator to type must actually WORK — driven through the
/// real predicate, so the claim in [`ConfirmGraceHazard::min_grace_ms`]'s doc is proven rather
/// than restated. `⌊budget/2⌋ + 1 ms` is also checked to be the SMALLEST such whole
/// millisecond, so the advice is not merely safe but not needlessly wasteful either.
#[test]
fn the_advised_minimum_grace_actually_clears_the_bound() {
    for t_secs in [1_u64, 7, 30, 55, 90, 301] {
        let timeout = Duration::from_secs(t_secs);
        let hazard = ConfirmGraceHazard {
            submit_ack_timeout: timeout,
            confirm_grace: Duration::ZERO,
            confirm_budget: confirm_budget(timeout),
        };
        let advised = Duration::from_millis(hazard.min_grace_ms() as u64);
        assert!(
            confirm_grace_clears_bound(timeout, advised),
            "the warning advises {advised:?} at {timeout:?} and that does not clear the bound"
        );
        assert!(
            !confirm_grace_clears_bound(timeout, advised - Duration::from_millis(1)),
            "the advised {advised:?} at {timeout:?} is one whole millisecond larger than it \
                 needs to be — the operator is being told to over-tune"
        );
    }
}

/// THE PAIR WARNING — the residual #1569 declared and did not close.
///
/// ⚠ The case that motivated it is row B: raise `submit_ack_timeout_ms`, say NOTHING about the
/// grace. That is legal, silent, and walks the untouched grace under bound (a) — #1569's fix
/// (each key gated on its own presence) is precisely what makes the grace stay put while the
/// bound it must clear moves out from under it. Rows A and C are the no-fire half and matter as
/// much: a warning that fires on the shipped defaults, or on an operator who paired the two keys
/// correctly, is noise, and noise is what let the original defect sit for six weeks.
#[test]
fn a_raised_timeout_with_an_untouched_grace_warns_and_a_sound_pairing_does_not() {
    let shipped_grace = crate::CoreConfig::default().submit_ack_confirm_grace;

    // ── A — the shipped defaults, profile naming NEITHER key. 2·15s = 30s > 30/2 + 2·5 = 25s.
    let mut cfg = live_root_config();
    let report = profile_with(Guards::default()).apply_guards_and_sinks(&mut cfg);
    assert_eq!(
        report.confirm_grace, None,
        "the SHIPPED pairing must not warn — a row that fires on every healthy mount is noise"
    );

    // ── B — THE CASE THAT MATTERS: the timeout is raised, the grace is left where the binary
    //        put it. 2·15s = 30s, against 90/2 + 2·5 = 55s.
    let raised = Duration::from_secs(90);
    let mut cfg = live_root_config();
    let report = profile_with(Guards {
        submit_ack_timeout_ms: Some(raised.as_millis() as u64),
        ..Guards::default()
    })
    .apply_guards_and_sinks(&mut cfg);
    let hazard =
        report.confirm_grace.expect("a raised timeout over an untouched grace must be reported");
    assert_eq!(hazard.submit_ack_timeout, raised);
    assert_eq!(hazard.confirm_grace, shipped_grace, "the grace is the one #1569 left in place");
    assert_eq!(hazard.confirm_budget, Duration::from_secs(55));
    assert_eq!(hazard.min_grace_ms(), 27_501);
    // …and the sentence an operator reads names the key they must edit and the number to put
    // in it — the whole point of returning DATA the roots render rather than a bare bool.
    let text = hazard.to_string();
    assert!(text.contains("guards.submit_ack_confirm_grace_ms = 27501"), "{text}");
    assert!(text.contains("PHANTOM REJECT"), "{text}");

    // ── C — the same raised timeout, PAIRED. 2·30s = 60s > 55s: silence.
    let mut cfg = live_root_config();
    let report = profile_with(Guards {
        submit_ack_timeout_ms: Some(raised.as_millis() as u64),
        submit_ack_confirm_grace_ms: Some(30_000),
        ..Guards::default()
    })
    .apply_guards_and_sinks(&mut cfg);
    assert_eq!(
        report.confirm_grace, None,
        "an operator who named a correct pair must not be warned at"
    );
    // The advised minimum is the boundary, and it is INCLUSIVE — the warning would be lying if
    // typing its own number still warned on the next start.
    let mut cfg = live_root_config();
    let report = profile_with(Guards {
        submit_ack_timeout_ms: Some(raised.as_millis() as u64),
        submit_ack_confirm_grace_ms: Some(27_501),
        ..Guards::default()
    })
    .apply_guards_and_sinks(&mut cfg);
    assert_eq!(report.confirm_grace, None, "the advised minimum must silence the warning");

    // ── D — THE MIRROR DIRECTION, which is why the check reads the FINAL pair rather than the
    //        profile's delta: the timeout is never mentioned, the GRACE is lowered under it.
    //        A check keyed on "did this profile raise the timeout" would miss this entirely.
    let mut cfg = live_root_config();
    let report =
        profile_with(Guards { submit_ack_confirm_grace_ms: Some(5_000), ..Guards::default() })
            .apply_guards_and_sinks(&mut cfg);
    let hazard =
        report.confirm_grace.expect("a grace lowered under an untouched timeout must warn");
    assert_eq!(hazard.submit_ack_timeout, Duration::from_secs(LIVE_ROOT_TIMEOUT_SECS));
    assert_eq!(hazard.confirm_grace, Duration::from_secs(5));

    // ── E — the watchdog DISARMED (`CoreConfig::default`'s `submit_ack_timeout: None`). No
    //        stage 1, no ladder, nothing to race: the grace is inert and warning is noise.
    let mut cfg = crate::CoreConfig {
        submit_ack_confirm_grace: Duration::from_millis(1),
        ..crate::CoreConfig::default()
    };
    assert_eq!(cfg.submit_ack_timeout, None, "precondition: stage 1 is disarmed");
    let report = profile_with(Guards::default()).apply_guards_and_sinks(&mut cfg);
    assert_eq!(
        report.confirm_grace, None,
        "a disarmed watchdog has no pairing to break, whatever the grace says"
    );

    // …and the two halves of the report are INDEPENDENT. The live sample's `[sinks]` names two
    // keys that reach no `CoreConfig` whatever the guards say, so the same profile is applied
    // twice — once tripping the bound, once not — and the unwired set must be the SAME list.
    // A hazard is not a reason to call a key unwired, nor a reason to stop naming one.
    let mut cfg = live_root_config();
    let hazardous = profile_with(Guards {
        submit_ack_timeout_ms: Some(raised.as_millis() as u64),
        ..Guards::default()
    })
    .apply_guards_and_sinks(&mut cfg);
    let mut cfg = live_root_config();
    let sound = profile_with(Guards::default()).apply_guards_and_sinks(&mut cfg);
    assert!(hazardous.confirm_grace.is_some(), "precondition");
    assert_eq!(sound.confirm_grace, None, "precondition");
    assert_eq!(
        hazardous.unwired, sound.unwired,
        "the pair check must not disturb the still-unwired disclosure"
    );
    assert_eq!(
        hazardous.unwired,
        vec!["sinks.gui", "sinks.recorder"],
        "…and it is a real list, not two empty ones compared to each other"
    );
}
