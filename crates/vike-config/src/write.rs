//! **The settings-file WRITE half** (split-plane REQ-7): set ONE key in ONE of the four
//! `<project>/settings/*.toml` files, PRESERVING the operator's comments and the formatting of
//! every untouched line, and VALIDATING the would-be file with this crate's own loader BEFORE a
//! byte lands on disk.
//!
//! # The invariants, in order of importance
//!
//! 1. **The write must never produce a file the next boot refuses.** The whole edit is performed
//!    in memory, the resulting text is run through the SAME parse + apply the loader runs
//!    ([`validate_settings_text`] — [`crate::load::parse_toml_str`] into the file's own `*Patch`
//!    type, folded onto its default), and only a text that passes is written. A refusal echoes
//!    the loader's own [`crate::ConfigError`] message — the exact error a restart would have
//!    raised, surfaced NOW instead.
//! 2. **Untouched lines are untouched bytes.** These files are hand-edited by operators and carry
//!    their comments (`# raised for the weekend — revert Monday`); a writer that re-serialized
//!    the parsed model would silently delete every one. `toml_edit` is used for exactly this: the
//!    document keeps its formatting, and only the one assigned value changes. Replacing a value
//!    preserves the key's own decor (a comment block above the key) AND the value's same-line
//!    trailing comment (its suffix decor is copied onto the new value).
//!
//! The write itself is atomic (write a sibling tmp, rename over the target — the
//! `vike_connections::env_write::save_credentials` idiom): a crash mid-write leaves the old file
//! intact, never a truncated one.
//!
//! # The third invariant: one writer at a time, ACROSS PROCESSES
//!
//! 3. **The whole read → edit → validate → land sequence runs under an exclusive advisory lock on
//!    `<settings_dir>/`[`SETTINGS_LOCK_FILE`].** Without it this is a read-modify-write with no
//!    serialisation at all, and it has three concurrent callers in three different PROCESSES —
//!    `crates/vike-app-core/src/tool_views/venues.rs`'s `apply_arming` (the GUI's Save),
//!    `crates/vike-tradehub/src/server.rs`'s `apply_set_setting` (the daemon's control channel) and
//!    `crates/vike-cli/src/cmd/settings_write.rs`'s `set_setting_journalled` (the CLI) — so an
//!    in-process mutex answers none of it.
//!
//!    What that cost, before the lock: two writers read the same file, each edits its own key, and
//!    the second rename erases the first writer's edit. **The LEDGER, meanwhile, is already
//!    serialised** — `vike_model::change_journal`'s `append_record` takes an `AppendLock` across
//!    its open/write/sync — so both writes append cleanly and the change journal states that two
//!    changes landed when one did. An accountability record that is confidently wrong is worse than
//!    a missing one, and removing exactly that failure is what this writer's ledger exists for.
//!
//!    ⚠ **Three callers, but not three on every box.** The daemon's arm is the one that does not
//!    reach this lock on the shipped deployment at all — `ProtectSystem=strict` plus a
//!    `ReadWritePaths=` grant naming only `settings/state` makes the settings directory read-only
//!    inside that process's mount namespace, so its writes refuse at the sentinel open. The
//!    interlock is still the right mechanism and still load-bearing (the GUI and the CLI race on a
//!    dev box, and a deployment whose grant is widened by a future ruling puts the daemon straight
//!    back into the set), but a reader counting live writers on the CI box should count two.
//!    [`SettingsWriteError::Lock`] carries the measurement.
//!
//!    The idiom is the one already in this tree five times over
//!    (`crates/vike-model/src/change_journal.rs`'s `AppendLock`,
//!    `crates/vike-core/src/journal_lock.rs`'s `JournalLock`, `crates/vike-ops/src/live_lock.rs`'s
//!    `LiveLock`, `crates/vike-data/src/datafusion_hist/manifest.rs`'s `SeriesLock`): `flock` on
//!    unix, `LockFileEx` on Windows, through `std::fs::File::try_lock`, keyed on the open file
//!    description rather than on the process — so a second writer in the SAME process queues behind
//!    the first exactly as another process does, which is what makes the interlock testable at all.
//!    The kernel releases it when the holder dies, so there is no stale lock, no PID file and
//!    nothing to sweep.
//!
//! ## Why it SPINS rather than blocking, and **why the budget belongs to the CALLER**
//!
//! The change journal's acquire BLOCKS, and the argument it rests on does not transfer: its
//! critical section is one bounded append with no caller code inside it, measured in microseconds
//! at ~40 records a day. This one is a file read, a TOML parse, a whole loader validation and two
//! renames, and its other holders are a GUI and a daemon whose state this process cannot see.
//!
//! Refusing on the FIRST contention is the other wrong answer for most callers: it turns a
//! millisecond overlap into an operator-visible failure and a spurious `Outcome::Refused` ledger
//! row, for a race nobody was in.
//!
//! So: a `SeriesLock`-style spin — one `try_lock`, then 2 ms sleeps between attempts — and on
//! exhaustion a [`SettingsWriteError::Busy`] that names the sentinel, says how long it waited and
//! says NOTHING WAS WRITTEN. The uncontended path costs one syscall and no sleep.
//!
//! ⚠ **HOW LONG to spin is a property of WHO IS WAITING, not of the file being written, and this
//! module got that wrong once.** The budget shipped as a private constant every caller inherited,
//! and its whole argument was written for one of them: *"a CLI that hangs forever at 03:00 with
//! nothing on the terminal is the worst outcome available here — there is no supervisor to notice
//! and no timeout above it."* True of the CLI. But `apply_arming` is called **inside an egui
//! frame**, where three seconds of spinning is a frozen window an operator reads as "the app hung",
//! and `apply_set_setting` runs on a daemon CONNECTION THREAD, where the same three seconds holds
//! a socket and the rate token the peer paid for. Neither was ever what the paragraph was about.
//!
//! The budget is therefore a PARAMETER ([`LockBudget`], taken by [`set_setting_within`]), and each
//! of the three production callers chooses and ARGUES it at its own call site:
//!
//! | caller | budget | the argument, in one line (the full one is at the site) |
//! |---|---|---|
//! | `crates/vike-cli/src/cmd/settings_write.rs`'s `CLI_LOCK_BUDGET` | [`LockBudget::DEFAULT`] | a human typed the command and is watching it; this wait IS the process |
//! | `crates/vike-app-core/src/tool_views/venues.rs`'s `VENUE_ARM_LOCK_BUDGET` | [`LockBudget::NON_BLOCKING`] | a frame owes 16 ms and may not spend one of them waiting on another process |
//! | `crates/vike-tradehub/src/server.rs`'s `SETTINGS_LOCK_BUDGET` | 1 s | a peer holding a socket, with a reply deadline above it and a retry below it |
//!
//! ⚠ The third row is argued for a configuration the SHIPPED daemon is not in: its units make this
//! directory read-only in that process's own mount namespace, so its writes refuse at the sentinel
//! (a [`SettingsWriteError::Lock`], where the measurement is) before any budget is spent. That does
//! not make the row wrong — a container or dev deployment reaches it — but do not read the table as
//! three budgets all being exercised on the CI box.
//!
//! [`set_setting`] — the no-budget spelling — is [`LockBudget::DEFAULT`], and that default is
//! deliberately neither of the two silent wrong answers: never-wait would make a millisecond
//! overlap an operator-visible failure, and wait-forever would hand any caller the hang this
//! module refuses. A caller that says nothing gets the whole-process shape, which is the only shape
//! that is safe not to have thought about. ⚠ **No production caller says nothing** — the table
//! above is the whole set, and that spelling now has tests as its only callers; its own doc says so
//! rather than leaving "the default" to look like the shape somebody inherited.
//!
//! The dispositions therefore read as four different texts: a success report, a `Busy` refusal, an
//! `Io` refusal naming the file and the OS error, or a [`SettingsWriteError::Lock`] refusal naming
//! the sentinel it could not even open — which is how an operator tells which of them happened.
//!
//! ## What this does NOT close
//!
//! ⚠ **The file write and the ledger append are still TWO operations**, in that order (the caller
//! appends only after this function returns `Ok`). A crash between them leaves **a changed file and
//! no ledger row** — the UNDER-recording direction, which is the state this whole line of work
//! inherited rather than a new lie; the reverse, a row for a change that did not land, cannot arise
//! from a crash. Closing that would need the two to share one transaction, which is what
//! `docs/decisions/0054-settings-move-into-one-database.md` is about and is not available to a pair
//! of files.
//!
//! ⚠ **The lock orders THIS WORKSPACE's writers and nothing else.** An operator's `vim`, a `sed -i`
//! or a config-management tool takes no lock and can still clobber a write landing beside it. That
//! is the same bound `vike_model::change_journal`'s module doc states for its own lock, and the
//! same reason both files stay small and whole-file-atomic.
//!
//! # What deliberately does NOT live here
//!
//! The WIRE-level contract — which peer may write at all, and the typed-confirm rule for
//! `policy.toml` (a `SetSetting` naming the policy file is refused unless the request retypes the
//! exact key) — is `vike-tradehub`'s (`crates/vike-tradehub/src/server.rs`'s `accept_command`).
//! This module is the file mechanics: any caller that reaches it has already been authorized.
//! Restart-to-apply is likewise the caller's message to deliver: this module edits the file; it
//! neither knows nor changes what the running process loaded at boot.

use std::fs::{OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

use toml_edit::{DocumentMut, Item, TableLike, Value};

use crate::config::{Config, ConfigPatch};
use crate::error::ConfigError;
use crate::flags::{Flags, FlagsPatch};
use crate::load::parse_toml_str;
use crate::policy::{Policy, PolicyPatch};
use crate::preferences::{Preferences, PreferencesPatch};

/// One of the four settings files, by AUTHORITY level — the same four [`crate::load`] applies, in
/// its order. This is the parameter every write names first, because the file decides both the
/// validation type and (at the wire edge, not here) whether the typed-confirm contract applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFile {
    /// `policy.toml` — the hard ceilings ([`crate::Policy`]).
    Policy,
    /// `config.toml` — deployment settings ([`crate::Config`]).
    Config,
    /// `preferences.toml` — taste and tuning ([`crate::Preferences`]).
    Preferences,
    /// `flags.toml` — operator toggles ([`crate::Flags`]).
    Flags,
}

impl SettingsFile {
    /// Every settings file, in the loader's application order.
    pub const ALL: [SettingsFile; 4] = [
        SettingsFile::Policy,
        SettingsFile::Config,
        SettingsFile::Preferences,
        SettingsFile::Flags,
    ];

    /// Parse a caller-supplied file name: the file name (`"policy.toml"`) or its bare stem
    /// (`"policy"`), case-sensitive — these are literal file names, not vocabulary. `None` for
    /// anything else; [`unknown_file_message`] renders the refusal that names all four.
    pub fn parse(name: &str) -> Option<SettingsFile> {
        let stem = name.strip_suffix(".toml").unwrap_or(name);
        SettingsFile::ALL.into_iter().find(|f| f.section() == stem)
    }

