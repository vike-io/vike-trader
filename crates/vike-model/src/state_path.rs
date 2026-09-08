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
//! failure [`crate::store_path`] exists to prevent for stores.
//!
//! ⚠ **`VIKE_STATE_ROOT`, not `VIKE_STATE_DIR`** — that name was already taken, by `vike-app`'s
//! strategy-state SIDECAR directory (the per-mount `strategy_state::write_json_atomic` blobs).
//! Reusing it would have given one variable two meanings: an operator pointing it at the state root
//! would silently dump every strategy sidecar into that root, and an operator setting it for the
//! sidecars would silently relocate the window layout. So both names survive, and both still mean
//! what they meant.
//!
//! ⚠ **The fold this paragraph used to call "the obvious follow-up" HAS LANDED, and it needed no
//! dual read** — the sentence claiming otherwise outlived the change by long enough to be quoted
//! back as a live work item. `crates/vike-app/src/main.rs`'s `state_dir_path` resolves
//! `<project>/settings/state/strategy-state` from the boot's OWN already-resolved state directory,
//! keeping `<exe_dir>/strategy-state` only for a binary with no project above its working
//! directory — the last-resort shape [`crate::tick_store_path`] describes for every other
//! program-written path. CONSUMING the boot's answer sidesteps the name collision instead of
//! resolving it, which is why neither a third variable nor a dual read was needed.
//!
//! ⚠ What did NOT ship with it is a MIGRATION — but the case it would serve **cannot currently
//! arise, and this paragraph overstated it on 2026-08-28 by implying it can.** A sidecar written
//! beside an executable would indeed not be moved and would stop being read once a project
//! resolves; no such sidecar exists, because `vike-app` writes none at ANY rung. That binary sets
//! `state_dir` (and `state_save`) on its `CoreConfig` but never sets `strategy`, `extra_mounts` or
//! `strategy_factory`, and `crates/vike-core/src/runtime/timers.rs`'s `save_all_strategy_state`
//! iterates the mounts — with none, it writes nothing. So the migration is unbuilt because it has
//! no subject, which is a stronger reason than the bounded blast radius first claimed here and
//! wants re-checking the day a mount is wired.
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
//! PURE, exactly like its sibling [`crate::store_path`]: the environment arrives as PARAMETERS and
//! is read by the CALLER, per the rule `crates/vike-ops/tests/settings_registry.rs` enforces
//! (libraries take configuration as parameters; only binaries read the process environment).
//!
//! The only filesystem contact is the walk's `is_file`/`is_dir` probes, the existence probe in
//! [`read_path`], and the lazy `create_dir_all` in [`write_path`] — which is the ONE place the state
//! directory is ever created, and which reports failure as an `Err` the caller logs, never a panic.
//! An unwritable settings directory must not stop a program starting; it only means state keeps
//! landing where it landed before.

use std::path::{Path, PathBuf};

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
/// (the four TOMLs, the credential store) and half machine-WRITTEN (`state/`). A strategy a user
/// wrote is neither: nothing in this workspace may rewrite it, and nothing here is required for the
/// program to start. Three consequences follow from the split, and each is a reason it exists:
///
/// * **Backup and sharing.** `user_data/` is what a user copies between machines or commits to
///   their own repo. `settings/secrets.env` holds live venue keys and must NOT ride along — which
///   is exactly what happens the moment the two share a parent.
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
/// ([`crate::tick_store_path::TICKS_SUBDIR`], the live tape a recorder appends to while a daemon
/// trades) and a checkout's `market_data/bench_hist`. A fourth becomes a sibling the same way.
///
/// ⚠ The name says `market_data`, and the hist store is slightly wider than that: alongside the
/// venue-sourced kinds (`bar`/`quote`/`trade`/`book`/`depth`/`funding`/`chain`) it also holds
/// `equity`, `exec_fill` and `exec_order` — this account's own history. The name was chosen for the
/// pairing with [`PROJECT_USER_DATA_DIR`] (what the USER wrote, vs what was RECORDED), which is the
/// distinction an operator needs at the folder level; `vike_data::store_kind::STORE_KINDS` is the
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
/// ([`crate::tick_store_path::TICKS_SUBDIR`]) is a live-tape sibling written by a recorder, and a
/// checkout also carries `market_data/bench_hist`. See [`PROJECT_DATA_DIR`] for why each store sits
/// one level down rather than at `market_data/` itself.
pub const HIST_SUBDIR: &str = "hist";

/// `bin` — **the program binaries this project RUNS but did not compile**, one directory per tool
/// (`bin/lightgbm/`, `bin/jforex/`, `bin/jre/`, `bin/ibkr-gateway/`, `bin/ibkr-cpapi/`, …).
///
/// ⚠ That list is an ILLUSTRATION, not a roster, and is deliberately open-ended: it has grown twice
/// since this constant landed (the two IBKR gateway installs came indoors from `$HOME`, and the
/// socket gateway relocates its own vendored JRE under here rather than sharing the project's).
/// The RULE is what to carry — a program the project runs but did not compile lives at
/// `<project>/bin/<tool>/` — and each tool's own constant is the authority for its directory name
/// (`crates/bridges/dukascopy/src/config.rs`'s `JFOREX_TOOL_DIR` and `JRE_TOOL_DIR`). What holds the deploy-side
/// installs to the same rule is `crates/vike-ops/tests/deploy_tool_root_gate.rs`, which is also
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
/// in production. `crates/vike-ops/tests/system_temp_gate.rs` is the ratchet that keeps the class
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

/// **THE settings directory: `<project>/settings`.** The one place every setting, credential and
/// state file lives. Found by walking UP from `start` for the project root.
///
/// This is the ONLY resolver — there is no second one wrapping it and no fallback anywhere else. A
/// location the app silently used instead would be exactly the scattering this replaced.
///
/// ```text
/// <project>/settings/policy.toml   config.toml   preferences.toml   flags.toml
/// <project>/settings/secrets.env                 credentials
/// <project>/settings/state/*.json                program-written (pace.json, runtime state)
/// ```
///
/// The four TOMLs keep their existing names and schemas — only the directory they are read from
/// changes. `state/` is a sub-directory because those files are written by the PROGRAM, and a
/// program rewriting a file a human is editing loses that human's work.
///
/// **Resolved at RUNTIME, deliberately.** The credential path used to be
/// `concat!(env!("CARGO_MANIFEST_DIR"), …)`, baked in at COMPILE time, so a binary built in one
/// checkout read that checkout's file even when run from another — and every additional checkout
/// needed its own copy. Walking up at runtime means one settings directory per project, whichever
/// checkout the binary came from.
///
/// # A project is not always a source checkout — the TWO markers, and which one wins
///
/// The first version of this walk knew exactly one marker, `Cargo.toml`, and therefore knew exactly
/// one shape of project: a source checkout. **A DEPLOYMENT is the other shape, and it broke.** All
/// three shipped units (`deploy/vike-{tradehub,datahub,recorder}.service`) run
/// `WorkingDirectory=<project>`, where the install recipe puts a BINARY and a profile and nothing
/// else — there is no `Cargo.toml` at that root or above it, so the walk returned `None`, and a
/// production daemon loaded no `policy.toml`, no `config.toml` and no credentials: every venue
/// silently on paper, with no error anywhere, because "no settings" and "settings that say nothing"
/// are indistinguishable downstream.
///
/// So there are two markers — but only ONE of them is ever strong evidence, and the rule is a
/// precedence over the STRENGTH of the evidence, not over the kind of marker:
///
/// | evidence | means | strength | matched at |
/// |---|---|---|---|
/// | a `Cargo.toml` declaring `[workspace]` | a source checkout's ROOT | **decisive** | the OUTERMOST such |
/// | a `Cargo.toml` that could not be READ | unknown — must not be guessed at | **decisive** | the OUTERMOST manifest |
/// | a `settings/` DIRECTORY | a project, self-describing | weak | the NEAREST |
/// | a `Cargo.toml` declaring no workspace | *some* crate — maybe not this one | weak | the NEAREST |
///
/// 1. **A declared `[workspace]` root decides ALONE.** No `settings/` at any depth can move it.
///    This is what keeps #1089 fixed: `cargo test -p vike-aster` runs with the CWD set to the crate
///    directory, that crate directory holds a `settings/` the moment a buggy build writes one there,
///    and the answer must still be the workspace root
///    (`the_walk_reaches_the_workspace_root_not_the_nearest_crate`,
///    `a_settings_dir_inside_a_checkout_never_beats_the_workspace_root`).
/// 2. **An UNREADABLE manifest also decides alone**, on the OUTERMOST manifest — byte-identical to
///    the rule that shipped. An unreadable file is evidence of nothing, and letting a `settings/`
///    answer instead would resolve `crates/bridges/aster/settings` for any checkout whose root
///    manifest happens to be unreadable: #1089 again, and the failure that goes silently to paper.
/// 3. **Otherwise the NEAREST marker of EITHER kind answers.** Every manifest on the chain was read
///    and none claims to be a workspace root, so no manifest is strong evidence of anything; a
///    `settings/` directory is at least evidence about ITSELF. A level holding both yields the same
///    path either way, so the tie needs no rule — see `nearest_project_marker`, where that is
///    structural rather than argued.
/// 4. `None` when neither marker exists anywhere. The caller reports that with the path it wanted;
///    this never invents a second location.
///
/// ⚠ **(3) is the fix for the DEPLOYMENT hijack, and it is narrow on purpose.** A deployment is a
/// `settings/` beside a binary with no `Cargo.toml` at that level; one unrelated `[package]`
/// manifest anywhere above it used to take it, because the manifest arm fell back to the NEAREST
/// manifest — still above the deployment — and the `settings/` arm was never reached at all.
/// Measured on the CI box with real binaries: `no store found — every venue stays paper`, and
/// `max_notional_per_order` back to its unset default, from a deployment whose own `settings/` held
/// both files. With a `settings/` beside the stray, the STRANGER'S ceiling and credentials won
/// instead. The guards are `a_stray_manifest_above_a_deployment_cannot_capture_it` and
/// `a_deployment_under_a_stray_manifest_takes_the_nearest_settings_dir`.
///
/// ⚠ **"A `settings/` beats a workspace-less manifest" is NOT the rule, and the difference is a
/// credential leak.** It must be the NEAREST marker of either kind, because the mirror-image tree
/// exists: a stray `settings/` ABOVE a plain-package project, which under a settings-first rule
/// would capture it — #1101's bug, wearing the other marker
/// (`a_stray_settings_dir_above_a_plain_package_project_cannot_capture_it`).
///
/// ⚠ **The other tempting wrong move: "prefer the nearest ancestor holding BOTH markers".** It
/// reads like the obvious cure for a stray manifest above the project, and it reopens #1089 exactly
/// — the aster crate directory holds both the moment a `settings/` appears under it, which is
/// precisely what the buggy build created there. Those two trees (a member crate under its
/// workspace root; a project under a stranger's manifest) are INDISTINGUISHABLE by marker presence
/// alone, so no marker-counting rule can separate them. Only the manifest's CONTENT can, which is
/// what [`workspace_root`] reads — and (1) above is where that content is spent.
///
/// **The one accepted residual**, pinned by
/// `a_settings_dir_inside_a_checkout_never_beats_the_workspace_root`: a deployment installed INSIDE
/// a checkout (a `settings/` strictly below a declared `[workspace]` root) resolves to the
/// CHECKOUT's settings, because that tree is byte-identical to #1089's. [`SETTINGS_DIR_ENV`] names
/// the directory outright for anyone who genuinely wants that layout.
///
/// A deployment that has not created `<project>/settings/` yet still resolves to `None` — the probe
/// is [`Path::is_dir`], so *creating the directory* is the whole fix, and [`SETTINGS_DIR_ENV`] names
/// it outright for anyone who wants no probe at all.
pub fn project_settings_dir(start: &Path) -> Option<PathBuf> {
    match ManifestChain::walk(start).decisive_root() {
        Some(root) => Some(root.join(PROJECT_SETTINGS_DIR)),
        None => nearest_project_marker(start),
    }
}

