//! **The `[risk]` mirror against the real type** —
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 2.
//!
//! # Why this test is in THIS crate now
//!
//! It needs three things in one hand: the mirror (`vike_config::risk_rows_from_profile` and
//! `vike_config::check_risk_key`), `vike_model::ProfileRisk` (the type that actually judges an
//! order), and `toml`. It used to live in `vike-tradehub`, the one crate that linked all three,
//! because `ProfileRisk` sat in `vike-exec`, above this crate; since
//! `docs/decisions/0114-the-risk-config-types-live-in-vike-model.md` the type is `vike-model`'s and
//! the test sits beside the mirror it checks. The textual twin that read `ProfileRisk`'s SOURCE to
//! hold a hand-written key roster equal to it is gone with the roster: the keys are
//! `ProfileRisk::keys()`, serde's own field list, and every check below goes through the real
//! parser.
//!
//! # ⚠ What it does NOT prove, stated so nobody reads more into a green
//!
//! That a mirrored row is ENFORCED. It is not: nothing on the mount path reads one
//! (`crates/vike-ops/tests/settings_secrets/profile_risk_readers_gate.rs` is that gate), so the
//! rows are a disclosure copy and this file is about whether the copy is FAITHFUL, never about
//! whether it binds. It also does not run `vike_core::RunProfile::validate` — the semantic layer
//! above the parse, which refuses a live profile that sets the venue-owned grid fields — because a
//! mirror is not a second gate in front of the daemon and does not claim to be.

use serde::Deserialize;
use vike_config::{
    RiskKeyKind, check_risk_key, missing_keys, risk_key_kind, risk_rows_from_profile, unknown_rows,
};
use vike_model::ProfileRisk;

#[path = "common/workspace.rs"]
mod workspace;
use workspace::workspace_root;

/// The shipped live-profile template: the file the tree tells an operator to copy.
const LIVE_PROFILE_TEMPLATE: &str = "docs/ops/run-profile-live.toml";

/// The shape a `[risk]` table sits in — the same one a run profile presents it in, so the parse
/// under test is the parse the daemon performs rather than a flattened approximation of it.
#[derive(Debug, Deserialize)]
struct RiskDoc {
    #[serde(default)]
    risk: ProfileRisk,
}

fn parse(doc: &str) -> Result<ProfileRisk, toml::de::Error> {
    toml::from_str::<RiskDoc>(doc).map(|d| d.risk)
}

/// Write `body` as a run profile and mirror it through the REAL renderer.
fn mirrored(body: &str) -> vike_secrets::StoredProfileRisk {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("run-live.toml");
    std::fs::write(&path, body).unwrap();
    risk_rows_from_profile(&path).expect("the profile must mirror")
}

/// Reassemble a `[risk]` document from mirrored rows — the operation a future reader would perform,
/// written here rather than shipped because **no reader exists**: the rows are a disclosure copy.
/// It is the inverse of `vike_config::risk_rows_from_profile`, and the round trip below is what
/// makes "inverse" a measurement rather than a claim.
fn document_from(stored: &vike_secrets::StoredProfileRisk) -> String {
    let mut out = String::from("[risk]\n");
    for r in &stored.rows {
        out.push_str(&format!("{} = {}\n", r.key, r.value));
    }
    out
}

/// A value of the shape a key has — the smallest legal one, so the test exercises the PARSE rather
/// than any bound.
fn sample(kind: RiskKeyKind) -> &'static str {
    match kind {
        RiskKeyKind::Float => "1.0",
        RiskKeyKind::Integer => "1",
        RiskKeyKind::Boolean => "true",
    }
}

