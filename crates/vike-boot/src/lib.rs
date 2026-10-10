//! **The startup sequence, owned once.** Every composition root runs the same ordered steps
//! through [`boot`] — refuse a REMOVED environment variable, resolve `<project>/settings`, load
//! the credentials, load the settings, derive the state and log paths, render the disclosure —
//! and its [`BootSpec`] literal declares how it departs.
//!
//! # The incident this order is made of
//!
//! On the CI box the project-root walk answered with an unrelated directory. The daemon loaded no
//! policy, no config and NO CREDENTIALS, so every venue silently dropped to paper — with no error,
//! because "there are no settings here" and "the settings here say nothing" are indistinguishable
//! downstream. `crates/vike-config/src/boot.rs` carries that incident and renders the disclosure
//! that answers it; `crates/vike-model/src/paths/state_path/project_root.rs`'s
//! `project_settings_dir` carries the walk's own precedence rule and the hijacks it closed.
//!
//! The walk used to happen in five places. Here **ONE walk DECIDES**, and every project-relative
//! path a root uses derives from its answer — the settings load, the state root
//! ([`Booted::state_dir`]: the log home, `alerts.json`, the telegram ledger, the strategy-state
//! sidecars) and the disclosure. A second walk is the defect: the `_from`-less resolvers are
//! `$VIKE_SETTINGS_DIR`-blind, so under the override the second answer names another project
//! (the desktop's log file once hung off one while its disclosure named the other).
//!
//! ⚠ **"One walk decides" is not "one `is_dir` probe runs".** Two resolutions remain, and
//! neither can DISAGREE with this one:
//!
//! * **The credential store.** [`Credentials::LoadWith`] calls the ROOT's own loader, which
//!   resolves the store for itself through the same pure resolver
//!   (`vike_secrets::project_settings_dir_for`) over the same `$VIKE_SETTINGS_DIR` and working
//!   directory: a duplicated cost, never a second answer.
//!   `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` tracks
//!   making it one call.
//! * **`vike_bridge_core::halt`'s sentinel.** `halt_path_from_env` memoizes one resolution deep
//!   inside a library, so the project reaches it as a PARAMETER: a root calls
//!   `vike_bridge_core::halt::declare_project_state_dir` with [`Booted::state_dir`]. ⚠ A root
//!   that builds an `ExecutionClient` without booting still walks for the sentinel, blind to
//!   `$VIKE_SETTINGS_DIR`, and nothing GATES it: the last two such roots,
//!   `crates/vike-run/src/bin/ibkr_mount.rs` and
//!   `crates/vike-run/src/bin/polymarket_maker_paper.rs`, were deleted rather than fixed.
//!   `vike_bridge_core::halt::HaltProject::WalkFrom` carries the detail.
//!
//! # The order, and why each step is where it is
//!
//! 1. **[`vike_config::refuse_removed_env`]** — before a window, a feed, a core or a venue mount.
//!    An operator who set `VIKE_MAX_ORDER_NOTIONAL` believes a ceiling is armed; a build that
//!    quietly ignored it would trade UNCAPPED while they believed otherwise.
//! 2. **The settings DIRECTORY**, resolved once (see above).
//! 3. **The CREDENTIALS**, through the root's own loader, then
//!    [`vike_config::refuse_credential_file_arming`] (a credential row must not be able to arm
//!    real money: the store is the wrong place for that). ⚠ This precedes the settings LOAD on
//!    purpose: a tree that both arms real money AND has a broken `policy` row must fail with the
//!    ARMING message, the more urgent of the two.
//! 4. **[`vike_config::load_with_source`]** over the settings DATABASE, read once, read-only, from
//!    step 2's directory. The ARM it read (`vike_config::StoreLayer`) is handed in rather than an
//!    `Option`, so WHICH store state answered survives into the resolved settings. A store that
//!    will not open is a MARK on [`vike_config::Settings::store_refusal`] and a warning, never a
//!    refusal here (`vike-cli config check` fails that state at a deploy pre-flight); a row that
//!    is INVALID is a refusal naming the key.
//!
//! 5. **The project-relative paths** — [`Booted::state_dir`] and [`Booted::log_home`], joined
//!    onto step 2's answer — and then **the log subscriber**, built by the BINARY from those and
//!    [`Booted::settings`].
//! 6. **The disclosure** — [`Booted::identity_line`] then [`Booted::boot_lines`], emitted by the
//!    binary once a subscriber exists. It is rendered from step 2's DIRECTORY and step 4's STORE
//!    ARM, never from a second resolution of either.
//!
//! ⚠ **Steps 1-4 precede step 5.** The log DIRECTORY (`config.log_dir`) and both log LEVELS are
//! themselves settings, so a subscriber built first could only honour its defaults. Which is
//! why **this crate logs NOTHING**: everything it would say comes back as DATA
//! ([`Booted::boot_lines`], [`vike_config::Settings::warnings`]) and the binary emits it, which
//! also lets a binary whose STDOUT IS A PROTOCOL (`vike-tradehub`, `vike-datahub`, `vike-cli mcp`)
//! choose the stream.
//!
//! # What this crate deliberately does NOT do
//!
//! * **It does not call `vike_log::init`, nor depend on `vike-log`**: that one edge adds dozens
//!   of packages to `vike-cli` (`crates/vike-boot/Cargo.toml` carries the measurement). The root
//!   builds its own `vike_log::LogConfig` from [`Booted::log_home`] and holds its own guards; the
//!   order is still enforced by a data dependency, since a root cannot name a level or a log home
//!   it has not booted for.
//! * **It reads no environment.** [`BootSpec::env`] is the single `std::env::vars()` sweep the
//!   composition root owns. Anything under `crates/vike-boot/src/` scores `Layer::Library` in
//!   `crates/vike-ops/tests/settings_secrets/settings_registry.rs`, whose `LIBRARY_PIN` ratchet
//!   refuses a library reading global state its caller can neither see nor override.
//! * **It never opens the credential store.** [`Credentials::LoadWith`] takes the root's OWN
//!   loader as a function, so `vike-cli` — which must not link `vike_bridge_core`'s
//!   ureq/tungstenite/rustls stack — can defer the read
//!   (`crates/vike-boot/tests/dependency_floor.rs` is the gate).
//!
//! # The roots are NOT identical, and the differences are declared
//!
//! Every way a root departs from the full sequence is an enum arm CARRYING ITS REASON
//! ([`RemovedEnv::Ignore`], [`SettingsLoad::Skip`], [`Credentials::Deferred`],
//! [`LogHome::Elsewhere`], [`Disclosure::Skip`]), so a [`BootSpec`] literal reads as that root's
//! declaration of what it does at startup: `vike-cli` builds no subscriber and defers the
//! credential read to its order-write surfaces; `vike-backend catalog` loads no settings at all.
//!
//! # The BOOT ANCHOR — [`journal_boot_settings`], and why it is not part of [`boot`]
//!
//! A root that ENFORCES the ceilings also writes one durable line per start saying what they
//! effectively were, into `vike_model::change_journal`. It is a SEPARATE call rather than a
//! seventh step, for two reasons: [`boot`] runs before `vike_log::init`, so a write failure would
//! have nowhere to be reported; and the record belongs in the root's own state tree, which is NOT
//! always [`Booted::state_dir`] (`vike-tradehub`'s `$VIKE_STATE_ROOT` relocates it), so the state
//! directory arrives as a PARAMETER.

