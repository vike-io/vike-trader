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
//! ⚠ That per-append lock does NOT reopen the verdict — but the REASON this paragraph gave until
//! 2026-09-13 was measured FALSE, and is corrected here rather than quietly deleted. It read: "an
//! embedded store takes its write lock for the life of a CONNECTION, so the GUI being open is enough
//! to stop the daemon recording." That is true of redb and NOT of SQLite, which holds an exclusive
//! lock for the duration of a write TRANSACTION and, in WAL mode, serves readers concurrently with a
//! writer. The verdict SURVIVES on the arguments that did hold: ~40 records/day is ~4 MB/year with
//! no query to run, JSONL costs zero new packages on this crate's dependency floor, and a line is
//! readable by `jq`, by `grep`, and by a reader who has never heard of this module.
//! `docs/decisions/0054-settings-move-into-one-database.md` is where the journal verdict lives now —
//! it carries that half forward; `docs/decisions/0028-settings-stay-files-change-journal-is-jsonl.md`
//! reads `superseded` and stays readable for its measurements only. This one is taken and released
//! inside a single bounded append by every writer symmetrically, and the kernel releases it if the
//! holder dies.
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
//! (`crates/vike-journal/src/lock.rs`'s `JournalLock`, `crates/vike-ops/src/live_lock.rs`'s
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
//! | `credential_write` | **YES** | both callers of `crates/vike-secrets/src/env_write.rs`'s `save_credentials` — see below |
//! | `boot_settings` | **YES** | `crates/vike-boot/src/lib.rs`'s `journal_boot_settings`, called by the roots that ENFORCE the ceilings — see below |
//! | `venue_mounted` | **YES** | `crates/vike-mount/src/node.rs`'s `journal_venue_mounts`, called from `crates/vike-tradehub/src/tradehub_cli.rs` — the one root that mounts venues. Added by #1502 (the kind) and #1504 (the writer). ⚠ This row named TWO roots until the desktop cut took the local trading core out of the GUI shell (`crates/vike-desktop/src/main.rs`, then spelled `vike-app`): that binary mounts nothing now, so it writes no `venue_mounted` record and its absence from this column is a property rather than a gap |
//! | a HAND-EDIT of a settings file | no, and it never will be | nothing observes one; see the note below |
//! | RETENTION — [`ChangeJournal::prune`], not a record kind | **no** | nobody. Every call site is this file's own `#[cfg(test)]` module. The bound is written and tested, and no root arms it — see "Retention is not optional" above for why the disk cost of that is negligible and why arming it is a separate decision |
//!
//! **`credential_write` — the two call sites, all journalled.** All reach one function,
//! `crates/vike-secrets/src/env_write.rs`'s `save_credentials` (re-exported through
//! `crates/vike-connections/src/env_write.rs`), which is the workspace's ONE sanctioned credential
//! write — an in-place upsert of named keys that leaves every other line byte-identical:
//!
//!   * `crates/vike-connections/src/view.rs`'s `render_edit_form` — the GUI Connections editor's
//!     Save arm. Actor: [`Actor::Gui`].
//!   * `crates/bridges/ctrader/src/token_store.rs`'s `persist` — a grant the VENUE rotated, not
//!     something a human did. Actor: [`Actor::venue`] with `"ctrader"`. This one is the reason the
//!     `venue` origin exists at all.
//!
//! The Data Manager's Polymarket proxy box was the third until decision 0095; it writes
//! `venue.polymarket.socks_proxy` now — a `set_setting` record (file `venue`, value `<secret>`)
//! through `crates/vike-secrets/src/settings.rs`'s `set_venue_setting_in_journalled`.
//!
//! ⚠ **`save_credentials` itself does NOT journal, and the reason this used to give — that it
//! CANNOT — is false.** It read: *"`crates/vike-secrets/Cargo.toml` declares NO `vike-*`
//! dependency — the property that lets `vike-bridge-core` (the transport stack) and `vike-cli`
//! (transport-free) both link it — so that crate cannot name this module at all."* (It had been
//! corrected once already: it said *"declares a literally EMPTY `[dependencies]`"* until
//! 2026-09-13, when decision 0054 put `rusqlite` there, and that fix argued the `vike-*` clause
//! was the structural one and untouched.) The `vike-*` clause fell on 2026-09-20 —
//! `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` admitted
//! `vike-model` — so `vike-secrets` can name THIS module today. The property that survives is the
//! RANK: that crate names nothing above the vocabulary floor, tier `leaf`'s rule, machine-checked
//! by `crates/vike-ops/tests/layer_gate.rs`'s
//! `every_tier_15_crate_names_nothing_above_the_vocabulary` — and this module is inside that
//! floor, which is why the impossibility does not follow from it any more.
//!
//! ⚠ **So it is a standing arrangement, and whether to collapse it is an OPEN QUESTION recorded
//! rather than decided.** `crates/vike-connections/src/env_write.rs`'s module doc carries the
//! argument on both sides; it is the same shape as the three open questions 0072's own amendment
//! records, and a sweep of a false sentence is not the place to settle a seam. What is true
//! TODAY, and is all a reader here needs: the CALLER writes the record, over a
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
//! that daemon's log). Two of the five composition roots call it —
//! `crates/vike-tradehub/src/tradehub_cli.rs`, which ENFORCES the ceilings and mounts the venues,
//! and the GUI shell (`crates/vike-desktop/src/main.rs`, then spelled `vike-app`) — and the other
//! three carry a row in `crates/vike-boot/tests/boot_journal_wiring.rs` arguing why an anchor from
//! them would be noise (`vike-cli` runs for every subcommand) or a FALSEHOOD (`vike-recorder` and
//! `vike-datahub` sign no orders, and the latter loads no settings at all, so a record claiming the
//! effective ceilings would be reporting compiled-in defaults as though a file had been obeyed).
//!
//! ⚠ **That sentence read "the two that ENFORCE the ceilings and mount venues", and the desktop
//! cut made half of it false.** The GUI shell links no mount at all now — all orders and trading
//! leave it through the backend — so it RENDERS the policy ceiling
//! (`vike_config::venue_arming::ceilings_only`) and enforces nothing, while still writing an
//! anchor. Whether it should keep writing one is the same question `NO_BOOT_ANCHOR` answers for
//! `vike-recorder` and `vike-datahub`, asked of a root whose answer used to be obvious; it is
//! recorded here rather than settled here.
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
//! #1504 wired `crates/vike-mount/src/node.rs`'s `journal_venue_mounts`, which writes one record per
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
/// `as_str`, because almost every credential key is one venue at one tier. A key whose one value
/// governs every tier of its venue — the Polymarket egress proxy, `POLY_SOCKS_PROXY`, while the Data
/// Manager's box wrote it as a credential (it is the `venue.polymarket.socks_proxy` setting since
/// decision 0095) — is not: picking any of the three tiers would assert something false about the
/// two it did not pick, and an empty string would read as "the writer did not know" rather than
/// "there is nothing to know".
///
/// It is a CONSTANT rather than a literal at the one call site because the cell is a `jq` query's
/// vocabulary: a reader filtering `select(.target.tier=="LIVE")` needs to be able to find out what
/// else that field can hold, and the type is the only place that can tell them. Same spirit as
/// [`CredentialTarget::venue`]'s documented `"multi"`.
pub const TIER_UNTIERED: &str = "untiered";

