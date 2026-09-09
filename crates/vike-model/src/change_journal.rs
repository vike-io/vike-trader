//! **The CHANGE JOURNAL** — an append-only, durable, queryable record of what changed in this
//! project's settings and arming, and who changed it.
//!
//! Ported from nothing: net-new Rust surface. It is the durable half of a record that already
//! exists in the right SHAPE and lands in the wrong PLACE — `crates/vike-tradehub/src/audit.rs`'s
//! `record_settings_write` builds `peer` / `file` / dotted `key` / `old` → `new` / operator
//! `reason`, sanitizes every remote-influenced cell, distinguishes "key absent" from "key present
//! but empty", and redacts a credential-shaped key through `vike_config::is_secret_key`. Nothing
//! about that record is wrong. **What is wrong is that its only home is a `tracing::info!` line.**
//!
//! # Why a rolling log file is not a durable record — measured, not argued
//!
//! * `vike_log::DEFAULT_MAX_LOG_FILES` is the retention, `vike_log::LogConfig`'s `Default` uses it,
//!   and no binary overrides it. Rotation is daily. A risk-ceiling change from a handful of days ago
//!   has already been deleted.
//! * `deploy/vike-tradehub.service` sets `Environment=VIKE_LOG_FILE_LEVEL=warn`, and the
//!   environment beats `LogConfig`'s `file_level`. The audit record is emitted at `info`. Measured
//!   on the live the CI box box: that daemon's 23 MB log file held **53,160 ERROR lines, 1,928 WARN
//!   lines and ZERO INFO lines.** The record would never have reached disk there at all.
//!   ⚠ **CORRECTED: that half is now CLOSED, and it does not reopen the verdict.**
//!   `vike_tradehub::audit::FILE_PIN` raises that module's own `tracing` target above the global
//!   file level, and `record_settings_write` — the very call this module mirrors — lives in that
//!   module, so it is pinned too: an accepted settings write DOES reach the log file now, at the
//!   `warn` the shipped units set (`crates/vike-tradehub/tests/daemon/audit_reaches_disk.rs` drives a real
//!   daemon at that level and proves it). What survives untouched is the RETENTION bullet ABOVE,
//!   and that is the one this module exists for: a pinned line still rotates daily and is still
//!   deleted after `vike_log::DEFAULT_MAX_LOG_FILES` days, so the log file is now a COMPLETE record
//!   of a short window rather than an empty record of one. A ledger nothing deletes is a different
//!   promise from a line nothing filters, and only this module makes the first.
//!
//! So the tracing line stays — it is the console/journald copy, and it is the right thing for an
//! operator tailing a unit — and this module is what makes the same record survive.
//!
//! # Where it lives, and why not a database
//!
//! `<project>/settings/state/changes/changes-YYYY-MM.jsonl`
//! ([`crate::state_path::project_state_dir_from`] plus [`CHANGES_SUBDIR`]), a sibling of
//! [`crate::state_path::INCIDENTS_SUBDIR`] and of `crate::state_path::LOGS_SUBDIR`. It is STATE by
//! this workspace's own test — machine-written, no human edits it — and `vike-log`'s pruning cannot
//! reach it: that prunes `logs/` by executable prefix, and this directory is neither.
//!
//! ⚠ **SQLite and redb were both considered and REJECTED by the owner. Do not re-litigate.** The
//! decisive property is in the requirement rather than in the taste: **multi-process append.** The
//! daemon and the GUI both write, concurrently, from separate processes, and an embedded database
//! holding an exclusive write lock turns that into one writer plus a queue of failures — for a
//! record whose whole job is to exist when something went wrong. JSONL holds a lock for the
//! microseconds of ONE append (see the next section) — a lock a second writer WAITS on rather than
//! fails on, which is the whole difference — it has no schema to migrate, and a line is readable by
//! `jq`, by `grep`, and by a future reader that has never heard of this module.
//!
//! ⚠ That per-append lock does NOT reopen the verdict, and the distinction is the one
//! `docs/decisions/0028-settings-stay-files-change-journal-is-jsonl.md` turns on: an embedded store takes its write lock for the life
//! of a CONNECTION, so the GUI being open is enough to stop the daemon recording. This one is taken
//! and released inside a single bounded append by every writer symmetrically, and the kernel
//! releases it if the holder dies.
//!
//! # The write, and the lock around it
//!
//! One record is ONE [`std::io::Write::write`] of one buffer ending in `\n`, to a descriptor opened
//! with [`std::fs::OpenOptions::append`], followed by [`std::fs::File::sync_data`] — and that whole
//! sequence runs while an EXCLUSIVE advisory lock on `<dir>/`[`CHANGES_LOCK_FILE`] is held.
//! `write_all` is deliberately NOT used: it LOOPS on a short write, and a second write is a second
//! chance to interleave. A short write is reported as [`ChangeJournalError::ShortWrite`] instead,
//! which for a bounded append to a regular file is a bug rather than a condition. The lock fixes
//! INTERLEAVING; it licenses no loop.
//!
//! ## Why the lock exists — the residual stopped being theoretical
//!
//! ⚠ **POSIX guarantees that an `O_APPEND` write updates the file offset atomically — it does NOT
//! guarantee that an arbitrary-size write is delivered whole**, and it guarantees nothing whatever
//! on a layer that is not implementing POSIX. This paragraph used to rest the record's integrity on
//! that guarantee, naming NFS as the one theoretical exception and both production boxes' local
//! filesystems (ext4, btrfs) as the reason it did not matter. **The guarantee is correct and the
//! conclusion was too narrow**: the exception is not theoretical, and it is not NFS.
//!
//! Measured 2026-08-24 on Docker Desktop 29.7.2 / Windows 11 / WSL2, over a HOST BIND MOUNT — the
//! shape `docs/ops/tradehub-container.md` documents for a container deployment. Four processes each
//! appending 25 lines to one file opened `>>` left **36 of 100** lines; this crate's own
//! `crates/vike-model/tests/change_journal_concurrent.rs`'s
//! `concurrent_processes_both_append_and_no_line_tears`, run with its scratch directory on that
//! mount, reported **`got 60 of 200`**. Not torn lines — LOST writes. The same binary on the
//! container's own overlayfs passed, which is the control that makes it the mount rather than the
//! test. And it fails with NOTHING TO SEE: no error, no short write, just a ledger that is quietly
//! incomplete.
//!
//! **The primitive that survives there is the one that fixes the primitive that does not.** `flock`
//! IS honoured across that mount (measured the same day: a second process was refused while the
//! first held, and acquired after release), and the identical four-process probe with each append
//! serialised behind `flock` left **100 of 100**. So every append takes an exclusive advisory lock
//! first — the idiom already in this tree three times over
//! (`crates/vike-core/src/journal_lock.rs`'s `JournalLock`, `crates/vike-ops/src/live_lock.rs`'s
//! `LiveLock`, `crates/vike-data/src/datafusion_hist/manifest.rs`'s `SeriesLock`), all resting on
//! the same property: the kernel releases an advisory lock when the holder dies, so there is no
//! stale lock, no PID file and nothing to sweep.
//!
//! ⚠ **The acquire BLOCKS, and a lock that cannot be taken at all REFUSES the record loudly**
//! ([`ChangeJournalError::Lock`]) rather than writing beside it. Both halves are one argument:
//! losing an accountability record to the mechanism that exists to protect it is the defect being
//! fixed, so a contended writer WAITS (the critical section is one bounded append with no caller
//! code inside it, and a dead holder's lock is already released), and a filesystem whose locking
//! errors outright is one this journal cannot be complete on — the caller gets an error it can log
//! instead of a silence it cannot. There is deliberately no fast path, no lock-free mode and no
//! knob: at ~40 records a day the cost is unmeasurable, and a branch nobody exercises is where the
//! next bug lives.
//!
//! [`MAX_RECORD_BYTES`] stays, and stays for a reason the lock does not cover: the lock orders THIS
//! workspace's writers, while a record inside one page is what keeps a line whole against everything
//! else — a `cat >>` at a shell, an operator's editor, a future reader's own appender.
//!
//! # Retention is not optional
//!
//! This workspace once wrote a **341 GB** log file because nothing pruned, and 215 GB of leaked
//! scratch because nothing swept. [`ChangeJournal::prune`] bounds the population to
//! [`DEFAULT_MAX_CHANGE_FILES`] monthly files. It is a COUNT rather than an age for the same reason
//! [`crate::scratch::sweep`] is: an age needs `now`, and `vike-model` is inside
//! `crates/vike-ops/tests/clock_pin.rs`'s `DETERMINISM_CRITICAL_CRATES`, where a wall-clock read is
//! a ratchet that may shrink and never grow. Monthly file names make the bound a file count, and
//! they sort lexicographically in calendar order, so the prune needs no clock and no `stat`.
//!
//! ⚠ **CORRECTED 2026-08-25: that bound is AVAILABLE, not ARMED.** The paragraph above described
//! [`ChangeJournal::prune`] as though calling it were settled, and it reads that way because the
//! method's own doc says so — it opens *"Call it ONCE at startup, beside [`crate::scratch::sweep`]"*.
//! Nobody does. Every call to `prune` in this workspace is in this file's own `#[cfg(test)]` module
//! (six of them), and no composition root, daemon or CLI verb reaches it — while the sibling it
//! names, [`crate::scratch::sweep`], IS wired, from `crates/vike-backfill/src/cli.rs`. The sentence
//! sitting next to the 341 GB was doing work the code does not do.
//!
//! **Be proportionate: what that costs is essentially nothing, and the defect is the CLAIM.** An
//! unpruned journal is not the shape that produced the 341 GB — that was an unbounded per-message
//! DAILY file at `trace` level. [`DEFAULT_MAX_CHANGE_FILES`] is 120 MONTHLY files, i.e. ten years,
//! and that constant's own doc records why the number is generous: a change record is small and
//! rare, and an active month of settings edits is measured in kilobytes. Ten unbounded years here
//! is a few megabytes. What is actually wrong is a reader being told a bound is in force when it is
//! not — the same class of falsehood the WIRED/NOT section below exists to prevent, which is why
//! `prune` now carries a row there.
//!
//! **Arming it is a separate decision, deliberately not taken in this correction.** The natural
//! owner is the boot path, and `crates/vike-boot/src/lib.rs`'s `journal_boot_settings` — the anchor
//! that already opens this journal at startup — is called by only TWO of the composition roots (the
//! table below says which, and why the other three argue against an anchor at all). So "prune at
//! startup" means either accepting that only those two ever prune, or choosing a different owner
//! and a different startup hook. Neither is obvious enough to settle inside a doc pass.
//!
//! # Purity, and who reads the clock
//!
//! Same contract as [`crate::state_path`] and [`crate::scratch`]: **the timestamp is a PARAMETER.**
//! Nothing here reads the wall clock. The caller — which is always a binary or a crate outside the
//! determinism scope — stamps `ts_ms` and passes it in. [`Proc::current`] is the one call that
//! touches process ambient state, and what it reads (`current_exe`, `process::id`) is the very
//! thing being recorded; [`Proc::new`] is there for a caller that would rather say so itself.
//!
//! # ⚠ WHAT IS WIRED, AND WHAT IS NOT — read this before assuming a change is in here
//!
//! ⚠ **This paragraph used to read *"Coverage is two channels of four, and one record type here
//! still has no caller"*, and it has been stale since #1502/#1504.** All FOUR record kinds are
//! wired now: `venue_mounted` arrived after this section was written and never got a row, so the
//! table below undercounted itself. What is left unwired is not a record kind at all — it is the
//! RETENTION bound — and one class here is unrecordable by construction rather than unfinished. The
//! rows say which is which, and the hand-edit row is the one that makes **an absence in this
//! journal not evidence that nothing changed**, permanently.
//!
//! | what | wired? | the call site |
//! |---|---|---|
//! | `set_setting` | **YES** | `crates/vike-tradehub/src/audit.rs`'s `record_settings_write`, reached from `crates/vike-tradehub/src/server.rs`'s `accept_command` (the `WireCommand::SetSetting` arm) |
//! | `credential_write` | **YES** | all three callers of `crates/vike-secrets/src/env_write.rs`'s `save_credentials` — see below |
//! | `boot_settings` | **YES** | `crates/vike-boot/src/lib.rs`'s `journal_boot_settings`, called by the roots that ENFORCE the ceilings — see below |
//! | `venue_mounted` | **YES** | `crates/vike-run/src/node.rs`'s `journal_venue_mounts`, called from `crates/vike-app/src/main.rs` and `crates/vike-tradehub/src/tradehub_cli.rs` — the two roots that mount venues. Added by #1502 (the kind) and #1504 (the writer) |
//! | a HAND-EDIT of a settings file | no, and it never will be | nothing observes one; see the note below |
//! | RETENTION — [`ChangeJournal::prune`], not a record kind | **no** | nobody. Every call site is this file's own `#[cfg(test)]` module. The bound is written and tested, and no root arms it — see "Retention is not optional" above for why the disk cost of that is negligible and why arming it is a separate decision |
//!
//! **`credential_write` — the three call sites, all journalled.** All reach one function,
//! `crates/vike-secrets/src/env_write.rs`'s `save_credentials` (re-exported through
//! `crates/vike-connections/src/env_write.rs`), which is the workspace's ONE sanctioned credential
//! write — an in-place upsert of named keys that leaves every other line byte-identical:
//!
//!   * `crates/vike-connections/src/view.rs`'s `render_edit_form` — the GUI Connections editor's
//!     Save arm. Actor: [`Actor::Gui`].
//!   * `crates/vike-app-core/src/tool_views/stored.rs`'s `save_polymarket_proxy`. Actor:
//!     [`Actor::Gui`].
//!   * `crates/bridges/ctrader/src/token_store.rs`'s `persist` — a grant the VENUE rotated, not
//!     something a human did. Actor: [`Actor::venue`] with `"ctrader"`. This one is the reason the
//!     `venue` origin exists at all.
//!
//! ⚠ **`save_credentials` itself does NOT journal, and that is structural rather than a style
//! choice.** `crates/vike-secrets/Cargo.toml` declares a literally EMPTY `[dependencies]` — the
//! property that lets `vike-bridge-core` (the transport stack) and `vike-cli` (transport-free) both
//! link it — so that crate cannot name this module at all. The CALLER writes the record, over a
//! journal its own composition root resolved. The first two sites reach one shared wrapper,
//! `crates/vike-connections/src/env_write.rs`'s `save_credentials_journalled`, which performs the
//! store write and the append together so they cannot come apart at a call site; the third calls
//! [`ChangeJournal::append`] itself, because that wrapper sits at layer 75 (it links `egui`) and a
//! venue bridge at layer 40 cannot reach it.
//!
//! ⚠ **What the caller can and cannot say about a write.** `save_credentials` returns
//! `io::Result<()>` — it reports SUCCESS, and nothing about WHICH of the keys it was handed were
//! replaced in place versus appended as new lines. So a `credential_write` record names the keys the
//! caller ASKED to be written, which is the only thing anybody in that position knows; it does not
//! claim to distinguish an update from an insert, and no field here invites it to. Recovering that
//! distinction would mean re-reading and re-parsing the operator's only copy of their live keys.
//!
//! **`boot_settings` — wired, and NOT from every root.** `crates/vike-boot/src/lib.rs`'s
//! `journal_boot_settings` is the one implementation and it takes the root's own ALREADY-RESOLVED
//! state directory, because that is not always `vike_boot::Booted`'s `state_dir` (a
//! `$VIKE_STATE_ROOT` relocates `vike-tradehub`'s whole state tree, and the anchor belongs beside
//! that daemon's log). Two of the five composition roots call it — the two that ENFORCE the
//! ceilings and mount venues, `crates/vike-app/src/main.rs` and
//! `crates/vike-tradehub/src/tradehub_cli.rs` — and the other three carry a row in
//! `crates/vike-boot/tests/boot_journal_wiring.rs` arguing why an anchor from them would be noise
//! (`vike-cli` runs for every subcommand) or a FALSEHOOD (`vike-recorder` and `vike-datahub` sign
//! no orders, and the latter loads no settings at all, so a record claiming the effective ceilings
//! would be reporting compiled-in defaults as though a file had been obeyed).
//!
//! ⚠ **The ceiling set is NOT all of `vike_config::Policy`'s fields.** `boot_ceilings` names the
//! ones that are EFFECTIVE, which excludes the leverage ceiling: `crates/vike-mount/src/policy.rs`'s
//! `MountPolicy::from` deliberately does not carry it (its default would clamp every deployment
//! with no `policy.toml` to 1x), so a value written there is SET and enforces nothing, and
//! `crates/vike-config/tests/policy_is_consumed.rs` records exactly that. A record whose target
//! claims the effective ceilings must not list it. That function's own exhaustive destructure is
//! the authority for the list, which is why no copy of it is written into a constant here.
//!
//! ⚠ **A hand-edit of a settings file is the class nothing can see, and `boot_settings` is the
//! answer to it rather than a detection.** No `set_setting` record exists for somebody opening
//! `policy.toml` in an editor, and no realistic amount of filesystem watching would make one
//! trustworthy. What the boot anchor buys is a BRACKET: the effective value before the restart and
//! after it, from which a hand-edit is inferable even though it was never observed.
//!
//! ⚠ **The venue's EFFECTIVE tier at mount IS recorded now, and this paragraph used to say it was
//! not.** It read: *"a fifth thing nothing records — `crates/vike-mount/src/lib.rs`'s
//! `make_engine_with_legs` resolves a `vike_bridge_core::credentials::Environment` per venue and
//! falls back to paper when credentials are absent, and 'which venues were actually armed on this
//! run' is exactly the question an incident review asks first. It is left out of this pass because
//! it belongs beside `boot_settings` as one arming record rather than bolted onto a credential
//! channel."* That is precisely what was then built: #1502 added the `venue_mounted` kind here and
//! #1504 wired `crates/vike-run/src/node.rs`'s `journal_venue_mounts`, which writes one record per
//! interesting venue carrying what was ASKED for and what was REACHED — an arming record beside the
//! boot anchor, exactly as the paragraph proposed. The prose stayed behind because it sits three
//! sections away from the table it contradicts, which is the same distance that let the table's own
//! "two channels of four" intro go stale. Kept rather than deleted: it is the design note that
//! produced the fourth kind.
//!
//! # A future sibling, and why it needs no migration
//!
//! Connection drops and reconnects are deliberately OUT OF SCOPE here — a different rate class, and
//! mixing a per-second event stream into a per-change ledger would bury the ledger. A future author
//! adds `<project>/settings/state/connectivity/` beside this directory: a separate subdirectory, a
//! separate [`ChangeJournal`]-shaped writer, no shared file and therefore nothing to migrate.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

