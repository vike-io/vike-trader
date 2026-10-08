//! The `rhai = "<path>"` strategy spelling (decision 0024).

use super::*;

// ---------------------------------------------------------------------------------------------
// The `rhai = "<path>"` strategy spelling (docs/decisions/0024-rhai-strategies-live.md).
// ---------------------------------------------------------------------------------------------

/// A profile-shaped rhai TOML over a hyperliquid mount — the same base `strategy_profile` uses.
fn rhai_profile_toml(rhai_line: &str, params: &str) -> String {
    format!("venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\n{rhai_line}\n{params}")
}

/// A script file this test owns, under a pid-keyed temp dir (the `own_sentinel` idiom) — the
/// unit tests here never touch a checkout path.
/// ⚠ Returns the `TempDir` ALONGSIDE the path, and the caller must bind it — dropping it
/// deletes the script the path points at.
///
/// This used to be `temp_dir().join(format!("…-{}", process::id()))`, which satisfies
/// `crates/vike-ops/tests/hygiene/temp_path_gate.rs` (the name is not fixed) and was still wrong twice
/// over. MEASURED on the CI box, 2026-08-25:
///
/// * **1,725 of these directories were sitting in `/tmp`**, dating back to 2026-08-18 — 1,082
///   owned by `the CI user` and 643 by `the operator`. Nothing ever deleted one, so every CI run and
///   every lane run leaked a directory permanently.
/// * **A PID is REUSED.** When one collides with a directory the OTHER user created, the
///   `create_dir_all` succeeds (it already exists) and the `fs::write` fails with
///   PermissionDenied. That is a live, intermittent CI flake, and it is exactly the failure
///   `temp_path_gate`'s own message describes — *"whichever creates that directory first owns
///   it and every later run under the other user fails"* — arriving through the very idiom
///   that gate suggests as the remedy. PID-uniquification prevents collision WITHIN a run; it
///   does not prevent collision ACROSS users over time, and it leaks either way.
///
/// `tempfile::TempDir` fixes both halves at once: unique by construction, and self-deleting.
fn own_script(name: &str, source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp script dir");
    let path = dir.path().join(format!("{name}.rhai"));
    std::fs::write(&path, source).expect("write script");
    (dir, path)
}

/// TOML-safe spelling of a path (backslashes escaped — this suite runs on the Windows dev box
/// as well as the Linux runners).
fn toml_path(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "\\\\")
}

#[test]
fn a_rhai_profile_parses_and_validates() {
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "rhai = \"strategies/thing.rhai\"",
        "[strategy.params]\nsize = 2.0\n",
    ))
    .expect("a rhai profile parses and validates without touching the filesystem");
    assert_eq!(p.strategy_name(), "rhai");
    // The maker path must NOT claim it: a script is not the A-S maker.
    assert!(p.mounted_maker(&p.to_mount_config()).is_none());
}

#[test]
fn a_strategy_table_with_both_name_and_rhai_is_refused() {
    let err = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "name = \"grid\"\nrhai = \"thing.rhai\"",
        "",
    ))
    .unwrap_err();
    assert!(err.contains("not both"), "names the conflict: {err}");
}

#[test]
fn a_strategy_table_with_neither_name_nor_rhai_is_refused() {
    // With params AND without: a table that selects nothing must fail either way, with a
    // message naming both spellings.
    for params in ["", "[strategy.params]\nsize = 2.0\n"] {
        let err = DaemonProfile::from_toml_str(&rhai_profile_toml("", params)).unwrap_err();
        assert!(err.contains("`name"), "names the name spelling: {err}");
        assert!(err.contains("`rhai"), "names the script spelling: {err}");
    }
}

#[test]
fn an_empty_rhai_path_is_refused() {
    let err = DaemonProfile::from_toml_str(&rhai_profile_toml("rhai = \"\"", "")).unwrap_err();
    assert!(err.contains("script file"), "names what is missing: {err}");
}

/// The script arm keeps the daemon's no-silently-ignored-key posture at the VALUE level: a
/// non-numeric override can never apply (`param` takes an f64), so it is refused at LOAD.
#[test]
fn a_non_numeric_rhai_param_is_refused_at_load() {
    let err = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "rhai = \"thing.rhai\"",
        "[strategy.params]\nsize = \"2\"\n",
    ))
    .unwrap_err();
    assert!(err.contains("size"), "names the offending key: {err}");
    assert!(err.contains("NUMBER"), "states the requirement: {err}");
}

/// `name = "rhai"` is redirected to the path spelling rather than refused as simulator-only —
/// the message an operator acts on after the 0024 reversal.
#[test]
fn name_rhai_is_redirected_to_the_path_spelling() {
    let err = strategy_profile("rhai").unwrap_err();
    assert!(err.contains("rhai = "), "points at the path spelling: {err}");
    assert!(err.contains("0024"), "cites the decision record: {err}");
}

