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
//! * **`VIKE_USER_DATA_DIR` and `VIKE_SETTINGS_DIR`** (a project scratch only) — named outright, so
//!   the runs directory and the settings do not depend on the walk at all. Not idle: a `settings/`
//!   marker does NOT outrank a `[workspace]` manifest above it (the walk ranks by the strength of the
//!   evidence, and `crates/vike-model/src/paths/state_path.rs`'s `project_settings_dir` names a
//!   deployment "installed INSIDE a checkout" as its one accepted residual), so a scratch created
//!   INSIDE a checkout — a `TMPDIR` pointed into one — would resolve the checkout again while
//!   every assertion here still read the scratch.
//! * **The same two REMOVED** for [`Scratch::outside_any_project`], whose subject is a run with
//!   nowhere to persist — a value inherited from the box would hand it one.
//! * **`VIKE_HIST_STORE` removed** — the box's own store must not answer a default the test never
//!   named.
//! * **`VIKE_LOG_FILE_LEVEL=off`** — the file log layer defaults to `trace`, and nothing here wants a
//!   JSON log, in the scratch or anywhere else.
//! * **The datahub address defaulted to [`NO_DATAHUB`]** — a test that reached for history without
//!   naming a hub fails loudly instead of dialling `127.0.0.1:7878`, which on a the CI box lane is the
//!   box's REAL datahub. A caller that serves one (`local_datahub.rs`) overrides it with a later
//!   `.env`, which `Command` honours.
//!
//! The variable names are the owning crates' constants rather than literals, so a rename there is a
//! compile error here rather than a pin that silently stops pinning.
//!
//! Included with `#[path]` by each test binary that spawns the engine (`help_cli.rs`,
//! `optimizer_cli.rs`, `search_persist_cli.rs`): a file under `tests/support/` is no test target of
//! its own. Each includer uses a different slice of it, hence the `dead_code` allowance below — the
//! established shape for a shared test module (`crates/vike-cli/tests/common/mod.rs` carries the
//! same one).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use vike_config::config::STORE_ROOT_ENV;
use vike_config::preferences::LOG_FILE_LEVEL_ENV;
use vike_datahub_client::route::DATAHUB_ADDR_ENV;
use vike_model::paths::state_path::{
    PROJECT_SETTINGS_DIR, PROJECT_USER_DATA_DIR, RUNS_SUBDIR, SETTINGS_DIR_ENV, USER_DATA_DIR_ENV,
};

/// An address nothing listens on (port 1 is `tcpmux`, never served here) — what every spawn dials
/// unless its caller names a hub of its own. See this module's doc for why the default would be the
/// wrong answer.
pub const NO_DATAHUB: &str = "127.0.0.1:1";

/// A throwaway directory to run the engine in, removed with everything the engine wrote into it
/// when this is dropped.
pub struct Scratch {
    dir: tempfile::TempDir,
    /// Whether this scratch is a PROJECT (it carries the `settings/` marker and the engine is told
    /// so) or deliberately outside one.
    project: bool,
}

impl Scratch {
    /// A scratch PROJECT: a `settings/` marker — the self-describing marker
    /// `vike_model::paths::state_path::project_settings_dir` answers with at the nearest level — so the
    /// engine resolves its project, and therefore its runs directory, HERE.
    ///
    /// `tempfile`, never a fixed name under the system temp directory —
    /// `crates/vike-ops/tests/temp_path_gate.rs` says why.
    pub fn project() -> Self {
        let dir = tempfile::tempdir().expect("a scratch project");
        std::fs::create_dir_all(dir.path().join(PROJECT_SETTINGS_DIR)).expect("the project marker");
        Scratch { dir, project: true }
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
        Scratch { dir, project: false }
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
            .env_remove(STORE_ROOT_ENV)
            .env(LOG_FILE_LEVEL_ENV, "off")
            .env(DATAHUB_ADDR_ENV, NO_DATAHUB);
        if self.project {
            cmd.env(SETTINGS_DIR_ENV, self.path().join(PROJECT_SETTINGS_DIR))
                .env(USER_DATA_DIR_ENV, self.path().join(PROJECT_USER_DATA_DIR));
        } else {
            cmd.env_remove(SETTINGS_DIR_ENV).env_remove(USER_DATA_DIR_ENV);
        }
        cmd
    }
}
