//! `vike-datahub store <verb>` through the SHIPPED binary, over a REAL scratch store.
//!
//! The store is the only copy of the data, so these assert on the BYTES of the tree, and every command
//! names `--store` under a temp directory or runs in a [`Scratch`] that pins the walk: `store rm` with
//! no `--store` resolves a root off the working directory, and no test here may ever reach a real one.
//! A child that inherited `cargo test`'s working directory would resolve the CHECKOUT, which is why
//! every spawn goes through [`Scratch::tool`].

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vike_paths::state_path::{PROJECT_SETTINGS_DIR, SETTINGS_DIR_ENV};
use vike_store::HistoryStoreDataFusion;

mod argv_pins;
mod export;
mod rename;
mod repair;
mod store_root;
mod verbs;

/// An address nothing listens on (port 1 is `tcpmux`, never served here): a verb that reached for
/// history by accident fails loudly instead of dialling `127.0.0.1:7878`, which on a the CI box lane is the
/// box's REAL datahub.
const NO_DATAHUB: &str = "127.0.0.1:1";

/// The retired variables (decision 0111), REMOVED from every child so a box that still exports one
/// cannot turn a test into a startup refusal. Composed, so the registry's literal sweep reads no
/// fixture here.
const RETIRED_VARS: [&str; 3] = [
    concat!("VIKE", "_HIST_STORE"),
    concat!("VIKE", "_DATAHUB_STORE"),
    concat!("VIKE", "_LOG_FILE_LEVEL"),
];

/// A throwaway project to run the tool in, removed with everything the tool wrote into it.
struct Scratch {
    dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Scratch {
    /// A `settings/` marker, so the child's walk resolves HERE, and the settings database with the
    /// `config.datahub_addr` row at [`NO_DATAHUB`] and the file log off.
    fn project() -> Self {
        let dir = tempfile::tempdir().expect("a scratch project");
        let settings = dir.path().join(PROJECT_SETTINGS_DIR);
        std::fs::create_dir_all(&settings).expect("the project marker");
        let scratch = Scratch { dir, settings };
        scratch.dial(NO_DATAHUB);
        scratch
    }

    /// Point the child at the datahub `hub`: the row a reader dials. One plant, which replaces the
    /// whole settings table.
    fn dial(&self, hub: &str) {
        self.plant(hub, None, None);
    }

    /// [`Self::dial`] at [`NO_DATAHUB`], with the `config.store_root` row naming `root`: the tree the
    /// daemon of this project would serve.
    fn row_names_store(&self, root: &Path) {
        self.plant(NO_DATAHUB, Some(root), None);
    }

    /// [`Self::row_names_store`], with this project's daemon listening at `bind`
    /// (`config.datahub_bind_addr`).
    fn serves_row_store(&self, root: &Path, bind: &str) {
        self.plant(NO_DATAHUB, Some(root), Some(bind));
    }

    fn plant(&self, hub: &str, store_root: Option<&Path>, bind: Option<&str>) {
        let row = |section: &str, key: &str, value: String| vike_secrets::SettingRow {
            section: section.to_string(),
            key: key.to_string(),
            value,
        };
        let mut settings = vec![
            row("config", "datahub_addr", serde_json::Value::from(hub).to_string()),
            row("preferences", "log_file_level", "\"off\"".to_string()),
        ];
        if let Some(root) = store_root {
            let root = root.to_str().expect("a UTF-8 path");
            settings.push(row("config", "store_root", serde_json::Value::from(root).to_string()));
        }
        if let Some(bind) = bind {
            settings.push(row(
                "config",
                "datahub_bind_addr",
                serde_json::Value::from(bind).to_string(),
            ));
        }
        vike_secrets::plant_settings_rows(
            &self.settings,
            &vike_secrets::StoredSettings { settings, ..Default::default() },
        )
        .unwrap_or_else(|e| panic!("plant the settings rows: {e}"));
    }

    /// The shipped binary as a `Command`, pinned to this scratch. Add the argv.
    fn tool(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-datahub"));
        cmd.current_dir(self.dir.path()).env(SETTINGS_DIR_ENV, &self.settings);
        for retired in RETIRED_VARS {
            cmd.env_remove(retired);
        }
        cmd
    }
}

/// Run the tool in a fresh scratch project that is gone when this returns, dialling nothing that
/// listens. `stdin` is closed, which is the shape a CI step and a systemd unit have.
fn run(args: &[&str]) -> Output {
    run_via(NO_DATAHUB, args)
}

/// [`run`], with the child's history read from the datahub at `hub`.
fn run_via(hub: &str, args: &[&str]) -> Output {
    let scratch = Scratch::project();
    scratch.dial(hub);
    scratch.tool().args(args).output().unwrap_or_else(|e| panic!("run vike-datahub {args:?}: {e}"))
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `args` must exit 2 with a one-flag refusal on stderr naming every needle, and print nothing on
/// stdout. The usage block is what a refusal must NOT dump: every needle appears somewhere in it, so a
/// refusal that does makes this assertion unfalsifiable.
fn refuses(args: &[&str], needles: &[&str]) {
    refuses_via(NO_DATAHUB, args, needles);
}

/// [`refuses`], with the child pointed at `hub`: a witness, in the tests whose subject is that a refusal
/// dials nothing.
fn refuses_via(hub: &str, args: &[&str], needles: &[&str]) {
    let out = run_via(hub, args);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "`{args:?}` must exit 2: {stderr:?}");
    assert!(out.stdout.is_empty(), "…and print nothing on stdout: {:?}", stdout_of(&out));
    assert!(!stderr.contains("usage:"), "`{args:?}` dumped the usage block: {stderr:?}");
    for needle in needles {
        assert!(stderr.contains(needle), "`{args:?}` must name {needle:?}: {stderr:?}");
    }
}

/// Write the TEST tape (`vike_data::demo`, behind `test-support`) into the store at `root`. No shipped
/// verb writes it since the owner's ruling of 2026-11-13 (no demo or fake data in the product), so
/// the suites seed it through the library.
fn seed_test_tape(root: &Path) {
    let store = HistoryStoreDataFusion::open(root).expect("open the scratch store");
    let seeded = vike_data::demo::seed(&store).expect("seed the test tape");
    assert!(seeded.iter().any(|s| s.rows > 0), "the test tape wrote nothing");
}

/// A scratch directory holding a store the test tape was written into: `(guard, root, root as argv)`.
fn seeded() -> (tempfile::TempDir, PathBuf, String) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let root = dir.path().join("hist");
    let arg = root.to_str().expect("a UTF-8 path").to_string();
    seed_test_tape(&root);
    (dir, root, arg)
}

/// The test tape's `demo/BTCUSDT/1h` series under `root`.
fn series_dir(root: &Path) -> PathBuf {
    root.join("kind=bar").join("venue=demo").join("symbol=BTCUSDT").join("interval=1h")
}