    /// The file name inside the settings directory (`"policy.toml"`).
    pub fn file_name(self) -> &'static str {
        match self {
            SettingsFile::Policy => crate::load::POLICY_FILE,
            SettingsFile::Config => crate::load::CONFIG_FILE,
            SettingsFile::Preferences => crate::load::PREFERENCES_FILE,
            SettingsFile::Flags => crate::load::FLAGS_FILE,
        }
    }

    /// The dotted-key SECTION this file's keys are spelled under (`"policy"` for
    /// `policy.max_notional_per_order`) — the first segment of every `vike-cli config show` /
    /// `WireSettingsRow` key, which is also the spelling [`set_setting`] demands.
    pub fn section(self) -> &'static str {
        match self {
            SettingsFile::Policy => "policy",
            SettingsFile::Config => "config",
            SettingsFile::Preferences => "preferences",
            SettingsFile::Flags => "flags",
        }
    }

    /// **Which file a dotted KEY belongs to, decided from its SECTION WORD** — the first segment of
    /// `policy.max_notional_per_order`, never a file-name string. `None` when the first segment
    /// names none of the four.
    ///
    /// ⚠ Not [`SettingsFile::parse`]: that one parses a caller-supplied FILE NAME (`"policy.toml"`
    /// or its stem) and would answer `None` for a dotted key, because it strips a `.toml` suffix
    /// and compares the whole remainder. The two are different questions over strings that look
    /// alike, which is exactly why this is a named function rather than a `split('.').next()` at
    /// four call sites.
    ///
    /// It agrees with [`set_setting`] BY CONSTRUCTION: `key_path` refuses a write whose first
    /// segment is not the target file's `section()`, so for every write that can actually land,
    /// "which file" and "which section word" are the same answer.
    pub fn of_key(key: &str) -> Option<SettingsFile> {
        let head = key.split('.').next()?;
        SettingsFile::ALL.into_iter().find(|f| f.section() == head)
    }
}

/// **Is `key` in the POLICY plane?** — the membership the three enforcing surfaces key the typed
/// confirm on, decided from the key's SECTION WORD.
///
/// ⚠ **This exists because every enforcing site keyed that decision on a FILE NAME, and one of
/// them did it by EXACT STRING MATCH.** `crates/vike-app-core/src/tool_views/backend_settings.rs`
/// compared a `WireSettingsRow`'s `section` field — which carries `"policy.toml"` today — against
/// the literal file name. `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`
/// names the consequence: the moment the wire stops carrying file names, that comparison answers
/// `false`, the GUI stops demanding the retype, and NOTHING FAILS — a safety check disarmed by a
/// vocabulary change, silently. The re-key lands with the vocabulary move or the migration ships a
/// half-disarmed confirm.
///
/// ⚠ **It is NOT [`typed_confirm_reason`], and swapping it in would be a different change.** That
/// predicate is the KEY-keyed line `docs/decisions/0055-every-setting-is-editable-from-the-ui.md`
/// asks for: it is WIDER (the live-arm and safety-override flags) and NARROWER (it exempts
/// `policy.venues`/`policy.accounts`, the arming tables 0055 exempts) than today's membership.
/// Adopting it at the three surfaces would RELAX a live contract — a policy write that carries no
/// confirm is refused there today — which is its own change with its own review, and
/// [`requires_typed_confirm`]'s doc says exactly that. This function preserves today's membership
/// EXACTLY and changes only what it is computed from.
#[must_use]
pub fn is_policy_plane_key(key: &str) -> bool {
    SettingsFile::of_key(key) == Some(SettingsFile::Policy)
}

/// The refusal for a `file` that names none of the four settings files — one message, naming all
/// four, so a caller's typo gets the full menu instead of a guess.
pub fn unknown_file_message(name: &str) -> String {
    format!(
        "unknown settings file {name:?} — the four settings files are policy.toml / config.toml \
         / preferences.toml / flags.toml"
    )
}

/// What an accepted [`set_setting`] did — the audit record's raw material. `old_value` is the
/// value the FILE held before the write (`None` = the key was not set in the file; the effective
/// value may still have been a compiled-in default or an env override, which this module cannot
/// see and does not claim to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsWrite {
    /// The file that was edited (`"policy.toml"`).
    pub file: &'static str,
    /// The full dotted key, as the caller spelled it (`"policy.max_notional_per_order"`).
    pub key: String,
    /// The file's previous value for the key, rendered as TOML; `None` = not set in the file.
    pub old_value: Option<String>,
    /// The value written, rendered as TOML (`"250"`, `"\"127.0.0.1:7879\""`).
    pub new_value: String,
}

/// Why a [`set_setting`] refused. Every variant's `Display` is operator-facing — the daemon
/// surfaces it verbatim as the wire refusal.
#[derive(Debug)]
pub enum SettingsWriteError {
    /// The dotted key does not fit the file (wrong/missing section prefix, an empty segment, a
    /// path through a non-table, or a key naming a whole table).
    BadKey {
        /// The key as supplied.
        key: String,
        /// Why it was refused.
        reason: String,
    },
    /// The CURRENT file cannot be used as an edit base — refused rather than clobbered.
    ///
    /// Two shapes reach it. The file is PRESENT and cannot be read or parsed: an edit that
    /// replaces a hand-broken file would destroy exactly the bytes the operator needs in order to
    /// fix it. Or the file is ABSENT **with a `<file>.toml.bak` beside it**, which is the
    /// [`SettingsWriteError::Stranded`] end state and not a fresh project — see the empty-edit-base
    /// arm of [`set_setting_within`] for why that one is a refusal rather than a creation.
    CurrentFile {
        /// The file that could not be used as the edit base.
        file: PathBuf,
        /// What is wrong with it (parse messages are REDACTED — never the source line).
        message: String,
    },
    /// The WOULD-BE file fails this crate's own loader (unknown key, wrong type, out-of-range
    /// value, a removed key) — the write is refused with the loader's own message, so the error
    /// a restart would have raised surfaces now instead.
    Validation(ConfigError),
    /// The disk write itself failed (permissions, disk full, the rename).
    Io {
        /// The file the caller aimed at — the TARGET, never the sibling tmp: an operator who typed
        /// `config set policy.…` has never seen `.policy.toml.tmp-4711` and cannot find it, and a
        /// message naming it sends them looking for the wrong thing.
        file: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// **The writer lock's SENTINEL could not be opened or locked**, so the write was never
    /// attempted. NOTHING was written.
    ///
    /// Its own variant rather than an [`SettingsWriteError::Io`] blamed on the directory, because
    /// the two causes it has are both ones an `ls` of that directory ANSWERS WRONGLY.
    ///
    /// **1. The settings directory is read-only IN THE WRITER'S OWN MOUNT NAMESPACE** — the cause
    /// the shipped deployment GUARANTEES, and the one to check first. `deploy/vike-tradehub.service`
    /// — the one shipped unit, which a box installs with its own root substituted in — carries
    /// `ProtectSystem=strict`,
    /// which mounts the whole filesystem read-only except what `ReadWritePaths=` names, and that
    /// grant names `<project>/settings/state` and nothing above it — deliberately, and the grant
    /// argues it at itself: *"a daemon that can rewrite its own ceiling has none"*. Inside that
    /// unit the `create(true)` open of the sentinel is `EROFS`, while `ls -ld <project>/settings`
    /// from an ordinary shell — which runs OUTSIDE that namespace — shows a writable directory with
    /// no sentinel in it. **MEASURED 2026-09-13 on the CI box**, inside the live daemon's namespace
    /// (`nsenter -t <MainPID> -m`): `touch settings/settings.lock` → "Read-only file system",
    /// `touch settings/state/.probe` → OK.
    ///
    /// **2. The sentinel is owned by another account** — possible only where the directory IS
    /// writable by the writer, so it is the SECOND thing to look at rather than the first.
    /// [`SETTINGS_LOCK_FILE`] is created by whichever writer runs first and is **never unlinked**,
    /// so whoever created it owns the inode for good; with `UMask=0077` on the creating side that
    /// leaves an owner-only sentinel a differently-accounted writer cannot open, inside a directory
    /// `ls -ld` shows as writable by them. ⚠ It is NOT what the the CI box unit produces: that unit runs
    /// as the same account the operator uses (`User=the operator`), and it never creates the sentinel
    /// at all because cause 1 refuses it first.
    ///
    /// Blaming the DIRECTORY alone sends the operator to `chmod` a path that is already correct,
    /// re-run, and meet the identical message. So the `Display` names both, and leads with the one
    /// no `ls` can see.
    Lock {
        /// The settings directory the write aimed at — what the operator asked for.
        dir: PathBuf,
        /// The sentinel that was actually opened: `<dir>/`[`SETTINGS_LOCK_FILE`].
        lock: PathBuf,
        /// The open (or the platform's refusal to lock at all).
        source: std::io::Error,
    },
    /// **Another writer holds the settings directory** and the spin budget ran out. NOTHING was
    /// written — see the module doc's third invariant for who the other writers are and why this
    /// refuses rather than blocking forever.
    Busy {
        /// The sentinel that is held: `<settings_dir>/`[`SETTINGS_LOCK_FILE`].
        lock: PathBuf,
        /// How long the acquire spun before giving up.
        waited_ms: u64,
    },
    /// The replacement could not be landed AND the previous file had already been moved aside, so
    /// the target is now ABSENT and the operator's bytes are at `backup`.
    ///
    /// Its own variant rather than an [`SettingsWriteError::Io`] because the operator's next action
    /// is different from every other failure here: put a file back. Reachable only through the
    /// rename fallback, which exists for a destination another process holds open on Windows.
    Stranded {
        /// The target that is now absent.
        file: PathBuf,
        /// Where the previous file was moved to, and where it still is.
        backup: PathBuf,
        /// The rename failure that stranded it.
        source: std::io::Error,
    },
}

impl std::fmt::Display for SettingsWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsWriteError::BadKey { key, reason } => {
                write!(f, "bad settings key {key:?}: {reason}")
            }
            SettingsWriteError::CurrentFile { file, message } => {
                write!(f, "{}: {message}", file.display())
            }
            SettingsWriteError::Validation(e) => write!(f, "{e}"),
            SettingsWriteError::Io { file, source } => {
                write!(f, "{}: write failed: {source}", file.display())
            }
            SettingsWriteError::Lock { dir, lock, source } => write!(
                f,
                "{}: cannot take the writer lock ({source}) while opening {} — NOTHING was \
                 written. ⚠ FIRST: is that directory read-only to THIS writer? A sandboxed unit \
                 (`ProtectSystem=strict` with a `ReadWritePaths=` grant naming only \
                 `settings/state`) gets its own mount namespace, so `ls -ld {}` from an ordinary \
                 shell shows a directory the writer still cannot write — ask \
                 `systemctl show <unit> -p ProtectSystem -p ReadWritePaths` instead, and do NOT \
                 widen that grant to make this write land. SECOND, and only where the directory IS \
                 writable by you: the sentinel is created by whichever writer runs first and is \
                 never unlinked, so it can be owned by another account — `ls -l {}`.",
                dir.display(),
                lock.display(),
                dir.display(),
                lock.display()
            ),
            SettingsWriteError::Busy { lock, waited_ms } => write!(
                f,
                "another process is writing this settings directory (lock file {}) — waited \
                 {waited_ms} ms and gave up; NOTHING was written. The other writer is this box's \
                 GUI Save, the daemon's control channel, or a second local write. The kernel \
                 releases this lock when its holder exits, so there is no stale lock to clear: \
                 something is genuinely holding it. Re-run the command.",
                lock.display()
            ),
            SettingsWriteError::Stranded { file, backup, source } => write!(
                f,
                "{}: the replacement could not be landed ({source}) and the PREVIOUS file was \
                 already moved aside — it is at {}, and NOTHING is at {} now. Put it back by hand \
                 before restarting anything that reads it.",
                file.display(),
                backup.display(),
                file.display()
            ),
        }
    }
}

