//! Where PROGRAM-WRITTEN STATE lives — one root, one resolver, for every file the program writes
//! and no human edits.
//!
//! Ported from nothing: net-new Rust surface, Phase 2 of the settings-unification design
//! (`docs/superpowers/specs/2026-08-04-settings-unification-design.md`). STATE is that taxonomy's
//! sixth type, and the test that separates it from a SETTING is: *delete it — does behaviour
//! change permanently, or does the program re-derive it?* A window layout, an alert rule set and a
//! measured venue pace all re-derive. None of them is configuration, and none of them belonged
//! where it independently ended up — each in a different directory, chosen by that file's own
//! history rather than by anything the three have in common.
//!
//! # Where it lives
//!
//! `<project>/settings/state` — [`project_state_dir`], a sub-directory of the ONE settings
//! directory every setting and credential lives in. `$VIKE_STATE_ROOT` names it outright for an
//! operator who wants no walk, and `None` (no project above the working directory) means the caller
//! keeps whatever path it used before rather than this module inventing one from the CWD — the exact
//! failure [`crate::paths::store_path`] exists to prevent for stores.
//!
//! ⚠ **`VIKE_STATE_ROOT`, not `VIKE_STATE_DIR`** — that name was already taken, by `vike-app`'s
//! strategy-state SIDECAR directory (the per-mount `strategy_state::write_json_atomic` blobs).
//! Reusing it would have given one variable two meanings: an operator pointing it at the state root
//! would silently dump every strategy sidecar into that root, and an operator setting it for the
//! sidecars would silently relocate the window layout. So both names survived, each meaning what it
//! meant — until the unread-settings sweep REMOVED `VIKE_STATE_DIR` (a set value is refused at
//! startup; `vike_config::REMOVED_ENV` carries the row), leaving `VIKE_STATE_ROOT` the one name.
//!
//! ⚠ **The fold this paragraph used to call "the obvious follow-up" HAS LANDED, and it needed no
//! dual read** — the sentence claiming otherwise outlived the change by long enough to be quoted
//! back as a live work item. `crates/vike-desktop/src/main.rs`'s `state_dir_path` resolved
//! `<project>/settings/state/strategy-state` from the boot's OWN already-resolved state directory,
//! keeping `<exe_dir>/strategy-state` only for a binary with no project above its working
//! directory — the last-resort shape [`crate::paths::tick_store_path`] describes for every other
//! program-written path. (That function went with the desktop's local core; its tombstone stands in
//! the same file.) CONSUMING the boot's answer sidesteps the name collision instead of
//! resolving it, which is why neither a third variable nor a dual read was needed.
//!
//! ⚠ What did NOT ship with it is a MIGRATION — but the case it would serve **cannot currently
//! arise, and this paragraph overstated it on 2026-08-28 by implying it can.** A sidecar written
//! beside an executable would indeed not be moved and would stop being read once a project
//! resolves; no such sidecar exists, because `vike-app` wrote none at ANY rung. That binary set
//! `state_dir` (and `state_save`) on its `CoreConfig` but never set `strategy`, `extra_mounts` or
//! `strategy_factory`, and `crates/vike-core/src/runtime/timers.rs`'s `save_all_strategy_state`
//! iterates the mounts — with none, it writes nothing — and `vike-desktop` builds no `CoreConfig`
//! at all since it lost its local core (#1610). So the migration is unbuilt because it has no
//! subject, which is a stronger reason than the bounded blast radius first claimed here and wants
//! re-checking the day a mount is wired.
//!
//! ⚠ The directory is NOT therefore empty, and assuming so is how the first version of this note
//! went wrong: `crates/vike-ops/src/live_lock.rs`'s `LiveLock::acquire` does a `create_dir_all` on
//! the state dir it is handed. It no longer lands HERE — that call was moved to the boot's own
//! state dir on 2026-08-29, because a GUI locking one directory below the daemon's excluded
//! nothing — but the lesson stands: a directory this module names may hold files this module does
//! not know about.
//!
//! # Dual read
//!
//! Adoption is [`read_path`] + [`write_path`], never a move: a READ prefers the state directory and
//! falls back to the file's `legacy` location when nothing is there yet; a WRITE always lands in the
//! state directory. An existing install therefore keeps working and silently migrates on its first
//! write. The old file is deliberately NOT deleted — a delete that races a rollback loses data.
//!
//! # Purity, and who reads the environment
//!
//! PURE, exactly like its sibling [`crate::paths::store_path`]: the environment arrives as PARAMETERS and
//! is read by the CALLER, per the rule `crates/vike-ops/tests/settings/settings_registry.rs` enforces
//! (libraries take configuration as parameters; only binaries read the process environment).
//!
//! The only filesystem contact is the walk's `is_file`/`is_dir` probes, the existence probe in
//! [`read_path`], and the lazy `create_dir_all` in [`write_path`] — which is the ONE place the state
//! directory is ever created, and which reports failure as an `Err` the caller logs, never a panic.
//! An unwritable settings directory must not stop a program starting; it only means state keeps
//! landing where it landed before.

