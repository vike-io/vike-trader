//! **The startup sequence, owned once.** Five composition roots used to hand-assemble the same
//! ordered steps — refuse a REMOVED environment variable, resolve `<project>/settings`, load the
//! settings, load the credentials, build the log subscriber, then disclose what was resolved — each
//! from its own copy, each with its own paragraph explaining why the order is what it is.
//!
//! # The incident this order is made of
//!
//! On the CI box the project-root walk answered with an unrelated directory. The daemon loaded no policy,
//! no config and NO CREDENTIALS, so every venue silently dropped to paper — with no error, because
//! "there are no settings here" and "the settings here say nothing" are indistinguishable
//! downstream. `crates/vike-config/src/boot.rs` carries that incident and renders the disclosure
//! that answers it; `crates/vike-model/src/state_path.rs`'s `project_settings_dir_from` carries
//! three separate bugs in the walk's own precedence rule.
//!
//! What made it expensive to fix everywhere at once is that the walk happened in five places. This
//! crate is the answer to that: **ONE walk DECIDES**, and every project-relative path a root uses
//! is derived from its answer — the settings load, the state root ([`Booted::state_dir`]: the log
//! home, `alerts.json`, the telegram ledger, the strategy-state sidecars) and the disclosure — so
//! the "two walks, two answers" shape the disclosure exists to make visible cannot be reproduced by
//! the code doing the disclosing. `crates/vike-desktop/src/main.rs` had exactly that bug: its
//! rolling log file hung off a second `project_log_dir(&cwd)` walk, which is
//! `VIKE_SETTINGS_DIR`-blind, so under the override the log file and the block describing it named
//! two different projects.
//!
//! ⚠ **"One walk decides" is the claim, and it is deliberately not "one `is_dir` probe runs".** Two
//! resolutions remain, and neither can DISAGREE with this one, which is the property that matters:
//!
//! * **The credential store.** [`Credentials::LoadWith`] calls the ROOT's own loader, which resolves
//!   `<project>/settings/secrets.env` for itself. That is the price of the loader being the root's
//!   (see the third bullet below) and it is a repeat of the SAME pure resolver over the SAME
//!   `$VIKE_SETTINGS_DIR` and the same working directory, so it is a duplicated cost, never a
//!   second answer. Making it literally one call needs a store reader that takes a resolved
//!   directory; `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` is where that
//!   work is tracked.
//!   ⚠ **"Never a second answer" was FALSE in one state until it was fixed, and the state is worth
//!   remembering because it is what a duplicated law looks like from the inside.** The store's
//!   resolver honours a NAMED directory with no walk (`vike_secrets::dotenv_path_for`'s no-CWD arm);
//!   [`boot`] wrote that law out a second time as `spec.cwd.and_then(..)` and forgot the arm, so
//!   with an unreadable working directory the credentials came from `$VIKE_SETTINGS_DIR` while every
//!   path in [`Booted`] came back `None`. The cure was not vigilance — the two now share ONE
//!   function, `vike_secrets::project_settings_dir_for`, which the store's resolver is written in
//!   terms of, so "the same pure resolver" is structural rather than asserted.
//! * **`vike_bridge_core::halt`'s sentinel default.** `halt_path_from_env` memoizes ONE resolution
//!   per process, deep inside a library with no boot in reach, so the project reaches it as a
//!   PARAMETER: a root calls `vike_bridge_core::halt::declare_project_state_dir` with
//!   [`Booted::state_dir`] and the sentinel stops walking. It walked before, with the
//!   `$VIKE_SETTINGS_DIR`-BLIND resolver — the same defect as the four fixed above, on the kill
//!   switch. ⚠ **FOUR binaries in this workspace construct an `ExecutionClient`, and only the two
//!   that BOOT declare**: `vike-app` and `vike-tradehub` do; `crates/vike-run/src/bin/ibkr_mount.rs`
//!   and `crates/vike-run/src/bin/polymarket_maker_paper.rs` never call [`boot`] at all, so
//!   they have no resolved project to hand over and their sentinel still walks — present tense,
//!   today, not a hazard held in reserve for a sixth booting root. Nothing gates it either way;
//!   `vike_bridge_core::halt::HaltProject::WalkFrom` carries the detail and
//!   `docs/ops/kill-switches.md` carries the operator's answer (`$VIKE_HALT_FILE` outranks the
//!   rung).
//!
//! # The order, and why each step is where it is
//!
//! 1. **[`vike_config::refuse_removed_env`]** — before a window, a feed, a core or a venue mount. An
//!    operator who set `VIKE_MAX_ORDER_NOTIONAL` believes a ceiling is armed; a build that quietly
//!    ignored it would trade UNCAPPED while they believed otherwise, which is worse than either
//!    keeping the variable or refusing to start.
//! 2. **The settings DIRECTORY**, resolved once (see above).
//! 3. **The CREDENTIALS**, through the root's own loader, followed by
//!    [`vike_config::refuse_credential_file_arming`] — `secrets.env` is plaintext and parsed
//!    last-wins, so appending ONE line to it would otherwise be enough to flip a mount onto a live
//!    venue. ⚠ This step precedes the settings LOAD, which is the one place this crate's order
//!    differs from the sequence as usually stated. It is deliberate and it is the pre-existing
//!    behaviour of both trading roots: a tree that both arms real money AND has a broken
//!    `policy.toml` must fail with the ARMING message, which is the more urgent of the two.
//! 4. **[`vike_config::load`]** — a missing file is the permissive default; a file that EXISTS and
//!    is broken is an error, because a deployment that wrote a ceiling and typo'd the key must not
//!    silently run without one.
//! 5. **The project-relative paths** — [`Booted::state_dir`] and [`Booted::log_home`], joined onto
//!    step 2's answer, and then **the log subscriber**, built by the BINARY from those and
//!    [`Booted::settings`]. See the next section.
//! 6. **The disclosure** — [`Booted::identity_line`] then [`Booted::boot_lines`], emitted by the
//!    binary the moment a subscriber exists.
//!
//! ⚠ **Steps 1-4 must precede step 5, and that is the subtle part.** The log DIRECTORY
//! (`config.log_dir`) and both log LEVELS (`preferences.log_level` / `preferences.log_file_level`)
//! are themselves settings, so a subscriber built first could only ever honour the environment.
//! Which is why **this crate logs NOTHING**: everything it would say comes back as DATA
//! ([`Booted::boot_lines`], [`vike_config::Settings::warnings`]) and the binary emits it once there
//! is somewhere for it to go. That discipline is [`vike_config`]'s own, one layer down, and it is
//! also what lets a binary whose STDOUT IS A PROTOCOL (`vike-tradehub`, `vike-datahub`,
//! `vike-cli mcp`) choose the stream.
//!
//! # What this crate deliberately does NOT do
//!
//! * **It does not call `vike_log::init`, and does not depend on `vike-log`.** The doctrine is that
//!   binaries call it and hold the returned guards; returning the guards from here would arguably
//!   satisfy the same contract, but the cost is not arguable. Measured on the tree this crate was
//!   written against, that one edge adds **43 packages** to `vike-cli` — `tracing-subscriber`,
//!   `tracing-appender`, `time`, `regex-automata` and the whole ICU4X `idna` tree — to a crate
//!   whose entire identity is being light, DataFusion-free and on the FAST CI lane. So the root
//!   builds its own `vike_log::LogConfig` and holds its own guards, exactly as it does today; what
//!   is hoisted is the ORDER around it and the log HOME ([`Booted::log_home`]) derived from the one
//!   settings walk. The order is still enforced structurally, by a data dependency: a root cannot
//!   name a level or a log home it has not booted for.
//! * **It reads no environment.** [`BootSpec::env`] is the single `std::env::vars()` sweep the
//!   composition root already owns. Anything under `crates/vike-boot/src/` scores `Layer::Library`
//!   in `crates/vike-ops/tests/settings_registry.rs`, and a library reading global state its caller
//!   can neither see nor override is the exact defect that file's `LIBRARY_PIN` ratchet exists to
//!   stop. Taking the map keeps every row `Layer::Injected` — the shape
//!   `vike_config::refuse_removed_env`, `vike_tradehub_client::auth::from_vars` and
//!   `credentials::load_workspace_secrets_from_env` already use.
//! * **It never opens the credential store.** [`Credentials::LoadWith`] takes the root's OWN loader
//!   as a function, so `vike-cli` — which must not link `vike_bridge_core`'s ureq/tungstenite/rustls
//!   stack — can defer the read entirely, and every root keeps the exact loader (and the exact log
//!   output) it has today. `crates/vike-boot/tests/dependency_floor.rs` is the gate on that.
//!
//! # The roots are NOT identical, and the differences are declared, not flattened
//!
//! Every way a root departs from the full sequence is an enum arm CARRYING ITS REASON
//! ([`RemovedEnv::Ignore`], [`SettingsLoad::Skip`], [`Credentials::Deferred`],
//! [`LogHome::Elsewhere`], [`Disclosure::Skip`]), so a [`BootSpec`] literal reads as that root's
//! declaration of what it does at startup — where today the same facts live in prose comments no
//! gate can see. `vike-cli` runs the refusal for every subcommand but builds no subscriber and
//! defers the credential read to the two order-write surfaces; `vike-datahub` loads no settings at
//! all — and since ruling 10 it is also the RECORDER, which discloses settings it does not itself
//! consume, so those two declarations are now one binary's.
//!
//! # The BOOT ANCHOR — [`journal_boot_settings`], and why it is not part of [`boot`]
//!
//! A root that ENFORCES the ceilings also writes one durable line per start saying what they
//! effectively were, into `vike_model::change_journal`. It is deliberately a SEPARATE call rather
//! than a seventh step of [`boot`], for two reasons that are both about this crate's own contract:
//! [`boot`] runs before `vike_log::init`, so a write failure there would have nowhere to be
//! reported; and the state root the record belongs in is NOT always [`Booted::state_dir`] —
//! `vike-tradehub`'s `$VIKE_STATE_ROOT` relocates its whole state tree, and the anchor must land in
//! the same tree as that root's rolling log, its `alerts.json` and its telegram ledger. So the
//! state directory arrives as a PARAMETER, the way `vike_model::change_journal::ChangeJournal`'s
//! `in_state_dir` takes one and for the same reason.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_config::{Policy, Settings};
use vike_model::change_journal::{Actor, Change, ChangeJournal, ChangeJournalError, Outcome, Proc};

