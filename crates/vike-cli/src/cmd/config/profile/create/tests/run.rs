use std::collections::BTreeSet;

use super::*;

fn parse(args: &[&str]) -> Result<BootstrapRunArgs, args::Stop> {
    parse_bootstrap_run(args.iter().map(|s| (*s).to_string()))
}

/// The live run part of a the build runner box, as this verb's arguments: the daemon profile it rides
/// (decision 0297) and its two ceilings (its `--mode live` went with the key, decision 0156 item 7).
const LIVE: [&str; 5] =
    ["tradehub", "--risk.max_notional_per_order", "100", "--risk.max_total_exposure", "500"];

/// A daemon body under `name`, stored and activated through the shipped writer of that part
/// (`profile create daemon`; this file calls no store writer of its own:
/// `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs` pins who may).
fn store_daemon(dir: &Path, name: &str, symbol: &str) -> String {
    let db = vike_secrets::db_path_in(dir);
    if !db.exists() {
        vike_secrets::create_empty_store_for_test(&db).unwrap();
    }
    let daemon = crate::cmd::config::profile::create::daemon::parse_bootstrap_daemon(
        [
            name,
            "--venue",
            "bybit",
            "--asset-class",
            "CryptoPerp",
            "--symbol",
            symbol,
            "--strategy",
            "nothing.rhai",
            "--summary-ms",
            "60000",
        ]
        .iter()
        .map(|s| (*s).to_string()),
    )
    .unwrap();
    crate::cmd::config::profile::create::daemon::bootstrap(&daemon, Some(dir), 1).unwrap()
}

/// A valid value for every key — chosen so the WHOLE body passes `ProfileRun::validate` (the
/// venue-owned grid fields LOAD: a profile states no tier, and only a `demo`/`live` account's
/// mount refuses them). The watchdog pair is the model's shipped defaults, which satisfy the grace
/// bound `ProfileRun::validate` holds them to.
fn valid_value(path: &str, shape: Shape) -> String {
    let millis = |d: std::time::Duration| d.as_millis().to_string();
    match (path, shape) {
        ("guards.initial_trading_state", _) => "active".into(),
        ("risk.max_leverage", _) => "2.0".into(),
        ("risk.required_free_buying_power_percent", _) => "0.05".into(),
        ("guards.margin_call.buffer", _) => "0.1".into(),
        ("sinks.journal.segment_bytes", _) => "67108864".into(),
        ("sinks.journal.flush_every", _) => "256".into(),
        ("guards.submit_ack_timeout_ms", _) => millis(vike_model::DEFAULT_SUBMIT_ACK_TIMEOUT),
        ("guards.submit_ack_confirm_grace_ms", _) => {
            millis(vike_model::DEFAULT_SUBMIT_ACK_CONFIRM_GRACE)
        }
        (_, Shape::Number) => "0.05".into(),
        (_, Shape::Count) => "1000".into(),
        (_, Shape::Switch) => "true".into(),
        (_, Shape::Word(words)) => words[0].into(),
    }
}

/// A body carrying EVERY key this verb can write, each at a valid value.
fn every_key_body() -> BootstrapRunArgs {
    let mut settings = BTreeMap::new();
    for path in all_run_keys() {
        let shape = run_key(&path).expect("every listed key has a shape");
        let stored = lower_value(&path, shape, &valid_value(&path, shape)).expect("valid value");
        settings.insert(path, stored);
    }
    BootstrapRunArgs { name: "every".to_string(), settings, dry_run: true }
}

/// The run part these arguments write, as a daemon body that carries nothing else (what
/// [`bootstrap`] lays over the stored body).
fn stored_body(args: &BootstrapRunArgs) -> vike_secrets::profile_store::ProfileRecord {
    vike_secrets::profile_store::ProfileRecord {
        row: vike_secrets::profile_store::ProfileRow {
            name: args.name.clone(),
            kind: ProfileKind::Daemon,
            active: false,
            note: None,
        },
        mounts: Vec::new(),
        params: BTreeMap::new(),
        settings: args.settings.clone(),
        recorder: None,
    }
}