mod project_dirs;
mod project_root;

pub use project_dirs::{
    bin_dir_beside, imports_dir_beside, project_bin_dir, project_bin_dir_from, project_data_dir,
    project_data_dir_from, project_hist_store_dir, project_hist_store_dir_from, project_log_dir,
    project_log_dir_from, project_state_dir, project_state_dir_from, project_state_dir_from_env,
    project_tmp_dir, project_tmp_dir_from, project_user_data_dir_from, read_path,
    user_data_dir_beside, user_indicators_dir, user_logs_dir, user_plugins_dir,
    user_rhai_strategies_dir, user_rhai_studies_dir, user_runs_dir, user_runs_dir_from,
    user_rust_strategies_dir, user_rust_studies_dir, user_studies_dir, write_path,
};
pub use project_root::{
    deployed_settings_dir, project_settings_dir, project_settings_dir_from, project_user_data_dir,
    workspace_root,
};

/// The settings directory NAME inside a project: `<project>/settings`.
///
/// **Every setting and credential lives here, in the project, in ONE visible directory.** Not the
/// user's home, not the project root, not a dot-directory. Deliberately visible (no leading `.`)
/// because these are files a human is meant to open and edit — unlike `.git/` or `.cargo/`, which
/// are tool-managed.
pub const PROJECT_SETTINGS_DIR: &str = "settings";

/// **THE user-content directory: `<project>/user_data`.** Strategies, run profiles, backtest
/// results, notebooks — everything a HUMAN authors or a run produces on their behalf.
///
/// A SIBLING of [`PROJECT_SETTINGS_DIR`], never a child, and the distinction is ownership rather
/// than taste. `settings/` is machine-owned in both directions — half of it is machine-READ
/// (the settings database, or the credential file on a box that has not migrated) and half
/// machine-WRITTEN (`state/`). A strategy a user
/// wrote is neither: nothing in this workspace may rewrite it, and nothing here is required for the
/// program to start. Three consequences follow from the split, and each is a reason it exists:
///
/// * **Backup and sharing.** `user_data/` is what a user copies between machines or commits to
///   their own repo. `settings/` holds live venue keys (in `settings/db/vike.db`, or in
///   `settings/secrets.env` on a box that has not migrated) and must NOT ride along — which is
///   exactly what happens the moment the two share a parent.
/// * **Lifecycle.** `settings/state/*.json` is disposable; delete it and the program rewrites it.
///   Delete a strategy and the user's work is gone.
/// * **Blast radius.** A directory users are told to drop files into should not sit beside the
///   credential store whose permissions this workspace already warns about.
///
/// The layout under it, and the two rules the loaders depend on:
///
/// ```text
/// <project>/user_data/strategies/rhai/<name>/<name>.rhai   interpreted — works on a binary install
/// <project>/user_data/strategies/rust/<name>/<name>.rs     compiled — SOURCE CHECKOUT ONLY
/// <project>/user_data/research/studies/rhai/<name>/        a STUDY, same two tiers as a strategy
/// <project>/user_data/research/studies/rust/<name>/        compiled — SOURCE CHECKOUT ONLY
/// <project>/user_data/indicators/<name>.rhai               user-written indicators, FLAT
/// <project>/user_data/profiles/*.toml                      backtest / sweep / walkforward
/// <project>/user_data/runs/<id>/                           EVERY run, of every kind — RUNS_SUBDIR
/// <project>/user_data/marks/<name>.json                    a MARK: a stable name for one run —
///                                                          MARKS_SUBDIR, a SIBLING of runs/
/// <project>/user_data/backtest_results/                    SUPERSEDED by runs/ — nothing writes it
/// <project>/user_data/notebooks/
/// <project>/user_data/logs/compile.log                     every strategy load, pass and fail
/// ```
///
/// 1. The entry file matches its folder name, so a folder holding two scripts is never ambiguous.
/// 2. Any `.toml` beside the entry file is a PRESET for that strategy — a preset belongs to its
///    strategy, so `rm -r <name>/` removes the whole thing and two strategies' presets cannot be
///    confused. An optional `presets/` sub-directory is honoured too, for the user whose sweep left
///    forty of them.
///
/// ⚠ **`market_data/` is deliberately NOT here**, though Freqtrade's `user_data/` includes it. The hist
/// store is measured in hundreds of gigabytes; filing it under a directory users are told to copy
/// and commit would be wrong in both directions. It is a THIRD sibling instead —
/// [`PROJECT_DATA_DIR`], found by this same walk — so it still lands inside the project, without
/// riding along in the copy a user makes of their own work.
///
/// `indicators/` holds user-WRITTEN indicators, one `<name>.rhai` per indicator — flat, not a
/// folder each, because an indicator has no presets to keep beside it. Each compiles to a real
/// `vike_indicators::Indicator` via `vike_script::compile_indicator` and is callable from a
/// strategy as `<name>()`.
///
/// ⚠ It arrived two steps after the rest of this tree, and the shape of that delay is the reason
/// the folder is worth trusting now. It was first withheld because only three indicators existed
/// to call; then, when the registry binding made ~140 callable, it stayed absent for a second and
/// better reason — nothing gave a user-written indicator the fed-once-per-bar streaming state the
/// built-ins get, and an indicator called inside an `if` silently skips bars and quietly stops
/// being an average of the last N. Shipping the folder with an example that did that would have
/// been worse than shipping nothing. `vike_script::RhaiIndicator` is that seam, and the folder
/// arrived with it.
pub const PROJECT_USER_DATA_DIR: &str = "user_data";

