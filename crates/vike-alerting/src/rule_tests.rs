//! Tests of the persisted rule model: the OFF default, JSON round-trips, `kind` tags, old files.

use super::*;

#[test]
fn empty_ruleset_is_the_default_off_state() {
    let s = AlertRuleSet::default();
    assert!(s.is_empty());
    assert_eq!(s.version, CURRENT_VERSION);
    // and it serializes to a tiny, rule-free object.
    let json = serde_json::to_string(&s).unwrap();
    let back: AlertRuleSet = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s);
}

#[test]
fn rule_round_trips_through_json_losslessly() {
    let set = AlertRuleSet {
        version: CURRENT_VERSION,
        rules: vec![
            AlertRule {
                id: "r1".into(),
                name: "BTC breakout".into(),
                enabled: true,
                trigger: RuleTrigger::Price {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    op: Compare::Above,
                    level: 100_000.0,
                },
                targets: AlertTargets { in_process: true, webhooks: vec!["tg".into()] },
                cooldown_ms: 60_000,
                once: false,
            },
            AlertRule::new("r2", RuleTrigger::OrderRejected),
            AlertRule::new(
                "r3",
                RuleTrigger::Indicator {
                    venue: "binance".into(),
                    symbol: "ETHUSDT".into(),
                    indicator: "rsi".into(),
                    output: 0,
                    params: vec![14.0],
                    op: Compare::Below,
                    threshold: 30.0,
                },
            ),
        ],
    };
    let json = serde_json::to_string_pretty(&set).unwrap();
    let back: AlertRuleSet = serde_json::from_str(&json).unwrap();
    assert_eq!(back, set, "full round-trip is lossless");
}

#[test]
fn trigger_kind_is_the_internally_tagged_discriminant() {
    // the wire tag is snake_case `kind` — the shape the (future) UI + forward-compat rely on.
    let t = RuleTrigger::Drawdown { pct: 0.1 };
    let v: serde_json::Value = serde_json::to_value(&t).unwrap();
    assert_eq!(v["kind"], "drawdown");
    assert_eq!(v["pct"], 0.1);

    let f = RuleTrigger::Feed { venue: None, state: FeedState::Degraded };
    let v: serde_json::Value = serde_json::to_value(&f).unwrap();
    assert_eq!(v["kind"], "feed");
    assert_eq!(v["state"], "degraded");
    // a `None` venue writes no `venue` key (skip_serializing_if), keeping scoped/unscoped
    // rules byte-distinct on disk.
    assert!(v.get("venue").is_none());
}

/// The FOURTH recorder trigger round-trips, and its wire tag is distinct from the two series
/// rules it sits beside.
///
/// ⚠ The distinctness is the point, not the serde plumbing. `series_stale`, `series_slow` and
/// `family_collapse` are three different faults with three different fixes, and an operator
/// correlating a persisted rule on its `kind` must be able to tell them apart — the same
/// argument `vike_recorder::alerts`' four rule ids make on their side of the boundary.
#[test]
fn the_family_collapse_trigger_round_trips_and_tags_itself_distinctly() {
    let scoped =
        RuleTrigger::FamilyCollapse { series_prefix: Some("book/polymarket/".to_string()) };
    let v: serde_json::Value = serde_json::to_value(&scoped).unwrap();
    assert_eq!(v["kind"], "family_collapse");
    assert_eq!(v["series_prefix"], "book/polymarket/");
    let back: RuleTrigger = serde_json::from_value(v).unwrap();
    assert_eq!(back, scoped, "round-trip is lossless");

    // Unscoped writes NO `series_prefix` key, exactly as its two siblings do — so a scoped and
    // an unscoped rule stay byte-distinct on disk.
    let un = RuleTrigger::FamilyCollapse { series_prefix: None };
    let v: serde_json::Value = serde_json::to_value(&un).unwrap();
    assert!(v.get("series_prefix").is_none(), "{v}");

    // …and it is NOT either of the two triggers it could be confused with.
    for other in [
        RuleTrigger::SeriesStale { series_prefix: None },
        RuleTrigger::SeriesSlow { series_prefix: None },
    ] {
        let ov: serde_json::Value = serde_json::to_value(&other).unwrap();
        assert_ne!(ov["kind"], v["kind"], "three faults must not share one wire tag");
    }
}

/// A rule file written by an OLDER build — one that predates this trigger — still loads, and a
/// `family_collapse` rule written by a newer one parses with its forward-compat defaults.
#[test]
fn a_family_collapse_rule_loads_from_a_minimal_file() {
    let raw = r#"{
            "rules": [
                { "id": "fam", "trigger": { "kind": "family_collapse" } }
            ]
        }"#;
    let set: AlertRuleSet = serde_json::from_str(raw).unwrap();
    assert_eq!(set.rules.len(), 1);
    assert_eq!(set.rules[0].trigger, RuleTrigger::FamilyCollapse { series_prefix: None });
    assert!(set.rules[0].enabled, "absent `enabled` defaults true");
}

#[test]
fn old_file_without_defaulted_keys_still_loads() {
    // A minimal rule JSON with `enabled`/`targets`/`cooldown_ms`/`once`/`name` all ABSENT must
    // load with their forward-compat defaults (enabled + in-process), never fail.
    let raw = r#"{
            "rules": [
                { "id": "r1", "trigger": { "kind": "fill" } }
            ]
        }"#;
    let set: AlertRuleSet = serde_json::from_str(raw).unwrap();
    assert_eq!(set.version, CURRENT_VERSION, "absent version defaults to current");
    assert_eq!(set.rules.len(), 1);
    let r = &set.rules[0];
    assert!(r.enabled, "absent `enabled` defaults true");
    assert!(r.name.is_empty());
    assert!(r.targets.in_process, "absent `targets` defaults to in-process on");
    assert!(r.targets.webhooks.is_empty());
    assert_eq!(r.cooldown_ms, 0);
    assert!(!r.once);
    assert_eq!(r.trigger, RuleTrigger::Fill { venue: None, symbol: None });
}