/// The run profile the daemon would load from these rows, through ITS OWN loader, as its derived
/// `Debug` rendering — this crate links no `vike-core`, so the type itself cannot be named here.
fn daemon_loads(args: &BootstrapRunArgs) -> Result<String, String> {
    vike_config::rows_to_run_profile(&stored_body(args)).map(|p| format!("{p:?}"))
}

#[test]
fn the_minimal_shape_is_a_name() {
    let a = parse(&["paper-run"]).unwrap();
    assert_eq!(a.name, "paper-run");
    assert!(a.settings.is_empty(), "a key left off stores no row: {:?}", a.settings);
    assert!(!a.dry_run);
}

/// **A run profile has no `mode`** (decision 0156 item 7): the kind of run is the program that
/// runs and where orders go is each account's tier, so `--mode` is an unknown option like any
/// other — refused by `clap`, and no key of the table is named `mode`.
#[test]
fn mode_is_no_key_of_a_run_profile() {
    for word in ["backtest", "paper", "live"] {
        let e = parse(&["r", "--mode", word]).unwrap_err();
        assert!(e.contains("unexpected argument '--mode'"), "{e}");
    }
    assert!(all_run_keys().iter().all(|k| k != "mode"), "no `mode` key is written");
}

#[test]
fn every_key_is_its_dotted_path_and_an_unknown_one_is_refused_with_the_list() {
    let a = parse(&[
        "r",
        "--risk.max_leverage",
        "3",
        "--sinks.journal.snapshot_every",
        "64",
        "--guards.margin_call.maintenance_margin_requirement",
        "0.05",
    ])
    .unwrap();
    assert_eq!(a.settings.get("risk.max_leverage").map(String::as_str), Some("3.0"));
    assert_eq!(a.settings.get("sinks.journal.snapshot_every").map(String::as_str), Some("64"));
    assert_eq!(
        a.settings.get("guards.margin_call.maintenance_margin_requirement").map(String::as_str),
        Some("0.05")
    );

    let e = parse(&["r", "--risk.max_levrage", "3"]).unwrap_err();
    assert!(e.contains("--risk.max_levrage"), "names the typo: {e}");
    assert!(e.contains("--risk.max_leverage"), "…and lists the real keys: {e}");
    // The tombstones, the retired journal directory (the `config.journal_dir` row's alone), the
    // four keys no mount ever applied and the document's own `name` are not keys this verb writes.
    for gone in [
        "--broker.kind",
        "--event_source.kind",
        "--sinks.journal.dir",
        "--sinks.gui",
        "--sinks.recorder",
        "--sinks.raw_capture_dir",
        "--guards.freshness_ms",
        "--name",
    ] {
        assert!(parse(&["r", gone, "x"]).is_err(), "{gone}");
    }
}

#[test]
fn a_value_of_the_wrong_shape_is_refused_by_name() {
    for (flag, bad) in [
        ("--risk.max_notional_per_order", "lots"),
        ("--risk.max_notional_per_order", "NaN"),
        ("--risk.max_notional_per_order", "inf"),
        ("--risk.max_orders_per_window", "-1"),
        ("--risk.max_orders_per_window", "1.5"),
        ("--risk.block_reduce_only_overshoot", "yes"),
        ("--sinks.equity_sample_ms", "soon"),
        ("--guards.initial_trading_state", "paused"),
    ] {
        let e = parse(&["r", flag, bad]).unwrap_err();
        assert!(e.contains(flag), "{flag} {bad}: {e}");
    }
}

#[test]
fn a_key_given_twice_is_refused_rather_than_resolved() {
    let e = parse(&["r", "--risk.max_leverage", "2", "--risk.max_leverage", "3"]).unwrap_err();
    assert!(e.contains("--risk.max_leverage") && e.contains("multiple times"), "{e}");
}

