use super::*;
use vike_exec::recon::{DivergenceKind, ReconMode};

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// -- S2: the default-ON gate, and the quarantine it is paired with ------------------------

/// **THE S2 property.** A mount that arms a live venue account and sets NOTHING reconciles —
/// and reconciles under a policy that folds nothing. Both halves are asserted here because
/// either alone is a defect: the gate without the pairing auto-applies `PositionDrift` at the
/// first pass after a restart, and the pairing without the gate reconciles nobody.
///
/// The fold set is ASKED of [`auto_applied_kinds`] rather than restated as a list of kinds —
/// `crates/vike-exec/CLAUDE.md` and `crates/vike-exec/tests/recon/recon_policy_pin.rs`'s module doc
/// spell out why a hand-written fold list rots (the "hybrid auto-cancels `OrphanLocalOrder`" claim
/// was stated in six places and was false).
#[test]
fn a_live_mount_that_configures_nothing_reconciles_and_folds_nothing() {
    let gate = reconcile_gate(false, false, 1);
    assert_eq!(gate, ReconcileGate::LiveDefault);
    assert!(gate.enabled(), "one armed live account is enough — this is the S2 default");

    let cfg = build_recon_config(&quarantine_first_default(HashMap::new()), HashMap::new());
    assert_eq!(cfg.policy.default, ReconMode::Quarantine);
    assert!(
        auto_applied_kinds(&cfg.policy).is_empty(),
        "a default-on reconcile must fold NOTHING without an operator; auto-applied set was \
             {:?}",
        auto_applied_kinds(&cfg.policy)
    );
}

/// The gate is scoped to LIVE mounts by its own input: no armed live account, nothing asked,
/// no driver. This is the paper box, and it is the half that keeps a default-on reconcile from
/// meaning "every process now talks to a venue".
#[test]
fn a_paper_mount_arms_no_reconcile_driver() {
    let gate = reconcile_gate(false, false, 0);
    assert_eq!(gate, ReconcileGate::NothingArmedLive);
    assert!(!gate.enabled());
}

/// An operator who refuses gets the refusal, and gets it even against a `flags.toml` that says
/// `reconcile = true` — the case where a box was armed months ago and the person at the
/// terminal is silencing it now.
#[test]
fn the_operator_opt_out_wins_over_the_default_and_over_a_stale_request() {
    for armed in [0usize, 3] {
        let refused = reconcile_gate(false, true, armed);
        assert_eq!(refused, ReconcileGate::RefusedByOperator, "armed={armed}");
        assert!(!refused.enabled(), "armed={armed}");

        let both = reconcile_gate(true, true, armed);
        assert_eq!(both, ReconcileGate::RefusedByOperator, "armed={armed}");
        assert!(!both.enabled(), "armed={armed}");
    }
}

/// `flags.reconcile` still MEANS something after the default landed: it turns the driver on
/// where the armed-live probe reports nothing. That is not a hypothetical — the probe is a
/// hand-written per-venue table, so a venue arm merged without its row reports zero for a live
/// mount, and this is the operator's override for exactly that.
#[test]
fn an_explicit_request_still_arms_a_mount_the_probe_reports_as_paper() {
    let gate = reconcile_gate(true, false, 0);
    assert_eq!(gate, ReconcileGate::Requested);
    assert!(gate.enabled());
}

/// Every arm's disclosure names the settings key an operator would edit, and the two ON arms
/// say which policy is in force. A startup line that only said "reconcile: true" is what made
/// "was the venue ever queried on this box" unanswerable from a log.
#[test]
fn every_gate_arm_discloses_the_key_an_operator_would_edit() {
    for arm in [
        ReconcileGate::Requested,
        ReconcileGate::LiveDefault,
        ReconcileGate::RefusedByOperator,
        ReconcileGate::NothingArmedLive,
    ] {
        let d = arm.disclosure();
        assert!(d.contains("reconcile"), "{arm:?}: {d}");
        if arm.enabled() {
            assert!(
                d.contains("quarantine"),
                "{arm:?}: an ON disclosure must name the policy in force: {d}"
            );
        }
    }
    assert!(
        ReconcileGate::LiveDefault.disclosure().contains("VIKE_RECONCILE_OFF"),
        "the DEFAULT-on arm must tell the operator how to refuse it"
    );
    assert!(
        ReconcileGate::LiveDefault.disclosure().contains("AUTHENTICATED READS"),
        "the DEFAULT-on arm must say that it talks to the venue — that is the consequence an \
             operator did not opt into"
    );
}