/// The variable that names the user-content directory OUTRIGHT, skipping the walk:
/// `VIKE_USER_DATA_DIR`.
///
/// The twin of [`SETTINGS_DIR_ENV`], and deliberately a SEPARATE variable rather than a subdirectory
/// of it: an operator relocating settings for a deployment is answering a different question from a
/// user pointing the app at a strategy library on another disk. One variable for both would force
/// them to move together.
pub const USER_DATA_DIR_ENV: &str = "VIKE_USER_DATA_DIR";

/// **THE recorded-data directory: `<project>/market_data`** — one folder, inside the project,
/// holding everything the program STORES. It already holds THREE stores, each a sibling INSIDE this
/// folder rather than a second top-level name: `market_data/hist` ([`HIST_SUBDIR`], the historical
/// store the backfill collectors and the harness populate), `market_data/ticks`
/// ([`crate::paths::tick_store_path::TICKS_SUBDIR`], the live tape a recorder appends to while a daemon
/// trades) and a checkout's `market_data/bench_hist`. A fourth becomes a sibling the same way.
///
/// ⚠ The name says `market_data`, and the hist store is slightly wider than that: alongside the
/// venue-sourced kinds (`bar`/`quote`/`trade`/`book`/`depth`/`funding`/`chain`) it also holds
/// `equity`, `exec_fill` and `exec_order` — this account's own history. The name was chosen for the
/// pairing with [`PROJECT_USER_DATA_DIR`] (what the USER wrote, vs what was RECORDED), which is the
/// distinction an operator needs at the folder level; `vike_data::store::store_kind::STORE_KINDS` is the
/// authority on what a kind actually is.
///
/// The owner's decision, and the shape three peers already ship: Freqtrade (`user_data/data/`),
/// OctoBot and Hummingbot all keep everything in ONE directory the user owns — one folder you can
/// move to another disk, back up, or delete, with no second location to remember.
///
/// # Why a SIBLING of `settings/`, and not a directory under it
///
/// The same ownership split that put [`PROJECT_USER_DATA_DIR`] beside `settings/` rather than
/// inside it, argued from the other end. `settings/` is small, hand-edited and copied between
/// machines; this is neither of those things, and each difference is load-bearing:
///
/// * **Size and shape.** `settings/` holds four TOMLs and a credential store — a directory a human
///   opens. A hist store is a `kind=`/`venue=`/`symbol=`/`date=` partition tree; one measured
///   recorder series wrote a Parquet part every few seconds. Putting that inside `settings/` makes
///   the directory a human is meant to browse unbrowsable, and makes "copy my settings" copy a tape.
/// * **Permissions.** `settings/` is installed `-m700` and this workspace WARNS when the credential
///   store is group- or world-readable. A store root that a recorder, a backfill tool and a datahub
///   server all write is a different permissions question and must not inherit that one's answer.
/// * **Which disk.** The whole point of one owned folder is being able to move it. Nesting would
///   force the credential store onto whichever disk the tape needs, or the tape onto whichever disk
///   the credentials are on.
///
/// # Why `market_data/hist` and not `market_data/` itself
///
/// A checkout already resolves its store to `<repo>/market_data/hist`, beside
/// `market_data/bench_hist` — so this folder is ALREADY the container and `hist` already the store,
/// and answering `<project>/market_data/hist` gives a deployment and a checkout ONE path shape
/// instead of two to document. It also leaves the folder
/// able to grow (an export, an archive, a second store) without a store's own partition directories
/// sitting loose in the folder users are pointed at.
pub const PROJECT_DATA_DIR: &str = "market_data";

