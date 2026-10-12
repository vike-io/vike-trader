//! **The rolling file's level for a root that runs no `vike_boot::boot`**: the one
//! `preferences.log_file_level` row such a tool reads before `vike_log::init`, best-effort.
//!
//! The level is a row (`docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md`,
//! verdict 3), and a booted root reads it as part of `vike_boot::Booted::settings`. A batch tool (the
//! `vike-backfill` bins, the `backtest` engine's one-shot arms) builds its subscriber without
//! booting, so without this read its rolling file could only ever honour its compiled default.
//! [`log_file_level`] reads that row over the SAME resolution `vike_boot::boot` performs
//! (`$VIKE_SETTINGS_DIR`, else the walk from the working directory) and through the same loader,
//! and returns the log home off the same answer, so the level and the file it governs cannot come
//! from two projects.
//!
//! **Best-effort, never a new requirement.** No project, no database, or a database with no such
//! row is [`LogFileLevel::level`] `None`, and the tool keeps its compiled default in silence. A
//! database that will not open, or one that opened and said something illegal, is `None` too, with
//! [`LogFileLevel::unread`] saying why: a tool that wanted only its log level must neither stop over
//! a broken store nor stay silent about it. Like the rest of this crate it logs and prints nothing;
//! the root prints [`LogFileLevel::unread_line`] on stderr, because no subscriber exists yet.
//!
//! ⚠ **The one module here that walks for the project and opens the database.** The loader still
//! never does (this is the binary-side read the loader's contract assigns to the binary, packaged
//! for a root that runs no `vike_boot::boot`), and the inputs are still PARAMETERS: the caller hands
//! over its own `std::env::vars()` sweep and working directory, and this module reads no process
//! environment of its own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The row this module reads, as `vike_config::setting_keys` spells it.
const KEY: &str = "preferences.log_file_level";

/// What [`log_file_level`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFileLevel {
    /// The `preferences.log_file_level` row, or `None` when no row names it or the store could not
    /// answer. `None` means "keep your compiled default" ([`LogFileLevel::level_or`]).
    pub level: Option<String>,
    /// `<settings dir>/state/logs` off the same resolution, `None` when no project resolved. The
    /// `vike_boot::Booted::log_home` twin; it goes into `vike_log::LogConfig::project_dir`.
    pub log_home: Option<PathBuf>,
    /// Why a store that is there could not answer, `None` when nothing went wrong.
    pub unread: Option<String>,
}

impl LogFileLevel {
    /// The level to hand `vike_log::LogConfig::file_level`: the row, else `compiled`.
    #[must_use]
    pub fn level_or(&self, compiled: &str) -> String {
        self.level.clone().unwrap_or_else(|| compiled.to_string())
    }

    /// The one stderr line a root prints before its subscriber exists when the store could not
    /// answer, naming the default it fell back to. `None` when there is nothing to say.
    #[must_use]
    pub fn unread_line(&self, compiled: &str) -> Option<String> {
        self.unread.as_ref().map(|why| {
            format!(
                "the `{KEY}` row could not be read, so the rolling log keeps its compiled default \
                 `{compiled}`: {why}"
            )
        })
    }
}

/// **Read the `preferences.log_file_level` row for a root that runs no `vike_boot::boot`.**
///
/// `env` is the root's own `std::env::vars()` sweep and `cwd` its working directory, the two inputs
/// `vike_boot::BootSpec` takes for the same resolution. Read-only: the store is opened by flag and
/// never created. See this module's doc for what each outcome means.
///
/// The override is `$VIKE_SETTINGS_DIR` through `vike_model::paths::state_path::SETTINGS_DIR_ENV`,
/// the variable's one named home. A blank or padded value needs no handling here:
/// `vike_secrets::project_settings_dir_for` trims it and treats a blank one as absent, the same
/// answer `vike_boot::boot` reaches over its own spelling of the lookup.
#[must_use]
pub fn log_file_level(env: &HashMap<String, String>, cwd: Option<&Path>) -> LogFileLevel {
    let override_dir = env.get(vike_model::paths::state_path::SETTINGS_DIR_ENV).map(String::as_str);
    let Some(settings_dir) = vike_secrets::project_settings_dir_for(override_dir, cwd) else {
        return LogFileLevel::default();
    };
    let log_home = Some(
        settings_dir
            .join(vike_model::paths::state_path::STATE_SUBDIR)
            .join(vike_model::paths::state_path::LOGS_SUBDIR),
    );
    let read = vike_secrets::read_settings_in(&settings_dir);
    let mut refusal = String::new();
    let source = crate::StoreLayer::of(Some(&read), &mut refusal);
    let (level, unread) = match crate::describe_with_source(Some(&settings_dir), source) {
        Ok(described) => row_of(&described),
        Err(e) => (None, Some(e.to_string())),
    };
    LogFileLevel { level, log_home, unread }
}

/// The row out of one description, or why there is none to trust.
///
/// A store that could not be read, or that opened and said something illegal (the seal), answers
/// nothing here: its values are compiled-in defaults or rows nobody can vouch for, and a level is
/// not worth guessing at.
fn row_of(described: &crate::Description) -> (Option<String>, Option<String>) {
    let refusal = described.store_refusal.as_ref().or(described.settings.seal_refusal.as_ref());
    if let Some(why) = refusal {
        return (None, Some(why.clone()));
    }
    let level = described
        .rows
        .iter()
        .find(|row| row.key == KEY && row.origin == crate::Origin::Db)
        .and_then(|row| row.value.clone());
    (level, None)
}

#[path = "file_level_tests.rs"]
#[cfg(test)]
mod file_level_tests;