/// The pairing HONOURS an explicit policy verbatim: it is a default, not a ceiling. An
/// operator who has proven a venue's position-report completeness and typed `hybrid` gets
/// `hybrid` — including its auto-fold of a local-origin `MissingFill`.
#[test]
fn the_quarantine_pairing_only_fills_an_unset_policy() {
    let explicit = quarantine_first_default(map(&[("VIKE_RECONCILE_POLICY", "hybrid")]));
    let cfg = build_recon_config(&explicit, HashMap::new());
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
    assert!(
        !auto_applied_kinds(&cfg.policy).is_empty(),
        "hybrid folds something — if it did not, this test would pass with the pairing removed"
    );
}

/// …and it touches nothing else in the map it is handed. The daemon threads ONE map through
/// the whole `VIKE_RECONCILE_*` family plus its own reads, so a fold that rewrote a neighbour
/// would be a settings bug wearing a reconcile hat.
#[test]
fn the_quarantine_pairing_rewrites_no_other_key() {
    let before =
        map(&[("VIKE_RECONCILE_INTERVAL_MS", "5000"), ("VIKE_RECONCILE_LOOKBACK_MS", "7200000")]);
    let after = quarantine_first_default(before.clone());
    for (k, v) in &before {
        assert_eq!(after.get(k), Some(v), "{k} must survive the fold verbatim");
    }
    assert_eq!(after.len(), before.len() + 1, "exactly one key is added");
}
// -- reconcile_enabled ------------------------------------------------------------------

#[test]
fn reconcile_enabled_true_only_for_exact_one() {
    assert!(reconcile_enabled(&map(&[("VIKE_RECONCILE", "1")])));
    assert!(!reconcile_enabled(&map(&[("VIKE_RECONCILE", "true")])));
    assert!(!reconcile_enabled(&map(&[("VIKE_RECONCILE", "0")])));
    assert!(!reconcile_enabled(&map(&[])));
}

// -- generate_missing_orders_enabled --------------------------------------------------------

#[test]
fn generate_missing_orders_enabled_true_only_for_exact_one() {
    assert!(generate_missing_orders_enabled(&map(&[("VIKE_RECONCILE_GENERATE_MISSING", "1")])));
    assert!(!generate_missing_orders_enabled(&map(&[("VIKE_RECONCILE_GENERATE_MISSING", "true")])));
    assert!(!generate_missing_orders_enabled(&map(&[("VIKE_RECONCILE_GENERATE_MISSING", "0")])));
    assert!(!generate_missing_orders_enabled(&map(&[])));
}

#[test]
fn build_recon_config_wires_generate_missing_orders_from_env() {
    let off = build_recon_config(&map(&[]), HashMap::new());
    assert!(!off.generate_missing_orders, "unset defaults to false");

    let on = build_recon_config(&map(&[("VIKE_RECONCILE_GENERATE_MISSING", "1")]), HashMap::new());
    assert!(on.generate_missing_orders);
}

// -- inflight_confirm_interval (recon path-to-superset, F1-A) -------------------------------

#[test]
fn inflight_confirm_interval_unset_is_none() {
    // OFF by default: no knob ⇒ no fast in-flight confirm timer, byte-identical core.
    assert_eq!(inflight_confirm_interval(&map(&[])), None);
}

#[test]
fn inflight_confirm_interval_zero_is_none() {
    // Explicit opt-out spelling, same as the interval/audit knobs.
    assert_eq!(inflight_confirm_interval(&map(&[("VIKE_RECONCILE_INFLIGHT_MS", "0")])), None);
}

#[test]
fn inflight_confirm_interval_positive_value_parses() {
    assert_eq!(
        inflight_confirm_interval(&map(&[("VIKE_RECONCILE_INFLIGHT_MS", "2000")])),
        Some(Duration::from_millis(2000))
    );
}

#[test]
fn inflight_confirm_interval_malformed_falls_back_to_none() {
    // Malformed ⇒ the `None` default (feature stays off), never a panic.
    assert_eq!(
        inflight_confirm_interval(&map(&[("VIKE_RECONCILE_INFLIGHT_MS", "not-a-number")])),
        None
    );
}