/// A body without the ceilings a `demo`/`live` account's mount refuses to start without is
/// WARNED about, never refused: a profile states no tier (decision 0156 item 7), so it cannot know
/// whether it will reach such an account, and a paper-only box runs without them. The set is
/// derived from `PRE_TRADE_CEILINGS`, so the warning names exactly the missing ones.
#[test]
fn a_body_missing_a_refusing_ceiling_is_stored_with_a_warning_naming_it() {
    let bare = parse(&["r"]).expect("a body with no ceiling is a valid body");
    let missing = missing_live_ceilings(&bare.settings);
    assert_eq!(missing, ["--risk.max_notional_per_order", "--risk.max_total_exposure"]);
    let half = parse(&["r", "--risk.max_notional_per_order", "100"]).unwrap();
    assert_eq!(missing_live_ceilings(&half.settings), ["--risk.max_total_exposure"]);
    assert!(missing_live_ceilings(&parse(&LIVE).unwrap().settings).is_empty());

    // …and the report says it, on the rehearsal as on the write.
    let dir = tempfile::tempdir().unwrap();
    store_daemon(dir.path(), "r", "BTCUSDT");
    store_daemon(dir.path(), "tradehub", "BTCUSDT");
    let mut a = half;
    a.dry_run = true;
    let report = bootstrap(&a, Some(dir.path()), 1).unwrap();
    assert!(report.contains("This body carries no --risk.max_total_exposure"), "{report}");
    assert!(report.contains("REFUSES TO START"), "{report}");
    let mut full = parse(&LIVE).unwrap();
    full.dry_run = true;
    let report = bootstrap(&full, Some(dir.path()), 1).unwrap();
    assert!(!report.contains("This body carries no"), "a full body is not warned: {report}");
}

/// A nested table written without the key it cannot exist without is refused here — and the
/// daemon's loader agrees that each such body is unloadable, which is what pins
/// [`TABLE_REQUIRES`] to the type rather than to this file.
#[test]
fn a_table_missing_its_required_key_is_refused_and_the_daemon_agrees() {
    for (table, key) in TABLE_REQUIRES {
        let other = all_run_keys()
            .into_iter()
            .find(|k| k.starts_with(&format!("{table}.")) && *k != format!("{table}.{key}"))
            .expect("each table has a second key");
        let flag = format!("--{other}");
        let shape = run_key(&other).unwrap();
        let value = valid_value(&other, shape);
        let value = value.as_str();
        let e = parse(&["r", &flag, value]).unwrap_err();
        assert!(e.contains(&format!("--{table}.{key}")), "{e}");

        let mut settings = BTreeMap::new();
        settings.insert(other.clone(), lower_value(&other, shape, value).unwrap());
        let body = BootstrapRunArgs { name: "r".to_string(), settings, dry_run: true };
        let refused = daemon_loads(&body).expect_err("the daemon refuses it too");
        assert!(refused.contains(key), "the daemon names the missing key: {refused}");
    }
}

/// **WRITER ⊆ TYPE.** Every key this verb can write loads through the DAEMON's own loader
/// (`rows_to_run_profile` → `ProfileRun::from_toml_str`, `deny_unknown_fields` on every table), so
/// a key the type does not have, or a value shape it refuses, cannot be stored by this verb.
#[test]
fn every_key_this_verb_writes_loads_through_the_daemons_loader() {
    let body = every_key_body();
    daemon_loads(&body).unwrap_or_else(|e| {
        panic!(
            "the daemon refuses a body this verb would store: {e}\n{}",
            render_run_toml(&stored_body(&body))
        )
    });
}

