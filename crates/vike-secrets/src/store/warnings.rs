//! Findings about the credential file's placement, returned as data: `PermissionWarning` etc.

use super::*;

/// Something about the credential file's PLACEMENT that its reader ought to know.
///
/// Returned as DATA rather than logged here: this crate has no dependencies at all (see the crate
/// doc) and must not grow a logging one for a warning string. `vike_bridge_core::credentials` logs
/// it through `tracing`; `vike-cli secrets` prints it on stderr.
///
/// **A finding is never a refusal.** Refusing to read a 0644 file would strand somebody mid-setup
/// with every venue on paper — strictly worse than the exposure it objects to. Reading one
/// SILENTLY is worse still, which is why this exists. The same reasoning covers
/// [`Finding::Symlink`] for a different reason: a symlinked store is a LEGITIMATE setup, and the
/// finding says where the file really is rather than objecting to it.
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
    /// calls); `target_mode` is the followed `st_mode & 0o777`, i.e. the mode of the file that
    /// `read_to_string` will actually open, and `None` when the link dangles.
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
/// Read-write-execute across both classes, not just read: a group-WRITABLE credential file lets
/// somebody substitute the keys an order is signed with, which is worse than letting them read it.
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
/// irrelevant. The mode carried on that variant is the TARGET's — what `read_to_string` opens.
///
/// **Unix only.** On Windows the mode arm is a `None`-returning no-op: the mode bits do not exist
/// there and the equivalent question is an ACL query, which needs a Win32 crate this workspace does
/// not carry. The SYMLINK arm is not unix-specific, but reporting it alone on Windows would be a
/// finding this crate cannot pair with the permission question that gives it meaning, so the
/// Windows no-op is left exactly as it was.
///
/// ⚠ **`pub` so a caller can ask the question WITHOUT opening the store.** This performs one or two
/// `stat` calls and a `readlink`; it never reads the file's CONTENTS, so it pulls no credential
/// value into the process. That distinction is the whole reason it is exported: [`resolve`] answers
/// the same question, but only as a side effect of `read_to_string` + `parse_dotenv`, so a command
/// whose documented contract is "opens nothing" — `vike-cli secrets path`, the command the README
/// and the ops runbook name FIRST — could not reach the finding, and therefore reported a
/// world-writable credential file in silence.
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

/// The credential store this project used to have: `<project>/.env`.
///
/// A tombstone, kept as a named constant for the same reason `vike_config`'s
/// `REMOVED_PROJECT_FILE` is: it is the string [`legacy_store_warning`], its tests and the upgrade
/// note (`docs/ops/upgrading.md`) all have to agree on, and a name spelled once cannot drift from
/// the message that names it. Nothing reads the file.
pub const LEGACY_STORE_FILE: &str = ".env";

/// **A pre-one-store credential store left beside the project while the real store is ABSENT.**
///
/// The upgrade path was silent exactly here. [`resolve`] answers [`Source::None`], the map comes
/// back empty, every venue loader turns that into `None`, and every venue stays paper — which is the
/// CORRECT behaviour for a box with no credentials and an INDISTINGUISHABLE one for a box whose
/// credentials are sitting in the file that used to be read. The operator's symptom is "my orders
/// aren't reaching the venue", with nothing in any log.
///
/// Returned as DATA, like [`PermissionWarning`] and for the same reason: this crate has no
/// dependencies at all, a logging one included. `vike_bridge_core::credentials` logs it through
/// `tracing`; `vike-cli secrets` prints it on stderr.
///
/// ⚠ **A finding, never a refusal — a deliberate departure from
/// `vike_config::refuse_removed_project_file`, which REFUSES its leftover.** A `vike.toml` has no
/// other meaning, so refusing over one is safe. A `.env` does: it is a systemd `EnvironmentFile`,
/// and the CI box's live recorder ships `EnvironmentFile=-<project>/.env` holding `POLY_PROXY_ENABLED`
/// and no credential at all. Refusing would make a genuinely fresh, correct install unstartable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyStoreWarning {
    /// The leftover: `<project>/.env`. Its CONTENTS are never read — see [`legacy_store_warning`].
    pub legacy: PathBuf,
    /// The store that was looked for and is not there.
    pub store: PathBuf,
}

impl std::fmt::Display for LegacyStoreWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (legacy, store) = (self.legacy.display(), self.store.display());
        let dir = self.store.parent().unwrap_or(Path::new(".")).display();
        write!(
            f,
            "{legacy} is present but the credential store {store} is NOT — nothing has read a \
             project-root `{LEGACY_STORE_FILE}` since credentials moved into the settings \
             directory, so every venue stays paper. If that file holds venue credentials, copy \
             them into the store: `mkdir -p {dir} && cp {legacy} {store} && chmod 600 {store}`. \
             If it is a systemd EnvironmentFile of tunables it is still doing its job, and this \
             says only that the store is missing. Nothing has been moved or deleted for you."
        )
    }
}

/// `Some` when the pre-one-store [`LEGACY_STORE_FILE`] sits beside the project whose `store` this
/// is. **Ask only when the store is ABSENT** — [`resolve`] does, and so does `vike-cli secrets`.
///
/// The gate matters as much as the probe. A `.env` beside a store that LOADED is a systemd
/// `EnvironmentFile` doing its job (the CI box's recorder), and a warning that fires on a
/// correctly-configured box every time is one everybody learns to scroll past. An absent store is
/// precisely the silent case and nothing else.
///
/// `store` is `<project>/settings/secrets.env`, so the project is its GRANDparent — one
/// [`Path::parent`] more than `vike_config::refuse_removed_project_file`'s, which starts from the
/// settings directory. `None` when there is no such ancestor: there is then no project beside which
/// a leftover could be misleading anybody.
///
/// ⚠ **It never opens the file.** [`std::fs::metadata`] answers existence, and the contents are the
/// operator's credentials — no diagnostic needs them. Only `NotFound` counts as absent: any other
/// error means absence could not be ESTABLISHED, and a file we cannot see is exactly the one someone
/// would believe is in force (the same rule `refuse_removed_project_file` applies).
pub fn legacy_store_warning(store: &Path) -> Option<LegacyStoreWarning> {
    let project = store.parent()?.parent()?;
    let legacy = project.join(LEGACY_STORE_FILE);
    match std::fs::metadata(&legacy) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        _ => Some(LegacyStoreWarning { legacy, store: store.to_path_buf() }),
    }
}