// -- reconcile_balance_enabled (Feature 2) --------------------------------------------------

#[test]
fn reconcile_balance_enabled_true_only_for_exact_one() {
    assert!(reconcile_balance_enabled(&map(&[("VIKE_RECONCILE_BALANCE", "1")])));
    assert!(!reconcile_balance_enabled(&map(&[("VIKE_RECONCILE_BALANCE", "true")])));
    assert!(!reconcile_balance_enabled(&map(&[("VIKE_RECONCILE_BALANCE", "0")])));
    assert!(!reconcile_balance_enabled(&map(&[])));
}

#[test]
fn build_recon_config_wires_reconcile_balance_from_env() {
    let off = build_recon_config(&map(&[]), HashMap::new());
    assert!(!off.reconcile_balance, "unset defaults to false (byte-identical legacy seed)");

    let on = build_recon_config(&map(&[("VIKE_RECONCILE_BALANCE", "1")]), HashMap::new());
    assert!(on.reconcile_balance);
}

// -- balance tolerance parse (VIKE_RECONCILE_BALANCE_TOL_{ABS,REL}) --------------------------

#[test]
fn balance_tol_unset_is_the_conservative_default() {
    // Both knobs unset ⇒ exactly BalanceTol::default() ⇒ byte-identical to the pre-knob diff.
    assert_eq!(parse_balance_tol(&map(&[])), BalanceTol::default());
}

#[test]
fn balance_tol_overrides_each_band_independently() {
    let def = BalanceTol::default();

    // ABS only: abs overridden, rel keeps its default.
    let abs_only = parse_balance_tol(&map(&[("VIKE_RECONCILE_BALANCE_TOL_ABS", "25.0")]));
    assert_eq!(abs_only.abs_floor, 25.0);
    assert_eq!(abs_only.rel_frac, def.rel_frac);

    // REL only: rel overridden, abs keeps its default.
    let rel_only = parse_balance_tol(&map(&[("VIKE_RECONCILE_BALANCE_TOL_REL", "0.0005")]));
    assert_eq!(rel_only.rel_frac, 0.0005);
    assert_eq!(rel_only.abs_floor, def.abs_floor);

    // Both set: both overridden.
    let both = parse_balance_tol(&map(&[
        ("VIKE_RECONCILE_BALANCE_TOL_ABS", "10.0"),
        ("VIKE_RECONCILE_BALANCE_TOL_REL", "0.002"),
    ]));
    assert_eq!(both, BalanceTol { abs_floor: 10.0, rel_frac: 0.002 });
}

#[test]
fn balance_tol_malformed_or_nonfinite_falls_back_per_band_no_panic() {
    let def = BalanceTol::default();
    // Malformed ABS with a valid REL: only the bad band falls back, the good one applies.
    let mixed = parse_balance_tol(&map(&[
        ("VIKE_RECONCILE_BALANCE_TOL_ABS", "not-a-number"),
        ("VIKE_RECONCILE_BALANCE_TOL_REL", "0.003"),
    ]));
    assert_eq!(mixed.abs_floor, def.abs_floor, "malformed abs falls back to default");
    assert_eq!(mixed.rel_frac, 0.003);
    // NaN/inf parse as f64 but are rejected (a non-finite tolerance is a "never flags" footgun).
    let nan = parse_balance_tol(&map(&[
        ("VIKE_RECONCILE_BALANCE_TOL_ABS", "NaN"),
        ("VIKE_RECONCILE_BALANCE_TOL_REL", "inf"),
    ]));
    assert_eq!(nan, def);
}

#[test]
fn build_recon_config_wires_balance_tol_from_env() {
    // Unset ⇒ the default rides into ReconConfig unchanged.
    let off = build_recon_config(&map(&[]), HashMap::new());
    assert_eq!(off.balance_tol, BalanceTol::default());

    // Set ⇒ the overridden tolerance is threaded into ReconConfig (and thence each pass).
    let on = build_recon_config(
        &map(&[
            ("VIKE_RECONCILE_BALANCE_TOL_ABS", "50.0"),
            ("VIKE_RECONCILE_BALANCE_TOL_REL", "0.001"),
        ]),
        HashMap::new(),
    );
    assert_eq!(on.balance_tol, BalanceTol { abs_floor: 50.0, rel_frac: 0.001 });
}

