use super::*;

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// The SECOND question the table answers — "is this box armed for live?" — asked of a
/// PROCESS-ENVIRONMENT map, and held to the same grammar as the refusal so the two can never
/// disagree about what counts as armed. Its consumer is `vike-cli config check`, which uses it
/// to decide whether an unreadable credential store is a degrade or a refusal.
#[test]
fn armed_settings_in_reads_the_same_table_with_the_same_grammar() {
    assert!(armed_settings_in(&map(&[])).is_empty());
    for s in CREDENTIAL_FILE_ARMING_REFUSED {
        let armed = armed_settings_in(&map(&[(s.var, "1")]));
        assert_eq!(armed.len(), 1, "{} must read as armed", s.var);
        assert_eq!(armed[0].var, s.var);
        // …and the disarming spellings the refusal also ignores.
        for v in ["0", "", "  ", "true", "# 1"] {
            assert!(
                armed_settings_in(&map(&[(s.var, v)])).is_empty(),
                "{v:?} arms nothing at any reader"
            );
        }
    }
    // The refusal is now this function plus a message, so the two are the same predicate.
    let both = map(&[("BINANCE_MAINNET", "1"), ("POLY_EXEC", "1  # arbdub only")]);
    assert_eq!(armed_settings_in(&both).len(), 2);
    assert!(refuse_credential_file_arming(&both).is_err());
}

/// THE ESCALATION: one appended line, and the process would have gone live. It now stops.
#[test]
fn an_arming_line_in_the_credential_file_refuses_to_start() {
    for s in CREDENTIAL_FILE_ARMING_REFUSED {
        let err = refuse_credential_file_arming(&map(&[(s.var, "1")]))
            .expect_err("an arming credential-file line must refuse");
        assert!(err.contains(s.var), "the refusal must NAME the variable, got {err}");
        assert!(err.contains(s.instead), "and must say what to do instead, got {err}");
    }
}

/// The annotated spelling the real credential file uses, and which the Polymarket readers
/// normalise with `first_token`, must not slip past — otherwise the refusal is evaded by a
/// trailing comment while the venue still arms.
#[test]
fn a_trailing_comment_does_not_evade_the_refusal() {
    assert!(
        refuse_credential_file_arming(&map(&[("POLY_EXEC", "1  # real money, arbdub only")]))
            .is_err()
    );
    assert!(refuse_credential_file_arming(&map(&[("BINANCE_MAINNET", " 1 ")])).is_err());
}

/// A DISARMING line is not an offence. Refusing `BINANCE_MAINNET=0` would fire on a
/// configuration that arms nothing at any reader, and a check that cries wolf gets worked
/// around.
#[test]
fn a_disarming_or_absent_value_starts_normally() {
    assert!(refuse_credential_file_arming(&map(&[])).is_ok());
    for v in ["0", "", "  ", "true", "yes", "# 1"] {
        assert!(
            refuse_credential_file_arming(&map(&[("BINANCE_MAINNET", v)])).is_ok(),
            "{v:?} arms nothing and must not refuse"
        );
    }
}

/// Ordinary credentials are untouched — this must never fire on the file's actual contents.
#[test]
fn a_normal_credential_file_is_not_refused() {
    assert!(
        refuse_credential_file_arming(&map(&[
            ("BINANCE_DEMO_API_KEY", "k"),
            ("BINANCE_DEMO_API_SECRET", "s"),
            ("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xdead"),
            ("POLY_EXEC_MARKETS", "btc-updown-5m"),
        ]))
        .is_ok()
    );
}

/// Every offender is reported in ONE pass.
#[test]
fn all_offenders_are_named_at_once() {
    let err = refuse_credential_file_arming(&map(&[("BINANCE_MAINNET", "1"), ("POLY_EXEC", "1")]))
        .expect_err("two offenders");
    assert!(err.contains("BINANCE_MAINNET") && err.contains("POLY_EXEC"), "got {err}");
}

// -- "is this box armed for live?" — the two-source union -----------------------------------

fn live(tradehub_live: bool) -> Flags {
    Flags { tradehub_live, ..Default::default() }
}

/// **The residual this function exists to close, driven directly.** A box whose ONLY arming is
/// `flags.tradehub_live` reads as ARMED — with an EMPTY process environment, i.e. with no
/// `{VENUE}_MAINNET` anywhere, which is exactly the shape of a box live on a SWITCHLESS venue
/// (deribit/alpaca/aster/…). Before this source existed [`armed_settings_in`] answered "not
/// armed" for it, and `vike-cli config check` degraded an unreadable credential store on a live
/// node.
#[test]
fn the_node_scoped_source_arms_with_no_venue_variable_anywhere() {
    let v = armed_for_live(Some(live(true)), &map(&[]));
    assert_eq!(v, LiveArmingVerdict::Armed(vec![TRADEHUB_LIVE_ARMING]), "{v:?}");
    assert!(v.refuses());
    assert_eq!(v.evidence()[0].scope, ArmingScope::Node, "it speaks for the whole mount");
    // …and the venue-scoped half genuinely saw nothing, so this is the NEW source answering.
    assert!(armed_settings_in(&map(&[])).is_empty());
}

