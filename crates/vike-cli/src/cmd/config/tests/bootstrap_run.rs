use std::collections::BTreeSet;

use super::*;

fn parse(args: &[&str]) -> Result<BootstrapRunArgs, String> {
    parse_bootstrap_run(args.iter().map(|s| (*s).to_string()))
}

/// The ONE live run profile the CI box carries today, as this verb's arguments.
const LIVE: [&str; 7] = [
    "run-live",
    "--mode",
    "live",
    "--risk.max_notional_per_order",
    "100",
    "--risk.max_total_exposure",
    "500",
];

/// A valid value for every key — chosen so the WHOLE body passes `RunProfile::validate` on a
/// `paper` profile (which may carry the venue-owned grid fields a `live` one may not).
fn valid_value(path: &str, shape: Shape) -> &'static str {
    match (path, shape) {
        ("mode", _) => "paper",
        ("guards.initial_trading_state", _) => "active",
        ("risk.max_leverage", _) => "2.0",
        ("risk.required_free_bp_pct", _) => "0.05",
        ("guards.margin_call.buffer", _) => "0.1",
        ("sinks.journal.segment_bytes", _) => "67108864",
        ("sinks.journal.flush_every", _) => "256",
        ("guards.submit_ack_timeout_ms", _) => "30000",
        ("guards.submit_ack_confirm_grace_ms", _) => "15000",
        (_, Shape::Number) => "0.05",
        (_, Shape::Count) => "1000",
        (_, Shape::Switch) => "true",
        (_, Shape::Text) => "/no/such/vike-dir",
        (_, Shape::Word(words)) => words[0],
    }
}

/// A body carrying EVERY key this verb can write, each at a valid value.
fn every_key_body() -> BootstrapRunArgs {
    let mut settings = BTreeMap::new();
    for path in all_run_keys() {
        let shape = run_key(&path).expect("every listed key has a shape");
        let stored = lower_value(&path, shape, valid_value(&path, shape)).expect("valid value");
        settings.insert(path, stored);
    }
    BootstrapRunArgs { name: "every".to_string(), settings, dry_run: true }
}

/// The run profile the daemon would load from these rows, through ITS OWN loader, as its derived
/// `Debug` rendering — this crate links no `vike-core`, so the type itself cannot be named here.
fn daemon_loads(args: &BootstrapRunArgs) -> Result<String, String> {
    vike_tradehub::profile_rows::rows_to_run_profile(&stored_body(args)).map(|p| format!("{p:?}"))
}

#[test]
fn the_minimal_shape_is_a_name_and_a_mode() {
    let a = parse(&["paper-run", "--mode", "paper"]).unwrap();
    assert_eq!(a.name, "paper-run");
    assert_eq!(a.settings.get("mode").map(String::as_str), Some("\"paper\""));
    assert_eq!(a.settings.len(), 1, "a key left off stores no row: {:?}", a.settings);
    assert!(!a.dry_run);
}

#[test]
fn mode_is_required_and_is_one_of_three_words() {
    let e = parse(&["r"]).unwrap_err();
    assert!(e.contains("--mode"), "{e}");
    let e = parse(&["r", "--mode", "demo"]).unwrap_err();
    assert!(e.contains("backtest, paper, live"), "{e}");
}

