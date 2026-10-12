//! `data realtime record`: the recorder-profile subscription rows this project persists.

use vike_model::AssetClass;

use super::*;

// ─── `data realtime record` — the subscription ROWS this box persists ────────────────────────────

/// The recorder profile every `record` case starts from: one FAMILY row and one SYMBOLS row, which
/// is the shape the CI box actually records.
///
/// ⚠ It has to ROUND-TRIP through `vike_secrets::profile_store::render_recorder_toml`, because
/// `vike-cli config mirror --recorder` refuses to store a body that does not reproduce the file it
/// came from — so an extra key here fails the FIXTURE rather than the case.
const RECORDER_PROFILE: &str = concat!(
    "store = \"market_data/hist\"\n\n",
    "[[subscribe]]\nvenue = \"polymarket\"\nfamily = \"btc-updown-5m\"\nbackfill = \"off\"\n\n",
    "[[subscribe]]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT.P\"]\n",
);

/// A single-mount daemon profile that round-trips through `render_daemon_toml` unchanged — the
/// OTHER profile document `config mirror` stores (the run profile is written by
/// `config bootstrap-run` since decision 0111, and no file is imported for it).
const DAEMON_PROFILE: &str =
    "venue = \"bybit\"\nasset_class = \"CryptoPerp\"\nsymbol = \"BTCUSDT\"\n";

/// A settings directory with a MIGRATED store and nothing else in it.
///
/// ⚠ **`datahub_addr` is pinned at a port nothing serves, and that is load-bearing rather than
/// tidy.** `record add` dials the configured datahub to ask which venues it can RECORD, and the
/// default rung is `127.0.0.1:7878` — the address a developer box or a CI lane may genuinely have
/// a datahub on. Pinning it here makes every case take the same documented branch (unreachable →
/// WARN and write) instead of one that depends on what else is running on the box.
///
/// ⚠ The store is created by `vike-cli secrets init`, the one creator of a fresh box's
/// settings database, so `config mirror` has a store to write into.
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

/// …and with one recorder profile in it, mirrored through the SHIPPED verb, so the fixture is a
/// state an operator can actually produce.
fn recorder_settings_dir(name: &str, active: bool) -> tempfile::TempDir {
    let dir = migrated_settings_dir();
    let profile = dir.path().join("rec.toml");
    std::fs::write(&profile, RECORDER_PROFILE).expect("write the profile");
    let out = run(
        dir.path(),
        &[
            "config",
            "mirror",
            "--recorder",
            profile.to_str().expect("utf-8 temp path"),
            "--recorder-name",
            name,
        ],
    );
    assert!(out.status.success(), "the fixture's mirror must succeed: {}", stderr(&out));
    if active {
        // ⚠ Through the LIBRARY, because no shipped verb selects a recorder profile yet — the
        // design's ruling 5 names that verb as work of its own, and `config mirror` deliberately
        // withholds the active row. A test is outside
        // `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs`'s scope by that gate's own statement, so
        // this is not a second production writer.
        vike_secrets::profile_store::set_active(
            &vike_secrets::db_path_in(dir.path()),
            vike_secrets::profile_store::ProfileKind::Recorder,
            name,
            &vike_secrets::profile_store::OperatorWrite::claim("data_cli.rs fixture"),
            0,
            AssetClass::SQL_WORDS,
        )
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
    assert!(err.contains("config mirror --recorder"), "{err}");
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

/// **⚠ THE CROSS-KIND KILL PROOF, END TO END, on the CI box's own shape.**
///
/// `profile.name` is `TEXT PRIMARY KEY` — ONE namespace across all three kinds — and
/// `vike_secrets::profile_store::store_profile` replaces a body with `DELETE` + `INSERT` while
/// PRESERVING the `active` bit it finds. the CI box's store holds an ACTIVE `recorder` profile called
/// `default` (`deploy/vike-datahub.service` runs `--recorder-profile default`), so
/// `config mirror --daemon <tradehub.toml> --daemon-name default` is ONE flag away from
/// deleting that body and its `subscription` rows and re-inserting the name as `kind = 'daemon'`
/// with `active = 1` inherited — selecting a mount set with no `config activate --proves` in front
/// of it, while the report printed *"No active daemon row was written … this box resolves its
/// daemon profile exactly as it does today"*. (It was measured with the run plane's
/// `--profile <run.toml> --profile-name default`, which decision 0111 removed with the run plane.)
///
/// It lives in THIS file rather than beside the other `config` cases because
/// [`recorder_settings_dir`] is the fixture that produces exactly that shape through the shipped
/// verbs, and a second copy of it next door is a fixture that can drift from this one.
///
/// ⚠ `--dry-run` is asserted FIRST and deliberately: `config mirror` plans every half before it
/// writes any of them, so a rehearsal that answered *would mirror* here would be promising a write
/// the store was going to refuse — the finding's own "positive confirmation of something false",
/// one layer up.
#[test]
fn a_mirror_onto_a_name_another_kind_holds_is_refused_and_destroys_nothing() {
    let dir = recorder_settings_dir("default", true);
    let daemon_profile = dir.path().join("tradehub.toml");
    std::fs::write(&daemon_profile, DAEMON_PROFILE).expect("write the daemon profile");
    let argv = |extra: &'static str| -> Vec<String> {
        let mut v: Vec<String> =
            ["config", "mirror", "--daemon"].iter().map(|s| (*s).to_string()).collect();
        v.push(daemon_profile.to_str().expect("utf-8 temp path").to_string());
        v.push("--daemon-name".to_string());
        v.push("default".to_string());
        if !extra.is_empty() {
            v.push(extra.to_string());
        }
        v
    };

    for extra in ["--dry-run", ""] {
        let owned = argv(extra);
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let out = run(dir.path(), &args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "the cross-kind mirror must FAIL (extra: {extra:?}): {}{}",
            stdout(&out),
            stderr(&out)
        );
        let err = stderr(&out);
        for needle in ["recorder", "default", "NOTHING WAS WRITTEN"] {
            assert!(err.contains(needle), "the refusal must name {needle:?}: {err}");
        }
        assert!(
            !stdout(&out).contains("would mirror"),
            "a rehearsal may not promise a write the store refuses: {}",
            stdout(&out)
        );
    }

    // NOTHING was destroyed: the recorder body, its subscriptions and its `active` bit are all
    // still there, read back through the shipped verb that reads them.
    let out = run(dir.path(), &["data", "realtime", "record", "ls"]);
    assert!(out.status.success(), "the recorder profile must still resolve: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("family btc-updown-5m"), "the subscriptions survived: {text}");
    assert!(text.contains("polymarket") && text.contains("binance"), "both of them: {text}");
    assert!(
        text.contains("profile: default (active)"),
        "…and the `active` bit is still the RECORDER's: {text}"
    );

    // …and the DAEMON plane is still empty: nothing was selected by a verb that only stores bodies.
    let profiles =
        vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir.path()))
            .expect("the store must still read");
    assert_eq!(
        profiles.active(vike_secrets::profile_store::ProfileKind::Daemon),
        None,
        "THE ONE THAT MATTERS: no `daemon` row was selected by a verb that only stores bodies"
    );
    assert_eq!(
        profiles.all().len(),
        1,
        "and no second body landed beside the recorder: {:?}",
        profiles.all().iter().map(|p| (&p.row.name, p.row.kind)).collect::<Vec<_>>()
    );
}