/// What a binary calls itself in its first log line: `<name> <version> (<build identity>)`.
///
/// Both halves come from the ROOT's own `env!("CARGO_PKG_NAME")`/`env!("CARGO_PKG_VERSION")` —
/// those macros expand in the crate they are written in, so this crate cannot supply them and must
/// not try.
#[derive(Debug, Clone, Copy)]
pub struct Identity<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

/// Step 1: whether this root refuses a REMOVED environment variable.
#[derive(Debug, Clone, Copy)]
pub enum RemovedEnv {
    /// Refuse to start, naming the file and key that replace it. Every root that loads settings.
    Refuse,
    /// This root does not refuse, and says why. A reason rather than a bare `false`: turning a
    /// startup refusal off is exactly the change that should have to justify itself in a diff.
    Ignore(&'static str),
}

/// Step 4: whether this root loads `<project>/settings/*.toml` for its own use.
#[derive(Debug, Clone, Copy)]
pub enum SettingsLoad {
    Load,
    /// This root consumes no setting, and says why. [`Booted::settings`] is then the compiled-in
    /// defaults — never a half-loaded tree, so a consumer cannot accidentally read one.
    Skip(&'static str),
}

/// Step 3: the credential map, and the arming refusal that depends on it.
pub enum Credentials<'a> {
    /// Load them HERE, through the ROOT's own loader, then refuse a credential file that ARMS REAL
    /// MONEY ([`vike_config::refuse_credential_file_arming`]).
    ///
    /// A function rather than a value so the OFF path never opens the file, and rather than a
    /// loader of this crate's own so every root keeps the exact resolution — and the exact log
    /// output — it has today. It is called AT MOST ONCE.
    LoadWith(&'a dyn Fn() -> HashMap<String, String>),
    /// This root does not need credentials at boot, and says why. The arming refusal is skipped
    /// with them: it has nothing to inspect, and inventing a read here to run it would open the
    /// file on a path that deliberately does not.
    Deferred(&'static str),
}

/// Step 5's input: where the rolling log file goes by default.
#[derive(Debug, Clone, Copy)]
pub enum LogHome {
    /// `<settings dir>/state/logs`, off the ONE walk this boot performed — the layer BELOW a
    /// `config.log_dir` a human wrote and below `$VIKE_LOG_DIR`, and ABOVE `vike-log`'s
    /// `<exe_dir>/logs` last resort.
    UnderSettings,
    /// This root does not take its log home from the settings walk, and says why — it derives one
    /// of its own (`vike-tradehub`, whose `$VIKE_STATE_ROOT` relocates the whole state tree) or it
    /// builds no subscriber at all (`vike-cli`). [`Booted::log_home`] is then `None`.
    Elsewhere(&'static str),
}

/// Step 6: whether the startup disclosure is rendered.
#[derive(Debug, Clone, Copy)]
pub enum Disclosure {
    /// Render [`vike_config::boot_lines`]. ⚠ It performs a SECOND read of the settings files, by
    /// design (a row's ORIGIN cannot be recovered from a merged [`Settings`]), so it is worth
    /// skipping on a short-lived command-line invocation and worth paying for on a daemon.
    Render,
    /// This root renders no disclosure, and says why.
    Skip(&'static str),
}

/// One composition root's declaration of its own startup.
pub struct BootSpec<'a> {
    /// The single `std::env::vars()` sweep the BINARY owns. This crate reads no environment.
    pub env: &'a HashMap<String, String>,
    /// Where the project WALK starts — the binary's `std::env::current_dir()`.
    ///
    /// ⚠ `None` (an unreadable working directory) disables the WALK, not the resolution: a
    /// `$VIKE_SETTINGS_DIR` in [`BootSpec::env`] NAMES the directory and is still honoured, because
    /// a name needs nowhere to start. `None` on BOTH rungs is what means no project can be found,
    /// and that is the legitimate answer the disclosure states out loud.
    ///
    /// It is a FIELD rather than a read of this crate's own for two reasons: this crate reads no
    /// process state (see the module doc), and a test can then reach the no-working-directory arm
    /// without `std::env::set_current_dir`, which is process-global and would race every other test
    /// in the binary. `vike_secrets::project_settings_dir_for` takes it the same way, for the same
    /// reason.
    pub cwd: Option<&'a Path>,
    pub identity: Identity<'a>,
    pub removed_env: RemovedEnv,
    pub settings: SettingsLoad,
    pub credentials: Credentials<'a>,
    pub log_home: LogHome,
    pub disclosure: Disclosure,
}

/// Everything the sequence resolved — the value a binary destructures and keeps.
pub struct Booted {
    /// The resolved settings, or the compiled-in defaults under [`SettingsLoad::Skip`].
    ///
    /// ⚠ Its `warnings` are the loader's own non-fatal resolutions and are returned, never logged
    /// (this crate runs before a subscriber). The binary emits them — see this module's doc.
    pub settings: Settings,
    /// `<project>/settings` as the ONE resolution answered — `$VIKE_SETTINGS_DIR` if it named one,
    /// else the walk — or `None` when NEITHER rung could. Every other path in this struct hangs off
    /// it.
    ///
    /// ⚠ **`None` does not imply [`Booted::settings_dir_override`] is `None`, and it used to.** The
    /// resolution was `spec.cwd.and_then(..)`, so an unreadable working directory dropped a name
    /// that needed no walk; it no longer does. The two fields still answer different questions —
    /// WHERE, and WHICH RUNG — and a consumer that needs the rung must read the second.
    pub settings_dir: Option<PathBuf>,
    /// The `$VIKE_SETTINGS_DIR` value that was HONOURED — trimmed, and `None` when blank or unset,
    /// which is the resolver's own rule (an empty `Environment=VIKE_SETTINGS_DIR=` line must fall
    /// through to the walk rather than resolve settings to the working directory).
    ///
    /// Returned because two roots need to distinguish "named" from "walked" and neither may read
    /// the environment for itself: `vike-cli config check` fails on a NAMED directory that is not
    /// on disk (set-but-unhonoured) while merely warning on a walked one, and `vike-datahub`
    /// threads the override into its alerting credential read.
    ///
    /// ⚠ **It is the RUNG, not a spare copy of the directory.** `Some` here now implies `Some` in
    /// [`Booted::settings_dir`], holding the same path — a name is honoured whether or not there is
    /// a working directory. It did not before, and #1514 exists because of the gap: `vike-cli
    /// secrets path` took this value as a FALLBACK for a `settings_dir` the boot had dropped. That
    /// fallback is now unreachable through [`boot`] and is kept as the belt against this line
    /// regressing; `crates/vike-cli/src/cmd/secrets.rs`'s `store_path` says so at its own site.
    pub settings_dir_override: Option<String>,
    /// `<settings dir>/state` — the PROGRAM-WRITTEN state root, off the same one walk.
    ///
    /// Returned rather than left to each root to re-derive, because re-deriving it is exactly what
    /// went wrong: `vike-tradehub`'s `state_dir` and `vike-app`'s `state_dir_path` each called
    /// `vike_model::state_path::project_state_dir(&cwd)`, which is `$VIKE_SETTINGS_DIR`-BLIND, so
    /// the log home, `alerts.json`, the telegram at-most-once ledger and the strategy-state
    /// sidecars all hung off a SECOND walk that could answer with a different project from the one
    /// the settings and credentials came from. On the CI box the two agreed only because the working
    /// directory happened to equal the overridden directory.
    ///
    /// ⚠ **A root with its own state-root variable still layers it ON TOP.** `$VIKE_STATE_ROOT`
    /// relocates the whole state tree and outranks this; that read stays in the binary that owns it
    /// (see [`LogHome::Elsewhere`]), and this is the rung below it.
    pub state_dir: Option<PathBuf>,
    /// The credential map under [`Credentials::LoadWith`], `None` under [`Credentials::Deferred`].
    pub credentials: Option<HashMap<String, String>>,
    /// `<settings dir>/state/logs` under [`LogHome::UnderSettings`], else `None`. Goes straight
    /// into `vike_log::LogConfig::project_dir`.
    pub log_home: Option<PathBuf>,
    /// `<name> <version> (<build identity>)` — WHICH BINARY IS THIS, the first line in the log and
    /// the same string `--version` prints, so the answer is readable from the log alone and from
    /// the binary alone and the two cannot disagree.
    pub identity_line: String,
    /// The startup disclosure, or empty under [`Disclosure::Skip`]. Kept SEPARATE from
    /// [`Booted::identity_line`] because two roots interleave other lines between them.
    pub boot_lines: Vec<String>,
}

/// `$VIKE_SETTINGS_DIR`, spelled as a LITERAL rather than through `vike_secrets::SETTINGS_DIR_ENV`.
///
/// `vike_ops::scan`'s map-lookup sweep resolves constants CRATE-wide, so an IMPORTED one would make
/// this read invisible to the settings registry — and a declared read is the point.
/// `crates/vike-bridge-core/src/credentials.rs`'s `load_workspace_secrets_from_env` spells it the
/// same way for the same reason, and `crates/vike-bridge-core/tests/settings_dir_spellings.rs` pins
/// the two resolvers equal.
fn settings_dir_override(env: &HashMap<String, String>) -> Option<String> {
    env.get("VIKE_SETTINGS_DIR").map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// **Run the startup sequence.** Returns the value the binary holds, or the one message it prints
/// to stderr before exiting.
///
/// The error strings are the roots' own, verbatim: `refuse_removed_env`'s operator-facing block,
/// `refuse_credential_file_arming`'s, and `"settings could not be loaded: {e}"` for a settings tree
/// that exists and is broken. A caller prefixes its own binary name, as it does today.
///
/// Logs nothing, prints nothing, and touches no global state — see this module's doc.
pub fn boot(spec: &BootSpec<'_>) -> Result<Booted, String> {
    // 1. A stale RISK CEILING stops the process before it does anything at all.
    if let RemovedEnv::Refuse = spec.removed_env {
        vike_config::refuse_removed_env(spec.env)?;
    }

    // 2. THE settings directory, resolved ONCE for this whole process. `$VIKE_SETTINGS_DIR` names
    //    it outright and wins; otherwise the runtime walk answers — a checkout's WORKSPACE ROOT
    //    (the outermost `Cargo.toml` declaring a `[workspace]` table), else a DEPLOYMENT's own
    //    `settings/` directory (an installed binary with no source tree above it). `None` means
    //    NEITHER rung answered — no name AND no project above the working directory — and is a
    //    legitimate answer the disclosure states.
    //
    //    ⚠ `_for`, not `_from`: the working directory is an `Option` all the way into the resolver,
    //    because the WALK is the half that needs somewhere to start and a NAME is not. This line
    //    used to be `spec.cwd.and_then(|cwd| …_from(override, cwd))` — the `and_then` on the CWD —
    //    so a process whose working directory had been removed, unmounted or made unsearchable
    //    dropped a `$VIKE_SETTINGS_DIR` that needed no walk to honour, while still RETURNING it in
    //    `Booted::settings_dir_override`. Every shipped `deploy/*.service` unit sets that variable,
    //    and on such a box the credential store still opened at the named directory (`vike_secrets::
    //    resolve_project` carries the override with no walk) while the policy CEILINGS, the state
    //    root, the log home and this boot's own disclosure all fell back to the no-project answers.
    //    `vike_secrets::project_settings_dir_for` is where that law is spelled, once, so the
    //    settings directory and the store inside it cannot answer differently.
    //
    //    `vike_secrets`'s resolver rather than `vike_model::state_path`'s: they are the same
    //    function twice (pinned equal by
    //    `crates/vike-bridge-core/tests/settings_dir_spellings.rs`), and this crate reaches for the
    //    copy in the ZERO-dependency crate.
    let override_dir = settings_dir_override(spec.env);
    let settings_dir = vike_secrets::project_settings_dir_for(override_dir.as_deref(), spec.cwd);

    // 3. The credentials, and the refusal that depends on them. BEFORE the load — see the module
    //    doc's step 3.
    let credentials = match &spec.credentials {
        Credentials::LoadWith(load) => {
            let map = load();
            vike_config::refuse_credential_file_arming(&map)?;
            Some(map)
        }
        Credentials::Deferred(_) => None,
    };

    // 4. ONE settings directory, ONE loader, no per-root layer wiring to get wrong.
    let settings = match spec.settings {
        SettingsLoad::Load => vike_config::load(settings_dir.as_deref(), spec.env)
            .map_err(|e| format!("settings could not be loaded: {e}"))?,
        SettingsLoad::Skip(_) => Settings::default(),
    };

    // 5. The PROGRAM-WRITTEN paths, all derived from the directory resolved at step 2 rather than
    //    from a second walk. A second walk does not honour the same override, which is how a
    //    disclosure came to describe a project the log file was not written into — and how a
    //    daemon's `alerts.json`, telegram ledger and rolling log came to hang off a different
    //    project from its own settings and credentials.
    let state_dir = settings_dir.as_ref().map(|dir| dir.join(vike_model::state_path::STATE_SUBDIR));
    let log_home = match spec.log_home {
        LogHome::UnderSettings => {
            state_dir.as_ref().map(|dir| dir.join(vike_model::state_path::LOGS_SUBDIR))
        }
        LogHome::Elsewhere(_) => None,
    };

    // 6. The disclosure, as LINES. Never printed here: a binary whose stdout is a protocol decides
    //    where they go, and there is no subscriber yet in any case.
    let boot_lines = match spec.disclosure {
        // Given step 2's OWN answer, never a fresh walk — the disclosure must not be able to
        // describe a directory this process did not load from.
        Disclosure::Render => vike_config::boot_lines(settings_dir.as_deref(), spec.env),
        Disclosure::Skip(_) => Vec::new(),
    };

    Ok(Booted {
        settings,
        settings_dir,
        settings_dir_override: override_dir,
        state_dir,
        credentials,
        log_home,
        identity_line: vike_buildinfo::version_line(spec.identity.name, spec.identity.version),
        boot_lines,
    })
}

/// The dotted key every [`boot_ceilings`] entry is spelled with — the same `<file>.<key>` shape
/// `vike-cli config show` prints and `vike_model::change_journal::SettingTarget`'s `key` carries,
/// so a `boot_settings` line and a `set_setting` line naming the same ceiling are `grep`-able with
/// one pattern.
const CEILING_KEY_PREFIX: &str = "policy.";

/// **The EFFECTIVE ceilings, as `(dotted key, value)` pairs** — what one boot record carries.
///
/// `None` is a ceiling that is NOT SET, and it is distinct from a missing pair: the key is always
/// present, only its value is absent, which is exactly why
/// `vike_model::change_journal::BootSettingsTarget` renders it as `null` rather than by omitting
/// the entry. "Uncapped" and "this build did not report that key" must not read identically to
/// somebody comparing two records a month apart.
///
/// # The [`Policy`] field that is deliberately NOT in here
///
/// The record's target claims the ceilings that are EFFECTIVE, so it carries only the fields that
/// are. ONE is excluded.
///
/// ⚠ `venues` — the per-venue arming ceiling — WAS the second exclusion, and it is in now. That
/// paragraph said the exclusion ends when `policy_is_consumed.rs`'s `Consumed::No` row for `venues`
/// is promoted to a `Consumed::At`, and names whoever edits that row as the author who adds it
/// here; stage 3 promoted it (the fold is `crates/vike-mount/src/lib.rs`'s `make_engine_with_legs`,
/// above the credential read), so it is effective and it is claimed.
///
/// It is rendered AGGREGATED — `live=[…] demo=[…] paper=<n>` — rather than as fourteen entries, for
/// the reason that paragraph gave: this anchor is costed at a few hundred bytes and fourteen rows
/// would be most of the record. The aggregation is lossless in the direction that matters, because
/// `paper` is the default: the two interesting tiers are NAMED, and the third is a count. See
/// [`render_venue_ceilings`].
///
/// `max_leverage` is the permanent exclusion, and the reason is semantic rather than cosmetic:
/// `crates/vike-mount/src/policy.rs`'s `MountPolicy::from` deliberately does not carry it — its
/// `1.0` default would clamp every deployment with no `policy.toml` to 1x — so a value an operator
/// wrote there is SET and NOT EFFECTIVE, and
/// `crates/vike-config/tests/policy_is_consumed.rs`'s row for it says so in as many words. Listing
/// it under a record that claims otherwise would assert the opposite of the gate. (That gate also
/// reads a textual `policy.<field>` mention outside a comment as a READ and would turn CI red — but
/// the record would be wrong even if the gate were silent.)
///
/// The dead-man pair is a THIRD shape, between "always set" and "excluded": the timeout is claimed
/// as `null` when the key is absent (off, with a warning at the live mount) and as its number
/// when written, and the action is claimed only while a written timeout actually arms the switch
/// — the inline comments on both rows argue it, and `vike_config::Policy::deadman_timeout_ms`
/// records why the timeout is no longer "always set" (it was, for one morning, at 60 s).
///
/// ⚠ The destructure below is EXHAUSTIVE on purpose — do NOT add `..`. A new [`Policy`] field must
/// break this line, so its author has to decide whether the boot anchor claims it, exactly as
/// `policy_is_consumed.rs`'s own destructure forces them to say where it is consumed.
pub fn boot_ceilings(policy: &Policy) -> Vec<(String, Option<String>)> {
    let Policy {
        max_leverage: _,
        venues,
        max_notional_per_order,
        max_account_exposure,
        max_sizing_equity,
        market_slippage,
        halt_admit,
        deadman_timeout_ms,
        deadman_action,
        // Bound as `_` and read back through the resolver below: the effective value of THIS key is
        // not the field (absent means ARMED), and a second copy of that rule here is the drift the
        // resolver exists to prevent.
        link_deadman_grace_ms: _,
    } = policy;
    vec![
        // Quote-currency notional cap on any single order. `None` = uncapped.
        (
            format!("{CEILING_KEY_PREFIX}max_notional_per_order"),
            max_notional_per_order.map(|v| v.to_string()),
        ),
        // The ACCOUNT-aggregate open-notional ceiling. `None` = uncapped, the
        // `max_notional_per_order` shape directly above and for the same reason: an absent key is
        // recorded as `null` rather than as an omitted pair, so "nobody set an account ceiling" and
        // "this build did not report the key" cannot read identically a month apart. It is claimed
        // here rather than excluded like `max_leverage` because it IS effective wherever it is
        // written — `crates/vike-mount/src/lib.rs`'s `make_engine_for_account` folds it onto
        // `vike_exec::RiskLimits` and `crates/vike-config/tests/policy_is_consumed.rs`'s row for it
        // is `Consumed::At`, which is exactly the distinction that keeps `max_leverage` out.
        (
            format!("{CEILING_KEY_PREFIX}max_account_exposure"),
            max_account_exposure.map(|v| v.to_string()),
        ),
        // The ceiling on the equity FIGURE the sizing and admission lanes may see. Same
        // `null`-when-absent shape as the two ceilings above, and claimed for the same reason: it
        // IS effective wherever it is written (`crates/vike-mount/src/lib.rs`'s
        // `make_engine_for_account` folds it onto `vike_exec::RiskLimits` and
        // `crates/vike-config/tests/policy_is_consumed.rs`'s row for it is a `Consumed::At`).
        // ⚠ An incident review needs this one specifically: absent means the engines sized against
        // the VENUE's wallet figure, which on a shared account is a number nobody on this box set.
        (
            format!("{CEILING_KEY_PREFIX}max_sizing_equity"),
            max_sizing_equity.map(|v| v.to_string()),
        ),
        // The emulated-market aggression band. `None` = each venue keeps its own literal.
        (format!("{CEILING_KEY_PREFIX}market_slippage"), market_slippage.map(|v| v.to_string())),
        // How much evidence the HALT sentinel demands. Always set — it is an enum with a default,
        // so there is no "unset" reading of it, and the compiled-in default IS the effective value.
        (format!("{CEILING_KEY_PREFIX}halt_admit"), Some(halt_admit.as_str().to_string())),
        // The dead-man switch — CLAIMED when it is effective, which is when the key is WRITTEN:
        // `vike-tradehub`'s live mount constructs `vike_core::CoreConfig::deadman` from these two
        // keys (`crates/vike-tradehub/src/tradehub_cli.rs`'s `deadman_config_from_policy`), and
        // `crates/vike-config/tests/policy_is_consumed.rs`'s rows for both are `Consumed::At`.
        // The timeout is the `max_notional_per_order` shape now — `None` = the key is ABSENT, the
        // switch is OFF and the live mount warned once — where for one morning it was "always
        // set, like `halt_admit`", because its compiled-in default was ARMED at 60 s; the field's
        // own doc records the reversal. An absent key is `null` here and NOT an omitted pair, so
        // "off by omission" and "this build did not report the key" cannot read identically a
        // month apart — and `0`, the explicit-off spelling, is still recorded as the number it is,
        // because "off by decision" and "off by omission" are exactly the two readings the mount's
        // warning exists to separate.
        (
            format!("{CEILING_KEY_PREFIX}deadman_timeout_ms"),
            deadman_timeout_ms.map(|v| v.to_string()),
        ),
        // The action is an enum with a default, like `halt_admit` — but unlike `halt_admit` it is
        // INERT unless the switch is constructed: an action nothing will ever take is not an
        // effective ceiling, and a record that named `cancel_all_and_halt` beside a `null` timeout
        // would claim a halt this process cannot perform. So it is claimed exactly when the
        // timeout arms the switch (`Some(n)`, `n > 0`), and `null` otherwise — the same test
        // `deadman_config_from_policy` applies, restated here because `vike-boot` sits below the
        // crate that owns that fold and cannot call it.
        (
            format!("{CEILING_KEY_PREFIX}deadman_action"),
            deadman_timeout_ms
                .filter(|&ms| ms != vike_config::DEADMAN_DISABLED_MS)
                .map(|_| deadman_action.as_str().to_string()),
        ),
        // The LINK dead-man's grace (M13) — ALWAYS set, and that is the row's whole content: this
        // key's absent state is ARMED at `vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS`, so a `null`
        // here would claim the opposite of what the process will do. `link_deadman_grace_ms_
        // effective` is the resolver, and it is called rather than restated so this record and the
        // live mount cannot disagree about which state a missing key is.
        (
            format!("{CEILING_KEY_PREFIX}link_deadman_grace_ms"),
            policy.link_deadman_grace_ms_effective().map(|ms| ms.to_string()),
        ),
        // The per-venue ARMING ceiling, aggregated. Always set, for the same reason `halt_admit`
        // is: every venue has a tier, and the compiled-in default (`paper` everywhere) IS the
        // effective value — which on this ceiling is the one that BITES, so a record that omitted
        // it would be silent about the single most consequential line in the file.
        (format!("{CEILING_KEY_PREFIX}venues"), Some(render_venue_ceilings(venues))),
    ]
}

/// The aggregated rendering [`boot_ceilings`] records for the per-venue arming ceiling:
/// `live=[aster,bybit] demo=[binance] paper=11`.
///
/// **The two risky tiers are NAMED and the safe one is COUNTED**, which is what makes one line
/// enough. `paper` is the default and the overwhelming majority; a name list of it would be most of
/// the record and would say nothing a reader could act on, while `live` and `demo` are exactly the
/// facts an incident review reaches for — *which venues could this process have traded on?* The
/// count is still there so two records a month apart cannot disagree about the roster's SIZE
/// without saying so (a venue added between them shows up as a different total).
///
/// Venue ids are in the map's own order, which is `vike_model::VENUES` sorted — the same order every
/// other roster walk in this workspace uses, so two records are diffable line-for-line.
fn render_venue_ceilings(venues: &vike_config::VenuePolicy) -> String {
    let named = |want: vike_config::VenueMode| {
        venues
            .iter()
            .filter(|(_, mode)| *mode == want)
            .map(|(venue, _)| venue)
            .collect::<Vec<_>>()
            .join(",")
    };
    let paper = venues.iter().filter(|(_, m)| *m == vike_config::VenueMode::Paper).count();
    format!(
        "live=[{}] demo=[{}] paper={paper}",
        named(vike_config::VenueMode::Live),
        named(vike_config::VenueMode::Demo)
    )
}

/// **Write ONE boot anchor** — the effective ceilings at this process start — into
/// `<state_dir>/changes/changes-YYYY-MM.jsonl`, with actor origin `boot`.
///
/// It is the durable twin of the disclosure a root prints from [`Booted::boot_lines`]. That
/// disclosure goes to a rolling log file `vike_log::DEFAULT_MAX_LOG_FILES` prunes and that the
/// shipped units' `VIKE_LOG_FILE_LEVEL=warn` silences outright, so it is not a record of anything a
/// week later — `vike_model::change_journal`'s module doc carries the measurement off the live
/// the CI box daemon. This is.
///
/// # ⚠ It is a BRACKET, not a detector. Do not describe it as one.
///
/// **Nothing in this workspace observes a HAND EDIT of `<project>/settings/policy.toml`.** There is
/// no file watcher in the tree (`notify` is not a workspace dependency), the running process does
/// not notice the edit at all, and `vike_model::change_journal`'s `set_setting` channel sees only
/// changes that arrived through the daemon's control socket. This record does not close that gap.
///
/// What it buys is strictly weaker and still worth having: **two consecutive boot records that
/// disagree prove something changed between them.** They do not say WHO, they do not say WHEN
/// inside the interval, and they cannot tell a hand edit from a redeploy that shipped a different
/// file. The instrument's resolution is the RESTART CADENCE and nothing finer, so a ceiling edited
/// and reverted between two starts is invisible to it — by construction, not by oversight.
///
/// # Rate, and the tail that bounds it
///
/// One record per start, a few hundred bytes each (`vike_model::change_journal::MAX_RECORD_BYTES`
/// caps one line at 4 KB and this shape is nowhere near it). A daemon restarted ten times a day
/// costs a few hundred kilobytes a year. The tail is a CRASH LOOP, where the ledger's growth rate
/// is the supervisor's retry rate rather than anything about settings: `deploy/vike-tradehub.service`
/// sets `RestartSec=5`, so the worst case is bounded at roughly 17k records a day. That is the
/// honest ceiling on this instrument's cost, and it is why the anchor is per-START rather than
/// periodic — a re-read on a timer would need a second `vike_config::load`, which
/// `crates/vike-boot/tests/one_owner.rs` exists to refuse.
///
/// # Returns, and the honest degradation
///
/// * `None` — **no state directory resolved, so NOTHING is written.** A root with no project above
///   its working directory must not invent a ledger location; that is
///   `crates/vike-tradehub/tests/settings_write_journal.rs`'s
///   `a_journal_less_surface_writes_nothing` rule, and it is the same answer
///   `vike_tradehub::server::SettingsShowSource`'s `change_journal` gives.
/// * `Some(Err(_))` — the append failed. Returned rather than logged, because this crate carries no
///   `tracing` dependency (see the module doc); the ROOT emits it, which it can, because unlike
///   [`boot`] this runs after `vike_log::init`.
///
/// `ts_ms` is a PARAMETER: `vike_model::change_journal` reads no clock, and the caller — a
/// composition root — stamps it.
pub fn journal_boot_settings(
    state_dir: Option<&Path>,
    policy: &Policy,
    version: &str,
    ts_ms: i64,
) -> Option<Result<PathBuf, ChangeJournalError>> {
    let state_dir = state_dir?;
    let ceilings = boot_ceilings(policy);
    let borrowed: Vec<(&str, Option<&str>)> =
        ceilings.iter().map(|(k, v)| (k.as_str(), v.as_deref())).collect();
    // `Outcome::Applied`: the values ARE in effect, which is the whole claim. `Actor::Boot`: the
    // channel is the process starting, and there is no human to name — see that enum's doc.
    let change = Change::boot_settings(Outcome::Applied, Actor::Boot, &borrowed);
    // `Proc::current` reads `current_exe`, which is the very thing being recorded — one file is
    // written by several binaries, and "which one wrote this line" is the first question asked of a
    // record nobody expected.
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(version));
    Some(journal.append(ts_ms, &change))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'a>(env: &'a HashMap<String, String>, cwd: &'a Path) -> BootSpec<'a> {
        BootSpec {
            env,
            cwd: Some(cwd),
            identity: Identity { name: "vike-test", version: "0.0.0" },
            removed_env: RemovedEnv::Refuse,
            settings: SettingsLoad::Load,
            credentials: Credentials::Deferred("the unit tests open no store"),
            log_home: LogHome::UnderSettings,
            disclosure: Disclosure::Render,
        }
    }

    /// The identity line is the SAME string `--version` prints — the two must not be able to
    /// disagree about which commit a box is running.
    #[test]
    fn the_identity_line_is_the_version_line() {
        let env = HashMap::new();
        let cwd = std::env::current_dir().unwrap();
        let booted = boot(&spec(&env, &cwd)).expect("a clean env boots");
        assert_eq!(booted.identity_line, vike_buildinfo::version_line("vike-test", "0.0.0"));
    }

    /// A BLANK override falls through to the walk rather than resolving settings to the working
    /// directory — `project_settings_dir_from`'s documented rule, restated here because this crate
    /// is what decides which value the resolver ever sees.
    #[test]
    fn a_blank_settings_dir_override_is_ignored() {
        for blank in ["", "   ", "\t"] {
            let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), blank.to_string())]);
            assert_eq!(settings_dir_override(&env), None, "{blank:?}");
        }
        let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), "  /srv/x  ".to_string())]);
        assert_eq!(settings_dir_override(&env).as_deref(), Some("/srv/x"));
    }

    /// **Every project-relative path hangs off the ONE resolved directory** — the property this
    /// crate exists for, asserted where it can be seen rather than left to the roots.
    ///
    /// Driven under `$VIKE_SETTINGS_DIR` pointing somewhere the working directory is not, because
    /// that is the only configuration in which a second walk gives itself away: a blind
    /// `project_state_dir(&cwd)`/`project_log_dir(&cwd)` answers with the CWD's project, and on
    /// the CI box the two agreed only because `WorkingDirectory=` happened to equal the override.
    #[test]
    fn the_state_root_and_the_log_home_hang_off_the_one_resolved_directory() {
        let named = std::env::temp_dir().join("vike-boot-elsewhere").join("settings");
        let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), named.display().to_string())]);
        let cwd = std::env::current_dir().unwrap();
        let booted = boot(&spec(&env, &cwd)).expect("an override boots");

        assert_eq!(booted.settings_dir.as_deref(), Some(named.as_path()));
        assert_eq!(
            booted.state_dir.as_deref(),
            Some(named.join(vike_model::state_path::STATE_SUBDIR).as_path()),
            "the state root is <settings dir>/state, never a second walk"
        );
        assert_eq!(
            booted.log_home.as_deref(),
            Some(
                named
                    .join(vike_model::state_path::STATE_SUBDIR)
                    .join(vike_model::state_path::LOGS_SUBDIR)
                    .as_path()
            ),
            "…and the log home is <state root>/logs, under it"
        );
        assert!(
            !cwd.starts_with(&named),
            "precondition: the working directory is NOT under the overridden project, so a blind \
             walk could not have produced these answers"
        );
    }

    /// **An override needs NO WALK to honour, so a process with no readable working directory must
    /// still resolve one.** `$VIKE_SETTINGS_DIR` NAMES the directory; the walk is the thing that
    /// needs somewhere to start.
    ///
    /// `std::env::current_dir()` fails whenever the directory a process was started in has been
    /// removed, unmounted or made unsearchable — an ordinary event for a long-lived deployment and
    /// for a verification lane whose tree is replaced under it, and all three shipped `deploy/*.
    /// service` units set `$VIKE_SETTINGS_DIR`. This boot used to resolve the directory as
    /// `spec.cwd.and_then(..)`, so on such a box it answered `settings_dir: None` while still
    /// returning `settings_dir_override: Some(..)` — and the two halves of the process then read
    /// DIFFERENT projects:
    ///
    /// * the CREDENTIALS still came from the named directory, because
    ///   `vike_secrets::resolve_project` -> `workspace_dotenv_path_from` honours the override with
    ///   no walk at all (`dotenv_path_for`'s no-CWD arm), while
    /// * the POLICY CEILINGS, the state root, the log home and the disclosure all fell back to the
    ///   no-project answers — compiled-in defaults, no `alerts.json` home, and a banner reading
    ///   "settings dir: NONE" on a box whose settings directory was named outright.
    ///
    /// A daemon holding live venue keys out of a file whose sibling `policy.toml` it never opened is
    /// precisely the split this crate exists to make impossible, and `max_notional_per_order`
    /// silently reverting to UNCAPPED is the sharp end of it.
    #[test]
    fn an_override_is_honoured_with_no_working_directory() {
        let named = std::env::temp_dir().join("vike-boot-no-cwd").join("settings");
        let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), named.display().to_string())]);
        let cwd = std::env::current_dir().unwrap();
        let mut s = spec(&env, &cwd);
        s.cwd = None;
        let booted = boot(&s).expect("no working directory is a legitimate boot, not a failure");

        assert_eq!(
            booted.settings_dir.as_deref(),
            Some(named.as_path()),
            "a NAMED settings directory needs no walk to reach it"
        );
        assert_eq!(
            booted.settings_dir_override.as_deref(),
            Some(named.display().to_string().as_str()),
            "…and it is still reported as the rung that answered"
        );
        assert_eq!(
            booted.state_dir.as_deref(),
            Some(named.join(vike_model::state_path::STATE_SUBDIR).as_path()),
            "the state root hangs off it, exactly as it does with a working directory"
        );
        assert_eq!(
            booted.log_home.as_deref(),
            Some(
                named
                    .join(vike_model::state_path::STATE_SUBDIR)
                    .join(vike_model::state_path::LOGS_SUBDIR)
                    .as_path()
            ),
            "…and so does the log home"
        );
        assert!(
            booted.boot_lines.iter().any(|l| l.contains(&named.display().to_string())),
            "the disclosure must NAME the directory this process resolved, never the \
             no-project line: {:?}",
            booted.boot_lines
        );
    }

    /// …and the other half of the same law: with no working directory AND no override there is
    /// genuinely nothing to answer with, so `None` stays `None`.
    ///
    /// This is the arm that keeps the fix above from being "always return something": the walk is
    /// the only other rung, and it has no start. Every path in [`Booted`] is then `None` together,
    /// which is the state the disclosure's `settings dir: NONE` line describes honestly.
    #[test]
    fn with_neither_a_working_directory_nor_an_override_nothing_resolves() {
        let env = HashMap::new();
        let cwd = std::env::current_dir().unwrap();
        let mut s = spec(&env, &cwd);
        s.cwd = None;
        let booted = boot(&s).expect("a clean env boots");

        assert_eq!(booted.settings_dir, None, "no name and no walk is no answer");
        assert_eq!(booted.settings_dir_override, None);
        assert_eq!(booted.state_dir, None, "and nothing may be derived from an absent project");
        assert_eq!(booted.log_home, None);
    }

    /// A BLANK override is not an override on this arm either — it must not resolve the settings
    /// directory to `""`, which is the working directory a process without one does not have.
    #[test]
    fn a_blank_override_resolves_nothing_with_no_working_directory() {
        for blank in ["", "   ", "\t"] {
            let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), blank.to_string())]);
            let cwd = std::env::current_dir().unwrap();
            let mut s = spec(&env, &cwd);
            s.cwd = None;
            let booted = boot(&s).expect("a blank override boots");
            assert_eq!(booted.settings_dir, None, "{blank:?}");
            assert_eq!(booted.state_dir, None, "{blank:?}");
        }
    }

    /// `LogHome::Elsewhere` withholds the log home and NOTHING else. A root with its own state-root
    /// variable (`vike-tradehub`'s `$VIKE_STATE_ROOT`) still needs this rung underneath it — that
    /// is what stopped its `state_dir` from having to walk again.
    #[test]
    fn declining_the_log_home_does_not_withhold_the_state_root() {
        let env = HashMap::new();
        let cwd = std::env::current_dir().unwrap();
        let mut s = spec(&env, &cwd);
        s.log_home = LogHome::Elsewhere("the test declines it");
        let booted = boot(&s).expect("a clean env boots");

        assert!(booted.log_home.is_none(), "declined");
        assert_eq!(
            booted.state_dir,
            booted.settings_dir.map(|d| d.join(vike_model::state_path::STATE_SUBDIR)),
            "the state root is still resolved — a declining root must not have to walk for it"
        );
    }
}