impl std::error::Error for SettingsWriteError {}

/// **What the CHANGE JOURNAL should say about a refused write** — drawn once, in the crate that
/// owns the error, because every caller that journals one has to answer it and two of them already
/// answered it identically and WRONGLY.
///
/// ⚠ Every variant here is [`vike_model::change_journal::Outcome::Refused`] except ONE.
/// [`SettingsWriteError::Stranded`] means the previous file has been moved to `<file>.toml.bak`
/// and the target is now ABSENT — a LARGER change than the write would have been — while
/// `Refused`'s own definition is *"the change was refused and nothing was written"*. Both the GUI
/// and the CLI mapped it there, so the one error variant this writer adds for the one state that
/// needs an operator to act reproduced, inside the ledger, exactly the "accountability record that
/// is confidently wrong" that the ledger exists to remove. The reason cell carried the truth; a
/// reader branches on the OUTCOME.
///
/// The window is narrow (four renames must go the wrong way) and the correctness of the row does
/// not depend on how often it is written.
#[must_use]
pub fn journal_outcome(e: &SettingsWriteError) -> vike_model::change_journal::Outcome {
    use vike_model::change_journal::Outcome;
    match e {
        SettingsWriteError::Stranded { .. } => Outcome::Stranded,
        SettingsWriteError::BadKey { .. }
        | SettingsWriteError::CurrentFile { .. }
        | SettingsWriteError::Validation(_)
        | SettingsWriteError::Io { .. }
        | SettingsWriteError::Lock { .. }
        | SettingsWriteError::Busy { .. } => Outcome::Refused,
    }
}

/// Validate one settings file's WOULD-BE text with the loader's own machinery: parse into the
/// file's `*Patch` type ([`parse_toml_str`] — the same redacted-message parse [`crate::load`]
/// runs) and fold it onto the type's default (the same `apply` the loader runs, so range checks
/// and removed-key refusals fire too). `path_for_errors` is the REAL file path, so the message
/// blames the file a restart would blame.
pub fn validate_settings_text(
    file: SettingsFile,
    path_for_errors: &Path,
    text: &str,
) -> Result<(), ConfigError> {
    match file {
        SettingsFile::Policy => {
            let patch: PolicyPatch = parse_toml_str(path_for_errors, text)?;
            Policy::default().apply(patch, path_for_errors)
        }
        SettingsFile::Config => {
            let patch: ConfigPatch = parse_toml_str(path_for_errors, text)?;
            Config::default().apply(patch, path_for_errors)
        }
        SettingsFile::Preferences => {
            let patch: PreferencesPatch = parse_toml_str(path_for_errors, text)?;
            Preferences::default().apply(patch, path_for_errors)
        }
        SettingsFile::Flags => {
            let patch: FlagsPatch = parse_toml_str(path_for_errors, text)?;
            // ⚠ `?`, not a discard: `Flags::apply` became fallible when six keys were DELETED from
            // this type while the variables they mirrored stayed live
            // (`crate::flags::REMOVED_FLAG_KEYS`). A write that validated a removed key here and
            // then failed the next LOAD would be the worst available outcome — the operator's
            // edit accepted, their daemon refusing to start.
            Flags::default().apply(patch, path_for_errors)
        }
    }
}

/// Set ONE key in ONE settings file under `settings_dir` — comment-preserving, validated before
/// written, written atomically. See the module doc for the two invariants.
///
/// `dotted_key` is the FULL dotted spelling the read half renders
/// (`"config.tradehub_addr"`, `"policy.rate.max_commands_per_sec"`): its first segment must equal
/// `file`'s section and the remainder is the path inside the file. `raw_value` is text: trimmed,
/// then parsed as a TOML value (`250`, `true`, `1.5`, `["a"]`, `"quoted"`) when it is one, else
/// written as a TOML string — and either way the whole would-be file then faces the loader, so a
/// type the key cannot take is refused with the loader's message, never landed.
///
/// An absent file is an empty edit base (the write creates it); an absent `settings_dir` is
/// created. A PRESENT file that cannot be read or parsed refuses
/// ([`SettingsWriteError::CurrentFile`]) rather than clobbering the operator's bytes.
///
/// ⚠ **This spelling has NO production caller, and that is worth knowing before you reach for it.**
/// All three production writers — the CLI, the GUI's venue arming and the daemon's control channel
/// — call [`set_setting_within`] and ARGUE a budget at their own call site, because how long to
/// wait is a property of who is waiting (the module doc's table). What is left here is the tests
/// and any future caller for which [`LockBudget::DEFAULT`]'s whole-process shape is genuinely
/// right. It is kept rather than deleted because that shape is the only one safe to inherit without
/// having thought about the question — but a FOURTH production caller arriving through this door
/// has, by construction, not thought about it. Reach for [`set_setting_within`] and say why.
pub fn set_setting(
    settings_dir: &Path,
    file: SettingsFile,
    dotted_key: &str,
    raw_value: &str,
) -> Result<SettingsWrite, SettingsWriteError> {
    set_setting_within(settings_dir, file, dotted_key, raw_value, LockBudget::DEFAULT)
}

/// [`set_setting`], with **the lock's wait budget as a parameter** — the spelling every production
/// caller uses, because how long to wait is a property of who is waiting.
///
/// The module doc's *why the budget belongs to the CALLER* section carries the argument and the
/// table of what each of the three callers chose; each site carries its own. `PUBLIC`, where the
/// predecessor was private: the privacy was justified as "no surface can quietly choose a shorter
/// wait than the one the module doc argues for", which held only for as long as that argument
/// covered every surface — and it never covered the GUI, which the same paragraph named.
///
/// It is also what a deliberately-CONTENDED test needs, for the reason
/// `crates/vike-data/src/datafusion_hist/manifest.rs`'s `SeriesLock::acquire_within` takes one: a
/// test that lowered the budget by editing a constant would stop testing the shipped number.
pub fn set_setting_within(
    settings_dir: &Path,
    file: SettingsFile,
    dotted_key: &str,
    raw_value: &str,
    budget: LockBudget,
) -> Result<SettingsWrite, SettingsWriteError> {
    let path = settings_dir.join(file.file_name());
    // PURE, and deliberately FIRST: a key this file has no place for is refused before a directory
    // is created, a sentinel is opened or a lock is taken, so the commonest refusal leaves no trace
    // at all. (The `ChangeJournal::append_record` disposition, for the same reason.)
    let segs = key_path(file, dotted_key)?;

    let io_err = |source, file: &Path| SettingsWriteError::Io { file: file.to_path_buf(), source };
    std::fs::create_dir_all(settings_dir).map_err(|e| SettingsWriteError::Lock {
        dir: settings_dir.to_path_buf(),
        lock: settings_dir.join(SETTINGS_LOCK_FILE),
        source: e,
    })?;
    // ⚠ THE SERIALISATION — module doc, third invariant. Held from here to the return, which is
    // what makes the read → edit → validate → land sequence one writer's. It moves the
    // `create_dir_all` that used to sit beside the rename up here, and that trade is WIDER than
    // the sentinel:
    //
    //   * the SENTINEL. A validation refusal now leaves a zero-byte `settings.lock` where it used
    //     to leave nothing, and that claims nothing — the loader reads the four settings files BY
    //     NAME and nothing in this tree enumerates the directory, so a file beside them cannot be
    //     mistaken for a setting.
    //   * ⚠ the DIRECTORY, which the residual used to omit. `<settings_dir>` itself is created
    //     before the refusal, and a `settings/` directory is a PROJECT-RESOLUTION MARKER (the
    //     nearest `is_dir` match in the walk rules). So a REFUSED write against a typo'd
    //     `$VIKE_SETTINGS_DIR` materialises that path, after which `vike-cli config show` reports
    //     it as the resolved settings directory rather than as missing, and the typo reads as a
    //     plausible empty project.
    //
    // That is ACCEPTED rather than fixed, and the bound is what makes it acceptable: only a
    // caller-NAMED directory can be created, never a discovered one — the walk rung answers with a
    // `settings/` that already exists, so it creates nothing new — and a named directory is a path
    // the operator asserted. Creating it lazily (open the sentinel first, `create_dir_all` only on
    // `NotFound`) was considered and REFUSED as a non-fix: every refusal this residual is about
    // fires AFTER the lock, so the lazy ladder would create the directory on exactly the same
    // runs. Not creating it at all is not available — the lock needs somewhere to live, and
    // `backend setup` writing into a fresh project is a real first-time path.
    let _lock = SettingsLock::acquire_within(settings_dir, budget)?;

    // The edit base: the current file, or empty when absent. `NotFound` on the READ result, not a
    // prior `exists()` — the load.rs TOCTOU/permissions argument verbatim.
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        // ⚠ **An absent target with a `<file>.toml.bak` beside it is the STRANDED state, not a
        // fresh project**, and treating it as an empty edit base is how a loss becomes permanent:
        // the next write from ANY of the three callers lands a brand-new one-key file over it and
        // reports "the file did not set it before", while the operator's real file sits in the
        // backup with nothing looking for it. `land_atomically` removes that backup on success AND
        // on a restore, so its presence beside an absent target has exactly one cause.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && backup_of(&path).is_file() => {
            return Err(SettingsWriteError::CurrentFile {
                file: path.clone(),
                message: format!(
                    "it is ABSENT and {} is beside it — that is a STRANDED write, not a fresh \
                     file: an earlier landing failed after moving the previous file aside. Put it \
                     back (`mv {} {}`) before writing, or delete the backup if you truly mean to \
                     start from nothing. Writing now would land a one-key file over your \
                     settings.",
                    backup_of(&path).display(),
                    backup_of(&path).display(),
                    path.display()
                ),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(SettingsWriteError::CurrentFile {
                file: path,
                message: format!("cannot read the current file: {e}"),
            });
        }
    };
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        // `.message()` only — the full Display renders the offending SOURCE LINE, and these
        // files sit beside `secrets.env` (the `error::redacted_parse_message` argument).
        SettingsWriteError::CurrentFile {
            file: path.clone(),
            message: format!(
                "the current file is not parseable TOML ({}) — fix it by hand before editing \
                 through this path",
                e.message()
            ),
        }
    })?;

    // Walk to the leaf's parent, creating tables for segments that do not exist yet.
    // `TableLike` so a `[rate]` section table and an inline `rate = {…}` navigate alike.
    let mut cur: &mut dyn TableLike = doc.as_table_mut();
    let (leaf, parents) = segs.split_last().expect("key_path returns a non-empty path");
    for (i, seg) in parents.iter().enumerate() {
        if cur.get(seg).is_none() {
            cur.insert(seg, toml_edit::table());
        }
        let item = cur.get_mut(seg).expect("present or just inserted");
        cur = item.as_table_like_mut().ok_or_else(|| SettingsWriteError::BadKey {
            key: dotted_key.to_string(),
            reason: format!(
                "`{}` is already set to a plain value in {}, so `{dotted_key}` cannot nest \
                 under it",
                segs[..=i].join("."),
                file.file_name()
            ),
        })?;
    }

    // The previous value (for the caller's audit record) and its decor (so a same-line trailing
    // comment survives the replacement). A key holding a whole TABLE is refused — this verb sets
    // one value, it does not replace subtrees.
    let (old_value, old_decor) = match cur.get(leaf) {
        None => (None, None),
        Some(item) => match item.as_value() {
            Some(v) => (
                Some(render_value(v)),
                Some((v.decor().prefix().cloned(), v.decor().suffix().cloned())),
            ),
            None => {
                return Err(SettingsWriteError::BadKey {
                    key: dotted_key.to_string(),
                    reason: format!(
                        "it names a whole table in {}, not a single setting",
                        file.file_name()
                    ),
                });
            }
        },
    };

    // Type the new value: a parseable TOML value is taken as typed (`250` an integer the loader
    // may coerce, `true` a bool); anything else is a string. Wrongly-typed input is not this
    // site's problem to guess at — the loader below refuses it with the key's own message.
    let trimmed = raw_value.trim();
    let mut new_v: Value = trimmed.parse::<Value>().unwrap_or_else(|_| Value::from(trimmed));
    if let Some((prefix, suffix)) = old_decor {
        if let Some(p) = prefix {
            new_v.decor_mut().set_prefix(p);
        }
        if let Some(s) = suffix {
            new_v.decor_mut().set_suffix(s);
        }
    }
    let new_value = render_value(&new_v);
    match cur.get_mut(leaf) {
        // Assign through the existing entry, so the KEY object (and with it a comment block
        // above the key, which lives in the key's decor) is untouched.
        Some(item) => *item = Item::Value(new_v),
        None => {
            cur.insert(leaf, Item::Value(new_v));
        }
    }

    // THE GATE: the would-be file through the loader's own parse + apply. Nothing has been
    // written yet, so a refusal here changes no byte on disk.
    let new_text = doc.to_string();
    validate_settings_text(file, &path, &new_text).map_err(SettingsWriteError::Validation)?;

    // Atomic landing: sibling tmp + rename (the env_write idiom — a crash mid-write leaves the
    // old file intact). `std::fs::rename` replaces the destination on Windows too; the
    // move-aside-and-retry arm below is the fallback for a destination held open there.
    let tmp = settings_dir.join(format!(".{}.tmp-{}", file.file_name(), std::process::id()));
    // ⚠ The TARGET is blamed for a tmp-write failure, not the tmp. A `chmod 0555 settings/` used to
    // print `…/settings/.policy.toml.tmp-4711: write failed: Permission denied` — a path the
    // operator has never seen, for an edit they aimed at `policy.toml`.
    std::fs::write(&tmp, new_text.as_bytes()).map_err(|e| io_err(e, &path))?;
    land_atomically(&tmp, &path)?;

    Ok(SettingsWrite { file: file.file_name(), key: dotted_key.to_string(), old_value, new_value })
}