/// **The `venue` cell for a write that spans several venues rather than naming one** — what
/// `vike-cli secrets migrate` records, since a migration carries every venue in the store at once.
///
/// ⚠ It is a CONSTANT for the reason [`TIER_UNTIERED`] is one, and it became a constant late: until
/// then the spelling existed only as a doc mention on [`CredentialTarget::venue`] and was passed as
/// a bare literal at its one call site, while new prose beside that call claimed *"both are
/// constants on the type that owns the cell, so a `jq` reader can learn the field's domain"*. That
/// was true of the tier and false of this one, which is the half a reader filtering
/// `select(.target.venue=="multi")` actually needs.
pub const VENUE_MULTI: &str = "multi";

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
    /// **The change did NOT take effect, and something on disk changed anyway** — the state is
    /// neither the old one nor the intended one, and the `reason` cell says what to put back.
    ///
    /// ⚠ It exists because [`Outcome::Refused`] was being used for it, and `Refused`'s own
    /// definition — *"the change was refused and nothing was written"* — was then the opposite of
    /// what had happened. A ledger row a reader BRANCHES on may not say the opposite of the event
    /// it records; that is the failure this whole ledger exists to remove, and a writer reproducing
    /// it in its own error path is worse than a gap.
    ///
    /// Today's one producer is `vike_config`'s `SettingsWriteError::Stranded` (the settings write
    /// whose replacement could not be landed after the previous file had been moved aside, so the
    /// target is ABSENT and the operator's bytes are at `<file>.toml.bak`), reached through
    /// `vike_config::journal_outcome`. It is spelled generally because the SHAPE is not
    /// settings-specific: any two-step landing has this third answer, and the ledger's vocabulary
    /// should not have to grow a variant per writer.
    Stranded,
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
        /// Supplied by `vike_node_proto::auth`'s `NodeKeys::key_id`: `nk-` plus 16 hex
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
    /// `kind = "account_book"` — an account ROW learned which book it is, in the settings database.
    AccountBook(AccountBookTarget),
    /// `kind = "account_lifecycle"` — an account ROW was created, renamed, (de)activated or removed.
    AccountLifecycle(AccountLifecycleTarget),
}