/// `hist` — the historical store inside [`PROJECT_DATA_DIR`], populated by the backfill collectors
/// and the harness. It is NOT the only thing in that folder: `market_data/ticks`
/// ([`crate::paths::tick_store_path::TICKS_SUBDIR`]) is a live-tape sibling written by a recorder, and a
/// checkout also carries `market_data/bench_hist`. See [`PROJECT_DATA_DIR`] for why each store sits
/// one level down rather than at `market_data/` itself.
pub const HIST_SUBDIR: &str = "hist";

/// `imports` — the ARCHIVE IMPORT root inside [`PROJECT_DATA_DIR`]: the folder a user fills with a
/// vendor archive under their OWN account (`aws s3 sync` into `imports/dukascopy-bi5/EURUSD/`), which
/// the data daemon's `ImportArchive` verb then reads into the [`HIST_SUBDIR`] store beside it.
/// `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §2.3 and
/// `docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md`
/// verdict 2 are the argument; [`imports_dir_beside`] is the one resolver.
///
/// A SIBLING of `hist/` rather than a child of it, because the two have opposite owners: the store
/// is written by the daemon alone, and this folder by the operator's own tools — a store that
/// found a vendor tree inside its partition directories would be reading something nobody wrote.
///
/// ⚠ **No variable and no settings key, deliberately** (the design's Q8). A root that must live on
/// another disk is a symlink or a mount AT this directory, which the daemon resolves once at
/// startup — the [`PROJECT_BIN_DIR`] argument for having no directory-level knob, applied again.
pub const IMPORTS_SUBDIR: &str = "imports";

/// `bin` — **the program binaries this project RUNS but did not compile**, one directory per tool
/// (`bin/lightgbm/`, `bin/jforex/`, `bin/jre/`, `bin/ibkr-gateway/`, `bin/ibkr-cpapi/`, …).
///
/// ⚠ That list is an ILLUSTRATION, not a roster, and is deliberately open-ended: it has grown twice
/// since this constant landed (the two IBKR gateway installs came indoors from `$HOME`, and the
/// socket gateway relocates its own vendored JRE under here rather than sharing the project's).
/// The RULE is what to carry — a program the project runs but did not compile lives at
/// `<project>/bin/<tool>/` — and each tool's own constant is the authority for its directory name
/// (`crates/bridges/dukascopy/src/config.rs`'s `JFOREX_TOOL_DIR` and `JRE_TOOL_DIR`). What holds the deploy-side
/// installs to the same rule is `crates/vike-ops/tests/deploy/deploy_tool_root_gate.rs`, which is also
/// where the measured exceptions are declared.
///
/// The fourth sibling of `settings/`, `user_data/` and `market_data/`, and it exists because the runtime
/// tools this workspace spawns as child processes had no home a DEPLOYED binary could
/// name. Their paths were baked in at compile time — Dukascopy's sidecar jar and JVM both resolved
/// through the build tree, and that adapter's own doc convicted them: a binary built elsewhere
/// "finds neither jar nor JVM and degrades to paper, with nothing in the log but 'bridge jar not
/// found' — the same defect class the credential-store ratchet spent a release retiring for
/// settings". This constant is the settings-shaped answer to the same question, and
/// `crates/bridges/dukascopy/src/config.rs`'s `resolve_dukascopy_tools` is the adapter side of it:
/// both artifacts now resolve from a `<project>/bin` the CALLER passes in.
///
/// # Why a SIBLING, argued the way its three siblings argue it
///
/// * **Ownership.** Machine-owned in both directions, like `settings/state/` and unlike
///   `user_data/`: nobody hand-edits a 9 MB ELF, and nobody wants one in the directory they copy
///   between machines or commit to their own repository.
/// * **Lifecycle.** Re-obtainable from a pin, exactly like a build artifact and unlike a strategy.
///   Deleting `bin/` costs a download; deleting `user_data/` costs somebody's work.
/// * **Platform.** The contents are platform-specific — a Linux ELF, a `.jar`, a per-OS JVM — while
///   every other sibling is portable across boxes. That alone makes them the wrong thing to carry
///   inside a directory whose whole purpose is being copied.
///
/// # Why not `vendor/`, which already exists
///
/// `vendor/` is the BUILD-time home: third-party source this workspace does not compile (the
/// proprietary FXCM SDK, a portable JDK used by `javac`) plus one committed artifact. It is
/// gitignored and lives only in a CHECKOUT — an installed project has no `vendor/`, so a runtime
/// path resolved against it cannot survive installation, which is precisely the defect this
/// constant closes. The split is by WHEN the artifact is needed, not by who wrote it.
///
/// # ⚠ No `VIKE_BIN_DIR` variable, deliberately
///
/// Every tool under here that a shipped binary resolves is already named outright by a variable of
/// its own — `JAVA_HOME` for the JVM and `JFOREX_BRIDGE_JAR` for the jar (both `Layer::Library`
/// rows in `vike_ops::settings::SETTINGS`) — and each of those wins over
/// this rung. A directory-level variable would be a second way to say one thing, and the two would
/// then need a documented precedence between them. This is [`PROJECT_DATA_DIR`]'s argument for
/// having no `VIKE_DATA_DIR`, applied to the same shape: `user_data/` got
/// [`USER_DATA_DIR_ENV`] precisely because it had no such existing override, and the tools under
/// this directory do.
///
/// ⚠ This paragraph named a third — `--lightgbm` for the trainer — and that flag is GONE with the
/// research binary that offered it. `crates/vike-ml/src/cli.rs`'s `LightGbmCli` takes its path as a
/// PARAMETER, so a future driver supplies its own flag; the argument is unchanged either way, since
/// the answer to "why no directory variable" is "the specific consumer's own knob is the right
/// grain", not a count.
pub const PROJECT_BIN_DIR: &str = "bin";

