//! THE ONE WAY a test in this crate spawns the shipped `backtest` engine: inside a SCRATCH directory
//! the test owns, never inside the checkout it was built in.
//!
//! ⚠ **Why this is a helper and not a habit.** The engine persists every finished run and every
//! search under `<project>/user_data/runs/`, and it finds `<project>` by walking UP from its WORKING
//! DIRECTORY. A child spawned with no `current_dir` inherits the test process's, which under
//! `cargo test` is this crate's directory — so the walk found the checkout's `[workspace]` manifest
//! and every run the test made landed in `<checkout>/user_data/runs`, at exit 0, in a test that
//! passed. `optimizer_cli.rs`'s `run_via` did exactly that, and so did the `run` before it: on
//! 2026-09-26 every verification lane on both boxes held hundreds to thousands of run directories,
//! and the profile each manifest names attributed them to that file's fixtures almost to a run.
//! `search_persist_cli.rs` had pinned its child's directory all along — the file beside it had not,
//! and nothing said it must.
//!
//! So every spawn goes through [`Scratch::engine`], and `search_persist_cli.rs`'s
//! `every_engine_spawn_goes_through_the_scratch_helper` fails a test file that names the engine
//! binary anywhere else.
//!
//! What [`Scratch::engine`] pins, and why each one:
//!
//! * **The working directory** — the scratch itself. For a [`Scratch::project`] the walk then
//!   resolves THIS directory, so the log directory, `<project>/tmp` and the settings land in it too,
//!   not only the runs.
//! * **`VIKE_SETTINGS_DIR`** — always a settings directory this scratch owns, holding the
//!   `config.datahub_addr` row the child dials (see the last bullet). For a project scratch it is
//!   `<scratch>/settings`; for [`Scratch::outside_any_project`] it is a SEPARATE throwaway one,
//!   which gives the run no project: its runs root is found by walking up from the working
//!   directory, which `VIKE_SETTINGS_DIR` does not move.
//! * **`VIKE_USER_DATA_DIR`** (a project scratch only) — named outright, so the runs directory
//!   does not depend on the walk at all. Not idle: a `settings/`
//!   marker does NOT outrank a `[workspace]` manifest above it (the walk ranks by the strength of the
//!   evidence, and `crates/vike-model/src/paths/state_path.rs`'s `project_settings_dir` names a
//!   deployment "installed INSIDE a checkout" as its one accepted residual), so a scratch created
//!   INSIDE a checkout — a `TMPDIR` pointed into one — would resolve the checkout again while
//!   every assertion here still read the scratch.
//! * **`VIKE_USER_DATA_DIR` REMOVED** for [`Scratch::outside_any_project`], whose subject is a run
//!   with nowhere to persist — a value inherited from the box would hand it one.
//! * **The retired store variables removed** — a box that still exports one would have the child
//!   refuse to start (decision 0111) rather than run the test.
//! * **`VIKE_LOG_FILE_LEVEL=off`** — the file log layer defaults to `trace`, and nothing here wants a
//!   JSON log, in the scratch or anywhere else.
//! * **The datahub address is the `config.datahub_addr` ROW, defaulted to [`NO_DATAHUB`]** — a test
//!   that reached for history without naming a hub fails loudly instead of dialling
//!   `127.0.0.1:7878`, which on a the CI box lane is the box's REAL datahub. A caller that serves one
//!   (`local_datahub.rs`) writes its address with [`Scratch::dial`] before spawning.
//!
//! Included with `#[path]` by each test binary that spawns the engine (`help_cli.rs`,
//! `optimizer_cli.rs`, `search_persist_cli.rs`): a file under `tests/support/` is no test target of
//! its own. Each includer uses a different slice of it, hence the `dead_code` allowance below — the
//! established shape for a shared test module (`crates/vike-cli/tests/common/mod.rs` carries the
//! same one).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use vike_model::paths::state_path::{
    PROJECT_SETTINGS_DIR, PROJECT_USER_DATA_DIR, RUNS_SUBDIR, SETTINGS_DIR_ENV, USER_DATA_DIR_ENV,
};

/// An address nothing listens on (port 1 is `tcpmux`, never served here) — what every spawn dials
/// unless its caller names a hub of its own. See this module's doc for why the default would be the
/// wrong answer.
pub const NO_DATAHUB: &str = "127.0.0.1:1";

/// The file log layer's level variable — `vike_log` reads it, and it is the one logging knob a
/// test hands a child (decision 0111 left it an environment variable: a tool opens no database).
const LOG_FILE_LEVEL_VAR: &str = "VIKE_LOG_FILE_LEVEL";

