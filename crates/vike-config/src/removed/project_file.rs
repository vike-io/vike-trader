//! The removed per-project override FILE, `<project>/vike.toml`: its name, the probe and the refusal.

use std::path::Path;

use crate::error::ConfigError;

/// The per-project override file that USED to sit above `<project>/settings/*.toml`.
///
/// Kept as a named constant even though nothing loads it any more: it is the string the refusal,
/// the gate in `crates/vike-config/tests/layers_are_reachable.rs` and
/// `crates/vike-cli/tests/settings_layers_reachable.rs` all have to agree on, and a tombstone
/// spelled once cannot drift from the message that names it.
pub const REMOVED_PROJECT_FILE: &str = "vike.toml";

/// Refuse to start when a `<project>/vike.toml` is present. `Ok(())` is the overwhelmingly common
/// case — nobody has one.
///
/// `settings_dir` is `<project>/settings`, so the project is its PARENT: one [`Path::parent`] call,
/// which resolves no directory of its own (this crate never walks, never expands `~`, never reads a
/// platform variable — see [`fn@crate::load`]). A `None` settings directory means there is no project
/// to look in, and a settings directory at a filesystem root has no parent; both skip the check,
/// because there is no file that could be misleading anybody.
///
/// ⚠ The probe is [`std::fs::metadata`] and only `NotFound` counts as absent. Any other error means
/// we could not ESTABLISH absence, and a file we cannot see is precisely the one an operator would
/// believe is in force — so it refuses and names the path, the same reasoning `read_toml` uses for
/// preferring a failed read over a prior `Path::exists()`.
///
/// ⚠ **Which is why the outcome is CARRIED rather than collapsed.** Fail-closed is the right
/// verdict and it is not changing; asserting *"this file is present"* on the strength of it is not.
/// See [`RemovedFileProbe`].
pub(crate) fn refuse_removed_project_file(settings_dir: Option<&Path>) -> Result<(), ConfigError> {
    let Some(project) = settings_dir.and_then(Path::parent) else { return Ok(()) };
    let file = project.join(REMOVED_PROJECT_FILE);
    match std::fs::metadata(&file) {
        Ok(_) => Err(ConfigError::RemovedProjectFile { file, probe: RemovedFileProbe::Present }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            Err(ConfigError::RemovedProjectFile { file, probe: RemovedFileProbe::Unestablished(e) })
        }
    }
}

/// **What the stat actually established.** The two answers that are not `NotFound`, kept apart
/// because only ONE of them is a statement about a file that exists.
///
/// Both REFUSE — [`refuse_removed_project_file`] is fail-closed and stays that way, for the reason
/// its own doc gives. What they must not share is the message. Under a systemd unit's
/// `ProtectHome=yes` a stat of any path below `/home`, `/root` or `/run/user` returns **`EACCES`,
/// not `ENOENT`** (measured on the CI box), so a project root inside a user's home produced a hard
/// startup refusal that named a `vike.toml` **which did not exist** and told the operator to delete
/// it. Neither half of that is a cosmetic defect: it asserts something false, it names none of the
/// causes that actually produce it, and — because this check is layer 0 of [`crate::load_with_cli`]
/// — it fires before anything else in boot, so a false one MASKS whatever the real problem was.
///
/// The shape is `vike_data`'s `unwritable_store_root`, one crate over: a sandbox errno is decorated
/// with the unit directive that causes it and the directive that fixes it, rather than surfacing as
/// a bare `os error 13` pointing at the disk.
#[derive(Debug)]
pub enum RemovedFileProbe {
    /// [`std::fs::metadata`] SUCCEEDED, so something really is there under that name — a file, or
    /// a directory somebody created by mistake, which is at least as confusing. This is the answer
    /// that has earned the right to say *is present* and *delete it*.
    Present,
    /// The stat failed with something other than `NotFound`: absence was not established, and
    /// neither was presence.
    ///
    /// Carries the [`std::io::Error`] verbatim rather than a rendered string — the errno is the
    /// operator's first clue, and it is what [`std::error::Error::source`] hands a programmatic
    /// caller.
    Unestablished(std::io::Error),
}

/// The whole operator-facing refusal for a [`REMOVED_PROJECT_FILE`], ready to print — one message
/// per [`RemovedFileProbe`] answer.
///
/// Split from the error variant so the text lives beside the argument it makes, and so a test can
/// assert the message without constructing a filesystem.
pub(crate) fn removed_project_file_message(file: &Path, probe: &RemovedFileProbe) -> String {
    match probe {
        RemovedFileProbe::Present => present_project_file_message(file),
        RemovedFileProbe::Unestablished(e) => unestablished_project_file_message(file, e),
    }
}

/// The refusal for a file that IS there. Says so, and says what to do with it.
fn present_project_file_message(file: &Path) -> String {
    format!(
        "{path} is present, but the per-project override layer is NO LONGER READ — removed because \
         a fifth settings file above <project>/settings/ reintroduces the `which file won?` \
         question that directory exists to answer. There are no settings files to move its keys \
         into any more either (`docs/decisions/0086`): write each `[config]`/`[preferences]` key it \
         holds with `vike-cli config set config.<key> <value>` / `vike-cli config set \
         preferences.<key> <value>`, then delete {path}.\n\
         [policy] and [flags] tables were never accepted in it; write those with `vike-cli config \
         set policy.<key> <value>` / `vike-cli config set flags.<key> <value>`.\n\
         Nothing has been moved or deleted for you: it is your file.",
        path = file.display(),
    )
}

/// The refusal for a path that could not be PROBED. Same fail-closed verdict, and it claims
/// nothing at all about the file — because nothing is known about it.
///
/// The likeliest causes are named because the errno alone sends an operator hunting for a file that
/// is very probably not there: `ProtectHome=` and `ProtectSystem=` are the two unit directives that
/// turn "nothing here" into `Permission denied`, and `VIKE_SETTINGS_DIR` is the way to point the
/// process at a project it can actually see. The relocation instructions from
/// [`present_project_file_message`] are still offered, but conditionally — *if* it really is there.
fn unestablished_project_file_message(file: &Path, error: &std::io::Error) -> String {
    format!(
        "{path} could not be STATTED: {error} — this is NOT the same as absent, and it is NOT \
         evidence that the file is there. The per-project override layer is NO LONGER READ, and a \
         path whose absence cannot be ESTABLISHED refuses rather than being assumed empty: a file \
         nobody can see is precisely the one an operator would believe is in force.\n\
         ⚠ It may not exist at all. Under a systemd unit the likeliest cause is the sandbox rather \
         than a leftover file: ProtectHome=yes makes /home, /root and /run/user unreachable, so a \
         stat below them fails with `Permission denied` whether or not anything is there, and \
         ProtectSystem=strict does the same outside what ReadWritePaths= / ReadOnlyPaths= name. \
         Give the unit access to the project directory {project}, relax ProtectHome= to read-only, \
         or name a project the process can see with VIKE_SETTINGS_DIR. Otherwise it is ordinary \
         permissions: a parent directory this process cannot search (no `x` bit) fails identically.\n\
         If {path} really is there, it is the removed layer: write its `[config]`/`[preferences]` \
         keys with `vike-cli config set config.<key> <value>` / `vike-cli config set \
         preferences.<key> <value>`, then delete it.\n\
         Nothing has been moved or deleted for you.",
        path = file.display(),
        project = file.parent().unwrap_or(Path::new(".")).display(),
    )
}
