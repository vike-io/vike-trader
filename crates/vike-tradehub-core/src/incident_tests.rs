use super::*;

// ---- window-overlap selection ----

#[test]
fn select_overlapping_keeps_at_or_after_cutoff() {
    let items = [("a", 100_i64), ("b", 200), ("c", 300)];
    // now=300, since=100 → cutoff=200 → keep b (boundary, inclusive) and c; drop a.
    assert_eq!(select_overlapping(&items, 300, 100), vec!["b", "c"]);
    // since=0 → cutoff=now=300 → only the file written exactly at now.
    assert_eq!(select_overlapping(&items, 300, 0), vec!["c"]);
    // a huge window (saturating) → everything.
    assert_eq!(select_overlapping(&items, 300, i64::MAX), vec!["a", "b", "c"]);
}

#[test]
fn select_overlapping_excludes_everything_before_the_window() {
    // every file's last write predates the cutoff → nothing overlaps (the degenerate/off case).
    let items = [("old1", 10_i64), ("old2", 50)];
    assert!(select_overlapping(&items, 1_000, 100).is_empty()); // cutoff=900 > 50
    // and an empty input is empty out.
    let empty: Vec<((), i64)> = Vec::new();
    assert!(select_overlapping(&empty, 1_000, 100).is_empty());
}

// ---- redaction ----

/// The environment dump masks through `vike_config::redact::matches_secret_rule`, the STRICT
/// predicate, and through nothing else: this crate holds no list of credential names (there used to
/// be a second one here, which printed an `_USER` login verbatim). The table's own tests are
/// `vike-config`'s; this one proves the wiring, in both directions, on shapes only the other table
/// used to know (`_USER`) and only this one used to know (`_PK`, a mnemonic).
#[test]
fn the_env_dump_masks_exactly_what_the_shared_classifier_calls_secret() {
    let names = [
        "ACME_LIVE_API_KEY",
        "ACME_LIVE_API_SECRET",
        "ACME_DEMO_API_PASSPHRASE",
        "ACME_PRIVATE_KEY",
        "ACME_LIVE_PK",
        "ACME_DEMO_USER",
        "GITHUB_TOKEN",
        "some_wallet_mnemonic",
        "VIKE_RECONCILE",
        "VIKE_JOURNAL_DIR",
        "VIKE_LOG_DIR",
        "RUST_LOG",
        "VIKE_PIN_CORES",
    ];
    let raw: Vec<(String, String)> =
        names.iter().map(|k| ((*k).to_string(), "value-1234".to_string())).collect();
    for (k, v) in redact_env(&raw) {
        let secret = vike_config::redact::matches_secret_rule(&k);
        assert_eq!(v == "***1234", secret, "{k}: masked iff the shared classifier says secret");
        assert!(secret || v == "value-1234", "{k}: an unmasked value passes verbatim");
    }
    // The names `vike_config::redact::is_secret` EXCUSES (flags, counts, paths that merely contain
    // a needle) are still masked in a bundle: the dump is the strict side. Assembled at run time
    // because the settings-registry literal sweep reads a whole `VIKE_*` string as a read.
    for excused in [
        ["VIKE", "NO", "LIVE", "KEYS"].join("_"),
        ["VIKE", "HOLD", "TOKENS"].join("_"),
        ["CTRADER", "TOKEN", "FILE"].join("_"),
        ["VIKE", "TRADE", "SEED"].join("_"),
    ] {
        assert!(!vike_config::redact::is_secret(&excused), "{excused}: premise, it is excused");
        let out = redact_env(&[(excused.clone(), "value-1234".to_string())]);
        assert_eq!(out[0].1, "***1234", "{excused}: the incident bundle masks it all the same");
    }
    let masked =
        |k: &str| redact_env(&[(k.to_string(), "value-1234".to_string())])[0].1 != "value-1234";
    for k in ["ACME_LIVE_API_KEY", "ACME_LIVE_PK", "ACME_DEMO_USER", "some_wallet_mnemonic"] {
        assert!(masked(k), "{k} should be treated as secret");
    }
    for k in ["VIKE_RECONCILE", "VIKE_JOURNAL_DIR", "VIKE_LOG_DIR", "RUST_LOG", "VIKE_PIN_CORES"] {
        assert!(!masked(k), "{k} is a runtime flag, not a secret");
    }
}