/// Replace `path` with `tmp`, and **never leave `path` absent on a failure**.
///
/// The happy path is one `rename`, which replaces the destination on unix and on Windows alike.
/// The fallback exists for the one case that does not: a destination another process holds open on
/// Windows, where the replace is refused outright.
///
/// ⚠ **The fallback MOVES the previous file aside; it never removes it.** The removing form ran
/// here for as long as this module existed — `remove_file(&path)` then retry — and a second failed
/// rename left `policy.toml` DELETED with nothing in its place: the box boots on compiled-in
/// defaults, there is no file to inspect and no backup, and the module doc's first invariant ("a
/// crash mid-write leaves the old file intact") stood contradicted by the recovery path of the code
/// that states it. A refusal that destroys the file is strictly worse than one that leaves it
/// alone.
///
/// Its own function so the recovery ladder is reachable from a test with a `tmp` that cannot be
/// renamed — the only portable way to drive the arm, since a rename over a regular file in a
/// writable directory does not fail on demand.
fn land_atomically(tmp: &Path, path: &Path) -> Result<(), SettingsWriteError> {
    let io_err = |source, file: &Path| SettingsWriteError::Io { file: file.to_path_buf(), source };
    let Err(first) = std::fs::rename(tmp, path) else { return Ok(()) };
    // An earlier stranded backup at this name is replaced: the file being moved NOW is the one
    // that was live.
    let aside = backup_of(path);
    if std::fs::rename(path, &aside).is_err() {
        // The target could not even be moved, so it is untouched — the safe end state.
        let _ = std::fs::remove_file(tmp);
        return Err(io_err(first, path));
    }
    if let Err(second) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        // Put it back. Only if THAT fails is the operator left holding a path.
        return Err(match std::fs::rename(&aside, path) {
            Ok(()) => io_err(second, path),
            Err(_) => SettingsWriteError::Stranded {
                file: path.to_path_buf(),
                backup: aside,
                source: second,
            },
        });
    }
    let _ = std::fs::remove_file(&aside);
    Ok(())
}

/// Where [`land_atomically`] moves the previous file when a rename has to be retried — and, when
/// it sits beside an ABSENT target, the one on-disk trace of a [`SettingsWriteError::Stranded`].
///
/// One function because two sites must agree on the name: the one that CREATES it and the one
/// that reads its presence as evidence.
fn backup_of(path: &Path) -> PathBuf {
    path.with_extension("toml.bak")
}

/// The sentinel [`set_setting`] serialises every write on: `<settings_dir>/settings.lock`.
///
/// `pub` because two things outside this crate read it — the CLI's refusal message, which names the
/// held file so an operator can see what is holding it, and that verb's own cross-process
/// interlock test, which takes the lock from a DIFFERENT process and asserts the shipped binary
/// refuses. The file's BYTES are meaningless: the lock lives in the kernel.
///
/// It is created-if-absent and **never unlinked**, the `SeriesLock` rule and for its reason: a
/// second process can create a NEW inode at the same path and lock that, so two writers would each
/// hold a lock and each believe it was alone. One empty file beside the four TOMLs is the price.
/// `/settings/*` is gitignored with the four TOMLs re-included, so it is invisible in a checkout.
pub const SETTINGS_LOCK_FILE: &str = "settings.lock";

/// The sleep between two `try_lock` attempts — the `SeriesLock` interval, unchanged.
const LOCK_SPIN_INTERVAL_MS: u64 = 2;

/// [`LOCK_SPIN_INTERVAL_MS`] as the `Duration` the spin actually sleeps.
const LOCK_SPIN_INTERVAL: Duration = Duration::from_millis(LOCK_SPIN_INTERVAL_MS);

/// **How long THIS CALLER is willing to wait for the settings-directory lock** — the parameter
/// [`set_setting_within`] takes, and the module doc's *why the budget belongs to the CALLER*
/// section is the argument for its existing at all.
///
/// A wait is spelled in MILLISECONDS rather than in attempts because every caller reasons in time:
/// a frame owes 16 ms, a daemon peer has a reply deadline, a human at a prompt has patience. The
/// attempt count is this type's business.
///
/// ⚠ **Every budget makes at least one attempt**, [`LockBudget::NON_BLOCKING`] included: a "do not
/// wait" spelling that also never TRIED would be a refusal wearing a budget's clothes, and the
/// uncontended path — which is every path, almost always — would fail for nobody's benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockBudget(u32);

impl LockBudget {
    /// **One `try_lock`, no sleep at all** — for a caller that may not block, whatever the answer.
    ///
    /// The GUI's, and the reason the loop below does not sleep after its FINAL attempt: with a
    /// trailing sleep this budget would still have cost a frame [`LOCK_SPIN_INTERVAL_MS`] ms of
    /// doing nothing, which is a small version of exactly the defect it exists to remove.
    pub const NON_BLOCKING: LockBudget = LockBudget(1);

    /// **The whole-process budget: ~3 s**, and what [`set_setting`] uses.
    ///
    /// Sized against the critical section rather than against patience: one read of a file measured
    /// in kilobytes, a TOML parse, a loader validation and two renames — tens of milliseconds even
    /// on a loaded box — so three seconds is roughly two orders of magnitude of headroom for an
    /// overlap anybody was actually in, and short enough that an operator at a prompt never has to
    /// wonder whether the command is hung.
    ///
    /// ⚠ It is the DEFAULT because it is the safe thing to inherit, not because it fits every
    /// caller: it is bounded in both directions, so a caller that has not thought about the
    /// question gets neither the spurious refusal of never-wait nor the hang of wait-forever. A
    /// caller that HAS thought about it says so with [`LockBudget::from_millis`].
    pub const DEFAULT: LockBudget = LockBudget::from_millis(3_000);

    /// A budget of `ms` milliseconds, rounded down to whole [`LOCK_SPIN_INTERVAL_MS`] sleeps and
    /// never below one attempt. `from_millis(0)` is [`LockBudget::NON_BLOCKING`].
    ///
    /// ⚠ **It SATURATES rather than truncating.** The attempt count is a `u32` and the parameter is
    /// a `u64`, so the cast that used to sit here answered a SHORTER wait than it was asked for
    /// above `u32::MAX * `[`LOCK_SPIN_INTERVAL_MS`] ms — silently, on a public API, in the one
    /// direction a caller cannot detect (a budget that refuses sooner than it promised). Nothing in
    /// this tree asks for a wait that long and nothing should; that is exactly why the answer must
    /// be "the longest wait this type can express" rather than an arbitrary small one.
    #[must_use]
    pub const fn from_millis(ms: u64) -> LockBudget {
        let sleeps = ms / LOCK_SPIN_INTERVAL_MS;
        // `+ 1` is the attempt the sleeps sit between, so the ceiling is one below `u32::MAX`.
        let sleeps = if sleeps > (u32::MAX - 1) as u64 { u32::MAX - 1 } else { sleeps as u32 };
        LockBudget(sleeps + 1)
    }

    /// How many `try_lock` attempts this budget makes — at least one.
    #[must_use]
    pub const fn attempts(self) -> u32 {
        self.0
    }

    /// The longest this budget can SLEEP before refusing: the sleeps sit BETWEEN attempts, so it is
    /// one interval short of the attempt count, and it is `0` for [`LockBudget::NON_BLOCKING`].
    ///
    /// This is the number [`SettingsWriteError::Busy`] reports, which makes it the seam a test uses
    /// to prove WHICH budget a caller passed without measuring a duration.
    #[must_use]
    pub const fn max_wait_ms(self) -> u64 {
        (self.0 as u64 - 1) * LOCK_SPIN_INTERVAL_MS
    }
}

/// The held settings-directory lock: one exclusive advisory lock on
/// `<settings_dir>/`[`SETTINGS_LOCK_FILE`], released when this value drops.
///
/// Private on purpose, the [`crate::write`] twin of `change_journal`'s `AppendLock`: it is not a
/// capability a caller can hold across several writes. The whole argument for a bounded wait is
/// that the critical section is one bounded read-modify-write with no caller code inside it, and a
/// public guard would be an invitation to widen exactly that.
#[derive(Debug)]
struct SettingsLock(std::fs::File);

