//! `alerts.json` load/save tests: disk round-trip and the never-brick load.

use super::*;
use crate::rule::{AlertRule, AlertRuleSet, Compare, RuleTrigger};
use crate::testkit::price_rule;

/// **A save replaces the file WHOLE and never writes into the old one** (`write_atomic`), so a save cut short (a
/// crash, a full disk) leaves the old rules as they were. Witnessed through a hard link to the old file: a write in
/// place, which truncates first, would change what the link reads; a whole replace leaves it alone. No temporary file
/// stays behind.
#[test]
fn a_save_replaces_the_file_whole_and_never_writes_into_the_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("alerts.json");
    let old = AlertRuleSet {
        rules: vec![AlertRule::new("old", RuleTrigger::OrderRejected)],
        ..Default::default()
    };
    save_path(&old, &p).unwrap();
    let before = std::fs::read(&p).unwrap();
    let link = dir.path().join("alerts.json.link");
    std::fs::hard_link(&p, &link).unwrap();

    let new =
        AlertRuleSet { rules: vec![price_rule("new", Compare::Above, 1.0)], ..Default::default() };
    save_path(&new, &p).unwrap();
    assert_eq!(load_path(&p), Ok(Some(new)), "the new rules are saved");
    assert_eq!(std::fs::read(&link).unwrap(), before, "the old file was never written into");
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["alerts.json", "alerts.json.link"], "no temporary file is left");
}

#[test]
fn disk_round_trip_and_missing_and_corrupt_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("alerts.json");

    // missing file → None (the OFF state), never an error.
    assert_eq!(load_path(&p), Ok(None));

    let set = AlertRuleSet {
        rules: vec![
            price_rule("p", Compare::Above, 100_000.0),
            AlertRule::new("r", RuleTrigger::OrderRejected),
        ],
        ..Default::default()
    };
    save_path(&set, &p).unwrap();
    let back = load_path(&p).unwrap().expect("saved file loads");
    assert_eq!(back, set, "disk round-trip is lossless");

    // corrupt file → refused with its hint (never bricks: the caller runs no rule).
    std::fs::write(&p, "{ not valid json ]").unwrap();
    let hint = load_path(&p).expect_err("a corrupt file is refused");
    assert!(hint.contains("could not be read"), "{hint}");
    assert!(hint.contains("no alert rule runs until it is fixed or moved away"), "{hint}");
}

/// THE OWNER'S RULING OF 2026-11-17: an `alerts.json` in the older words (`op`, `pct`,
/// `recon_alert`, version 1 or no version) is REFUSED with a one-line hint naming the file and
/// what to do; nothing in it is read, and the bytes stay as they were.
#[test]
fn an_old_alerts_file_is_refused_and_left_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("alerts.json");
    let rules = r#""rules": [
        { "id": "btc", "trigger": { "kind": "price", "venue": "binance", "symbol": "BTCUSDT",
          "op": "above", "level": 100000.0 } },
        { "id": "dd", "trigger": { "kind": "drawdown", "pct": 0.1 } },
        { "id": "ra", "trigger": { "kind": "recon_alert" } }
    ]"#;
    for (old, said) in [
        (format!(r#"{{ "version": 1, {rules} }}"#), "version 1"),
        (format!("{{ {rules} }}"), "no version"),
    ] {
        std::fs::write(&p, &old).unwrap();
        let hint = load_path(&p).expect_err("an old file must be refused");
        assert_eq!(
            hint,
            format!(
                "{} says {said}, so it is an alert rule file from another Vike Trader and is not \
                 read (this build reads version 2, whose words are `comparison`, `fraction` and \
                 `reconciliation_alert`): move it away and write the rules again. No alert rule \
                 runs until then.",
                p.display()
            )
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), old, "the file is left as it was");
    }
    // The same rules in the version-2 words read.
    let new = rules
        .replace(r#""op""#, r#""comparison""#)
        .replace(r#""pct""#, r#""fraction""#)
        .replace("recon_alert", "reconciliation_alert");
    std::fs::write(&p, format!(r#"{{ "version": 2, {new} }}"#)).unwrap();
    assert_eq!(load_path(&p).unwrap().expect("present").rules.len(), 3);
}