#[test]
fn redact_secret_value_masks_all_but_the_last_four() {
    assert_eq!(redact_secret_value("supersecretvalue123"), "***e123");
    assert_eq!(redact_secret_value("abcd"), "***abcd"); // exactly four → all four are the tail
    assert_eq!(redact_secret_value("xy"), "***"); // too short → bare mask, no tail
    assert_eq!(redact_secret_value(""), "***");
}

#[test]
fn redact_env_sorts_and_masks_secret_values_only() {
    let raw = [
        ("VIKE_RECONCILE".to_string(), "1".to_string()),
        ("ACME_LIVE_API_SECRET".to_string(), "supersecretvalue123".to_string()),
        ("ABC".to_string(), "plain".to_string()),
    ];
    let out = redact_env(&raw);
    // sorted by key
    assert_eq!(
        out.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        ["ABC", "ACME_LIVE_API_SECRET", "VIKE_RECONCILE"]
    );
    // the full secret never appears; only the masked fingerprint does; flags pass verbatim.
    let flat = format!("{out:?}");
    assert!(!flat.contains("supersecretvalue123"), "full secret leaked: {flat}");
    assert!(flat.contains("***e123"), "masked fingerprint missing: {flat}");
    assert!(flat.contains("\"1\""), "the plain flag value must pass through: {flat}");
}

// ---- stamps + hash ----

#[test]
fn utc_stamps_match_known_instants() {
    assert_eq!(utc_stamp_compact(0), "19700101T000000Z");
    assert_eq!(vike_model::time::epoch_ms_to_utc_timestamp_ms(0), "1970-01-01T00:00:00.000Z");
    // 2021-01-01T00:00:00Z + 1h1m1s
    assert_eq!(utc_stamp_compact(1_609_459_200_000), "20210101T000000Z");
    assert_eq!(utc_stamp_compact(1_609_459_200_000 + 3_661_000), "20210101T010101Z");
    assert_eq!(
        vike_model::time::epoch_ms_to_utc_timestamp_ms(1_609_459_200_000 + 3_661_123),
        "2021-01-01T01:01:01.123Z"
    );
}

// ---- manifest assembly ----

fn build_info() -> GitBuildInfo {
    GitBuildInfo {
        pkg_name: "vike-tradehub-core".to_string(),
        pkg_version: "0.1.0".to_string(),
        build_profile: "release".to_string(),
        git_sha: Some("deadbeef".to_string()),
        git_dirty: Some(false),
    }
}

#[test]
fn build_manifest_assembles_window_and_redacts_env_by_construction() {
    let env = [
        ("VIKE_RECONCILE".to_string(), "1".to_string()),
        ("ACME_LIVE_API_SECRET".to_string(), "supersecretvalue123".to_string()),
    ];
    let m = build_manifest(
        1_609_459_200_000,
        3_600_000,
        build_info(),
        None,
        None,
        None,
        EngineState::unreachable(None),
        &env, // RAW env with a secret — build_manifest must redact it
    );
    assert_eq!(m.kind, "vike-incident-bundle");
    assert_eq!(m.schema, 1);
    assert_eq!(m.window.now_ms, 1_609_459_200_000);
    assert_eq!(m.window.cutoff_ms, 1_609_459_200_000 - 3_600_000);
    assert_eq!(m.window.cutoff_utc, "2020-12-31T23:00:00.000Z");
    assert!(!m.engine_state.reachable);

    // The serialized artifact: no full secret; the mask and the flag.
    let json = serde_json::to_string(&m).expect("manifest serializes");
    assert!(!json.contains("supersecretvalue123"), "full secret reached the manifest: {json}");
    assert!(json.contains("***e123"), "masked fingerprint missing: {json}");
    assert!(json.contains("VIKE_RECONCILE"), "plain flag missing: {json}");

    // env sorted: the credential key precedes the VIKE flag.
    let ib = m.env.iter().position(|(k, _)| k == "ACME_LIVE_API_SECRET").unwrap();
    let iv = m.env.iter().position(|(k, _)| k == "VIKE_RECONCILE").unwrap();
    assert!(ib < iv, "env must be sorted by key");

    // round-trips (the artifact is machine-readable).
    let back: IncidentManifest = serde_json::from_str(&json).expect("manifest round-trips");
    assert_eq!(back.window.cutoff_ms, m.window.cutoff_ms);
}