/// **TYPE ⊆ WRITER.** Every leaf of a fully-populated `ProfileRun` — read off its `Debug` form,
/// because this crate cannot name the type — is a key this verb writes, except the ones it leaves
/// out on purpose (the document's `name`, the two table tombstones, the retired
/// `sinks.journal.dir` tombstone and the four tombstones of the keys no mount ever applied). A
/// field added to the type without a key here fails this test, so an operator is never left with a
/// run-profile setting no command can write.
#[test]
fn every_leaf_of_the_run_profile_is_a_key_this_verb_writes() {
    let debug = daemon_loads(&every_key_body()).expect("loads");
    let mut leaves = debug_leaves(&debug);
    for left_out in [
        "name",
        "event_source",
        "broker",
        "sinks.journal.dir",
        "sinks.gui",
        "sinks.recorder",
        "sinks.raw_capture_dir",
        "guards.freshness_ms",
    ] {
        assert!(leaves.remove(left_out), "`{left_out}` is a ProfileRun leaf: {debug}");
    }
    let writes: BTreeSet<String> = all_run_keys().into_iter().collect();
    assert_eq!(
        leaves, writes,
        "the run profile's leaves and this verb's keys must be the same set — read off:\n{debug}"
    );
    assert!(leaves.len() > 20, "the harvest found almost nothing: {leaves:?}");
}

/// The dotted paths of every scalar leaf in a derived `Debug` rendering: a field whose value opens
/// a struct (`Name {` or `Some(Name {`) is a table, anything else is a leaf.
fn debug_leaves(debug: &str) -> BTreeSet<String> {
    let bytes = debug.as_bytes();
    let mut out = BTreeSet::new();
    let mut path: Vec<String> = Vec::new();
    let mut opened: Vec<bool> = Vec::new();
    let mut pending: Option<String> = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'{' => {
                let table = pending.take();
                if let Some(t) = &table {
                    path.push(t.clone());
                }
                opened.push(table.is_some());
                i += 1;
            }
            b'}' => {
                if opened.pop() == Some(true) {
                    path.pop();
                }
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let ident = &debug[start..i];
                if let Some(rest) = debug[i..].strip_prefix(": ") {
                    let value = rest.strip_prefix("Some(").unwrap_or(rest);
                    let opens_struct = value.split_once(' ').is_some_and(|(head, tail)| {
                        head.starts_with(|c: char| c.is_ascii_uppercase())
                            && head.chars().all(|c| c.is_ascii_alphanumeric())
                            && tail.starts_with('{')
                    });
                    if opens_struct {
                        pending = Some(ident.to_string());
                    } else {
                        let mut leaf = path.clone();
                        leaf.push(ident.to_string());
                        out.insert(leaf.join("."));
                    }
                }
            }
            _ => i += 1,
        }
    }
    out
}

#[test]
fn the_debug_harvest_reads_nesting_and_options() {
    let leaves = debug_leaves(
        "R { a: 1, s: S { b: true, j: Some(J { d: \"x{y}\", e: 2 }), k: None }, m: Live }",
    );
    let want: BTreeSet<String> =
        ["a", "s.b", "s.j.d", "s.j.e", "s.k", "m"].iter().map(|s| (*s).to_string()).collect();
    assert_eq!(leaves, want);
}