/// **⚠ THE SAME DESTRUCTION REACHED FROM ONE COMMAND LINE — END TO END, over the shipped binary.**
///
/// The sibling above plants the colliding name in the store FIRST, so each half's plan-time
/// pre-check can see it. This one plants NOTHING: both names arrive from the same invocation, the
/// store holds neither when the run starts, and `--recorder` supplies `default` from its own
/// `DEFAULT_PROFILE_NAME` with nothing typed twice. Measured at `bab92f0ee`, before the fix:
///
/// ```text
/// PROBE dry:   code=Some(0)   would mirror … run `default` …, recorder `default` … — NOTHING WAS WRITTEN.
/// PROBE apply: code=Some(1)   the settings store already holds a `run` profile called `default` … NOTHING WAS WRITTEN.
/// PROBE store after: [("default", Run)]
/// ```
///
/// Both printed sentences were false: the rehearsal promised a write the store then refused, and
/// the refusal's headline printed over a committed `run` body. The last line is what this test
/// asserts the hardest — **the store must be EMPTY afterwards**, because a check placed anywhere
/// below the plans would still leave that row. (Measured with the run plane's `--profile`, which
/// decision 0111 removed; the `--daemon` half reaches the same collision.)
///
/// It drives the SHIPPED binary rather than `execute`, deliberately: the defect is an ORDERING one
/// between halves of one run, and the exit code an operator's script branches on is part of it.
#[test]
fn one_invocation_may_not_name_one_profile_under_two_kinds() {
    let dir = migrated_settings_dir();
    let daemon_profile = dir.path().join("tradehub.toml");
    std::fs::write(&daemon_profile, DAEMON_PROFILE).expect("write the daemon profile");
    let rec = dir.path().join("rec.toml");
    std::fs::write(&rec, RECORDER_PROFILE).expect("write the recorder profile");

    for extra in ["--dry-run", ""] {
        let mut owned: Vec<String> =
            ["config", "mirror", "--daemon"].iter().map(|s| (*s).to_string()).collect();
        owned.push(daemon_profile.to_str().expect("utf-8 temp path").to_string());
        owned.push("--daemon-name".to_string());
        owned.push("default".to_string());
        owned.push("--recorder".to_string());
        owned.push(rec.to_str().expect("utf-8 temp path").to_string());
        if !extra.is_empty() {
            owned.push(extra.to_string());
        }
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let out = run(dir.path(), &args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "one name under two kinds must FAIL (extra: {extra:?}): {}{}",
            stdout(&out),
            stderr(&out)
        );
        let err = stderr(&out);
        for needle in ["`daemon` profile", "`recorder` profile", "default", "NOTHING WAS WRITTEN"] {
            assert!(err.contains(needle), "the refusal must name {needle:?}: {err}");
        }
        assert!(
            !stdout(&out).contains("would mirror"),
            "a rehearsal may not promise a write the store refuses: {}",
            stdout(&out)
        );
    }

    // THE ASSERTION THE OLD BEHAVIOUR FAILED: nothing at all is in the profile plane. Before the
    // fix this read `[("default", Run)]` while the command had just said NOTHING WAS WRITTEN.
    let profiles =
        vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir.path()))
            .expect("the store must still read");
    assert!(
        profiles.all().is_empty(),
        "no half of a refused run may land: {:?}",
        profiles.all().iter().map(|p| (&p.row.name, p.row.kind)).collect::<Vec<_>>()
    );

    // …and the same two documents under DIFFERENT names are an ordinary success, so the refusal
    // above is about the collision and not about naming two planes in one command.
    let owned: Vec<String> = [
        "config",
        "mirror",
        "--daemon",
        daemon_profile.to_str().expect("utf-8"),
        "--daemon-name",
        "tradehub",
        "--recorder",
        rec.to_str().expect("utf-8"),
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    let out = run(dir.path(), &args);
    assert!(out.status.success(), "two distinct names must mirror: {}", stderr(&out));
    let profiles =
        vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir.path()))
            .expect("read back");
    assert_eq!(profiles.all().len(), 2, "both bodies landed");
}

