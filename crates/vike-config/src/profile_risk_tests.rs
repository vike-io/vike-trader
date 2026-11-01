use super::*;

use serde::Deserialize;

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

/// A value of the shape a key has — the smallest legal one, so the test exercises the PARSE rather
/// than any bound.
fn sample(kind: RiskKeyKind) -> &'static str {
    match kind {
        RiskKeyKind::Float => "1.0",
        RiskKeyKind::Integer => "1",
        RiskKeyKind::Boolean => "true",
    }
}

/// **Every key, at the shape this module reports for it, parses as a `[risk]` document** and is
/// accepted by [`check_risk_key`]. One document per key, so a failure names the key. The shape comes
/// from [`risk_key_kind`], which asks the parser, so this is the round trip of that probe through a
/// real TOML document rather than a second opinion.
#[test]
fn every_key_parses_at_the_shape_the_probe_reports() {
    for key in ProfileRisk::keys() {
        let kind = risk_key_kind(key).unwrap_or_else(|| panic!("`{key}` has no shape"));
        let doc = format!("[risk]\n{key} = {}\n", sample(kind));
        parse(&doc).unwrap_or_else(|e| {
            panic!("`{key}` is reported as a {}, and the type refuses it: {e}", kind.as_str())
        });
        let value: toml::Value = toml::from_str::<toml::Table>(&doc).unwrap()["risk"][*key].clone();
        assert_eq!(check_risk_key(key, &value), Ok(()), "{key}");
    }
}

/// **What the REAL parser does when a scalar's TOML type is not its field's Rust type** — pinned,
/// because [`check_risk_key`]'s answers are judged by these three and a first attempt at the old
/// roster assumed one of them and was wrong.
///
/// TOML's `3` is an integer and serde does NOT refuse it for an `f64`, so a check stricter than the
/// boot would reject `max_leverage = 3`, a profile the daemon starts on perfectly well. The check
/// asks the parser itself, so it cannot drift from these answers, and the last three asserts hold
/// it to them.
///
/// ⚠ These are properties of `toml` + `serde`, not of this workspace: a version bump that changed
/// one changes the check with it, which is what pinning them here makes visible.
#[test]
fn the_real_parsers_cross_shape_answers_are_pinned() {
    assert!(
        parse("[risk]\nmax_leverage = 3\n").is_ok(),
        "an INTEGER for an `Option<f64>` field is accepted"
    );
    assert!(
        parse("[risk]\nmax_orders_per_window = 3.0\n").is_err(),
        "a FLOAT for an `Option<usize>` field is refused"
    );
    assert!(
        parse("[risk]\nblock_reduce_only_overshoot = 1\n").is_err(),
        "an INTEGER for a `bool` field is refused"
    );
    assert_eq!(check_risk_key("max_leverage", &toml::Value::Integer(3)), Ok(()));
    assert!(check_risk_key("max_orders_per_window", &toml::Value::Float(3.0)).is_err());
    assert!(check_risk_key("block_reduce_only_overshoot", &toml::Value::Integer(1)).is_err());
}

/// The complement: a key outside the type is refused by the real type too, so [`check_risk_key`]'s
/// error and the daemon's error are about the same thing.
#[test]
fn a_key_outside_the_type_is_refused_by_the_real_type() {
    assert!(
        parse("[risk]\nmax_levrage = 3.0\n").is_err(),
        "`ProfileRisk` is `deny_unknown_fields`; if this ever passes, the refusal here is stricter \
         than the daemon's"
    );
    assert_eq!(
        check_risk_key("max_levrage", &toml::Value::Float(3.0)),
        Err(RiskKeyRefusal::Unknown)
    );
}

/// With no row stored, every key is unset — the answer `config show` prints for an empty profile.
#[test]
fn an_empty_profile_has_every_key_unset() {
    let stored = StoredProfileRisk { profile: "x".into(), rows: Vec::new() };
    assert_eq!(missing_keys(&stored), ProfileRisk::keys());
}