// ---- end-to-end orchestration (temp dir, injected clock + env; no real secret on disk) ----

fn temp_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vike_incident_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn run_incident_at_freezes_the_window_and_never_writes_a_secret() {
    let root = temp_root("wide");
    let jdir = root.join("journal");
    let ldir = root.join("logs");
    std::fs::create_dir_all(&jdir).unwrap();
    std::fs::create_dir_all(&ldir).unwrap();
    // A fake segment (no journal MAGIC → engine_state unreachable, but still frozen) and a log.
    std::fs::write(jdir.join("journal-00000001.vjl"), b"not-a-real-journal-segment").unwrap();
    std::fs::write(ldir.join("vike.2026-07-24"), b"log line, no secrets here\n").unwrap();

    let cfg = IncidentConfig {
        since: Duration::from_secs(10 * 365 * 86_400), // wide → include everything
        out_root: root.join("out"),
        journal_dir: Some(jdir.clone()),
        log_dir: Some(ldir.clone()),
        run_profile: None,
    };
    // Injected env carries a secret; injected clock is ahead of the just-written files.
    let env = vec![
        ("VIKE_JOURNAL_DIR".to_string(), jdir.display().to_string()),
        ("ACME_LIVE_API_SECRET".to_string(), "supersecretvalue123".to_string()),
    ];
    let now = vike_model::now_ms() + 60_000;
    let bundle = run_incident_at(&cfg, now, env).expect("bundle written");

    // manifest exists, parses, carries NO secret.
    let text = std::fs::read_to_string(bundle.join("manifest.json")).expect("manifest present");
    assert!(!text.contains("supersecretvalue123"), "the on-disk bundle leaked a secret!");
    let m: IncidentManifest = serde_json::from_str(&text).expect("manifest parses");

    // both evidence files frozen as gz.
    assert!(bundle.join("journal").join("journal-00000001.vjl.gz").is_file());
    assert!(bundle.join("logs").join("vike.2026-07-24.gz").is_file());
    assert_eq!(m.journal.as_ref().unwrap().segments_selected, 1);
    assert_eq!(m.logs.as_ref().unwrap().files_selected, 1);
    // a non-journal file yields an unreachable (but valid) engine-state section.
    assert!(!m.engine_state.reachable);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn run_incident_at_is_inert_when_the_window_selects_nothing() {
    // The OFF path: a zero-length window freezes nothing and needs nothing from the live
    // system, yet still emits a valid, secret-free manifest; the trading node is untouched.
    let root = temp_root("empty");
    let jdir = root.join("journal");
    let ldir = root.join("logs");
    std::fs::create_dir_all(&jdir).unwrap();
    std::fs::create_dir_all(&ldir).unwrap();
    std::fs::write(jdir.join("journal-00000001.vjl"), b"not-a-real-journal-segment").unwrap();
    std::fs::write(ldir.join("vike.2026-07-24"), b"older log line\n").unwrap();

    let cfg = IncidentConfig {
        since: Duration::from_millis(0), // cutoff == now → the just-written files are excluded
        out_root: root.join("out"),
        journal_dir: Some(jdir.clone()),
        log_dir: Some(ldir.clone()),
        run_profile: None,
    };
    let now = vike_model::now_ms() + 60_000; // strictly after the files' mtimes
    let bundle = run_incident_at(&cfg, now, vec![("VIKE_RECONCILE".to_string(), "1".to_string())])
        .expect("bundle written");

    let text = std::fs::read_to_string(bundle.join("manifest.json")).expect("manifest present");
    let m: IncidentManifest = serde_json::from_str(&text).expect("manifest parses");
    assert_eq!(m.journal.as_ref().unwrap().segments_selected, 0, "nothing in the window");
    assert_eq!(m.logs.as_ref().unwrap().files_selected, 0, "nothing in the window");
    // segments were still ENUMERATED (total), just none selected — the tool is inert, not blind.
    assert_eq!(m.journal.as_ref().unwrap().segments_total, 1);
    // no gz files written for an empty selection.
    assert!(!bundle.join("journal").join("journal-00000001.vjl.gz").exists());

    let _ = std::fs::remove_dir_all(&root);
}

// ---- the run profile: the ACTIVE `run` row, never a file ----

/// The bundle pins the row the daemon loads: its name, the hash of the rendered body, a gzipped
/// copy, and whether it validates. The journal it freezes is the `config.journal_dir` row's (the
/// binary resolves it into `journal_dir`), never the run profile's: a profile names no directory
/// (decision 0111, amended 2026-10-11), so a profile with a `[sinks.journal]` tuning table and no
/// directory handed in freezes no journal.
#[test]
fn the_active_run_row_is_pinned_and_the_journal_is_the_rows() {
    let root = temp_root("row");
    let jdir = root.join("wal");
    std::fs::create_dir_all(&jdir).unwrap();
    std::fs::write(jdir.join("journal-00000001.vjl"), b"not-a-real-journal-segment").unwrap();
    let toml = "[risk]\nmax_notional_per_order = 5000.0\nmax_total_exposure = 25000.0\n\n\
                [sinks.journal]\nsnapshot_every = 64\n"
        .to_string();

    // No directory handed in: the profile's tuning table names none, so no journal is frozen.
    let no_dir = IncidentConfig {
        since: Duration::from_secs(10 * 365 * 86_400),
        out_root: root.join("out-no-dir"),
        journal_dir: None,
        log_dir: None,
        run_profile: Some(Ok(RunProfileRow { name: "run-live".to_string(), toml: toml.clone() })),
    };
    let bundle = run_incident_at(&no_dir, vike_model::now_ms() + 60_000, Vec::new()).unwrap();
    let text = std::fs::read_to_string(bundle.join("manifest.json")).unwrap();
    let m: IncidentManifest = serde_json::from_str(&text).unwrap();
    assert!(m.journal.is_none(), "a run profile names no journal directory: {:?}", m.journal);

    let cfg = IncidentConfig {
        since: Duration::from_secs(10 * 365 * 86_400),
        out_root: root.join("out"),
        journal_dir: Some(jdir.clone()),
        log_dir: None,
        run_profile: Some(Ok(RunProfileRow { name: "run-live".to_string(), toml: toml.clone() })),
    };
    let bundle = run_incident_at(&cfg, vike_model::now_ms() + 60_000, Vec::new()).unwrap();
    let text = std::fs::read_to_string(bundle.join("manifest.json")).unwrap();
    let m: IncidentManifest = serde_json::from_str(&text).unwrap();

    let p = m.profile.expect("an active row is captured");
    assert_eq!(p.row, "run-live");
    assert!(p.loaded && p.valid, "{p:?}");
    assert_eq!(
        p.sha_fnv1a64.as_deref(),
        Some(vike_util::fnv::fnv1a64_hex(toml.as_bytes()).as_str())
    );
    assert!(bundle.join("profile.toml.gz").is_file(), "the rendered body is frozen");
    let j = m.journal.expect("the config.journal_dir directory names the segments to freeze");
    assert_eq!(j.segments_selected, 1);

    let _ = std::fs::remove_dir_all(&root);
}

/// A settings store that EXISTS and cannot be read is recorded as such in the manifest — never
/// left out as if no run profile were in force.
#[test]
fn an_unreadable_store_is_recorded_not_skipped() {
    let root = temp_root("unreadable");
    let cfg = IncidentConfig {
        since: Duration::from_secs(60),
        out_root: root.join("out"),
        journal_dir: None,
        log_dir: None,
        run_profile: Some(Err("the settings database could not be read".to_string())),
    };
    let bundle = run_incident_at(&cfg, vike_model::now_ms(), Vec::new()).unwrap();
    let text = std::fs::read_to_string(bundle.join("manifest.json")).unwrap();
    let m: IncidentManifest = serde_json::from_str(&text).unwrap();
    let p = m.profile.expect("the failure is part of the evidence");
    assert!(!p.loaded && !p.valid);
    assert!(p.error.as_deref().is_some_and(|e| e.contains("could not be read")), "{p:?}");
    assert!(!bundle.join("profile.toml.gz").exists());

    let _ = std::fs::remove_dir_all(&root);
}