/// **⚠ THE WRITE PHASE IS NOT ONE TRANSACTION, AND THE REPORT NOW SAYS SO.**
///
/// A `subscription.backfill` word outside the schema's `CHECK` is the one store refusal this verb
/// cannot foresee — `crate::cmd::config::mirror_recorder`'s lowering carries the string through
/// unvalidated and it round-trips, so the recorder half plans clean and the DATABASE is what
/// refuses it. Put a `--daemon` ahead of it and the daemon body has already committed in its own
/// transaction when that happens.
///
/// Before this landing that run printed the store's refusal alone, which is written from inside
/// ONE transaction and is true of it — leaving an operator to read *nothing was written* over a
/// body on disk. Now the run scope is the verb's to state: the sentence is re-scoped to the
/// write it is about, and the report names what landed.
///
/// ⚠ This test deliberately does NOT assert that the `backfill` word is refused at plan time. It
/// is a real gap and it is not this commit's to close — it is also the only deterministic fault
/// injector in the tree for the state this report exists to describe, so closing it silently would
/// take the coverage with it.
#[test]
fn a_fault_after_a_half_has_committed_names_what_landed_and_rescopes_the_claim() {
    let dir = migrated_settings_dir();
    let daemon_profile = dir.path().join("tradehub.toml");
    std::fs::write(&daemon_profile, DAEMON_PROFILE).expect("write the daemon profile");
    let rec = dir.path().join("rec.toml");
    std::fs::write(
        &rec,
        "store = \"market_data/hist\"\n\n[[subscribe]]\nvenue = \"binance\"\n\
         symbols = [\"BTCUSDT.P\"]\nbackfill = \"sometimes\"\n",
    )
    .expect("write a recorder profile the SCHEMA refuses");

    let out = run(
        dir.path(),
        &[
            "config",
            "mirror",
            "--daemon",
            daemon_profile.to_str().expect("utf-8"),
            "--recorder",
            rec.to_str().expect("utf-8"),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "the store refuses it: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("PART OF THIS RUN WAS ALREADY WRITTEN"), "{err}");
    assert!(err.contains("daemon profile `tradehub`"), "it names what landed: {err}");
    assert!(err.contains("recorder profile `default`"), "…and what failed: {err}");

    // THE HALF THAT MATTERS: the run body really is on disk, so a report claiming otherwise would
    // be the defect this verb was audited for.
    let profiles =
        vike_secrets::profile_store::read_profiles(&vike_secrets::db_path_in(dir.path()))
            .expect("the store must still read");
    assert_eq!(
        profiles.all().iter().map(|p| p.row.name.as_str()).collect::<Vec<_>>(),
        vec!["tradehub"],
        "the earlier half committed in its own transaction"
    );
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
            "--backfill",
            "venue",
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
    // …and so did the rows the mirror wrote, which is the whole point of the row-based write path.
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
    assert_eq!(rows[2]["backfill"], "venue");
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