#[test]
fn every_key_is_its_dotted_path_and_an_unknown_one_is_refused_with_the_list() {
    let a = parse(&[
        "r",
        "--mode",
        "paper",
        "--risk.max_leverage",
        "3",
        "--sinks.journal.dir",
        "/var/wal",
        "--guards.margin_call.mm_requirement",
        "0.05",
    ])
    .unwrap();
    assert_eq!(a.settings.get("risk.max_leverage").map(String::as_str), Some("3.0"));
    assert_eq!(a.settings.get("sinks.journal.dir").map(String::as_str), Some("\"/var/wal\""));
    assert_eq!(
        a.settings.get("guards.margin_call.mm_requirement").map(String::as_str),
        Some("0.05")
    );

    let e = parse(&["r", "--mode", "paper", "--risk.max_levrage", "3"]).unwrap_err();
    assert!(e.contains("--risk.max_levrage"), "names the typo: {e}");
    assert!(e.contains("--risk.max_leverage"), "…and lists the real keys: {e}");
    // The tombstones and the document's own `name` are not keys this verb writes.
    for gone in ["--broker.kind", "--event_source.kind", "--name"] {
        assert!(parse(&["r", "--mode", "paper", gone, "x"]).is_err(), "{gone}");
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
        ("--sinks.journal.dir", " "),
        ("--guards.initial_trading_state", "paused"),
    ] {
        let e = parse(&["r", "--mode", "paper", flag, bad]).unwrap_err();
        assert!(e.contains(flag), "{flag} {bad}: {e}");
    }
}

#[test]
fn a_key_given_twice_is_refused_rather_than_resolved() {
    let e = parse(&["r", "--mode", "paper", "--mode", "live"]).unwrap_err();
    assert!(e.contains("twice"), "{e}");
}

/// A `live` body without the ceilings a live mount refuses to start without is refused HERE — it
/// would otherwise be stored, activated, and then refuse the very mount it exists for. The set is
/// derived from `PRE_TRADE_CEILINGS`, so the message names exactly the missing ones.
#[test]
fn a_live_profile_must_carry_every_refusing_ceiling() {
    let e = parse(&["r", "--mode", "live"]).unwrap_err();
    assert!(e.contains("--risk.max_notional_per_order"), "{e}");
    assert!(e.contains("--risk.max_total_exposure"), "{e}");
    let e = parse(&["r", "--mode", "live", "--risk.max_notional_per_order", "100"]).unwrap_err();
    assert!(!e.contains("--risk.max_notional_per_order"), "only the missing one: {e}");
    assert!(e.contains("--risk.max_total_exposure"), "{e}");
    assert!(parse(&LIVE).is_ok());
    // …and a paper profile needs neither.
    assert!(parse(&["r", "--mode", "paper"]).is_ok());
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
        let e = parse(&["r", "--mode", "paper", &flag, value]).unwrap_err();
        assert!(e.contains(&format!("--{table}.{key}")), "{e}");

        let mut settings = BTreeMap::new();
        settings.insert("mode".to_string(), "\"paper\"".to_string());
        settings.insert(other.clone(), lower_value(&other, shape, value).unwrap());
        let body = BootstrapRunArgs { name: "r".to_string(), settings, dry_run: true };
        let refused = daemon_loads(&body).expect_err("the daemon refuses it too");
        assert!(refused.contains(key), "the daemon names the missing key: {refused}");
    }
}

/// **WRITER ⊆ TYPE.** Every key this verb can write loads through the DAEMON's own loader
/// (`rows_to_run_profile` → `RunProfile::from_toml_str`, `deny_unknown_fields` on every table), so
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

/// **TYPE ⊆ WRITER.** Every leaf of a fully-populated `RunProfile` — read off its `Debug` form,
/// because this crate cannot name the type — is a key this verb writes, except the three it leaves
/// out on purpose (the document's `name`, and the two tombstones). A field added to the type
/// without a key here fails this test, so an operator is never left with a run-profile setting
/// no command can write.
#[test]
fn every_leaf_of_the_run_profile_is_a_key_this_verb_writes() {
    let debug = daemon_loads(&every_key_body()).expect("loads");
    let mut leaves = debug_leaves(&debug);
    for left_out in ["name", "event_source", "broker"] {
        assert!(leaves.remove(left_out), "`{left_out}` is a RunProfile leaf: {debug}");
    }
    let writes: BTreeSet<String> = all_run_keys().into_iter().collect();
    assert_eq!(
        leaves, writes,
        "the run profile's leaves and this verb's keys must be the same set — read off:\n{debug}"
    );
    assert!(leaves.len() > 25, "the harvest found almost nothing: {leaves:?}");
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
    let db = vike_secrets::db_path_in(dir.path());
    vike_secrets::create_empty_store_for_test(&db).unwrap();
    let mut a = parse(&LIVE).unwrap();
    a.dry_run = true;
    let report = bootstrap(&a, Some(dir.path()), 1).unwrap();
    assert!(report.contains("NOTHING WAS WRITTEN"), "{report}");
    assert!(report.contains("mode = \"live\""), "the rendered document: {report}");
    assert!(report.contains("max_total_exposure = 500.0"), "{report}");
    assert!(read_profiles(&db).unwrap().active(ProfileKind::Run).is_none(), "nothing stored");
}

