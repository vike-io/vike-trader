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
/// value into the process. That distinction is the whole reason it is exported: the retired file reader answered
/// the same question, but only as a side effect of reading the whole store, so a command
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
/// The upgrade path was silent exactly here. With no store the front doors answer [`Source::None`],
/// the map comes back empty, every venue loader turns that into `None`, and every venue stays paper
/// — which is the CORRECT behaviour for a box with no credentials and an INDISTINGUISHABLE one for a
/// box whose credentials are sitting in the file that used to be read. The operator's symptom is "my orders
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
    /// Where `vike-cli secrets migrate` would read a credential file from — `<settings>/secrets.env`.
    /// It is NOT a store: the settings database is the only one, and it is not there either.
    pub store: PathBuf,
}

impl std::fmt::Display for LegacyStoreWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (legacy, store) = (self.legacy.display(), self.store.display());
        let dir = self.store.parent().unwrap_or(Path::new(".")).display();
        write!(
            f,
            "{legacy} is present but there is NO credential store — nothing has read a \
             project-root `{LEGACY_STORE_FILE}` since credentials moved into the settings \
             directory, and the settings database is the only store now, so every venue stays \
             paper. If that file holds venue credentials, carry them in: `mkdir -p {dir} && cp \
             {legacy} {store} && vike-cli secrets migrate` (migrate only READS {store}). If it is \
             a systemd EnvironmentFile of tunables it is still doing its job, and this says only \
             that the store is missing. Nothing has been moved or deleted for you."
        )
    }
}

/// **A credential FILE on a box with NO settings database — on disk, holding keys, and NOT READ.**
///
/// The credential FILE store was removed (the owner's order of 2026-10-07; see [`Backend`]). A box
/// that never ran `vike-cli secrets migrate` therefore resolves NO credentials, and every venue
/// mounts paper — which is the correct, safe answer (absent credentials ARE the live gate) and,
/// without this finding, an INVISIBLE one: the operator's keys are sitting right there in
/// `secrets.env` and nothing says why they stopped arming anything.
///
/// So the file is reported, and the report says three things: that it is NOT READ, how many keyed
/// names it holds (so "an empty template" and "my live keys" read differently), and the one way to
/// carry it into the store — `vike-cli secrets migrate`, which reads it and never edits, moves or
/// deletes it.
///
/// Counting the keyed names means the file is READ here (by the same reader migrate carries a file
/// with); the values are dropped before this returns and never reach the finding — it holds a path
/// and a NUMBER. A file that cannot even be read is still reported, with the OS reason in place of
/// the count: what matters is that it is not a store, and an unreadable leftover is a louder reason
/// to look, not a quieter one.
///
/// A finding, never a refusal, and returned as DATA (this crate carries no logging dependency):
/// `vike_bridge_core::credentials` logs it at `error` when it holds keys, `vike-cli secrets list` /
/// `path` and `vike-cli config check` print it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadCredentialFile {
    /// The file on disk that is not read: `<settings>/secrets.env` or `<settings>/node.env`.
    pub file: PathBuf,
    /// The settings database that would answer, and does not exist.
    pub db: PathBuf,
    /// How many names in the file carry a NON-BLANK value — `Err` with the OS reason when the file
    /// could not be read to count them.
    pub keyed: Result<usize, String>,
}

impl UnreadCredentialFile {
    /// Whether this file may be holding credentials an operator expects to be in force — anything
    /// but a file READ and found to carry no value. The caller's cue for `error` over `warn`.
    #[must_use]
    pub fn holds_keys(&self) -> bool {
        !matches!(self.keyed, Ok(0))
    }
}

impl std::fmt::Display for UnreadCredentialFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (file, db) = (self.file.display(), self.db.display());
        match &self.keyed {
            Ok(0) => write!(
                f,
                "{file} is on disk but is NOT READ, and holds no credential value — the settings \
                 database {db} is the only credential store and does not exist, so this box has \
                 no credentials. To create the store: {CREATE_STORE_REMEDY}."
            ),
            Ok(n) => write!(
                f,
                "{file} holds {n} credential name(s) but is NOT READ any more — the credential \
                 FILE store was removed and the settings database {db}, the only credential \
                 store, does not exist. This box has NO credentials, so every venue mounts PAPER. \
                 Carry the file in with `vike-cli secrets migrate` (`--dry-run` first): it only \
                 READS {file}, and never edits, moves or deletes it."
            ),
            Err(why) => write!(
                f,
                "{file} is on disk but could not be read ({why}) — and it is NOT READ as a \
                 credential store either way: the settings database {db} is the only one and does \
                 not exist, so this box has NO credentials and every venue mounts PAPER. Fix the \
                 file's permissions and carry it in with `vike-cli secrets migrate` \
                 (`--dry-run` first); it never edits, moves or deletes the file."
            ),
        }
    }
}

/// `Some` when `file` — a `secrets.env` or `node.env` — is on disk while the settings database at
/// `db` is not. **Ask only when there is NO database**: beside a database the same file is a
/// [`ShadowedStore`], and that is a different sentence.
///
/// Only `NotFound` counts as absent, the rule [`legacy_store_warning`] applies for the same reason:
/// a file whose existence cannot be ESTABLISHED is exactly the one somebody believes is in force.
#[must_use]
pub fn unread_credential_file(file: &Path, db: &Path) -> Option<UnreadCredentialFile> {
    let keyed = match std::fs::metadata(file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => Err(e.to_string()),
        Ok(_) => crate::db::read_credential_file(file)
            .map(|carried| carried.values().filter(|v| !v.trim().is_empty()).count())
            .map_err(|e| e.source.to_string()),
    };
    Some(UnreadCredentialFile { file: file.to_path_buf(), db: db.to_path_buf(), keyed })
}

/// `Some` when the pre-one-store [`LEGACY_STORE_FILE`] sits beside the project whose `store` this
/// is. **Ask only when the store is ABSENT** — `crate::store`'s absent arm does, and so does `vike-cli secrets`.
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
