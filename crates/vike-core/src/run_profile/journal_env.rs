//! Resolving the write-ahead journal sink from a loaded profile or the environment.

use std::collections::HashMap;

use super::schema::RunProfile;

/// Pure resolver behind [`journal_config_from_env`] (env-free, so it is unit-tested directly): pick
/// the write-ahead journal sink from an optional loaded profile and an optional quick-path dir.
///
/// A loaded `profile` is AUTHORITATIVE — its `[sinks].journal` decides, even when that means "no
/// journal" (a profile with no journal sink returns `None`); the `VIKE_JOURNAL_DIR` fallback is not
/// consulted when a profile is present, so a profile can never be silently overridden. Absent a
/// profile, `journal_dir` (with an optional `snapshot_every` override) builds a default-cadence
/// [`crate::JournalConfig`] via [`crate::JournalConfig::at`]. Absent both → `None`.
pub(crate) fn choose_journal(
    profile: Option<&RunProfile>,
    journal_dir: Option<std::path::PathBuf>,
    snapshot_every: Option<u64>,
) -> Option<crate::JournalConfig> {
    if let Some(p) = profile {
        return p.sinks.journal_config();
    }
    let mut cfg = crate::JournalConfig::at(journal_dir?);
    if let Some(v) = snapshot_every.filter(|&v| v >= 1) {
        cfg.snapshot_every = v;
    }
    Some(cfg)
}

/// Resolve the opt-in write-ahead command journal sink from the environment for a PRODUCTION binary
/// (the `vike-mount` cores; `vike-tradehub` resolves the same three rungs over its own map through
/// [`journal_config_from`], and the desktop — named here as `vike-app` until 2026-09-28 — builds
/// no core at all). OFF by default so a standard mount stays zero-overhead and byte-identical —
/// journaling is enabled only when one of these is set:
///
/// 1. **`VIKE_RUN_PROFILE=<run.toml>`** — load that [`RunProfile`] and use its `[sinks].journal`
///    mapping (the auditable-config-artifact path). A missing/malformed profile disables journaling
///    and logs a `warn` (a set-but-broken profile is a misconfiguration worth surfacing, not silently
///    falling through to the dir knob); a valid profile whose `[sinks]` omits `journal` is honored as
///    "journaling off".
/// 2. **`VIKE_JOURNAL_DIR=<dir>`** — a single-knob quick opt-in with the default cadence
///    ([`crate::JournalConfig::at`]); `VIKE_JOURNAL_SNAPSHOT_EVERY=<n>` optionally overrides the snap
///    cadence. Consulted only when `VIKE_RUN_PROFILE` is unset.
/// 3. neither set → `None`.
///
/// This mirrors the codebase's established env-driven opt-in config (e.g. `vike_exec::affinity`'s
/// `VIKE_PIN_CORES`, the `VIKE_RECORD_PROPERTIES` recorder) — absent env ⇒ inert.
pub fn journal_config_from_env() -> Option<crate::JournalConfig> {
    journal_config_from(&journal_env_snapshot())
}

/// This function's three variables, read from the process env as THREE explicit `env::var` calls.
///
/// ⚠ Deliberately not a `std::env::vars()` sweep, and the reason is the settings registry rather
/// than taste: `crates/vike-ops/tests/settings/settings_registry.rs` resolves a read by CALL SITE, so a bulk
/// sweep here would leave three declared rows with no resolvable read and the gate would call them
/// stale. `vike_data::PropertiesRecorder`'s `env_snapshot` is the same shape for the same reason and
/// says so in its own doc.
fn journal_env_snapshot() -> HashMap<String, String> {
    [
        ("VIKE_RUN_PROFILE", std::env::var("VIKE_RUN_PROFILE")),
        ("VIKE_JOURNAL_DIR", std::env::var("VIKE_JOURNAL_DIR")),
        ("VIKE_JOURNAL_SNAPSHOT_EVERY", std::env::var("VIKE_JOURNAL_SNAPSHOT_EVERY")),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.ok().map(|v| (k.to_string(), v)))
    .collect()
}

/// [`journal_config_from_env`] over a CALLER-SUPPLIED map — the same three variables, the same
/// precedence, and no process-environment read of its own.
///
/// This exists because `config.journal_dir` was a declared settings key nothing could reach:
/// `VIKE_JOURNAL_DIR` is read here, in a library, several frames below the binaries that own the
/// settings sweep, and `vike_config::CONSUMPTION` carried that as a written admission. The fix is
/// the one this workspace prefers everywhere — the value arrives as a PARAMETER and the BINARY
/// does the I/O.
///
/// ⚠ **The caller decides precedence by what it puts in the map, and the rule is unchanged: the
/// ENVIRONMENT wins.** `crates/vike-tradehub/src/tradehub_cli/flags.rs`'s `journal_vars` is the worked
/// example — it starts from the real process env and inserts the `config.journal_dir` setting only
/// where the variable is ABSENT, so an `Environment=` line in a unit beats the `config.journal_dir`
/// row exactly as it did when this function could only read the environment.
///
/// ⚠ `VIKE_RUN_PROFILE` still short-circuits the whole thing, and a profile that names no
/// `[sinks].journal` still means "journaling off". That ordering predates this seam and is not a
/// consequence of it; [`journal_config_from_env`]'s doc above is the authority for all three rungs.
#[must_use]
pub fn journal_config_from(vars: &HashMap<String, String>) -> Option<crate::JournalConfig> {
    if let Some(path) = vars.get("VIKE_RUN_PROFILE") {
        return match RunProfile::from_path(path) {
            Ok(p) => choose_journal(Some(&p), None, None),
            Err(e) => {
                tracing::warn!(
                    "VIKE_RUN_PROFILE set but the profile did not load ({e}); journaling disabled"
                );
                None
            }
        };
    }
    let dir = vars.get("VIKE_JOURNAL_DIR")?;
    let snapshot_every =
        vars.get("VIKE_JOURNAL_SNAPSHOT_EVERY").and_then(|s| s.parse::<u64>().ok());
    choose_journal(None, Some(std::path::PathBuf::from(dir)), snapshot_every)
}