// -- policy parse -------------------------------------------------------------------------

#[test]
fn policy_hybrid_synthesizes_local_origin_quarantines_the_rest() {
    let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "hybrid")]));
    assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
    assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
}

#[test]
fn policy_synthesize_is_synthesize_default_with_no_overrides() {
    let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "synthesize")]));
    assert_eq!(p.default, ReconMode::Synthesize);
    // no hybrid overrides: even the no-local-origin kinds synthesize
    assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Synthesize);
}

#[test]
fn policy_quarantine_is_quarantine_default_with_no_overrides() {
    let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "quarantine")]));
    assert_eq!(p.default, ReconMode::Quarantine);
    assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Quarantine);
}

#[test]
fn policy_unknown_or_absent_defaults_to_hybrid() {
    let unset = parse_policy(&map(&[]));
    assert_eq!(unset.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
    assert_eq!(unset.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);

    let bogus = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "bogus")]));
    assert_eq!(bogus.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
}

#[test]
fn policy_parse_is_case_insensitive() {
    let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "SYNTHESIZE")]));
    assert_eq!(p.default, ReconMode::Synthesize);

    // ...including the new hold-first value (same lowercasing path as the other three).
    let eq = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "External-Quarantine")]));
    assert_eq!(eq.mode_for(DivergenceKind::PositionDrift), ReconMode::Quarantine);
}

/// `external-quarantine` = `hybrid` with the EXTERNAL-origin kinds held: `PositionDrift`
/// stops auto-applying, the already-quarantined External kinds stay held, and hybrid's
/// local-origin folds are untouched. `ReconPolicy::external_quarantine`'s doc is the
/// authority; the exhaustive per-kind pin lives in
/// `crates/vike-exec/tests/recon/recon_policy_pin.rs`.
#[test]
fn policy_external_quarantine_holds_external_kinds_and_keeps_local_folds() {
    let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "external-quarantine")]));
    assert_eq!(p.mode_for(DivergenceKind::PositionDrift), ReconMode::Quarantine);
    assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
    assert_eq!(p.mode_for(DivergenceKind::BalanceDrift), ReconMode::Quarantine);
    assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
    // ...and it is the ONE policy that opts in to the per-divergence refinement (the
    // coid-less MissingFill hold); the other three arms must leave the flag false, which is
    // what makes them byte-identical by construction.
    assert!(p.hold_external_instances);
    for value in ["hybrid", "synthesize", "quarantine"] {
        let flat = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", value)]));
        assert!(!flat.hold_external_instances, "{value} must not opt in");
    }
}

/// Near-miss spellings fall back to `hybrid` — the PRE-EXISTING unknown-value contract
/// (`policy_unknown_or_absent_defaults_to_hybrid`), pinned separately for the one value where
/// the fallback direction is the dangerous one: a typo'd hold-first policy AUTO-APPLIES
/// `PositionDrift`. [`log_policy_effect`]'s startup line naming the resolved fold set is the
/// operator's check that the spelling took.
#[test]
fn policy_external_quarantine_typos_fall_back_to_hybrid_which_folds_more() {
    for typo in ["external_quarantine", "externalquarantine", "external"] {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", typo)]));
        assert_eq!(
            p.mode_for(DivergenceKind::PositionDrift),
            ReconMode::Synthesize,
            "{typo}: an unrecognized value keeps the documented hybrid fallback"
        );
    }
}

// -- what each policy actually auto-folds ---------------------------------------------------

/// The DEFAULT policy an operator gets from an unset `VIKE_RECONCILE_POLICY` folds exactly two
/// kinds without asking. `OrphanLocalOrder` is NOT one of them — it folds no events at all
/// (`crates/vike-exec/tests/recon/recon_policy_pin.rs`), which is the fact six documents got
/// backwards. Its reclassification to a quarantined kind therefore left this list unchanged,
/// which is the point: what it gained was an operator ALERT, not an automatic fold.
#[test]
fn hybrid_auto_applies_missing_fill_and_position_drift_only() {
    let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "hybrid")]));
    let kinds = auto_applied_kinds(&p);
    assert_eq!(
        kinds.iter().map(|a| a.kind).collect::<Vec<_>>(),
        vec![DivergenceKind::MissingFill, DivergenceKind::PositionDrift]
    );
    assert!(kinds.iter().all(|a| !a.coid_linked_only), "hybrid's folds are unconditional");
    // The startup line's rendering is BYTE-IDENTICAL to the pre-qualifier era — the three
    // pre-existing policies' operator-facing output must not move.
    assert_eq!(format!("{kinds:?}"), "[MissingFill, PositionDrift]");
}