/// `kind` for [`Target::Setting`].
pub const KIND_SET_SETTING: &str = "set_setting";
/// `kind` for [`Target::Credential`].
pub const KIND_CREDENTIAL_WRITE: &str = "credential_write";
/// `kind` for [`Target::BootSettings`].
pub const KIND_BOOT_SETTINGS: &str = "boot_settings";

/// `kind` for [`Target::VenueMounted`].
pub const KIND_VENUE_MOUNTED: &str = "venue_mounted";

/// `kind` for [`Target::AccountBook`].
///
/// ⚠ **A kind of its own rather than a [`KIND_CREDENTIAL_WRITE`] record with an odd key name**, and
/// the distinction is the point: what changes here is not a credential and carries no value. A
/// ledger line that said `credential_write` for it would make *"which credentials changed in
/// September"* answer with a row in which none did.
pub const KIND_ACCOUNT_BOOK: &str = "account_book";

/// `kind` for [`Target::AccountLifecycle`].
///
/// ⚠ **A kind of its own rather than a second shape of [`KIND_ACCOUNT_BOOK`]** — see
/// [`AccountLifecycleTarget`]. That one answers *which broker*; this one answers *which rows
/// exist*, and the second is the question somebody has to be able to ask after a delete.
pub const KIND_ACCOUNT_LIFECYCLE: &str = "account_lifecycle";

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
pub const KINDS: &[&str] = &[
    KIND_SET_SETTING,
    KIND_CREDENTIAL_WRITE,
    KIND_BOOT_SETTINGS,
    KIND_VENUE_MOUNTED,
    KIND_ACCOUNT_BOOK,
    KIND_ACCOUNT_LIFECYCLE,
];

impl Target {
    /// The `kind` string for this target. Total by construction — a new variant that forgets a kind
    /// is a compile error, not a record that serializes with the wrong discriminator.
    pub fn kind(&self) -> &'static str {
        match self {
            Target::Setting(_) => KIND_SET_SETTING,
            Target::Credential(_) => KIND_CREDENTIAL_WRITE,
            Target::BootSettings(_) => KIND_BOOT_SETTINGS,
            Target::VenueMounted(_) => KIND_VENUE_MOUNTED,
            Target::AccountBook(_) => KIND_ACCOUNT_BOOK,
            Target::AccountLifecycle(_) => KIND_ACCOUNT_LIFECYCLE,
        }
    }
}