/// **The rows land, are ACTIVE, and are what the daemon loads.** Read back out of a real database
/// and lowered through the daemon's own loader — the same path `vike-tradehub` takes at boot.
#[test]
fn the_run_profile_is_stored_activated_and_loads_as_the_daemon_loads_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = vike_secrets::db_path_in(dir.path());
    vike_secrets::create_empty_store_for_test(&db).unwrap();

    let report = bootstrap(&parse(&LIVE).unwrap(), Some(dir.path()), 1).unwrap();
    assert!(report.contains("ACTIVATED"), "{report}");
    assert!(report.contains("CROSSING"), "nothing was selected before: {report}");

    let profiles = read_profiles(&db).unwrap();
    let stored = profiles.active(ProfileKind::Run).expect("the verb activates what it stores");
    assert_eq!(stored.row.name, "run-live");
    let loaded = vike_tradehub::profile_rows::rows_to_run_profile(stored).expect("loads");
    assert_eq!(loaded.risk.max_notional_per_order, Some(100.0));
    assert_eq!(loaded.risk.max_total_exposure, Some(500.0));
    loaded.risk_for_live_venue_mount().expect("a live profile clears the live-mount gate");

    // A second body under ANOTHER name repoints the active row; the first body stays stored.
    let paper = parse(&["paper-run", "--mode", "paper"]).unwrap();
    let report = bootstrap(&paper, Some(dir.path()), 2).unwrap();
    assert!(report.contains("REPOINTED"), "{report}");
    let profiles = read_profiles(&db).unwrap();
    assert_eq!(profiles.active(ProfileKind::Run).map(|p| p.row.name.as_str()), Some("paper-run"));
    assert!(profiles.by_name("run-live").is_some(), "the earlier body is untouched");
}

/// `profile.name` is ONE namespace across the three kinds: a name a daemon profile holds is
/// refused here, before anything is written, in the store's own words.
#[test]
fn a_name_held_by_another_kind_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let db = vike_secrets::db_path_in(dir.path());
    vike_secrets::create_empty_store_for_test(&db).unwrap();
    // A DAEMON body under the name, stored through the shipped writer of that plane (this file
    // calls no store writer of its own: `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs`
    // pins who may).
    let daemon_file = dir.path().join("shared.toml");
    std::fs::write(
        &daemon_file,
        "venue = \"bybit\"\nasset_class = \"CryptoPerp\"\nsymbol = \"BTCUSDT\"\n",
    )
    .unwrap();
    let planned =
        crate::cmd::config::mirror_profile::plan(dir.path(), &daemon_file, "shared").unwrap();
    crate::cmd::config::mirror_profile::write(
        dir.path(),
        &planned,
        1,
        vike_model::AssetClass::SQL_WORDS,
    )
    .unwrap();
    let e = bootstrap(&parse(&["shared", "--mode", "paper"]).unwrap(), Some(dir.path()), 2)
        .unwrap_err();
    assert!(e.contains("shared"), "{e}");
    assert!(read_profiles(&db).unwrap().active(ProfileKind::Run).is_none());
}
