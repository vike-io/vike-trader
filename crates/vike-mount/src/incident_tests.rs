use super::*;

// ── window-overlap selection ─────────────────────────────────────────────────────────────

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

// ── redaction ────────────────────────────────────────────────────────────────────────────

#[test]
fn is_secret_key_matches_every_credential_shape_but_not_vike_flags() {
    for k in [
        "BINANCE_LIVE_API_KEY",
        "BINANCE_LIVE_API_SECRET",
        "OKX_DEMO_API_PASSPHRASE",
        "POLY_MAINNET_PRIVATE_KEY",
        "POLY_LIVE_PK",
        "GITHUB_TOKEN",
        "some_wallet_mnemonic",
    ] {
        assert!(is_secret_key(k), "{k} should be treated as secret");
    }
    for k in ["VIKE_RECONCILE", "VIKE_JOURNAL_DIR", "VIKE_LOG_DIR", "RUST_LOG", "VIKE_PIN_CORES"] {
        assert!(!is_secret_key(k), "{k} is a runtime flag, not a secret");
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
        ("BINANCE_LIVE_API_SECRET".to_string(), "supersecretvalue123".to_string()),
        ("ABC".to_string(), "plain".to_string()),
    ];
    let out = redact_env(&raw);
    // sorted by key
    assert_eq!(
        out.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        ["ABC", "BINANCE_LIVE_API_SECRET", "VIKE_RECONCILE"]
    );
    // the full secret never appears; only the masked fingerprint does; flags pass verbatim.
    let flat = format!("{out:?}");
    assert!(!flat.contains("supersecretvalue123"), "full secret leaked: {flat}");
    assert!(flat.contains("***e123"), "masked fingerprint missing: {flat}");
    assert!(flat.contains("\"1\""), "the plain flag value must pass through: {flat}");
}

// ── stamps + hash ────────────────────────────────────────────────────────────────────────

#[test]
fn utc_stamps_match_known_instants() {
    assert_eq!(utc_stamp_compact(0), "19700101T000000Z");
    assert_eq!(utc_stamp_rfc3339(0), "1970-01-01T00:00:00.000Z");
    // 2021-01-01T00:00:00Z + 1h1m1s
    assert_eq!(utc_stamp_compact(1_609_459_200_000), "20210101T000000Z");
    assert_eq!(utc_stamp_compact(1_609_459_200_000 + 3_661_000), "20210101T010101Z");
    assert_eq!(utc_stamp_rfc3339(1_609_459_200_000 + 3_661_123), "2021-01-01T01:01:01.123Z");
}

#[test]
fn fnv1a64_hex_is_stable_and_distinguishing() {
    // empty input → the FNV-1a64 offset basis, 16 hex digits.
    assert_eq!(fnv1a64_hex(b""), "cbf29ce484222325");
    assert_eq!(fnv1a64_hex(b"profile-a"), fnv1a64_hex(b"profile-a")); // deterministic
    assert_ne!(fnv1a64_hex(b"profile-a"), fnv1a64_hex(b"profile-b")); // differs on change
    assert_eq!(fnv1a64_hex(b"x").len(), 16); // always 16 hex digits
}

// ── manifest assembly ────────────────────────────────────────────────────────────────────

fn build_info() -> GitBuildInfo {
    GitBuildInfo {
        pkg_name: "vike-run".to_string(),
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
        ("BINANCE_LIVE_API_SECRET".to_string(), "supersecretvalue123".to_string()),
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

    // Serialized artifact must never carry the full secret, must carry the mask + the flag.
    let json = serde_json::to_string(&m).expect("manifest serializes");
    assert!(!json.contains("supersecretvalue123"), "full secret reached the manifest: {json}");
    assert!(json.contains("***e123"), "masked fingerprint missing: {json}");
    assert!(json.contains("VIKE_RECONCILE"), "plain flag missing: {json}");

    // env sorted: the credential key precedes the VIKE flag.
    let ib = m.env.iter().position(|(k, _)| k == "BINANCE_LIVE_API_SECRET").unwrap();
    let iv = m.env.iter().position(|(k, _)| k == "VIKE_RECONCILE").unwrap();
    assert!(ib < iv, "env must be sorted by key");

    // round-trips back through serde (the artifact is machine-readable).
    let back: IncidentManifest = serde_json::from_str(&json).expect("manifest round-trips");
    assert_eq!(back.window.cutoff_ms, m.window.cutoff_ms);
}

// ── end-to-end orchestration (temp dir, injected clock + env; no real secret on disk) ─────

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
        profile_path: None,
    };
    // Injected env carries a secret; injected clock is ahead of the just-written files.
    let env = vec![
        ("VIKE_JOURNAL_DIR".to_string(), jdir.display().to_string()),
        ("BINANCE_LIVE_API_SECRET".to_string(), "supersecretvalue123".to_string()),
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
    // The OFF/degenerate path: a zero-length window selects no evidence, so the collector
    // freezes nothing and requires nothing from the live system — yet still emits a valid,
    // secret-free manifest. Nothing about the trading node is read for state or altered.
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
        profile_path: None,
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
