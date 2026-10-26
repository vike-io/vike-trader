use super::*;
use vike_exec::recon::{DivergenceKind, ReconMode};

/// A [`ReconSettings`] with only the policy row written.
fn policy(word: &str) -> ReconSettings {
    ReconSettings { policy: Some(word.to_string()), ..ReconSettings::default() }
}

// -- S2: the default-ON gate, and the quarantine it is paired with ------------------------

/// **THE S2 property.** A start that arms a live venue account and writes NOTHING reconciles —
/// and reconciles under a policy that folds nothing. Both halves are asserted here because
/// either alone is a defect: the gate without the pairing auto-applies `PositionDrift` at the
/// first pass after a restart, and the pairing without the gate reconciles nobody.
///
/// The fold set is ASKED of [`auto_applied_kinds`] rather than restated as a list of kinds —
/// `crates/vike-exec/CLAUDE.md` and `crates/vike-exec/tests/recon/recon_policy_pin.rs`'s module doc
/// spell out why a hand-written fold list rots (the "hybrid auto-cancels `OrphanLocalOrder`" claim
/// was stated in six places and was false).
#[test]
fn a_live_start_that_configures_nothing_reconciles_and_folds_nothing() {
    let gate = reconcile_gate(false, false, 1);
    assert_eq!(gate, ReconcileGate::LiveDefault);
    assert!(gate.enabled(), "one armed live account is enough — this is the S2 default");

    let cfg = build_recon_config(&ReconSettings::default(), HashMap::new());
    assert_eq!(cfg.policy.default, ReconMode::Quarantine);
    assert!(
        auto_applied_kinds(&cfg.policy).is_empty(),
        "a default-on reconcile must fold NOTHING without an operator; auto-applied set was \
             {:?}",
        auto_applied_kinds(&cfg.policy)
    );
}

/// The gate is scoped to LIVE starts by its own input: no armed live account, nothing asked,
/// no driver. This is the paper box, and it is the half that keeps a default-on reconcile from
/// meaning "every process now talks to a venue".
#[test]
fn a_paper_start_arms_no_reconcile_driver() {
    let gate = reconcile_gate(false, false, 0);
    assert_eq!(gate, ReconcileGate::NothingArmedLive);
    assert!(!gate.enabled());
}

/// An operator who refuses gets the refusal, and gets it even against a `flags.reconcile` row
/// that says `true` — the case where a box was armed months ago and the person at the
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
/// start, and this is the operator's override for exactly that.
#[test]
fn an_explicit_request_still_arms_a_start_the_probe_reports_as_paper() {
    let gate = reconcile_gate(true, false, 0);
    assert_eq!(gate, ReconcileGate::Requested);
    assert!(gate.enabled());
}

/// **A refused reconcile closes the order-restore venue gate for every venue, and no other verdict
/// touches it.** The refusal starts no driver, so no pass will ever adopt a restored order; with the
/// declared table's gate the core would mute the strategy's tagged resubmit for a pass that cannot
/// come. The other three verdicts get `restores_orders` itself (compared by what it answers for
/// every roster venue: a box that reconciles builds the gate it always did).
///
/// The precondition is what keeps this from proving nothing: with every row of the table off, "the
/// refusal closes it" and "it was closed anyway" read the same.
#[test]
fn a_refused_reconcile_closes_the_order_restore_gate_and_no_other_verdict_does() {
    use vike_venues::venue_restore::restores_orders;

    assert!(
        vike_model::VENUES.iter().copied().any(restores_orders),
        "precondition: the declared table enables a venue, else this test proves nothing"
    );

    let refused = restore_venue_gate(ReconcileGate::RefusedByOperator);
    for &venue in vike_model::VENUES {
        assert!(!refused(venue), "{venue}: nothing may wait for a pass on a box that refused them");
    }
    assert!(!refused("not-a-venue"));

    for gate in
        [ReconcileGate::Requested, ReconcileGate::LiveDefault, ReconcileGate::NothingArmedLive]
    {
        let kept = restore_venue_gate(gate);
        for &venue in vike_model::VENUES {
            assert_eq!(kept(venue), restores_orders(venue), "{gate:?} {venue}: the declared table");
        }
    }
}

