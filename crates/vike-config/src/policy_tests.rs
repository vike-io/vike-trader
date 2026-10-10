use super::*;

fn file() -> &'static Path {
    Path::new("policy.toml")
}

#[test]
fn defaults_are_the_conservative_end() {
    let p = Policy::default();
    assert_eq!(p.max_leverage, 1.0);
    assert_eq!(p.max_notional_per_order, None);
    // `None`, NOT a number: every venue keeps its own historical band, byte-identically. A
    // default here would re-price the market orders of every existing deployment on upgrade.
    assert_eq!(p.market_slippage, None);
    // ⚠ `Admit`, NOT the stricter `Verify`, and here the conservative end is the PERMISSIVE
    // one — for once. `Verify` narrows what a halt lets OUT, i.e. what an operator can still
    // close with, so defaulting to it would change the behaviour of every existing deployment's
    // kill switch from a file nobody wrote. It is opt-in for the same reason `market_slippage`
    // is `None`.
    assert_eq!(p.halt_admit, HaltAdmit::Admit);
    // ⚠ ABSENT, like every other scalar. For one morning this assertion read
    // `assert_eq!(p.deadman_timeout_ms, DEFAULT_DEADMAN_TIMEOUT_MS)` — "the ONE default that
    // is the ARMED end" — and existed to make an "align it with the other scalars" edit a red
    // test. The alignment is now the ruling (the field's doc records why: the switch observes
    // silence, so an armed default halts every session-bounded venue at every close), and this
    // assertion is what makes a future "arm it by default, it is a safety switch" edit the red
    // test instead. `None` and `Some(0)` are DISTINCT here: the mount warns on the first only.
    assert_eq!(p.deadman_timeout_ms, None, "a policy that says nothing arms NO dead-man");
    assert_ne!(p.deadman_timeout_ms, Some(DEADMAN_DISABLED_MS), "absent is not explicit-off");
    assert_eq!(p.deadman_action, DeadManActionSetting::CancelAllAndHalt);
}

// -- the dead-man switch ------------------------------------------------------------------