/// The NEAREST project marker at or above `start`, of EITHER kind — the answer whenever the
/// manifest chain is not decisive (see [`project_settings_dir`]'s rule 3).
///
/// Both markers resolve to the SAME expression, `<level>/settings`: a `settings/` directory is
/// itself the answer, and a `Cargo.toml` names the directory the answer sits in. So a level holding
/// both needs no tie-break — it cannot produce two answers. That is why this is one interleaved
/// walk and not two passes with a precedence bolted on top.
///
/// The walk can only ever NARROW relative to the manifest chain's own nearest answer: a `settings/`
/// can win only by being nearer than every manifest. It can never escape upward past one, which is
/// what keeps #1101's hijack fixed while curing the deployment shape.
fn nearest_project_marker(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let settings = d.join(PROJECT_SETTINGS_DIR);
        if settings.is_dir() || d.join(CARGO_MANIFEST).is_file() {
            return Some(settings);
        }
        dir = d.parent();
    }
    None
}

/// [`project_settings_dir`] with [`SETTINGS_DIR_ENV`]'s value, which WINS over the whole walk.
///
/// The override arrives as a parameter and the CALLER reads it, per the module doc's purity
/// section: a binary that already swept `std::env::vars()` passes `vars.get(SETTINGS_DIR_ENV)`, and
/// this module still touches no environment. A blank or whitespace-only value is ignored rather than
/// honoured — an empty `Environment=VIKE_SETTINGS_DIR=` line would otherwise resolve settings to
/// `""` and read them out of the working directory, the same class of bug
/// [`crate::store_path`]'s blank-value guard exists for.
pub fn project_settings_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    match override_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => project_settings_dir(start),
    }
}

/// **THE user-content directory: `<project>/user_data`** — see [`PROJECT_USER_DATA_DIR`] for what
/// lives under it and why it is a sibling of `settings/` rather than a child.
///
/// Resolved by the SAME walk as [`project_settings_dir`], deliberately: "which project am I in" must
/// have one answer, or a user could edit a strategy in one project while the app reads another's.
/// Reusing the walk rather than restating it is what keeps that true as the walk's rules change —
/// and they have changed three times, each time to fix a mis-resolution (#1089, #1101, and the
/// deployment shape).
///
/// The probe differs in one way that matters: [`project_settings_dir`] can answer with a `settings/`
/// directory it FOUND, since that directory is itself the marker. `user_data/` is not a marker — a
/// tree with no `user_data/` is the ordinary state of a fresh install, not evidence of a
/// mis-resolution. So this returns the path where it BELONGS, whether or not it exists yet, and the
/// caller decides between "create it" (`vike-cli init`) and "no user content" (a daemon, which has
/// none and needs none).
pub fn project_user_data_dir(start: &Path) -> Option<PathBuf> {
    Some(project_root(start)?.join(PROJECT_USER_DATA_DIR))
}

/// The directory the SIBLING folders hang off — the project root [`project_settings_dir`] resolved,
/// recovered by stripping its trailing `settings` component.
///
/// Going through the settings resolver rather than re-walking is the whole point: one walk, one
/// answer, for `settings/`, `user_data/` and `market_data/` alike. The strip is safe because every path
/// that function returns ends in [`PROJECT_SETTINGS_DIR`] — both its branches join it — and a
/// `parent()` that somehow failed leaves us with `None`, which the caller already handles.
fn project_root(start: &Path) -> Option<PathBuf> {
    project_settings_dir(start)?.parent().map(Path::to_path_buf)
}

/// [`project_root`] under [`SETTINGS_DIR_ENV`]'s value — the project root an operator who set that
/// variable MEANT, recovered as the override's parent.
///
/// The variable names `<project>/settings`, so its parent is `<project>` and every sibling folder
/// hangs off it. Without this, `VIKE_SETTINGS_DIR` would relocate settings, credentials and state
/// while leaving `market_data/` behind on the walk — one project's configuration paired with another
/// project's tape, which is the exact disagreement the shared walk exists to prevent, and which the
/// three shipped units would have hit on the first box whose working directory was not the project.
///
/// ⚠ **A parent that comes back EMPTY is refused** (`None`, so the caller falls through), and the
/// case is real rather than theoretical: `VIKE_SETTINGS_DIR=settings` — a bare relative name, easy
/// to write in a unit file — has `""` for a parent, and joining `market_data/hist` onto that would resolve
/// the store against the WORKING DIRECTORY. That is precisely the CWD-relative default
/// [`crate::store_path`] exists to eliminate, so it must not sneak back in through the override.
///
/// ⚠ **The override is a PROJECT root claim, not merely a settings location.** An operator who
/// points it at a directory that is NOT `<project>/settings` — say `/etc/vike` — gets `/etc/market_data` for
/// the sibling, which is almost certainly not what they wanted. That is stated rather than
/// second-guessed: the alternative is a heuristic on the last path component, and a heuristic here
/// would silently disagree with the very variable it claims to honour. `$VIKE_HIST_STORE` names the
/// store outright and outranks this whole rung for exactly that layout.
fn project_root_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    project_settings_dir_from(override_dir, start)?
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

/// **THE data directory: `<project>/market_data`** — see [`PROJECT_DATA_DIR`] for what lives under it and
/// why it is a sibling of `settings/` rather than a child.
///
/// Resolved by the SAME walk as [`project_settings_dir`] and [`project_user_data_dir`], for the same
/// reason: "which project am I in" must have ONE answer, or a run could write tape into one
/// project while reading another's credentials. The walk's rules have been revised three times
/// (#1089, #1101, the deployment shape) and reusing it rather than restating it is what keeps that
/// true across the next revision.
///
/// Like `user_data/` and unlike `settings/`, this answers with where the directory BELONGS whether
/// or not it exists yet: `market_data/` is NOT a marker. A project with no `market_data/` is a fresh install that
/// has recorded nothing, not evidence of a mis-resolution, and the store creates its own root on
/// first write.
pub fn project_data_dir(start: &Path) -> Option<PathBuf> {
    Some(project_root(start)?.join(PROJECT_DATA_DIR))
}

/// [`project_data_dir`] under [`SETTINGS_DIR_ENV`]'s value — the data twin of
/// [`project_state_dir_from`], so ONE variable relocates a whole project rather than half of one.
///
/// See [`project_root_from`] for the strip, the empty-parent refusal and the one residual.
pub fn project_data_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_root_from(override_dir, start)?.join(PROJECT_DATA_DIR))
}

/// **THE runtime-tool directory: `<project>/bin`** — see [`PROJECT_BIN_DIR`] for what lives under
/// it, why it is a sibling rather than a child, and why it is not `vendor/`.
///
/// Resolved by the SAME walk as every other sibling, for the reason that walk exists: "which
/// project am I in" must have ONE answer. A tool resolved by a second rule could be the one from
/// another project while the credentials came from this one.
///
/// Like `user_data/` and `market_data/` and unlike `settings/`, this answers with where the directory
/// BELONGS whether or not it exists yet: `bin/` is NOT a marker. A project that has never
/// installed a tool is an ordinary fresh install, and the caller's own "is it there" check is what
/// distinguishes that from a mis-resolution.
///
/// ⚠ **The answer is a DIRECTORY, and every tool under it owns a subdirectory rather than sitting
/// loose.** That is not tidiness: `bin/lightgbm/` holds the binary AND the `PROVENANCE` file
/// `vike_ml::cli::LightGbmCli::new` refuses to start without, so a tool is a directory's worth of
/// files even when only one of them is executable.
pub fn project_bin_dir(start: &Path) -> Option<PathBuf> {
    Some(project_root(start)?.join(PROJECT_BIN_DIR))
}

/// [`project_bin_dir`] under [`SETTINGS_DIR_ENV`]'s value — the tool twin of
/// [`project_data_dir_from`], so ONE variable relocates a whole project rather than part of one.
///
/// **This is the form a binary should call.** The bare walk is `$VIKE_SETTINGS_DIR`-BLIND, so a
/// unit whose service file relocates the project would find its settings in one place and its
/// tools in another — the exact split [`project_log_dir_from`] was added to close after a daemon
/// on the CI box wrote its log beside the executable while reading a relocated settings directory.
///
/// See [`project_root_from`] for the strip, the empty-parent refusal and the one residual.
pub fn project_bin_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_root_from(override_dir, start)?.join(PROJECT_BIN_DIR))
}

/// **THE scratch directory: `<project>/tmp`** — see [`PROJECT_TMP_DIR`] for what belongs under it,
/// why it is a sibling rather than a child, why it carries no variable of its own, and — the half
/// that is not optional — why nothing may write here without the ownership and sweep in
/// [`crate::scratch`].
///
/// Resolved by the SAME walk as every other sibling, for the reason that walk exists: "which project
/// am I in" must have ONE answer. Scratch resolved by a second rule could land outside the mounted
/// volume while everything else landed inside it, which is the exact failure this directory replaces.
///
/// Like `user_data/`, `market_data/` and `bin/` and unlike `settings/`, this answers with where the
/// directory BELONGS whether or not it exists yet: `tmp/` is NOT a marker. A project that has never
/// staged anything is an ordinary fresh install, and [`crate::scratch::ScratchDir::create_in`]
/// creates the root on first use.
pub fn project_tmp_dir(start: &Path) -> Option<PathBuf> {
    Some(project_root(start)?.join(PROJECT_TMP_DIR))
}

/// [`project_tmp_dir`] under [`SETTINGS_DIR_ENV`]'s value — the scratch twin of
/// [`project_bin_dir_from`], so ONE variable relocates a whole project rather than part of one.
///
/// **This is the form a binary should call**, and here the bare walk's blindness is more than an
/// inconsistency. `$VIKE_SETTINGS_DIR` is how a deployment says where its project folder is — all
/// three shipped units set it precisely so the answer stops depending on `WorkingDirectory=` — and
/// a unit that relocated its project while its scratch kept resolving off the working directory
/// would stage gigabytes somewhere nobody mounted, backs up, or sweeps.
///
/// See [`project_root_from`] for the strip, the empty-parent refusal and the one residual.
pub fn project_tmp_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_root_from(override_dir, start)?.join(PROJECT_TMP_DIR))
}

/// `<project>/market_data/hist` — the historical-market-data store inside [`project_data_dir`], and the
/// PROJECT rung of [`crate::store_path::resolve_store_root`].
///
/// The caller passes the result down as that function's `project_default` parameter: `store_path`
/// performs no walk and no environment read of its own, so the two modules stay independently
/// testable and the precedence stays a pure function of its arguments. In practice binaries reach
/// this through [`crate::store_path::resolve_store_root_from`], which calls the
/// [`project_hist_store_dir_from`] twin below so the override is never forgotten at a call site.
///
/// ⚠ **No `VIKE_DATA_DIR` variable exists, deliberately.** `$VIKE_HIST_STORE` already names this
/// store outright and already wins over this rung, so a second variable would be a second way to
/// say one thing — and the two would then need a documented precedence between them. `user_data/`
/// got its own [`USER_DATA_DIR_ENV`] precisely because it had no such existing override.
pub fn project_hist_store_dir(start: &Path) -> Option<PathBuf> {
    Some(project_data_dir(start)?.join(HIST_SUBDIR))
}

/// [`project_hist_store_dir`] under [`SETTINGS_DIR_ENV`]'s value — **the form every binary should
/// use**, and the one [`crate::store_path::resolve_store_root_from`] calls.
///
/// `VIKE_SETTINGS_DIR` moves settings, credentials and state; it must move the store's default with
/// them, or an operator who relocated their project would read one project's `secrets.env` while
/// writing another project's tape. That defect shipped in the first cut of this rung and is what
/// this function exists to close.
pub fn project_hist_store_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_data_dir_from(override_dir, start)?.join(HIST_SUBDIR))
}

/// [`project_user_data_dir`] with [`USER_DATA_DIR_ENV`]'s value, which WINS over the whole walk.
///
/// Same contract as [`project_settings_dir_from`]: the CALLER reads the variable and passes the
/// value, so this module still touches no environment, and a blank or whitespace-only value is
/// IGNORED rather than honoured — an empty `VIKE_USER_DATA_DIR=` line would otherwise resolve user
/// content to `""` and scan the working directory for strategies.
pub fn project_user_data_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    match override_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => project_user_data_dir(start),
    }
}