/// **The command the DAEMON prints for a stored `mode` row is a command THIS verb runs, and it
/// deletes exactly that row** (decision 0156 item 7: a stored `mode` row refuses startup, and the
/// owner deletes it at the deploy that ships this). The daemon's error is read back, its
/// `profile create run` line tokenised as a shell would, parsed by this verb's own parser, and the body
/// it would store must be the old body minus `mode` — every other row carried — and must load.
/// (No run-profile key holds free text any more, so the shell-quoting of a value is pinned where
/// it is made, `vike_config`'s `rows_tests.rs`.)
#[test]
fn the_daemons_fix_for_a_stored_mode_row_is_a_command_this_verb_runs() {
    let mut body = parse(&LIVE).unwrap();
    for (path, raw) in
        [("guards.initial_trading_state", "active"), ("sinks.equity_sample_ms", "2500")]
    {
        let stored = lower_value(path, run_key(path).expect("a key"), raw).expect("a valid value");
        body.settings.insert(path.to_string(), stored);
    }
    let without_mode = body.settings.clone();
    body.settings.insert("mode".into(), "\"live\"".into());

    let e = daemon_loads(&body).expect_err("a stored `mode` row refuses the daemon's start");
    let line = e
        .lines()
        .map(str::trim_start)
        .find(|l| l.starts_with("vike-cli config profile create run "))
        .unwrap_or_else(|| panic!("the refusal must carry the command: {e}"));
    let words = shell_words(line);
    assert_eq!(&words[..5], ["vike-cli", "config", "profile", "create", "run"], "{line}");
    let fixed = parse_bootstrap_run(words.into_iter().skip(5))
        .unwrap_or_else(|err| panic!("this verb must accept the printed command `{line}`: {err}"));
    assert_eq!(fixed.name, "tradehub");
    assert_eq!(fixed.settings, without_mode, "every other row carried, `mode` gone: {line}");
    daemon_loads(&fixed).expect("the re-written body loads");
}

/// Split one command line the way a POSIX shell does for the quoting the daemon's fix emits:
/// whitespace separates words, `'…'` quotes literally, `\` escapes one character outside quotes.
fn shell_words(line: &str) -> Vec<String> {
    let (mut out, mut current, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => (quoted, any) = (!quoted, true),
            '\\' if !quoted => {
                if let Some(n) = chars.next() {
                    current.push(n);
                    any = true;
                }
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut current));
                    any = false;
                }
            }
            c => {
                current.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(current);
    }
    out
}

#[test]
fn help_travels_back_as_the_shared_sentinel() {
    assert!(parse(&["--help"]).is_err());
}

#[test]
fn a_run_with_no_settings_directory_refuses_rather_than_guessing_one() {
    let a = parse(&LIVE).unwrap();
    let e = bootstrap(&a, None, 0).unwrap_err();
    assert!(e.contains("VIKE_SETTINGS_DIR"), "{e}");
}

/// The dry run prints the document the daemon would parse and writes NOTHING.
#[test]
fn the_dry_run_prints_the_document_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    store_daemon(dir.path(), "tradehub", "BTCUSDT");
    let db = vike_secrets::db_path_in(dir.path());
    let mut a = parse(&LIVE).unwrap();
    a.dry_run = true;
    let report = bootstrap(&a, Some(dir.path()), 1).unwrap();
    assert!(report.contains("NOTHING WAS WRITTEN"), "{report}");
    assert!(!report.contains("mode = "), "a run profile has no mode: {report}");
    assert!(report.contains("max_total_exposure = 500.0"), "{report}");
    assert!(!report.contains("summary_ms"), "the daemon part is no run document: {report}");
    let profiles = read_profiles(&db).unwrap();
    let body = profiles.by_name("tradehub").unwrap();
    assert!(!body.settings.keys().any(|k| is_run_part(k)), "nothing stored");
}