/// The flag OFF is the paper box `docs/decisions/0013-degrade-vs-refuse.md` protects: nothing
/// armed, so an unreadable store stays a degrade. Same tree, one boolean apart — and
/// `Unarmed` is the ONLY verdict that does not refuse.
#[test]
fn the_node_scoped_source_is_silent_on_a_paper_box() {
    let v = armed_for_live(Some(live(false)), &map(&[]));
    assert_eq!(v, LiveArmingVerdict::Unarmed);
    assert!(!v.refuses(), "a paper box must keep starting — ADR 0013's degrade");
    assert!(v.evidence().is_empty());
}

/// ⚠ **The hole one level up: a tree that did not LOAD is not a paper box.** `None` must never
/// answer `Unarmed`, because that is the same "no signal ⇒ assume paper" inference the
/// node-scoped source was added to end. It refuses instead.
#[test]
fn an_unresolvable_settings_tree_is_undetermined_and_still_refuses() {
    let v = armed_for_live(None, &map(&[]));
    assert_eq!(v, LiveArmingVerdict::Undetermined);
    assert!(v.refuses(), "'I could not tell' must not be spelled 'not armed'");
    assert!(v.evidence().is_empty(), "…and it must not invent evidence either");
    // A venue-scoped row still answers on its own — positive evidence needs no corroboration,
    // and naming it beats an anonymous refusal.
    let armer = CREDENTIAL_FILE_ARMING_REFUSED[0].var;
    let v = armed_for_live(None, &map(&[(armer, "1")]));
    assert_eq!(v.evidence().len(), 1, "{v:?}");
    assert!(v.refuses());
}

/// ⚠ `Flags::default()` is a REAL answer (an absent `flags.toml`), not a stand-in for "unknown"
/// — the two are one `Option` apart and only one of them degrades. Pinned because passing the
/// default for an unresolved tree is the single call-site mistake that reopens the hole.
#[test]
fn default_flags_are_an_answer_and_none_is_not() {
    assert_eq!(armed_for_live(Some(Flags::default()), &map(&[])), LiveArmingVerdict::Unarmed);
    assert_eq!(armed_for_live(None, &map(&[])), LiveArmingVerdict::Undetermined);
}

/// Both sources at once, in table order, each carrying its own scope — the shape a refusal
/// message iterates so an operator fixing a box is handed the whole list.
#[test]
fn both_sources_are_reported_together_venue_rows_first() {
    let armer = CREDENTIAL_FILE_ARMING_REFUSED[0].var;
    let v = armed_for_live(Some(live(true)), &map(&[(armer, "1")]));
    let ev = v.evidence();
    assert_eq!(ev.len(), 2, "{v:?}");
    assert_eq!(ev[0], LiveArming::of_setting(&CREDENTIAL_FILE_ARMING_REFUSED[0]));
    assert_eq!(ev[0].scope, ArmingScope::Venue);
    assert_eq!(ev[1], TRADEHUB_LIVE_ARMING);
}

/// Every venue-scoped row still arms on its own, with the flag OFF — the pre-existing behaviour
/// this change must not narrow while widening the signal.
#[test]
fn every_table_row_still_arms_without_the_node_scoped_flag() {
    for s in CREDENTIAL_FILE_ARMING_REFUSED {
        let v = armed_for_live(Some(live(false)), &map(&[(s.var, "1")]));
        assert_eq!(v.evidence().len(), 1, "{} must still arm alone: {v:?}", s.var);
        assert_eq!(v.evidence()[0].source, s.var);
        assert!(v.refuses());
    }
}

/// The node-scoped source must name BOTH layers it resolves from. An operator told only
/// `VIKE_TRADEHUB_LIVE` greps a `flags.toml` that never mentioned it — or, on the CI box, greps
/// `flags.toml` while the arming line sits in the unit's `EnvironmentFile=`.
#[test]
fn the_node_scoped_source_names_both_layers_it_resolves_from() {
    assert!(TRADEHUB_LIVE_ARMING.source.contains("tradehub_live"), "the file KEY");
    assert!(TRADEHUB_LIVE_ARMING.source.contains("flags.toml"), "…and the file");
    assert!(TRADEHUB_LIVE_ARMING.source.contains(crate::flags::TRADEHUB_LIVE_ENV), "…and the env");
    assert!(TRADEHUB_LIVE_ARMING.arms.contains("SWITCHLESS"), "…and WHY it is the new source");
}

