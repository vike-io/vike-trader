//! **The BEHAVIOURAL half of the `[risk]` roster gate** —
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 2.
//!
//! # Why this test is in THIS crate
//!
//! It needs three things in one hand: `vike_config::PROFILE_RISK_KEYS` (the roster the mirror
//! validates against), `vike_exec::ProfileRisk` (the type that actually judges an order), and
//! `toml`. `vike-config` sits BELOW `vike-exec` and may not link it; `vike-cli` links neither
//! `vike-core` nor `vike-exec` on purpose and the `light-consumers` CI lane holds it there. This
//! daemon links all three already — and it is also the binary whose `--profile` /
//! `VIKE_RUN_PROFILE` the mirrored file belongs to, so the test sits with the consumer rather than
//! beside the roster.
//!
//! `crates/vike-config/tests/profile_risk.rs` is the TEXTUAL twin: it harvests `ProfileRisk`'s
//! fields out of that crate's source and holds the roster equal to them. This file proves the thing
//! a text gate cannot — that a row round-trips into the type, through the real parser, for every
//! key.
//!
//! # ⚠ What it does NOT prove, stated so nobody reads more into a green
//!
//! That a mirrored row is ENFORCED. It is not: nothing on the mount path reads one
//! (`crates/vike-ops/tests/profile_risk_readers_gate.rs` is that gate), so the rows are a
//! disclosure copy and this file is about whether the copy is FAITHFUL, never about whether it
//! binds. It also does not run `vike_core::RunProfile::validate` — the semantic layer above the
//! parse, which refuses a live profile that sets the venue-owned grid fields — because a mirror is
//! not a second gate in front of the daemon and does not claim to be.

use serde::Deserialize;
use vike_config::{PROFILE_RISK_KEYS, RiskKeyKind, risk_rows_from_profile};
use vike_exec::ProfileRisk;

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

/// A value of the shape a roster row declares — the smallest legal one, so the test exercises the
/// PARSE rather than any bound.
fn sample(kind: RiskKeyKind) -> &'static str {
    match kind {
        RiskKeyKind::Float => "1.0",
        RiskKeyKind::Integer => "1",
        RiskKeyKind::Boolean => "true",
    }
}

/// **THE gate this file exists for: every roster key parses as the field it claims to be.**
///
/// One document per key, so a failure names the key rather than the whole table. A roster row whose
/// NAME is wrong fails here on `deny_unknown_fields` — the failure the mirror would otherwise
/// produce silently, writing a row the daemon's own parser then refuses in the file it came from.
///
/// ⚠ **It does NOT catch every wrong SHAPE, and that claim was made here before it was measured.**
/// A `Float` key mis-declared `Integer` still parses, because an integer IS accepted for an `f64`
/// (see [`the_real_parsers_cross_shape_answers_are_pinned`]) — so this sample would be `1` and the
/// type would take it. The gate on that direction is TEXTUAL and lives in
/// `crates/vike-config/tests/profile_risk.rs`'s
/// `every_roster_row_declares_the_shape_its_field_actually_has`, which compares the declared kind
/// to the field's Rust type in `vike_exec`'s own source. Both were red under the mutation that
/// found this; only the textual one was red for the right reason.
#[test]
fn every_roster_key_parses_as_the_field_it_claims_to_be() {
    for k in PROFILE_RISK_KEYS {
        let doc = format!("[risk]\n{} = {}\n", k.name, sample(k.kind));
        parse(&doc).unwrap_or_else(|e| {
            panic!(
                "`PROFILE_RISK_KEYS` carries `{}` as a {}, and `vike_exec::ProfileRisk` refuses \
                 it: {e}\nThe roster and the type have diverged — fix the row in \
                 `crates/vike-config/src/profile_risk.rs`, never this test.",
                k.name,
                k.kind.as_str()
            )
        });
    }
}

/// **What the REAL parser does when a scalar's TOML type is not its field's Rust type** — pinned,
/// because the roster's shape check is written against these three answers and the first attempt at
/// it assumed one of them and was wrong.
///
/// `vike_config::ProfileRiskKey::accepts` began as three exact matches, on the reasoning that
/// TOML's `3` is an integer and serde would refuse it for an `f64`. It does not. That made the
/// mirror STRICTER than the boot: `config mirror --profile` refused `max_leverage = 3`, a profile
/// the daemon starts on perfectly well, and told the operator their working file was invalid. It
/// was caught by a MUTATION PROOF on that key's declared kind, not by reading it.
///
/// The asymmetry is genuinely one-directional, which is why the rule cannot be "any number": a
/// float for an integer field and an integer for a bool are both refused, so the mirror must still
/// refuse those or it writes a row the boot rejects in the file it came from.
///
/// ⚠ These are properties of `toml` + `serde`, not of this workspace. A version bump that changed
/// one would have to move `accepts` with it, which is exactly why the answers are pinned HERE —
/// against the real type, in the crate that links it — rather than restated in a comment beside the
/// check.
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
}

/// The complement, and the one that makes the roster a REFUSAL rather than a list: a key outside it
/// is refused by the real type too, so the mirror's error and the daemon's error are about the same
/// thing.
#[test]
fn a_key_outside_the_roster_is_refused_by_the_real_type() {
    assert!(
        parse("[risk]\nmax_levrage = 3.0\n").is_err(),
        "`ProfileRisk` is `deny_unknown_fields`; if this ever passes, the roster's refusal is \
         stricter than the daemon's and the mirror would reject a file that starts fine"
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
/// both sides. That is the direction the roster must not refuse.
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

/// **The shipped operator template**, through the same round trip. `docs/ops/run-profile-live.toml`
/// is the file the tree tells an operator to copy, so it is the one whose mirror must be faithful
/// before anybody's is.
///
/// Skipped — loudly — where `docs/` is absent, which is the public source mirror.
#[test]
fn the_shipped_live_profile_template_round_trips() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let template = root.join("docs/ops/run-profile-live.toml");
    let Ok(body) = std::fs::read_to_string(&template) else {
        assert!(
            !root.join("docs").exists(),
            "`docs/ops/run-profile-live.toml` is missing while `docs/` is present — that is a \
             MOVE, and this test's path needs re-keying"
        );
        eprintln!("SKIPPED: this tree carries no `docs/` — the public source mirror withholds it.");
        return;
    };
    let from_file = parse(&body).expect("the shipped template's `[risk]` must parse");
    let from_rows = parse(&document_from(&mirrored(&body))).expect("its mirror must parse");
    assert_eq!(from_file, from_rows);
}