/// `tmp` — **the project's own scratch directory**, where every intermediate a run needs and no run
/// outlives is staged: a ClickHouse export on its way into a Parquet decoder, a downloaded archive
/// hour on its way into the store, a replay core's working journal.
///
/// The FIFTH sibling of `settings/`, `user_data/`, `market_data/` and `bin/`, and it exists because the
/// alternative is the operating system's temp directory — which is about to stop being a place this
/// program can use. The project is moving to ONE container image with the project folder mounted in,
/// and inside an image the system temp directory is not the host's, does not survive a restart, and
/// cannot be mounted alongside the project folder. A production path into it is the same defect
/// class as a path into `$HOME` or into the tree the compiler was run in: it exists on the
/// developer's box, resolves to something else on the operator's, and is invisible everywhere except
/// in production. `crates/vike-ops/tests/hygiene/system_temp_gate.rs` is the ratchet that keeps the class
/// from growing back, and this constant is where it points.
///
/// # Why a SIBLING, argued the way its four siblings argue it
///
/// * **Ownership.** Machine-owned in BOTH directions and in the strongest sense of any sibling:
///   nothing here is hand-edited, nothing here is hand-read, and nothing here is even meant to be
///   seen. `settings/` is installed `-m700` and hand-edited; a stager writing a 4 GB Parquet file
///   into it would make the directory a human is meant to browse unbrowsable, and would put scratch
///   under the permissions this workspace WARNS about for the credential store.
/// * **Lifecycle.** Re-derivable BY DEFINITION — that is the whole definition of scratch, not merely
///   a property of it. Delete `tmp/` mid-run and a run fails; delete it between runs and nothing at
///   all is lost. `user_data/` is the exact opposite (delete a strategy and somebody's work is
///   gone), which is why this must not sit inside the directory a human copies between machines.
/// * **Size and shape.** Potentially large and BURSTY — a single ClickHouse export is measured in
///   gigabytes and lives for seconds. That combination belongs beside `market_data/` in kind, not inside
///   it: a store is a partition tree a reader scans, and a stager's half-written file appearing in
///   it is a correctness problem rather than an untidiness one.
/// * **Which disk.** The same argument [`PROJECT_DATA_DIR`] makes, and the reason this is one
///   folder rather than a directory under another: the whole point of one owned project folder is
///   being able to move it. Nesting scratch under `settings/` would force the credential store onto
///   whichever disk the biggest export needs.
///
/// # ⚠ The retention rule, which is not optional
///
/// The system temp directory has exactly one property this directory does not: something else
/// EMPTIES it. Take that away without replacing it and the leak is unbounded — and that is measured
/// rather than predicted. On 2026-08-23 the CI box held **26,851 leaked scratch directories totalling
/// 215 GB, about 70% of that filesystem**, because a journal segment is `posix_fallocate`d to its
/// full size and nothing ever removed one.
///
/// So this directory ships with its replacement, in [`crate::scratch`]: an OWNED guard that removes
/// what it created (including on the panic path), plus a bounded sweep for the residual no RAII
/// shape can close. Read that module before writing anything here by hand.
///
/// # ⚠ No `VIKE_TMP_DIR` variable, deliberately — and this one was NOT obvious
///
/// [`PROJECT_BIN_DIR`] has no variable because every tool under it already has one of its own.
/// That argument does not transfer: nothing under `tmp/` is named by anything, because scratch is
/// ANONYMOUS by construction — each path is minted fresh and nobody ever names it again. So the
/// question had to be answered on its own terms, and the answer is still no.
///
/// The case FOR one is real and worth stating: a big scratch is exactly the thing an operator wants
/// on another disk, and that is precisely why [`USER_DATA_DIR_ENV`] exists for `user_data/`. Three
/// things defeat it here.
///
/// 1. **The container is the whole point.** This directory exists BECAUSE the project folder is the
///    mounted volume. A variable whose main use is "put scratch somewhere else" is a hatch back out
///    of the mount — the defect wearing a configuration knob.
/// 2. **The disk question already has a better answer.** `user_data/` needed its own variable
///    because a user pointing the app at a strategy library on another disk is asking a genuinely
///    different question from an operator relocating a deployment. Nobody points anything at
///    somebody else's scratch: moving it means moving THIS project's scratch, and `VIKE_SETTINGS_DIR`
///    already moves the whole project including this folder. Splitting scratch off alone re-creates
///    the "one project's configuration paired with another project's tape" disagreement the shared
///    walk exists to prevent.
/// 3. **The specific consumer's own knob is the right grain.** A tool that stages something huge
///    should take a flag for it, the way `JFOREX_BRIDGE_JAR` outranks any directory-level variable
///    for the sidecar jar. One knob on the one tool beats a variable every binary in the tree must
///    then agree about.
///
/// **What would reopen it:** a shipped unit that genuinely cannot put its project folder on a disk
/// big enough for its own scratch — a deployment root on a small system volume with the tape on a
/// separate mount. That is a concrete configuration, not a preference, and if one appears the
/// variable should be added with the same shape [`USER_DATA_DIR_ENV`] has.
pub const PROJECT_TMP_DIR: &str = "tmp";

