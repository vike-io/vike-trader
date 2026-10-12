//! The store's one placement finding, returned as data: `PermissionWarning` and `permission_warning`;
//! and the ONE place a finding becomes a log line, `warn_once`.

use super::*;

/// Something about the credential store's PLACEMENT that its reader ought to know.
///
/// Returned as DATA rather than logged by the READ: `resolve_project` and its siblings answer with
/// the finding in a field, because a caller that cannot log (`vike_boot`, which runs before a
/// subscriber exists) or that prints it itself (`vike-cli secrets` on stderr) must not have a log
/// line chosen for it. The LOGGING loaders are the ones that turn it into a line, through
/// [`warn_once`]: `vike_secrets::credentials` for the whole-map reads and
/// [`crate::load_workspace_secrets_scoped_at`] for the scoped ones, so the two share one dedupe set.
///
/// **A finding is never a refusal.** Refusing to read a 0644 store would strand somebody mid-setup
/// with every venue on paper — strictly worse than the exposure it objects to. Reading one
/// SILENTLY is worse still, which is why this exists. The same reasoning covers
/// [`Finding::Symlink`] for a different reason: a symlinked store is a LEGITIMATE setup, and the
/// finding says where the store really is rather than objecting to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionWarning {
    /// The path that was asked about — the one the operator was shown, which for a symlink is not
    /// the file that is read.
    pub path: PathBuf,
    /// What was found there.
    pub finding: Finding,
}

/// The two things [`permission_warning`] can find. Separate variants because the ADVICE differs:
/// one is fixed with `chmod`, the other is not a defect at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// `st_mode & 0o777` grants something to group or other. Unix only.
    ExposedMode(u32),
    /// The path is a SYMLINK: the credentials actually read live elsewhere, and this path says
    /// nothing about that file's owner, its mode, or who may replace it in ITS directory.
    ///
    /// `target` is `read_link`'s answer (`None` only if the link became unreadable between the two
    /// calls); `target_mode` is the followed `st_mode & 0o777`, i.e. the mode of the file the
    /// engine will actually open, and `None` when the link dangles.
    Symlink { target: Option<PathBuf>, target_mode: Option<u32> },
}

impl std::fmt::Display for PermissionWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = self.path.display();
        match &self.finding {
            Finding::ExposedMode(mode) => write!(
                f,
                "{p} holds live venue credentials in PLAINTEXT and is readable beyond its owner \
                 (mode {mode:04o}); run `chmod 600 {p}`"
            ),
            Finding::Symlink { target, target_mode } => {
                write!(f, "{p} is a SYMLINK, so the live venue credentials actually read are ")?;
                match target {
                    Some(t) => write!(f, "at {}", t.display())?,
                    None => write!(f, "elsewhere")?,
                }
                match target_mode {
                    Some(m) => write!(f, " (mode {m:04o})")?,
                    None => write!(f, " (a DANGLING link — nothing is there)")?,
                }
                write!(
                    f,
                    "; check that file's owner and the permissions of the directory holding it — \
                     this path's own mode says nothing about either"
                )
            }
        }
    }
}

/// `Some` when the credential store at `path` is a symlink, or when it grants any permission to
/// group or other (`mode & 0o077`).
///
/// Read-write-execute across both classes, not just read: a group-WRITABLE store lets somebody
/// substitute the keys an order is signed with, which is worse than letting them read it.
///
/// ⚠ **The stat is `symlink_metadata`, not `metadata`, and that is the difference between seeing a
/// symlinked store and not.** `metadata` FOLLOWS the link, so an owner-only target reported clean
/// and the indirection itself was invisible — while the path the operator was shown told them
/// nothing about where their credentials live, who owns that file, or who can replace it in its
/// own directory (a 0600 file in a 0777 directory is anybody's to substitute). On the regular-file
/// case the two calls are identical, so nothing about an ordinary install changes.
///
/// ⚠ **A symlink's OWN mode is not reported, and must not be**: on Linux it is 0777 by
/// construction and the kernel ignores it, so folding it into the `mode & 0o077` test would warn
/// on every symlinked store forever and tell the operator to `chmod` something that is already
/// irrelevant. The mode carried on that variant is the TARGET's — what the engine opens.
///
/// **Unix only.** On Windows the mode arm is a `None`-returning no-op: the mode bits do not exist
/// there and the equivalent question is an ACL query, which needs a Win32 crate this workspace does
/// not carry. The SYMLINK arm is not unix-specific, but reporting it alone on Windows would be a
/// finding this crate cannot pair with the permission question that gives it meaning, so the
/// Windows no-op is left exactly as it was.
///
/// ⚠ **`pub` so a caller can ask the question WITHOUT opening the store.** This performs one or two
/// `stat` calls and a `readlink`; it never reads the file's CONTENTS, so it pulls no credential
/// value into the process — which is what lets a command whose documented contract is "opens
/// nothing" (`vike-cli secrets path`) report a world-writable store.
#[cfg(unix)]
pub fn permission_warning(path: &Path) -> Option<PermissionWarning> {
    use std::os::unix::fs::PermissionsExt;
    let link = std::fs::symlink_metadata(path).ok()?;
    if link.file_type().is_symlink() {
        return Some(PermissionWarning {
            path: path.to_path_buf(),
            finding: Finding::Symlink {
                target: std::fs::read_link(path).ok(),
                // FOLLOWED on purpose: the mode worth reporting is the one of the file that is
                // actually read, not the link's meaningless 0777.
                target_mode: std::fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o777),
            },
        });
    }
    let mode = link.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then(|| PermissionWarning {
        path: path.to_path_buf(),
        finding: Finding::ExposedMode(mode),
    })
}

#[cfg(not(unix))]
pub fn permission_warning(_path: &Path) -> Option<PermissionWarning> {
    None
}

/// `true` the first time `message` is seen in this process — [`warn_once`]'s dedupe, so a per-frame
/// caller cannot flood the log.
fn first_time(message: &str) -> bool {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    // A poisoned mutex must not lose the finding: recover the guard rather than panic in a logging
    // path.
    match seen.lock() {
        Ok(mut g) => g.insert(message.to_string()),
        Err(poisoned) => poisoned.into_inner().insert(message.to_string()),
    }
}

/// **Every store finding, logged ONCE PER DISTINCT MESSAGE for the life of the process.**
///
/// ⚠ A per-call `warn!` floods the log: one caller reads credentials PER FRAME
/// (`crates/vike-desktop/src/main.rs`'s `workspace_credentials_checked`), and a store finding is a
/// permanent state, not a fault — a per-call line buried the whole WARN channel.
///
/// Keyed on the RENDERED MESSAGE, not a plain `Once`: the message embeds the paths, so two settings
/// directories in one process each get their line and a NEW finding is never swallowed by an old
/// one. A finding is a STATE, not an event, so the repeats carry no information; `vike-cli secrets`
/// prints the finding as data on every invocation.
///
/// ⚠ **ONE dedupe set per process, and this function owns it.** The scoped loaders in this crate
/// and `vike_secrets::credentials`' whole-map reads both call it, so the same finding reaching
/// the log by two doors prints once. A second `static` set anywhere would print it twice.
///
/// Callers pass a [`PermissionWarning`]'s `Display` — paths and an octal mode, never a value.
pub fn warn_once(message: &str) {
    if first_time(message) {
        tracing::warn!("{message}");
    }
}