/// A settings write — taken verbatim from `vike_config::write::SettingsWrite`, whose own doc calls
/// itself "the audit record's raw material".
///
/// ⚠ **`old` ABSENT is not `old: null`.** On an APPLIED row an absent field means the key was not
/// in the file at all; a present empty string means the key was there and held nothing. The
/// distinction is `SettingsWrite`'s (`old_value: Option<String>`), it is the one `vike-tradehub`'s
/// `audit::sanitize_value` preserves, and collapsing it would make "the ceiling was unset" and "the
/// ceiling was blank" read identically.
///
/// ⚠ **READ THE [`Outcome`] FIRST: on a row that did not apply, these cells describe the ATTEMPT,
/// not the file.** A refusing writer has no old value to report — it may have refused before
/// reading one ([`Outcome::Refused`] from a bad key or a held lock), or had the whole previous file
/// moved aside underneath it ([`Outcome::Stranded`]) — so both surfaces that journal a failure pass
/// `old: None` and `new` = the value that was being attempted. Taking absence as "the key was
/// previously unset" is therefore only sound on an applied row, and on a `Stranded` one it is
/// actively wrong: that outcome says the file this cell would describe is no longer where it was.
/// This is stated here, on the type, rather than fixed by making the cells optional, because the
/// absence has the SAME cause on every non-applied row and a per-outcome shape would invite a
/// reader to trust the cells on the rows that still carry them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingTarget {
    /// The file the write AIMED AT (`"policy.toml"`) — edited on an applied row, merely named on a
    /// refused or stranded one.
    pub file: String,
    /// The full dotted key (`"policy.max_notional_per_order"`).
    pub key: String,
    /// The file's previous value, rendered as TOML. ABSENT when the key was not set in the file —
    /// ⚠ and also whenever the row's [`Outcome`] is not an applied one, where no previous value was
    /// established at all. See the type's own doc: branch on the outcome before reading this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// The value written, rendered as TOML — ⚠ or, on a row whose [`Outcome`] is not an applied
    /// one, the value that was being attempted and did NOT land.
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

/// **An account ROW learned which book it is** — the settings database's `account.venue_account_id`.
///
/// # Why this is in the ledger at all
///
/// A `venue_account_id` is the venue's own name for the BOOK an account trades. Getting it wrong
/// does not leak anything and does not fail loudly: it routes orders to another broker. The two
/// accounts that motivated the column are `DUKASCOPY_DEMO1_*` and `DUKASCOPY_DEMO2_*`, which are
/// Dukascopy Bank SA and Dukascopy Europe IBS AS — two legal entities under one venue id — and
/// after `docs/superpowers/specs/2026-09-14-the-credential-schema.md`'s migration they are two rows
/// distinguishable only by `id`. *When was account 7 told it was 1234567, and what did it say
/// before* is therefore a question somebody eventually has to answer, and a delta channel is the
/// only thing that can.
///
/// # ⚠ Every cell here is an identifier, and NONE of them is a secret
///
/// The ids, the venue, the tier and the book are all things the venue prints on its own pages and
/// echoes on its own wire; the root `CLAUDE.md`'s rule is about credential VALUES, and this record
/// reaches none. There is deliberately no `value`, no key name and no store content of any kind —
/// the same structural property [`CredentialTarget`] holds, reached the same way: there is no
/// parameter for one on [`Change::account_book`].
///
/// [`AccountBookTarget::old`] is recorded BECAUSE the wrong-broker failure is the one this row
/// exists for: without the previous value a mistaken write is visible and not reversible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountBookTarget {
    /// The store that was written — `"vike.db"`. A file name rather than a path, the same cell
    /// shape (and the same reason) as [`CredentialTarget::store`].
    store: String,
    /// The `account.id` — the identity, and on a multi-account venue the ONLY thing that separates
    /// two rows. An integer rather than a string because it is one in the schema, and rendering it
    /// as text would invite a reader to match it against a label.
    account_id: i64,
    /// The row's venue id, from `vike_model::VENUES`.
    venue: String,
    /// The row's tier — `paper` / `demo` / `live`, the arming ceiling's vocabulary.
    tier: String,
    /// What the row named BEFORE this write. ABSENT when the book was not yet known, which is the
    /// state every migrated row starts in — so an absent `old` is *this row learned its book* and a
    /// present one is *this row was re-pointed*, which are very different events.
    #[serde(skip_serializing_if = "Option::is_none")]
    old: Option<String>,
    /// The book the row names now — ABSENT when this write CLEARED the column back to *not yet
    /// known*.
    ///
    /// ⚠ A clear is a real and necessary event on this column, not an omission: a pair of rows
    /// written the wrong way round can only be repaired by clearing one of them first (ruling 11's
    /// one-account-per-book index refuses every direct correction), so a ledger that could not
    /// record one would go silent on the step that actually moves a book between two brokers.
    /// `old` present with `new` absent is exactly *this row stopped naming a book*.
    #[serde(skip_serializing_if = "Option::is_none")]
    new: Option<String>,
}

