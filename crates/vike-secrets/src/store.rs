//! Opening the credential store: `<project>/settings/secrets.env`.
//!
//! [`resolve`] opens a path the caller names; [`resolve_project`] is the entry point for a caller
//! with no opinion, which asks [`crate::workspace_dotenv_path_from`] for the project's own file.
//! There is one file, so there is no precedence to implement here — only reading it, reporting
//! where the answer came from, and reporting a permission finding on it.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// A credential map. Redacts in `Debug`; never implements `Display`.
///
/// `Debug` shows the KEY NAMES and the count but never a value — the same balance
/// `vike_bridge_core::credentials::Credentials` strikes with `api_key=***{last4}`. Key names are
/// already public knowledge (they are the `vike_ops::settings` registry's whole subject matter);
/// values are the credential.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretMap(BTreeMap<String, String>);

impl SecretMap {
    pub fn new(map: BTreeMap<String, String>) -> Self {
        SecretMap(map)
    }

    /// Build from the `HashMap<String, String>` shape the rest of the workspace speaks.
    pub fn from_map(map: HashMap<String, String>) -> Self {
        SecretMap(map.into_iter().collect())
    }

    /// Hand back the `HashMap<String, String>` every existing credential call site expects. Named
    /// so the call site reads as the deliberate end of redaction that it is.
    pub fn into_map(self) -> HashMap<String, String> {
        self.0.into_iter().collect()
    }

    /// The sorted key names — safe to print, and what `vike-cli secrets list` shows.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretMap({} entries: [", self.0.len())?;
        for (i, k) in self.0.keys().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{k}=***")?;
        }
        f.write_str("])")
    }
}

/// The one thing that can go wrong: the store EXISTS and could not be read.
///
/// An ABSENT store is not this — it is [`Source::None`] and an empty map, which is the live gate
/// (no credentials ⇒ every venue stays paper). "Not configured" and "cannot open" must never look
/// the same to an operator, which is the whole reason this type exists.
///
/// Carries the PATH and the OS reason, never file contents — so `Debug`/`Display` are safe to log
/// verbatim, which is the point: an operator has to be able to see "the store did not open" in a
/// daemon log without that log becoming a credential.
#[derive(Debug)]
pub struct SecretsError {
    /// The file that could not be read.
    pub path: PathBuf,
    /// Why the OS refused.
    pub source: std::io::Error,
}

impl std::fmt::Display for SecretsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "credential store {} could not be read: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for SecretsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Where a resolved credential map came from — the provenance `vike-cli secrets` prints so an
/// operator is never guessing which file is live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Parsed from this file.
    File(PathBuf),
    /// The store does not exist. An empty map: the live gate (no credentials ⇒ stay paper).
    None,
}

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

/// What [`resolve`] found: the credentials, WHICH file they came from, and any finding about the
/// store — its exposure, or a leftover predecessor beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The credential map. Empty when the store does not exist — the live gate.
    pub secrets: SecretMap,
    /// Which file answered.
    pub source: Source,
    /// Set when the store is readable beyond its owner. The CALLER logs or prints it; see
    /// [`PermissionWarning`].
    pub warning: Option<PermissionWarning>,
    /// Set when the store is ABSENT and the pre-one-store [`LEGACY_STORE_FILE`] is sitting beside
    /// the project. The CALLER logs or prints it; see [`LegacyStoreWarning`].
    pub legacy: Option<LegacyStoreWarning>,
}