/// The variable that names the settings directory OUTRIGHT, skipping the walk entirely:
/// `VIKE_SETTINGS_DIR`.
///
/// The escape hatch for every shape the two markers below cannot describe — a deployment that keeps
/// its settings somewhere other than beside the binary, two configurations on one box, an operator
/// who simply wants the answer to be unambiguous and independent of the working directory. Read by
/// [`project_settings_dir_from`]'s CALLER (this module performs no environment read of its own; see
/// the module doc's purity section) and honoured before any filesystem probe.
pub const SETTINGS_DIR_ENV: &str = "VIKE_SETTINGS_DIR";

/// The manifest FILE name, spelled once — both the [`workspace_root`] walk and
/// `nearest_project_marker` probe for it.
const CARGO_MANIFEST: &str = "Cargo.toml";

/// The state sub-directory inside [`PROJECT_SETTINGS_DIR`]: `<project>/settings/state`.
///
/// # ⚠ The name is odd, it was raised, and the owner ruled it STAYS (2026-09-13)
///
/// `state/` is what the process WRITES at runtime — the rolling logs, the change journal, the
/// `LIVE-*.lock` sentinels, `strategy-state/` and the `HALT` switch. `settings/` is what it READS.
/// So "the written things" is nested inside "the read things", which reads backwards, and a fresh
/// pair of eyes lands on that every time. It landed on it again on 2026-09-13; **the verdict was to
/// leave it.** This note exists so the next reader spends a minute here instead of an afternoon
/// proposing the move.
///
/// **The measurement, which is the whole argument.** A rename is one line HERE and then a long tail:
/// 30 uses reach the name through this constant and move for free, but ~15 code sites spell
/// `settings/state` as a literal, ~93 doc and `CLAUDE.md` mentions name it, and — the part that is
/// not text — **28 live directives in the shipped units and the container entrypoint**. Under the
/// SYSTEM manager a `ReadWritePaths=` naming a path that does not exist REFUSES to start the unit
/// (status=226), so renaming the directory without the same-breath edit of nine units stops the
/// daemons. It is therefore a two-step migration (resolve both names, move, then drop the old), not
/// an edit, and step two costs a STOP of the live trading daemon — which holds the live-session lock
/// and is writing the command journal, so it is a different class from restarting a data service.
///
/// **What reopens it: the settings database landing.** `docs/decisions/0054-settings-move-into-one-database.md` moves the read-side out to
/// `<project>/settings/db/vike.db`, and `secrets.env`/`node.env` go with it. After that `state/`
/// holds exactly what the process writes and nothing else, so the CONTENT split becomes honest even
/// though the nesting still reads backwards — and the migration is already touching these paths, so
/// a rename rides along for one stop instead of two. Raise it THEN, with that measurement in hand,
/// or not at all.
///
/// ⚠ Do NOT read this as "the layout is fine". It reads backwards and that is acknowledged. It is
/// "the cure costs a live-daemon stop and the disease costs a minute of confusion, and the moment
/// when the cure is nearly free is already scheduled".
pub const STATE_SUBDIR: &str = "state";