/// `quarantine` is the only policy under which the reported list is empty — which is what makes
/// the startup line worth printing.
#[test]
fn quarantine_auto_applies_nothing() {
    let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "quarantine")]));
    assert!(auto_applied_kinds(&p).is_empty());
}

/// `external-quarantine`'s whole point, stated as what the operator's startup line will say:
/// only `MissingFill` still folds without a confirm — `PositionDrift` left the list (the
/// delta from [`hybrid_auto_applies_missing_fill_and_position_drift_only`]) — and the row is
/// QUALIFIED: only coid-LINKED fills fold; a coid-less one (a foreign order's fill) is held
/// for an operator claim (`vike_exec::recon::mode_applies_divergence`'s refinement). The
/// rendered vocabulary is pinned because the startup line is the operator's check that the
/// policy took — an unqualified `MissingFill` here would claim a blanket fold that no longer
/// happens.
#[test]
fn external_quarantine_auto_applies_missing_fill_coid_linked_only() {
    let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "external-quarantine")]));
    let kinds = auto_applied_kinds(&p);
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].kind, DivergenceKind::MissingFill);
    assert!(kinds[0].coid_linked_only, "the foreign sub-case is held, so the row qualifies");
    assert_eq!(format!("{kinds:?}"), "[MissingFill (coid-linked only)]");
}

/// The probe asks the real authority instead of restating its rule — and for a coid-LINKED
/// fill (nothing to refine) the instance-level answer must equal the kind-level one under
/// every reachable policy, which is what lets [`auto_applied_kinds`] keep filtering rows on
/// `mode_applies` alone.
#[test]
fn probe_agrees_with_the_kind_level_answer_when_nothing_refines() {
    let linked = match coidless_missing_fill_probe() {
        Divergence::MissingFill(mut f) => {
            f.client_order_id = Some("c-1".into());
            Divergence::MissingFill(f)
        }
        _ => unreachable!("the probe is a MissingFill by construction"),
    };
    for value in ["hybrid", "synthesize", "quarantine", "external-quarantine"] {
        let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", value)]));
        assert_eq!(
            vike_exec::recon::mode_applies_divergence(&p, &linked),
            vike_exec::recon::mode_applies(&p, DivergenceKind::MissingFill),
            "{value}: a coid-linked fill must answer at the kind level"
        );
    }
}

#[test]
fn synthesize_auto_applies_every_venue_reported_kind() {
    let p = parse_policy(&map(&[("VIKE_RECONCILE_POLICY", "synthesize")]));
    let kinds = auto_applied_kinds(&p);
    assert_eq!(
        kinds.iter().map(|a| a.kind).collect::<Vec<_>>(),
        vec![
            DivergenceKind::MissingFill,
            DivergenceKind::PositionDrift,
            DivergenceKind::UnknownOrder,
            DivergenceKind::PositionOnlyExternal,
            DivergenceKind::BalanceDrift,
        ]
    );
    assert!(kinds.iter().all(|a| !a.coid_linked_only), "synthesize folds unconditionally");
}

// -- interval / audit parse ----------------------------------------------------------------

const DEFAULT_INTERVAL: Option<Duration> = Some(Duration::from_millis(60_000));

#[test]
fn interval_zero_is_none() {
    let v = map(&[("VIKE_RECONCILE_INTERVAL_MS", "0")]);
    assert_eq!(parse_ms_opt(&v, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL), None);
}

#[test]
fn interval_absent_defaults_to_60s() {
    let v = map(&[]);
    assert_eq!(parse_ms_opt(&v, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL), DEFAULT_INTERVAL);
}

#[test]
fn interval_positive_value_parses() {
    let v = map(&[("VIKE_RECONCILE_INTERVAL_MS", "5000")]);
    assert_eq!(
        parse_ms_opt(&v, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL),
        Some(Duration::from_millis(5000))
    );
}