/// The resolve reads the file, and a missing one fails NAMING THE PATH — before any core
/// spawns, same as every other resolve failure.
#[test]
fn a_missing_script_file_is_a_resolve_error_naming_the_path() {
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "rhai = \"no-such-dir/no-such-script.rhai\"",
        "",
    ))
    .expect("validates — the file is read at resolve, not at load");
    let err = match p.resolve_mount(&p.to_mount_config()) {
        Err(e) => e,
        Ok(_) => panic!("a missing script file must fail the resolve"),
    };
    assert!(err.contains("no-such-script.rhai"), "names the path: {err}");
}

/// The resolve refuses an override the script's own top level never asks for — the script-arm
/// twin of `an_unread_params_key_is_refused_with_the_readable_set`, keyed on
/// `vike_script::discover_params`.
#[test]
fn a_rhai_override_the_script_never_asks_for_is_refused_at_resolve() {
    let (_tmp, path) = own_script(
        "declares-size",
        "const SIZE = param(\"size\", 1.0);\nfn on_bar() { if position() == 0.0 { \
             buy(SIZE); } }\n",
    );
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        &format!("rhai = \"{}\"", toml_path(&path)),
        "[strategy.params]\nsizee = 2.0\n",
    ))
    .expect("validates — key names are checked at resolve, against the script text");
    let err = match p.resolve_mount(&p.to_mount_config()) {
        Err(e) => e,
        Ok(_) => panic!("an unasked-for override must fail the resolve"),
    };
    assert!(err.contains("sizee"), "names the offender: {err}");
    assert!(err.contains("size (default 1)"), "names the script's own knobs: {err}");
}

/// The happy path: a script resolves into the SAME `MountedStrategy` seam a registry name
/// does, and the `Script` variant carries the audit pair — the path as the profile spelled it
/// and the sha256 of the source that was actually read — so the INFO audit line's claim is
/// assertable without a log subscriber.
#[test]
fn a_rhai_profile_resolves_to_a_script_mount_carrying_the_audit_hash() {
    let source = "const SIZE = param(\"size\", 1.0);\nfn on_bar() { if position() == 0.0 { \
                      buy(SIZE); } }\n";
    let (_tmp, path) = own_script("resolves", source);
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        &format!("rhai = \"{}\"", toml_path(&path)),
        "[strategy.params]\nsize = 3.0\n",
    ))
    .expect("validates");
    match p.resolve_mount(&p.to_mount_config()).expect("the script compiles and mounts") {
        MountedStrategy::Script { path: got_path, sha256, .. } => {
            assert_eq!(got_path, path.display().to_string());
            assert_eq!(sha256, script_sha256(source), "the hash is of the source read");
        }
        MountedStrategy::AsMaker(_) | MountedStrategy::Registered(_) => {
            panic!("a rhai profile must resolve through the Script arm")
        }
    }
    // ...and the boxing wrapper `main` calls accepts it like any other strategy.
    assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
}

/// [`script_sha256`] against the NIST SHA-256 test vector for "abc" — the pure half of the
/// audit line, pinned to a value computed outside this codebase.
#[test]
fn script_sha256_matches_the_known_vector() {
    assert_eq!(
        script_sha256("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// The effective-params line reports the script path plus the overrides that DID land (the
/// resolve refuses any other kind), never the raw table.
#[test]
fn the_rhai_effective_params_line_reports_path_and_overrides() {
    let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
        "rhai = \"thing.rhai\"",
        "[strategy.params]\nsize = 3.0\n",
    ))
    .expect("validates");
    let line = p.effective_params(&p.to_mount_config());
    assert!(line.contains("script=thing.rhai"), "names the script: {line}");
    assert!(line.contains("size=3"), "names the override: {line}");

    let bare = DaemonProfile::from_toml_str(&rhai_profile_toml("rhai = \"thing.rhai\"", ""))
        .expect("validates");
    let line = bare.effective_params(&bare.to_mount_config());
    assert!(
        line.contains("no overrides"),
        "an override-free mount says so rather than claiming knobs: {line}"
    );
}

/// `validate_for_live` is strategy-agnostic and a rhai profile rides it unchanged: the wired
/// hyperliquid/BTC pair passes, a foreign symbol on the same venue is refused — the same
/// verdicts a named-strategy profile gets.
#[test]
fn a_rhai_profile_gets_the_same_live_verdicts_as_a_named_one() {
    let ok = DaemonProfile::from_toml_str(&rhai_profile_toml("rhai = \"thing.rhai\"", ""))
        .expect("validates");
    assert!(ok.validate_for_live().is_ok(), "hyperliquid/BTC is a live-wired pair");

    let foreign = DaemonProfile::from_toml_str(
        "venue = \"hyperliquid\"\nsymbol = \"NOPE\"\n[strategy]\nrhai = \"thing.rhai\"\n",
    )
    .expect("validates");
    assert!(foreign.validate_for_live().is_err(), "a foreign symbol is still refused");
}