/// The refusal's startup line says what the restore gate does about it, so an operator reading the
/// journal after a restart learns that the orders it left resting are not adopted, and does not
/// have to infer it from a strategy quoting twice.
#[test]
fn the_refusal_disclosure_says_restored_orders_are_not_adopted() {
    let d = ReconcileGate::RefusedByOperator.disclosure();
    assert!(d.contains("never adopted"), "{d}");
    assert!(d.contains("flags.reconcile_off"), "{d}");
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
        assert!(
            !d.contains(concat!("VIKE", "_RECONCILE")),
            "{arm:?} names a retired variable: {d}"
        );
        if arm.enabled() {
            assert!(
                d.contains("quarantine"),
                "{arm:?}: an ON disclosure must name the policy in force: {d}"
            );
        }
    }
    assert!(
        ReconcileGate::LiveDefault.disclosure().contains("flags.reconcile_off"),
        "the DEFAULT-on arm must tell the operator how to refuse it"
    );
    assert!(
        ReconcileGate::LiveDefault.disclosure().contains("AUTHENTICATED READS"),
        "the DEFAULT-on arm must say that it talks to the venue — that is the consequence an \
             operator did not opt into"
    );
}

/// The quarantine-first default HONOURS an explicit policy row verbatim: it is a default, not a
/// ceiling. An operator who has proven a venue's position-report completeness and wrote `hybrid`
/// gets `hybrid` — including its auto-fold of a local-origin `MissingFill`.
#[test]
fn the_quarantine_default_only_fills_an_unset_policy() {
    let cfg = build_recon_config(&policy("hybrid"), HashMap::new());
    assert_eq!(cfg.policy.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
    assert!(
        !auto_applied_kinds(&cfg.policy).is_empty(),
        "hybrid folds something — if it did not, this test would pass with the default removed"
    );
}

// -- the two flags --------------------------------------------------------------------------

#[test]
fn build_recon_config_wires_generate_missing_orders_from_its_flag() {
    let off = build_recon_config(&ReconSettings::default(), HashMap::new());
    assert!(!off.generate_missing_orders, "no row defaults to false");

    let on = build_recon_config(
        &ReconSettings { generate_missing: true, ..ReconSettings::default() },
        HashMap::new(),
    );
    assert!(on.generate_missing_orders);
}

#[test]
fn build_recon_config_wires_reconcile_balance_from_its_flag() {
    let off = build_recon_config(&ReconSettings::default(), HashMap::new());
    assert!(!off.reconcile_balance, "no row defaults to false (byte-identical legacy seed)");

    let on = build_recon_config(
        &ReconSettings { balance: true, ..ReconSettings::default() },
        HashMap::new(),
    );
    assert!(on.reconcile_balance);
}

/// `vike_config::config::RECONCILE_POLICIES` — the words `config.reconcile_policy` accepts — is a
/// copy of what `ReconPolicy::from_policy_name` accepts (the settings crate cannot name
/// `vike-exec`). Held equal here: every word names a real policy and resolves to it, so a row can
/// never carry a word [`parse_policy`] does not know.
#[test]
fn every_reconcile_policy_word_names_a_real_policy() {
    for word in vike_config::config::RECONCILE_POLICIES {
        let named = ReconPolicy::from_policy_name(word);
        assert!(named.is_some(), "{word} names no policy");
        let cfg = build_recon_config(&policy(word), HashMap::new());
        // `ReconPolicy` has no `PartialEq`; its `Debug` is a deterministic rendering (a `BTreeMap`
        // per-kind table), so equal renderings are equal policies.
        assert_eq!(format!("{:?}", Some(cfg.policy)), format!("{named:?}"), "{word}");
    }
    for near_miss in ["quarintine", "external_quarantine", "external", ""] {
        assert!(ReconPolicy::from_policy_name(near_miss).is_none(), "{near_miss:?}");
    }
}

// -- the cash tolerance --------------------------------------------------------------------

#[test]
fn balance_tol_unset_is_the_conservative_default() {
    // Both rows unset ⇒ exactly BalanceTol::default() ⇒ byte-identical to the pre-knob diff.
    let cfg = build_recon_config(&ReconSettings::default(), HashMap::new());
    assert_eq!(cfg.balance_tol, BalanceTol::default());
}

#[test]
fn balance_tol_overrides_each_band_independently() {
    let def = BalanceTol::default();
    let tol = |abs: Option<f64>, rel: Option<f64>| {
        build_recon_config(
            &ReconSettings {
                balance_tol_abs: abs,
                balance_tol_rel: rel,
                ..ReconSettings::default()
            },
            HashMap::new(),
        )
        .balance_tol
    };

    // ABS only: abs overridden, rel keeps its default.
    let abs_only = tol(Some(25.0), None);
    assert_eq!(abs_only.abs_floor, 25.0);
    assert_eq!(abs_only.rel_frac, def.rel_frac);

    // REL only: rel overridden, abs keeps its default.
    let rel_only = tol(None, Some(0.0005));
    assert_eq!(rel_only.rel_frac, 0.0005);
    assert_eq!(rel_only.abs_floor, def.abs_floor);

    // Both set: both overridden.
    assert_eq!(tol(Some(10.0), Some(0.002)), BalanceTol { abs_floor: 10.0, rel_frac: 0.002 });
}

// -- policy -------------------------------------------------------------------------------

#[test]
fn policy_hybrid_synthesizes_local_origin_quarantines_the_rest() {
    let p = parse_policy(Some("hybrid"));
    assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
    assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
}

#[test]
fn policy_synthesize_is_synthesize_default_with_no_overrides() {
    let p = parse_policy(Some("synthesize"));
    assert_eq!(p.default, ReconMode::Synthesize);
    // no hybrid overrides: even the no-local-origin kinds synthesize
    assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Synthesize);
}