/// The change-journal sub-directory inside `crate::state_path::STATE_SUBDIR`:
/// `<project>/settings/state/changes`.
///
/// A DIRECTORY rather than a single file, because retention is per-file and the files are monthly —
/// the same reason `crate::state_path::LOGS_SUBDIR` is a directory rather than a file.
pub const CHANGES_SUBDIR: &str = "changes";

/// Every journal file name starts with this: `changes-2026-08.jsonl`.
pub const CHANGE_FILE_PREFIX: &str = "changes-";

/// …and ends with this. JSON Lines, one record per line, so the file is `jq`-able and appendable.
pub const CHANGE_FILE_SUFFIX: &str = ".jsonl";

/// The sentinel every appender locks before it writes: `<dir>/changes.lock`.
///
/// ⚠ **Its BYTES are meaningless; its LIFETIME is the lock** — the `journal_lock`/`live_lock`
/// idiom. It is never read, never truncated and never unlinked (unlinking a lock file is a race,
/// not a tidy-up), and a leftover one after a crash is NOT a stale lock: the kernel released the
/// lock when the holder's descriptor closed.
///
/// ⚠ The NAME is chosen so [`is_month_file_name`] cannot match it — it carries no
/// [`CHANGE_FILE_PREFIX`] — which is what keeps [`ChangeJournal::prune`] from ever deleting the
/// file every writer is coordinating on. `prune_leaves_anything_that_is_not_a_monthly_file_alone`
/// includes it in its stranger set so that stays true.
///
/// One sentinel per DIRECTORY rather than one per month file: the directory holds one live month at
/// a time, and a directory-wide lock is also what orders the one append per month that crosses from
/// the old file to the new one.
pub const CHANGES_LOCK_FILE: &str = "changes.lock";

/// How many monthly files [`ChangeJournal::prune`] keeps when the caller has no opinion: **120
/// months, ten years.**
///
/// The number lives HERE rather than in prose, exactly as `vike_log::DEFAULT_MAX_LOG_FILES` and
/// [`crate::scratch::DEFAULT_MAX_SCRATCH_ENTRIES`] do — this repository has watched every
/// hand-copied constant rot. It is generous on purpose: a change record is small and rare (an
/// active month of settings edits is measured in kilobytes), so the bound exists to make the growth
/// FINITE rather than to reclaim space. What it refuses is the shape that produced the 341 GB log:
/// a file set with no upper bound at all.
pub const DEFAULT_MAX_CHANGE_FILES: usize = 120;

/// The hard ceiling on ONE serialized record, newline included: 4096 bytes, one page.
///
/// The whole point of the module doc's atomicity residual. Every free-text cell is capped at
/// construction ([`MAX_FIELD_BYTES`] and friends) so that a record can never approach this even
/// when every character in it needs a JSON escape — `record_shapes_at_their_caps_fit_a_page` is the
/// test that proves that arithmetic rather than asserting it, by building a maximal record of each
/// kind out of the most expensive character there is. A record that somehow exceeds it is refused
/// as [`ChangeJournalError::TooLarge`] rather than written, because a torn line is worse than a
/// missing one: a reader cannot tell a truncated record from a forged one.
pub const MAX_RECORD_BYTES: usize = 4096;

/// The cap on one free-text cell (`file`, `key`, `old`, `new`, `reason`, `store`), in BYTES.
///
/// Bytes rather than chars, and the difference is the point: a char cap bounds nothing about the
/// line, since one char is up to four bytes and then up to six more after escaping.
/// [`cap_bytes`] truncates on a char boundary so the result is always valid UTF-8.
pub const MAX_FIELD_BYTES: usize = 256;

/// The cap on one IDENTIFIER cell — a venue, a tier, an actor's peer/scope/key-id, a `proc` field.
pub const MAX_IDENT_BYTES: usize = 64;

/// How many credential key NAMES one `credential_write` record may carry.
///
/// A venue tier has at most three keys (`_API_KEY` / `_API_SECRET` / `_API_PASSPHRASE` —
/// `crate::credential_keys::CREDENTIAL_SUFFIXES`), so 16 covers a multi-venue save with room to
/// spare. Names beyond the cap are dropped from `keys` but STILL COUNTED in `count`, so the record
/// says "I recorded 16 of 40" instead of quietly claiming 16 were all there was.
pub const MAX_CREDENTIAL_KEYS: usize = 16;

/// The `tier` cell for a credential key that is **not tier-scoped at all** — the honest answer
/// where `"SIM"` / `"DEMO"` / `"LIVE"` all lie.
///
/// [`CredentialTarget::tier`] normally carries `vike_bridge_core::credentials::Environment`'s
/// `as_str`, because almost every credential key is one venue at one tier. `POLY_SOCKS_PROXY` — the
/// Polymarket egress proxy the Data Manager's box writes — is not: one value governs every
/// Polymarket tier, so picking any of the three would assert something false about the two it did
/// not pick, and an empty string would read as "the writer did not know" rather than "there is
/// nothing to know".
///
/// It is a CONSTANT rather than a literal at the one call site because the cell is a `jq` query's
/// vocabulary: a reader filtering `select(.target.tier=="LIVE")` needs to be able to find out what
/// else that field can hold, and the type is the only place that can tell them. Same spirit as
/// [`CredentialTarget::venue`]'s documented `"multi"`.
pub const TIER_UNTIERED: &str = "untiered";

/// How many key/value pairs one `boot_settings` record may carry.
///
/// ⚠ **Excess entries are SILENTLY DROPPED (`take`), and this cap has already bitten once.** It
/// said "the ceiling set is four (`vike_config::Policy` has exactly four fields); 8 leaves room for
/// one more" — a written count that had rotted twice over. `vike_boot::boot_ceilings` grew to
/// exactly eight rows, so the next ceiling added (`policy.max_sizing_equity`, 2026-09-07) pushed
/// the NINTH — `policy.venues`, the one ceiling on that struct whose default BITES — off the end of
/// the record. Nothing said so; `crates/vike-boot/tests/boot_journal.rs`'s
/// `the_anchor_never_claims_a_ceiling_that_is_not_effective` caught it because it asserts each key
/// by name.
///
/// No count of the ceiling set is written beside it this time —
/// `vike_boot::boot_ceilings`' own exhaustive `Policy` destructure is the roster, and it forces an
/// author to decide about a new field there.
///
/// ⚠ **This value is MEASURED, not chosen, and it is at the ceiling.** A change to it re-opens the
/// size arithmetic: `record_shapes_at_their_caps_fit_a_page` builds a maximal record of this kind
/// from the most expensive character there is and requires it inside [`MAX_RECORD_BYTES`]. At the
/// present [`MAX_BOOT_CELL_BYTES`] an entry costs about 264 of those bytes at its worst, so 12 was
/// MEASURED at 4534 bytes (refused) and this value is the largest that fits — the record it
/// produces leaves under a hundred bytes of the page spare. **An eleventh ceiling therefore cannot
/// simply be added here**: it needs a smaller [`MAX_BOOT_CELL_BYTES`], or a second record kind, or
/// the page arithmetic re-opened. Run that test rather than reasoning about this paragraph.
pub const MAX_BOOT_ENTRIES: usize = 10;