/// **Every key, at the shape the mirror reports for it, parses as a `[risk]` document** and is
/// accepted by the mirror's own check. One document per key, so a failure names the key. The shape
/// comes from `vike_config::risk_key_kind`, which asks the parser, so this is the round trip of
/// that probe through a real TOML document rather than a second opinion.
#[test]
fn every_key_parses_at_the_shape_the_mirror_reports() {
    for key in ProfileRisk::keys() {
        let kind = risk_key_kind(key).unwrap_or_else(|| panic!("`{key}` has no shape"));
        let doc = format!("[risk]\n{key} = {}\n", sample(kind));
        parse(&doc).unwrap_or_else(|e| {
            panic!(
                "the mirror reports `{key}` as a {}, and the type refuses it: {e}",
                kind.as_str()
            )
        });
        let value: toml::Value = toml::from_str::<toml::Table>(&doc).unwrap()["risk"][*key].clone();
        assert_eq!(check_risk_key(key, &value), Ok(()), "{key}");
    }
}

/// **What the REAL parser does when a scalar's TOML type is not its field's Rust type** — pinned,
/// because the mirror's refusals are judged by these three answers and a first attempt at the old
/// roster assumed one of them and was wrong.
///
/// The old roster's shape check began as three exact matches, on the reasoning that TOML's `3` is
/// an integer and serde would refuse it for an `f64`. It does not. That made the mirror STRICTER
/// than the boot: `config mirror --profile` refused `max_leverage = 3`, a profile the daemon starts
/// on perfectly well. It was caught by a MUTATION PROOF on that key's declared kind. The mirror now
/// asks the parser itself (`vike_config::check_risk_key`), so it cannot drift from these answers,
/// and the last three asserts hold it to them.
///
/// ⚠ These are properties of `toml` + `serde`, not of this workspace: a version bump that changed
/// one changes the mirror with it, which is what pinning them here makes visible.
#[test]
fn the_real_parsers_cross_shape_answers_are_pinned() {
    assert!(
        parse("[risk]\nmax_leverage = 3\n").is_ok(),
        "an INTEGER for an `Option<f64>` field is accepted — the mirror must not refuse it, or it \
         rejects a profile the daemon starts on"
    );
    assert!(
        parse("[risk]\nmax_orders_per_window = 3.0\n").is_err(),
        "a FLOAT for an `Option<usize>` field is refused — the mirror must refuse it too, or it \
         writes a row the boot rejects"
    );
    assert!(
        parse("[risk]\nblock_reduce_only_overshoot = 1\n").is_err(),
        "an INTEGER for a `bool` field is refused — same rule"
    );
    assert_eq!(check_risk_key("max_leverage", &toml::Value::Integer(3)), Ok(()));
    assert!(check_risk_key("max_orders_per_window", &toml::Value::Float(3.0)).is_err());
    assert!(check_risk_key("block_reduce_only_overshoot", &toml::Value::Integer(1)).is_err());
}

/// The complement: a key outside the type is refused by the real type too, so the mirror's error
/// and the daemon's error are about the same thing.
#[test]
fn a_key_outside_the_type_is_refused_by_the_real_type() {
    assert!(
        parse("[risk]\nmax_levrage = 3.0\n").is_err(),
        "`ProfileRisk` is `deny_unknown_fields`; if this ever passes, the mirror's refusal is \
         stricter than the daemon's and it would reject a file that starts fine"
    );
    assert_eq!(
        check_risk_key("max_levrage", &toml::Value::Float(3.0)),
        Err(vike_config::RiskKeyRefusal::Unknown)
    );
}