impl AccountBookTarget {
    /// The store file name.
    pub fn store(&self) -> &str {
        &self.store
    }

    /// The `account.id` that was written.
    pub fn account_id(&self) -> i64 {
        self.account_id
    }

    /// The row's venue.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The row's tier.
    pub fn tier(&self) -> &str {
        &self.tier
    }

    /// The book the row named before — `None` when it did not name one. See the field's own doc:
    /// this absence distinguishes *learned* from *re-pointed*.
    ///
    /// ⚠ The ACCESSOR is `previous_book` while the serialized field stays `old`, matching
    /// [`SettingTarget`]'s wire shape — the two are deliberately allowed to differ. A reader parses
    /// `old`/`new`; a caller in Rust reads a name that says what the value is. Its sibling below is
    /// the reason the pair moved at all: `fn new(&self) -> &str` is `clippy::new_ret_no_self`, a
    /// `-D warnings` failure, because `new` is the constructor's name in every other type.
    pub fn previous_book(&self) -> Option<&str> {
        self.old.as_deref()
    }

    /// The book the row names now — `None` when this write CLEARED it. Serialized as `new`; see
    /// [`AccountBookTarget::previous_book`] for why the accessor is not called that.
    pub fn book(&self) -> Option<&str> {
        self.new.as_deref()
    }

    /// Whether this write RE-POINTED an account that already named a book, rather than teaching one
    /// that named none. The question a reviewer of this ledger asks first.
    pub fn repointed(&self) -> bool {
        self.old.is_some()
    }

    /// Whether this write took a book AWAY rather than assigning one — `old` present, `new` absent.
    ///
    /// The repair step: clearing one row is how a swapped pair is corrected, so a run of records
    /// reading *cleared, assigned, assigned* is a repair rather than three unrelated writes.
    pub fn cleared(&self) -> bool {
        self.new.is_none()
    }
}