/// The per-cell cap inside a `boot_settings` record. Tighter than [`MAX_FIELD_BYTES`] because there
/// are up to [`MAX_BOOT_ENTRIES`] of them and they must all fit one page together.
pub const MAX_BOOT_CELL_BYTES: usize = 64;

/// Orders two records written in the same millisecond by one process.
///
/// ⚠ **A tiebreaker, not a ledger index.** It starts at zero in every process, it is shared by
/// every [`ChangeJournal`] in the process, and a failed write consumes one — so a GAP in the
/// sequence means "a record was refused or the process restarted", never "a line was deleted".
/// Detecting deletion is a different problem (a hash chain) and is not what this field is.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// What happened to the change being recorded.
///
/// `Refused` is a first-class outcome rather than an omission: "somebody tried to raise the ceiling
/// and was refused" is exactly as much of an audit fact as a successful write, and a journal that
/// records only successes cannot answer whether anyone tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The change is in effect now.
    Applied,
    /// The change is on disk, and the RUNNING process keeps its boot-time value until restarted —
    /// `vike_tradehub::server::Accepted`'s `SettingsWritten { restart_required: true }`.
    AppliedPendingRestart,
    /// The change was refused and nothing was written.
    Refused,
}

/// WHO made the change — the truth, which is not a person.
///
/// ⚠ **There are no human accounts in this system.** The daemon authenticates a KEY, not a user;
/// the GUI is whoever is at the machine; the CLI is whoever ran it. Recording a `user` field would
/// be recording a fiction, and an audit record that invents an actor is worse than one that admits
/// it cannot name one. So this enum records the CHANNEL and whatever that channel actually knows.
///
/// Internally tagged on `origin`, so a line reads `{"origin":"wire","peer":"…","scope":"control"}`
/// and a reader can branch on one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum Actor {
    /// A remote peer on the control socket.
    Wire {
        /// The TCP peer address. `None` because `TcpStream::peer_addr` can fail — the whole
        /// tradehub server module carries it as an `Option` for that reason.
        #[serde(skip_serializing_if = "Option::is_none")]
        peer: Option<String>,
        /// The authenticated scope (`"control"`). `vike_tradehub_client::auth::Scope`'s name.
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        /// A stable, NON-SECRET identifier for the key that authenticated —
        /// ⚠ **never the key itself, and never a prefix of it.**
        ///
        /// Supplied by `vike_datahub_client::node_auth`'s `NodeKeys::key_id`: `nk-` plus 16 hex
        /// characters of an HMAC-SHA256 tag taken under a domain separator that is deliberately
        /// NOT one of the two protocol signing domains, so a value recorded here can never be
        /// replayed as an auth tag. That module is the authority for the construction and for its
        /// one declared residual.
        ///
        /// ⚠ Still `Option`, and the absence is load-bearing in two ways: a control surface that
        /// authenticates no key at all (the Telegram channel) records NO field rather than a
        /// borrowed or placeholder id, and a key that is not configured is never fingerprinted —
        /// an id for a credential nobody set would be a lie in an append-only ledger.
        #[serde(skip_serializing_if = "Option::is_none")]
        key_id: Option<String>,
    },
    /// The desktop GUI (`vike-app`).
    Gui,
    /// A command-line invocation.
    Cli {
        /// The binary that ran (`"vike-cli"`).
        bin: String,
    },
    /// The VENUE changed it — an OAuth grant the venue rotated, not something a human did.
    Venue {
        /// The venue id, as `crate::venues::VENUES` spells it.
        venue: String,
    },
    /// Process startup, recording what the effective values ARE rather than a change to them.
    Boot,
}

impl Actor {
    /// A control-socket peer. Every cell is capped and control-stripped, because `peer` and `scope`
    /// are remote-influenced text landing in a structured line.
    pub fn wire(peer: Option<&str>, scope: Option<&str>, key_id: Option<&str>) -> Self {
        Actor::Wire {
            peer: peer.map(clean_ident),
            scope: scope.map(clean_ident),
            key_id: key_id.map(clean_ident),
        }
    }

    /// A command-line invocation by `bin`.
    pub fn cli(bin: &str) -> Self {
        Actor::Cli { bin: clean_ident(bin) }
    }

    /// The venue itself rotated something.
    pub fn venue(venue: &str) -> Self {
        Actor::Venue { venue: clean_ident(venue) }
    }
}

/// The PROCESS that wrote the record — `{"bin":"vike-tradehub","pid":4711,"ver":"0.1.0"}`.
///
/// It is on every record rather than on the file, because one file is written by several processes:
/// the daemon and the GUI append to the same month, and "which binary wrote this" is the first
/// question an incident review asks of a line it did not expect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proc {
    /// The executable's file stem (`"vike-tradehub"`, `.exe` already stripped on Windows).
    pub bin: String,
    /// The OS process id — what ties a record to a log line and to a core file.
    pub pid: u32,
    /// The build version, as the caller states it (`env!("CARGO_PKG_VERSION")`, or
    /// `vike_buildinfo::summary`'s richer line).
    pub ver: String,
}

impl Proc {
    /// State the process identity outright — the form a caller that already knows it should use.
    pub fn new(bin: &str, pid: u32, ver: &str) -> Self {
        Self { bin: clean_ident(bin), pid, ver: clean_ident(ver) }
    }

    /// Read the identity off the running process.
    ///
    /// ⚠ This is the one call in the module that touches ambient process state, and it is exempt
    /// from the module doc's purity rule for a reason that does not generalize: **the ambient state
    /// IS the record.** A caller-supplied "which binary am I" would be a caller-supplied claim, and
    /// a record whose most forgeable field is the one identifying the writer is not worth writing.
    /// `current_exe` is not an environment read (`crates/vike-ops/tests/settings_registry.rs`'s
    /// scanner keys on `env::var`) and not a clock read
    /// (`crates/vike-ops/tests/clock_pin.rs`'s `AMBIENT_CLOCK_READERS`), so neither ratchet is
    /// touched.
    ///
    /// An unreadable `current_exe` yields `"unknown"` rather than failing: a journal that refuses
    /// to record because it could not name itself is a journal that goes silent exactly when the
    /// process is in trouble.
    pub fn current(ver: &str) -> Self {
        let bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "unknown".to_string());
        Self::new(&bin, std::process::id(), ver)
    }
}

/// WHAT changed, one variant per `kind`.
///
/// Serialized `untagged`, so the object appears bare under `"target"` and the record's own `kind`
/// field is the discriminator — which is what lets a reader match on `kind` and skip the line
/// without parsing the rest of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Target {
    /// `kind = "set_setting"` — one key in one settings file.
    Setting(SettingTarget),
    /// `kind = "credential_write"` — key NAMES and a count, and structurally nothing else.
    Credential(CredentialTarget),
    /// `kind = "boot_settings"` — the effective ceilings at process start.
    BootSettings(BootSettingsTarget),
    /// `kind = "venue_mounted"` — the tier a venue was asked for, and the tier it reached.
    VenueMounted(VenueMountTarget),
}

/// `kind` for [`Target::Setting`].
pub const KIND_SET_SETTING: &str = "set_setting";
/// `kind` for [`Target::Credential`].
pub const KIND_CREDENTIAL_WRITE: &str = "credential_write";
/// `kind` for [`Target::BootSettings`].
pub const KIND_BOOT_SETTINGS: &str = "boot_settings";

/// `kind` for [`Target::VenueMounted`].
pub const KIND_VENUE_MOUNTED: &str = "venue_mounted";

/// Every `kind` this journal can write.
///
/// It exists for `record_shapes_at_their_caps_fit_a_page`, which asserts it tests ALL of them: that
/// gate's whole claim is that no record can exceed one page, and a hand-written case list silently
/// stops covering a kind the moment somebody adds one. Before this roster it was exactly that.
///
/// ⚠ **Kept in step by [`Target::kind`], not by this constant.** A new [`Target`] variant fails to
/// COMPILE at that total match, which puts the author in this file with this list on screen; the
/// size gate then fails until the new kind has a maximal record. The residual is honest and worth
/// stating: a variant added to `kind()` and forgotten HERE leaves the gate passing over a narrower
/// roster. That is one hop, not none.
pub const KINDS: &[&str] =
    &[KIND_SET_SETTING, KIND_CREDENTIAL_WRITE, KIND_BOOT_SETTINGS, KIND_VENUE_MOUNTED];

impl Target {
    /// The `kind` string for this target. Total by construction — a new variant that forgets a kind
    /// is a compile error, not a record that serializes with the wrong discriminator.
    pub fn kind(&self) -> &'static str {
        match self {
            Target::Setting(_) => KIND_SET_SETTING,
            Target::Credential(_) => KIND_CREDENTIAL_WRITE,
            Target::BootSettings(_) => KIND_BOOT_SETTINGS,
            Target::VenueMounted(_) => KIND_VENUE_MOUNTED,
        }
    }
}

/// A settings write — taken verbatim from `vike_config::write::SettingsWrite`, whose own doc calls
/// itself "the audit record's raw material".
///
/// ⚠ **`old` ABSENT is not `old: null`.** An absent field means the key was not in the file at all;
/// a present empty string means the key was there and held nothing. The distinction is
/// `SettingsWrite`'s (`old_value: Option<String>`), it is the one `vike-tradehub`'s
/// `audit::sanitize_value` preserves, and collapsing it would make "the ceiling was unset" and "the
/// ceiling was blank" read identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingTarget {
    /// The file that was edited (`"policy.toml"`).
    pub file: String,
    /// The full dotted key (`"policy.max_notional_per_order"`).
    pub key: String,
    /// The file's previous value, rendered as TOML. ABSENT when the key was not set in the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// The value written, rendered as TOML.
    pub new: String,
}

/// A credential write — **key NAMES and a count, and there is no way to put a value in one.**
///
/// # Why the values are structurally impossible rather than merely omitted
///
/// This is the same discipline that makes `vike_config::Policy` unable to implement `EnvOverride`:
/// the property is enforced by there being no code path, not by every author remembering. Every
/// field below is PRIVATE and the only constructor is [`Change::credential_write`], **which accepts
/// no old/new/value parameter at all.** A future author who wants to log the value has to change
/// this type's shape and that constructor's signature — a diff a reviewer sees — rather than pass
/// one more argument at a call site nobody is reading.
/// `crates/vike-model/tests/change_journal_credential_values.rs` gates both halves.
///
/// # Why the NAMES are recordable, and why they are the useful part
///
/// A key name is not a secret. `vike-cli secrets list` prints names by an explicit decision in the
/// root `CLAUDE.md` ("shows key NAMES only, never values"), and the names are exactly what makes
/// *"when did I change the okx passphrase"* answerable — which is the question this channel exists
/// for. A record saying only "3 credentials changed" answers nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialTarget {
    /// The store file that was written (`"secrets.env"`).
    store: String,
    /// The venue the keys belong to, or `"multi"` when a save spanned several.
    venue: String,
    /// The credential tier (`"SIM"` / `"DEMO"` / `"LIVE"`) —
    /// `vike_bridge_core::credentials::Environment`'s `as_str`.
    tier: String,
    /// The key NAMES written, capped at [`MAX_CREDENTIAL_KEYS`].
    keys: Vec<String>,
    /// How many keys were written — the TRUE total, which may exceed `keys.len()` when the cap bit.
    count: usize,
}

impl CredentialTarget {
    /// The recorded key names (possibly fewer than [`CredentialTarget::count`] — see that field).
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// How many keys the write actually touched.
    pub fn count(&self) -> usize {
        self.count
    }

    /// The store file name.
    pub fn store(&self) -> &str {
        &self.store
    }

    /// The venue the keys belong to.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The credential tier.
    pub fn tier(&self) -> &str {
        &self.tier
    }
}

/// The effective value of a small fixed set of ceiling keys at process start.
///
/// One record per process start, and it exists because the other two kinds record DELTAS: a journal
/// of deltas cannot answer "what was the ceiling on the 14th" without replaying every delta from
/// the beginning of time and hoping none is missing. A periodic absolute anchor turns that into a
/// lookup, and process start is the cadence that costs nothing and is guaranteed to bracket every
/// hand-edit of a settings file (a class no delta channel can ever see).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BootSettingsTarget {
    /// Dotted key → effective value, in the order the caller supplied. `None` = not set (the
    /// compiled-in default applies), spelled as an absent map entry's twin: `null`, because unlike
    /// [`SettingTarget::old`] the KEY is always present here and only its value is missing.
    settings: Vec<(String, Option<String>)>,
}

