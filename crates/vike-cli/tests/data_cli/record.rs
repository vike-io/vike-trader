//! `data realtime record`: the recorder-profile subscription rows this project persists.

use vike_model::AssetClass;

use super::*;

// ─── `data realtime record` — the subscription ROWS this box persists ────────────────────────────

/// One subscription row of the recorder profile every `record` case starts from.
struct Subscription {
    venue: &'static str,
    family: Option<&'static str>,
    symbols: Option<&'static str>,
}

/// The recorder profile every `record` case starts from: one FAMILY row and one SYMBOLS row, which
/// is the shape the CI box actually records.
const SUBSCRIPTIONS: [Subscription; 2] = [
    Subscription { venue: "polymarket", family: Some("btc-updown-5m"), symbols: None },
    Subscription { venue: "binance", family: None, symbols: Some("[\"BTCUSDT.P\"]") },
];

/// A settings directory with a MIGRATED store and nothing else in it.
///
/// ⚠ **`datahub_addr` is pinned at a port nothing serves, and that is load-bearing rather than
/// tidy.** `record add` dials the configured datahub to ask which venues it can RECORD, and the
/// default rung is `127.0.0.1:7878` — the address a developer box or a CI lane may genuinely have
/// a datahub on. Pinning it here makes every case take the same documented branch (unreachable →
/// WARN and write) instead of one that depends on what else is running on the box.
///
/// ⚠ The store is created by `vike-cli secrets init`, the one creator of a fresh box's
/// settings database, so the recorder fixture has a store to write into.
fn migrated_settings_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = run(dir.path(), &["secrets", "init"]);
    assert!(out.status.success(), "the fixture's migrate must succeed: {}", stderr(&out));
    // ⚠ `datahub_addr` used to be a `config.toml` write here, read straight off disk.
    // `docs/decisions/0086` deletes that layer outright — there are no settings files any more, so
    // this is a settings ROW instead, planted into the SAME database `secrets init` just
    // created (a whole-table REPLACE of the settings/arming tables alone).
    vike_secrets::plant_settings_rows(
        dir.path(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "datahub_addr".to_string(),
                value: "\"127.0.0.1:1\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("plant the fixture's datahub_addr row");
    dir
}

/// …and with one recorder profile in it, stored through the library writer (a test is outside
/// `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs`'s scope by that gate's own
/// statement, so this is not a second production writer). The body is the one
/// `config bootstrap-recorder` would store, with two subscriptions rather than its one.
fn recorder_settings_dir(name: &str, active: bool) -> tempfile::TempDir {
    use vike_secrets::profile_store as ps;
    let dir = migrated_settings_dir();
    let db = vike_secrets::db_path_in(dir.path());
    let write = ps::OperatorWrite::claim("data_cli.rs fixture");
    let body = ps::RecorderBody {
        row: ps::RecorderRow {
            store: "market_data/hist".to_string(),
            interval_secs: None,
            min_parts: None,
            target_mb: None,
            max_merge_rows: None,
            retention_days: None,
            alert_webhooks: None,
            alert_repeat_secs: None,
            alert_series_prefix: None,
            note: None,
        },
        subscriptions: SUBSCRIPTIONS
            .iter()
            .enumerate()
            .map(|(i, sub)| ps::SubscriptionRow {
                ord: i64::try_from(i).expect("two rows"),
                venue: sub.venue.to_string(),
                family: sub.family.map(str::to_string),
                symbols: sub.symbols.map(str::to_string),
                note: None,
            })
            .collect(),
    };
    let stored = ps::StoredProfile {
        row: ps::ProfileRow {
            name: name.to_string(),
            kind: ps::ProfileKind::Recorder,
            active: false,
            note: None,
        },
        mounts: Vec::new(),
        params: Default::default(),
        settings: Default::default(),
        recorder: Some(body),
    };
    ps::store_profile(&db, &stored, &write, 0, AssetClass::SQL_WORDS)
        .expect("the fixture's recorder body must store");
    if active {
        ps::set_active(&db, ps::ProfileKind::Recorder, name, &write, 0, AssetClass::SQL_WORDS)
            .expect("the fixture's active row must be settable");
    }
    dir
}

/// The group's help is the only place its verbs are named — it takes no default action — and a
/// `--help` that exited non-zero would break every `set -e` caller.
///
/// ⚠ The verb check reads the LABEL COLUMN rather than the page, for the reason
/// [`realtime_help_names_both_verbs_and_exits_zero`] states: every verb name also occurs in the
/// surrounding prose, so a `contains` would pass with a verb's whole block deleted.
#[test]
fn record_help_names_every_verb_and_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "realtime", "record", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for verb in ["ls", "add", "rm"] {
        assert!(
            text.lines().any(|l| l.starts_with(&format!("  {verb}"))),
            "`{verb}` has no block of its own on the page: {text}"
        );
    }
    assert!(text.contains("NEXT RESTART"), "the ruling an operator must carry: {text}");
    // ...and the sub-group is reachable from its PARENT's page, or nobody finds it.
    let parent = stdout(&run(scratch.path(), &["data", "realtime", "--help"]));
    assert!(parent.lines().any(|l| l.starts_with("  record")), "{parent}");
}