mod boot_anchor;
mod refusals;
mod spec;

pub use boot_anchor::{boot_ceilings, journal_boot_settings};
pub use spec::{
    BootSpec, Booted, Credentials, Disclosure, Identity, LogHome, RemovedEnv, SettingsLoad,
};

use std::collections::HashMap;

use refusals::seal_gate;
use vike_config::Settings;

/// `$VIKE_SETTINGS_DIR`, spelled as a LITERAL rather than through `vike_secrets::SETTINGS_DIR_ENV`.
///
/// `vike_model::scan`'s map-lookup sweep resolves constants CRATE-wide, so an IMPORTED one would make
/// this read invisible to the settings registry — and a declared read is the point.
/// `crates/vike-bridge-core/src/credentials.rs`'s `load_workspace_secrets_from_env` spells it the
/// same way for the same reason.
fn settings_dir_override(env: &HashMap<String, String>) -> Option<String> {
    env.get("VIKE_SETTINGS_DIR").map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// **Run the startup sequence.** Returns the value the binary holds, or the one message it prints
/// to stderr before exiting.
///
/// The error strings are the roots' own, verbatim: `refuse_removed_env`'s operator-facing block,
/// `refuse_credential_file_arming`'s, and `"settings could not be loaded: {e}"` for a settings
/// store that holds an invalid row. A caller prefixes its own binary name.
///
/// Logs nothing, prints nothing, and touches no global state — see this module's doc.
pub fn boot(spec: &BootSpec<'_>) -> Result<Booted, String> {
    // 1. A stale RISK CEILING stops the process before it does anything at all.
    if let RemovedEnv::Refuse = spec.removed_env {
        vike_config::refuse_removed_env(spec.env)?;
    }

    // 2. THE settings directory, resolved ONCE for this whole process: `$VIKE_SETTINGS_DIR` names
    //    it outright and wins, otherwise the walk answers; `None` means NEITHER rung answered, a
    //    legitimate answer the disclosure states. ⚠ `_for`, not `_from`: the working directory is
    //    an `Option` all the way in, because only the WALK needs somewhere to start, so a process
    //    with no readable working directory still honours a NAMED directory. That law is spelled
    //    once, in `vike_secrets::project_settings_dir_for`, which the credential store's resolver
    //    shares, so the settings directory and the store inside it cannot answer differently.
    let override_dir = settings_dir_override(spec.env);
    let settings_dir = vike_secrets::project_settings_dir_for(override_dir.as_deref(), spec.cwd);

    // 3. The credentials, and the refusals that depend on them. BEFORE the load — see the module
    //    doc's step 3.
    let credentials = match &spec.credentials {
        Credentials::LoadWith(load) => {
            let map = load();
            vike_config::refuse_credential_file_arming(&map)?;
            Some(map)
        }
        Credentials::Deferred(_) => None,
    };

    // 4. ONE settings directory, ONE loader. The settings DATABASE is read here, once and
    //    read-only (`vike_secrets::read_settings_in` opens by flag and never creates), over step
    //    2's directory; `vike_config::StoreLayer::of` is the ONE mapping from that read to the arm
    //    both `load_with_source` and step 6 are handed. Two dispositions, deliberately split: a
    //    store that will not OPEN is a mark on `Settings::store_refusal` plus a warning, never a
    //    refusal here; a store that opens and holds an INVALID row is a hard refusal NAMING THE
    //    KEY, because a store that says something illegal is one somebody edited and believes in.
    //    The read sits behind the same `match spec.settings` as the load, so `SettingsLoad::Skip`
    //    probes no store.
    let store_read = match spec.settings {
        SettingsLoad::Load | SettingsLoad::LoadAndRefuseUnsoundSeal => {
            settings_dir.as_deref().map(vike_secrets::read_settings_in)
        }
        SettingsLoad::Skip(_) => None,
    };
    let mut store_refusal = String::new();
    let source = vike_config::StoreLayer::of(store_read.as_ref(), &mut store_refusal);

    let settings = match spec.settings {
        SettingsLoad::Load | SettingsLoad::LoadAndRefuseUnsoundSeal => {
            let settings = vike_config::load_with_source(
                settings_dir.as_deref(),
                source,
                &vike_config::CliOverrides::default(),
            )
            .map_err(|e| format!("settings could not be loaded: {e}"))?;
            // ⚠ The refusal is HERE and not inside `load_with_source`, deliberately: the loader is
            // shared by every root including the repair verbs, which must keep running. The LOADER
            // marks; the ROOT decides. See `SettingsLoad::LoadAndRefuseUnsoundSeal` for why exactly
            // one disposition refuses.
            seal_gate(
                matches!(spec.settings, SettingsLoad::LoadAndRefuseUnsoundSeal),
                settings.seal_refusal.as_deref(),
            )?;
            settings
        }
        SettingsLoad::Skip(_) => Settings::default(),
    };

    // 5. The PROGRAM-WRITTEN paths, all derived from the directory resolved at step 2 rather than
    //    from a second walk, which would not honour the same override.
    let state_dir =
        settings_dir.as_ref().map(|dir| dir.join(vike_model::paths::state_path::STATE_SUBDIR));
    let log_home = match spec.log_home {
        LogHome::UnderSettings => {
            state_dir.as_ref().map(|dir| dir.join(vike_model::paths::state_path::LOGS_SUBDIR))
        }
        LogHome::Elsewhere(_) => None,
    };

    // 6. The disclosure, as LINES. Never printed here: a binary whose stdout is a protocol decides
    //    where they go, and there is no subscriber yet in any case.
    let boot_lines = match spec.disclosure {
        // Given step 2's OWN answer and step 4's OWN store arm, never a fresh walk and never a
        // second read — the disclosure must not be able to describe a directory this process did
        // not load from, nor a SOURCE it did not resolve from.
        Disclosure::Render => vike_config::boot_lines(settings_dir.as_deref(), source),
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

#[cfg(test)]
mod lib_tests;