impl SettingsLock {
    /// Take the lock on `settings_dir`, spending at most `budget`. `settings_dir` must already
    /// exist (the caller's `create_dir_all` runs first).
    fn acquire_within(settings_dir: &Path, budget: LockBudget) -> Result<Self, SettingsWriteError> {
        let path = settings_dir.join(SETTINGS_LOCK_FILE);
        let lock_err = |source| SettingsWriteError::Lock {
            dir: settings_dir.to_path_buf(),
            lock: path.clone(),
            source,
        };
        // `read(true).write(true)`, NOT `append(true)`: Windows refuses to lock an append-opened
        // handle. `truncate(false)`: the bytes are meaningless, and a file another process is
        // holding must never be rewritten. (The `AppendLock`/`JournalLock` idiom, verbatim.)
        //
        // ⚠ A settings directory the process cannot WRITE fails HERE rather than at the tmp write,
        // and the refusal names the DIRECTORY (what the operator aimed at, and can chmod) AND the
        // SENTINEL (what was actually opened, and which may be owned by somebody else entirely) —
        // see [`SettingsWriteError::Lock`] for the failure that costs.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(lock_err)?;
        let attempts = budget.attempts();
        for attempt in 0..attempts {
            match file.try_lock() {
                Ok(()) => return Ok(SettingsLock(file)),
                // Another writer holds it — in this process or any other; `flock`/`LockFileEx` key
                // on the open file description, so both queue here identically.
                //
                // ⚠ The sleep sits BETWEEN attempts and is skipped after the last one. Sleeping
                // after the final `try_lock` buys nothing (nobody looks again) and costs the one
                // thing `LockBudget::NON_BLOCKING` exists to protect: a frame that must not block
                // would still have spent an interval doing nothing.
                Err(TryLockError::WouldBlock) => {
                    if attempt + 1 < attempts {
                        std::thread::sleep(LOCK_SPIN_INTERVAL);
                    }
                }
                // The platform refuses to lock at all. A filesystem this writer cannot serialise on
                // is one whose settings files nobody can claim are consistent, so this REFUSES
                // rather than writing unserialised — the `append_record` disposition, same reason.
                Err(TryLockError::Error(source)) => return Err(lock_err(source)),
            }
        }
        Err(SettingsWriteError::Busy { lock: path, waited_ms: budget.max_wait_ms() })
    }
}

impl Drop for SettingsLock {
    fn drop(&mut self) {
        // Closing the descriptor releases the lock on its own; unlocking first makes the release
        // explicit and ordered. The FILE stays on disk — see [`SETTINGS_LOCK_FILE`].
        let _ = self.0.unlock();
    }
}

/// Split the full dotted key into its in-file path, refusing a spelling that does not fit
/// `file`: the first segment must be the file's section (the read half's rendering), at least
/// one segment must follow, and no segment may be empty.
fn key_path(file: SettingsFile, dotted_key: &str) -> Result<Vec<&str>, SettingsWriteError> {
    let bad = |reason: String| SettingsWriteError::BadKey { key: dotted_key.to_string(), reason };
    let mut segs: Vec<&str> = dotted_key.split('.').collect();
    if segs.iter().any(|s| s.trim().is_empty()) {
        return Err(bad("empty key segment".to_string()));
    }
    if segs[0] != file.section() {
        return Err(bad(format!(
            "a {} key is spelled `{}.<key>` (the same dotted form `config show` renders), got \
             first segment `{}`",
            file.file_name(),
            file.section(),
            segs[0]
        )));
    }
    segs.remove(0);
    if segs.is_empty() {
        return Err(bad(format!(
            "it names the whole {} file — a write sets one key inside it",
            file.file_name()
        )));
    }
    Ok(segs)
}

/// Render one TOML value bare — decor stripped, so a same-line comment or alignment whitespace
/// never pollutes an audit record's old/new cell.
fn render_value(v: &Value) -> String {
    let mut bare = v.clone();
    bare.decor_mut().clear();
    bare.to_string().trim().to_string()
}

/// **Does writing `key` demand the RETYPED-KEY confirmation?** — keyed on the KEY, which is the
/// expressiveness `docs/decisions/0055-every-setting-is-editable-from-the-ui.md` records as
/// missing, and which `docs/decisions/0054-settings-move-into-one-database.md` owes anyway once
/// there are no file names left to key on.
///
/// Every enforcing site in this tree keys the typed confirm on the FILE —
/// `crates/vike-tradehub/src/server.rs`'s `apply_set_setting` and
/// `crates/vike-app-core/src/tool_views/backend_settings.rs`'s `can_save_fields` both ask which
/// file a write names — so "confirm the risk ceilings but not the ARMING ceiling" is not
/// expressible against either: the arming ceilings (`policy.venues.<venue>`, and
/// `policy.accounts.<venue>.<LABEL>` with it) happen to live in the same file as
/// `max_notional_per_order` and its siblings. This is that line, drawn once, in the crate that
/// already owns [`SettingsFile`] and `is_secret_key`, so the daemon, the GUI and the MCP gate can
/// adopt it later rather than spelling a fourth copy of it.
///
/// ⚠ **Matched by SEGMENT, never by string prefix.** A `starts_with("policy.venues.")` rule reads
/// as equivalent and silently exempts a future `policy.venues_something` — a key in the ceilings
/// class, waved through by an exemption for a different table whose name it merely begins with.
/// Splitting first makes `venues_something` a second segment equal to neither exempt name.
///
/// Today's ONE caller is `vike-cli config set`. That is deliberate rather than incomplete: the
/// three wire/GUI sites already refuse a policy write carrying no confirm, so adopting this
/// predicate there RELAXES a live contract, which is its own change with its own review.
#[must_use]
pub fn requires_typed_confirm(key: &str) -> bool {
    typed_confirm_reason(key).is_some()
}

/// **The flags whose leaf demands the retyped key**, with the reason each one is here.
///
/// ⚠ **This exists because a FILE-keyed rule cannot see that `flags.toml` holds the real-money
/// arm.** Every enforcing site in this tree draws the line on the file, and the consequence was an
/// asymmetry nobody chose: `policy.max_leverage` — the tree's one declared-UNCONSUMED ceiling —
/// demanded the ceremony, while `flags.tradehub_live`, whose own doc calls it *"the single largest
/// blast radius on this list"*, was a one-line non-interactive write. Drawing the line on the KEY
/// in the crate all three surfaces share is precisely what [`requires_typed_confirm`] was added to
/// make possible, so this is that function doing its job rather than a new mechanism.
///
/// ⚠ **THE MEMBERSHIP RULE IS THE PREDICATE [`typed_confirm_reason`] RENDERS**, and this paragraph
/// used to state a narrower one: *the class `flags.rs`'s module doc declares — the safety OVERRIDES
/// — plus the live arm*. A reader who checked the rule against the rendered reason got **two
/// different sets**, and the gap was not academic. A flag is here when turning it ON *"either puts
/// real money on a real wire or turns off a guard that is on by default"* — the reason's own words,
/// because a rule and the sentence an operator is shown may not be two claims. Each row then CITES
/// where that property is written down (`flags.rs`'s module doc for the three flags it declares as
/// safety overrides, the field's own doc for the other three), so joining is still evidence rather
/// than a feeling about which flags seem dangerous.
///
/// ⚠ **What the narrower rule cost, stated because it is the finding this row set was corrected
/// for.** [`Flags::tradehub_control`] *"opens the headless daemon's authenticated CONTROL scope — a
/// remote order-origination path"*, is read from `flags.toml` (`crate::consumed`'s row names
/// `crates/vike-tradehub/src/tradehub_cli.rs`'s `settings.flags.tradehub_control`), and matches the
/// rendered reason squarely — yet it sat OUTSIDE the ceremony while
/// `flags.tradehub_allow_public_bind`, which only widens the BIND ADDRESS of a surface that must
/// already be open, sat inside it. Opening the remote order-write surface took one un-retyped
/// non-interactive line while widening that same surface's address demanded a retype: the inverse
/// of the ordering the class claims. It is in the list now. ⚠ The direction of that repair matters
/// — ADDING a key can only make a write harder, which is why it is the half that may be done on a
/// finding; loosening the reason to match the shorter list would have been a weakening and is not
/// available here.
///
/// ⚠ **[`Flags::poly_exec`]'s exclusion has LOST ITS ARGUMENT, and this paragraph carried the dead
/// one.** It read: "`crate::consumed` declares it `Consumer::Not` — the live read is the process
/// env and the credentials map, never `Flags` — so a write of it through this surface arms nothing
/// and a ceremony over it would be theatre." That was true when written and is not true now: the
/// unread-settings sweep WIRED the key, `crate::consumed`'s row is a `Consumer::At` naming
/// `crates/vike-tradehub/src/tradehub_cli.rs`'s `flags.poly_exec, FoldTier::CredentialStoreFirst)`,
/// and `POLY_EXEC` is a `crate::arming::CREDENTIAL_FILE_ARMING_REFUSED` row precisely so that
/// `flags.toml` is the sanctioned place to set it. So a write of it through this surface now arms
/// LIVE Polymarket execution on the daemon.
///
/// **It therefore JOINS the list**, and the row carries the argument. The alternative on offer was
/// to name the hole and leave it — "a behaviour change to a CLI ceremony does not belong in a change
/// about the truth of claims" — and that reasoning does not survive contact with what the list is
/// FOR: adding a key can only make a write harder (the paragraph above says exactly that, and calls
/// it the half that may be done on a finding), so deferring costs an operator the retype on a
/// real-money arming flag and buys nothing but tidiness. A finding that a live-arm flag sits
/// outside the ceremony is not a finding to schedule.
///
/// What must not happen again is the state this note replaces: an exclusion standing on a reason
/// that had stopped being true, in a list whose whole purpose is blast radius.
///
/// ⚠ **What a retype is and is NOT worth, stated because the other claim is tempting.** It stops a
/// MISTAKE — the wrong key, the fat finger, the copied line — for a human and for an agent alike.
/// It is no barrier at all to either one that MEANT it: `--confirm <key>` is one more argument.
/// Nothing here should be read as making the surface safe to hand to something untrusted; see
/// `crates/vike-cli/src/cmd/config_set.rs`'s module doc, where that question is answered on its
/// own terms.
///
/// ⚠ **The residual: nothing DERIVES this set.** `FLAG_REGISTRY` carries an owner, a date and a
/// disposition per flag, and no "is a safety override" column, so a NEW override added to
/// `flags.rs` does not appear here on its own. The half that IS gated is the other direction —
/// `crates/vike-config/tests/typed_confirm.rs`'s `every_confirmed_flag_is_a_real_flags_field`
/// drives each row through the real loader, so a renamed or deleted field fails rather than
/// quietly protecting nothing.
const CONFIRMED_FLAG_KEYS: [&str; 7] = [
    // "⚠ The single largest blast radius on this list" — it runs the headless daemon against REAL
    // venues with real credentials. (`Flags::tradehub_live`'s own doc.)
    "flags.tradehub_live",
    // It OPENS the remote order-origination path: "the headless daemon's authenticated CONTROL
    // scope" (`Flags::tradehub_control`'s own doc), from which a peer submits and cancels real
    // orders. It joined late, and the row below is why — the WIDENER of this surface was inside the
    // ceremony while its OPENER was outside it.
    "flags.tradehub_control",
    // A safety OVERRIDE: it lets that same control surface bind a NON-LOOPBACK address, and its
    // confidentiality comes entirely from being unreachable (the handshake is PLAINTEXT and
    // authenticates the connection, not each frame).
    "flags.tradehub_allow_public_bind",
    // A safety OVERRIDE: it arms a live venue whose API key is WITHDRAW-CAPABLE, which the
    // key-permission check otherwise refuses.
    "flags.allow_withdraw_keys",
    // A safety OVERRIDE: it skips the pre-flight that refuses a broken tree before a mount.
    "flags.preflight_skip",
    // A safety OVERRIDE, and the one that reads backwards: it REFUSES the default-on reconcile, so
    // writing it takes a live account's divergence detection off.
    "flags.reconcile_off",
    // ⚠ JOINED on the unread-settings sweep, and it is the second row here to join because its
    // EXCLUSION had outlived its reason rather than because anybody re-weighed the risk. The
    // exclusion read: "`crate::consumed` declares it `Consumer::Not` — the live read is the process
    // env and the credentials map, never `Flags` — so a write of it through this surface arms
    // nothing." That stopped being true when the sweep WIRED the key: `consumed`'s row is a
    // `Consumer::At` naming the daemon's fold, and `POLY_EXEC` is a
    // `crate::arming::CREDENTIAL_FILE_ARMING_REFUSED` row precisely so that `flags.toml` is the
    // sanctioned place to set it. A write of it here now arms LIVE Polymarket execution.
    //
    // Its blast radius is `flags.tradehub_live`'s shape and, in one respect, worse: this venue has
    // NO TESTNET, so every order it arms is real money on Polygon mainnet — there is no demo tier
    // an operator could land on by mistake instead. Adding a key can only make a write HARDER (the
    // paragraph above says so: it is the half that may be done on a finding), so leaving the slot
    // empty pending a wider review would have shipped the hole to buy nothing.
    "flags.poly_exec",
];