/// The retired store variables (decision 0111), REMOVED from every child so a box that still
/// exports one cannot turn a test into a startup refusal. Composed, so the registry's literal sweep
/// reads no fixture here.
const RETIRED_STORE_VARS: [&str; 2] =
    [concat!("VIKE", "_HIST_STORE"), concat!("VIKE", "_DATAHUB_STORE")];

/// Write `hub` as the `config.datahub_addr` row of the settings database under `settings` — ONE
/// plant, which replaces the whole settings table (nothing else is planted here).
fn plant_hub(settings: &Path, hub: &str) {
    vike_secrets::plant_settings_rows(
        settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "datahub_addr".to_string(),
                value: serde_json::Value::String(hub.to_string()).to_string(),
            }],
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| {
        panic!("plant config.datahub_addr = {hub} under {}: {e}", settings.display())
    });
}

/// A throwaway directory to run the engine in, removed with everything the engine wrote into it
/// when this is dropped.
pub struct Scratch {
    dir: tempfile::TempDir,
    /// Whether this scratch is a PROJECT (it carries the `settings/` marker and the engine is told
    /// so) or deliberately outside one.
    project: bool,
    /// The settings directory the child is pointed at — see the module doc's `VIKE_SETTINGS_DIR`
    /// bullet.
    settings: PathBuf,
    /// The separate throwaway that holds `settings` for a scratch OUTSIDE any project, kept so it
    /// lives as long as this scratch.
    _outside_settings: Option<tempfile::TempDir>,
}

impl Scratch {
    /// A scratch PROJECT: a `settings/` marker — the self-describing marker
    /// `vike_model::paths::state_path::project_settings_dir` answers with at the nearest level — so the
    /// engine resolves its project, and therefore its runs directory, HERE.
    ///
    /// `tempfile`, never a fixed name under the system temp directory —
    /// `crates/vike-ops/tests/hygiene/temp_path_gate.rs` says why.
    pub fn project() -> Self {
        let dir = tempfile::tempdir().expect("a scratch project");
        let settings = dir.path().join(PROJECT_SETTINGS_DIR);
        std::fs::create_dir_all(&settings).expect("the project marker");
        plant_hub(&settings, NO_DATAHUB);
        Scratch { dir, project: true, settings, _outside_settings: None }
    }

    /// A scratch that is NOT a project — no marker, and neither project variable handed down — for
    /// the tests whose subject is a run that has nowhere to persist.
    ///
    /// ⚠ "Nowhere" is the walk's answer from a `tempfile` directory, i.e. from the system temp root
    /// upward, which this test does not own. A `settings/` directory planted above it by somebody
    /// else would make it a project; that is the residual the tests using this accept, and they
    /// fail on their own `NOT saved` assertion rather than passing if it ever bites.
    pub fn outside_any_project() -> Self {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let outside = tempfile::tempdir().expect("a settings directory outside the scratch");
        let settings = outside.path().join(PROJECT_SETTINGS_DIR);
        std::fs::create_dir_all(&settings).expect("the settings directory");
        plant_hub(&settings, NO_DATAHUB);
        Scratch { dir, project: false, settings, _outside_settings: Some(outside) }
    }

    /// Point the child at the datahub `hub` — the `config.datahub_addr` row of this scratch's
    /// settings database. Call it before [`Scratch::engine`].
    pub fn dial(&self, hub: &str) {
        plant_hub(&self.settings, hub);
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `<scratch>/user_data/runs` — where a [`Scratch::project`]'s runs land.
    pub fn runs_root(&self) -> PathBuf {
        self.dir.path().join(PROJECT_USER_DATA_DIR).join(RUNS_SUBDIR)
    }

    /// Every run directory under [`Scratch::runs_root`], sorted — the listing a Studio Research tab
    /// would build, and what a flood would show up in.
    pub fn run_dirs(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(self.runs_root())
            .map(|it| it.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default();
        out.sort();
        out
    }

    /// The shipped `backtest` binary as a `Command`, pinned to this scratch — see the module doc for
    /// what is pinned and why. Add the argv, and a datahub address if the run reads history.
    pub fn engine(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_backtest"));
        cmd.current_dir(self.path())
            .env(LOG_FILE_LEVEL_VAR, "off")
            .env(SETTINGS_DIR_ENV, &self.settings);
        for retired in RETIRED_STORE_VARS {
            cmd.env_remove(retired);
        }
        if self.project {
            cmd.env(USER_DATA_DIR_ENV, self.path().join(PROJECT_USER_DATA_DIR));
        } else {
            cmd.env_remove(USER_DATA_DIR_ENV);
        }
        cmd
    }
}