/// **Open the credential store at `path`.**
///
/// | on disk | result |
/// |---|---|
/// | present and readable | the parsed map, [`Source::File`] |
/// | absent | an EMPTY map, [`Source::None`] — the live gate (no credentials ⇒ stay paper) |
/// | present and unreadable | [`SecretsError`] |
///
/// The absent arm is an ANSWER, not a failure: a checkout with no credentials is the normal state
/// of CI and of a fresh clone, and every venue loader turning that into `None` is the designed
/// behaviour. The unreadable arm is the opposite case and is loud, because a permissions bug that
/// silently degraded to "no credentials" would look exactly like a correct fresh install.
///
/// ⚠ The absent arm carries ONE extra finding: [`legacy_store_warning`], the pre-one-store
/// `<project>/.env`. An empty map is the right ANSWER for a box with no credentials and the wrong
/// one for a box whose credentials never moved, and from here the two are indistinguishable — so
/// the arm that produces the emptiness is where the difference has to be noticed. It is a finding,
/// not an error: the arm still returns the same empty map and the same [`Source::None`].
///
/// Nothing here ever writes, moves or deletes the file. It is the user's only copy of live venue
/// credentials.
pub fn resolve(path: &Path) -> Result<Resolved, SecretsError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Resolved {
                secrets: SecretMap::default(),
                source: Source::None,
                warning: None,
                legacy: legacy_store_warning(path),
            });
        }
        Err(source) => return Err(SecretsError { path: path.to_path_buf(), source }),
    };
    Ok(Resolved {
        secrets: SecretMap::from_map(crate::dotenv::parse_dotenv(&text)),
        source: Source::File(path.to_path_buf()),
        warning: permission_warning(path),
        // The store LOADED, so nothing was silent — and a `.env` beside a working store is a
        // systemd `EnvironmentFile`, not a leftover. See `legacy_store_warning`.
        legacy: None,
    })
}

/// [`resolve`] over the PROJECT's own store — `<project>/settings/secrets.env`, found by
/// [`crate::workspace_dotenv_path_from`].
///
/// `settings_dir` is [`crate::SETTINGS_DIR_ENV`]'s value, which names the directory outright and
/// wins over the walk. It arrives as a PARAMETER: this crate reads no environment, so the
/// composition root passes it down out of the one `std::env::vars()` sweep it already owns.
///
/// `vike_bridge_core::credentials::load_workspace_secrets_at` is the infallible wrapper over this.
pub fn resolve_project(settings_dir: Option<&str>) -> Result<Resolved, SecretsError> {
    resolve(&crate::dotenv::workspace_dotenv_path_from(settings_dir))
}

/// Where a node key was actually found, so a caller can WARN about the legacy home without
/// re-deriving the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKeySource {
    /// `<project>/settings/node.env` — the home. Nothing to say.
    NodeFile,
    /// `<project>/settings/secrets.env` — the LEGACY home, still read, warned about.
    LegacyCredentialStore,
    /// Neither file carries a node key. The ordinary unconfigured state; silent.
    Absent,
}

/// The NODE-key store: `<project>/settings/node.env`, falling back to the credential store for keys
/// that have not moved yet.
///
/// ⚠ **This is the ONE fallback in the two-store design, it is a MIGRATION and it is temporary.**
/// The rule the split obeys is that a name has one home decided statically — no ladder. This arm
/// exists because the tradehub node keys were written into `secrets.env` by every `node setup` run
/// before 2026-09-08 and a live daemon is holding a pair there right now; deleting the read outright
/// would take a running node's authentication away on the next deploy. It returns
/// [`NodeKeySource::LegacyCredentialStore`] so the caller can say so, once, by name.
///
/// ⚠ It is DELIBERATELY not a merge. Whichever file answers FIRST answers wholly: a pair split
/// across the two files is a half-migrated box, and merging would hide that while producing a
/// mismatched pair — an opaque `bad mac` at the node, which is the exact symptom
/// `crates/vike-cli/tests/node_cli.rs` records as the expensive one. `node.env` existing with a
/// non-empty node key is the whole test.
///
/// Nothing here writes, moves or deletes either file.
pub fn resolve_node_keys(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
) -> Result<(Resolved, NodeKeySource), SecretsError> {
    let node = resolve(&crate::dotenv::workspace_node_path_from(settings_dir))?;
    let carries_one = node.secrets.keys().any(&is_node_key);
    if carries_one {
        return Ok((node, NodeKeySource::NodeFile));
    }
    let legacy = resolve_project(settings_dir)?;
    let source = if legacy.secrets.keys().any(&is_node_key) {
        NodeKeySource::LegacyCredentialStore
    } else {
        NodeKeySource::Absent
    };
    Ok((legacy, source))
}