/// `strategies` — the sub-directory of [`PROJECT_USER_DATA_DIR`] holding user-authored strategies,
/// split one level further by language because the two have different LIFECYCLES: a `.rhai` file is
/// read at startup, a `.rs` file must be compiled. Artifact first, language second, so "where are my
/// strategies" has one answer and a third language later adds a sibling rather than a new root.
pub const STRATEGIES_SUBDIR: &str = "strategies";

/// `rhai` — interpreted strategies, loaded at runtime on every install.
pub const RHAI_SUBDIR: &str = "rhai";

/// `rust` — compiled strategies, meaningful only in a source checkout.
pub const RUST_SUBDIR: &str = "rust";

/// `indicators` — user-WRITTEN indicators, one `<name>.rhai` each, FLAT rather than a folder
/// apiece.
///
/// The asymmetry with [`STRATEGIES_SUBDIR`] is deliberate and is about presets, not tidiness: a
/// strategy owns `.toml` presets that must travel with it (which is what makes `rm -r <name>/` a
/// complete removal), while an indicator's only knob is a top-level `let` inside its own file. A
/// folder per indicator would be an empty directory wrapping one file.
pub const INDICATORS_SUBDIR: &str = "indicators";

/// `plugins` — the sub-directory of [`PROJECT_USER_DATA_DIR`] holding BUILT plugin artifacts, one
/// `<name>-<sha256>.so` per build. See [`user_plugins_dir`] for why the built artifacts sit beside
/// the strategy sources rather than inside them.
///
/// ⚠ The builder service's own out-dir setting defaults to the same two components spelled as one
/// relative literal, because that binary resolves nothing (it runs with
/// `WorkingDirectory=<project>` and takes an explicit override on every real deployment). This
/// constant is the resolved half, for the HOST that has to find the same file after a project
/// walk.
pub const PLUGINS_SUBDIR: &str = "plugins";

/// `runs` — the sub-directory of [`PROJECT_USER_DATA_DIR`] holding EVERY run, of every kind:
/// `<project>/user_data/runs`.
///
/// **ONE directory, because a research run and a strategy backtest are the same SHAPE.** Each is an
/// id, a manifest saying which config produced it, the result files, and a report. They differ only
/// in the CONTENT of those results — which is a difference between files INSIDE a run, not a reason
/// for two directories. And the workflow this serves runs research and then a strategy on the SAME
/// idea, so splitting the two apart would file the halves of one investigation in two places and
/// make the pairing invisible exactly when it is the thing worth seeing.
///
/// ⚠ **It supersedes `user_data/backtest_results/`**, which [`PROJECT_USER_DATA_DIR`]'s layout block
/// showed until this landed. That directory was VERIFIED dead before this one replaced it: a grep
/// of the whole workspace finds no writer at all — the only code naming it is
/// `crates/vike-cli/src/cmd/init/mod.rs`'s `DIRS`, which CREATES it, plus the two files init drops
/// in (`crates/vike-cli/src/cmd/init/content/readmes.rs`'s `RESULTS_README` and `SAMPLE_RESULT_JSON`) and
/// the shipped notebook that reads that one sample. So an operator was shown a folder, told saved
/// reports land there, and nothing ever put one there. That is the defect class `CLAUDE.md` names
/// for a settings key nothing reads — worse than an unimplemented feature, because it hands the
/// operator positive confirmation of something false.
///
/// ⚠ **At the `user_data/` ROOT, not under a `research/` parent**, and the reason is the same
/// argument read the other way: a strategy backtest is not research. A `research/runs` would make
/// every strategy run either mis-filed or exiled to a second directory, which is precisely the split
/// this constant exists to refuse.
pub const RUNS_SUBDIR: &str = "runs";

/// `marks` — the sub-directory of [`PROJECT_USER_DATA_DIR`] holding the MARKS a
/// `vike-cli backtest tag --as` sets: one file per mark, each naming the run id it points at.
///
/// ⚠ **A SIBLING of [`RUNS_SUBDIR`], never a child, and the reason is mechanical.** A directory
/// under the runs root is a RUN as far as every scan of that tree is concerned
/// (`crates/vike-cli/src/cmd/runs/scan.rs`'s `scan_runs` and
/// `crates/vike-studio-core/src/listing.rs`'s `list_runs` both take a directory holding no manifest
/// to be an UNFINISHED run and render it as a diagnostic row). A marks directory filed under
/// `runs/` would therefore be a permanent "this run never finished writing" warning in every
/// listing, on every box, forever.
///
/// A mark is what makes a gate writable at all: it is the one handle that does not move when you
/// run again (`docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §7.2). A run id
/// moves every time, so a gate written against one passes once and then names a run nobody is
/// comparing to.
///
/// ⚠ There is deliberately no `user_marks_dir` resolver beside [`user_runs_dir_from`]. The one
/// caller that needs it — `crates/vike-cli/src/lib.rs`'s dispatcher — already holds the resolved
/// `user_data` directory and joins this constant to it, exactly as it joins [`RUNS_SUBDIR`]; a
/// second resolver would be a second walk answering the same question.
pub const MARKS_SUBDIR: &str = "marks";