/// **The round trip**: file -> rows -> document -> the real type, equal to parsing the file's own
/// `[risk]` table directly. This is what makes a mirrored value a faithful copy rather than a
/// plausible-looking one.
///
/// The renderings are the half worth stating: `toml::Value`'s `Display` is what the row carries, so
/// `5000.0` stays a float, `20` stays an integer and `true` stays a bool. A renderer that normalised
/// any of them would fail here rather than in production — and `max_orders_per_window` is the one
/// that would BITE, since a float is genuinely refused for that field
/// ([`the_real_parsers_cross_shape_answers_are_pinned`]).
///
/// `max_leverage` is deliberately written as a bare `1`, the INTEGER spelling an operator is free
/// to use for a float key: the row carries `1`, reassembles as `1`, and lands as `Some(1.0)` on
/// both sides. That is the direction the mirror must not refuse.
#[test]
fn a_profiles_risk_table_round_trips_through_rows_into_the_real_type() {
    let body = "name = \"live-operator-budget\"\nmode = \"live\"\n\n\
                [risk]\n\
                max_notional_per_order      = 5000.0\n\
                max_total_exposure          = 25000.0\n\
                max_orders_per_window       = 20\n\
                window_ms                   = 1000\n\
                max_leverage                = 1\n\
                required_free_bp_pct        = 0.05\n\
                block_reduce_only_overshoot = true\n";

    let from_file = parse(body).expect("the profile's own `[risk]` must parse");
    let stored = mirrored(body);
    let from_rows = parse(&document_from(&stored)).expect("the reassembled `[risk]` must parse");

    assert_eq!(
        from_file, from_rows,
        "a mirrored `[risk]` table must reassemble into the SAME `ProfileRisk` the file parses \
         into — otherwise `config show` prints a ceiling the daemon is not using"
    );
    // ...and the two ceilings a live mount refuses to start without really are in the rows, which
    // is the whole operator-facing payoff of the phase.
    assert_eq!(from_rows.max_notional_per_order, Some(5000.0));
    assert_eq!(from_rows.max_total_exposure, Some(25000.0));
    // ...and the integer-spelled float survived as a float on BOTH sides.
    assert_eq!(from_rows.max_leverage, Some(1.0));
    assert!(
        stored.rows.iter().any(|r| r.key == "max_leverage" && r.value == "1"),
        "the row must carry the operator's own spelling, not a normalised one: {:?}",
        stored.rows
    );
}

/// A profile that sets NO `[risk]` key round-trips to the type's own default — so "mirrored and
/// empty" is a faithful copy of "the file sets nothing", not a lost table.
#[test]
fn a_profile_with_no_risk_table_round_trips_to_the_types_default() {
    let stored = mirrored("name = \"x\"\nmode = \"paper\"\n");
    assert!(stored.rows.is_empty());
    assert_eq!(parse(&document_from(&stored)).unwrap(), ProfileRisk::default());
}

/// **The shipped operator template**: it mirrors, round-trips into the same `ProfileRisk` its own
/// `[risk]` parses into, sets no key outside the type, sets both keys a live mount refuses to start
/// without, and sets none of the venue-owned grid fields a live mount rejects.
///
/// Skipped — loudly — where `docs/` is absent, which is the public source mirror.
#[test]
fn the_shipped_live_profile_template_mirrors_and_round_trips() {
    let root = workspace_root();
    let Ok(body) = std::fs::read_to_string(root.join(LIVE_PROFILE_TEMPLATE)) else {
        assert!(
            !root.join("docs").exists(),
            "`{LIVE_PROFILE_TEMPLATE}` is missing while `docs/` is present — that is a MOVE, and \
             this test's path needs re-keying"
        );
        eprintln!("SKIPPED: this tree carries no `docs/` — the public source mirror withholds it.");
        return;
    };
    let stored = mirrored(&body);
    assert_eq!(stored.profile, "run-live.toml");
    assert!(unknown_rows(&stored).is_empty(), "the template may set no key outside the type");

    let from_file = parse(&body).expect("the shipped template's `[risk]` must parse");
    let from_rows = parse(&document_from(&stored)).expect("its mirror must parse");
    assert_eq!(from_file, from_rows);

    for key in ["max_notional_per_order", "max_total_exposure"] {
        assert!(
            stored.rows.iter().any(|r| r.key == key),
            "`{LIVE_PROFILE_TEMPLATE}` sets no `{key}`, and a live mount refuses to start without \
             it: {:?}",
            stored.rows
        );
    }
    let unset = missing_keys(&stored);
    assert!(
        unset.contains(&"tick_size"),
        "a LIVE template must not set the venue-owned grid fields; a live mount rejects them: \
         {unset:?}"
    );
}