impl BootSettingsTarget {
    /// The recorded (key, value) pairs.
    pub fn settings(&self) -> &[(String, Option<String>)] {
        &self.settings
    }
}

/// What a venue was ASKED to be at mount, and what it actually became.
///
/// The other three kinds record what an operator SAID. This one records what the program DID with
/// it, and the two differ routinely and legitimately: a ceiling of `live` still mounts paper when
/// no credentials are present, when the venue's build feature is absent, or when the venue declined
/// the key. Without this record the journal can prove the operator armed binance on the 14th and
/// cannot prove binance ever traded — which is the half an incident review actually needs, and the
/// exact question a per-venue switch invites ("I set live, why is it on paper?").
///
/// ⚠ **A record is written when the two AGREE, too.** An absence is not evidence: "no divergence
/// record for binance" and "binance never mounted at all" are the same silence, and somebody will
/// read the first out of it. Same argument as [`BootSettingsTarget`] — an absolute anchor beats an
/// inference over deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VenueMountTarget {
    /// The venue id, from `vike_model::VENUES`.
    venue: String,
    /// **WHICH ACCOUNT of that venue** — the label from
    /// [`crate::account_keys::AccountLabel`], or `None` for the account an unlabelled credential key
    /// addresses.
    ///
    /// ⚠ `None` is SKIPPED on the wire, and that is the whole reason it is an `Option` rather than a
    /// `String` carrying [`crate::account_keys::RESERVED_DEFAULT_LABEL`]: a box with one account per
    /// venue — which is every box that has not written an `[accounts]` table — writes the record it
    /// has always written, byte for byte, so nothing reading the ledger has to learn a new field to
    /// keep reading the old ones.
    ///
    /// It is the LABEL and not the route key deliberately: the route key is `venue#LABEL`, so a
    /// record carrying it would state the venue twice and a reader filtering on `venue` would have
    /// to parse it back apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    /// The tier asked for — the `policy.venues.<venue>` ceiling.
    requested: String,
    /// The tier the venue actually mounted at.
    effective: String,
    /// Why the two differ, when they do — `vike_config::ArmingBlock::as_str`'s rendering.
    ///
    /// Stored as a STRING rather than that enum, and not by preference: `vike-config` is layer 20
    /// and this crate is layer 10, so naming the type here would invert the direction
    /// `crates/vike-ops/tests/layer_gate.rs` enforces. [`CredentialTarget::tier`] holds a
    /// `vike_bridge_core::credentials::Environment` the same way and for the same reason — the
    /// vocabulary lives with the code that DECIDES, and the journal records its rendering.
    block: Option<String>,
}

impl VenueMountTarget {
    /// The venue id.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// Which account of it — `None` for the default account. See the field's own doc for why the
    /// default account is an absence rather than a spelling.
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    /// The tier the operator asked for.
    pub fn requested(&self) -> &str {
        &self.requested
    }

    /// The tier actually reached.
    pub fn effective(&self) -> &str {
        &self.effective
    }

    /// Why the mount fell short, when it did.
    pub fn block(&self) -> Option<&str> {
        self.block.as_deref()
    }

    /// Whether the mount reached less than it was asked for.
    pub fn diverged(&self) -> bool {
        self.requested != self.effective
    }
}

/// ONE change, ready to be journalled: the [`Target`] plus who and how it went.
///
/// The timestamp and the sequence number are NOT here — [`ChangeJournal::append`] stamps them, so a
/// caller cannot accidentally record a stale time and the sequence is always the write order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    outcome: Outcome,
    actor: Actor,
    target: Target,
    reason: Option<String>,
}

impl Change {
    /// A settings write, from `vike_config::write::SettingsWrite`'s cells.
    pub fn set_setting(
        outcome: Outcome,
        actor: Actor,
        file: &str,
        key: &str,
        old: Option<&str>,
        new: &str,
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::Setting(SettingTarget {
                file: clean_field(file),
                key: clean_field(key),
                old: old.map(clean_field),
                new: clean_field(new),
            }),
            reason: None,
        }
    }

    /// A credential write — **note what this signature does NOT take.**
    ///
    /// There is no `old`, no `new`, no `value` and no `values` parameter, and adding one is the
    /// change [`CredentialTarget`]'s doc and
    /// `crates/vike-model/tests/change_journal_credential_values.rs` exist to make loud. `keys` are
    /// NAMES; each is reduced to `[A-Za-z0-9_]` (a credential key name is that alphabet by
    /// construction — `crate::credential_keys::credential_key` builds them) so that even a caller
    /// that passed the wrong thing cannot get punctuation, whitespace or a line terminator into the
    /// record.
    pub fn credential_write(
        outcome: Outcome,
        actor: Actor,
        store: &str,
        venue: &str,
        tier: &str,
        keys: &[&str],
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::Credential(CredentialTarget {
                store: clean_field(store),
                venue: clean_ident(venue),
                tier: clean_ident(tier),
                keys: keys.iter().copied().take(MAX_CREDENTIAL_KEYS).map(clean_key_name).collect(),
                // The TRUE total, taken before the cap — so a capped record says how much it is not
                // showing instead of silently claiming the cap was the whole write.
                count: keys.len(),
            }),
            reason: None,
        }
    }

    /// The effective ceilings at process start.
    pub fn boot_settings(
        outcome: Outcome,
        actor: Actor,
        settings: &[(&str, Option<&str>)],
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::BootSettings(BootSettingsTarget {
                settings: settings
                    .iter()
                    .take(MAX_BOOT_ENTRIES)
                    .map(|(k, v)| {
                        (cap_bytes(&strip_control(k), MAX_BOOT_CELL_BYTES), v.map(clean_boot_cell))
                    })
                    .collect(),
            }),
            reason: None,
        }
    }

    /// A venue mount — the tier asked for, the tier reached, and the block between them.
    ///
    /// `block` is the caller's rendering of `vike_config::ArmingBlock` (see
    /// [`VenueMountTarget::block`] for why this crate takes a string rather than that enum). It is
    /// DROPPED when the tiers agree: a block recorded against a mount that reached what it was
    /// asked for would read as a refusal that never happened, which is worse than recording
    /// nothing.
    /// `account` is the ACCOUNT LABEL, `None` for the default account — see
    /// [`VenueMountTarget::account`] for why the default is an absence, and why that keeps a
    /// single-account box's records byte-identical.
    #[allow(clippy::too_many_arguments)]
    pub fn venue_mounted(
        outcome: Outcome,
        actor: Actor,
        venue: &str,
        account: Option<&str>,
        requested: &str,
        effective: &str,
        block: Option<&str>,
    ) -> Self {
        let requested = cap_bytes(&strip_control(requested), MAX_IDENT_BYTES);
        let effective = cap_bytes(&strip_control(effective), MAX_IDENT_BYTES);
        let diverged = requested != effective;
        Self {
            outcome,
            actor,
            target: Target::VenueMounted(VenueMountTarget {
                venue: cap_bytes(&strip_control(venue), MAX_IDENT_BYTES),
                account: account.map(|a| cap_bytes(&strip_control(a), MAX_IDENT_BYTES)),
                requested,
                effective,
                block: block
                    .filter(|_| diverged)
                    .map(|b| cap_bytes(&strip_control(b), MAX_IDENT_BYTES)),
            }),
            reason: None,
        }
    }
    /// Attach the operator's rationale. `None`, or text that sanitizes to nothing, records no
    /// `reason` field at all — `vike-tradehub`'s `audit::sanitize_reason` idiom, so a change with
    /// no rationale does not carry a blank one that reads like a supplied-but-empty explanation.
    #[must_use]
    pub fn with_reason(mut self, reason: Option<&str>) -> Self {
        self.reason = reason.map(clean_field).filter(|s| !s.trim().is_empty());
        self
    }

    /// What kind of change this is.
    pub fn kind(&self) -> &'static str {
        self.target.kind()
    }

    /// The target, for a caller that wants to inspect what it built.
    pub fn target(&self) -> &Target {
        &self.target
    }
}

/// ONE line of the journal, exactly as it is written.
///
/// ⚠ **Field order is the wire format.** `serde_json` emits struct fields in declaration order, and
/// `kind` sits immediately after the timestamp deliberately: a reader scanning a year of records
/// for credential writes can match a short prefix of each line and discard it without parsing the
/// rest. Reordering these fields is a format change, not a refactor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeRecord {
    /// Epoch milliseconds, UTC — supplied by the caller (see the module doc's purity section).
    pub ts_ms: i64,
    /// Per-process tiebreaker for two records in one millisecond. See [`SEQ`] for what a gap means.
    pub seq: u64,
    /// The discriminator for `target` — see [`Target::kind`].
    pub kind: &'static str,
    /// Applied, applied-pending-restart, or refused.
    pub outcome: Outcome,
    /// Which channel, and what that channel knows about who.
    pub actor: Actor,
    /// What changed.
    pub target: Target,
    /// The operator's rationale. ABSENT when none was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Which process wrote this line.
    #[serde(rename = "proc")]
    pub process: Proc,
}

/// Why an append did not happen. Returned rather than logged, because `vike-model` carries no
/// logging dependency — the caller owns the `tracing` line, exactly as [`crate::scratch::Swept`]
/// leaves the reporting to its caller.
#[derive(Debug)]
pub enum ChangeJournalError {
    /// The directory could not be created, or the file could not be opened, written or synced.
    Io(std::io::Error),
    /// The record serialized to more than [`MAX_RECORD_BYTES`]. Refused rather than truncated: a
    /// torn line cannot be told from a forged one.
    TooLarge {
        /// The serialized size, newline included.
        bytes: usize,
    },
    /// The single `write` did not deliver the whole buffer. For a bounded append to a regular file
    /// this is a bug; it is reported instead of retried, because a retry is a second chance to
    /// interleave with a concurrent writer (see the module doc's atomicity residual).
    ShortWrite {
        /// Bytes the kernel accepted.
        wrote: usize,
        /// Bytes the record needed.
        expected: usize,
    },
    /// The record could not be serialized. Unreachable for the types in this module (no maps with
    /// non-string keys, no non-finite floats), and reported rather than unwrapped anyway.
    Serialize(String),
    /// The append lock could not be TAKEN — the sentinel could not be created or opened, or the
    /// platform refused the lock outright.
    ///
    /// ⚠ **Not contention.** A contended acquire WAITS (see the module doc); this is the case where
    /// there is no working lock to wait on, and the record is refused rather than written beside
    /// whatever else is appending. Surfaced verbatim rather than degraded into "no lock", exactly
    /// as `crates/vike-core/src/journal_lock.rs`'s `JournalLock` and
    /// `crates/vike-ops/src/live_lock.rs`'s `LiveLock` surface theirs — a journal that cannot
    /// serialise its writers on this filesystem is one whose completeness nobody can claim, and the
    /// caller can log an error where it can never see a silence.
    Lock(std::io::Error),
}

impl std::fmt::Display for ChangeJournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeJournalError::Io(e) => write!(f, "change journal I/O: {e}"),
            ChangeJournalError::TooLarge { bytes } => {
                write!(f, "change record is {bytes} bytes, over the {MAX_RECORD_BYTES}-byte cap")
            }
            ChangeJournalError::ShortWrite { wrote, expected } => {
                write!(f, "change record short write: {wrote} of {expected} bytes")
            }
            ChangeJournalError::Serialize(e) => write!(f, "change record serialize: {e}"),
            ChangeJournalError::Lock(e) => write!(
                f,
                "change journal append lock ({CHANGES_LOCK_FILE}) could not be taken: {e}. \
                 Every append is serialised behind this lock because a host-passthrough filesystem \
                 does not preserve O_APPEND atomicity and silently loses concurrent records; a \
                 filesystem that cannot lock cannot hold a complete journal, so the record was \
                 REFUSED rather than written unserialised"
            ),
        }
    }
}

impl std::error::Error for ChangeJournalError {}

/// What one [`ChangeJournal::prune`] did. Data, not a log line — see [`ChangeJournalError`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pruned {
    /// Monthly journal files present before the prune.
    pub found: usize,
    /// How many were removed.
    pub removed: usize,
    /// How many removals FAILED. Non-zero is not fatal — a file another process holds open on
    /// Windows is the ordinary cause — but it is the number that says a prune is not keeping up.
    pub failed: usize,
}