/// `research` — the sub-directory of [`PROJECT_USER_DATA_DIR`] holding the work a user does to
/// FIND an edge, as opposed to the strategies that trade one: `<project>/user_data/research`.
///
/// **A study is not a strategy, and filing it as one would be the mis-classification this constant
/// exists to refuse.** A strategy is a thing that is MOUNTED — it takes bars and emits orders, and
/// `strategies/` is what `crates/vike-studio-core/src/user_strategies/load.rs`'s
/// `load_user_strategies` scans in order to offer a user something they can run against a venue. A
/// study answers a question about data and produces a NUMBER, and nothing about it belongs in a
/// list of things that can be armed. Two directories keep that distinction visible in the one place
/// a user actually looks — their own folder — instead of leaving it to a field inside a file.
///
/// ⚠ **[`RUNS_SUBDIR`] is deliberately NOT under here**, and that argument is stated there rather
/// than repeated: a strategy backtest is a run too, so filing runs under `research/` would exile or
/// mis-file half of them. Research owns the QUESTIONS; `runs/` owns every ANSWER, of every kind.
///
/// There is deliberately no `user_research_dir` resolver beside the study ones below. Nothing scans
/// `research/` as a whole — [`STUDIES_SUBDIR`] is its only content today — and a resolver nothing
/// reads is the defect class this workspace names for a settings key nothing reads: it hands a
/// caller positive confirmation of a directory nobody has agreed on. The day research grows a
/// second child, that child gets its own resolver, exactly as `studies/` has.
pub const RESEARCH_SUBDIR: &str = "research";

/// `studies` — the sub-directory of [`RESEARCH_SUBDIR`] holding user-authored studies, split one
/// level further into the SAME two tiers a strategy is split into:
/// `<project>/user_data/research/studies/{rhai,rust}`.
///
/// **The tiers are [`RHAI_SUBDIR`] and [`RUST_SUBDIR`] themselves, not a second pair spelled the
/// same way.** A study may be written in either language for exactly the reason a strategy may: an
/// interpreted one is read at startup and works on a shipped binary, a compiled one must be built
/// and is therefore meaningful only in a source checkout. That is one lifecycle difference, not
/// two, so it gets one pair of names — and a user who has already learned where their `.rhai`
/// strategies go has, by construction, learned where their `.rhai` studies go.
///
/// Artifact first, language second, for the reason [`STRATEGIES_SUBDIR`] gives: "where are my
/// studies" keeps a single answer and a third language later adds a sibling rather than a new root.
pub const STUDIES_SUBDIR: &str = "studies";

/// The log sub-directory inside [`STATE_SUBDIR`]: `<project>/settings/state/logs` — see
/// [`project_log_dir`].
///
/// A directory of its own rather than loose files in the state root, because a rolling appender
/// writes one file PER DAY PER BINARY and the state root also holds files a human occasionally
/// opens (`alerts.json`, `pace.json`); a fortnight of daily logs from four binaries would bury
/// them.
pub const LOGS_SUBDIR: &str = "logs";

/// `incidents` — the sub-directory inside [`STATE_SUBDIR`] where `crates/vike-mount/src/bin/incident.rs`
/// writes one frozen post-mortem bundle per run: `<project>/settings/state/incidents`.
///
/// A sibling of [`LOGS_SUBDIR`] and beside it deliberately — a bundle is a FROZEN COPY of those
/// logs plus the matching journal slice, so filing it anywhere else would let the evidence and its
/// source resolve to two different projects.
///
/// ⚠ **Not `<project>/tmp`, and the distinction is the whole point of that directory.**
/// [`PROJECT_TMP_DIR`] holds what is re-derivable by definition, is owned by an RAII guard that
/// deletes on drop, and is swept to a bounded newest-N. Every one of those would destroy a bundle:
/// it is written precisely because the thing that produced it is gone. This is state in the sense
/// [this module's own doc](self) means — machine-written, no human edits it, and deleting it changes
/// no behaviour — and it sits under `state/` for that reason rather than because it is disposable.
pub const INCIDENTS_SUBDIR: &str = "incidents";

#[cfg(test)]
mod tests;