#[test]
fn policy_quarantine_is_quarantine_default_with_no_overrides() {
    let p = parse_policy(Some("quarantine"));
    assert_eq!(p.default, ReconMode::Quarantine);
    assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Quarantine);
}

/// **No row is `quarantine`, and so is a word only a bypassed loader could deliver** — the
/// direction that folds LESS. Before decision 0111 the variable's reader fell back to `hybrid`,
/// the policy that auto-applies `PositionDrift`, and only a separate map fold every root had to
/// remember kept a default-on start off it.
#[test]
fn policy_unset_or_unknown_is_quarantine() {
    for row in [None, Some("bogus"), Some("quarintine"), Some("external_quarantine")] {
        let p = parse_policy(row);
        assert_eq!(p.default, ReconMode::Quarantine, "{row:?}");
        assert!(auto_applied_kinds(&p).is_empty(), "{row:?} must fold nothing");
    }
}

/// `external-quarantine` = `hybrid` with the EXTERNAL-origin kinds held: `PositionDrift`
/// stops auto-applying, the already-quarantined External kinds stay held, and hybrid's
/// local-origin folds are untouched. `ReconPolicy::external_quarantine`'s doc is the
/// authority; the exhaustive per-kind pin lives in
/// `crates/vike-exec/tests/recon/recon_policy_pin.rs`.
#[test]
fn policy_external_quarantine_holds_external_kinds_and_keeps_local_folds() {
    let p = parse_policy(Some("external-quarantine"));
    assert_eq!(p.mode_for(DivergenceKind::PositionDrift), ReconMode::Quarantine);
    assert_eq!(p.mode_for(DivergenceKind::UnknownOrder), ReconMode::Quarantine);
    assert_eq!(p.mode_for(DivergenceKind::BalanceDrift), ReconMode::Quarantine);
    assert_eq!(p.mode_for(DivergenceKind::MissingFill), ReconMode::Synthesize);
    // ...and it is the ONE policy that opts in to the per-divergence refinement (the
    // coid-less MissingFill hold); the other three arms must leave the flag false, which is
    // what makes them byte-identical by construction.
    assert!(p.hold_external_instances);
    for value in ["hybrid", "synthesize", "quarantine"] {
        let flat = parse_policy(Some(value));
        assert!(!flat.hold_external_instances, "{value} must not opt in");
    }
}