/// [`project_user_data_dir_from`] over an **already-resolved settings directory** instead of a
/// fresh walk — the form a composition root must use, and the reason it exists is a
/// mis-resolution rather than tidiness.
///
/// [`project_user_data_dir_from`]'s own fallback is [`project_user_data_dir`], which WALKS, and that
/// walk is `$VIKE_SETTINGS_DIR`-BLIND. So a root that had already resolved `<project>/settings`
/// through [`project_settings_dir_from`] — honouring the override — then asked for the sibling and
/// silently got a DIFFERENT project's `user_data/` back, whenever the override and the working
/// directory disagreed. Which is the one case the override exists for: all three shipped units set
/// it precisely so the answer stops depending on `WorkingDirectory=`.
///
/// `settings_dir` is that already-resolved `<project>/settings`; the sibling is recovered by
/// stripping the trailing component, exactly as [`project_root_from`] does, with the same
/// empty-parent refusal (a bare relative `VIKE_SETTINGS_DIR=settings` has `""` for a parent, and
/// joining onto that would resolve user content against the WORKING DIRECTORY).
///
/// ⚠ **`override_dir` here is [`USER_DATA_DIR_ENV`], not [`SETTINGS_DIR_ENV`]** — the two are
/// deliberately separate variables, and the settings one has already been spent in producing
/// `settings_dir`. Passing the wrong one relocates user content to the settings directory itself.
///
/// Equal to [`project_user_data_dir_from`] whenever `settings_dir` came from a walk with no
/// settings override — pinned by `the_sibling_agrees_with_the_walk_when_nothing_overrides_it`, so
/// the two spellings cannot drift into two answers for the ordinary case.
pub fn user_data_dir_beside(
    override_dir: Option<&str>,
    settings_dir: Option<&Path>,
) -> Option<PathBuf> {
    match override_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => settings_dir?
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|root| root.join(PROJECT_USER_DATA_DIR)),
    }
}

/// `<project>/user_data/strategies/rhai` — user-authored interpreted strategies.
///
/// Present on every install, including a shipped binary: these are read at startup and need no
/// toolchain.
pub fn user_rhai_strategies_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(STRATEGIES_SUBDIR).join(RHAI_SUBDIR))
}

/// `<project>/user_data/strategies/rust` — locally-authored COMPILED strategies.
///
/// ⚠ **Meaningful only in a source checkout.** These are picked up by a build script and compiled
/// into the binary; on an installed binary the directory is inert, and that is not a defect — a user
/// who installed a release was never going to run `cargo`. It is called out here, in the folder's own
/// README and in `vike-cli init`'s output, because "I dropped a file in and nothing happened" is
/// otherwise unanswerable.
pub fn user_rust_strategies_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(STRATEGIES_SUBDIR).join(RUST_SUBDIR))
}

/// `<project>/user_data/indicators` — user-WRITTEN indicators, resolved by the same walk.
///
/// `None` when no project is found, exactly like its strategy siblings: an absent project is the
/// ordinary unconfigured state, not an error.
pub fn user_indicators_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(INDICATORS_SUBDIR))
}

/// `<project>/user_data/runs` — EVERY run the user has produced, of every kind. See
/// [`RUNS_SUBDIR`] for why there is one such directory rather than one per kind of run.
///
/// Resolved by the SAME walk as [`project_user_data_dir`] and its siblings, so "which project am I
/// in" keeps ONE answer: a run must not land in one project while the strategy that produced it was
/// read from another.
///
/// Like every `user_data/` resolver and unlike [`project_settings_dir`], this answers with where the
/// directory BELONGS whether or not it exists. `user_data/` is not a marker and neither is `runs/`:
/// a project that has run nothing is a fresh install, not a mis-resolution, and creating the
/// directory belongs to whoever writes the first run.
pub fn user_runs_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(RUNS_SUBDIR))
}

/// `<project>/user_data/research/studies` — BOTH study tiers under one root, which is what a
/// listing over the tree needs. See [`STUDIES_SUBDIR`] for why the tiers are the strategy tree's
/// own, and [`RESEARCH_SUBDIR`] for why a study is filed apart from a strategy.
///
/// It exists as its own resolver, unlike its strategy twin, for a reason the strategy loader states
/// against itself: `crates/vike-studio-core/src/user_strategies/load.rs`'s `load_user_strategies`
/// takes the PARENT of the two tiers and its own doc has to describe that argument as the tier
/// resolvers "minus their leaf" — a caller stripping a component off a resolved path is one edit
/// away from stripping the wrong one. `crates/vike-studio-core/src/listing.rs`'s `list_studies`
/// takes this directly.
pub fn user_studies_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR))
}

/// `<project>/user_data/research/studies/rhai` — user-authored interpreted studies.
///
/// Present on every install, including a shipped binary, for the same reason
/// [`user_rhai_strategies_dir`] is: an interpreted artifact needs no toolchain.
pub fn user_rhai_studies_dir(start: &Path) -> Option<PathBuf> {
    Some(user_studies_dir(start)?.join(RHAI_SUBDIR))
}

/// `<project>/user_data/research/studies/rust` — locally-authored COMPILED studies.
///
/// ⚠ **Meaningful only in a source checkout**, exactly as [`user_rust_strategies_dir`] is and for
/// exactly its reason: a `.rs` file is an input to `cargo`, and a user who installed a release was
/// never going to run one. The tier still EXISTS on a binary install, and a listing still shows what
/// is in it — an empty directory is an answer, while a tier that silently disappears on half the
/// installs is the thing that makes "I dropped a file in and nothing happened" unanswerable.
pub fn user_rust_studies_dir(start: &Path) -> Option<PathBuf> {
    Some(user_studies_dir(start)?.join(RUST_SUBDIR))
}

/// `<project>/user_data/logs` — logs belonging to work the USER ran, distinct from the app's own
/// runtime log under [`project_log_dir`].
///
/// The split is by owner, which is how MetaTrader draws it too (platform `Logs/` vs the tester's own
/// tree): a daemon on a server has no `user_data/` at all, while a backtest trace is the user's to
/// read and delete.
///
/// ⚠ **The two files under it have deliberately OPPOSITE defaults.** `compile.log` is always
/// written — it is bounded by the number of strategies rather than by runtime, and a user whose
/// script failed to parse needs one obvious place to look. Per-run backtest traces are OPT-IN,
/// because their size scales with runtime and this workspace has already written 6.4 GB in an hour
/// and 341 GB on one long run. Freqtrade writes no log at all without `--logfile`, and that instinct
/// is right for anything unbounded.
///
/// ⚠ **`$VIKE_LOG_DIR` must NOT redirect this.** That variable moves the APP log; if a user's run
/// history followed it, setting it for a daemon would silently relocate their backtests.
pub fn user_logs_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(LOGS_SUBDIR))
}

/// The project's STATE directory — `<project>/settings/state` — where program-written JSON lives
/// (`pace.json`, workspace state).
///
/// A sub-directory of [`project_settings_dir`], not a sibling, so everything the app owns is under
/// one folder. It is separate from the TOMLs beside it because these files are written by the
/// PROGRAM: a program rewriting a file a human is editing loses that human's work, and keeping the
/// two in one directory invites exactly that.
pub fn project_state_dir(start: &Path) -> Option<PathBuf> {
    Some(project_settings_dir(start)?.join(STATE_SUBDIR))
}

/// [`project_state_dir`] under [`project_settings_dir_from`]'s override — the state twin, so a
/// binary that relocates settings relocates the state under them with the same one variable.
pub fn project_state_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_settings_dir_from(override_dir, start)?.join(STATE_SUBDIR))
}

/// The project's LOG directory — `<project>/settings/state/logs`, [`project_state_dir`] plus
/// [`LOGS_SUBDIR`].
///
/// A rolling trace file is program-written state by this module's own test (delete it and the
/// program writes a new one; nothing about behaviour changes permanently), so it belongs under the
/// state root with everything else the program owns. It had ended up in `<exe_dir>/logs` instead —
/// `target/debug/logs/vike-tradehub.<date>` in a checkout, a directory `cargo clean` deletes, and
/// on a deployment a directory beside the binary an operator has no reason to open.
///
/// A DIRECTORY rather than a file, because `tracing_appender` rolls a file per day per binary
/// inside it (`vike-tradehub.2026-08-05`, `vike-recorder.2026-08-05`, …).
///
/// PURE, like everything here: `start` is the caller's working directory, and the BINARY hands the
/// result to `vike_log::LogConfig::project_dir`. vike-log cannot compute this itself — it is
/// deliberately the bottom layer and depends on no crate at all — and `$VIKE_LOG_DIR` still wins
/// over whatever this returns.
pub fn project_log_dir(start: &Path) -> Option<PathBuf> {
    Some(project_state_dir(start)?.join(LOGS_SUBDIR))
}

/// [`project_log_dir`] under [`project_settings_dir_from`]'s override — the log twin of
/// [`project_state_dir_from`], so ONE variable relocates a whole project rather than most of one.
///
/// It exists because [`project_log_dir`] was the one member of this family with no `_from` sibling,
/// and the daemon that needed it (`vike-recorder`) therefore resolved its rolling log purely from
/// the working directory while its shipped unit set `VIKE_SETTINGS_DIR` and its runbook claimed
/// that made the answer "independent of WorkingDirectory". Measured: with the variable set to one
/// directory and the CWD under no project marker at all, the log landed in `<exe_dir>/logs`.
pub fn project_log_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_state_dir_from(override_dir, start)?.join(LOGS_SUBDIR))
}

/// The WORKSPACE root above `start`: the **OUTERMOST directory whose `Cargo.toml` declares a
/// `[workspace]` table**, else — when no manifest on the chain declares one — the NEAREST directory
/// holding a `Cargo.toml`.
///
/// # Why the content, and not just the file
///
/// ⚠ **Taking the FIRST manifest is a bug, and it shipped.** Every crate has its own `Cargo.toml`,
/// and `cargo test -p vike-aster` runs with the CWD set to the CRATE directory — so a first-match
/// walk resolved `crates/bridges/aster/settings/secrets.env` and the credentials silently vanished
/// (#1089). ⚠ **Taking the OUTERMOST manifest is also a bug, and it shipped too.** One unrelated
/// `Cargo.toml` above the project — a `cargo new` at the wrong level, a parent monorepo, a vendored
/// crate directory — captured it: with no `settings/` beside the stray the project's own populated
/// store vanished, and with one the project silently read the STRANGER'S credentials.
///
/// Both bugs are the same mistake: `Cargo.toml` was treated as the marker when the QUESTION is "is
/// this the workspace root?", and only a `[workspace]` table answers that. It is cargo's own
/// definition, not a heuristic of ours:
///
/// * a MEMBER crate's manifest never carries one, so a member can never be picked — #1089 is fixed
///   by construction rather than by "climb further";
/// * an unrelated `[package]` manifest is not a workspace root, so it cannot capture a project that
///   has one — the hijack, fixed without climbing further at all.
///
/// The result is always the same directory as the previous rule's, or a DESCENDANT of it: the
/// candidates are a subset of the manifest directories, so this walk can only ever narrow, never
/// escape further.
///
/// # The three fallbacks, in order
///
/// 1. Some manifest declares `[workspace]` ⇒ the **OUTERMOST** such directory. Outermost, not
///    nearest, because a genuinely nested workspace (`crates/bridges/ctrader/protogen` carries its
///    own `[workspace]` so the drift-gate codegen stays out of the build) must not become "the
///    project" for a tool run inside it — that is the #1089 failure shape again, one level up.
/// 2. Every manifest was read and NONE declares one ⇒ the **NEAREST** manifest. A plain package
///    cannot own another plain package, so the nearest is the project and the outermost is just the
///    nearest stranger. ⚠ This arm cannot reopen #1089: that bug needs `cargo test -p <crate>`,
///    which needs a workspace, which needs a `[workspace]` table — a chain that has one never
///    reaches here.
/// 3. A manifest could NOT be read ⇒ the **OUTERMOST** manifest, byte-identical to the rule that
///    shipped. An unreadable file is evidence of nothing, and guessing "not a workspace" would mean
///    an unreadable ROOT manifest resolves to the nearest crate — #1089, the worse of the two
///    failures, since it fails silently onto paper.
///
/// ⚠ **This answers "what does the manifest chain point at", which is NOT always "where are this
/// project's settings".** Fallback (2) is a GUESS — the nearest manifest may be a stranger's, and
/// on a deployment it always is — so [`project_settings_dir`] does not consult this function; it
/// takes `ManifestChain::decisive_root` and lets a nearer `settings/` answer otherwise. This
/// function keeps the full three-fallback shape because callers ask it a different question:
/// `crates/vike-tradehub/tests/daemon/policy_ceiling_e2e.rs`'s
/// `a_ceiling_in_a_deployment_refuses_the_same_order_a_checkout_refuses` asserts through it that a
/// deployment has no manifest root at all.
pub fn workspace_root(start: &Path) -> Option<PathBuf> {
    ManifestChain::walk(start).best_effort_root().map(Path::to_path_buf)
}