/// The key loads from the file, `0` is the EXPLICIT-off spelling and loads as `Some(0)` (so
/// the mount can tell it from an absent key and stay quiet), and an unmentioned key INHERITS
/// (the layered-patch contract — which here runs the safe way round too: a later layer that
/// says nothing cannot re-arm a switch an earlier one disabled, nor arm one no layer named).
#[test]
fn the_deadman_timeout_loads_disables_on_zero_and_inherits_when_unmentioned() {
    let mut p = Policy::default();
    // A patch that never names the key leaves it ABSENT — this is the "file with no line"
    // case the mount warns on, and it must not read as `Some(0)`.
    p.apply(PolicyPatch { max_leverage: Some(5.0), ..Default::default() }, file()).unwrap();
    assert_eq!(p.deadman_timeout_ms, None, "unmentioned on the first layer stays absent");

    let patch: PolicyPatch = toml::from_str("deadman_timeout_ms = 5000\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(p.deadman_timeout_ms, Some(5000));

    let patch: PolicyPatch = toml::from_str("deadman_timeout_ms = 0\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(
        p.deadman_timeout_ms,
        Some(DEADMAN_DISABLED_MS),
        "zero is legal, means OFF, and is DISTINCT from absent"
    );

    p.apply(PolicyPatch { max_leverage: Some(5.0), ..Default::default() }, file()).unwrap();
    assert_eq!(p.deadman_timeout_ms, Some(0), "an unmentioned key inherits — it must not re-arm");
}

/// The LINK dead-man's absent-is-ARMED rule, which is the OPPOSITE of the sibling above and is
/// therefore the one thing about this key worth pinning on its own. Absent ⇒ the default grace;
/// `0` ⇒ off; a written value ⇒ itself.
#[test]
fn the_link_deadman_grace_is_armed_when_absent_and_off_only_at_zero() {
    let mut p = Policy::default();
    assert_eq!(p.link_deadman_grace_ms, None, "the raw field is absent on a bare default");
    assert_eq!(
        p.link_deadman_grace_ms_effective(),
        Some(DEFAULT_LINK_DEADMAN_GRACE_MS),
        "a policy that says NOTHING arms the link switch — the whole M13 default"
    );

    let patch: PolicyPatch = toml::from_str("link_deadman_grace_ms = 45000\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(p.link_deadman_grace_ms_effective(), Some(45_000));

    let patch: PolicyPatch = toml::from_str("link_deadman_grace_ms = 0\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(p.link_deadman_grace_ms, Some(LINK_DEADMAN_DISABLED_MS));
    assert_eq!(
        p.link_deadman_grace_ms_effective(),
        None,
        "zero is the ONLY off — and unlike the silence switch it needs no warning, because \
             writing it is the only way to reach off"
    );

    p.apply(PolicyPatch { max_leverage: Some(5.0), ..Default::default() }, file()).unwrap();
    assert_eq!(
        p.link_deadman_grace_ms_effective(),
        None,
        "an unmentioned key inherits — a later layer's silence must not re-arm it"
    );
}

/// Every value in `1..30000` is refused BY NAME — below one ordinary reconnect the switch
/// trips on the venue's own backoff — and the refusal names the measured cycle and both legal
/// moves.
#[test]
fn a_link_grace_below_one_reconnect_is_rejected_naming_the_cycle_and_both_moves() {
    for v in [1u64, 500, 23_000, MIN_LINK_DEADMAN_GRACE_MS - 1] {
        let err = Policy::default()
            .apply(PolicyPatch { link_deadman_grace_ms: Some(v), ..Default::default() }, file())
            .expect_err("{v} ms must be refused");
        let msg = err.to_string();
        assert!(msg.starts_with(&format!("policy.toml: link_deadman_grace_ms = {v} ")), "{msg}");
        assert!(msg.contains("below the minimum armed grace 30000 ms"), "{msg}");
        assert!(msg.contains("23 s"), "cites the measured reconnect cycle: {msg}");
        assert!(msg.contains("Write 0 to disable"), "names the OFF spelling: {msg}");
    }
}

/// Above an hour is refused — a link dead-man that never fires — and the message names the
/// unit, because seconds-written-as-milliseconds is the typo that lands here.
#[test]
fn a_link_grace_above_an_hour_is_rejected_and_names_the_unit() {
    for v in [MAX_LINK_DEADMAN_GRACE_MS + 1, 120_000_000, u64::MAX] {
        let err = Policy::default()
            .apply(PolicyPatch { link_deadman_grace_ms: Some(v), ..Default::default() }, file())
            .expect_err("{v} ms must be refused");
        let msg = err.to_string();
        assert!(msg.starts_with(&format!("policy.toml: link_deadman_grace_ms = {v} ")), "{msg}");
        assert!(msg.contains("exceeds the maximum 3600000 ms"), "{msg}");
        assert!(msg.contains("MILLISECONDS"), "names the unit: {msg}");
    }
}

/// Every value in `1..1000` is refused BY NAME, naming the file — a sub-second dead-man is a
/// false halt on any quiet second — and the refusal tells the operator both legal moves.
#[test]
fn a_sub_second_deadman_timeout_is_rejected_naming_file_key_and_the_two_legal_moves() {
    for v in [1u64, 2, 500, 999] {
        let err = Policy::default()
            .apply(PolicyPatch { deadman_timeout_ms: Some(v), ..Default::default() }, file())
            .expect_err("{v} ms must be refused");
        let msg = err.to_string();
        assert!(msg.starts_with(&format!("policy.toml: deadman_timeout_ms = {v} ")), "{msg}");
        assert!(msg.contains("below the minimum armed timeout 1000 ms"), "{msg}");
        assert!(msg.contains("false halt"), "says what the value would DO: {msg}");
        assert!(msg.contains("Write 0 to disable"), "names the OFF spelling: {msg}");
    }
}

/// Above a day is refused too — a switch that never fires — and the message names the unit,
/// because seconds-written-as-milliseconds is the typo that lands here.
#[test]
fn a_deadman_timeout_above_a_day_is_rejected_and_names_the_unit() {
    // ⚠ `600_000_000` (a "600 s" slip written as microseconds) rather than `60_000_000`: the
    // latter is 16.7 HOURS, inside the day, and is accepted. A day is a wide ceiling.
    for v in [MAX_DEADMAN_TIMEOUT_MS + 1, 600_000_000, u64::MAX] {
        let err = Policy::default()
            .apply(PolicyPatch { deadman_timeout_ms: Some(v), ..Default::default() }, file())
            .expect_err("{v} ms must be refused");
        let msg = err.to_string();
        assert!(msg.starts_with(&format!("policy.toml: deadman_timeout_ms = {v} ")), "{msg}");
        assert!(msg.contains("exceeds the maximum 86400000 ms"), "{msg}");
        assert!(msg.contains("MILLISECONDS"), "names the unit: {msg}");
    }
}

/// Both bounds are inclusive on the legal side, and the disabling zero sits outside the range
/// rather than at its bottom — pinned so a `<=` slip on either comparison fails here.
#[test]
fn the_deadman_timeout_bounds_are_inclusive_and_zero_is_its_own_case() {
    for v in [
        DEADMAN_DISABLED_MS,
        MIN_DEADMAN_TIMEOUT_MS,
        RECOMMENDED_DEADMAN_TIMEOUT_MS,
        MAX_DEADMAN_TIMEOUT_MS,
    ] {
        let mut p = Policy::default();
        p.apply(PolicyPatch { deadman_timeout_ms: Some(v), ..Default::default() }, file())
            .unwrap_or_else(|e| panic!("{v} must be accepted: {e}"));
        assert_eq!(p.deadman_timeout_ms, Some(v));
    }
    // …and the recommendation is itself a legal armed value, or the warning would be
    // recommending a line the loader refuses.
    assert!(
        (MIN_DEADMAN_TIMEOUT_MS..=MAX_DEADMAN_TIMEOUT_MS).contains(&RECOMMENDED_DEADMAN_TIMEOUT_MS)
    );
}

/// The action loads by its file spelling, defaults to the HALTING one, and an unknown
/// spelling is refused at DESERIALIZATION naming the legal two — so `"halt"` or `"cancel"`
/// cannot load as something else.
#[test]
fn the_deadman_action_loads_and_an_unknown_spelling_names_the_legal_ones() {
    let mut p = Policy::default();
    let patch: PolicyPatch = toml::from_str("deadman_action = \"cancel_all\"\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(p.deadman_action, DeadManActionSetting::CancelAll);

    let patch: PolicyPatch =
        toml::from_str("deadman_action = \"cancel_all_and_halt\"\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(p.deadman_action, DeadManActionSetting::CancelAllAndHalt);

    for bad in ["halt", "cancel", "CancelAll", "CANCEL_ALL", "off", "true", ""] {
        let err = toml::from_str::<PolicyPatch>(&format!("deadman_action = {bad:?}\n"))
            .expect_err("{bad} must not load");
        let msg = err.to_string();
        for legal in [DeadManActionSetting::CancelAllAndHalt, DeadManActionSetting::CancelAll] {
            assert!(msg.contains(legal.as_str()), "{bad}: {} missing from {msg}", legal.as_str());
        }
    }
}

/// The serialized shape carries both keys as plain leaves — the timeout as JSON `null` when
/// absent (the `max_notional_per_order` shape: a real leaf in JSON, an ABSENT key in TOML,
/// which is what lets `config show` report `default` for it) and as the integer when written;
/// the action as a string. The shape `crates/vike-config/src/provenance.rs`'s rows path into
/// and the templates are gated against. Pinned here because the `Serialize` impl is
/// hand-written, so a field added to the struct and forgotten there would otherwise vanish
/// from every disclosure surface at once.
#[test]
fn the_deadman_keys_serialize_as_leaves() {
    let json = serde_json::to_value(Policy::default()).expect("serializes");
    assert_eq!(json["deadman_timeout_ms"], serde_json::Value::Null, "absent is null, not 0");
    assert_eq!(json["deadman_action"], serde_json::json!("cancel_all_and_halt"));

    let written = Policy { deadman_timeout_ms: Some(60_000), ..Policy::default() };
    let json = serde_json::to_value(written).expect("serializes");
    assert_eq!(json["deadman_timeout_ms"], serde_json::json!(60_000));
}

/// The knob loads from the file it is supposed to live in, both ways round, and an unmentioned
/// key inherits rather than resetting (the layered-patch contract).
#[test]
fn halt_admit_loads_from_the_policy_file_and_inherits_when_unmentioned() {
    let mut p = Policy::default();
    let patch: PolicyPatch = toml::from_str("halt_admit = \"verify\"\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(p.halt_admit, HaltAdmit::Verify);

    // A later layer naming only another key must not reset it back to `admit`.
    p.apply(PolicyPatch { max_leverage: Some(5.0), ..Default::default() }, file()).unwrap();
    assert_eq!(p.halt_admit, HaltAdmit::Verify, "an unmentioned key inherits");

    // …and it can be set back explicitly.
    let patch: PolicyPatch = toml::from_str("halt_admit = \"admit\"\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(p.halt_admit, HaltAdmit::Admit);
}

/// An illegal mode is refused BY NAME at load, with the legal set in the message — including
/// `"refuse"`, the mode this design deliberately does not offer (it disarmed the panic button
/// in exactly the situations that halt on their own). An operator must not be able to write a
/// mode that silently loads as something else.
#[test]
fn an_unknown_halt_admit_mode_is_rejected_and_names_the_legal_ones() {
    for bad in ["refuse", "Verify", "VERIFY", "off", "true", ""] {
        let err = toml::from_str::<PolicyPatch>(&format!("halt_admit = {bad:?}\n"))
            .expect_err("{bad} must not load");
        let msg = err.to_string();
        assert!(msg.contains("admit") && msg.contains("verify"), "{bad}: {msg}");
    }
}

/// The tombstone REFUSES rather than ignoring — and the refusal has to be actionable, because
/// the operator who wrote this key wanted a real protection that exists in a different file.
#[test]
fn the_removed_max_total_exposure_key_is_refused_and_names_where_it_lives_now() {
    let err = Policy::default()
        .apply(PolicyPatch { max_total_exposure: Some(25_000.0), ..Default::default() }, file())
        .expect_err("a ceiling nothing reads must not load silently");
    let msg = err.to_string();
    assert!(msg.starts_with("policy.toml: max_total_exposure = 25000 "), "{msg}");
    assert!(msg.contains("no longer a policy key"), "says it is gone: {msg}");
    assert!(msg.contains("[risk]"), "names the table that replaces it: {msg}");
    assert!(msg.contains("RiskGate"), "names what actually enforces it: {msg}");
    // …and the redirect must NOT be sold as an equivalence. The operator who wrote THIS key
    // wanted a book-wide ceiling; the run-profile key they are sent to caps ONE symbol at ONE
    // venue (`vike_model::RiskLimits::max_total_exposure`'s doc is the authority, pinned by
    // `crates/vike-exec/tests/risk/risk_lane_pricing.rs`'s
    // `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`). Describing it as aggregate
    // would re-create this very field's defect one file over: a number that looks like the
    // protection asked for and is N× weaker.
    // ⚠ The refusal offers the aggregate too, so the disclaimer is carried by the CONTRAST —
    // "Per INSTRUMENT" against "Per ACCOUNT" — rather than by a negation. What must not weaken
    // is that the per-symbol redirect still declares its scope.
    assert!(
        msg.contains("ONE SYMBOL at ONE VENUE") && msg.contains("Per ACCOUNT"),
        "the refusal must state the per-instrument replacement's REAL scope AND offer the \
             aggregate as a separate answer, so neither is mistaken for the other: {msg}"
    );
    // ⚠ The refusal must hand the operator the aggregate key by name, and the verb that writes
    // the row, or this tombstone sends someone who asked for a book-wide ceiling to a
    // per-symbol one and stops there.
    assert!(
        msg.contains("`policy.max_account_exposure`") && msg.contains("vike-cli config set"),
        "must name the ACCOUNT-aggregate ceiling that now exists, and the verb that writes it: \
         {msg}"
    );
    assert!(
        !msg.contains("this same file") && !msg.contains("Delete this line"),
        "no file or line is left to name (decision 0086): {msg}"
    );
    assert!(
        msg.contains("no sanctioned way to remove this row yet")
            && msg.contains("no verb deletes a settings row")
            && msg.contains("`config unset`")
            && msg.contains("seal"),
        "a stale row has no sanctioned removal yet, and the refusal says why — a hand DELETE \
         trips the adoption seal — instead of sending the operator to one: {msg}"
    );
    assert!(
        !msg.contains("Remove this row") && !msg.contains("settings database itself"),
        "the refusal must not advise the hand DELETE the seal refuses: {msg}"
    );
    assert!(
        !msg.contains("no account-aggregate ceiling"),
        "the old 'it does not exist' sentence must be gone, not merely added to: {msg}"
    );
}

/// The ACCOUNT-aggregate ceiling loads, validates like its per-order sibling, and is ABSENT by
/// default — the three facts a `Policy` field owes before anything downstream may trust it.
///
/// ⚠ The absent arm is not decoration: this key's default is the UNCAPPED end (see the field's
/// own doc for why upgrade safety outranks the conservative default here), so a change that
/// gave it a number would silently start refusing orders on live accounts from a file nobody
/// wrote. That is the failure this arm exists to catch.
#[test]
fn the_account_ceiling_loads_validates_and_is_absent_by_default() {
    assert_eq!(
        Policy::default().max_account_exposure,
        None,
        "no policy rows ⇒ no account ceiling ⇒ the gate is byte-identical to before it existed"
    );
    let mut p = Policy::default();
    p.apply(PolicyPatch { max_account_exposure: Some(25_000.0), ..Default::default() }, file())
        .expect("a positive finite ceiling must load");
    assert_eq!(p.max_account_exposure, Some(25_000.0));
    // …and the refusals are the sibling's, by name and with the file attached. ZERO is the
    // interesting one: it would deny every opening order, i.e. a kill switch spelled sideways,
    // and this workspace has a real one for that.
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let err = Policy::default()
            .apply(PolicyPatch { max_account_exposure: Some(bad), ..Default::default() }, file())
            .expect_err("{bad} must not load as an account ceiling");
        let msg = err.to_string();
        assert!(
            msg.contains("policy.toml") && msg.contains("max_account_exposure"),
            "{bad}: the refusal must name the file and the key: {msg}"
        );
    }
}

/// …and it is refused on its own merits, not merely as a side effect of validation: a value
/// that would have PASSED `check_positive` still fails, and an out-of-range one does not
/// silently take the old "not finite / not positive" path instead.
#[test]
fn the_tombstone_refuses_every_value_including_ones_that_used_to_be_valid() {
    for v in [1.0, 25_000.0, f64::MAX, -1.0, 0.0, f64::NAN] {
        let err = Policy::default()
            .apply(PolicyPatch { max_total_exposure: Some(v), ..Default::default() }, file())
            .expect_err("{v} must be refused");
        assert!(err.to_string().contains("no longer a policy key"), "{v}: {err}");
    }
}

/// The tombstone is NOT a general amnesty on unknown keys: `deny_unknown_fields` still rejects
/// a genuine typo by name, which is the property that makes a mistyped ceiling visible.
#[test]
fn a_mistyped_key_is_still_rejected_by_name() {
    let err = toml::from_str::<PolicyPatch>("max_total_exposur = 1.0\n")
        .expect_err("a typo must not be accepted");
    assert!(err.to_string().contains("max_total_exposur"), "{err}");
}

#[test]
fn a_market_slippage_band_inside_the_range_is_accepted() {
    let mut p = Policy::default();
    p.apply(PolicyPatch { market_slippage: Some(0.002), ..Default::default() }, file()).unwrap();
    assert_eq!(p.market_slippage, Some(0.002));
    // Both endpoints are legal.
    for v in [MIN_MARKET_SLIPPAGE, MAX_MARKET_SLIPPAGE] {
        let mut p = Policy::default();
        p.apply(PolicyPatch { market_slippage: Some(v), ..Default::default() }, file()).unwrap();
        assert_eq!(p.market_slippage, Some(v));
    }
}

/// The money case: "set it to 50% so orders always fill" is refused BY NAME, at the file, rather
/// than silently clamped — an operator who wrote it must learn that they do not have it.
#[test]
fn a_market_slippage_band_above_the_ceiling_is_rejected_naming_file_key_and_bound() {
    let err = Policy::default()
        .apply(PolicyPatch { market_slippage: Some(0.5), ..Default::default() }, file())
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.starts_with("policy.toml: market_slippage = 0.5 "), "{msg}");
    assert!(msg.contains("exceeds the allowed maximum 0.05"), "{msg}");
}

/// A band too tight to cross the book cancels unfilled — on a tripped stop, an exit that did not
/// happen. Also refused, and the message says why rather than just quoting a number.
#[test]
fn a_market_slippage_band_below_the_floor_is_rejected() {
    let err = Policy::default()
        .apply(PolicyPatch { market_slippage: Some(0.0), ..Default::default() }, file())
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("below the allowed minimum 0.001"), "{msg}");
    assert!(msg.contains("did not exit"), "{msg}");
}

#[test]
fn a_non_finite_market_slippage_band_is_rejected() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let err = Policy::default()
            .apply(PolicyPatch { market_slippage: Some(bad), ..Default::default() }, file())
            .unwrap_err();
        assert!(err.to_string().contains("market_slippage"), "{err}");
    }
}

/// The two edges must agree on WHICH values are in bounds: this file edge (which rejects,
/// naming the key) and the wire edge (`vike_bridge_core::market_slippage`, which clamps) both
/// answer to `vike_model`'s range test. Driven off that predicate rather than restating the
/// numbers, so a bound that moved in one place and not the other fails here.
#[test]
fn accepted_bands_are_exactly_those_vike_model_calls_usable() {
    use vike_model::market_slippage::is_usable_market_slippage;
    for v in [
        0.0,
        0.0009,
        MIN_MARKET_SLIPPAGE,
        0.002,
        0.01,
        MAX_MARKET_SLIPPAGE,
        0.0500001,
        0.5,
        f64::NAN,
        f64::INFINITY,
    ] {
        let accepted = Policy::default()
            .apply(PolicyPatch { market_slippage: Some(v), ..Default::default() }, file())
            .is_ok();
        assert_eq!(
            accepted,
            is_usable_market_slippage(v),
            "the file edge and vike-model disagree about {v}"
        );
    }
}

#[test]
fn an_unmentioned_key_is_inherited_not_reset() {
    let mut p = Policy::default();
    p.apply(PolicyPatch { max_leverage: Some(10.0), ..Default::default() }, file()).unwrap();
    // A second patch naming only the band must not reset max_leverage to the code default.
    p.apply(PolicyPatch { market_slippage: Some(0.01), ..Default::default() }, file()).unwrap();
    assert_eq!(p.max_leverage, 10.0);
    assert_eq!(p.market_slippage, Some(0.01));
}

/// The SECOND tombstone: `[rate] max_utilization` was a ceiling on a preference nothing read.
///
/// It is refused rather than dropped for the same reason `max_total_exposure` is — an operator
/// who wrote it believes a pacing limit is armed, and `deny_unknown_fields`' generic "unknown
/// field" would tell them only that the key is wrong. Every value is refused, including ones
/// that used to VALIDATE (`0.6`) and ones that used to be rejected by the bounds (`1.5`,
/// `0.001`): the key means nothing now, so accepting any of them would be the false confirmation
/// this whole taxonomy exists to prevent.
#[test]
fn the_removed_rate_max_utilization_key_is_refused_and_names_where_the_number_lives_now() {
    for v in [0.6, 1.5, 0.001, 0.95, 0.05] {
        let err = Policy::default()
            .apply(
                PolicyPatch {
                    rate: Some(RatePolicyPatch { max_utilization: Some(v) }),
                    ..Default::default()
                },
                file(),
            )
            .expect_err("a ceiling that bounded nothing must not load silently");
        let msg = err.to_string();
        assert!(msg.starts_with("policy.toml: rate.max_utilization = "), "{msg}");
        assert!(msg.contains("no longer a policy key"), "says it is gone: {msg}");
        assert!(
            msg.contains("preferences.rate_utilization"),
            "names the dead value it clamped: {msg}"
        );
        assert!(
            msg.contains("DEFAULT_UTILIZATION"),
            "names where the number really comes from: {msg}"
        );
        assert!(
            msg.contains("no sanctioned way to remove this row yet")
                && msg.contains("no verb deletes a settings row")
                && msg.contains("`config unset`")
                && msg.contains("seal"),
            "a stale row has no sanctioned removal yet, and the refusal says why — a hand DELETE \
             trips the adoption seal — instead of sending the operator to one: {msg}"
        );
        assert!(
            !msg.contains("Remove this row") && !msg.contains("settings database itself"),
            "the refusal must not advise the hand DELETE the seal refuses: {msg}"
        );
        assert!(
            !msg.contains("preferences.toml"),
            "no settings file is a remedy (decision 0086): {msg}"
        );
    }
}

/// An EMPTY `[rate]` table is not an operator claiming a ceiling, so it loads — the refusal is
/// keyed on the value, exactly like the `max_total_exposure` tombstone.
#[test]
fn an_empty_rate_table_is_not_refused() {
    Policy::default()
        .apply(PolicyPatch { rate: Some(RatePolicyPatch::default()), ..Default::default() }, file())
        .expect("`[rate]` with nothing in it claims nothing");
}

#[test]
fn sub_1x_leverage_is_rejected() {
    let err = Policy::default()
        .apply(PolicyPatch { max_leverage: Some(0.0), ..Default::default() }, file())
        .unwrap_err();
    assert!(err.to_string().starts_with("policy.toml: max_leverage = 0 "), "{err}");
}

// -- `[venues]` — the per-venue arming ceilings ------------------------------------------------

/// **The default is a FILLED map: one `paper` entry per roster venue.** Exhaustive over
/// `vike_model::VENUES`, so a new bridge crate reddens this until it has a ceiling — the same
/// contract every per-venue capability table carries.
///
/// ⚠ Not merely "the default is safe": an EMPTY map would also be safe to READ and would make
/// `crates/vike-config/tests/provenance.rs`'s completeness gate pass vacuously (its walk only
/// records a leaf at a non-object node), so the setting would exist and `vike-cli config show`
/// would never mention it. The length assertion is what distinguishes the two.
#[test]
fn every_roster_venue_defaults_to_paper() {
    let p = Policy::default();
    assert_eq!(p.venues.len(), VENUES.len(), "one entry per roster venue, never an empty map");
    for venue in VENUES {
        assert_eq!(p.venues.get(venue), VenueMode::Paper, "{venue} must default to paper");
    }
}

/// A `[venues]` table sets the venues it names and leaves every other one alone — the layered
/// -patch contract, applied one level down inside a map.
#[test]
fn a_venues_table_sets_what_it_names_and_inherits_the_rest() {
    let mut p = Policy::default();
    let patch: PolicyPatch =
        toml::from_str("[venues]\nbybit = \"live\"\nbinance = \"demo\"\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(p.venues.get("bybit"), VenueMode::Live);
    assert_eq!(p.venues.get("binance"), VenueMode::Demo);
    assert_eq!(p.venues.get("okx"), VenueMode::Paper, "an unnamed venue keeps the default");

    // A later layer naming only ANOTHER venue must not reset the first two.
    let patch: PolicyPatch = toml::from_str("[venues]\nokx = \"demo\"\n").expect("parses");
    p.apply(patch, file()).unwrap();
    assert_eq!(p.venues.get("bybit"), VenueMode::Live, "an unmentioned venue inherits");
    assert_eq!(p.venues.get("okx"), VenueMode::Demo);

    // …and a patch naming no `[venues]` table at all leaves the whole map alone.
    p.apply(PolicyPatch { max_leverage: Some(5.0), ..Default::default() }, file()).unwrap();
    assert_eq!(p.venues.get("bybit"), VenueMode::Live);
}

/// **An unknown VENUE is refused by name, with the roster in the message** — the half
/// `deny_unknown_fields` structurally cannot do, because serde treats a map's keys as data.
///
/// The dangerous direction is the reason: an accepted `venues.bybitt = "paper"` reads to its
/// author as "bybit is held to paper", and bybit would not be.
#[test]
fn an_unknown_venue_is_refused_by_name_and_the_roster_is_listed() {
    for bad in ["bybitt", "kraken", "", "binance-futures"] {
        let text = format!("[venues]\n{bad:?} = \"paper\"\n");
        let patch: PolicyPatch = toml::from_str(&text).expect("the MODE is legal, so it parses");
        let err = Policy::default()
            .apply(patch, file())
            .expect_err("a ceiling on a venue that does not exist must not load");
        let msg = err.to_string();
        assert!(msg.starts_with(&format!("policy.toml: venues.{bad} = ")), "{msg}");
        assert!(msg.contains("names no venue"), "{msg}");
        assert!(msg.contains("apply to NOTHING"), "says what the line would do: {msg}");
        for venue in VENUES {
            assert!(msg.contains(*venue), "the roster must be listed; {venue} missing: {msg}");
        }
    }
}

/// A CASE mismatch is refused too — and the refusal suggests the canonical id rather than
/// silently repairing it. A spelling the loader fixes on your behalf is a spelling nobody
/// learns, and the next reader of the file greps the tree for a key that is not there.
#[test]
fn a_miscased_venue_is_refused_and_the_canonical_id_is_suggested() {
    let patch: PolicyPatch = toml::from_str("[venues]\nBybit = \"live\"\n").expect("parses");
    let err = Policy::default().apply(patch, file()).expect_err("ids are lowercase");
    let msg = err.to_string();
    assert!(msg.contains("Did you mean `bybit`?"), "{msg}");
    assert!(msg.contains("lowercase"), "{msg}");
}

/// An unknown MODE is refused at DESERIALIZATION, naming the legal three — the `halt_admit`
/// idiom, free because the value type is an enum. `"true"`/`"1"`/`"mainnet"` are the plausible
/// wrong spellings; none of them may load as something else.
#[test]
fn an_unknown_mode_is_rejected_and_names_the_legal_ones() {
    for bad in ["mainnet", "testnet", "Live", "LIVE", "true", "1", ""] {
        let text = format!("[venues]\nbybit = {bad:?}\n");
        let err = toml::from_str::<PolicyPatch>(&text).expect_err("{bad} must not load");
        let msg = err.to_string();
        for mode in VenueMode::ALL {
            assert!(msg.contains(mode.as_str()), "{bad}: {mode} missing from {msg}");
        }
    }
}

/// An EMPTY `[venues]` table claims nothing, so it loads — the same rule as the empty `[rate]`
/// table, and the same reason: the refusal is keyed on a VALUE somebody wrote.
///
/// ⚠ …and it declares nothing either, which is the half the stage-3 migration warning rests on:
/// a section header with no venue under it has stated no arming intent, so silencing the warning
/// on it would leave a credentialled box on all-paper with nothing said.
#[test]
fn an_empty_venues_table_is_not_refused() {
    let patch: PolicyPatch = toml::from_str("[venues]\n").expect("parses");
    let mut p = Policy::default();
    p.apply(patch, file()).expect("`[venues]` with nothing in it claims nothing");
    assert_eq!(p.venues, Policy::default().venues);
    assert!(!p.venues.is_declared(), "a table naming no venue has declared no venue");
}

/// **The migration warning's self-silencing fact, driven through the REAL patch path.** A table
/// that sets every venue to the value they already had is a DECISION, and after this the warning
/// must never fire again on that box — while the ceiling it produced is the default's.
///
/// Driven through `apply` rather than by calling `VenuePolicy::set` directly, because the claim
/// is about what a FILE does: `apply` is the only production caller, and a test that pokes the
/// setter would stay green if the `[venues]` arm stopped reaching it.
#[test]
fn an_all_paper_table_declares_while_no_table_does_not() {
    let mut untouched = Policy::default();
    assert!(!untouched.venues.is_declared(), "the compiled-in default is nobody's decision");
    // A policy that sets OTHER ceilings and no `policy.venues.*` row is still undeclared —
    // the the CI box shape while its `settings/policy.toml` set only the notional cap.
    untouched
        .apply(PolicyPatch { max_notional_per_order: Some(250.0), ..Default::default() }, file())
        .unwrap();
    assert!(!untouched.venues.is_declared(), "another key is not a venue decision");

    let mut declared = Policy::default();
    let all_paper: String = std::iter::once("[venues]".to_string())
        .chain(VENUES.iter().map(|v| format!("{v} = \"paper\"")))
        .collect::<Vec<_>>()
        .join("\n");
    declared.apply(toml::from_str(&all_paper).expect("parses"), file()).unwrap();
    assert!(declared.venues.is_declared(), "an all-paper table IS a stated decision");
    assert!(
        declared.venues.iter().eq(untouched.venues.iter()),
        "…and it produced exactly the default CEILING, which is why the map alone cannot \
             answer the question this flag answers"
    );
}

/// A venue this BUILD cannot mount is still a legal ceiling — the file is portable across
/// boxes whose cargo features differ, and a ceiling for an unmountable venue caps at paper
/// anyway. See `crate::venue_mode`'s module doc for the full argument.
#[test]
fn a_feature_gated_venue_is_still_a_legal_ceiling() {
    for venue in ["ibkr", "polymarket", "fxcm"] {
        let text = format!("[venues]\n{venue} = \"demo\"\n");
        let patch: PolicyPatch = toml::from_str(&text).expect("parses");
        let mut p = Policy::default();
        p.apply(patch, file()).unwrap_or_else(|e| panic!("{venue} must be settable: {e}"));
        assert_eq!(p.venues.get(venue), VenueMode::Demo);
    }
}

#[test]
fn a_zero_money_ceiling_is_rejected_rather_than_silently_halting_trading() {
    let err = Policy::default()
        .apply(PolicyPatch { max_notional_per_order: Some(0.0), ..Default::default() }, file())
        .unwrap_err();
    assert!(err.to_string().contains("max_notional_per_order"), "{err}");
    assert!(err.to_string().contains("denies every order"), "{err}");
}