#[test]
fn audit_ms_defaults_to_whatever_interval_resolved_to() {
    // interval unset (defaults 60s) -> audit unset mirrors it.
    let v1 = map(&[]);
    let interval1 = parse_ms_opt(&v1, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL);
    assert_eq!(parse_ms_opt(&v1, "VIKE_RECONCILE_AUDIT_MS", interval1), interval1);

    // interval explicitly disabled (0) -> unset audit mirrors None too.
    let v2 = map(&[("VIKE_RECONCILE_INTERVAL_MS", "0")]);
    let interval2 = parse_ms_opt(&v2, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL);
    assert_eq!(interval2, None);
    assert_eq!(parse_ms_opt(&v2, "VIKE_RECONCILE_AUDIT_MS", interval2), None);

    // audit set independently of a disabled interval.
    let v3 = map(&[("VIKE_RECONCILE_INTERVAL_MS", "0"), ("VIKE_RECONCILE_AUDIT_MS", "30000")]);
    let interval3 = parse_ms_opt(&v3, "VIKE_RECONCILE_INTERVAL_MS", DEFAULT_INTERVAL);
    assert_eq!(interval3, None);
    assert_eq!(
        parse_ms_opt(&v3, "VIKE_RECONCILE_AUDIT_MS", interval3),
        Some(Duration::from_millis(30_000))
    );
}

// -- lookback / startup delay --------------------------------------------------------------

#[test]
fn lookback_ms_default_and_override() {
    assert_eq!(parse_i64(&map(&[]), "VIKE_RECONCILE_LOOKBACK_MS", 3_600_000), 3_600_000);
    let v = map(&[("VIKE_RECONCILE_LOOKBACK_MS", "7200000")]);
    assert_eq!(parse_i64(&v, "VIKE_RECONCILE_LOOKBACK_MS", 3_600_000), 7_200_000);
}

#[test]
fn malformed_numeric_env_falls_back_to_default() {
    let v = map(&[("VIKE_RECONCILE_LOOKBACK_MS", "not-a-number")]);
    assert_eq!(parse_i64(&v, "VIKE_RECONCILE_LOOKBACK_MS", 3_600_000), 3_600_000);
}

/// The public window accessor and the `ReconConfig` the venue leg is driven by must be ONE
/// answer — that is the entire reason [`lookback_ms`] is `pub`. `vike-app`'s journal leg
/// carried its own `3_600_000` literal until 2026-08-30, and the two agreed only while nobody
/// set the variable; this pins that they now cannot disagree for any value.
#[test]
fn the_public_lookback_is_the_same_window_build_recon_config_uses() {
    for vars in [
        map(&[]),
        map(&[("VIKE_RECONCILE_LOOKBACK_MS", "21600000")]),
        map(&[("VIKE_RECONCILE_LOOKBACK_MS", "not-a-number")]),
        map(&[("VIKE_RECONCILE_LOOKBACK_MS", "0")]),
    ] {
        assert_eq!(
            lookback_ms(&vars),
            build_recon_config(&vars, HashMap::new()).lookback_ms,
            "the journal leg and the venue leg must resolve ONE window: {vars:?}"
        );
    }
    // ...and the unset answer is still the documented hour, spelled once.
    assert_eq!(lookback_ms(&map(&[])), DEFAULT_LOOKBACK_MS);
    assert_eq!(DEFAULT_LOOKBACK_MS, 3_600_000);
    // A six-hour window — the value that used to disarm the journal leg for five of its six
    // hours — is honoured rather than clamped to the default.
    assert_eq!(lookback_ms(&map(&[("VIKE_RECONCILE_LOOKBACK_MS", "21600000")])), 21_600_000);
}

// -- health_from_feed_status ----------------------------------------------------------------

#[test]
fn health_connected_is_healthy() {
    assert_eq!(health_from_feed_status("Connected"), ReconHealth::Healthy);
    assert_eq!(health_from_feed_status("LIVE \u{b7} Binance"), ReconHealth::Healthy);
}

#[test]
fn health_explicit_disconnected_is_degraded() {
    assert_eq!(health_from_feed_status("Disconnected"), ReconHealth::Degraded);
    assert_eq!(health_from_feed_status("disconnected"), ReconHealth::Degraded);
}