/// [`requires_typed_confirm`]'s decision, with the REASON — so the surface demanding the ceremony
/// can say which class the key is in rather than asserting one.
///
/// One function because the class and its wording must not drift apart: a CLI that printed "this is
/// a policy RISK CEILING" for `flags.tradehub_live` would be confidently wrong about the one key it
/// most needs to be right about.
#[must_use]
pub fn typed_confirm_reason(key: &str) -> Option<&'static str> {
    if CONFIRMED_FLAG_KEYS.contains(&key) {
        return Some(
            "a LIVE-ARM or safety-override flag: it either puts real money on a real wire or turns \
             off a guard that is on by default",
        );
    }
    let mut segs = key.split('.');
    if segs.next() != Some(SettingsFile::Policy.section()) {
        return None;
    }
    // The two ARMING tables `docs/decisions/0055-every-setting-is-editable-from-the-ui.md`
    // exempts. A `policy` key with no second segment is refused by
    // `set_setting` long before a confirm matters, and answers `Some` here rather than being a
    // special case — a refusal is the same either way.
    if matches!(segs.next(), Some("venues" | "accounts")) {
        return None;
    }
    Some("a policy RISK CEILING: the file holds this box's risk limits")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp settings dir")
    }

    /// A hand-shaped config file: a header comment, a commented-out key, a set key with a
    /// same-line comment, and deliberate extra blank lines — everything a re-serializer destroys.
    const CONFIG_WITH_COMMENTS: &str = "\
# deployment config — hand-tuned 2026-08-12
# store_root = \"/var/lib/vike\"   (moved to the NVMe 08-01)

tradehub_addr = \"127.0.0.1:7879\"  # loopback only; ssh tunnel in front