/// ⚠ **The two Polymarket flags ARE file-layer evidence now**, and this pins the FACT the fold
/// rests on rather than the prose — the same shape as the exclusion it replaces, pointed the
/// other way.
///
/// Its predecessor (`the_polymarket_flags_are_excluded_because_the_file_layer_arms_nothing`)
/// asserted `!is_consumed` for both keys and FAILED on the commit that wired them, which is the
/// whole reason the exclusion was gated instead of written down. If either is ever UN-wired,
/// this fails in turn and the fold has to be revisited.
#[test]
fn the_polymarket_flags_are_file_evidence_now() {
    for key in ["flags.poly_exec", "flags.poly_reconcile"] {
        assert!(
            crate::is_consumed(key),
            "{key} stopped being consumed — a file line arms nothing again, so counting it as \
                 evidence would refuse a box over a setting with no effect (see POLY_FILE_ARMING)"
        );
    }
    // A flags-file-only Polymarket arm is evidence on its own, with no variable set anywhere…
    let f = Flags { poly_exec: true, poly_reconcile: true, ..Default::default() };
    let v = armed_for_live(Some(f), &map(&[]));
    assert_eq!(v.evidence().len(), 2, "{v:?}");
    assert!(v.evidence().iter().all(|a| a.scope == ArmingScope::Venue), "{v:?}");
    assert!(v.evidence()[0].source.contains("flags.poly_exec"), "it must name the setting: {v:?}");
    assert!(v.refuses());
    // …and the retired PROCESS-ENV spelling of the SAME gate is not counted twice: an environment
    // still carrying it beside the row is one arm, one piece of evidence, reported under the
    // variable — the spelling somebody has to remove (decision 0095 retired the flag's environment
    // layer, so the row alone is what the resolved flag reads).
    let both = armed_for_live(Some(f), &map(&[("POLY_EXEC", "1")]));
    assert_eq!(both.evidence().len(), 2, "{both:?}");
    assert_eq!(both.evidence()[0].source, "POLY_EXEC", "the env row wins the tie: {both:?}");
}

/// Decision 0095: a stale Polymarket arming row in the credential store points at the FLAG — the
/// environment arms nothing any more.
#[test]
fn a_stale_polymarket_row_points_at_its_flag() {
    let err = refuse_credential_file_arming(&map(&[("POLY_EXEC", "1")])).unwrap_err();
    assert!(err.contains("vike-cli config set flags.poly_exec true"), "{err}");
    assert!(!err.contains("Environment=POLY_EXEC"), "{err}");
}

/// `VIKE_TRADEHUB_LIVE` must NOT join the credential-file refusal table: `vike-tradehub`
/// resolves it through `crate::load` over its own process-env sweep, the store is never merged
/// in, so a line in `secrets.env` arms nothing — and a refusal list that fires on harmless lines
/// trains operators to work around it (this module's table doc).
#[test]
fn the_node_scoped_flag_is_not_a_credential_file_offence() {
    assert!(
        !CREDENTIAL_FILE_ARMING_REFUSED.iter().any(|s| s.var == crate::flags::TRADEHUB_LIVE_ENV),
        "the credential file cannot arm this flag, so it must not be refused there"
    );
    assert!(refuse_credential_file_arming(&map(&[(crate::flags::TRADEHUB_LIVE_ENV, "1")])).is_ok());
}

/// The `{VENUE}_MAINNET` rows must stay in step with
/// `vike_secrets::live_means_mainnet::SWITCHED_VENUES` — the migration's own list. This crate
/// cannot depend on `vike-mount` (which used to carry the switch table), so the pin is spelled
/// here against the one list decision 0095 left, `vike-secrets`' `SWITCHED_VENUES`.
#[test]
fn the_mainnet_rows_are_exactly_the_switched_venues() {
    let mut rows: Vec<String> = CREDENTIAL_FILE_ARMING_REFUSED
        .iter()
        .map(|s| s.var)
        .filter(|v| v.ends_with("_MAINNET"))
        .map(str::to_string)
        .collect();
    rows.sort();
    let mut want: Vec<String> = vike_secrets::live_means_mainnet::SWITCHED_VENUES
        .iter()
        .map(|v| format!("{}_MAINNET", v.to_ascii_uppercase()))
        .collect();
    want.sort();
    assert_eq!(
        rows, want,
        "the switched-venue set changed — see vike_secrets::live_means_mainnet::SWITCHED_VENUES"
    );
}

/// Decision 0095: a stale switch row in the credential store points at the ceiling, not at the
/// process environment — the environment arms nothing any more.
#[test]
fn a_retired_switch_in_the_credential_store_points_at_the_ceiling() {
    let err = refuse_credential_file_arming(&map(&[("BYBIT_MAINNET", "1")])).unwrap_err();
    assert!(err.contains("vike-cli config set policy.venues.bybit live"), "{err}");
    assert!(!err.contains("Environment=BYBIT_MAINNET"), "{err}");
}