/// The append-only change journal for one project.
///
/// Cheap to construct and cheap to hold: a path and a [`Proc`]. It opens no file until something is
/// appended and holds no descriptor between appends, which is what makes several processes able to
/// write the same month with nothing to coordinate.
#[derive(Debug, Clone)]
pub struct ChangeJournal {
    dir: PathBuf,
    process: Proc,
}

impl ChangeJournal {
    /// A journal writing into `dir` directly.
    pub fn new(dir: PathBuf, process: Proc) -> Self {
        Self { dir, process }
    }

    /// A journal at `<state_dir>/changes` — the form every caller should use, over the state
    /// directory `crate::state_path::project_state_dir_from` already resolved.
    ///
    /// Taking the ALREADY-RESOLVED state directory rather than walking is the rule
    /// `crate::state_path::user_data_dir_beside` exists to enforce: the bare walk is
    /// `$VIKE_SETTINGS_DIR`-blind, so a daemon whose unit relocates the project would journal into
    /// a different project's folder than the one it is reading settings from.
    pub fn in_state_dir(state_dir: &Path, process: Proc) -> Self {
        Self::new(state_dir.join(CHANGES_SUBDIR), process)
    }

    /// The directory this journal writes into.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file `ts_ms` belongs in: `<dir>/changes-YYYY-MM.jsonl`.
    pub fn file_for(&self, ts_ms: i64) -> PathBuf {
        self.dir.join(month_file_name(ts_ms))
    }

    /// Stamp `change` with `ts_ms` and the next [`SEQ`] value, producing the record that would be
    /// written. Consumes a sequence number, exactly as an append does.
    pub fn record(&self, ts_ms: i64, change: &Change) -> ChangeRecord {
        ChangeRecord {
            ts_ms,
            seq: SEQ.fetch_add(1, Ordering::Relaxed),
            kind: change.target.kind(),
            outcome: change.outcome,
            actor: change.actor.clone(),
            target: change.target.clone(),
            reason: change.reason.clone(),
            process: self.process.clone(),
        }
    }

    /// Render the line that [`ChangeJournal::append`] would write, WITHOUT touching the disk —
    /// including the trailing `\n`.
    ///
    /// Public because it is how a caller (and every test here) inspects the exact wire bytes; the
    /// alternative is a test that re-implements the serialization and then agrees with itself.
    pub fn render(&self, ts_ms: i64, change: &Change) -> Result<String, ChangeJournalError> {
        line_of(&self.record(ts_ms, change))
    }

    /// Append ONE change, durably, and return the file it landed in.
    ///
    /// The whole write is described in the module doc: one buffer, one `write` on an `O_APPEND`
    /// descriptor, then `sync_data`.
    pub fn append(&self, ts_ms: i64, change: &Change) -> Result<PathBuf, ChangeJournalError> {
        self.append_record(&self.record(ts_ms, change))
    }

    /// [`ChangeJournal::append`] over an ALREADY-STAMPED record — the form for a caller that minted
    /// its own [`ChangeRecord`], and the seam the size refusal is driven through
    /// (`an_oversized_record_is_refused_and_writes_nothing`: the public constructors cap every
    /// cell, so an over-budget record is unreachable through [`ChangeJournal::append`] and the
    /// belt behind those braces would otherwise be untestable and therefore untested).
    ///
    /// The directory is created lazily, because `settings/state/changes` is not a marker and a
    /// fresh install has none.
    ///
    /// ⚠ `sync_data` rather than `sync_all`: the data and the file length must be on the platter
    /// before this returns (that is the entire durability claim), while the directory entry only
    /// needs to be durable when the FILE is new — which happens once a month, and which the
    /// `create_dir_all` plus the next month's first write settle in practice. `sync_all` on every
    /// append would add a metadata flush per record for no property this journal claims.
    ///
    /// ⚠ **The size check happens BEFORE the directory is created, the lock is taken and the file
    /// is opened**, so a refused record leaves no trace at all — not an empty file, not a lock
    /// sentinel, not a directory that makes a fresh install look like it has journalled something.
    ///
    /// ⚠ **The lock spans the open, the write and the sync, and nothing else.** No caller code runs
    /// inside it and it is never held across a return, which is what makes a blocking acquire safe
    /// to wait on — see the module doc.
    pub fn append_record(&self, record: &ChangeRecord) -> Result<PathBuf, ChangeJournalError> {
        let line = line_of(record)?;
        std::fs::create_dir_all(&self.dir).map_err(ChangeJournalError::Io)?;
        let path = self.file_for(record.ts_ms);
        // THE SERIALISATION. Held until this function returns; released by the kernel if this
        // process dies holding it. See the module doc for the measurement that put it here.
        let _lock = AppendLock::acquire(&self.dir)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(ChangeJournalError::Io)?;
        // ONE write, deliberately not `write_all`: that loops, and a second write is a second
        // chance to interleave with a concurrent appender. The lock excludes this workspace's own
        // writers; the single write is what keeps a line whole against everything else.
        let wrote = file.write(line.as_bytes()).map_err(ChangeJournalError::Io)?;
        if wrote != line.len() {
            return Err(ChangeJournalError::ShortWrite { wrote, expected: line.len() });
        }
        file.sync_data().map_err(ChangeJournalError::Io)?;
        Ok(path)
    }

    /// Prune to the newest `max_files` monthly files, oldest first.
    ///
    /// Call it ONCE at startup, beside `crate::scratch::sweep`. `None` keeps everything — the
    /// `vike_log::LogConfig::file_max_files` spelling for "retention off". An absent or unreadable
    /// directory is not an error: a project that has changed nothing has no journal, and a startup
    /// must not fail over housekeeping.
    ///
    /// ⚠ **NOBODY CALLS IT: read the sentence above as an invitation, not as a description of what
    /// happens today.** Every call site of this method in the workspace is in this file's own
    /// `#[cfg(test)]` module, so the bound it implements is AVAILABLE rather than in force, and the
    /// sibling it points at — `crate::scratch::sweep` — is the one of the pair that actually has a
    /// caller (`crates/vike-backfill/src/cli.rs`). The module doc's "Retention is not optional"
    /// section carries the rest: why the disk consequence is negligible ([`DEFAULT_MAX_CHANGE_FILES`]
    /// is 120 MONTHLY files, ten years, over a journal whose busy month is measured in kilobytes),
    /// and why picking a startup owner is a decision rather than a one-line wiring job.
    ///
    /// ⚠ **Ordered by NAME, not by mtime**, which is what makes it clock-free AND correct: a
    /// `changes-YYYY-MM.jsonl` name sorts lexicographically in calendar order, while an mtime says
    /// only when a file was last touched — and an old month's file is touched again the moment a
    /// record arrives with a backdated `ts_ms`. Anything in the directory that is not a
    /// well-formed monthly file is left ALONE rather than counted or removed, so a reader's own
    /// export sitting there cannot be deleted by housekeeping and cannot make the bound bite early.
    pub fn prune(&self, max_files: Option<usize>) -> Pruned {
        let Some(max_files) = max_files else { return Pruned::default() };
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Pruned::default() };

        let mut found: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.file_name().to_str().is_some_and(is_month_file_name) && e.path().is_file()
            })
            .map(|e| e.path())
            .collect();
        found.sort();

        let mut out = Pruned { found: found.len(), ..Pruned::default() };
        let excess = found.len().saturating_sub(max_files);
        for path in found.into_iter().take(excess) {
            match std::fs::remove_file(&path) {
                Ok(()) => out.removed += 1,
                Err(_) => out.failed += 1,
            }
        }
        out
    }
}

/// The held append lock: one exclusive advisory lock on `<dir>/`[`CHANGES_LOCK_FILE`], released
/// when this value drops.
///
/// Private on purpose. It is not a capability a caller can hold across several appends — the whole
/// argument for a BLOCKING acquire (module doc) is that the critical section is one bounded
/// open-write-sync with no caller code inside it, and a public guard would be an invitation to
/// widen exactly that.
///
/// ⚠ On both platforms this ships on the lock is keyed on the OPEN FILE DESCRIPTION / file handle
/// rather than on the process — Unix `flock`, Windows `LockFileEx` — so a second [`ChangeJournal`]
/// in the SAME process queues behind the first exactly as another process does. That is what makes
/// the interlock testable in one process at all, and it is a property of those two primitives, not
/// a universal one: a port to a target whose `try_lock`/`lock` is backed by POSIX `fcntl` record
/// locks would key on the PROCESS instead, and would have to re-verify this paragraph rather than
/// inherit it. (Same caveat, same words, as `crates/vike-core/src/journal_lock.rs`'s `JournalLock`.)
#[derive(Debug)]
struct AppendLock(std::fs::File);

impl AppendLock {
    /// Take the exclusive lock on `dir`, creating the sentinel if absent. `dir` must already exist
    /// (the caller's `create_dir_all` runs first).
    ///
    /// **Blocks** while another writer holds it — see the module doc for why waiting is the right
    /// answer and dropping the record is not. Every other failure (the sentinel cannot be created
    /// or opened, the platform refuses to lock) becomes [`ChangeJournalError::Lock`].
    fn acquire(dir: &Path) -> Result<Self, ChangeJournalError> {
        let path = dir.join(CHANGES_LOCK_FILE);
        // `read(true).write(true)`, NOT `append(true)`: Windows refuses to lock an append-opened
        // handle. `truncate(false)`: the bytes are meaningless, and a file another process is
        // holding must never be rewritten. (The `journal_lock`/`live_lock` idiom, verbatim.)
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(ChangeJournalError::Lock)?;
        file.lock().map_err(ChangeJournalError::Lock)?;
        Ok(AppendLock(file))
    }
}

impl Drop for AppendLock {
    fn drop(&mut self) {
        // Closing the descriptor releases the lock on its own; unlocking first makes the release
        // explicit and ordered. The FILE stays on disk — see [`CHANGES_LOCK_FILE`].
        let _ = self.0.unlock();
    }
}

/// Serialize ONE record to its wire line — the trailing `\n` included — and REFUSE it if it does
/// not fit [`MAX_RECORD_BYTES`].
///
/// The refusal is the module doc's atomicity mitigation made mechanical, and it is a refusal rather
/// than a truncation because a torn line cannot be told from a forged one: a reader that finds
/// `{"ts_ms":1787356800000,"seq":3,"kind":"set_se` has no way to know whether a writer was
/// interrupted or a record was tampered with, and both readings are worse than a counted absence.
fn line_of(record: &ChangeRecord) -> Result<String, ChangeJournalError> {
    let mut line =
        serde_json::to_string(record).map_err(|e| ChangeJournalError::Serialize(e.to_string()))?;
    line.push('\n');
    if line.len() > MAX_RECORD_BYTES {
        return Err(ChangeJournalError::TooLarge { bytes: line.len() });
    }
    Ok(line)
}

/// `changes-YYYY-MM.jsonl` for an epoch-ms instant, UTC.
///
/// Built on [`crate::time::civil_from_days`] — this workspace's one home for calendar math, and
/// chrono-free, which matters because `vike-model` sits below everything and must not grow a
/// calendar dependency. `div_euclid` floors toward -inf, so a pre-1970 instant lands in its own
/// month rather than the next one, exactly as [`crate::time::epoch_ms_to_utc_date`] does.
pub fn month_file_name(ts_ms: i64) -> String {
    let (y, m, _) = crate::time::civil_from_days(ts_ms.div_euclid(86_400_000));
    format!("{CHANGE_FILE_PREFIX}{y:04}-{m:02}{CHANGE_FILE_SUFFIX}")
}

/// Is `name` a well-formed monthly journal file — `changes-YYYY-MM.jsonl` with four digits, a
/// hyphen and two digits?
///
/// Strict on purpose. [`ChangeJournal::prune`] DELETES what this accepts, so anything it is unsure
/// about (a hand-made `changes-old.jsonl`, an editor's `changes-2026-08.jsonl.bak`, a reader's
/// export) must fall outside and be left alone.
fn is_month_file_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(CHANGE_FILE_PREFIX) else { return false };
    let Some(stamp) = rest.strip_suffix(CHANGE_FILE_SUFFIX) else { return false };
    let b = stamp.as_bytes();
    b.len() == 7
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..].iter().all(u8::is_ascii_digit)
}

/// Strip every Unicode control character — `vike-tradehub`'s `audit::sanitize_reason` discipline,
/// and the load-bearing half of it.
///
/// ⚠ Here it is belt AND braces rather than the only guard: `serde_json` escapes a newline to `\n`,
/// so a control character could not break the JSONL framing even if one survived. It is stripped
/// anyway because the framing is not the only consumer — `grep`, a terminal and a naive splitter
/// all see the raw bytes, and a record whose safety depends on the reader using a JSON parser is a
/// record with a footgun in it.
fn strip_control(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).collect()
}

