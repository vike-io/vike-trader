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
    MountRow, ProfileKind, ProfileOperatorWrite, ProfileRecord, ProfileRow, clear_active,
    set_active, store_profile,
};

use crate::temp_project;

/// The one-mount polymarket body every case starts from: a symbol and a strategy file (the check
/// is the LOAD, which reads no file, so the file need not exist).
fn mount() -> MountRow {
    let mut m = MountRow::new(0, "polymarket", "PredictionMarket");
    m.symbol = Some("TOK".to_string());
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
    spawn(root, &["--check-profile"])
}

/// The shipped `vike-tradehub` with `args`, against the store under `root`.
fn spawn(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
        .args(args)
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
        .expect("run vike-tradehub")
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
        Some(i32::from(vike_utility::cli::OPERATOR_NEEDED_EXIT)),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Store `rows` as the settings of the ACTIVE daemon profile `name` (the `mount()` body) in a fresh
/// store, and, when `run` is given, a body of the RETIRED `run` kind holding those `run` rows, ACTIVE.
fn seed_one_live_profile(settings: &Path, name: &str, rows: &[(&str, &str)], run: &[(&str, &str)]) {
    seed(settings, name, mount(), BTreeMap::new());
    let db = vike_secrets::db_path_in(settings);
    let write = ProfileOperatorWrite::claim("vike-tradehub check-profile test");
    let body = |kind, name: &str, rows: &[(&str, &str)], mounts| ProfileRecord {
        row: ProfileRow { name: name.to_string(), kind, active: false, note: None },
        mounts,
        params: BTreeMap::new(),
        settings: rows.iter().map(|(p, v)| ((*p).to_string(), (*v).to_string())).collect(),
        recorder: None,
    };
    let daemon = body(ProfileKind::Daemon, name, rows, vec![mount()]);
    store_profile(&db, &daemon, &write, 0, vike_model::AssetClass::SQL_WORDS).expect("store");
    if !run.is_empty() {
        let retired = body(ProfileKind::Run, "run-live", run, Vec::new());
        store_profile(&db, &retired, &write, 0, vike_model::AssetClass::SQL_WORDS).expect("store");
        set_active(&db, ProfileKind::Run, "run-live", &write, 0, vike_model::AssetClass::SQL_WORDS)
            .expect("activate");
    }
}

/// **Both parts of the one live profile are judged (decision 0297).** A daemon body whose RUN part
/// loads passes and says so; one whose run part the loader refuses (`max_leverage < 1.0`) is exit 1
/// in the loader's words.
#[test]
fn the_run_part_of_the_active_daemon_profile_is_judged_too() {
    let root = temp_project::temp_dir("check_profile_run_part");
    let ceilings = [("risk.max_notional_per_order", "100.0"), ("risk.max_total_exposure", "500.0")];
    seed_one_live_profile(&root.path().join("settings"), "tradehub", &ceilings, &[]);
    let out = check(root.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("`tradehub` loads with its run part"), "{stdout}");

    let root = temp_project::temp_dir("check_profile_bad_run_part");
    let bad = [ceilings[0], ceilings[1], ("risk.max_leverage", "0.5")];
    seed_one_live_profile(&root.path().join("settings"), "tradehub", &bad, &[]);
    let out = check(root.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "refused is exit 1: {stderr}");
    assert!(stderr.contains("bad run part of profile row `tradehub`"), "{stderr}");
    assert!(stderr.contains("max_leverage"), "the loader's own words: {stderr}");
}

/// **THE FENCE (decision 0297)**: a START refuses with the operator-needed exit (78) while a row of
/// the retired `run` kind is ACTIVE, printing the fold line built from that row's own values and the
/// deselect, before any mount is resolved. `--check-profile` judges the live profile only (`vike-cli
/// config check` warns about the row), so it still passes.
#[test]
fn an_active_retired_run_row_parks_the_start_naming_the_fold_line() {
    let root = temp_project::temp_dir("check_profile_retired_run");
    let run =
        [("risk.max_notional_per_order", "100000.0"), ("risk.max_total_exposure", "1000000.0")];
    seed_one_live_profile(&root.path().join("settings"), "tradehub", &[], &run);

    let out = spawn(root.path(), &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(i32::from(vike_utility::cli::OPERATOR_NEEDED_EXIT)),
        "{stderr}"
    );
    assert!(stderr.contains("the retired `run` profile `run-live` is still ACTIVE"), "{stderr}");
    assert!(
        stderr.contains(
            "vike-cli config profile create run tradehub --risk.max_notional_per_order 100000.0 \
             --risk.max_total_exposure 1000000.0"
        ),
        "the fold line, from the row's own values: {stderr}"
    );
    assert!(stderr.contains("vike-cli config profile deactivate run"), "{stderr}");
    assert!(!stderr.contains("strategy resolve failed"), "refused before a mount: {stderr}");

    assert_eq!(check(root.path()).status.code(), Some(0), "the check judges the live profile");

    // Deselected, the row is the rollback mirror the CLI keeps (inactive), and the start passes the
    // fence: whatever stops this bare project next, it is not the retired row.
    let db = vike_secrets::db_path_in(&root.path().join("settings"));
    let write = ProfileOperatorWrite::claim("vike-tradehub check-profile test");
    clear_active(&db, ProfileKind::Run, &write, 1, vike_model::AssetClass::SQL_WORDS)
        .expect("deselect");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
        .current_dir(root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("VIKE_SETTINGS_DIR", root.path().join("settings"))
        .env("VIKE_LOG_DIR", root.path().join("logs"))
        .env_remove("VIKE_STATE_ROOT")
        .env_remove("VIKE_TRADEHUB_LIVE")
        .env_remove("RUST_LOG")
        .spawn()
        .expect("start vike-tradehub");
    // Bounded: past the fence a start may go on to serve, so it is stopped after a few seconds.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while child.try_wait().expect("poll").is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let out = child.wait_with_output().expect("collect");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("retired `run` profile"), "an inactive row is not refused: {stderr}");
}