/// **An account ROW was created, renamed, deactivated or removed** — the settings database's
/// `account` table, everything [`AccountBookTarget`] is not about.
///
/// # Why this is a kind of its own rather than a second shape of `account_book`
///
/// The same argument [`KIND_ACCOUNT_BOOK`] makes against riding [`KIND_CREDENTIAL_WRITE`], one rung
/// along: what changes here is not a book, and a ledger line that said `account_book` for a REMOVE
/// would make *"when was account 7 re-pointed"* answer with a row in which it was not. The two
/// kinds also answer different questions — that one is *which broker*, this one is *which rows
/// exist* — and the second is the one that has to be answerable after somebody deletes something.
///
/// # ⚠ Every cell here is an identifier, and NONE of them is a secret
///
/// Ids, the venue, the tier, labels and credential key NAMES. The key names are recorded for
/// exactly the reason [`CredentialTarget::keys`] records them — they are not secret
/// (`vike-cli secrets list` prints them by an explicit decision), and *"what did that account own
/// when it was removed"* is unanswerable without them. There is deliberately no `value` parameter
/// on [`Change::account_lifecycle`], which is the enforcement rather than a convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountLifecycleTarget {
    /// The store that was written — `"vike.db"`. A file name rather than a path, the same cell
    /// shape (and the same reason) as [`CredentialTarget::store`].
    store: String,
    /// The act: `create` / `rename` / `activate` / `deactivate` / `remove`. The caller's rendering
    /// of `vike_secrets::AccountEdit::verb`, which is total over that enum — this crate takes a
    /// string for the reason [`VenueMountTarget::block`] does, since `vike-secrets` sits beside
    /// this crate rather than under it.
    verb: String,
    /// The `account.id` the act names. ⚠ For a `create` this is the id the INSERT was given, so a
    /// reader may not assume a record of this kind describes a row that still exists — a later
    /// `remove` record for the same id is exactly the pair that says it does not.
    account_id: i64,
    /// The row's venue id, from `vike_model::VENUES`.
    venue: String,
    /// The row's tier — one of `vike_secrets::ACCOUNT_TIERS`.
    tier: String,
    /// The label the row carried BEFORE — ABSENT when it carried none, which is the state every
    /// migrated row is in and the state a `create` starts from.
    #[serde(skip_serializing_if = "Option::is_none")]
    old_label: Option<String>,
    /// The label the row carries now — ABSENT when it carries none, and absent on a `remove`,
    /// which leaves no row to carry one.
    #[serde(skip_serializing_if = "Option::is_none")]
    new_label: Option<String>,
    /// Whether the row is ACTIVE after the act. `false` on a `deactivate`; absent-as-`false` is not
    /// available here because *the row is off* is the whole content of that record.
    active: bool,
    /// The live credential key NAMES the row owned at the moment of the act — **names, never
    /// values**, capped at [`MAX_CREDENTIAL_KEYS`] like [`CredentialTarget::keys`].
    ///
    /// ⚠ It is what makes a `deactivate` record answerable later: an account is deactivated
    /// BECAUSE of what it owns, and a row recording only the id would leave the reader of a
    /// six-month-old ledger unable to say which keys stopped being used. A `remove` record always
    /// carries an EMPTY list, structurally — the store refuses to delete a row that owns any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    keys: Vec<String>,
}

impl AccountLifecycleTarget {
    /// The store file name.
    pub fn store(&self) -> &str {
        &self.store
    }

    /// The act, as the PRODUCER spelled it — `crates/vike-secrets/src/db.rs`'s `AccountEdit`, whose
    /// match is the authority and renders `set-tier` among them.
    ///
    /// ⚠ **The vocabulary is deliberately not listed here.** It was, and the list was one act short
    /// within a day of a new one landing — this crate is below `vike-secrets` and cannot name the
    /// enum in a doc link, so a copy here is a copy nothing holds in step. That is the same reason
    /// the producer's own doc gives for not restating it either.
    pub fn verb(&self) -> &str {
        &self.verb
    }

    /// The `account.id` the act named. See the field's own doc for why this is not a claim that the
    /// row still exists.
    pub fn account_id(&self) -> i64 {
        self.account_id
    }

    /// The row's venue.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The row's tier.
    pub fn tier(&self) -> &str {
        &self.tier
    }

    /// The label the row carried before — `None` when it carried none.
    pub fn previous_label(&self) -> Option<&str> {
        self.old_label.as_deref()
    }

    /// The label the row carries now — `None` when it carries none, or when there is no row.
    pub fn label(&self) -> Option<&str> {
        self.new_label.as_deref()
    }

    /// Whether the row is active after the act.
    pub fn active(&self) -> bool {
        self.active
    }