// -- what each policy actually auto-folds ---------------------------------------------------

/// `hybrid` folds exactly two kinds without asking. `OrphanLocalOrder` is NOT one of them — it
/// folds no events at all (`crates/vike-exec/tests/recon/recon_policy_pin.rs`), which is the fact
/// six documents got backwards. Its reclassification to a quarantined kind therefore left this
/// list unchanged, which is the point: what it gained was an operator ALERT, not an automatic fold.
#[test]
fn hybrid_auto_applies_missing_fill_and_position_drift_only() {
    let p = parse_policy(Some("hybrid"));
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
    let p = parse_policy(Some("quarantine"));
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
    let p = parse_policy(Some("external-quarantine"));
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
        let p = parse_policy(Some(value));
        assert_eq!(
            vike_exec::recon::mode_applies_divergence(&p, &linked),
            vike_exec::recon::mode_applies(&p, DivergenceKind::MissingFill),
            "{value}: a coid-linked fill must answer at the kind level"
        );
    }
}

#[test]
fn synthesize_auto_applies_every_venue_reported_kind() {
    let p = parse_policy(Some("synthesize"));
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

// -- interval / audit ---------------------------------------------------------------------

const DEFAULT_INTERVAL: Option<Duration> = Some(Duration::from_millis(60_000));

#[test]
fn an_interval_row_of_zero_is_none_and_no_row_is_60s() {
    assert_eq!(cadence(Some(0), DEFAULT_INTERVAL), None);
    assert_eq!(cadence(None, DEFAULT_INTERVAL), DEFAULT_INTERVAL);
    assert_eq!(cadence(Some(5_000), DEFAULT_INTERVAL), Some(Duration::from_millis(5_000)));
}

#[test]
fn audit_ms_defaults_to_whatever_interval_resolved_to() {
    let cfg = |interval_ms: Option<u32>, audit_ms: Option<u32>| {
        build_recon_config(
            &ReconSettings { interval_ms, audit_ms, ..ReconSettings::default() },
            HashMap::new(),
        )
    };
    // interval unset (defaults 60s) -> audit unset mirrors it.
    let c = cfg(None, None);
    assert_eq!(c.audit_interval, c.interval);
    assert_eq!(c.interval, DEFAULT_INTERVAL);

    // interval explicitly disabled (0) -> unset audit mirrors None too.
    let c = cfg(Some(0), None);
    assert_eq!(c.interval, None);
    assert_eq!(c.audit_interval, None);

    // audit set independently of a disabled interval.
    let c = cfg(Some(0), Some(30_000));
    assert_eq!(c.interval, None);
    assert_eq!(c.audit_interval, Some(Duration::from_millis(30_000)));
}

// -- lookback / startup delay --------------------------------------------------------------

/// The public window accessor and the `ReconConfig` the venue leg is driven by must be ONE
/// answer — that is the entire reason [`lookback_ms`] is `pub`. `vike-app`'s journal leg
/// carried its own `3_600_000` literal until 2026-08-30, and the two agreed only while nobody
/// set the window; this pins that they now cannot disagree for any value.
#[test]
fn the_public_lookback_is_the_same_window_build_recon_config_uses() {
    for row in [None, Some(21_600_000), Some(1)] {
        assert_eq!(
            lookback_ms(row),
            build_recon_config(
                &ReconSettings { lookback_ms: row, ..ReconSettings::default() },
                HashMap::new()
            )
            .lookback_ms,
            "the journal leg and the venue leg must resolve ONE window: {row:?}"
        );
    }
    // ...and the unset answer is still the documented hour, spelled once.
    assert_eq!(lookback_ms(None), DEFAULT_LOOKBACK_MS);
    assert_eq!(DEFAULT_LOOKBACK_MS, 3_600_000);
    // A six-hour window — the value that used to disarm the journal leg for five of its six
    // hours — is honoured rather than clamped to the default.
    assert_eq!(lookback_ms(Some(21_600_000)), 21_600_000);
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
    // An empty/idle status — the constructor default before any feed activity, or a venue whose
    // feed never started because no chart subscribed to it — is Healthy, not Degraded. This is the
    // live-verify bugfix (2026-07-18): these used to permanently suppress
    // bybit/okx/hyperliquid/aster's reconcile legs.
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
    // "connecting to X…" is every venue feed's real constructor-default status string until a
    // chart subscribes and its WS pump actually runs — not treated as a fault.
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
fn build_recon_config_wires_the_rows_and_the_health_closure() {
    let recon = ReconSettings {
        policy: Some("quarantine".to_string()),
        lookback_ms: Some(1000),
        startup_delay_ms: Some(500),
        interval_ms: Some(0),
        ..ReconSettings::default()
    };
    // An actually-failing status (Error), not "connecting" — a never-started/idle feed is
    // Healthy under the mapping, so this test uses a genuine fault to still exercise the
    // Degraded->Healthy recovery transition below.
    let binance_status = Arc::new(Mutex::new("feed fault: reset".to_string()));
    let bybit_status = Arc::new(Mutex::new("Connected".to_string()));
    let feeds: HashMap<String, Arc<Mutex<String>>> = [
        ("binance".to_string(), Arc::clone(&binance_status)),
        ("bybit".to_string(), Arc::clone(&bybit_status)),
    ]
    .into_iter()
    .collect();
    let cfg = build_recon_config(&recon, feeds);
    assert_eq!(cfg.policy.default, ReconMode::Quarantine);
    assert_eq!(cfg.lookback_ms, 1000);
    assert_eq!(cfg.startup_delay, Duration::from_millis(500));
    assert_eq!(cfg.interval, None);
    assert_eq!(cfg.audit_interval, None); // mirrors the interval: no audit row
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

/// **The first pass starts inside half of the window in which a restored order mutes its
/// strategy's tagged resubmit** — for no row, for `0`, and for the longest delay the settings
/// loader accepts (`vike_config`'s `Config::apply` refuses anything above
/// `MAX_RECONCILE_STARTUP_DELAY_MS`). This crate is where the two numbers meet:
/// `build_recon_config` turns the row into the driver's `startup_delay`, and the window is
/// `vike-core`'s (`RESTORED_TAG_DENY_WINDOW_MS`, which lives beside the bound in `vike-model`). The
/// other half of the window is the room the pass itself needs (its venues are fetched one after
/// another).
#[test]
fn the_first_pass_starts_inside_half_the_restored_tag_refusal_window() {
    use vike_venues::venue_restore::{MAX_RECONCILE_STARTUP_DELAY_MS, RESTORED_TAG_DENY_WINDOW_MS};

    let window = Duration::from_millis(
        u64::try_from(RESTORED_TAG_DENY_WINDOW_MS).expect("a window is not negative"),
    );
    for row in [None, Some(0), Some(DEFAULT_STARTUP_DELAY_MS), Some(MAX_RECONCILE_STARTUP_DELAY_MS)]
    {
        let settings = ReconSettings { startup_delay_ms: row, ..ReconSettings::default() };
        let cfg = build_recon_config(&settings, HashMap::new());
        assert!(
            cfg.startup_delay * 2 <= window,
            "{row:?}: the first pass starts after {:?}, more than half of the {window:?} refusal \
             window",
            cfg.startup_delay
        );
    }
}

#[test]
fn build_recon_config_defaults_when_no_row_is_written() {
    let cfg = build_recon_config(&ReconSettings::default(), HashMap::new());
    assert_eq!(cfg.lookback_ms, 3_600_000);
    assert_eq!(cfg.startup_delay, Duration::from_millis(2_000));
    assert_eq!(cfg.interval, DEFAULT_INTERVAL);
    assert_eq!(cfg.audit_interval, DEFAULT_INTERVAL);
    assert_eq!(cfg.balance_tol, BalanceTol::default());
}

// ---- wave 2, W13: a policy that rewrites positions does not ride a replaying ring ----------------------

/// **`hybrid` and `synthesize` are refused over the gatewayhub; `quarantine` and `external-quarantine`
/// are not** (the W3c double count: a `PositionDrift` the pass synthesized is booked again by the ring
/// replay). The verdict is COMPUTED from `auto_applied_kinds`, never a list of words: a fifth policy that
/// folded `PositionDrift` would be refused without an edit here.
#[test]
fn a_policy_that_folds_position_drift_is_refused_over_a_replaying_ring_and_the_holding_ones_are_not()
 {
    for word in vike_config::config::RECONCILE_POLICIES {
        let policy = parse_policy(Some(word));
        let folds_drift =
            auto_applied_kinds(&policy).iter().any(|a| a.kind == DivergenceKind::PositionDrift);
        let verdict = refuse_replaying_policy(&policy);
        assert_eq!(
            verdict.is_err(),
            folds_drift,
            "{word}: the refusal follows what the policy folds"
        );
        if let Err(why) = verdict {
            assert!(why.contains("quarantine") && why.contains("PositionDrift"), "{word}: {why}");
        }
    }
    assert!(refuse_replaying_policy(&parse_policy(None)).is_ok(), "no row is quarantine");
    assert!(refuse_replaying_policy(&parse_policy(Some("quarantine"))).is_ok());
    assert!(refuse_replaying_policy(&parse_policy(Some("hybrid"))).is_err());
    assert!(refuse_replaying_policy(&parse_policy(Some("synthesize"))).is_err());
}

/// The start asks it only when reconcile runs and the backend replays: an in-process backend, or a
/// reconcile the operator refused, is untouched (a refactor changes no working behaviour). W13b finding
/// 9: a `hybrid`/`synthesize` row (written while the daemon ran, or left from before) is OVERRIDDEN with
/// `quarantine`, never refused: a refusal at start was a restart loop with live orders resting.
#[test]
fn a_replaying_start_overrides_a_position_rewriting_policy_with_quarantine_and_never_refuses() {
    for word in ["hybrid", "synthesize"] {
        let row = parse_policy(Some(word));
        assert!(!auto_applied_kinds(&row).is_empty(), "precondition: {word} folds something");
        let got = quarantine_when_replaying(true, true, row.clone());
        assert!(
            auto_applied_kinds(&got).is_empty(),
            "{word} is overridden: the manager built from it folds nothing"
        );
        assert_eq!(got.default, ReconMode::Quarantine);
        assert_eq!(
            quarantine_when_replaying(true, false, row.clone()).default,
            row.default,
            "an in-process backend replays nothing"
        );
        assert_eq!(
            quarantine_when_replaying(false, true, row.clone()).default,
            row.default,
            "no reconcile, no drift"
        );
    }
    for word in ["quarantine", "external-quarantine"] {
        let row = parse_policy(Some(word));
        assert_eq!(
            quarantine_when_replaying(true, true, row.clone()).default,
            row.default,
            "{word} holds as written"
        );
    }
    let default = parse_policy(None);
    assert_eq!(quarantine_when_replaying(true, true, default.clone()).default, default.default);
}

/// **B.10 first-pass arithmetic, over the calls the pass really makes** (W13b finding 6). The first
/// reconcile pass has to beat the 120 s window in which a restored order mutes its strategy's tagged
/// resubmit. Nothing is enqueued before the pass has fetched everything, one call after the other, each
/// at most one venue call at the bridges' own 30 s HTTP timeout: the default `fetch_mass_status` is
/// THREE fetches (orders, fills, positions), then `fetch_balance`. The count below is MEASURED off the
/// real manager (a counting client behind `spawn_recon`), not restated, and the hop adds no further
/// wait: its reply bound is the silence bound of each fetch (a fetch that keeps reporting progress is
/// never clipped, one that is silent for the bound is a dead hop and the pass is SKIPPED), so a 30 s
/// venue call is the whole 30 s.
///
/// ⚠ **The worst case does not fit the window**: the startup delay plus four full 30 s calls is 122 s
/// against 120 s. This test PINS that fact (`DOUBTS` of the W13b notes carry the smallest safe change)
/// rather than asserting a fit that is false; it goes red the day either number moves, which is the day
/// the arithmetic must be re-read. A realistic pass (a venue answering in seconds) is nowhere near it,
/// and the longer-delay rows keep the older rule above (at most half the window).
#[test]
fn the_default_first_pass_worst_case_is_pinned_against_the_restored_tag_window() {
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use vike_exec::recon::ReconClient;
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
    use vike_gatewayhub_client::proto::{FETCH_PROGRESS_EVERY, FETCH_STALL_FLOOR};
    use vike_gatewayhub_client::trade::TradeTimings;
    use vike_model::{FillReport, OrderStatusReport, PositionStatusReport, RiskLimits};
    use vike_venues::venue_restore::RESTORED_TAG_DENY_WINDOW_MS;

    const VENUE_CALL_BOUND: Duration = Duration::from_secs(30);

    /// Every fetch the manager makes, counted in the order it makes them.
    struct Counting {
        calls: Arc<AtomicU32>,
        balance_asked: Arc<AtomicUsize>,
    }
    impl ReconClient for Counting {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<OrderStatusReport>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn fetch_balance(&self) -> Result<Option<f64>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.balance_asked.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    }

    let (calls, balance_asked) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicUsize::new(0)));
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let handle = vike_core::spawn_core(engine, vike_core::CoreConfig::default());
    let leg = vike_tradehub_core::ReconLeg::sole_account_of(
        "binance",
        Box::new(Counting { calls: Arc::clone(&calls), balance_asked: Arc::clone(&balance_asked) }),
    );
    let one_pass = vike_tradehub_core::ReconConfig {
        startup_delay: Duration::ZERO,
        interval: None,
        audit_interval: None,
        ..build_recon_config(&ReconSettings::default(), HashMap::new())
    };
    let driver = vike_tradehub_core::spawn_recon(&handle, vec![leg], one_pass, None);
    let started = std::time::Instant::now();
    while balance_asked.load(Ordering::SeqCst) == 0 {
        assert!(started.elapsed() < Duration::from_secs(10), "the startup pass never ran");
        std::thread::sleep(Duration::from_millis(5));
    }
    driver.shutdown();
    handle.shutdown_and_join();
    let fetches = calls.load(Ordering::SeqCst);
    assert_eq!(
        fetches, 4,
        "orders, fills, positions and balance: the pass makes four venue calls before it enqueues \
         anything (the older arithmetic modelled two)"
    );

    let window = Duration::from_millis(
        u64::try_from(RESTORED_TAG_DENY_WINDOW_MS).expect("a window is not negative"),
    );
    let delay = build_recon_config(&ReconSettings::default(), HashMap::new()).startup_delay;
    assert_eq!(delay, Duration::from_millis(u64::from(DEFAULT_STARTUP_DELAY_MS)));
    let hop = vike_gatewayhub_client::client::stall_bound(
        TradeTimings::default().recon_stall_floor,
        u64::try_from(FETCH_PROGRESS_EVERY.as_millis()).expect("a small cadence"),
    );
    assert_eq!(
        hop,
        FETCH_STALL_FLOOR.max(FETCH_PROGRESS_EVERY * 3),
        "the bound is the client's own"
    );
    assert!(
        hop <= VENUE_CALL_BOUND,
        "the hop's bound is each fetch's silence bound, not an extra wait"
    );

    let worst_case = delay + VENUE_CALL_BOUND * fetches;
    assert_eq!(
        worst_case,
        window + delay,
        "KNOWN: the worst case ({worst_case:?}) overshoots the {window:?} window by the startup delay \
         alone. Raise the window or shorten the pass, then make this a fit (W13b DOUBTS)"
    );
}