/// **The three noes of ruling 5 are three DIFFERENT answers**, and an UNMIGRATED box gets the one
/// that names the verb which creates a store — not the one that names the verb which creates a
/// profile.
#[test]
fn record_on_a_box_with_no_store_names_the_verb_that_creates_one() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "realtime", "record", "ls"]);
    assert_eq!(out.status.code(), Some(1), "a missing store is a run failure: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("secrets init"), "{err}");
    assert!(!err.contains("NONE is marked active"), "a DIFFERENT no: {err}");

    // A MIGRATED box with no recorder profile is the SECOND no, and it names the other verb.
    let dir = migrated_settings_dir();
    let out = run(dir.path(), &["data", "realtime", "record", "ls"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("config bootstrap-recorder"), "{err}");
    assert!(!err.contains("secrets init"), "the store EXISTS here: {err}");

    // A profile that is stored and NOT selected is the THIRD, and it names the flag that picks one.
    let dir = recorder_settings_dir("default", false);
    let out = run(dir.path(), &["data", "realtime", "record", "ls"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("--profile NAME"), "{err}");
    assert!(err.contains("Recorder profiles in this store: default."), "{err}");
    // ...and naming it is the answer, which is what makes the refusal above actionable.
    let out = run(dir.path(), &["data", "realtime", "record", "ls", "--profile", "default"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("family btc-updown-5m"), "{}", stdout(&out));
}

/// **Ruling 3: `--addr` is ACCEPTED and refused BY NAME on every verb**, so an operator who asks
/// for the remote route is told the route is unbuilt rather than that the flag does not exist.
#[test]
fn record_refuses_the_remote_route_by_name_on_every_verb() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for argv in [
        vec!["data", "realtime", "record", "ls", "--addr", "1.2.3.4:9"],
        vec!["data", "realtime", "record", "add", "binance:BTCUSDT", "--addr", "1.2.3.4:9"],
        vec!["data", "realtime", "record", "rm", "binance:BTCUSDT", "--addr=1.2.3.4:9"],
    ] {
        let out = run(scratch.path(), &argv);
        assert_eq!(out.status.code(), Some(2), "{argv:?} is a usage error: {}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("designed and not built"), "{argv:?}: {err}");
        assert!(!err.contains("unknown option"), "a real flag is not an unknown one: {err}");
    }
    // ...and `--lane`, the word this group's SIBLING verb owns, is refused with the reserved one.
    let out = run(
        scratch.path(),
        &["data", "realtime", "record", "add", "binance:BTCUSDT", "--lane", "trades"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--stream"), "{}", stderr(&out));
    // ...and the retired per-subscription `--backfill` is refused loudly, never accepted and dropped.
    let out = run(
        scratch.path(),
        &["data", "realtime", "record", "add", "binance:BTCUSDT", "--backfill", "off"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--backfill"), "{}", stderr(&out));
}

/// **THE END-TO-END: `add` → `ls` → `rm`, through the SHIPPED binary and a real SQLite store.**
///
/// What no unit test can see: that the row genuinely lands in `<settings>/db/vike.db`, that
/// `vike-cli config recorder` — the verb an operator greps with — reads back what this verb wrote,
/// and that a `--dry-run` really writes nothing.
#[test]
fn record_add_and_rm_round_trip_through_the_real_store() {
    let dir = recorder_settings_dir("default", true);

    // A DRY RUN first: it must print the plan and change nothing.
    let out =
        run(dir.path(), &["data", "realtime", "record", "add", "binance:ETHUSDT.P", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("NOTHING was written"), "{text}");
    assert!(text.contains("+ ord 2"), "the plan names the row it would write: {text}");
    let after_dry = stdout(&run(dir.path(), &["data", "realtime", "record", "ls"]));
    assert!(!after_dry.contains("ETHUSDT.P"), "a dry run must write nothing: {after_dry}");

    // ...then the real one.
    let out = run(
        dir.path(),
        &[
            "data",
            "realtime",
            "record",
            "add",
            "binance:ETHUSDT.P",
            "--note",
            "added by the round-trip case",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("NEXT RESTART"), "ruling 2 rides every write: {text}");
    assert!(text.contains("--record <path>"), "ruling 6 rides it too: {text}");
    // The venue probe could not reach a datahub, so the row was written UNCHECKED and says so.
    assert!(text.contains("UNCHECKED"), "the degrade is DISCLOSED, never silent: {text}");

    // It is in the store, and the verb an operator greps with reads it back.
    let listed = stdout(&run(dir.path(), &["config", "recorder"]));
    assert!(listed.contains("binance symbols [\"ETHUSDT.P\"]"), "{listed}");
    assert!(listed.contains("added by the round-trip case"), "the NOTE survived: {listed}");
    // …and so did the rows the fixture wrote, which is the whole point of the row-based write path.
    assert!(listed.contains("polymarket family btc-updown-5m"), "{listed}");

    // The JSON form carries `symbols` as an ARRAY rather than the stored TOML text.
    let doc: serde_json::Value = serde_json::from_str(&stdout(&run(
        dir.path(),
        &["data", "realtime", "record", "ls", "--json"],
    )))
    .expect("one document");
    assert_eq!(doc["profile"], "default");
    assert_eq!(doc["active"], true);
    let rows = doc["subscriptions"].as_array().expect("rows").clone();
    assert_eq!(rows.len(), 3, "{doc}");
    assert_eq!(rows[2]["symbols"][0], "ETHUSDT.P");
    assert!(rows[2].get("backfill").is_none(), "the retired policy column is not printed: {doc}");
    assert!(rows[0]["symbols"].is_null(), "a family row has no symbols: {doc}");

    // A DUPLICATE is refused by name rather than written twice.
    let out = run(dir.path(), &["data", "realtime", "record", "add", "binance:ETHUSDT.P"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("already a subscription"), "{}", stderr(&out));

    // ...and `rm` takes it out again.
    let out = run(dir.path(), &["data", "realtime", "record", "rm", "binance:ETHUSDT.P"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("- ord 2"), "{}", stdout(&out));
    let after = stdout(&run(dir.path(), &["data", "realtime", "record", "ls"]));
    assert!(!after.contains("ETHUSDT.P"), "{after}");
    assert!(after.contains("family btc-updown-5m"), "the other rows are untouched: {after}");

    // A SECOND `rm` of the same spec is a refusal rather than a silent success — a removal that
    // reported success while removing nothing is the failure ruling 5 exists to prevent.
    let out = run(dir.path(), &["data", "realtime", "record", "rm", "binance:ETHUSDT.P"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("no subscription"), "{}", stderr(&out));
}

/// **`@` IS A FAMILY ON THIS VERB AND AN ORDINARY SYMBOL ON ITS SIBLING**, end to end through the
/// binary — the one character this group reads two ways, which is why the two parsers may not be
/// shared.
#[test]
fn the_at_marker_is_a_family_on_record_and_an_ordinary_symbol_on_watch() {
    let dir = recorder_settings_dir("default", true);
    let out = run(dir.path(), &["data", "realtime", "record", "add", "hyperliquid:@PURR"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let listed = stdout(&run(dir.path(), &["config", "recorder"]));
    assert!(listed.contains("hyperliquid family PURR"), "it is a FAMILY here: {listed}");

    // The sibling verb reads the same token as a SYMBOL and hands it to the wire verbatim, so it
    // gets as far as the DIAL — a connect failure here, never a usage error.
    let out = run(
        dir.path(),
        &["data", "realtime", "watch", "hyperliquid:@PURR", "--lane", "trades", "--events", "1"],
    );
    assert_ne!(out.status.code(), Some(2), "`@` is not a usage error on watch: {}", stderr(&out));
}