/// The store-side defect the roster's read half exists for: a row nothing in this tree can
/// write, named rather than rendered as a ceiling.
#[test]
fn a_row_whose_key_is_not_in_the_roster_is_reported_as_unknown() {
    let stored = StoredProfileRisk {
        profile: "run-live.toml".into(),
        rows: vec![
            ProfileRiskRow { key: "max_leverage".into(), value: "3.0".into() },
            ProfileRiskRow { key: "max_levrage".into(), value: "9.0".into() },
        ],
    };
    let unknown = unknown_rows(&stored);
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].key, "max_levrage");
}

/// The JOIN onto the ceilings table, rather than a second copy of its flag.
#[test]
fn the_two_mount_refusing_keys_join_onto_their_ceiling_rows() {
    for name in ["max_notional_per_order", "max_total_exposure"] {
        let c = ceiling_for(name).unwrap_or_else(|| panic!("{name} has no run-profile ceiling"));
        assert!(c.refuses_live_mount_when_absent, "{name}");
    }
    assert!(ceiling_for("tick_size").is_none(), "a venue-owned grid field is not a ceiling row");
}

/// **The `BOUNDS` prose is held to the TYPE, not to a second list**: one row per key of
/// `vike_model::ProfileRisk::keys()` (serde's own field list), in the struct's order, each one
/// legible. A new `[risk]` field reddens here until it has a sentence; a removed one reddens until
/// its sentence goes. The column is `vike-cli config show`'s, and the owner kept it (decision 0114).
#[test]
fn the_bounds_table_is_the_types_own_key_list_in_order() {
    let names: Vec<&str> = RISK_KEY_BOUNDS.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, vike_model::ProfileRisk::keys(), "RISK_KEY_BOUNDS must follow the type");
    for (name, what) in RISK_KEY_BOUNDS {
        assert!(what.len() > 20, "`{name}`'s bounds sentence is too short to say what it bounds");
        assert_eq!(risk_key_bounds(name), Some(*what));
    }
}

/// **Every key's scalar shape is worked out by the REAL parser**, and a shape it cannot classify is
/// a STOP, not a default: a future `[risk]` field of a new kind (a string, a table) needs a
/// `RiskKeyKind` and a decision about what a row may hold. The four pins below are the answers the
/// refusals are written against.
#[test]
fn every_key_classifies_through_the_real_parser() {
    for key in vike_model::ProfileRisk::keys() {
        assert!(
            risk_key_kind(key).is_some(),
            "`ProfileRisk::{key}` has a shape no probe classifies — extend `RiskKeyKind` and \
             `risk_key_kind` deliberately, never default it"
        );
    }
    assert_eq!(risk_key_kind("max_leverage"), Some(RiskKeyKind::Float));
    assert_eq!(risk_key_kind("max_orders_per_window"), Some(RiskKeyKind::Integer));
    assert_eq!(risk_key_kind("window_ms"), Some(RiskKeyKind::Integer));
    assert_eq!(risk_key_kind("block_reduce_only_overshoot"), Some(RiskKeyKind::Boolean));
}

/// A name outside the type has neither a shape nor a sentence, so `config show` renders it as the
/// unknown row it is rather than as a ceiling.
#[test]
fn a_name_outside_the_type_has_no_kind_and_no_bounds() {
    assert_eq!(risk_key_kind("max_levrage"), None);
    assert_eq!(risk_key_bounds("max_levrage"), None);
}

/// The JOIN onto `PRE_TRADE_CEILINGS`, typed: every run-profile ceiling row must be a key of the
/// real type, or `config show` would name a ceiling no row can ever carry a value for.
#[test]
fn every_run_profile_ceiling_is_a_key_of_the_type() {
    for c in crate::ceilings::PRE_TRADE_CEILINGS {
        if c.home != crate::ceilings::CeilingHome::RunProfileRisk {
            continue;
        }
        assert!(
            vike_model::ProfileRisk::keys().contains(&c.name),
            "`PRE_TRADE_CEILINGS` carries a run-profile ceiling `{}` that is no `ProfileRisk` key",
            c.name
        );
    }
}