/// **The four keys no mount ever applied are no flags of this verb, and the daemon refuses a stored
/// body that still carries one — by name.** `clap` refuses the flag (the key table has no row for
/// it), and a body built with the retired rows added by hand — what an older `profile create run` stored
/// — is refused by the daemon's own loader with the sentence that names the key and the re-run that
/// drops it. `--help` says so too, and marks no key "NOT APPLIED": every key it lists is applied.
#[test]
fn a_retired_key_is_no_flag_and_a_stored_body_carrying_one_is_refused_by_name() {
    for (key, value) in [
        ("guards.freshness_ms", "5000"),
        ("sinks.gui", "true"),
        ("sinks.recorder", "true"),
        ("sinks.raw_capture_dir", "\"/srv/caps\""),
    ] {
        let e = parse(&["r", &format!("--{key}"), "1"]).unwrap_err();
        assert!(e.contains(&format!("unexpected argument '--{key}'")), "{key}: {e}");

        let mut body = every_key_body();
        body.settings.insert(key.to_string(), value.to_string());
        let refused = daemon_loads(&body).expect_err("the daemon refuses the retired row");
        assert!(
            refused.contains(&format!("`{key}` is no longer part of a run profile")),
            "{refused}"
        );
        assert!(refused.contains(&format!("WITHOUT `--{key}`")), "{refused}");
    }

    use clap::Args as _;
    let cmd = BootstrapRunCommand::augment_args(clap::Command::new("run"));
    let long = cmd.get_long_about().map(ToString::to_string).unwrap_or_default();
    for key in ["sinks.gui", "sinks.recorder", "sinks.raw_capture_dir", "guards.freshness_ms"] {
        assert!(long.contains(key), "the long help omits the retired {key}: {long}");
        assert!(cmd.get_arguments().all(|a| a.get_id().as_str() != key), "--{key} is a flag");
    }
    for arg in cmd.get_arguments() {
        let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
        assert!(!help.contains("NOT APPLIED"), "--{} is marked unapplied: {help}", arg.get_id());
    }
}

/// **Two writers, one body (decision 0297).** The run part lands in the named DAEMON body and is
/// what the daemon loads; this verb keeps the mounts and `daemon.*`, `profile create daemon` keeps the
/// run part, and re-running this verb with no key leaves the body with none. It selects nothing:
/// the active daemon profile is still the one that was active.
#[test]
fn the_run_part_is_written_into_the_daemon_body_and_each_writer_keeps_the_other_part() {
    let dir = tempfile::tempdir().unwrap();
    let db = vike_secrets::db_path_in(dir.path());
    // `profile create daemon` activates what it stores, so `tradehub`, stored last, is the active one.
    store_daemon(dir.path(), "twin", "ETHUSDT");
    store_daemon(dir.path(), "tradehub", "BTCUSDT");

    let report = bootstrap(&parse(&LIVE).unwrap(), Some(dir.path()), 2).unwrap();
    assert!(report.contains("`tradehub` is the ACTIVE daemon profile"), "{report}");
    let profiles = read_profiles(&db).unwrap();
    let stored = profiles.active(ProfileKind::Daemon).expect("the daemon body is still active");
    assert_eq!(stored.row.name, "tradehub");
    assert_eq!(stored.mounts.len(), 1, "the mount is kept");
    assert_eq!(stored.settings.get("daemon.summary_ms").map(String::as_str), Some("60000"));
    let loaded = vike_config::rows_to_run_profile(stored).expect("loads");
    assert_eq!(loaded.risk.max_notional_per_order, Some(100.0));
    assert_eq!(loaded.risk.max_total_exposure, Some(500.0));
    vike_secrets::profile_store::render_daemon_toml(stored).expect("the daemon part renders too");

    // A body that is not the active one gets the run part and stays unselected.
    let halted = parse(&["twin", "--guards.initial_trading_state", "halted"]).unwrap();
    let twin = bootstrap(&halted, Some(dir.path()), 3).unwrap();
    assert!(twin.contains("`twin` is NOT the active daemon profile"), "{twin}");
    let profiles = read_profiles(&db).unwrap();
    assert_eq!(profiles.active(ProfileKind::Daemon).unwrap().row.name, "tradehub");

    // `profile create daemon` re-stores the mount and KEEPS the run part, and says so.
    let report = store_daemon(dir.path(), "tradehub", "BTCUSDT.P");
    assert!(report.contains("Keeps its stored run part"), "{report}");
    let profiles = read_profiles(&db).unwrap();
    let stored = profiles.by_name("tradehub").unwrap();
    assert_eq!(stored.mounts.len(), 1, "the mount part is the re-stored one");
    assert_eq!(stored.settings.get("risk.max_total_exposure").map(String::as_str), Some("500.0"));

    // …and this verb with no key leaves the body with NO run part, the mount kept.
    bootstrap(&parse(&["tradehub"]).unwrap(), Some(dir.path()), 4).unwrap();
    let profiles = read_profiles(&db).unwrap();
    let stored = profiles.by_name("tradehub").unwrap();
    assert!(!stored.settings.keys().any(|k| is_run_part(k)), "{:?}", stored.settings);
    assert_eq!(stored.mounts.len(), 1);
}