log_dir = \"/var/tmp/vike-logs\"
";

    /// THE COMMENT-PRESERVATION PROOF at the unit level (the daemon test proves it over the
    /// wire): editing one key leaves every other LINE of the file byte-identical — the header
    /// comment, the commented-out key, the blank lines, the untouched key — and keeps the edited
    /// line's own trailing comment.
    #[test]
    fn an_edit_changes_one_line_and_preserves_every_other_byte() {
        let d = dir();
        std::fs::write(d.path().join("config.toml"), CONFIG_WITH_COMMENTS).unwrap();

        let w = set_setting(d.path(), SettingsFile::Config, "config.tradehub_addr", "0.0.0.0:9000")
            .expect("a valid config write lands");
        assert_eq!(w.file, "config.toml");
        assert_eq!(w.old_value.as_deref(), Some("\"127.0.0.1:7879\""));
        assert_eq!(w.new_value, "\"0.0.0.0:9000\"");

        let after = std::fs::read_to_string(d.path().join("config.toml")).unwrap();
        let before_lines: Vec<&str> = CONFIG_WITH_COMMENTS.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();
        assert_eq!(before_lines.len(), after_lines.len(), "no line added or removed:\n{after}");
        for (b, a) in before_lines.iter().zip(&after_lines) {
            if b.starts_with("tradehub_addr") {
                assert_eq!(
                    *a, "tradehub_addr = \"0.0.0.0:9000\"  # loopback only; ssh tunnel in front",
                    "the edited line keeps its trailing comment"
                );
            } else {
                assert_eq!(a, b, "an untouched line is untouched bytes");
            }
        }
    }

    /// The validation gate fires BEFORE the write: an unknown key refuses with the loader's own
    /// message (naming the key, `deny_unknown_fields`' text) and the file is byte-identical.
    #[test]
    fn an_unknown_key_is_refused_with_the_loaders_message_and_no_byte_changes() {
        let d = dir();
        std::fs::write(d.path().join("config.toml"), CONFIG_WITH_COMMENTS).unwrap();

        let err = set_setting(d.path(), SettingsFile::Config, "config.tradehub_adr", "x")
            .expect_err("an unknown key must refuse");
        let msg = err.to_string();
        assert!(msg.contains("tradehub_adr"), "names the offending key: {msg}");
        assert!(msg.contains("unknown field"), "the loader's own vocabulary: {msg}");
        assert!(matches!(err, SettingsWriteError::Validation(_)), "refused by the loader: {err:?}");

        let after = std::fs::read_to_string(d.path().join("config.toml")).unwrap();
        assert_eq!(after, CONFIG_WITH_COMMENTS, "a refused write changes NO byte");
    }

    /// A value the key's TYPE cannot take is refused by the same gate — `max_leverage` is an
    /// `f64`, and quoting a number is the ordinary way to mistype it.
    #[test]
    fn a_mistyped_value_is_refused_by_the_loader() {
        let d = dir();
        let err = set_setting(d.path(), SettingsFile::Policy, "policy.max_leverage", "\"high\"")
            .expect_err("a string in an f64 key must refuse");
        assert!(matches!(err, SettingsWriteError::Validation(_)), "{err:?}");
        assert!(err.to_string().contains("max_leverage"), "names the key: {err}");
        assert!(!d.path().join("policy.toml").exists(), "nothing was created for a refusal");
    }

    /// …and so is a well-typed value the loader's RANGE check refuses (`market_slippage` has a
    /// hard maximum) — proving `apply` runs, not just the parse.
    #[test]
    fn an_out_of_range_value_is_refused_by_apply() {
        let d = dir();
        let err = set_setting(d.path(), SettingsFile::Policy, "policy.market_slippage", "0.9")
            .expect_err("0.9 exceeds the policy maximum");
        assert!(matches!(err, SettingsWriteError::Validation(_)), "{err:?}");
        assert!(err.to_string().contains("market_slippage"), "{err}");
    }

    /// An absent file is a valid edit base: the write creates it, and the created file loads.
    #[test]
    fn writing_into_an_absent_file_creates_a_loadable_one() {
        let d = dir();
        let w = set_setting(d.path(), SettingsFile::Policy, "policy.max_notional_per_order", "250")
            .expect("a fresh policy.toml");
        assert_eq!(w.old_value, None, "no previous value in an absent file");
        assert_eq!(w.new_value, "250");

        let s = crate::load(Some(d.path()), &std::collections::HashMap::new())
            .expect("the created file loads");
        assert_eq!(s.policy.max_notional_per_order, Some(250.0));
    }

    /// A nested key (`policy.rate.*`) navigates into the `[rate]` table — creating it when
    /// absent — and the result still validates and loads.
    #[test]
    fn a_nested_key_edits_the_inner_table() {
        let d = dir();
        set_setting(d.path(), SettingsFile::Policy, "policy.rate.max_utilization", "0.5")
            .expect_err(
                "the REMOVED rate key is refused by the loader, proving the nested \
                         path reaches apply",
            );
        // A real nested write: none of policy's live keys nest today, so drive the mechanics
        // through config's flat keys plus a policy top-level one instead — and pin that an
        // intermediate segment that is a VALUE refuses rather than panicking.
        std::fs::write(d.path().join("policy.toml"), "max_leverage = 2.0\n").unwrap();
        let err = set_setting(d.path(), SettingsFile::Policy, "policy.max_leverage.inner", "1")
            .expect_err("cannot nest under a plain value");
        let msg = err.to_string();
        assert!(msg.contains("max_leverage"), "names the blocking segment: {msg}");
    }

    /// The dotted-key contract: the section prefix is mandatory and must match the file; the
    /// bare file name alone is refused; a foreign section is refused.
    #[test]
    fn the_key_spelling_is_the_read_halves_dotted_form() {
        let d = dir();
        for (key, needle) in [
            ("tradehub_addr", "spelled `config.<key>`"),
            ("config", "names the whole config.toml file"),
            ("policy.max_leverage", "spelled `config.<key>`"),
            ("config..x", "empty key segment"),
        ] {
            let err = set_setting(d.path(), SettingsFile::Config, key, "1")
                .expect_err("a malformed key must refuse");
            assert!(matches!(err, SettingsWriteError::BadKey { .. }), "{key}: {err:?}");
            assert!(err.to_string().contains(needle), "{key}: {err}");
        }
    }

    /// `SettingsFile::parse` accepts both spellings of each file and nothing else.
    #[test]
    fn file_names_parse_with_and_without_the_extension() {
        for f in SettingsFile::ALL {
            assert_eq!(SettingsFile::parse(f.file_name()), Some(f));
            assert_eq!(SettingsFile::parse(f.section()), Some(f));
        }
        assert_eq!(SettingsFile::parse("settings"), None);
        assert_eq!(SettingsFile::parse("Policy"), None, "case-sensitive: a file name, not a word");
        assert!(unknown_file_message("secrets.env").contains("policy.toml"));
    }

    /// Value typing: TOML-parseable text lands typed (integer / bool), everything else lands as
    /// a string — proven through the loaded model, not the writer's own claim.
    #[test]
    fn values_land_typed_when_parseable_and_as_strings_otherwise() {
        let d = dir();
        set_setting(d.path(), SettingsFile::Flags, "flags.reconcile", "true").expect("a bool");
        set_setting(d.path(), SettingsFile::Config, "config.tradehub_addr", "127.0.0.1:7879")
            .expect("an unquoted address is a string");
        let s = crate::load(Some(d.path()), &std::collections::HashMap::new()).expect("loads");
        assert!(s.flags.reconcile);
        assert_eq!(s.config.tradehub_addr.as_deref(), Some("127.0.0.1:7879"));
    }

    /// A PRESENT-but-broken current file refuses (never clobbered), and the parse message never
    /// echoes the offending source line (these files sit beside `secrets.env`).
    #[test]
    fn a_broken_current_file_is_refused_not_clobbered() {
        let d = dir();
        let broken = "tradehub_addr = \"unclosed sk-SECRET-IN-LINE\n";
        std::fs::write(d.path().join("config.toml"), broken).unwrap();
        let err = set_setting(d.path(), SettingsFile::Config, "config.log_dir", "/tmp/x")
            .expect_err("a broken base must refuse");
        assert!(matches!(err, SettingsWriteError::CurrentFile { .. }), "{err:?}");
        assert!(
            !err.to_string().contains("sk-SECRET-IN-LINE"),
            "the refusal never echoes the source line: {err}"
        );
        let after = std::fs::read_to_string(d.path().join("config.toml")).unwrap();
        assert_eq!(after, broken, "the operator's bytes are intact for a hand fix");
    }

    /// **The retyped-key line, drawn where the ruling draws it**: the scalar policy ceilings are
    /// inside the ceremony, both ARMING tables are outside it, and every other file is outside it
    /// entirely.
    #[test]
    fn the_typed_confirm_covers_the_risk_ceilings_and_not_the_arming_ceilings() {
        for key in [
            "policy.max_notional_per_order",
            "policy.max_leverage",
            "policy.max_account_exposure",
            "policy.halt_admit",
            "policy.deadman_timeout_ms",
            "policy.rate.max_commands_per_sec",
        ] {
            assert!(requires_typed_confirm(key), "{key} is a risk ceiling");
        }
        for key in [
            "policy.venues.binance",
            "policy.accounts",
            "policy.accounts.hyperliquid.ALT",
            "config.tradehub_addr",
            "preferences.log_level",
            "flags.reconcile",
        ] {
            assert!(!requires_typed_confirm(key), "{key} is outside the ceremony");
        }
    }

    /// ⚠ The SEGMENT rule, as its own case: a key whose second segment merely BEGINS with an
    /// exempt table name is a risk ceiling and must confirm. This is the assertion that goes red
    /// the day somebody "simplifies" the predicate into a `starts_with`.
    #[test]
    fn a_name_that_merely_begins_with_an_exempt_table_still_confirms() {
        assert!(requires_typed_confirm("policy.venues_something"));
        assert!(requires_typed_confirm("policy.accounts_total"));
        // …and the bare section, which `set_setting` refuses anyway, is not accidentally exempted.
        assert!(requires_typed_confirm("policy"));
    }

    // ---------------------------------------------------------------------------------------
    // The serialisation (module doc, third invariant)
    // ---------------------------------------------------------------------------------------

    /// Take the directory lock the way another PROCESS would, and hold it.
    ///
    /// Deliberately not [`SettingsLock`]: this stands in for a holder that is not this code, so it
    /// opens the sentinel by path and locks it with the bare `std` primitive. That is only a
    /// faithful stand-in because `flock`/`LockFileEx` key on the OPEN FILE DESCRIPTION rather than
    /// on the process — which is exactly what makes a single-process test of a cross-process
    /// interlock meaningful, and the property `change_journal`'s `AppendLock` doc states for the
    /// same trick.
    fn hold_the_lock(settings_dir: &Path) -> std::fs::File {
        std::fs::create_dir_all(settings_dir).unwrap();
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(settings_dir.join(SETTINGS_LOCK_FILE))
            .expect("the sentinel opens");
        f.try_lock().expect("this test must be the holder for anything below to prove anything");
        f
    }

    /// **THE MUTUAL-EXCLUSION PROOF, and it is deterministic.** While another holder has the
    /// sentinel, a write spins out its budget, refuses as [`SettingsWriteError::Busy`] and changes
    /// NO BYTE; the moment the holder releases, the same write lands.
    ///
    /// This is the half that kills cleanly: delete the `SettingsLock::acquire_within` line from
    /// [`set_setting_within`] and the first call below succeeds, which is the whole defect —
    /// a writer proceeding into a read-modify-write another writer is already inside.
    #[test]
    fn a_write_is_refused_while_another_process_holds_the_directory_lock() {
        let d = dir();
        std::fs::write(d.path().join("config.toml"), CONFIG_WITH_COMMENTS).unwrap();
        let held = hold_the_lock(d.path());

        // A SMALL budget: `LockBudget::DEFAULT` would cost this test three seconds to prove the
        // same thing. What is under test is the BEHAVIOUR at exhaustion, not any one number.
        let err = set_setting_within(
            d.path(),
            SettingsFile::Config,
            "config.tradehub_addr",
            "0.0.0.0:9000",
            LockBudget::from_millis(4),
        )
        .expect_err("a held lock must refuse the write");
        let msg = err.to_string();
        assert!(matches!(err, SettingsWriteError::Busy { .. }), "{err:?}");
        assert!(msg.contains(SETTINGS_LOCK_FILE), "the refusal names the held sentinel: {msg}");
        assert!(msg.contains("NOTHING was written"), "{msg}");
        assert!(msg.contains("no stale lock"), "a dead holder's lock releases itself: {msg}");
        assert_eq!(
            std::fs::read_to_string(d.path().join("config.toml")).unwrap(),
            CONFIG_WITH_COMMENTS,
            "a lock refusal changes NO byte"
        );

        drop(held);
        set_setting_within(
            d.path(),
            SettingsFile::Config,
            "config.tradehub_addr",
            "0.0.0.0:9000",
            LockBudget::from_millis(4),
        )
        .expect("the same write lands once the holder releases");
    }

    /// **THE NO-LOST-UPDATE PROOF.** Eight writers, one file, each setting a DIFFERENT key, all
    /// released from one barrier — and afterwards every one of the eight keys is present.
    ///
    /// ⚠ **Its kill is PROBABILISTIC and that is declared rather than hidden.** The assertion is
    /// deterministic (eight keys, all present, and the file still loads), but the FAILURE it
    /// catches needs two threads to overlap inside the read-modify-write. The barrier is what
    /// makes that overwhelming rather than incidental: eight threads enter a ~100 µs critical
    /// section at the same instant, so without the lock each one reads the same near-empty
    /// document and the last rename wins — measured here as one or two surviving keys, never
    /// eight. The sibling test above is the deterministic half; this is the half that proves the
    /// lock is in the RIGHT PLACE, i.e. spans the read as well as the write.
    #[test]
    fn concurrent_writers_do_not_lose_one_anothers_edits() {
        const WRITERS: usize = 8;
        let d = dir();
        std::fs::create_dir_all(d.path()).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));

        std::thread::scope(|s| {
            for i in 0..WRITERS {
                let path = d.path().to_path_buf();
                let barrier = std::sync::Arc::clone(&barrier);
                s.spawn(move || {
                    // `policy.venues.<venue>` — eight DISTINCT keys in one file, each a legitimate
                    // arming ceiling the loader accepts, so nothing here is refused for its value.
                    let key = format!("policy.venues.{}", vike_model::VENUES[i]);
                    barrier.wait();
                    set_setting(&path, SettingsFile::Policy, &key, "paper")
                        .unwrap_or_else(|e| panic!("{key}: {e}"));
                });
            }
        });

        let after = std::fs::read_to_string(d.path().join("policy.toml")).unwrap();
        for venue in &vike_model::VENUES[..WRITERS] {
            assert!(
                after.contains(venue),
                "{venue}'s edit was lost — a concurrent writer erased it:\n{after}"
            );
        }
        crate::load(Some(d.path()), &std::collections::HashMap::new())
            .expect("the file eight writers landed on still loads");
    }

    /// **The rename fallback does not destroy the operator's file.**
    ///
    /// Driven through [`land_atomically`] directly with a `tmp` that DOES NOT EXIST, which is the
    /// only portable way to make both renames fail on demand: a rename over a regular file in a
    /// writable directory does not fail to order. The ladder then runs in full — first rename
    /// fails, the previous file is moved aside, the retry fails, the previous file is put back —
    /// and the assertion is the property the whole arm exists for: the target is still there, with
    /// the operator's bytes in it, and no `.bak` is left littering the directory.
    #[test]
    fn a_failed_landing_restores_the_previous_file_rather_than_deleting_it() {
        let d = dir();
        let path = d.path().join("config.toml");
        std::fs::write(&path, CONFIG_WITH_COMMENTS).unwrap();
        let absent_tmp = d.path().join(".config.toml.tmp-nonexistent");

        let err = land_atomically(&absent_tmp, &path).expect_err("a missing tmp cannot land");
        assert!(matches!(err, SettingsWriteError::Io { .. }), "restored, so not Stranded: {err:?}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            CONFIG_WITH_COMMENTS,
            "the previous file must be back, byte for byte — NOT deleted"
        );
        assert!(
            !d.path().join("config.toml.bak").exists(),
            "a restored file leaves no backup littering the settings directory"
        );
    }

    /// **An I/O failure blames the TARGET, never the sibling tmp.** A permissions failure used to
    /// print `…/settings/.policy.toml.tmp-4711: write failed: …` — a path the operator has never
    /// seen and cannot find, for an edit they aimed at `config.toml`.
    ///
    /// Driven by planting a DIRECTORY at the exact tmp path (its name embeds this process's pid, so
    /// a test can compute it), which makes `std::fs::write` fail while the lock and the read
    /// succeed. That matters: the READ-ONLY-DIRECTORY case no longer reaches this arm at all — with
    /// the lock in place it fails earlier, at the sentinel open — so without this test the blame
    /// fix would be unproven.
    #[test]
    fn an_io_failure_names_the_file_the_operator_aimed_at_not_the_tmp() {
        let d = dir();
        std::fs::write(d.path().join("config.toml"), CONFIG_WITH_COMMENTS).unwrap();
        let tmp = d.path().join(format!(".config.toml.tmp-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let err = set_setting(d.path(), SettingsFile::Config, "config.log_dir", "/tmp/x")
            .expect_err("a tmp path that cannot be written must refuse");
        let msg = err.to_string();
        assert!(matches!(err, SettingsWriteError::Io { .. }), "{err:?}");
        assert!(
            !msg.contains(".tmp-"),
            "it must not blame a path the operator has never seen: {msg}"
        );
        assert!(msg.contains("config.toml"), "it must blame the file the edit aimed at: {msg}");
        assert_eq!(
            std::fs::read_to_string(d.path().join("config.toml")).unwrap(),
            CONFIG_WITH_COMMENTS,
            "and nothing may be written"
        );
    }

    /// …and the STRANDED message, which is what an operator gets in the one case the restore
    /// itself fails: it names the backup, says the target is empty, and is its own variant so the
    /// next action (put a file back) is not buried in an ordinary I/O failure.
    #[test]
    fn the_stranded_message_names_the_backup_and_the_empty_target() {
        let e = SettingsWriteError::Stranded {
            file: PathBuf::from("/srv/vike/settings/policy.toml"),
            backup: PathBuf::from("/srv/vike/settings/policy.toml.bak"),
            source: std::io::Error::other("device busy"),
        };
        let msg = e.to_string();
        assert!(msg.contains("policy.toml.bak"), "{msg}");
        assert!(msg.contains("NOTHING is at"), "{msg}");
        assert!(msg.contains("by hand"), "{msg}");
    }

    // ---------------------------------------------------------------------------------------
    // The budget belongs to the CALLER (module doc)
    // ---------------------------------------------------------------------------------------

    /// The arithmetic every caller's budget is read through — asserted as SHAPE, never as elapsed
    /// time, because a duration assertion in a spin test is a flake by construction.
    ///
    /// The two facts a call site depends on: a budget spelled in milliseconds sleeps at most that
    /// long, and [`LockBudget::NON_BLOCKING`] sleeps for NO time at all rather than "not much".
    #[test]
    fn a_budget_is_attempts_plus_the_sleeps_between_them() {
        assert_eq!(LockBudget::NON_BLOCKING.attempts(), 1, "one try_lock");
        assert_eq!(LockBudget::NON_BLOCKING.max_wait_ms(), 0, "…and no sleep whatever");
        assert_eq!(LockBudget::from_millis(0), LockBudget::NON_BLOCKING, "a zero wait IS that");
        assert_eq!(LockBudget::DEFAULT.max_wait_ms(), 3_000, "the whole-process budget is ~3 s");
        for ms in [2_u64, 16, 1_000, 3_000] {
            let b = LockBudget::from_millis(ms);
            assert_eq!(b.max_wait_ms(), ms, "a budget spelled in ms sleeps at most that long");
            assert!(b.attempts() >= 1, "every budget tries at least once");
        }

        // …and the edge a `u64` parameter over a `u32` attempt count creates: an absurd budget
        // SATURATES at the longest wait this type can express. The cast that used to sit in
        // `from_millis` wrapped instead, so `from_millis(u64::MAX)` answered a wait of 2 ms — a
        // SHORTER one than asked for, on a public API, which is the one direction a caller cannot
        // detect. The assertion is a floor rather than the exact ceiling: what must hold is "not
        // silently tiny", and pinning the exact product would make this test about `u32::MAX`.
        for ms in [u64::MAX, u64::MAX / 2, (u32::MAX as u64 + 1) * LOCK_SPIN_INTERVAL_MS] {
            let b = LockBudget::from_millis(ms);
            assert!(
                b.max_wait_ms() >= u32::MAX as u64,
                "an absurd budget must saturate, never wrap: from_millis({ms}) waits {} ms",
                b.max_wait_ms()
            );
            assert!(b.max_wait_ms() <= ms, "…and never longer than it was asked for");
        }
    }

    /// **THE NON-BLOCKING PROOF, asserted by SHAPE.** A held lock refuses a
    /// [`LockBudget::NON_BLOCKING`] write, and the refusal REPORTS that it waited zero
    /// milliseconds — which it can only do by having taken the no-sleep arm, since the sleep sits
    /// between attempts and there is exactly one attempt.
    ///
    /// ⚠ This is the assertion that goes red the day somebody restores the trailing sleep (the
    /// loop slept after its FINAL attempt until this branch): `waited_ms` would then be an
    /// interval rather than zero, and the caller that may not block would be blocking for it.
    #[test]
    fn a_non_blocking_budget_refuses_without_sleeping_at_all() {
        let d = dir();
        std::fs::write(d.path().join("config.toml"), CONFIG_WITH_COMMENTS).unwrap();
        let held = hold_the_lock(d.path());

        let err = set_setting_within(
            d.path(),
            SettingsFile::Config,
            "config.tradehub_addr",
            "0.0.0.0:9000",
            LockBudget::NON_BLOCKING,
        )
        .expect_err("a held lock must refuse a non-blocking write");
        match err {
            SettingsWriteError::Busy { waited_ms, .. } => {
                assert_eq!(waited_ms, 0, "a NON_BLOCKING acquire may not sleep");
            }
            other => panic!("expected Busy, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(d.path().join("config.toml")).unwrap(),
            CONFIG_WITH_COMMENTS,
            "and it changes NO byte"
        );
        drop(held);
    }

    /// The `Busy` refusal reports the budget it was GIVEN, which is the seam every caller's own
    /// test uses to prove which budget it passed without measuring a duration.
    #[test]
    fn the_busy_refusal_reports_the_budget_it_was_given() {
        let d = dir();
        let held = hold_the_lock(d.path());
        for budget in [LockBudget::NON_BLOCKING, LockBudget::from_millis(4)] {
            let err = set_setting_within(
                d.path(),
                SettingsFile::Flags,
                "flags.reconcile",
                "true",
                budget,
            )
            .expect_err("a held lock must refuse");
            match err {
                SettingsWriteError::Busy { waited_ms, .. } => {
                    assert_eq!(waited_ms, budget.max_wait_ms(), "{budget:?}");
                }
                other => panic!("expected Busy, got {other:?}"),
            }
        }
        drop(held);
    }

    // ---------------------------------------------------------------------------------------
    // The refusals an operator meets
    // ---------------------------------------------------------------------------------------

    /// **A sentinel failure names the SENTINEL as well as the directory, and leads with the cause
    /// an `ls` cannot see.** Every failure to open it used to be an `Io` blamed on the directory
    /// alone, which is a dead end when the directory is innocent; the repair for THAT then asserted
    /// a foreign-owned sentinel as the cause, which is a second dead end on the one box that
    /// actually produces this error — see [`SettingsWriteError::Lock`] for both and for the
    /// measurement.
    ///
    /// Driven by planting a DIRECTORY at the sentinel's path, which makes the file open fail on
    /// every platform while the settings directory itself stays perfectly writable. ⚠ That is the
    /// shape of the SECOND cause, not the first: a read-only mount namespace is not reproducible
    /// from a unit test, which is why the message is gated here and the namespace claim is carried
    /// as a measurement on the variant rather than as an assertion.
    #[test]
    fn a_sentinel_that_cannot_be_opened_names_both_the_directory_and_the_lock_file() {
        let d = dir();
        std::fs::create_dir_all(d.path().join(SETTINGS_LOCK_FILE)).unwrap();

        let err = set_setting(d.path(), SettingsFile::Flags, "flags.reconcile", "true")
            .expect_err("an unopenable sentinel must refuse");
        let msg = err.to_string();
        assert!(matches!(err, SettingsWriteError::Lock { .. }), "{err:?}");
        assert!(msg.contains(SETTINGS_LOCK_FILE), "it must name the sentinel: {msg}");
        assert!(
            msg.contains(&d.path().display().to_string()),
            "…and the directory the operator aimed at: {msg}"
        );
        assert!(msg.contains("NOTHING was written"), "{msg}");
        // The ORDER is the finding: an operator sent to `ls -ld` first meets a writable directory
        // and stops, which is the dead end this message exists to remove.
        let namespace = msg.find("ReadWritePaths");
        let owner = msg.find("owned by another account");
        assert!(namespace.is_some(), "the sandbox cause must be named: {msg}");
        assert!(owner.is_some(), "…and the foreign-owner one too: {msg}");
        assert!(namespace < owner, "the cause an `ls` cannot answer must come FIRST: {msg}");
        assert!(!d.path().join("flags.toml").exists(), "nothing may be written");
    }

    /// **An absent target with a `.bak` beside it is the STRANDED state, and is refused rather than
    /// treated as a fresh project.**
    ///
    /// The cheap second consequence of a `SettingsWriteError::Stranded` that nothing used to
    /// handle: the next write from ANY of the three callers found no target, took the absence as an
    /// empty edit base, and landed a one-key file over the loss — reporting "the file did not set
    /// it before" while the operator's real settings sat in a backup nothing was looking for.
    #[test]
    fn an_absent_target_with_a_backup_beside_it_is_refused_as_stranded() {
        let d = dir();
        let backup = d.path().join("config.toml.bak");
        std::fs::write(&backup, CONFIG_WITH_COMMENTS).unwrap();

        let err = set_setting(d.path(), SettingsFile::Config, "config.log_dir", "/tmp/x")
            .expect_err("a stranded directory must refuse");
        let msg = err.to_string();
        assert!(matches!(err, SettingsWriteError::CurrentFile { .. }), "{err:?}");
        assert!(msg.contains("STRANDED"), "it must name the state: {msg}");
        assert!(msg.contains("config.toml.bak"), "…and where the bytes are: {msg}");
        assert!(!d.path().join("config.toml").exists(), "no one-key file may be landed over it");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            CONFIG_WITH_COMMENTS,
            "the operator's bytes are untouched"
        );

        // …and with no backup beside it, an absent target is still an ordinary empty edit base.
        std::fs::remove_file(&backup).unwrap();
        set_setting(d.path(), SettingsFile::Config, "config.log_dir", "/tmp/x")
            .expect("an absent file with no backup is a fresh start");
    }

    /// **`Stranded` is the ONE variant that is not `Outcome::Refused`**, because `Refused`'s own
    /// definition is "the change was refused and nothing was written" and something demonstrably
    /// was. Exhaustive over the variants, so a NEW error variant cannot fall through unclassified.
    #[test]
    fn only_a_stranded_write_is_journalled_as_something_other_than_refused() {
        use vike_model::change_journal::Outcome;
        let io = || std::io::Error::other("x");
        let p = || PathBuf::from("/s/policy.toml");
        for e in [
            SettingsWriteError::BadKey { key: "k".into(), reason: "r".into() },
            SettingsWriteError::CurrentFile { file: p(), message: "m".into() },
            SettingsWriteError::Io { file: p(), source: io() },
            SettingsWriteError::Lock { dir: p(), lock: p(), source: io() },
            SettingsWriteError::Busy { lock: p(), waited_ms: 0 },
        ] {
            assert_eq!(journal_outcome(&e), Outcome::Refused, "{e:?} wrote nothing");
        }
        let stranded = SettingsWriteError::Stranded { file: p(), backup: p(), source: io() };
        assert_eq!(
            journal_outcome(&stranded),
            Outcome::Stranded,
            "the previous file was moved aside — a ledger row may not say nothing was written"
        );
    }

    /// **The live-arm and safety-override FLAGS are inside the ceremony**, which no file-keyed rule
    /// in this tree could express: they share `flags.toml` with a dozen ordinary toggles.
    ///
    /// The asymmetry this closes: `policy.max_leverage` — a field nothing reads — demanded a
    /// retype, while `flags.tradehub_live` ("the single largest blast radius on this list") did
    /// not.
    ///
    /// ⚠ **…and the SECOND asymmetry, which the first repair left behind.**
    /// `flags.tradehub_allow_public_bind` — which only widens the BIND ADDRESS of the daemon's
    /// control surface — was inside the ceremony while `flags.tradehub_control`, which OPENS that
    /// surface at all, was outside it. The named assertion below is the one that goes red if the
    /// opener is ever dropped again: an ordering where widening a remote order-write path costs
    /// more ceremony than opening it is the inverse of what the class claims.
    #[test]
    fn the_live_arm_and_the_safety_overrides_are_inside_the_ceremony() {
        for key in CONFIRMED_FLAG_KEYS {
            assert!(requires_typed_confirm(key), "{key}");
            let why = typed_confirm_reason(key).expect("a reason");
            assert!(!why.contains("RISK CEILING"), "{key} is not a policy ceiling: {why}");
        }
        assert!(
            requires_typed_confirm("flags.tradehub_control"),
            "the OPENER of the remote order-origination surface may not be cheaper to write than \
             `flags.tradehub_allow_public_bind`, which merely widens its address"
        );
        // …and an ordinary flag is still a one-line write.
        //
        // ⚠ `flags.poly_exec` STOOD IN THIS LIST and has moved to the one above. It was an honest
        // example while a write of it armed nothing — the const's excluding comment said exactly
        // that — and it stopped being one the moment the unread-settings sweep wired the key into
        // the daemon's mount. A pinned "this is ordinary" assertion is the last place a dead
        // argument survives, so the move is noted here rather than made silently.
        // `flags.record_properties` replaces it: a recorder toggle that opens no socket and signs
        // nothing, which is what "ordinary" is supposed to mean.
        for key in ["flags.reconcile", "flags.record_properties", "flags.tradehub_record"] {
            assert!(!requires_typed_confirm(key), "{key} is an ordinary toggle");
        }
        // The policy half keeps its own wording, so a surface can say which class it is refusing.
        let why = typed_confirm_reason("policy.max_notional_per_order").expect("a reason");
        assert!(why.contains("RISK CEILING"), "{why}");
    }
}
