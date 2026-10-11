//! Dukascopy JForex login config, read from the gitignored `.env`, **plus the three runtime facts
//! the sidecar is spawned with** ([`DukascopyTools`]): the JVM, the jar, and the `user.home` that
//! JVM is given.
//!
//! Dukascopy has no HMAC/token REST auth — its trading API is JForex (login/password/server).
//! Two demo accounts exist: DEMO1 (Dukascopy Bank SA, Swiss) and DEMO2 (Dukascopy Europe, EU).
//! Absent login/password → `None` (the live gate). The password never reaches Debug/Display.
//!
//! # Why the TOOL paths live here rather than in `exec.rs`
//!
//! Everything in this module is a PURE parser over a map the caller supplies — the shape
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs` calls `Layer::Injected` and names as the target
//! state ("libraries take configuration as parameters; only binaries read the process
//! environment"). [`resolve_dukascopy_tools`] is that shape applied to the jar and the JVM, and it
//! is the whole of the fix for the defect `exec.rs`'s module doc used to describe: those two paths
//! were resolved by the library itself, through `concat!(env!("CARGO_MANIFEST_DIR"), …)` — the tree
//! the binary was COMPILED in, which a deployed binary does not have. Both were
//! `Layer::Library` rows in `vike_ops::settings::SETTINGS`; they are `Layer::Injected` rows now,
//! and the `LIBRARY_PIN` ratchet shrank by two.
//!
//! The caller supplies the environment map AND the two already-resolved project directories —
//! `<project>/bin` ([`vike_model::paths::state_path::project_bin_dir_from`]) and
//! `<project>/settings/state` ([`vike_model::paths::state_path::project_state_dir_from`]). This module
//! performs no walk, reads no environment and probes the filesystem only where it must (an
//! existence test is the difference between a `JAVA_HOME` that names a JVM and one that names a
//! stale directory).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_model::paths::state_path::PROJECT_BIN_DIR;

/// Which provisioned Dukascopy demo account.
///
/// ⚠ **The two variants are two LEGAL ENTITIES, not two logins of one broker** — Dukascopy Bank SA
/// (Swiss/global) and Dukascopy Europe IBS AS (EU). An order routed to the wrong one reaches the
/// wrong counterparty under the wrong regulator, which is why every caller that picks a variant
/// from data must be able to REFUSE rather than fall back: see
/// [`DukascopyAccount::from_key_prefix`], whose `None` is the refusal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DukascopyAccount {
    /// Dukascopy Bank SA (Swiss / global).
    Demo1,
    /// Dukascopy Europe IBS AS (EU).
    Demo2,
}

impl DukascopyAccount {
    /// Every variant, so a caller enumerating them (and a test asserting the round trip below) does
    /// not write the list down a second time.
    pub const ALL: [DukascopyAccount; 2] = [DukascopyAccount::Demo1, DukascopyAccount::Demo2];

    /// **The OWNER PREFIX of this account's credential key names**, trailing `_` included —
    /// `DUKASCOPY_DEMO1_`.
    ///
    /// ⚠ **This is the ONE place either prefix is spelled**, and it used to be two: a private
    /// `prefix()` returning the bare `DUKASCOPY_DEMO1` for [`dukascopy_env_var_names`]' `format!`,
    /// plus this one for the store. Two spellings of one fact is two things to keep in step —
    /// `crates/vike-ops/tests/settings_secrets/settings_registry.rs` demanded a row for each, which would have made
    /// the registry claim this workspace reads a variable called `DUKASCOPY_DEMO1_` *and* one called
    /// `DUKASCOPY_DEMO1`, neither of which any store has ever held. The separator moved into the
    /// constant instead, so the key names are composed as `{p}LOGIN` and the two derivations cannot
    /// drift.
    ///
    /// ⚠ This is the spelling the settings database's `account` table uses as an account's
    /// re-derivable identity, and that is not a coincidence this crate may forget:
    /// `vike_secrets::AccountKeys::prefixes` is built by stripping each credential row's `field`
    /// off the end of its `name`, so a row filed against `DUKASCOPY_DEMO2_LOGIN` yields exactly
    /// `DUKASCOPY_DEMO2_`. `crates/vike-secrets/src/schema/classify.rs`'s `Classification::owner_prefix`
    /// states the property from the store's side: *an account's owner prefix is recoverable from
    /// any one of its own rows*, which is what lets two accounts that share `(venue, tier, label)`
    /// be told apart at all.
    #[must_use]
    pub const fn key_prefix(self) -> &'static str {
        match self {
            DukascopyAccount::Demo1 => "DUKASCOPY_DEMO1_",
            DukascopyAccount::Demo2 => "DUKASCOPY_DEMO2_",
        }
    }

    /// **Which account a credential-key OWNER PREFIX names**, or `None` when it names neither.
    ///
    /// The exact inverse of [`DukascopyAccount::key_prefix`], with the trailing `_` optional so a
    /// caller holding either spelling gets the same answer. `None` is a REFUSAL and the caller must
    /// treat it as one — never as "then it must be [`DukascopyAccount::Demo1`]". Demo1 is the Swiss
    /// bank and Demo2 is the EU one; a fallback would route an order to a broker nobody chose.
    ///
    /// Case-SENSITIVE and deliberately not repaired, for the reason
    /// `vike_model::accounts::account_keys::AccountLabel::parse` refuses `alt`: a spelling this function fixes
    /// on the operator's behalf is a spelling nobody learns, and every credential key in the store
    /// is upper-case already.
    #[must_use]
    pub fn from_key_prefix(prefix: &str) -> Option<DukascopyAccount> {
        DukascopyAccount::ALL.into_iter().find(|a| {
            let want = a.key_prefix();
            prefix == want || prefix == want.trim_end_matches('_')
        })
    }

    /// **The LEGAL ENTITY this account trades with**, for an operator-facing line.
    ///
    /// Not decoration: the whole hazard of picking a variant from data is that the two variants are
    /// different brokers, so a mount that says which one it reached is the only way an operator
    /// spots a wrong answer before an order does. Carries no credential and no account number.
    #[must_use]
    pub const fn broker(self) -> &'static str {
        match self {
            DukascopyAccount::Demo1 => "Dukascopy Bank SA (Swiss/global)",
            DukascopyAccount::Demo2 => "Dukascopy Europe IBS AS (EU)",
        }
    }
}

/// JForex session parameters for one account.
#[derive(Clone)]
pub struct DukascopyConfig {
    pub login: String,
    pub password: String,
    /// JForex platform host (e.g. the demo web-platform server).
    pub server: String,
}

impl std::fmt::Debug for DukascopyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never leak the password
        write!(f, "DukascopyConfig(login={}, server={})", self.login, self.server)
    }
}

/// The `(login, password)` credential names for a Dukascopy account.
///
/// Composed from [`DukascopyAccount::key_prefix`], which carries the separator — see that method for
/// why there is no second, separator-less spelling of the prefix any more. The JForex SERVER is not
/// one of them: it is the demo tier's `venue.dukascopy.demo.server` setting since decision 0095's
/// Task 7 retired the credential-map fold that carried it as `DUKASCOPY_DEMO{1,2}_SERVER`.
pub fn dukascopy_env_var_names(account: DukascopyAccount) -> (String, String) {
    let p = account.key_prefix();
    (format!("{p}LOGIN"), format!("{p}PASSWORD"))
}

/// Read Dukascopy config from a var map and the venue's settings. `None` when login or password is
/// unset/blank (the live gate). The server is `venue.dukascopy.demo.server` — every
/// [`DukascopyAccount`] is a demo account and both share the one row (ruling 10) — and may be empty
/// (the sidecar then resolves its built-in demo JNLP host).
pub fn load_dukascopy_config_from(
    account: DukascopyAccount,
    vars: &HashMap<String, String>,
    settings: &vike_secrets::venue_setting::VenueSettings,
) -> Option<DukascopyConfig> {
    let (login_k, pass_k) = dukascopy_env_var_names(account);
    let get = |k: &str| vars.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
    let login = get(&login_k);
    let password = get(&pass_k);
    if login.is_empty() || password.is_empty() {
        return None;
    }
    // EXACTLY the demo tier's row: the server is tier-scoped, so a machine-scoped `any` row is not
    // read (`vike-cli config show` labels one "not read as that field's value").
    let server = settings
        .get_exact(vike_secrets::venue_setting::SettingTier::Demo, "server")
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    Some(DukascopyConfig { login, password, server })
}

/// The variable that names the sidecar jar OUTRIGHT, skipping the project rung: `JFOREX_BRIDGE_JAR`.
pub const JFOREX_BRIDGE_JAR_ENV: &str = "JFOREX_BRIDGE_JAR";

/// The variable that names the JVM's home directory: `JAVA_HOME`. Third-party/OS-owned, and
/// honoured before the project's own JRE for exactly that reason — an operator who set it means it.
pub const JAVA_HOME_ENV: &str = "JAVA_HOME";

/// `jforex` — the sidecar's directory under `<project>/bin`, holding [`BRIDGE_JAR_FILE`].
///
/// A directory rather than a loose jar, per [`PROJECT_BIN_DIR`]'s rule: every tool under `bin/`
/// owns a subdirectory, because a tool is a directory's worth of files even when only one of them
/// is executable.
pub const JFOREX_TOOL_DIR: &str = "jforex";

/// `jre` — the RUNTIME JVM's directory under `<project>/bin`, holding one unpacked Temurin image
/// (`bin/jre/jdk-<v>-jre/bin/java`).
///
/// ⚠ **A JRE, and deliberately not the JDK.** The split is by WHEN the image is needed, which is
/// the same rule that put this whole directory beside `settings/` instead of in `vendor/`: a JRE
/// merely RUNS the sidecar jar, so it is a RUNTIME artifact and must live where a deployed binary
/// can find it; the JDK exists for `javac` on a `--rebuild`, so it is BUILD-TIME and stays in the
/// checkout's `vendor/tools/`. `crates/bridges/dukascopy/scripts/provision-jforex.sh` writes each
/// image to its own root, and `crates/bridges/dukascopy/tests/jdk_pin_gate.rs`'s
/// `the_runtime_jre_and_the_buildtime_jdk_never_share_a_directory` holds them apart.
pub const JRE_TOOL_DIR: &str = "jre";

/// The sidecar jar's file name inside [`JFOREX_TOOL_DIR`].
pub const BRIDGE_JAR_FILE: &str = "jforex-bridge.jar";

/// `jforex` — the JVM `user.home` the sidecar is spawned with, under `<project>/settings/state`.
///
/// ⚠ **The same spelling as [`JFOREX_TOOL_DIR`] and deliberately NOT the same constant**: that one
/// is `<project>/bin/jforex`, the READ-ONLY tool directory the jar is installed into, and this one
/// is `<project>/settings/state/jforex`, a directory the JVM WRITES. They sit under different roots
/// with different write grants — `deploy/vike-tradehub.service`'s `ReadWritePaths=` names
/// `<project>/settings/state` and deliberately does not name the tool directory or the binary
/// beside it — so unifying them on the argument that the strings match would move a writable
/// directory into a read-only tree.
///
/// ⚠ **What lands here is `user.home`, NOT the JForex folder itself.** The SDK builds its own
/// folder BENEATH the home it is given (`com/dukascopy/login/utils/NetworkUtil`'s
/// `defaultJForexFolderPath`, whose constant pool holds `user.home` and
/// `"Cannot create the JForex directory."`), so the platform cache and `Strategies/` land in
/// `<project>/settings/state/jforex/JForex/`. The extra level is the price of the only lever the
/// bundled SDK offers — the `IClient` has a cache-directory setter and `Bridge.java` calls none.
pub const JFOREX_HOME_SUBDIR: &str = "jforex";

/// Platform java executable name.
const JAVA_EXE: &str = if cfg!(windows) { "java.exe" } else { "java" };

/// The three runtime facts `DukascopyExecutionClient::spawn` needs, RESOLVED — never a rule the
/// exec client re-runs for itself.
///
/// Produced by [`resolve_dukascopy_tools`] and passed down as a parameter, so the only code that
/// decides where a jar, a JVM or the JVM's home lives is the composition root that already swept
/// the environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DukascopyTools {
    /// The program to spawn — an absolute path to a `java` executable, or the bare name `java`
    /// when the last rung (PATH lookup) is all that is left.
    pub java: String,
    /// The sidecar jar to run. Its existence is the caller's check, not this module's: a missing
    /// jar is the live gate (stay paper), and the path is wanted in the log either way.
    pub bridge_jar: PathBuf,
    /// **The `user.home` the JVM is started with** — `<project>/settings/state/jforex`, spelled
    /// into `-Duser.home=` by `DukascopyExecutionClient::spawn`. `None` means *do not override it*,
    /// which is the JVM's ordinary behaviour and is what a box with no project above its working
    /// directory gets; see [`resolve_dukascopy_tools`]' table for why that rung differs in KIND
    /// from the other two columns' last rungs.
    ///
    /// # Why this field exists at all
    ///
    /// The bundled JForex SDK writes its working directory (`.cache`, `Strategies`) to
    /// `$HOME/JForex` and nothing in `Bridge.java` says otherwise. MEASURED on the production box:
    /// `/home/<user>/JForex`, outside the project folder entirely. That is a latent defect in two
    /// deployments this venue is meant to run in:
    ///
    /// * `deploy/vike-tradehub.service` sets `ProtectHome=yes`, which makes `/home` INACCESSIBLE
    ///   inside the daemon's namespace rather than merely read-only — so the first armed Dukascopy
    ///   mount under the daemon cannot create its working directory at all. It has not bitten
    ///   because that box's `account` table arms no dukascopy row and the venue stays paper;
    /// * in a container `$HOME` may be `/` or unset, and the writable layer vanishes on the next
    ///   `docker run` — so the platform cache, whose first fetch is measured at over a minute
    ///   (`crates/bridges/dukascopy/src/exec.rs`'s `READY_TIMEOUT` is sized for it), would be
    ///   re-downloaded on every start.
    ///
    /// `<project>/settings/state` is already inside that unit's `ReadWritePaths=`, so pointing the
    /// home there costs no sandbox widening whatsoever.
    ///
    /// ⚠ **It does NOT retire the one-sidecar-per-process refusal.**
    /// `crates/bridges/dukascopy/src/mount.rs` declares the sidecar process-exclusive, and
    /// `crates/vike-mount/src/exclusive.rs`'s `pick_holder` rations it, because two JVMs of one
    /// user share one platform cache; moving that cache moves it for both sidecars of one process,
    /// so they would still share it. What retires the refusal is the `IClient` cache setter,
    /// unchanged.
    pub jforex_home: Option<PathBuf>,
}

/// Resolve all three runtime facts from a caller-supplied environment map, an already-resolved
/// `<project>/bin` directory and an already-resolved `<project>/settings/state` directory.
///
/// PURE with respect to configuration: `env` is the caller's `std::env::vars()` sweep,
/// `project_bin` is [`vike_model::paths::state_path::project_bin_dir_from`]'s answer and `project_state`
/// is [`vike_model::paths::state_path::project_state_dir_from`]'s. Nothing here reads the process
/// environment or performs the project walk — that is the binary's job, and it is what makes a
/// deployed binary resolve the PROJECT's tools rather than the build tree's.
///
/// ⚠ **`project_state` is a SEPARATE parameter and must not be derived from `project_bin`.** The
/// two directories are siblings today, so `project_bin.parent().join("settings/state")` would
/// compute the right answer on an ordinary box and the WRONG one on any box setting
/// `$VIKE_SETTINGS_DIR` — that variable moves `settings/` (and the state under it) without moving
/// `bin/`, which is precisely the split `project_bin_dir_from` and `project_state_dir_from` exist
/// to keep honest. Two resolvers, two parameters, one walk each.
///
/// # The three ladders
///
/// | | jar | JVM | JForex home |
/// |---|---|---|---|
/// | 1 | [`JFOREX_BRIDGE_JAR_ENV`] | [`JAVA_HOME_ENV`]`/bin/java`, when that file EXISTS | — no variable; see below |
/// | 2 | `<project>/bin/jforex/jforex-bridge.jar` | highest-sorting `<project>/bin/jre/*/bin/java` | `<project>/settings/state/jforex` |
/// | 3 | the same path, CWD-relative (no project found) | `java`, from `PATH` | `None` — the JVM keeps its own `user.home` |
///
/// Rung 2 is what replaced the compile-time `vendor/` fallbacks, and rung 3 is a genuine last
/// resort rather than a tidy default — with no project above the working directory there is no
/// better answer, and rung 1 names the artifact outright for anyone in that position. (Same shape,
/// and the same argument, as `crates/vike-research/src/bin/research.rs`'s `default_lightgbm` —
/// a DELETED binary, gone with the research crate, so the citation is the evidence for the shape
/// rather than a file to go and read; it is filed in `crates/vike-ops/tests/docs/citation_gate/dead_paths.rs`'s
/// `DEAD_PATH_EXCEPTIONS`.)
///
/// ⚠ **The home column's rung 3 is a DIFFERENT KIND of answer from the other two, and that is the
/// point.** A jar path and a program name are REQUIRED — there is no spawning without them, so the
/// last rung has to produce something even when it is a guess. The home is an OVERRIDE of a
/// default the JVM already has, and on a box with no project above the working directory the
/// pre-existing `$HOME/JForex` is not broken: that box is a developer's, not the sandboxed daemon
/// or the container this override exists for. So the honest last rung is to decide nothing and
/// leave `user.home` alone, which is what `None` means and what
/// `crates/bridges/dukascopy/src/exec.rs`'s `jvm_args` implements by emitting no flag at all.
///
/// ⚠ **The home column has NO environment rung either**, and adding one would need an argument
/// [`vike_model::paths::state_path::PROJECT_BIN_DIR`]'s own doc already makes for the directory above it:
/// a variable is warranted when the artifact has no other override, and this one moves with
/// `$VIKE_SETTINGS_DIR` like every other path under `settings/`. A second way to say one thing
/// would then need a documented precedence between the two.
///
/// ⚠ **Nothing here CREATES the home**, exactly as nothing here creates the jar or the JRE — this
/// module resolves and probes, and existence is the caller's live gate. The SDK's own
/// `"Cannot create the JForex directory."` path is what makes the directory on a real run.
///
/// ⚠ **A blank value is IGNORED rather than honoured**, the rule every `_from` resolver in
/// [`vike_model::paths::state_path`] follows: an empty `Environment=JFOREX_BRIDGE_JAR=` line would
/// otherwise resolve the jar to `""`, and the venue would degrade to paper reporting a path that
/// names nothing. `JAVA_HOME` already had this guard; the jar did not.
///
/// ⚠ **`JAVA_HOME` must actually CONTAIN a JVM to win.** A set-but-stale value falls through to
/// the project's own JRE instead of failing the spawn — preserved verbatim from the resolution
/// this replaced, because a leftover `JAVA_HOME` on a dev box is common and is not a decision.
pub fn resolve_dukascopy_tools(
    env: &HashMap<String, String>,
    project_bin: Option<&Path>,
    project_state: Option<&Path>,
) -> DukascopyTools {
    DukascopyTools {
        java: resolve_java(env, project_bin),
        bridge_jar: resolve_bridge_jar(env, project_bin),
        jforex_home: resolve_jforex_home(project_state),
    }
}

/// [`resolve_dukascopy_tools`]' JForex-home ladder — see its table.
///
/// One rung and a refusal, which is why it reads as a `map`: the answer is under the STATE
/// directory or there is no answer to give. It performs no filesystem probe because there is
/// nothing to choose between — unlike the JVM ladder, where an existence test is the difference
/// between a `JAVA_HOME` naming a JVM and one naming a stale directory.
fn resolve_jforex_home(project_state: Option<&Path>) -> Option<PathBuf> {
    project_state.map(|state| state.join(JFOREX_HOME_SUBDIR))
}

/// [`resolve_dukascopy_tools`]' jar ladder — see its table.
fn resolve_bridge_jar(env: &HashMap<String, String>, project_bin: Option<&Path>) -> PathBuf {
    if let Some(named) = non_blank(env.get(JFOREX_BRIDGE_JAR_ENV)) {
        return PathBuf::from(named);
    }
    let bin = project_bin.map_or_else(|| PathBuf::from(PROJECT_BIN_DIR), Path::to_path_buf);
    bin.join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE)
}

/// [`resolve_dukascopy_tools`]' JVM ladder — see its table.
fn resolve_java(env: &HashMap<String, String>, project_bin: Option<&Path>) -> String {
    if let Some(home) = non_blank(env.get(JAVA_HOME_ENV)) {
        let exe = PathBuf::from(home).join("bin").join(JAVA_EXE);
        if exe.is_file() {
            return exe.display().to_string();
        }
    }
    if let Some(jre) = project_bin.and_then(|bin| java_under(&bin.join(JRE_TOOL_DIR))) {
        return jre;
    }
    // The bare name, NOT `JAVA_EXE`: `std::process::Command` appends the platform suffix itself,
    // and this rung is a PATH lookup rather than a path.
    "java".to_string()
}

/// A map value, trimmed, or `None` when unset, empty or whitespace-only.
///
/// Takes the ALREADY-LOOKED-UP value rather than the map and a key, deliberately: that keeps each
/// `.get(CONST)` at the call site, where `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
/// `find_lookup_sites` can resolve the constant and PROVE the read is a map lookup. A key handed to
/// a helper has no call syntax to anchor on and lands in that gate's measured blind spot instead —
/// a spelling choice worth two lines, since these two rows are exactly the ones that just left the
/// `LIBRARY_PIN` work-list and the claim is that they are now injected.
fn non_blank(value: Option<&String>) -> Option<&str> {
    value.map(|v| v.trim()).filter(|v| !v.is_empty())
}

/// Highest-sorting `<dir>/*/bin/java` (e.g. `bin/jre/jdk-17.0.19+10-jre/bin/java`).
///
/// One level of nesting, because Temurin unpacks into a versioned directory. Highest-sorting so a
/// second image left by a version bump is preferred over the one it replaced.
///
/// ⚠ The image-mixing incident this workspace paid for once (`provision-jforex.sh`'s `find_java`)
/// cannot recur HERE: a JDK sorts before its own `-jre` sibling, but only JREs are ever written
/// under [`JRE_TOOL_DIR`] — the JDK lives in the checkout's `vendor/tools/`, which this sweep
/// cannot see at all.
fn java_under(dir: &Path) -> Option<String> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join("bin").join(JAVA_EXE))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.pop().map(|p| p.display().to_string())
}

#[path = "config_tests.rs"]
#[cfg(test)]
mod config_tests;