/// The sentence a caller prints when [`resolve_node_keys`] answered
/// [`NodeKeySource::LegacyCredentialStore`] — one place, so five binaries cannot word the same
/// migration five ways.
#[must_use]
pub fn legacy_node_key_notice(settings_dir_display: &str) -> String {
    format!(
        "node keys are still in {settings_dir_display}/{} — the file that also holds every venue \
         key. Move the `VIKE_*_OBSERVE_KEY` / `VIKE_*_CONTROL_KEY` lines to \
         {settings_dir_display}/{}, which holds node keys and nothing else; they are read from \
         there first. This fallback is a migration and will be removed.",
        crate::dotenv::SECRETS_FILE,
        crate::dotenv::NODE_FILE
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "vike-secrets-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> SecretMap {
        let mut m = BTreeMap::new();
        m.insert("BINANCE_LIVE_API_KEY".to_string(), "key-abcd".to_string());
        m.insert("BINANCE_LIVE_API_SECRET".to_string(), "sup3r-s3cr3t".to_string());
        SecretMap::new(m)
    }

    fn put(path: &Path, value: &str) {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(path, format!("# a comment\nBINANCE_LIVE_API_KEY={value}\n")).unwrap();
    }

    fn value(r: &Resolved) -> String {
        r.secrets.clone().into_map().get("BINANCE_LIVE_API_KEY").cloned().unwrap_or_default()
    }

    #[test]
    fn debug_shows_key_names_but_never_a_value() {
        let shown = format!("{:?}", sample());
        assert!(shown.contains("BINANCE_LIVE_API_KEY=***"));
        assert!(shown.contains("2 entries"));
        assert!(!shown.contains("sup3r-s3cr3t"));
        assert!(!shown.contains("key-abcd"));
    }

    #[test]
    fn debug_of_an_empty_map_is_harmless() {
        assert_eq!(format!("{:?}", SecretMap::default()), "SecretMap(0 entries: [])");
    }

    /// The store is read, byte for byte, and never rewritten.
    #[test]
    fn an_existing_store_is_parsed_and_left_alone() {
        let d = tmpdir("read");
        let store = d.join("settings").join("secrets.env");
        put(&store, "from-the-project");

        let r = resolve(&store).unwrap();
        assert_eq!(r.source, Source::File(store.clone()));
        assert_eq!(value(&r), "from-the-project");
        assert!(std::fs::read_to_string(&store).unwrap().contains("=from-the-project"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An ABSENT store is an empty map, not an error — the live gate.
    #[test]
    fn no_store_at_all_is_an_empty_map_not_an_error() {
        let d = tmpdir("absent");
        let r = resolve(&d.join("settings").join("secrets.env")).unwrap();
        assert!(r.secrets.is_empty());
        assert_eq!(r.source, Source::None);
        assert_eq!(r.warning, None);
        assert_eq!(r.legacy, None, "no leftover beside it either");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A leftover `<project>/.env` with NO store is a finding — the silent case.**
    ///
    /// The empty map is unchanged and [`Source::None`] is unchanged: this adds a diagnostic beside
    /// the live gate, it does not alter it. What it separates is the two boxes that produce the
    /// identical empty map — one with no credentials (correct) and one whose credentials never moved
    /// (every venue silently on paper).
    #[test]
    fn a_leftover_dotenv_beside_the_project_is_a_finding_when_the_store_is_absent() {
        let d = tmpdir("legacy-absent");
        let store = d.join("settings").join("secrets.env");
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        std::fs::write(d.join(LEGACY_STORE_FILE), "BINANCE_LIVE_API_KEY=never-printed\n").unwrap();

        let r = resolve(&store).unwrap();
        // The live gate is untouched.
        assert!(r.secrets.is_empty());
        assert_eq!(r.source, Source::None);
        // …and the finding names both files.
        let w = r.legacy.expect("a leftover store must not be silent");
        assert_eq!(w.legacy, d.join(LEGACY_STORE_FILE));
        assert_eq!(w.store, store);
        let msg = w.to_string();
        assert!(msg.contains("stays paper"), "{msg}");
        assert!(msg.contains("chmod 600"), "the finding must say how to fix it: {msg}");
        assert!(msg.contains("EnvironmentFile"), "…and when it is NOT a problem: {msg}");
        assert!(!msg.contains("never-printed"), "the probe must never read the CONTENTS: {msg}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A `.env` beside a store that LOADED is silent.** It is a systemd `EnvironmentFile` — the CI box's
    /// live recorder ships one — and nothing was silent about credentials, because the store answered.
    #[test]
    fn a_dotenv_beside_a_present_store_is_not_a_finding() {
        let d = tmpdir("legacy-present");
        let store = d.join("settings").join("secrets.env");
        put(&store, "from-the-store");
        std::fs::write(d.join(LEGACY_STORE_FILE), "POLY_PROXY_ENABLED=false\n").unwrap();

        let r = resolve(&store).unwrap();
        assert_eq!(value(&r), "from-the-store");
        assert_eq!(r.legacy, None, "a store that loaded is not a silent transition");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The probe is `metadata`, so only `NotFound` is absence — a DIRECTORY named `.env` still
    /// reports, and a path with no grandparent has no project to look beside.
    #[test]
    fn only_not_found_counts_as_absent_and_a_rootless_path_is_skipped() {
        let d = tmpdir("legacy-dir");
        let store = d.join("settings").join("secrets.env");
        std::fs::create_dir_all(store.parent().unwrap()).unwrap();
        std::fs::create_dir_all(d.join(LEGACY_STORE_FILE)).unwrap();
        assert!(legacy_store_warning(&store).is_some(), "a directory is not established absence");

        // ...but a DIRECTORY makes `metadata` return `Ok`, so the case above exercises the `_` arm
        // and never the guard itself. The guard — "only NotFound counts as absent" — needs an
        // error that is NOT NotFound, and this is it: probing THROUGH a regular file yields
        // ENOTDIR. Without this line, replacing the guard with `true` (absence always established)
        // passes, and a store nobody can stat reports as cleanly absent — the exact conflation the
        // doc above forbids.
        #[cfg(unix)]
        {
            let file = d.join("proj");
            put(&file, "not a directory");
            let under_a_file = file.join("settings").join(crate::SECRETS_FILE);
            assert!(
                legacy_store_warning(&under_a_file).is_some(),
                "ENOTDIR is not established absence — only NotFound is"
            );
        }

        // `secrets.env` alone: parent is "", and "" has no parent — no project, no probe.
        assert_eq!(legacy_store_warning(Path::new(crate::SECRETS_FILE)), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A store that EXISTS and cannot be opened is an ERROR, never a silent empty map.**
    ///
    /// The two must not look the same: an empty map means "this box is not configured", and a
    /// permissions bug quietly wearing that answer would look exactly like a correct fresh install
    /// while every venue dropped to paper for a completely different reason. A DIRECTORY where the
    /// file should be is the portable stand-in for an unreadable file (a `chmod 000` proves nothing
    /// when the test runs as root, which CI does).
    #[test]
    fn an_unreadable_store_errors_instead_of_reporting_no_credentials() {
        let d = tmpdir("unreadable");
        let store = d.join("secrets.env");
        std::fs::create_dir_all(&store).unwrap();

        let e = resolve(&store).expect_err("an unopenable store must not read as `not configured`");
        assert_eq!(e.path, store);
        assert!(e.to_string().contains("could not be read"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The project entry point resolves through the settings-directory override, with no
    /// environment read anywhere in this crate.
    #[test]
    fn the_project_entry_point_honours_an_explicit_settings_dir() {
        let d = tmpdir("project");
        let settings = d.join("settings");
        put(&settings.join("secrets.env"), "deployed");

        let r = resolve_project(settings.to_str()).unwrap();
        assert_eq!(r.source, Source::File(settings.join("secrets.env")));
        assert_eq!(value(&r), "deployed");

        // …and a blank override falls through to the walk rather than resolving to `""`.
        assert_eq!(resolve_project(Some("  ")).unwrap(), resolve_project(None).unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Two projects, two stores, one process — no global state is involved in pointing at either.
    #[test]
    fn two_stores_coexist_in_one_process() {
        let a = tmpdir("coexist-a");
        let b = tmpdir("coexist-b");
        put(&a.join("secrets.env"), "first");
        put(&b.join("secrets.env"), "second");

        assert_eq!(value(&resolve_project(a.to_str()).unwrap()), "first");
        assert_eq!(value(&resolve_project(b.to_str()).unwrap()), "second");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    /// **The permission warning fires on a too-open store and not on 0600.**
    ///
    /// `0o640` (group-readable) and `0o604` (other-readable) are the two shapes a `cp` or a shared
    /// deploy actually produces; `0o620` proves the check is not read-only — a group-WRITABLE
    /// credential file lets somebody substitute the keys an order is signed with.
    #[cfg(unix)]
    #[test]
    fn a_group_or_world_accessible_store_warns_but_still_loads() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("perm-warn");
        let store = d.join("secrets.env");
        // A value nothing else in this test spells, so the "never prints a value" assert is real.
        put(&store, "s3cr3t-key-material");

        for mode in [0o644u32, 0o640, 0o604, 0o620, 0o666] {
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode)).unwrap();
            let r = resolve(&store).unwrap();
            // It LOADS — a finding is never a refusal.
            assert_eq!(value(&r), "s3cr3t-key-material", "mode {mode:04o} must still load");
            let Some(w) = r.warning else { panic!("mode {mode:04o} must warn") };
            assert_eq!(w.path, store);
            assert_eq!(w.finding, Finding::ExposedMode(mode));
            let msg = w.to_string();
            assert!(msg.contains("chmod 600"), "the warning must say how to fix it: {msg}");
            assert!(
                !msg.contains("s3cr3t-key-material"),
                "the warning must never print a credential VALUE"
            );
        }

        // …and 0600 is silent.
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).unwrap();
        let r = resolve(&store).unwrap();
        assert_eq!(r.warning, None, "an owner-only store must not warn");
        assert_eq!(value(&r), "s3cr3t-key-material");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A SYMLINKED store is reported — and the check is the one that can see it.**
    ///
    /// The mode question was asked through `std::fs::metadata`, which FOLLOWS the link, so the
    /// answer described a file at a path the operator was never shown: an owner-only target
    /// reported clean while the path itself said nothing about where the credentials actually
    /// live, who owns that directory, or who can replace the file in it. `symlink_metadata` is
    /// what makes the indirection visible.
    ///
    /// The target here is 0600 on purpose — that is exactly the case the follow-the-link check
    /// calls clean and returns `None` for.
    ///
    /// ⚠ Still a FINDING, never a refusal: one shared credential file symlinked into several
    /// project checkouts is a legitimate setup (it is what the retired `~/.vike/secrets.env` slot
    /// existed for), so the store must keep loading. Refusing it would strand that operator with
    /// every venue on paper.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_store_is_reported_even_when_the_file_it_points_at_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("perm-symlink");
        let real = d.join("shared-secrets.env");
        put(&real, "s3cr3t-key-material");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.join("secrets.env");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let w = permission_warning(&link)
            .expect("a symlinked credential store must be reported, whatever its target's mode");
        assert_eq!(w.path, link);
        // The LINK-vs-TARGET contract, which nothing asserted: the reported mode is the FOLLOWED
        // target's 0o600 — not the link's own kernel-ignored 0o777, and not a raw `st_mode` with
        // its file-type bits still on (0o100600). A sweep replaced the `& 0o777` mask with `|` and
        // with `^` and the whole vike-secrets suite stayed green, because every assertion here was
        // about the MESSAGE and none about the number in it. (This is not an exposure inversion:
        // the `Symlink` finding fires whatever the mode, so nothing was ever silenced.)
        assert_eq!(
            w.finding,
            Finding::Symlink { target: Some(real.clone()), target_mode: Some(0o600) },
            "the reported mode is the target's, masked to the permission bits"
        );
        let msg = w.to_string();
        assert!(
            msg.to_lowercase().contains("symlink"),
            "the finding must say the path is a symlink: {msg}"
        );
        assert!(
            msg.contains(&real.display().to_string()),
            "the finding must name where the credentials actually are: {msg}"
        );
        assert!(
            !msg.contains("s3cr3t-key-material"),
            "the finding must never print a credential VALUE: {msg}"
        );

        // A finding is never a refusal — the store still loads through the link.
        let r = resolve(&link).unwrap();
        assert_eq!(value(&r), "s3cr3t-key-material", "a symlinked store must still load");
        assert!(r.warning.is_some(), "…and `resolve` must surface the same finding");
        let _ = std::fs::remove_dir_all(&d);
    }
}