    /// The credential key NAMES the row owned — names, never values.
    pub fn keys(&self) -> &[String] {
        &self.keys
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
    ///
    /// ⚠ It also builds the rows for writes that did NOT apply, which is why `outcome` is the first
    /// parameter rather than an afterthought: on a [`Outcome::Refused`] or [`Outcome::Stranded`]
    /// row `old`/`new` describe the ATTEMPT and not the file. [`SettingTarget`]'s own doc is the
    /// authority for how a reader is meant to take them.
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
    /// An account row learning which BOOK it is — `account.venue_account_id`, in the settings
    /// database.
    ///
    /// `old` is the book the row named before, `None` when it named none (every migrated row starts
    /// there). `new` is the book it names now, `None` when this write CLEARED the column — the
    /// repair step a swapped pair needs. See [`AccountBookTarget`] for why this kind exists at all
    /// rather than riding [`Change::credential_write`], and why the absence of either is
    /// load-bearing.
    ///
    /// ⚠ **There is no `value` parameter and there never may be**: nothing this constructor accepts
    /// is a credential, and the only way to put one in a record of this kind is to change this
    /// signature — a diff a reviewer sees, which is the same discipline
    /// [`Change::credential_write`] holds.
    #[allow(clippy::too_many_arguments)]
    pub fn account_book(
        outcome: Outcome,
        actor: Actor,
        store: &str,
        account_id: i64,
        venue: &str,
        tier: &str,
        old: Option<&str>,
        new: Option<&str>,
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::AccountBook(AccountBookTarget {
                store: cap_bytes(&strip_control(store), MAX_IDENT_BYTES),
                account_id,
                venue: cap_bytes(&strip_control(venue), MAX_IDENT_BYTES),
                tier: cap_bytes(&strip_control(tier), MAX_IDENT_BYTES),
                old: old.map(|o| cap_bytes(&strip_control(o), MAX_IDENT_BYTES)),
                new: new.map(|n| cap_bytes(&strip_control(n), MAX_IDENT_BYTES)),
            }),
            reason: None,
        }
    }

    /// An account ROW created, renamed, (de)activated or removed — `vike_secrets::edit_account`'s
    /// side of the settings database, everything [`Change::account_book`] is not about.
    ///
    /// `keys` are credential key NAMES, reduced to `[A-Za-z0-9_]` by the same [`clean_key_name`]
    /// [`Change::credential_write`] applies, so a caller that passed the wrong thing still cannot
    /// get punctuation, whitespace or a line terminator into the record.
    ///
    /// ⚠ **There is no `value` parameter and there never may be**: nothing this constructor accepts
    /// is a credential, and the only way to put one in a record of this kind is to change this
    /// signature — a diff a reviewer sees, which is the same discipline
    /// [`Change::credential_write`] and [`Change::account_book`] both hold.
    #[allow(clippy::too_many_arguments)]
    pub fn account_lifecycle(
        outcome: Outcome,
        actor: Actor,
        store: &str,
        verb: &str,
        account_id: i64,
        venue: &str,
        tier: &str,
        old_label: Option<&str>,
        new_label: Option<&str>,
        active: bool,
        keys: &[&str],
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::AccountLifecycle(AccountLifecycleTarget {
                store: cap_bytes(&strip_control(store), MAX_IDENT_BYTES),
                verb: cap_bytes(&strip_control(verb), MAX_IDENT_BYTES),
                account_id,
                venue: cap_bytes(&strip_control(venue), MAX_IDENT_BYTES),
                tier: cap_bytes(&strip_control(tier), MAX_IDENT_BYTES),
                old_label: old_label.map(|l| cap_bytes(&strip_control(l), MAX_IDENT_BYTES)),
                new_label: new_label.map(|l| cap_bytes(&strip_control(l), MAX_IDENT_BYTES)),
                active,
                keys: keys.iter().copied().take(MAX_CREDENTIAL_KEYS).map(clean_key_name).collect(),
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
    /// as `crates/vike-journal/src/lock.rs`'s `JournalLock` and
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
/// inherit it. (Same caveat, same words, as `crates/vike-journal/src/lock.rs`'s `JournalLock`.)
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

#[path = "change_journal_tests.rs"]
#[cfg(test)]
mod change_journal_tests;