/// The manifest FILE name, spelled once — both the [`workspace_root`] walk and
/// `nearest_project_marker` probe for it.
const CARGO_MANIFEST: &str = "Cargo.toml";

/// What the chain of `Cargo.toml`s at and above a start directory says, gathered in ONE walk and
/// read by two questions with deliberately different appetites for a guess
/// (`ManifestChain::decisive_root` vs `ManifestChain::best_effort_root`).
struct ManifestChain {
    /// The nearest directory holding a manifest — a GUESS at the project, and the fallback that
    /// hijacks a deployment when the manifest is a stranger's.
    nearest: Option<PathBuf>,
    /// The outermost directory holding a manifest — used only when one could not be read.
    outermost: Option<PathBuf>,
    /// The outermost directory whose manifest declares a `[workspace]` table: cargo's OWN
    /// definition of a workspace root, and the only strong evidence on the chain.
    outermost_workspace: Option<PathBuf>,
    /// Some manifest could not be READ. Deliberately distinct from "declares no workspace".
    unreadable: bool,
}

impl ManifestChain {
    fn walk(start: &Path) -> Self {
        let mut chain =
            Self { nearest: None, outermost: None, outermost_workspace: None, unreadable: false };

        let mut dir = Some(start);
        while let Some(d) = dir {
            let manifest = d.join(CARGO_MANIFEST);
            if manifest.is_file() {
                if chain.nearest.is_none() {
                    chain.nearest = Some(d.to_path_buf());
                }
                chain.outermost = Some(d.to_path_buf());
                match declares_a_workspace(&manifest) {
                    Some(true) => chain.outermost_workspace = Some(d.to_path_buf()),
                    Some(false) => {}
                    None => chain.unreadable = true,
                }
            }
            dir = d.parent();
        }
        chain
    }

    /// The root when the manifest evidence DECIDES ALONE — a declared `[workspace]` table, or a
    /// manifest we could not read and must not second-guess. `None` means the chain is weak
    /// evidence (only plain packages, or no manifest at all) and a nearer `settings/` may answer.
    fn decisive_root(&self) -> Option<&Path> {
        match (&self.outermost_workspace, self.unreadable) {
            (Some(root), _) => Some(root.as_path()),
            (None, true) => self.outermost.as_deref(),
            (None, false) => None,
        }
    }

    /// [`Self::decisive_root`], else the NEAREST manifest — the chain's best guess, including when
    /// that guess is only "some crate lives here".
    fn best_effort_root(&self) -> Option<&Path> {
        self.decisive_root().or(self.nearest.as_deref())
    }
}

/// Does this `Cargo.toml` declare a `[workspace]` table? `None` when the file could not be READ —
/// which is deliberately distinct from `Some(false)`, because [`workspace_root`] resolves the two
/// differently.
///
/// Line-buffered and short-circuiting: the common case reads one block and stops at the header.
fn declares_a_workspace(manifest: &Path) -> Option<bool> {
    use std::io::BufRead;

    let file = std::fs::File::open(manifest).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Some(false),
            // Invalid UTF-8 and I/O errors both land here: we do not know, and must not pretend.
            Err(_) => return None,
            Ok(_) if opens_the_workspace_table(&line) => return Some(true),
            Ok(_) => {}
        }
    }
}

/// Is this line a `[workspace]` / `[workspace.…]` TABLE HEADER?
///
/// Header only, on purpose. A member crate carries `workspace = "../.."` INSIDE its `[package]`
/// table — matching a bare `workspace =` assignment would read that member as a workspace root and
/// hand #1089 straight back. `# [workspace]` is prose, not a table, and does not match either.
///
/// Declared limits, both vanishingly rare in a real manifest and neither able to produce a WIDER
/// answer than the rule that shipped: a quoted header (`["workspace"]`) is not recognised, and
/// `workspace = { members = [...] }` written inline at the top level is not either — both simply
/// fall through to the fallbacks above.
fn opens_the_workspace_table(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix('[') else { return false };
    let Some(rest) = rest.trim_start().strip_prefix("workspace") else { return false };
    let rest = rest.trim_start();
    rest.starts_with(']') || rest.starts_with('.')
}

/// The NEAREST existing `settings/` directory at or above `start` — the DEPLOYMENT marker, ALONE.
///
/// A deployed box is a binary, a profile and a `settings/` directory beside them; it has no
/// `Cargo.toml` anywhere, which is precisely why [`workspace_root`] cannot see it. Unlike that walk
/// this one takes the NEAREST match and returns the directory ITSELF (not its parent): a `settings/`
/// directory is self-describing, so the closest one is the most specific answer, and taking the
/// closest bounds the damage a stray `settings/` higher up can do.
///
/// ⚠ **This is the marker in isolation, not the resolver.** [`project_settings_dir`] does NOT call
/// it: a `settings/` may only answer when it is nearer than every manifest, so the two markers are
/// probed in ONE interleaved walk (`nearest_project_marker`) rather than in two passes. Reaching
/// for this function instead re-creates the bug the interleaving fixed — a stray `settings/` above a
/// plain-package project capturing it.
pub fn deployed_settings_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(PROJECT_SETTINGS_DIR);
        if candidate.is_dir() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// The state sub-directory inside [`PROJECT_SETTINGS_DIR`]: `<project>/settings/state`.
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
/// in (`crates/vike-cli/src/cmd/init/content.rs`'s `RESULTS_README` and `SAMPLE_RESULT_JSON`) and
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

/// `incidents` — the sub-directory inside [`STATE_SUBDIR`] where `crates/vike-run/src/bin/incident.rs`
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

/// The path to READ `name` from: `<state_dir>/<name>` when that file EXISTS, else `legacy`.
///
/// Half one of the dual read. Deliberately an EXISTENCE test on the new file, not on the
/// directory: a state dir that exists because some OTHER file was already migrated must not make
/// this file's own un-migrated `legacy` copy invisible.
pub fn read_path(state_dir: Option<&Path>, name: &str, legacy: PathBuf) -> PathBuf {
    match state_dir {
        Some(dir) => {
            let new = dir.join(name);
            if new.exists() { new } else { legacy }
        }
        None => legacy,
    }
}