/// Truncate to at most `max` BYTES, on a char boundary so the result is always valid UTF-8.
///
/// Bytes rather than chars because the thing being bounded is the LINE (see [`MAX_RECORD_BYTES`]),
/// and a char cap bounds nothing about a line: one char is up to four bytes before escaping.
fn cap_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// A free-text cell: control-stripped and capped at [`MAX_FIELD_BYTES`].
fn clean_field(raw: &str) -> String {
    cap_bytes(&strip_control(raw), MAX_FIELD_BYTES)
}

/// An identifier cell (venue, tier, peer, scope, key id, `proc` field): capped at
/// [`MAX_IDENT_BYTES`].
fn clean_ident(raw: &str) -> String {
    cap_bytes(&strip_control(raw), MAX_IDENT_BYTES)
}

/// A `boot_settings` cell: capped at [`MAX_BOOT_CELL_BYTES`], because there are up to
/// [`MAX_BOOT_ENTRIES`] of them and they must fit one page together.
fn clean_boot_cell(raw: &str) -> String {
    cap_bytes(&strip_control(raw), MAX_BOOT_CELL_BYTES)
}

/// A credential key NAME, reduced to `[A-Za-z0-9_]` and capped at [`MAX_IDENT_BYTES`].
///
/// Stricter than [`clean_ident`] on purpose. A credential key name is that alphabet by construction
/// (`crate::credential_keys::credential_key` joins a venue, a tier and a suffix), so anything else
/// arriving here means the caller passed something that is not a key name — and the one thing that
/// must never happen in this record is a VALUE arriving where a name was expected. Reducing to the
/// alphabet cannot make that safe, but it does mean punctuation, whitespace, `=` and every line
/// terminator are gone before the cell is written.
fn clean_key_name(raw: &str) -> String {
    let filtered: String = raw.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    cap_bytes(&filtered, MAX_IDENT_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchDir;

    /// A throwaway journal directory for one test.
    ///
    /// Built with [`ScratchDir`], the same dogfooding [`crate::scratch`]'s own tests use: unique per
    /// process, self-deleting on the panic path, and no `tempfile` dev-dependency in the crate every
    /// binary in this workspace links. The system temp directory is legitimate here —
    /// `crates/vike-ops/tests/system_temp_gate.rs` scopes itself to production code.
    fn root() -> ScratchDir {
        ScratchDir::create_in(&std::env::temp_dir(), "vike-changejournal-selftest").expect("root")
    }

    fn journal(dir: &Path) -> ChangeJournal {
        ChangeJournal::new(dir.to_path_buf(), Proc::new("vike-test", 4711, "0.1.0"))
    }

    /// 2026-08-21T00:00:00Z, the anchor every timestamped test below uses.
    const T: i64 = 1_787_356_800_000;

    fn read_lines(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .expect("journal file")
            .lines()
            .map(|l| serde_json::from_str(l).expect("each line parses as one JSON object"))
            .collect()
    }

    /// The wire format, end to end: field ORDER, the absent-vs-empty distinction, and the file the
    /// record lands in.
    #[test]
    fn a_settings_write_lands_as_one_json_line() {
        let r = root();
        let j = journal(r.path());
        let change = Change::set_setting(
            Outcome::AppliedPendingRestart,
            Actor::wire(Some("127.0.0.1:51234"), Some("control"), None),
            "policy.toml",
            "policy.max_notional_per_order",
            Some("100"),
            "250",
        )
        .with_reason(Some("raising ahead of the CPI print"));

        let path = j.append(T, &change).expect("append");
        assert_eq!(path.file_name().unwrap(), "changes-2026-08.jsonl");

        let raw = std::fs::read_to_string(&path).expect("read back");
        assert!(raw.ends_with('\n'), "every record is newline-terminated: {raw:?}");
        assert_eq!(raw.lines().count(), 1);

        // `kind` sits immediately after the timestamp and the sequence, so a reader can discard a
        // line on a short PREFIX rather than parsing it whole. This is the wire format, not a
        // formatting preference — asserted on the literal bytes, since a `serde_json::Value`
        // round-trip is order-blind and would pass whatever order the struct declares.
        assert!(raw.starts_with(r#"{"ts_ms":"#), "ts_ms leads the line: {raw}");
        let at = |k: &str| raw.find(k).unwrap_or_else(|| panic!("{k} in {raw}"));
        assert!(at(r#""seq""#) < at(r#""kind""#), "seq then kind: {raw}");
        assert!(at(r#""kind""#) < at(r#""outcome""#), "kind precedes outcome: {raw}");
        assert!(at(r#""kind""#) < at(r#""target""#), "…and the target it discriminates: {raw}");
        assert!(at(r#""kind""#) < 64, "kind is inside a short prefix: {raw}");

        let v = &read_lines(&path)[0];
        assert_eq!(v["kind"], KIND_SET_SETTING);
        assert_eq!(v["outcome"], "applied_pending_restart");
        assert_eq!(v["actor"]["origin"], "wire");
        assert_eq!(v["actor"]["peer"], "127.0.0.1:51234");
        assert_eq!(v["actor"]["scope"], "control");
        assert!(v["actor"].get("key_id").is_none(), "an absent key id records no field");
        assert_eq!(v["target"]["file"], "policy.toml");
        assert_eq!(v["target"]["key"], "policy.max_notional_per_order");
        assert_eq!(v["target"]["old"], "100");
        assert_eq!(v["target"]["new"], "250");
        assert_eq!(v["reason"], "raising ahead of the CPI print");
        assert_eq!(v["proc"]["bin"], "vike-test");
        assert_eq!(v["proc"]["pid"], 4711);
        assert_eq!(v["ts_ms"], T);
    }

    /// ⚠ ABSENT is not empty. "The ceiling was unset" and "the ceiling was blank" must not read
    /// identically — the distinction `vike_config::write::SettingsWrite` draws with
    /// `old_value: Option<String>` and the one an incident review turns on.
    #[test]
    fn an_absent_old_value_is_an_absent_field_not_an_empty_one() {
        let r = root();
        let j = journal(r.path());

        let absent =
            Change::set_setting(Outcome::Applied, Actor::Gui, "policy.toml", "policy.x", None, "1");
        let empty = Change::set_setting(
            Outcome::Applied,
            Actor::Gui,
            "policy.toml",
            "policy.x",
            Some(""),
            "1",
        );
        let path = j.append(T, &absent).expect("append");
        j.append(T, &empty).expect("append");

        let lines = read_lines(&path);
        assert!(lines[0]["target"].get("old").is_none(), "not in the file at all: no field");
        assert_eq!(lines[1]["target"]["old"], "", "in the file, holding nothing: an empty string");
        // …and the two really are different lines, so this cannot pass by both being absent.
        assert_ne!(lines[0]["target"], lines[1]["target"]);
    }

    /// A rationale that sanitizes to nothing records NO field, rather than a blank one that reads
    /// like a supplied-but-empty explanation.
    #[test]
    fn an_empty_reason_records_no_field() {
        let r = root();
        let j = journal(r.path());
        for raw in [None, Some(""), Some("   "), Some("\n\t")] {
            let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, "1")
                .with_reason(raw);
            let line = j.render(T, &c).expect("render");
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert!(v.get("reason").is_none(), "{raw:?} must record no reason: {line}");
        }
    }

    /// Every actor variant round-trips as `origin` plus what that channel actually knows. There is
    /// no `user` field anywhere, because there are no human accounts in this system.
    #[test]
    fn every_actor_records_its_channel_and_never_a_user() {
        let r = root();
        let j = journal(r.path());
        let cases = [
            (Actor::wire(Some("<host>:9000"), Some("control"), Some("nk-3f21")), "wire"),
            (Actor::Gui, "gui"),
            (Actor::cli("vike-cli"), "cli"),
            (Actor::venue("ctrader"), "venue"),
            (Actor::Boot, "boot"),
        ];
        for (actor, origin) in cases {
            let c = Change::set_setting(Outcome::Applied, actor, "f.toml", "f.k", None, "1");
            let line = j.render(T, &c).expect("render");
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(v["actor"]["origin"], origin, "{line}");
            assert!(v["actor"].get("user").is_none(), "no invented human actor: {line}");
        }
        // The wire actor's key id is an ID, and the record must carry it rather than the key.
        let c = Change::set_setting(
            Outcome::Applied,
            Actor::wire(None, Some("control"), Some("nk-3f21")),
            "f.toml",
            "f.k",
            None,
            "1",
        );
        let line = j.render(T, &c).expect("render");
        assert!(line.contains("nk-3f21"), "{line}");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(v["actor"].get("peer").is_none(), "an unknown peer records no field: {line}");
    }

    /// A credential record carries NAMES and a COUNT — and the count is the TRUE total, so a capped
    /// record says how much it is not showing.
    #[test]
    fn a_credential_record_carries_names_and_a_true_count() {
        let r = root();
        let j = journal(r.path());
        let keys = ["OKX_LIVE_API_KEY", "OKX_LIVE_API_SECRET", "OKX_LIVE_API_PASSPHRASE"];
        let c = Change::credential_write(
            Outcome::Applied,
            Actor::Gui,
            "secrets.env",
            "okx",
            "LIVE",
            &keys,
        );
        let path = j.append(T, &c).expect("append");
        let v = &read_lines(&path)[0];
        assert_eq!(v["kind"], KIND_CREDENTIAL_WRITE);
        assert_eq!(v["target"]["venue"], "okx");
        assert_eq!(v["target"]["tier"], "LIVE");
        assert_eq!(v["target"]["count"], 3);
        let recorded: Vec<&str> =
            v["target"]["keys"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
        assert_eq!(recorded, keys, "the names are what makes the record answerable");

        // Over the cap: the names are trimmed, the COUNT is not — the record admits the trim.
        let many: Vec<String> = (0..40).map(|i| format!("V{i}_LIVE_API_KEY")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let c = Change::credential_write(
            Outcome::Applied,
            Actor::Gui,
            "secrets.env",
            "multi",
            "LIVE",
            &refs,
        );
        let Target::Credential(t) = c.target() else { panic!("credential target") };
        assert_eq!(t.keys().len(), MAX_CREDENTIAL_KEYS, "names are capped");
        assert_eq!(t.count(), 40, "…and the count still tells the truth");
    }

    /// A key NAME cell is reduced to `[A-Za-z0-9_]`, so a caller that passed the wrong thing cannot
    /// get punctuation, whitespace, `=` or a line terminator into the record.
    #[test]
    fn a_credential_key_name_is_reduced_to_its_alphabet() {
        let r = root();
        let j = journal(r.path());
        let c = Change::credential_write(
            Outcome::Applied,
            Actor::Gui,
            "secrets.env",
            "okx",
            "LIVE",
            &["OKX_LIVE_API_KEY=sk-live-abc\nOKX_LIVE_API_SECRET"],
        );
        let line = j.render(T, &c).expect("render");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        let got = v["target"]["keys"][0].as_str().unwrap();
        assert!(!got.contains('='), "no assignment survives: {got}");
        assert!(!got.contains('\n') && !got.contains('-'), "no separator survives: {got}");
        assert_eq!(got, "OKX_LIVE_API_KEYskliveabcOKX_LIVE_API_SECRET", "{got}");
    }

    /// The boot anchor: one record per process start carrying the effective ceilings, with `None`
    /// distinguishable from a value.
    #[test]
    fn a_boot_record_carries_the_effective_ceilings() {
        let r = root();
        let j = journal(r.path());
        let c = Change::boot_settings(
            Outcome::Applied,
            Actor::Boot,
            &[
                // ⚠ NOT `policy.max_leverage`, and the reason is semantic rather than cosmetic.
                // That field is `Consumed::No` in `crates/vike-config/tests/policy_is_consumed.rs`
                // BY DESIGN — `MountPolicy::from` deliberately does not carry it, because its `1.0`
                // default would clamp every deployment with no `policy.toml` to 1x. So a value an
                // operator wrote there is set but NOT EFFECTIVE, and a record whose target is named
                // `settings` under a boot anchor claiming the effective ceilings would be asserting
                // the opposite. That gate also reads a textual mention as a read, so naming it here
                // turned CI red on the merged tree — but the fixture would have been wrong even if
                // the gate had stayed silent.
                ("config.store_root", Some("/srv/vike/market_data/hist")),
                ("policy.max_notional_per_order", Some("250")),
                ("policy.market_slippage", None),
                ("policy.halt_admit", Some("admit")),
            ],
        );
        let path = j.append(T, &c).expect("append");
        let v = &read_lines(&path)[0];
        assert_eq!(v["kind"], KIND_BOOT_SETTINGS);
        assert_eq!(v["actor"]["origin"], "boot");
        let s = v["target"]["settings"].as_array().unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(s[1][0], "policy.max_notional_per_order");
        assert_eq!(s[1][1], "250");
        assert!(s[2][1].is_null(), "an unset ceiling is null, not a missing pair: {v}");
    }

    /// A venue mount records BOTH tiers and the block between them — the record that answers the
    /// question a per-venue switch invites: *"I set live, why did it trade paper?"*
    #[test]
    fn a_venue_mount_record_carries_both_tiers_and_the_block() {
        let r = root();
        let j = journal(r.path());
        let c = Change::venue_mounted(
            Outcome::Applied,
            Actor::Boot,
            "binance",
            None,
            "live",
            "paper",
            Some("no-credentials"),
        );
        let path = j.append(T, &c).expect("append");
        let v = &read_lines(&path)[0];
        assert_eq!(v["kind"], KIND_VENUE_MOUNTED);
        assert_eq!(v["target"]["venue"], "binance");
        assert_eq!(v["target"]["requested"], "live");
        assert_eq!(v["target"]["effective"], "paper");
        assert_eq!(v["target"]["block"], "no-credentials");
    }

    /// ...and a mount that REACHED what it was asked for records no block — while still recording.
    ///
    /// Both halves are the point. Writing the record on agreement is what makes the channel usable
    /// at all: an absence proves nothing, because *"no record for bybit"* reads identically whether
    /// bybit mounted cleanly or never mounted. Dropping the block is the other half — nothing was
    /// refused here, and a block on a mount that got what it asked for would document a refusal
    /// that never happened.
    #[test]
    fn a_mount_that_reached_its_tier_records_no_block() {
        let r = root();
        let j = journal(r.path());
        let c = Change::venue_mounted(
            Outcome::Applied,
            Actor::Boot,
            "bybit",
            None,
            "demo",
            "demo",
            // A caller that hands one anyway is NOT obeyed — the constructor decides from the
            // tiers, so a caller cannot stamp a refusal onto a mount that succeeded.
            Some("no-credentials"),
        );
        let path = j.append(T, &c).expect("append");
        let v = &read_lines(&path)[0];
        assert_eq!(v["kind"], KIND_VENUE_MOUNTED);
        assert_eq!(v["target"]["effective"], "demo");
        assert!(
            v["target"]["block"].is_null(),
            "a mount that got what it asked for carries no block: {v}"
        );
        match c.target() {
            Target::VenueMounted(t) => {
                assert!(!t.diverged(), "demo asked and demo reached is not a divergence");
                assert_eq!(t.block(), None);
            }
            other => panic!("expected a venue-mount target, got {other:?}"),
        }
    }

    /// THE SIZE GATE. A maximal record of every kind, built from the most expensive character there
    /// is, must still fit one page — because that arithmetic is the module doc's whole atomicity
    /// mitigation, and prose arithmetic rots.
    ///
    /// `"` is chosen deliberately: it is a single ASCII byte that `serde_json` escapes to two, which
    /// is the worst expansion available once control characters are stripped (a non-ASCII char
    /// passes through as its own UTF-8 bytes and does not expand at all).
    #[test]
    fn record_shapes_at_their_caps_fit_a_page() {
        let r = root();
        let worst = "\"".repeat(MAX_RECORD_BYTES); // longer than any cap, so every cap actually bites
        let j = ChangeJournal::new(r.path().to_path_buf(), Proc::new(&worst, u32::MAX, &worst));
        let actor = Actor::wire(Some(&worst), Some(&worst), Some(&worst));

        let setting = Change::set_setting(
            Outcome::Applied,
            actor.clone(),
            &worst,
            &worst,
            Some(&worst),
            &worst,
        )
        .with_reason(Some(&worst));

        let key_names: Vec<String> =
            (0..MAX_CREDENTIAL_KEYS * 4).map(|i| format!("{}_{i}", "K".repeat(80))).collect();
        let refs: Vec<&str> = key_names.iter().map(String::as_str).collect();
        let credential = Change::credential_write(
            Outcome::Applied,
            actor.clone(),
            &worst,
            &worst,
            &worst,
            &refs,
        )
        .with_reason(Some(&worst));

        let pairs: Vec<(String, Option<String>)> =
            (0..MAX_BOOT_ENTRIES * 3).map(|_| (worst.clone(), Some(worst.clone()))).collect();
        let borrowed: Vec<(&str, Option<&str>)> =
            pairs.iter().map(|(k, v)| (k.as_str(), v.as_deref())).collect();
        let boot = Change::boot_settings(Outcome::Applied, actor.clone(), &borrowed)
            .with_reason(Some(&worst));

        // ⚠ `requested` and `effective` must DIFFER here, or `venue_mounted` drops `block` by
        // design and this stops being the maximal record. They also cannot differ by a SUFFIX: both
        // are capped to `MAX_IDENT_BYTES`, so a longer twin truncates to the same prefix and
        // compares equal — which would make this case quietly smaller than it claims to be. The
        // difference goes in the FIRST byte, costing exactly one byte of escape expansion.
        let effective = format!("x{worst}");
        let venue_mounted = Change::venue_mounted(
            Outcome::Applied,
            actor,
            &worst,
            // ⚠ `Some`, never `None`: the default account SKIPS this field, so a `None` here would
            // make the maximal record one field smaller than the largest one this kind can write —
            // which is the shape of under-measurement this whole gate exists to refuse.
            Some(&worst),
            &worst,
            &effective,
            Some(&worst),
        )
        .with_reason(Some(&worst));

        let cases = [
            (KIND_SET_SETTING, setting),
            (KIND_CREDENTIAL_WRITE, credential),
            (KIND_BOOT_SETTINGS, boot),
            (KIND_VENUE_MOUNTED, venue_mounted),
        ];

        // THE COVERAGE CLAIM, and the reason this gate is worth more than it was. The case list
        // used to be three hand-written names; a fourth kind could join the journal without joining
        // the one-page guarantee, and nothing would have said so. Now the guarantee is asserted
        // over [`KINDS`], so a kind with no maximal record here is a RED test rather than a claim
        // quietly made over a shorter roster.
        let covered: Vec<&str> = cases.iter().map(|(_, c)| c.kind()).collect();
        for kind in KINDS {
            assert!(
                covered.contains(kind),
                "kind `{kind}` has no maximal record in this gate, so the one-page guarantee is \
                 being claimed over a roster the gate does not actually test"
            );
        }

        for (name, change) in cases {
            let line = j.render(T, &change).unwrap_or_else(|e| {
                panic!("a maximal {name} record must still render, got {e}");
            });
            assert!(
                line.len() <= MAX_RECORD_BYTES,
                "a maximal {name} record is {} bytes, over the {MAX_RECORD_BYTES}-byte page cap. \
                 Either a cap was raised without re-checking this arithmetic, or a field was added \
                 without one",
                line.len()
            );
            // …and it really is near the cap rather than trivially small, so this cannot pass
            // because the caps silently stopped applying.
            assert!(
                line.len() > 400,
                "a maximal {name} record collapsed to {} bytes — the caps are no longer being \
                 filled, so this gate is measuring nothing",
                line.len()
            );
        }
    }

    /// A record over the cap is REFUSED and NOTHING is written — not a truncated line, not an
    /// empty file, not even the directory.
    ///
    /// Driven through `append_record` with a hand-built [`ChangeRecord`], because every public
    /// `Change` constructor caps its cells and an over-budget record is therefore unreachable
    /// through [`ChangeJournal::append`]. That is the point of the caps and exactly why the belt
    /// behind them needs its own seam: an untestable guard is an untested one.
    #[test]
    fn an_oversized_record_is_refused_and_writes_nothing() {
        let r = root();
        let dir = r.path().join("changes");
        let j = ChangeJournal::new(dir.clone(), Proc::new("vike-test", 1, "0.1.0"));

        let oversized = ChangeRecord {
            ts_ms: T,
            seq: 0,
            kind: KIND_SET_SETTING,
            outcome: Outcome::Applied,
            actor: Actor::Boot,
            target: Target::Setting(SettingTarget {
                file: "policy.toml".into(),
                key: "policy.x".into(),
                old: None,
                new: "x".repeat(MAX_RECORD_BYTES * 2),
            }),
            reason: None,
            process: Proc::new("vike-test", 1, "0.1.0"),
        };
        match j.append_record(&oversized) {
            Err(ChangeJournalError::TooLarge { bytes }) => {
                assert!(bytes > MAX_RECORD_BYTES, "the reported size is the real one: {bytes}");
            }
            other => panic!("an oversized record must be refused, got {other:?}"),
        }
        assert!(!dir.exists(), "a refused record leaves no directory, let alone a torn line");

        // …and a record just UNDER the cap goes through the same path, so the refusal above is
        // about the size rather than about `append_record` being broken.
        let ok = ChangeRecord {
            target: Target::Setting(SettingTarget {
                file: "policy.toml".into(),
                key: "policy.x".into(),
                old: None,
                new: "x".repeat(MAX_RECORD_BYTES / 2),
            }),
            ..oversized
        };
        let path = j.append_record(&ok).expect("an in-budget record is written");
        assert_eq!(read_lines(&path).len(), 1);
    }

    /// Two records in the SAME millisecond are ordered by `seq`, which is what that field is for.
    #[test]
    fn two_records_in_one_millisecond_are_ordered_by_seq() {
        let r = root();
        let j = journal(r.path());
        let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, "1");
        let path = j.append(T, &c).expect("append");
        j.append(T, &c).expect("append");
        j.append(T, &c).expect("append");

        let lines = read_lines(&path);
        assert_eq!(lines.len(), 3, "three appends, three lines");
        let seqs: Vec<u64> = lines.iter().map(|v| v["seq"].as_u64().unwrap()).collect();
        assert!(seqs[0] < seqs[1] && seqs[1] < seqs[2], "strictly increasing: {seqs:?}");
        assert!(lines.iter().all(|v| v["ts_ms"] == T), "…within one millisecond");
    }

    /// Appends ACCUMULATE. A journal that truncated would look identical after one write, which is
    /// exactly how an append-only store stops being one without anyone noticing.
    #[test]
    fn appends_accumulate_rather_than_replace() {
        let r = root();
        let j = journal(r.path());
        for i in 0..25 {
            let c = Change::set_setting(
                Outcome::Applied,
                Actor::Gui,
                "policy.toml",
                "policy.max_notional_per_order",
                None,
                &i.to_string(),
            );
            j.append(T, &c).expect("append");
        }
        let lines = read_lines(&j.file_for(T));
        assert_eq!(lines.len(), 25);
        assert_eq!(lines[0]["target"]["new"], "0", "the FIRST record still exists");
        assert_eq!(lines[24]["target"]["new"], "24");
    }

    /// Concurrent appenders both land, and every line stays whole — the multi-writer requirement
    /// that ruled out an embedded database with an exclusive lock.
    ///
    /// Threads rather than processes here (a test binary cannot portably re-exec itself); the
    /// cross-PROCESS half is `crates/vike-model/tests/change_journal_concurrent.rs`, which spawns
    /// real child processes. This one exists because it is the cheap version that runs everywhere.
    #[test]
    fn concurrent_appenders_do_not_interleave() {
        let r = root();
        let dir = r.path().to_path_buf();
        let writers = 8;
        let each = 40;
        let handles: Vec<_> = (0..writers)
            .map(|w| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let j = ChangeJournal::new(dir, Proc::new("vike-test", w as u32, "0.1.0"));
                    for i in 0..each {
                        let c = Change::set_setting(
                            Outcome::Applied,
                            Actor::Gui,
                            "policy.toml",
                            "policy.max_notional_per_order",
                            None,
                            &format!("{w}-{i}"),
                        );
                        j.append(T, &c).expect("append");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("writer");
        }

        let lines = read_lines(&journal(r.path()).file_for(T));
        assert_eq!(lines.len(), writers * each, "every append landed");
        let mut seen: Vec<String> =
            lines.iter().map(|v| v["target"]["new"].as_str().unwrap().to_string()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), writers * each, "…and no two records were merged or lost");
    }

    /// ⚠ **THE APPEND PATH TAKES THE LOCK — and a contended append WAITS rather than losing its
    /// record.** This is the machine-checked half of the module doc's serialisation argument.
    ///
    /// What it can and cannot prove is worth stating plainly, because the two are easy to conflate.
    /// It PROVES that `append` blocks on `<dir>/`[`CHANGES_LOCK_FILE`] and completes once that lock
    /// is free — remove the lock and the writer finishes immediately, so this test goes red. It does
    /// NOT prove the Docker Desktop case: reproducing lost appends needs a filesystem that loses
    /// them, no CI box has one (there is no container runtime on any of them), and that half rests
    /// on the hand measurement recorded in the module doc and in
    /// `docs/ops/tradehub-container.md`.
    ///
    /// The lock is held from an INDEPENDENT descriptor in this same process, which works because
    /// `flock`/`LockFileEx` key on the open file description rather than on the process — see
    /// [`AppendLock`].
    #[test]
    fn an_append_waits_for_a_held_lock_instead_of_writing_beside_it() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;

        let r = root();
        let dir = r.path().to_path_buf();
        std::fs::create_dir_all(&dir).expect("journal dir");

        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(CHANGES_LOCK_FILE))
            .expect("open the sentinel");
        held.try_lock().expect("this test takes the append lock first");

        let started = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (s, d, wdir) = (started.clone(), done.clone(), dir.clone());
        let writer = std::thread::spawn(move || {
            let j = ChangeJournal::new(wdir, Proc::new("vike-test", 1, "0.1.0"));
            let c = Change::set_setting(
                Outcome::Applied,
                Actor::Gui,
                "policy.toml",
                "policy.max_notional_per_order",
                None,
                "250",
            );
            s.store(true, Ordering::SeqCst);
            j.append(T, &c).expect("the append completes once the lock is released");
            d.store(true, Ordering::SeqCst);
        });

        // ⚠ ANTI-VACUITY, half one: wait until the writer has genuinely REACHED the append. A
        // "still running" assertion against a thread that has not started yet measures nothing —
        // it is the shape of contention test that never contends.
        while !started.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        // …then a window in which it must NOT get through. Without the lock this append is a
        // create + one write + an fsync, so half a second is orders of magnitude of slack.
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(10));
            assert!(
                !done.load(Ordering::SeqCst),
                "the append completed while another descriptor held {CHANGES_LOCK_FILE} — the \
                 append path is NOT taking the lock"
            );
        }
        assert!(
            !dir.join(month_file_name(T)).exists(),
            "a blocked append must not have created the month file either"
        );

        // ⚠ ANTI-VACUITY, half two: release, and the SAME writer finishes. Without this the test
        // would pass just as well against a writer that had crashed, hung or never spawned.
        held.unlock().expect("release");
        drop(held);
        writer.join().expect("the writer finishes once the lock is free");
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(read_lines(&journal(r.path()).file_for(T)).len(), 1, "…and its record landed");
    }

    /// A lock that cannot be TAKEN AT ALL refuses the record loudly, and writes nothing — the
    /// module doc's second half. A filesystem this journal cannot serialise on is one whose
    /// completeness nobody can claim, so the caller gets an error it can log rather than a silence
    /// it cannot.
    ///
    /// Driven by putting a DIRECTORY where the sentinel belongs: `OpenOptions::open` cannot hand
    /// back a file handle for one on either platform this ships on, which is a portable way to
    /// reach a branch whose real-world cause (a filesystem whose locking errors) no box here has.
    #[test]
    fn a_lock_that_cannot_be_taken_refuses_the_record_and_writes_nothing() {
        let r = root();
        let dir = r.path().to_path_buf();
        std::fs::create_dir_all(dir.join(CHANGES_LOCK_FILE)).expect("a directory in the way");
        let j = journal(&dir);
        let c =
            Change::set_setting(Outcome::Applied, Actor::Gui, "policy.toml", "policy.k", None, "1");
        match j.append(T, &c) {
            Err(ChangeJournalError::Lock(e)) => {
                let msg = ChangeJournalError::Lock(e).to_string();
                assert!(msg.contains(CHANGES_LOCK_FILE), "the error names the sentinel: {msg}");
                assert!(msg.contains("REFUSED"), "…and says the record was not written: {msg}");
            }
            other => panic!("an unusable lock must refuse the record, got {other:?}"),
        }
        assert!(!j.file_for(T).exists(), "nothing was written unserialised");
    }

    /// Records land in the file for their OWN month, so a backdated record does not pollute the
    /// current one and the retention bound stays a calendar bound.
    #[test]
    fn each_record_lands_in_its_own_month() {
        let r = root();
        let j = journal(r.path());
        let jan = 1_767_225_600_000; // 2026-01-01T00:00:00Z
        let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, "1");
        assert_eq!(j.append(jan, &c).unwrap().file_name().unwrap(), "changes-2026-01.jsonl");
        assert_eq!(j.append(T, &c).unwrap().file_name().unwrap(), "changes-2026-08.jsonl");
        assert_eq!(month_file_name(-1), "changes-1969-12.jsonl", "pre-1970 floors toward -inf");
    }

    /// Retention: the newest `max_files` months survive, oldest first. Names sort in calendar
    /// order, so this needs no clock and no `stat`.
    #[test]
    fn prune_keeps_the_newest_months_and_removes_the_oldest() {
        let r = root();
        let j = journal(r.path());
        std::fs::create_dir_all(r.path()).unwrap();
        for (y, m) in [(2024, 11), (2024, 12), (2025, 1), (2025, 2), (2026, 8)] {
            std::fs::write(r.path().join(format!("changes-{y:04}-{m:02}.jsonl")), b"{}\n").unwrap();
        }
        let pruned = j.prune(Some(2));
        assert_eq!((pruned.found, pruned.removed, pruned.failed), (5, 3, 0), "{pruned:?}");
        assert!(!r.path().join("changes-2024-11.jsonl").exists());
        assert!(!r.path().join("changes-2025-01.jsonl").exists());
        assert!(r.path().join("changes-2025-02.jsonl").exists(), "second-newest kept");
        assert!(r.path().join("changes-2026-08.jsonl").exists(), "newest kept");
    }

    /// Under the limit the prune removes NOTHING — the anti-vacuity twin. A prune that deleted on
    /// every call would take out the month currently being written.
    #[test]
    fn prune_below_the_limit_removes_nothing() {
        let r = root();
        let j = journal(r.path());
        std::fs::create_dir_all(r.path()).unwrap();
        for m in 1..=3 {
            std::fs::write(r.path().join(format!("changes-2026-{m:02}.jsonl")), b"{}\n").unwrap();
        }
        assert_eq!(
            j.prune(Some(DEFAULT_MAX_CHANGE_FILES)),
            Pruned { found: 3, removed: 0, failed: 0 }
        );
        assert!(r.path().join("changes-2026-01.jsonl").exists());
    }

    /// `None` is retention OFF, asserted against a population that WOULD be pruned under the
    /// default — so this cannot pass by a limit nobody reached.
    #[test]
    fn prune_with_no_limit_keeps_everything() {
        let r = root();
        let j = journal(r.path());
        std::fs::create_dir_all(r.path()).unwrap();
        let n = DEFAULT_MAX_CHANGE_FILES + 4;
        for i in 0..n {
            let (y, m) = (2000 + i / 12, i % 12 + 1);
            std::fs::write(r.path().join(format!("changes-{y:04}-{m:02}.jsonl")), b"{}\n").unwrap();
        }
        assert_eq!(j.prune(None), Pruned::default(), "no limit means no work at all");
        assert_eq!(std::fs::read_dir(r.path()).unwrap().count(), n);
        assert_eq!(j.prune(Some(DEFAULT_MAX_CHANGE_FILES)).removed, 4, "…and the default bites");
    }

    /// An absent directory is the ordinary state of a project that has changed nothing — a startup
    /// must not fail over housekeeping.
    #[test]
    fn prune_of_an_absent_directory_is_silent() {
        let r = root();
        let j = ChangeJournal::new(r.path().join("never"), Proc::new("t", 1, "0"));
        assert_eq!(j.prune(Some(1)), Pruned::default());
    }

    /// ⚠ The prune DELETES what [`is_month_file_name`] accepts, so anything it is unsure about must
    /// fall outside — and must not be counted either, or the bound bites early on files it will
    /// never remove.
    #[test]
    fn prune_leaves_anything_that_is_not_a_monthly_file_alone() {
        let r = root();
        let j = journal(r.path());
        std::fs::create_dir_all(r.path()).unwrap();
        let strangers = [
            "changes-old.jsonl",
            "changes-2026-08.jsonl.bak",
            "changes-2026-8.jsonl",
            "changes-20260-8.jsonl",
            "export.csv",
            "README",
            // ⚠ The append lock's own sentinel. Deleting the file every writer coordinates on
            // would be housekeeping breaking the serialisation — see [`CHANGES_LOCK_FILE`].
            CHANGES_LOCK_FILE,
        ];
        for name in strangers {
            std::fs::write(r.path().join(name), b"not mine\n").unwrap();
        }
        for m in 1..=4 {
            std::fs::write(r.path().join(format!("changes-2026-{m:02}.jsonl")), b"{}\n").unwrap();
        }
        let pruned = j.prune(Some(1));
        assert_eq!(pruned.found, 4, "only real monthly files are counted: {pruned:?}");
        assert_eq!(pruned.removed, 3);
        for name in strangers {
            assert!(r.path().join(name).exists(), "{name} must survive housekeeping");
        }
    }

    /// The file-name matcher, at the boundaries the prune turns on.
    #[test]
    fn the_month_file_matcher_is_strict() {
        assert!(is_month_file_name("changes-2026-08.jsonl"));
        assert!(is_month_file_name("changes-0001-01.jsonl"));
        assert!(!is_month_file_name("changes-2026-8.jsonl"), "the month must be two digits");
        assert!(!is_month_file_name("changes-2026_08.jsonl"), "the separator is a hyphen");
        assert!(!is_month_file_name("changes-2026-08.jsonl.bak"));
        assert!(!is_month_file_name("changes-2026-08.json"));
        assert!(!is_month_file_name("2026-08.jsonl"));
        assert!(!is_month_file_name("changes-.jsonl"));
        // Every name this module MINTS must be accepted — the two halves cannot drift apart.
        for ts in [-1i64, 0, T, 4_102_444_800_000] {
            assert!(is_month_file_name(&month_file_name(ts)), "{ts}");
        }
    }

    /// Control characters never reach the file, whatever the caller passed. Belt and braces: the
    /// JSON escape would already keep the framing intact, and `grep`, a terminal and a naive
    /// splitter all see the raw bytes.
    #[test]
    fn no_control_character_survives_into_the_line() {
        let r = root();
        let j = journal(r.path());
        let nasty = "flat\n{\"kind\":\"forged\",\"seq\":0}\r\nmore\u{0}\u{7f}\u{85}";
        let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, nasty)
            .with_reason(Some(nasty));
        let path = j.append(T, &c).expect("append");

        let raw = std::fs::read_to_string(&path).expect("read");
        assert_eq!(raw.lines().count(), 1, "one record is still one line: {raw:?}");
        assert!(!raw.trim_end().contains('\n'), "no embedded newline");
        assert!(!raw.contains('\u{0}') && !raw.contains('\r'), "no NUL, no CR");
        let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("still parses");
        assert!(v["target"]["new"].as_str().unwrap().starts_with("flat{"));
    }

    /// Every cell is BYTE-capped on a char boundary, so a multi-byte sequence is never split into
    /// invalid UTF-8 — the trap a byte-wise truncation walks into.
    #[test]
    fn cells_are_byte_capped_without_splitting_a_multibyte_char() {
        let snow = "☃";
        assert_eq!(snow.len(), 3, "the fixture must actually be multi-byte");
        let long = snow.repeat(MAX_FIELD_BYTES);
        let out = clean_field(&long);
        assert!(out.len() <= MAX_FIELD_BYTES);
        assert!(out.chars().all(|c| c == '☃'), "no partial sequence: {out:?}");
        assert_eq!(out.len() % 3, 0, "whole chars only");
        // Under the cap, nothing is touched.
        assert_eq!(clean_field("policy.max_notional_per_order"), "policy.max_notional_per_order");
    }

    /// The directory is created on first append, because `settings/state/changes` is not a marker
    /// and a fresh install has none.
    #[test]
    fn the_journal_directory_is_created_on_first_append() {
        let r = root();
        let dir = r.path().join("state").join(CHANGES_SUBDIR);
        assert!(!dir.exists(), "precondition");
        let j = ChangeJournal::new(dir.clone(), Proc::new("t", 1, "0"));
        let c = Change::set_setting(Outcome::Applied, Actor::Boot, "f.toml", "f.k", None, "1");
        j.append(T, &c).expect("append creates the directory");
        assert!(dir.is_dir());
    }

    /// `in_state_dir` puts the journal exactly where the module doc says, under an ALREADY-RESOLVED
    /// state directory rather than a fresh walk.
    #[test]
    fn in_state_dir_joins_the_changes_subdirectory() {
        let state = Path::new("/srv/vike-<unit>/settings/state");
        let j = ChangeJournal::in_state_dir(state, Proc::new("t", 1, "0"));
        assert_eq!(j.dir(), state.join(CHANGES_SUBDIR));
        assert_eq!(CHANGES_SUBDIR, "changes");
    }
}