#[test]
fn health_idle_never_started_is_healthy() {
    // NEW: an empty/idle status — the constructor default before any feed activity, or a
    // venue whose feed never started because no chart subscribed to it — is Healthy, not
    // Degraded. This is the live-verify bugfix (2026-07-18): these used to permanently
    // suppress bybit/okx/hyperliquid/aster's reconcile legs.
    assert_eq!(health_from_feed_status(""), ReconHealth::Healthy);
    assert_eq!(health_from_feed_status("   "), ReconHealth::Healthy);
    assert_eq!(health_from_feed_status("idle"), ReconHealth::Healthy);
    assert_eq!(health_from_feed_status("\u{2014}"), ReconHealth::Healthy);
}

#[test]
fn health_error_is_degraded_but_connecting_is_healthy() {
    assert_eq!(health_from_feed_status("feed fault: reset"), ReconHealth::Degraded);
    assert_eq!(
        health_from_feed_status("btcusdt@kline_1m ws error (reconnecting): timeout"),
        ReconHealth::Degraded
    );
    // NEW: "connecting to X…" is every venue feed's real constructor-default status string
    // until a chart subscribes and its WS pump actually runs — no longer treated as a fault.
    assert_eq!(health_from_feed_status("connecting to Binance"), ReconHealth::Healthy);
    assert_eq!(health_from_feed_status("connecting to Bybit\u{2026}"), ReconHealth::Healthy);
    assert_eq!(health_from_feed_status("reconnecting"), ReconHealth::Healthy);
}

#[test]
fn health_unknown_is_healthy() {
    assert_eq!(health_from_feed_status("some unrecognized status text"), ReconHealth::Healthy);
}

// -- build_recon_config wiring (end-to-end smoke) --------------------------------------------

#[test]
fn build_recon_config_wires_env_and_health_closure() {
    let vars = map(&[
        ("VIKE_RECONCILE_POLICY", "quarantine"),
        ("VIKE_RECONCILE_LOOKBACK_MS", "1000"),
        ("VIKE_RECONCILE_STARTUP_DELAY_MS", "500"),
        ("VIKE_RECONCILE_INTERVAL_MS", "0"),
    ]);
    // An actually-failing status (Error), not "connecting" — a never-started/idle feed is
    // Healthy under the new mapping, so this test uses a genuine fault to still exercise the
    // Degraded->Healthy recovery transition below.
    let binance_status = Arc::new(Mutex::new("feed fault: reset".to_string()));
    let bybit_status = Arc::new(Mutex::new("Connected".to_string()));
    let feeds: HashMap<String, Arc<Mutex<String>>> = [
        ("binance".to_string(), Arc::clone(&binance_status)),
        ("bybit".to_string(), Arc::clone(&bybit_status)),
    ]
    .into_iter()
    .collect();
    let cfg = build_recon_config(&vars, feeds);
    assert_eq!(cfg.policy.default, ReconMode::Quarantine);
    assert_eq!(cfg.lookback_ms, 1000);
    assert_eq!(cfg.startup_delay, Duration::from_millis(500));
    assert_eq!(cfg.interval, None);
    assert_eq!(cfg.audit_interval, None); // mirrors interval: VIKE_RECONCILE_AUDIT_MS unset
    assert!(!cfg.generate_missing_orders);

    // PER-VENUE: binance's fault status is Degraded while bybit "Connected" is Healthy — in
    // the SAME config. An unknown venue (no map entry) is never blocked (Healthy).
    let health = cfg.health.expect("health closure always Some");
    assert_eq!(health("binance"), ReconHealth::Degraded);
    assert_eq!(health("bybit"), ReconHealth::Healthy);
    assert_eq!(health("deribit"), ReconHealth::Healthy, "un-mapped venue never blocked");

    // binance recovers → its own leg reads Healthy, independent of the others.
    *binance_status.lock().unwrap() = "LIVE \u{b7} Binance".to_string();
    assert_eq!(health("binance"), ReconHealth::Healthy);
}

#[test]
fn build_recon_config_defaults_when_vars_are_empty() {
    let cfg = build_recon_config(&map(&[]), HashMap::new());
    assert_eq!(cfg.lookback_ms, 3_600_000);
    assert_eq!(cfg.startup_delay, Duration::from_millis(2_000));
    assert_eq!(cfg.interval, DEFAULT_INTERVAL);
    assert_eq!(cfg.audit_interval, DEFAULT_INTERVAL);
    assert_eq!(cfg.balance_tol, BalanceTol::default());
}