/// The path to WRITE `name` to: `<state_dir>/<name>`, creating `<state_dir>` lazily.
///
/// Half two of the dual read — a write ALWAYS lands here, which is what silently migrates an
/// install. `Err` (never a panic) when there is no resolvable state dir, when it cannot be
/// created (a read-only `HOME`, or a plain file sitting where the directory should be), or when
/// the target file is a SYMLINK (below): the caller logs it and keeps writing to the file's legacy
/// path, so an unwritable state root degrades to pre-Phase-2 behaviour instead of losing the save.
///
/// # A symlinked state FILE is refused; a symlinked state DIRECTORY is not
///
/// Every caller takes this path and `fs::write`s it, which TRUNCATES whatever a link points at and
/// replaces its contents with something the program chose. So the refusal is aimed at exactly
/// that: the WRITE TARGET.
///
/// * **The file: refused.** Nothing under `settings/state` has a reason to be redirected
///   file-by-file. These files are program-written and re-derivable by definition (that is the test
///   that put them here rather than beside the TOMLs), their names are constants in the calling
///   crates, and a link at one of them buys an operator nothing they cannot get by relocating the
///   directory. What it DOES buy anyone else is a truncate-and-overwrite primitive aimed wherever
///   they point it.
/// * **The directory: allowed.** Pointing `settings/` or `settings/state` at shared or larger
///   storage is a legitimate setup — the filesystem's spelling of the same intent
///   [`SETTINGS_DIR_ENV`] and `VIKE_STATE_ROOT` serve — and the write still lands inside a
///   directory the operator chose, under that directory's own permissions. Refusing it would break
///   real deployments and prevent nothing: the target would still be a plain file.
///
/// ⚠ **This is a CHECK, not an enforcement.** The path is returned and opened later, so a symlink
/// planted in between is not caught — closing that needs `O_NOFOLLOW` at the caller's own `open`
/// (which is what `crates/bridges/ctrader/src/bin/ctrader_authorize.rs`'s `write_private` does for
/// the file with the worst payload). What this DOES close is the realistic case: a link that is
/// already sitting there when the program starts.
pub fn write_path(state_dir: Option<&Path>, name: &str) -> std::io::Result<PathBuf> {
    let dir = state_dir.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no state directory: no state-root override and no per-user data/home directory is set",
        )
    })?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    // `symlink_metadata`, so the link itself is what is inspected rather than whatever it points
    // at. An `Err` here is "nothing is there yet", the normal first-write case.
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is a symlink: refusing to write program state through it, because the write \
                 would truncate whatever it points at. Remove the link, or relocate the whole \
                 state directory with {SETTINGS_DIR_ENV} (a symlinked DIRECTORY is fine).",
                path.display()
            ),
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private scratch directory under the system temp dir — this crate has no `tempfile`
    /// dev-dependency and Phase 2 adds no dependency, so the three filesystem tests below make
    /// (and remove) their own unique directory.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!("vike-state-path-{tag}-{nanos}"));
            std::fs::create_dir_all(&p).expect("scratch dir");
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `user_data/` is a SIBLING of `settings/`, and the whole design rests on it: a user copies
    /// or commits `user_data/`, and `settings/secrets.env` holds live venue keys that must not ride
    /// along. Nesting it would make that impossible to avoid.
    #[test]
    fn user_data_is_a_sibling_of_settings_never_a_child() {
        let scratch = Scratch::new("ud-sibling");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let settings = project_settings_dir(scratch.path()).unwrap();
        let user_data = project_user_data_dir(scratch.path()).unwrap();

        assert_eq!(settings.parent(), user_data.parent(), "same project root");
        assert!(
            !user_data.starts_with(&settings),
            "user_data ({}) must not live under settings ({})",
            user_data.display(),
            settings.display()
        );
        assert_eq!(user_data.file_name().unwrap(), PROJECT_USER_DATA_DIR);
    }

    /// Both directories must come from ONE walk. If they could disagree, a user would edit a
    /// strategy in one project while the app read another's — and the walk's rules have already
    /// been revised three times, so restating them separately would drift.
    #[test]
    fn user_data_and_settings_resolve_to_the_same_project() {
        let scratch = Scratch::new("ud-same-root");
        // A workspace root two levels above the starting directory: the interesting case, since a
        // naive "look next to me" resolver would answer with the wrong level.
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let deep = scratch.path().join("crates").join("thing");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join(CARGO_MANIFEST), "[package]\nname = \"thing\"\n").unwrap();

        let settings = project_settings_dir(&deep).unwrap();
        let user_data = project_user_data_dir(&deep).unwrap();

        assert_eq!(settings, scratch.path().join(PROJECT_SETTINGS_DIR));
        assert_eq!(user_data, scratch.path().join(PROJECT_USER_DATA_DIR));
    }

    /// Absence is the ordinary state of a fresh install, not a mis-resolution. `settings/` can
    /// answer with a directory it FOUND because that directory is its own marker; `user_data/` is
    /// not a marker, so it answers with where the directory BELONGS and lets the caller decide
    /// between creating it and having no user content at all.
    #[test]
    fn user_data_resolves_even_when_the_directory_does_not_exist() {
        let scratch = Scratch::new("ud-absent");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let user_data = project_user_data_dir(scratch.path()).unwrap();
        assert!(!user_data.exists(), "precondition: nothing created it");
        assert_eq!(user_data, scratch.path().join(PROJECT_USER_DATA_DIR));
    }

    /// The override names the directory outright. A user pointing the app at a strategy library on
    /// another disk is answering a different question from an operator relocating a deployment's
    /// settings, which is why this is its own variable rather than a subdirectory of that one.
    #[test]
    fn the_override_wins_over_the_walk() {
        let scratch = Scratch::new("ud-override");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let elsewhere = scratch.path().join("some").join("other").join("place");
        let got = project_user_data_dir_from(Some(elsewhere.to_str().unwrap()), scratch.path());
        assert_eq!(got.unwrap(), elsewhere);
    }

    /// A blank value configured nothing. Honouring it would resolve user content to `""` and scan
    /// the working directory for strategies — the same class of bug the store-path blank guard
    /// exists for, and an empty `Environment=VIKE_USER_DATA_DIR=` line is easy to leave behind.
    #[test]
    fn a_blank_override_is_ignored_not_honoured() {
        let scratch = Scratch::new("ud-blank");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let walked = project_user_data_dir(scratch.path()).unwrap();

        for blank in ["", "   ", "\t"] {
            assert_eq!(
                project_user_data_dir_from(Some(blank), scratch.path()).unwrap(),
                walked,
                "blank override {blank:?} must fall through to the walk"
            );
        }
    }

    // ---- the SIBLING off an already-resolved settings directory ------------------------------

    /// The ordinary case must be IDENTICAL to the walking twin, or a composition root switching to
    /// the sibling form would silently relocate every user's strategy library.
    #[test]
    fn the_sibling_agrees_with_the_walk_when_nothing_overrides_it() {
        let scratch = Scratch::new("ud-beside-agrees");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let settings = project_settings_dir(scratch.path());

        assert_eq!(
            user_data_dir_beside(None, settings.as_deref()),
            project_user_data_dir_from(None, scratch.path()),
        );
    }

    /// **The whole reason this function exists.** `project_user_data_dir_from`'s fallback WALKS,
    /// and that walk does not honour `$VIKE_SETTINGS_DIR` — so a root that resolved its settings
    /// through the override and then asked for the sibling got a different project back. Here the
    /// working directory sits under one project and the override names another; the sibling must
    /// follow the OVERRIDE, and the walking twin demonstrably does not.
    #[test]
    fn the_sibling_follows_the_settings_override_where_the_walk_cannot() {
        let scratch = Scratch::new("ud-beside-override");
        let walked_project = scratch.path().join("checkout");
        std::fs::create_dir_all(&walked_project).unwrap();
        std::fs::write(walked_project.join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let named_project = scratch.path().join("deployment");
        let named_settings = named_project.join(PROJECT_SETTINGS_DIR);
        std::fs::create_dir_all(&named_settings).unwrap();

        let settings =
            project_settings_dir_from(Some(named_settings.to_str().unwrap()), &walked_project);
        assert_eq!(settings.as_deref(), Some(named_settings.as_path()), "precondition");

        assert_eq!(
            user_data_dir_beside(None, settings.as_deref()).unwrap(),
            named_project.join(PROJECT_USER_DATA_DIR),
            "the sibling hangs off the directory that ACTUALLY answered"
        );
        assert_eq!(
            project_user_data_dir_from(None, &walked_project).unwrap(),
            walked_project.join(PROJECT_USER_DATA_DIR),
            "…and the walking twin still answers with the CWD's project — the defect, pinned"
        );
    }

    /// `VIKE_USER_DATA_DIR` still wins outright, and a blank one still falls through — the same
    /// contract as the walking twin, because a caller swapping one for the other must not have to
    /// re-read the blank-value rule.
    #[test]
    fn the_sibling_keeps_the_user_data_override_and_its_blank_guard() {
        let settings = PathBuf::from("/srv/proj").join(PROJECT_SETTINGS_DIR);
        assert_eq!(
            user_data_dir_beside(Some("/elsewhere/lib"), Some(&settings)).unwrap(),
            PathBuf::from("/elsewhere/lib")
        );
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                user_data_dir_beside(Some(blank), Some(&settings)).unwrap(),
                PathBuf::from("/srv/proj").join(PROJECT_USER_DATA_DIR),
                "a blank {blank:?} must fall through to the sibling, not resolve to \"\""
            );
        }
    }

    /// No settings directory ⇒ no sibling. A root with no project above it has no user content, and
    /// inventing one from the working directory is the CWD-relative default this whole module
    /// exists to eliminate. An EMPTY parent (a bare relative `VIKE_SETTINGS_DIR=settings`) is
    /// refused for the same reason — see `project_root_from`.
    #[test]
    fn the_sibling_refuses_an_absent_or_rootless_settings_dir() {
        assert_eq!(user_data_dir_beside(None, None), None);
        assert_eq!(user_data_dir_beside(None, Some(Path::new(PROJECT_SETTINGS_DIR))), None);
    }

    // ---- the DATA directory: the same walk, the same override --------------------------------

    /// `market_data/` is a SIBLING of `settings/` and of `user_data/`, all three off ONE project root.
    /// The whole change rests on this: one folder to move, back up or delete.
    #[test]
    fn data_is_a_sibling_of_settings_and_user_data_off_one_root() {
        let scratch = Scratch::new("data-sibling");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let settings = project_settings_dir(scratch.path()).unwrap();
        let user_data = project_user_data_dir(scratch.path()).unwrap();
        let data = project_data_dir(scratch.path()).unwrap();

        assert_eq!(settings.parent(), data.parent(), "same project root as settings");
        assert_eq!(user_data.parent(), data.parent(), "same project root as user_data");
        assert!(
            !data.starts_with(&settings),
            "data ({}) must not live under settings ({}) — a tape is not a setting",
            data.display(),
            settings.display()
        );
        assert!(
            !data.starts_with(&user_data),
            "nor under user_data — a tape is not the user's work"
        );
        assert_eq!(data.file_name().unwrap(), PROJECT_DATA_DIR);
    }

    /// The store sits one level DOWN, at `market_data/hist`, so `market_data/` can grow an export or a second
    /// store later without a store's own `kind=`/`venue=` partition directories sitting loose in
    /// the folder users are pointed at.
    #[test]
    fn the_hist_store_is_a_named_subdirectory_of_the_data_dir() {
        let scratch = Scratch::new("data-hist");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let data = project_data_dir(scratch.path()).unwrap();
        let hist = project_hist_store_dir(scratch.path()).unwrap();

        assert_eq!(hist, data.join(HIST_SUBDIR));
        assert_eq!(hist, scratch.path().join(PROJECT_DATA_DIR).join(HIST_SUBDIR));
    }

    /// Like `user_data/` and unlike `settings/`, `market_data/` is NOT a marker: a project that has
    /// recorded nothing yet still resolves, and the store creates its own root on first write.
    /// Probing for it would send every fresh install to the per-user directory on run one and to
    /// the project on run two.
    #[test]
    fn the_data_dir_resolves_even_when_it_does_not_exist() {
        let scratch = Scratch::new("data-absent");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let data = project_data_dir(scratch.path()).unwrap();
        assert!(!data.exists(), "precondition: nothing created it");
        assert_eq!(data, scratch.path().join(PROJECT_DATA_DIR));
    }

    /// `bin/` is a SIBLING of the other three, not a child of any of them — the property
    /// [`PROJECT_BIN_DIR`]'s doc argues from ownership, lifecycle and platform. Asserted against
    /// all three siblings at once, because "sibling" is a claim about the whole set: a later edit
    /// that nested it under `settings/` would still pass a test that only compared it to the root.
    #[test]
    fn the_tool_dir_is_a_sibling_of_settings_user_data_and_data() {
        let scratch = Scratch::new("bin-sibling");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let bin = project_bin_dir(scratch.path()).unwrap();

        assert_eq!(bin, scratch.path().join(PROJECT_BIN_DIR));
        assert_eq!(bin.parent(), project_settings_dir(scratch.path()).unwrap().parent());
        assert_eq!(bin.parent(), project_user_data_dir(scratch.path()).unwrap().parent());
        assert_eq!(bin.parent(), project_data_dir(scratch.path()).unwrap().parent());
    }

    /// Like `user_data/` and `market_data/` and unlike `settings/`, `bin/` is NOT a marker: a project that
    /// has installed no tool yet still resolves. Probing for it would make "no tool installed" and
    /// "wrong project" the same answer — which is the pair the caller most needs told apart, since
    /// one is a fresh install and the other is a mis-resolution.
    #[test]
    fn the_tool_dir_resolves_even_when_it_does_not_exist() {
        let scratch = Scratch::new("bin-absent");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let bin = project_bin_dir(scratch.path()).unwrap();
        assert!(!bin.exists(), "precondition: nothing created it");
        assert_eq!(bin, scratch.path().join(PROJECT_BIN_DIR));
    }

    /// The tool twin of `the_settings_override_moves_the_data_root_with_it`, and the reason the
    /// `_from` form ships in the same commit as the bare one: a unit whose service file relocates
    /// the project must not read its settings from the new place and spawn a tool from the old
    /// walk. That split is not hypothetical — it is what `project_log_dir_from` was added to close
    /// after a the CI box daemon wrote its log beside the executable while reading relocated settings.
    #[test]
    fn the_settings_override_moves_the_tool_dir_with_it() {
        let scratch = Scratch::new("bin-override");
        // A resolvable walk underneath, so this asserts the override BEAT it rather than that the
        // walk found nothing.
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let walked = project_bin_dir(scratch.path()).unwrap();

        let elsewhere = scratch.path().join("relocated");
        let override_dir = elsewhere.join(PROJECT_SETTINGS_DIR);
        let override_str = override_dir.to_str().unwrap();

        let moved = project_bin_dir_from(Some(override_str), scratch.path()).unwrap();
        assert_eq!(moved, elsewhere.join(PROJECT_BIN_DIR), "tools hang off the OVERRIDE's root");
        assert_ne!(moved, walked, "the override must not agree with the walk by accident");

        // ...and a blank value is IGNORED rather than honoured, the same rule every `_from` in this
        // module follows: an empty `Environment=VIKE_SETTINGS_DIR=` line would otherwise resolve
        // the tool directory relative to the working directory.
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                project_bin_dir_from(Some(blank), scratch.path()).unwrap(),
                walked,
                "a blank override must fall through to the walk"
            );
        }
    }

    /// `tmp/` is a SIBLING of the other four, not a child of any of them — the property
    /// [`PROJECT_TMP_DIR`]'s doc argues from ownership, lifecycle, size and disk. Asserted against
    /// all four siblings at once, for the reason the `bin/` twin gives: "sibling" is a claim about
    /// the whole set, and a later edit that nested scratch under `settings/` (installed `-m700` and
    /// hand-edited) or under `market_data/` (where a half-written stage file is a reader's correctness
    /// problem) would still pass a test that only compared it to the root.
    #[test]
    fn the_scratch_dir_is_a_sibling_of_settings_user_data_data_and_bin() {
        let scratch = Scratch::new("tmp-sibling");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let tmp = project_tmp_dir(scratch.path()).unwrap();

        assert_eq!(tmp, scratch.path().join(PROJECT_TMP_DIR));
        assert_eq!(tmp.parent(), project_settings_dir(scratch.path()).unwrap().parent());
        assert_eq!(tmp.parent(), project_user_data_dir(scratch.path()).unwrap().parent());
        assert_eq!(tmp.parent(), project_data_dir(scratch.path()).unwrap().parent());
        assert_eq!(tmp.parent(), project_bin_dir(scratch.path()).unwrap().parent());
    }

    /// Like every sibling except `settings/`, `tmp/` is NOT a marker: a project that has staged
    /// nothing yet still resolves. Probing for it would make "nothing staged yet" and "wrong
    /// project" the same answer, and the first is the ordinary state of every fresh install.
    #[test]
    fn the_scratch_dir_resolves_even_when_it_does_not_exist() {
        let scratch = Scratch::new("tmp-absent");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let tmp = project_tmp_dir(scratch.path()).unwrap();
        assert!(!tmp.exists(), "precondition: nothing created it");
        assert_eq!(tmp, scratch.path().join(PROJECT_TMP_DIR));
    }

    /// The scratch twin of `the_settings_override_moves_the_tool_dir_with_it`. It matters more here
    /// than for any other sibling: `<project>/tmp` exists because the project folder is the MOUNTED
    /// volume, so a unit that relocated its project while scratch kept resolving off the working
    /// directory would stage gigabytes outside the mount — the exact failure the directory replaces,
    /// reintroduced by the walk instead of by the system temp directory.
    #[test]
    fn the_settings_override_moves_the_scratch_dir_with_it() {
        let scratch = Scratch::new("tmp-override");
        // A resolvable walk underneath, so this asserts the override BEAT it rather than that the
        // walk found nothing.
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let walked = project_tmp_dir(scratch.path()).unwrap();

        let elsewhere = scratch.path().join("relocated");
        let override_dir = elsewhere.join(PROJECT_SETTINGS_DIR);
        let override_str = override_dir.to_str().unwrap();

        let moved = project_tmp_dir_from(Some(override_str), scratch.path()).unwrap();
        assert_eq!(moved, elsewhere.join(PROJECT_TMP_DIR), "scratch hangs off the OVERRIDE's root");
        assert_ne!(moved, walked, "the override must not agree with the walk by accident");

        // ...and a blank value is IGNORED rather than honoured, the same rule every `_from` in this
        // module follows.
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                project_tmp_dir_from(Some(blank), scratch.path()).unwrap(),
                walked,
                "a blank override must fall through to the walk"
            );
        }
    }

    /// **B4: `VIKE_SETTINGS_DIR` moves the DATA root too.** An operator who relocates their project
    /// with that one variable must not get their settings from the new place and their tape from
    /// the old walk — that is one project's credentials paired with another project's data.
    #[test]
    fn the_settings_override_moves_the_data_root_with_it() {
        let scratch = Scratch::new("data-override");
        // A real project on the walk, so the assertion is "the override BEAT a resolvable walk"
        // rather than "the walk found nothing".
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let walked = project_hist_store_dir(scratch.path()).unwrap();

        let elsewhere = scratch.path().join("relocated");
        let override_dir = elsewhere.join(PROJECT_SETTINGS_DIR);
        let override_str = override_dir.to_str().unwrap();

        assert_eq!(
            project_data_dir_from(Some(override_str), scratch.path()).unwrap(),
            elsewhere.join(PROJECT_DATA_DIR),
            "data hangs off the OVERRIDE's project root"
        );
        assert_eq!(
            project_hist_store_dir_from(Some(override_str), scratch.path()).unwrap(),
            elsewhere.join(PROJECT_DATA_DIR).join(HIST_SUBDIR)
        );
        assert_ne!(
            project_hist_store_dir_from(Some(override_str), scratch.path()).unwrap(),
            walked,
            "the override must actually move it, not agree with the walk by accident"
        );
        // …and settings landed in that same project, which is the invariant being protected.
        assert_eq!(
            project_settings_dir_from(Some(override_str), scratch.path()).unwrap().parent(),
            project_data_dir_from(Some(override_str), scratch.path()).unwrap().parent(),
            "settings and data must hang off ONE root under the override"
        );
    }

    /// **The LOG twin, and the one that was missing.** `project_log_dir` had no `_from` sibling, so
    /// a daemon whose unit sets `VIKE_SETTINGS_DIR` still resolved its rolling trace file purely
    /// from the working directory — measured on the CI box: variable set, CWD under no project marker,
    /// log written to `<exe_dir>/logs`, the last resort. A blank value still falls through to the
    /// walk, like every other member of this family.
    #[test]
    fn the_settings_override_moves_the_log_directory_with_it() {
        let scratch = Scratch::new("log-override");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let walked = project_log_dir(scratch.path()).unwrap();

        let elsewhere = scratch.path().join("relocated");
        let override_dir = elsewhere.join(PROJECT_SETTINGS_DIR);
        let override_str = override_dir.to_str().unwrap();

        assert_eq!(
            project_log_dir_from(Some(override_str), scratch.path()).unwrap(),
            override_dir.join(STATE_SUBDIR).join(LOGS_SUBDIR),
            "the log hangs off the OVERRIDE's settings/state, beside the state it belongs to"
        );
        assert_ne!(
            project_log_dir_from(Some(override_str), scratch.path()).unwrap(),
            walked,
            "the override must actually move it, not agree with the walk by accident"
        );
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                project_log_dir_from(Some(blank), scratch.path()).unwrap(),
                walked,
                "a blank `{blank:?}` configured nothing"
            );
        }
        assert_eq!(project_log_dir_from(None, scratch.path()).unwrap(), walked);
    }

    /// A blank override configured nothing, exactly as for settings and user content — it falls
    /// through to the walk rather than resolving the store to `""`.
    #[test]
    fn a_blank_settings_override_leaves_the_data_root_on_the_walk() {
        let scratch = Scratch::new("data-blank");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let walked = project_hist_store_dir(scratch.path()).unwrap();

        for blank in ["", "   ", "\t"] {
            assert_eq!(
                project_hist_store_dir_from(Some(blank), scratch.path()).unwrap(),
                walked,
                "blank override {blank:?} must fall through to the walk"
            );
        }
    }

    /// ⚠ **The empty-parent refusal.** `VIKE_SETTINGS_DIR=settings` — a bare relative name, easy to
    /// leave in a unit file — has `""` for a parent, and joining `market_data/hist` onto that resolves the
    /// store against the WORKING DIRECTORY: a store created wherever the shell happened to stand,
    /// which is the exact bug `store_path` exists to eliminate. `None` instead, so the caller falls
    /// through to a rung that names a real place.
    #[test]
    fn a_relative_override_with_no_parent_refuses_rather_than_resolving_against_the_cwd() {
        let scratch = Scratch::new("data-rootless");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        for bare in [PROJECT_SETTINGS_DIR, "vike-settings"] {
            assert_eq!(
                project_data_dir_from(Some(bare), scratch.path()),
                None,
                "a parentless override ({bare:?}) must refuse, never answer with a CWD-relative path"
            );
            assert_eq!(project_hist_store_dir_from(Some(bare), scratch.path()), None);
        }
    }

    /// The two strategy roots differ by LANGUAGE under one artifact folder — `strategies/rhai` and
    /// `strategies/rust`, not `rhai/strategies`. Artifact first keeps "where are my strategies" a
    /// single answer and lets a third language add a sibling instead of a new root.
    #[test]
    fn strategy_roots_are_artifact_major_then_language() {
        let scratch = Scratch::new("ud-strat");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let root = scratch.path().join(PROJECT_USER_DATA_DIR).join(STRATEGIES_SUBDIR);

        assert_eq!(user_rhai_strategies_dir(scratch.path()).unwrap(), root.join(RHAI_SUBDIR));
        assert_eq!(user_rust_strategies_dir(scratch.path()).unwrap(), root.join(RUST_SUBDIR));
    }

    /// Indicators are a SIBLING of `strategies/`, not a third tier under it — and flat, so the
    /// path ends at the directory rather than at a per-indicator folder.
    #[test]
    fn indicators_sit_beside_strategies_and_are_flat() {
        let scratch = Scratch::new("ind-sibling");
        std::fs::write(
            scratch.path().join(CARGO_MANIFEST),
            "[workspace]
",
        )
        .unwrap();
        let ud = scratch.path().join(PROJECT_USER_DATA_DIR);

        assert_eq!(user_indicators_dir(scratch.path()).unwrap(), ud.join(INDICATORS_SUBDIR));
        assert_ne!(
            user_indicators_dir(scratch.path()).unwrap(),
            ud.join(STRATEGIES_SUBDIR).join(INDICATORS_SUBDIR),
            "indicators are not filed under strategies/"
        );
    }

    /// No project above the start path means `None`, matching every sibling resolver: an
    /// unconfigured tree is ordinary, not an error.
    #[test]
    fn no_project_means_no_indicators_dir() {
        let scratch = Scratch::new("ind-none");
        assert!(user_indicators_dir(&scratch.path().join("nowhere")).is_none());
    }

    /// ONE runs directory for every kind of run, and it sits at the `user_data/` ROOT rather than
    /// under a `research/` parent. A strategy backtest is not research, and filing it under one
    /// would hide the pairing this directory exists to keep visible: the same idea carried from a
    /// research run into a strategy run.
    #[test]
    fn runs_sit_at_the_user_data_root_rather_than_under_research() {
        let scratch = Scratch::new("ud-runs-root");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let ud = scratch.path().join(PROJECT_USER_DATA_DIR);

        assert_eq!(user_runs_dir(scratch.path()).unwrap(), ud.join(RUNS_SUBDIR));
        assert_ne!(
            user_runs_dir(scratch.path()).unwrap(),
            ud.join("research").join(RUNS_SUBDIR),
            "a strategy backtest is not research, so runs/ is not filed under it"
        );
    }

    /// The SAME walk as every sibling resolver, answered from two levels down. A second walk could
    /// disagree, and then a run would land in one project while the strategy that produced it was
    /// read from another.
    #[test]
    fn runs_and_settings_resolve_to_the_same_project() {
        let scratch = Scratch::new("ud-runs-same-root");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let deep = scratch.path().join("crates").join("thing");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join(CARGO_MANIFEST), "[package]\nname = \"thing\"\n").unwrap();

        assert_eq!(project_settings_dir(&deep).unwrap(), scratch.path().join(PROJECT_SETTINGS_DIR));
        assert_eq!(
            user_runs_dir(&deep).unwrap(),
            scratch.path().join(PROJECT_USER_DATA_DIR).join(RUNS_SUBDIR)
        );
    }

    /// `user_data/` is no marker and neither is `runs/`: a project that has run nothing is a fresh
    /// install, not a mis-resolution. So the resolver answers with where the directory BELONGS and
    /// leaves creating it to whoever writes the first run.
    #[test]
    fn runs_resolve_even_when_the_directory_does_not_exist() {
        let scratch = Scratch::new("ud-runs-absent");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let runs = user_runs_dir(scratch.path()).unwrap();
        assert!(!runs.exists(), "precondition: nothing created it");
        assert_eq!(runs, scratch.path().join(PROJECT_USER_DATA_DIR).join(RUNS_SUBDIR));
    }

    /// No project above the start path means `None`, matching every sibling resolver: an
    /// unconfigured tree is ordinary, not an error.
    #[test]
    fn no_project_means_no_runs_dir() {
        let scratch = Scratch::new("runs-none");
        assert!(user_runs_dir(&scratch.path().join("nowhere")).is_none());
    }

    /// A study is written in the SAME two tiers a strategy is — `research/studies/rhai` and
    /// `research/studies/rust` — and the tier constants are the strategy tree's own, not a second
    /// pair spelled the same. A study is a strategy-shaped artifact: interpreted work runs on a
    /// binary install, compiled work needs a checkout, and that difference is the same difference.
    #[test]
    fn study_roots_mirror_the_strategy_tiers_exactly() {
        let scratch = Scratch::new("ud-study-tiers");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let studies =
            scratch.path().join(PROJECT_USER_DATA_DIR).join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR);

        assert_eq!(user_studies_dir(scratch.path()).unwrap(), studies);
        assert_eq!(user_rhai_studies_dir(scratch.path()).unwrap(), studies.join(RHAI_SUBDIR));
        assert_eq!(user_rust_studies_dir(scratch.path()).unwrap(), studies.join(RUST_SUBDIR));
    }

    /// Studies are filed under `research/`, and RUNS deliberately are not — [`RUNS_SUBDIR`] carries
    /// that argument. The two live at different depths on purpose, so this pins the pair together:
    /// a later tidy-up that moved runs under `research/` would break exactly this.
    #[test]
    fn studies_are_research_but_runs_are_not() {
        let scratch = Scratch::new("ud-study-vs-runs");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let ud = scratch.path().join(PROJECT_USER_DATA_DIR);

        assert_eq!(
            user_studies_dir(scratch.path()).unwrap(),
            ud.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR)
        );
        assert_eq!(user_runs_dir(scratch.path()).unwrap(), ud.join(RUNS_SUBDIR));
        assert_ne!(
            user_runs_dir(scratch.path()).unwrap(),
            ud.join(RESEARCH_SUBDIR).join(RUNS_SUBDIR),
            "a strategy backtest is not research — runs stay at the user_data root"
        );
    }

    /// The SAME walk as every sibling resolver, answered from two levels down — the property that
    /// keeps "which project am I in" a single answer for a study and for the run it produced.
    #[test]
    fn studies_and_runs_resolve_to_the_same_project() {
        let scratch = Scratch::new("ud-study-same-root");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();
        let deep = scratch.path().join("crates").join("thing");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join(CARGO_MANIFEST), "[package]\nname = \"thing\"\n").unwrap();
        let ud = scratch.path().join(PROJECT_USER_DATA_DIR);

        assert_eq!(
            user_rhai_studies_dir(&deep).unwrap(),
            ud.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR).join(RHAI_SUBDIR)
        );
        assert_eq!(user_runs_dir(&deep).unwrap(), ud.join(RUNS_SUBDIR));
    }

    /// Like every `user_data/` resolver: the answer is where the directory BELONGS, whether or not
    /// it exists. A project that has studied nothing is a fresh install, not a mis-resolution.
    #[test]
    fn studies_resolve_even_when_the_directory_does_not_exist() {
        let scratch = Scratch::new("ud-study-absent");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let studies = user_studies_dir(scratch.path()).unwrap();
        assert!(!studies.exists(), "precondition: nothing created it");
        assert!(!user_rhai_studies_dir(scratch.path()).unwrap().exists());
    }

    /// No project above the start path means `None`, matching every sibling resolver: an
    /// unconfigured tree is ordinary, not an error.
    #[test]
    fn no_project_means_no_studies_dir() {
        let scratch = Scratch::new("study-none");
        let nowhere = scratch.path().join("nowhere");
        assert!(user_studies_dir(&nowhere).is_none());
        assert!(user_rhai_studies_dir(&nowhere).is_none());
        assert!(user_rust_studies_dir(&nowhere).is_none());
    }

    /// User logs are the user's; the app's runtime log stays under `settings/state`. A daemon on a
    /// server has no `user_data/` at all, and a user must be able to delete their backtest traces
    /// without touching anything the program relies on.
    #[test]
    fn user_logs_are_separate_from_the_app_runtime_log() {
        let scratch = Scratch::new("ud-logs");
        std::fs::write(scratch.path().join(CARGO_MANIFEST), "[workspace]\n").unwrap();

        let user = user_logs_dir(scratch.path()).unwrap();
        let app = project_log_dir(scratch.path()).unwrap();

        assert_ne!(user, app);
        assert!(user.starts_with(scratch.path().join(PROJECT_USER_DATA_DIR)));
        assert!(app.starts_with(scratch.path().join(PROJECT_SETTINGS_DIR)));
    }

    /// The dual read: the NEW path wins the moment the file exists there.
    #[test]
    fn read_prefers_the_new_path_when_the_file_exists() {
        let scratch = Scratch::new("read-new");
        let state = scratch.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("thing.json"), "{}").unwrap();
        let legacy = scratch.path().join("legacy").join("thing.json");

        let got = read_path(Some(state.as_path()), "thing.json", legacy);
        assert_eq!(got, state.join("thing.json"));
    }

    /// …and falls back to the OLD path when it does not — including when the state dir EXISTS but
    /// holds only some OTHER already-migrated file.
    #[test]
    fn read_falls_back_to_the_legacy_path() {
        let scratch = Scratch::new("read-old");
        let state = scratch.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("other.json"), "{}").unwrap();
        let legacy_dir = scratch.path().join("legacy");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy = legacy_dir.join("thing.json");
        std::fs::write(&legacy, "{}").unwrap();

        assert_eq!(read_path(Some(state.as_path()), "thing.json", legacy.clone()), legacy);
        assert_eq!(read_path(None, "thing.json", legacy.clone()), legacy, "no state dir ⇒ legacy");
    }

    /// A write always lands on the new path, and creates the directory on the way — lazily, only
    /// when something is actually written.
    #[test]
    fn write_lands_on_the_new_path_and_creates_the_directory() {
        let scratch = Scratch::new("write");
        let state = scratch.path().join("deep").join("state");
        assert!(!state.exists(), "precondition: the directory does not exist yet");

        let p = write_path(Some(state.as_path()), "thing.json").expect("creatable");
        assert_eq!(p, state.join("thing.json"));
        assert!(state.is_dir(), "the state directory is created lazily on first write");
    }

    /// **A state FILE that is a symlink is refused.**
    ///
    /// Everything under `settings/state` is program-written: the caller takes this path and
    /// `fs::write`s it, which TRUNCATES whatever the link points at and replaces it with content
    /// the program chose. Nothing here has a reason to be redirected file-by-file — these files
    /// are re-derivable, their names are constants, and the operator-facing way to relocate them
    /// is `VIKE_STATE_ROOT` / `VIKE_SETTINGS_DIR`, which relocates the whole directory.
    ///
    /// The assertion that matters is the last one: the decoy's CONTENT is intact. A refusal that
    /// still truncated the target would pass an `is_err()` check.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_state_file_is_refused_and_its_target_is_untouched() {
        let scratch = Scratch::new("write-symlink-file");
        let state = scratch.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let decoy = scratch.path().join("important.conf");
        std::fs::write(&decoy, "DO NOT TRUNCATE ME").unwrap();
        std::os::unix::fs::symlink(&decoy, state.join("thing.json")).unwrap();

        let err = write_path(Some(state.as_path()), "thing.json")
            .expect_err("a symlinked state file must be refused, not written through");
        assert!(
            err.to_string().to_lowercase().contains("symlink"),
            "the refusal must say why: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&decoy).unwrap(),
            "DO NOT TRUNCATE ME",
            "the link's target must be untouched"
        );
    }

    /// **…but a symlinked state DIRECTORY is fine.**
    ///
    /// Pointing `settings/` or `settings/state` at shared or larger storage is a legitimate
    /// operator setup — it is the filesystem's spelling of the same intent `VIKE_STATE_ROOT`
    /// serves — and the write still lands inside a directory the operator chose, under that
    /// directory's own permissions. Refusing it would break real deployments to buy nothing:
    /// the danger is the WRITE TARGET, and the target here is a plain file.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_state_directory_is_accepted() {
        let scratch = Scratch::new("write-symlink-dir");
        let real = scratch.path().join("shared-storage");
        std::fs::create_dir_all(&real).unwrap();
        let state = scratch.path().join("state");
        std::os::unix::fs::symlink(&real, &state).unwrap();

        let p = write_path(Some(state.as_path()), "thing.json")
            .expect("a symlinked state DIRECTORY is a legitimate operator setup");
        assert_eq!(p, state.join("thing.json"));
        std::fs::write(&p, "{}").unwrap();
        assert!(real.join("thing.json").is_file(), "the write lands in the linked directory");
    }

    // ---- the settings-directory walk: the two markers ----------------------------------------

    /// **The DEV shape, unchanged.** A source checkout resolves to the OUTERMOST `Cargo.toml`'s
    /// `settings/` from any depth — the #1089 regression guard, restated here because
    /// `state_path.rs` never had one of its own (only `vike-secrets`' copy did) and the two-marker
    /// change is exactly the kind that could quietly reintroduce the nearest-crate answer.
    #[test]
    fn a_source_checkout_resolves_to_the_outermost_cargo_toml() {
        let scratch = Scratch::new("dev-shape");
        let root = scratch.path();
        let crate_dir = root.join("crates").join("bridges").join("aster");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();

        let want = root.join(PROJECT_SETTINGS_DIR);
        for from in [root, &crate_dir, &crate_dir.join("src")] {
            assert_eq!(project_settings_dir(from).as_deref(), Some(want.as_path()));
        }
        assert_eq!(
            project_state_dir(&crate_dir).as_deref(),
            Some(want.join(STATE_SUBDIR).as_path())
        );
    }

    /// …and a `settings/` directory sitting at a CRATE level does not capture it either. This is
    /// the shape the two-marker walk could have broken and the reason `Cargo.toml` outranks the
    /// deployment marker instead of competing with it level by level.
    #[test]
    fn a_settings_dir_inside_a_checkout_never_beats_the_workspace_root() {
        let scratch = Scratch::new("dev-inner-settings");
        let root = scratch.path();
        let crate_dir = root.join("crates").join("bridges").join("aster");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
        // A real `settings/` directory at the crate level — the nearest deployment marker there is.
        std::fs::create_dir_all(crate_dir.join(PROJECT_SETTINGS_DIR)).unwrap();

        assert_eq!(
            project_settings_dir(&crate_dir.join("src")).as_deref(),
            Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
            "the workspace root must still win: a stray settings/ must not capture the project"
        );

        // …including a `settings/` at a level with NO manifest of its own — deployment-SHAPED, but
        // inside a checkout. **This is the one accepted residual, pinned rather than left to
        // drift**: it is byte-identical to the #1089 tree (`crates/bridges/aster/settings/` is also
        // a settings/ below the workspace root with no [workspace] table at that level), so no
        // marker rule can serve both, and a declared workspace root must keep winning. An operator
        // who genuinely deploys inside a checkout names the directory with `VIKE_SETTINGS_DIR`.
        let inside = root.join("deploy").join("vike");
        std::fs::create_dir_all(inside.join(PROJECT_SETTINGS_DIR)).unwrap();
        assert_eq!(
            project_settings_dir(&inside).as_deref(),
            Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
            "a declared [workspace] root decides alone — relaxing that is #1089"
        );
    }

    /// ⚠ **THE HIJACK.** One unrelated `Cargo.toml` a single level above a project used to capture
    /// it, because the walk kept the OUTERMOST manifest unconditionally — a `cargo new` at the
    /// wrong level, a parent monorepo or a vendored crate directory was enough. With no outer
    /// `settings/` the project's own populated store simply vanished; with one, the project
    /// silently read the OTHER tree's credentials.
    ///
    /// Reproduced end-to-end on the CI box before the fix, in exactly this shape: `vike-cli secrets
    /// list` printed `no store found — every venue stays paper` without the outer `settings/`, and
    /// listed the OUTER key and never the inner one with it.
    #[test]
    fn an_unrelated_outer_manifest_cannot_capture_the_project() {
        let scratch = Scratch::new("hijack");
        let outer = scratch.path();
        // The stray: a plain PACKAGE manifest, plus a settings/ to be stolen from.
        std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        std::fs::create_dir_all(outer.join(PROJECT_SETTINGS_DIR)).unwrap();
        // The project: a real cargo WORKSPACE with its own settings/.
        let proj = outer.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        std::fs::create_dir_all(proj.join(PROJECT_SETTINGS_DIR)).unwrap();

        let want = proj.join(PROJECT_SETTINGS_DIR);
        for from in [&proj, &proj.join("src")] {
            assert_eq!(
                project_settings_dir(from).as_deref(),
                Some(want.as_path()),
                "a stranger's manifest above the project must not capture its settings"
            );
        }
        assert_eq!(project_state_dir(&proj).as_deref(), Some(want.join(STATE_SUBDIR).as_path()));
    }

    /// The same escape with **no `[workspace]` table anywhere** — a single-crate project under a
    /// stray manifest. Nothing on the chain claims to be a workspace root, so the NEAREST manifest
    /// is the project and the outermost is just the nearest stranger.
    ///
    /// ⚠ This arm cannot reopen #1089: that bug needs `cargo test -p <crate>`, which needs a
    /// workspace, which needs a `[workspace]` table — and a chain that has one never reaches here.
    #[test]
    fn a_plain_package_project_under_a_stray_manifest_keeps_its_own_settings() {
        let scratch = Scratch::new("hijack-plain");
        let outer = scratch.path();
        std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        let proj = outer.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

        assert_eq!(
            project_settings_dir(&proj.join("src")).as_deref(),
            Some(proj.join(PROJECT_SETTINGS_DIR).as_path()),
        );
    }

    /// A COMMENTED-OUT `[workspace]` is not a workspace table — the marker is a table HEADER, and
    /// `# [workspace]` is prose. Without this the stray above would capture the project again.
    #[test]
    fn a_commented_out_workspace_table_is_not_a_workspace_root() {
        let scratch = Scratch::new("hijack-comment");
        let outer = scratch.path();
        std::fs::write(outer.join("Cargo.toml"), "# [workspace]\n[package]\nname=\"x\"\n").unwrap();
        let proj = outer.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[workspace]\n").unwrap();

        assert_eq!(
            project_settings_dir(&proj).as_deref(),
            Some(proj.join(PROJECT_SETTINGS_DIR).as_path()),
        );
    }

    /// A NESTED workspace resolves to the OUTERMOST one, not the nearest — this repo HAS one
    /// (`crates/bridges/ctrader/protogen` carries its own `[workspace]` table so the drift-gate
    /// codegen stays out of the build), and a tool run from inside it must still find the project's
    /// settings. This is the guard on choosing "outermost `[workspace]`" over "nearest".
    #[test]
    fn a_nested_workspace_still_resolves_to_the_outermost_one() {
        let scratch = Scratch::new("nested-ws");
        let root = scratch.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let inner = root.join("crates").join("ctrader").join("protogen");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("Cargo.toml"), "[package]\nname=\"p\"\n[workspace]\n").unwrap();

        assert_eq!(
            project_settings_dir(&inner).as_deref(),
            Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
            "a nested workspace must not become the project"
        );
    }

    /// An UNREADABLE manifest is evidence of NOTHING, so the walk degrades to the rule that
    /// shipped — the outermost manifest — rather than to a new answer. Guessing the other way
    /// would mean an unreadable ROOT manifest resolves to the nearest crate, which is #1089
    /// (credentials silently vanish, every venue on paper) — the worse of the two failures.
    ///
    /// Invalid UTF-8 is the portable stand-in for "cannot be read"; a mode-000 file is not.
    #[test]
    fn an_unreadable_manifest_degrades_to_the_shipped_outermost_rule() {
        let scratch = Scratch::new("unreadable");
        let outer = scratch.path();
        std::fs::write(outer.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
        let proj = outer.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

        assert_eq!(
            project_settings_dir(&proj).as_deref(),
            Some(outer.join(PROJECT_SETTINGS_DIR).as_path()),
        );
    }

    /// **The DEPLOYMENT shape — the bug this walk was extended for.** A project root holds a binary,
    /// a profile and `settings/`, and NO `Cargo.toml` anywhere above it. Before the second marker
    /// this returned `None` and a production daemon ran with no settings and no credentials.
    #[test]
    fn a_deployment_with_no_cargo_toml_resolves_through_its_settings_dir() {
        let scratch = Scratch::new("deploy-shape");
        let opt_vike = scratch.path().join("opt").join("vike");
        std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
        std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

        let want = opt_vike.join(PROJECT_SETTINGS_DIR);
        // From the unit's WorkingDirectory…
        assert_eq!(project_settings_dir(&opt_vike).as_deref(), Some(want.as_path()));
        // …and from anywhere below it.
        assert_eq!(project_settings_dir(&opt_vike.join("bin")).as_deref(), Some(want.as_path()));
        assert_eq!(
            project_state_dir(&opt_vike).as_deref(),
            Some(want.join(STATE_SUBDIR).as_path())
        );
    }

    /// ⚠ **THE DEPLOYMENT HIJACK — the half #1101 left behind.** A deployment is a `settings/`
    /// directory beside a binary with **no `Cargo.toml` at that level**, and ONE unrelated
    /// `[package]` manifest anywhere above it used to take it: with no `[workspace]` table on the
    /// chain the manifest arm falls back to the NEAREST manifest, which is still above the
    /// deployment, and the `settings/` arm was never reached at all because a manifest existed.
    ///
    /// Reproduced end-to-end on the CI box with a real `vike-cli` before the fix, in exactly this shape:
    /// from a deployment holding `settings/policy.toml` (ceiling 22.0) and `settings/secrets.env`,
    /// `secrets list` printed `no store found — every venue stays paper` and `config show` reported
    /// `policy.max_notional_per_order  -  default` — a daemon with **no ceiling and no
    /// credentials**, silently, because one stranger's manifest sat above the install directory.
    #[test]
    fn a_stray_manifest_above_a_deployment_cannot_capture_it() {
        let scratch = Scratch::new("deploy-hijack");
        let parent = scratch.path();
        std::fs::write(parent.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        let opt_vike = parent.join("opt-vike");
        std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
        std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

        let want = opt_vike.join(PROJECT_SETTINGS_DIR);
        for from in [&opt_vike, &opt_vike.join("bin")] {
            assert_eq!(
                project_settings_dir(from).as_deref(),
                Some(want.as_path()),
                "a stranger's manifest above a deployment must not capture its settings"
            );
        }
        assert_eq!(
            project_state_dir(&opt_vike).as_deref(),
            Some(want.join(STATE_SUBDIR).as_path())
        );
    }

    /// …and when the stranger has a `settings/` of its own, the deployment must still take ITS OWN
    /// — the NEAREST one, per the blast-radius reasoning the deployment marker was chosen for.
    /// Measured on the CI box before the fix: the stranger's 999.0 ceiling won over the deployment's
    /// 22.0, and `secrets list` printed the stranger's key and never the deployment's.
    #[test]
    fn a_deployment_under_a_stray_manifest_takes_the_nearest_settings_dir() {
        let scratch = Scratch::new("deploy-hijack-both");
        let parent = scratch.path();
        std::fs::write(parent.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        std::fs::create_dir_all(parent.join(PROJECT_SETTINGS_DIR)).unwrap();
        let opt_vike = parent.join("opt-vike");
        std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();

        assert_eq!(
            project_settings_dir(&opt_vike).as_deref(),
            Some(opt_vike.join(PROJECT_SETTINGS_DIR).as_path()),
            "the deployment's own settings/ is nearer than the stranger's — it must win"
        );
    }

    /// **The cure must not overreach into #1101's bug.** A stray `settings/` ABOVE a plain-package
    /// project must not capture it: the project's own manifest is NEARER, and it is the nearest
    /// marker OF EITHER KIND that answers — not "any `settings/` outranks any workspace-less
    /// manifest", which would hand back exactly the credential theft #1101 fixed.
    #[test]
    fn a_stray_settings_dir_above_a_plain_package_project_cannot_capture_it() {
        let scratch = Scratch::new("no-overreach");
        let outer = scratch.path();
        std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        std::fs::create_dir_all(outer.join(PROJECT_SETTINGS_DIR)).unwrap();
        let proj = outer.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();
        // …and the project has NOT created its settings/ yet: the stray's is the only one that
        // EXISTS, which is precisely when a settings-first rule would reach for it.

        let want = proj.join(PROJECT_SETTINGS_DIR);
        for from in [&proj, &proj.join("src")] {
            assert_eq!(
                project_settings_dir(from).as_deref(),
                Some(want.as_path()),
                "the project's own manifest is nearer than the stray settings/ — it must win"
            );
        }
    }

    /// **An UNREADABLE manifest still decides ALONE**, so a crate-level `settings/` cannot answer
    /// for a checkout whose root manifest could not be read. This is the guard on NOT applying the
    /// nearest-marker rule to every case: the aster crate directory holds BOTH markers, so a walk
    /// that let `settings/` compete here would resolve `crates/bridges/aster/settings` — #1089
    /// exactly, credentials silently gone and every venue on paper.
    #[test]
    fn an_unreadable_root_manifest_still_outranks_a_crate_level_settings_dir() {
        let scratch = Scratch::new("unreadable-vs-settings");
        let root = scratch.path();
        std::fs::write(root.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
        let crate_dir = root.join("crates").join("bridges").join("aster");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
        std::fs::create_dir_all(crate_dir.join(PROJECT_SETTINGS_DIR)).unwrap();

        assert_eq!(
            project_settings_dir(&crate_dir.join("src")).as_deref(),
            Some(root.join(PROJECT_SETTINGS_DIR).as_path()),
            "an unreadable manifest is evidence of nothing — it must not let a crate-level \
             settings/ answer, which is #1089"
        );
    }

    /// The deployment marker is the NEAREST one, so a stray `settings/` higher up loses to the
    /// deployment's own.
    #[test]
    fn the_deployment_marker_takes_the_nearest_settings_dir() {
        let scratch = Scratch::new("deploy-nearest");
        let stray = scratch.path().join("srv");
        let opt_vike = stray.join("opt").join("vike");
        std::fs::create_dir_all(opt_vike.join(PROJECT_SETTINGS_DIR)).unwrap();
        std::fs::create_dir_all(stray.join(PROJECT_SETTINGS_DIR)).unwrap();

        assert_eq!(
            project_settings_dir(&opt_vike).as_deref(),
            Some(opt_vike.join(PROJECT_SETTINGS_DIR).as_path()),
        );
    }

    /// A `settings` FILE is not a settings directory — the probe is `is_dir`, not `exists`.
    #[test]
    fn a_settings_file_is_not_the_marker() {
        let scratch = Scratch::new("settings-file");
        let dir = scratch.path().join("deploy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(PROJECT_SETTINGS_DIR), "not a directory").unwrap();
        assert_eq!(project_settings_dir(&dir), None);
    }

    /// Neither marker anywhere ⇒ `None`. Nothing is invented from the CWD, and a deployment that
    /// has not created `settings/` yet is told so by the caller rather than guessed at.
    ///
    /// ⚠ Assumes the system temp directory's own ancestry holds neither marker — the same
    /// assumption `vike-secrets`' sibling test already makes for `Cargo.toml`. A failure here means
    /// a stray `settings/` or `Cargo.toml` was created above `TMPDIR`, not that the walk regressed.
    #[test]
    fn no_marker_anywhere_still_refuses_to_guess() {
        let scratch = Scratch::new("no-marker");
        let deep = scratch.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(project_settings_dir(&deep), None, "no marker above TMPDIR: see the doc note");
        assert_eq!(project_state_dir(&deep), None);
    }

    /// The LOG directory is the state directory plus one named level — never a second walk, never
    /// a second root. Pinned because the whole point of the repoint is that logs stop having a
    /// location of their own (`<exe_dir>/logs`) and share the one the app already owns.
    #[test]
    fn the_log_dir_is_the_state_dir_plus_one_level() {
        let scratch = Scratch::new("logdir");
        let root = scratch.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

        let state = project_state_dir(root).expect("a workspace root resolves a state dir");
        assert_eq!(project_log_dir(root), Some(state.join(LOGS_SUBDIR)));
        assert_eq!(
            project_log_dir(root),
            Some(root.join(PROJECT_SETTINGS_DIR).join(STATE_SUBDIR).join(LOGS_SUBDIR)),
            "spelled out, so a change to any of the three names is visible here"
        );

        // And from DEEPER in the same project it is still the project's one directory, not a
        // CWD-relative one — a daemon started from a sub-directory must not scatter log files.
        let deep = root.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(project_log_dir(&deep), Some(state.join(LOGS_SUBDIR)));
    }

    /// No project above the working directory ⇒ **no answer**, exactly like every other resolver
    /// here. The caller then keeps whatever it used before (for logs, vike-log's `<exe_dir>/logs`)
    /// rather than this inventing a location from the CWD.
    ///
    /// ⚠ Same TMPDIR-ancestry assumption as `no_marker_anywhere_still_refuses_to_guess` above.
    #[test]
    fn no_project_means_no_log_dir_rather_than_a_guess() {
        let scratch = Scratch::new("logdir-none");
        let deep = scratch.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(project_log_dir(&deep), None, "no marker above TMPDIR: see the doc note");
    }

    /// **The override wins over both markers**, and a blank one is ignored rather than resolving
    /// settings to `""`.
    #[test]
    fn the_settings_dir_override_beats_the_walk() {
        let scratch = Scratch::new("override");
        let root = scratch.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let elsewhere = root.join("elsewhere");

        assert_eq!(
            project_settings_dir_from(elsewhere.to_str(), root),
            Some(elsewhere.clone()),
            "an explicit VIKE_SETTINGS_DIR must not be second-guessed by the walk"
        );
        assert_eq!(
            project_state_dir_from(elsewhere.to_str(), root),
            Some(elsewhere.join(STATE_SUBDIR)),
        );
        // …and it need not exist yet: it names a location, it is not a probe.
        assert!(!elsewhere.exists());

        // Blank/whitespace falls through to the walk.
        let want = root.join(PROJECT_SETTINGS_DIR);
        for blank in [None, Some(""), Some("   ")] {
            assert_eq!(project_settings_dir_from(blank, root).as_deref(), Some(want.as_path()));
        }
    }

    /// An unusable state root is an `Err` the caller logs, NEVER a panic: here a plain FILE sits
    /// where the directory should be (the portable stand-in for a read-only `HOME`).
    #[test]
    fn an_uncreatable_state_dir_errors_instead_of_panicking() {
        let scratch = Scratch::new("blocked");
        let blocker = scratch.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();

        let blocked = blocker.join("state");
        assert!(write_path(Some(blocked.as_path()), "thing.json").is_err());
        assert!(write_path(None, "thing.json").is_err(), "no state dir ⇒ Err, not a panic");
    }
}
