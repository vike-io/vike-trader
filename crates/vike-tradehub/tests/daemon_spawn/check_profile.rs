//! **`--check-profile` on the SHIPPED binary**: the ACTIVE daemon-profile row judged by the boot and
//! the loader a start runs (`crates/vike-tradehub/src/tradehub_cli/profile.rs`'s `check_profile`),
//! with nothing started and nothing written. It is what `vike-cli config check` spawns, so the exit
//! code and the stderr words are the contract that verb quotes.
//!
//! Each case seeds its own store through the profile writer (never through `ProfileDaemon`, which
//! would refuse the retired shape before it reached the store), runs the binary once, and reads only
//! that child's output.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use vike_secrets::profile_store::{
    MountRow, ProfileKind, ProfileOperatorWrite, ProfileRecord, ProfileRow, set_active,
    store_profile,
};

use crate::temp_project;

/// The one-mount polymarket body every case starts from: a `token_id` and a strategy file (the check
/// is the LOAD, which reads no file, so the file need not exist).
fn mount() -> MountRow {
    let mut m = MountRow::new(0, "polymarket", "PredictionMarket");
    m.token_id = Some("TOK".to_string());
    m.strategy_file = Some("market_making/spread_maker.rs".to_string());
    m
}

/// Store `mount` (with `params`) as the ACTIVE daemon profile `name`, in a fresh store.
fn seed(settings: &Path, name: &str, mount: MountRow, params: BTreeMap<(i64, String), String>) {
    let db = vike_secrets::db_path_in(settings);
    vike_secrets::create_empty_store_for_test(&db).expect("create an empty settings store");
    let stored = ProfileRecord {
        row: ProfileRow {
            name: name.to_string(),
            kind: ProfileKind::Daemon,
            active: false,
            note: None,
        },
        mounts: vec![mount],
        params,
        settings: BTreeMap::new(),
        recorder: None,
    };
    let write = ProfileOperatorWrite::claim("vike-tradehub check-profile test");
    store_profile(&db, &stored, &write, 0, vike_model::AssetClass::SQL_WORDS).expect("store");
    set_active(&db, ProfileKind::Daemon, name, &write, 0, vike_model::AssetClass::SQL_WORDS)
        .expect("activate");
}

/// Every file under `dir` with its bytes, in path order: the whole of what a run could have written.
fn snapshot(dir: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read the project") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).expect("read a file");
                out.push((path, bytes));
            }
        }
    }
    out.sort();
    out
}

/// `vike-tradehub --check-profile` against the store under `root`.
fn check(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
        .arg("--check-profile")
        .current_dir(root)
        .stdin(Stdio::null())
        .env("VIKE_SETTINGS_DIR", root.join("settings"))
        .env_remove("VIKE_STATE_ROOT")
        // A start would create its log here; a check must not.
        .env("VIKE_LOG_DIR", root.join("logs"))
        .env_remove("VIKE_LOG_FILE_LEVEL")
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_LIVE")
        .env_remove("VIKE_TRADEHUB_ADDR")
        .env_remove("VIKE_RECONCILE")
        .env_remove("RUST_LOG")
        .env_remove("VIKE_LOG")
        .output()
        .expect("run vike-tradehub --check-profile")
}

/// A row still carrying the retired top-level `qty` is REFUSED, exit 1, in the loader's own words —
/// and the check wrote nothing: the store's bytes, every other file and the log directory are as
/// they were.
#[test]
fn a_profile_with_a_retired_top_level_quantity_is_refused_in_the_daemons_words_and_nothing_is_written()
 {
    let root = temp_project::temp_dir("check_profile_retired");
    let mut m = mount();
    m.qty = Some(5.0);
    seed(&root.path().join("settings"), "old-shape", m, BTreeMap::new());
    let before = snapshot(root.path());

    let out = check(root.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "refused is exit 1: {stderr}");
    assert!(stderr.contains("bad profile row `old-shape`"), "names the profile: {stderr}");
    assert!(
        stderr.contains("are no longer profile fields (this profile sets: qty)"),
        "the loader's own refusal, verbatim: {stderr}"
    );
    assert!(out.stdout.is_empty(), "a refusal prints no verdict line on stdout");

    assert_eq!(snapshot(root.path()), before, "a check wrote into the project");
    assert!(!root.path().join("logs").exists(), "a check opened a log file");
}

/// The same knob under `[strategy.params]` loads: exit 0, one line on stdout naming the profile.
#[test]
fn the_same_knob_under_strategy_params_loads() {
    let root = temp_project::temp_dir("check_profile_params");
    let params = BTreeMap::from([((0, "qty".to_string()), "5.0".to_string())]);
    seed(&root.path().join("settings"), "new-shape", mount(), params);
    let before = snapshot(root.path());

    let out = check(root.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("the ACTIVE daemon profile `new-shape` loads"), "{stdout}");
    assert_eq!(snapshot(root.path()), before, "a check wrote into the project");
}

/// A row naming NO strategy file is REFUSED, exit 1, naming the folder a file is read from: no
/// strategy is compiled into the daemon, so there is no default to mount (a row whose retired
/// `strategy_name` column is all it says is this row to the loader, which no longer reads that column).
#[test]
fn a_profile_naming_no_strategy_file_is_refused_naming_the_folder() {
    let root = temp_project::temp_dir("check_profile_no_file");
    let mut m = mount();
    m.strategy_file = None;
    seed(&root.path().join("settings"), "no-file", m, BTreeMap::new());

    let out = check(root.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "refused is exit 1: {stderr}");
    assert!(stderr.contains("bad profile row `no-file`"), "names the profile: {stderr}");
    assert!(stderr.contains("names no strategy file"), "says what is missing: {stderr}");
    assert!(stderr.contains("`strategies/` folder"), "names the folder: {stderr}");
}

/// No ACTIVE row is the daemon's own setup-needed exit (78), never a pass.
#[test]
fn no_active_row_is_the_setup_needed_exit() {
    let root = temp_project::temp_dir("check_profile_none");
    let db = vike_secrets::db_path_in(&root.path().join("settings"));
    vike_secrets::create_empty_store_for_test(&db).expect("create an empty settings store");
    let out = check(root.path());
    assert_eq!(
        out.status.code(),
        Some(i32::from(vike_tradehub::late_arm::EXIT_OPERATOR_NEEDED)),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