/// The run part is written INTO a stored daemon body: a name the store holds as no daemon body is
/// refused naming `profile create daemon`, and a name held by a RETIRED `run` body (decision 0297) is
/// refused with the cure naming the active daemon profile. Nothing is written either way.
#[test]
fn a_name_that_is_no_daemon_body_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let db = vike_secrets::db_path_in(dir.path());
    store_daemon(dir.path(), "tradehub", "BTCUSDT");
    let e = bootstrap(&parse(&["nonsuch"]).unwrap(), Some(dir.path()), 2).unwrap_err();
    assert!(e.contains("holds no daemon profile called `nonsuch`"), "{e}");
    assert!(e.contains("profile create daemon nonsuch"), "{e}");

    // A run body an older CLI stored, planted by SQL: no writer stores the retired kind.
    vike_secrets::hold_write_lock(dir.path())
        .execute_batch(
            "INSERT INTO profile (name, kind, active, updated_utc, updated_by) \
             VALUES ('run-live', 'run', 1, 0, 'an older vike-cli'); COMMIT;",
        )
        .expect("the retired row is planted");
    for dry_run in [true, false] {
        let mut a = parse(&["run-live", "--risk.max_notional_per_order", "100"]).unwrap();
        a.dry_run = dry_run;
        let e = bootstrap(&a, Some(dir.path()), 3).unwrap_err();
        assert!(e.contains("RETIRED `run` profile"), "{e}");
        assert!(
            e.contains("profile create run tradehub"),
            "the cure names the daemon profile: {e}"
        );
        assert!(e.contains("config profile deactivate run"), "{e}");
    }
    let profiles = read_profiles(&db).unwrap();
    assert!(!profiles.by_name("tradehub").unwrap().settings.keys().any(|k| is_run_part(k)));
}

/// **A `profile create daemon` that REPOINTS the active profile says which run part comes in** (none,
/// here), as `config profile activate daemon` does, so the ceilings it drops are visible; re-storing the
/// active profile under its own name says nothing of the kind.
#[test]
fn a_bootstrap_daemon_that_repoints_names_the_run_part_it_brings() {
    let dir = tempfile::tempdir().unwrap();
    store_daemon(dir.path(), "tradehub", "BTCUSDT");
    bootstrap(&parse(&LIVE).unwrap(), Some(dir.path()), 2).unwrap();
    let same = store_daemon(dir.path(), "tradehub", "BTCUSDT");
    assert!(same.contains("Keeps its stored run part"), "{same}");
    assert!(!same.contains("Its run part is EMPTY"), "{same}");
    // The rehearsal says it too.
    let rehearsal = crate::cmd::config::profile::create::daemon::parse_bootstrap_daemon(
        [
            "other",
            "--venue",
            "bybit",
            "--asset-class",
            "CryptoPerp",
            "--symbol",
            "BTCUSDT",
            "--strategy",
            "nothing.rhai",
            "--dry-run",
        ]
        .iter()
        .map(|s| (*s).to_string()),
    )
    .unwrap();
    let dry =
        crate::cmd::config::profile::create::daemon::bootstrap(&rehearsal, Some(dir.path()), 3)
            .unwrap();
    assert!(dry.contains("NOTHING WAS WRITTEN"), "{dry}");
    assert!(dry.contains("Its run part is EMPTY"), "the rehearsal names it too: {dry}");
    let report = store_daemon(dir.path(), "other", "BTCUSDT");
    assert!(report.contains("REPOINTED"), "{report}");
    assert!(report.contains("Its run part is EMPTY"), "the ceilings it drops are said: {report}");
}
