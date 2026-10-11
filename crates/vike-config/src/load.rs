//! [`Settings`] and [`load`] — the layered loader.
//!
//! ```text
//! code defaults -> the settings database's rows -> CLI
//! ```
//!
//! **`docs/decisions/0086`: settings live only in the database.** **A box either has a row for a
//! key, or it does not — there is no second place to look.** The `<project>/vike.toml` per-project
//! override refusal (see [`crate::removed`]) is a DIFFERENT, independently-decided refusal.
//!
//! **The root is a DIRECTORY and it is a PARAMETER.** `settings_dir` is `<project>/settings`; this
//! crate resolves no directory of its own, never walks, never expands `~` and never reads a platform
//! variable — `vike_model::paths::state_path::project_settings_dir_from` is where the walk lives, and the
//! BINARY performs it. `settings_dir` is still meaningful under 0086: it is where
//! `<project>/settings/db/vike.db` lives, and it is what [`crate::removed::refuse_removed_project_file`]
//! probes beside. The loader reads no environment at all (decision 0111), so it is a pure function
//! of its arguments.
//!
//! **The Adoption/seal machinery is the store's own integrity check**: every write moves it
//! (`vike_secrets::write_setting_row_in`), and the question is do the counts a past write sealed
//! still match the tables now. See [`crate::source`] and
//! [`crate::mirror::apply_rows`].
//!
//! **A key with no row is not an error.** A store with no `setting` row for a key, or no database at
//! all, contributes nothing and the compiled-in default stands — that is what makes
//! `load(None)` the pure code-default answer.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{ConfigError, key_from_parse_message, redacted_parse_message};
use crate::flags::Flags;
use crate::layers::{CliOverride, CliOverrides};
use crate::policy::Policy;
use crate::preferences::Preferences;
use crate::source::StoreLayer;

/// The fully-resolved settings for this process.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Settings {
    /// Hard ceilings. Defaults + the settings database only — see [`Policy`].
    pub policy: Policy,
    /// Deployment settings. Full chain.
    pub config: Config,
    /// Taste and tuning. Full chain.
    pub preferences: Preferences,
    /// Operator toggles. Full chain.
    pub flags: Flags,
    /// Non-fatal resolutions the loader made, in the order it made them.
    ///
    /// Returned as DATA rather than logged, for the same reason the store is a parameter: this
    /// crate depends on `serde` + `toml` + `vike-model` and deliberately not on `tracing`, and a
    /// library that writes to stderr on its own initiative is a library that cannot be used by a
    /// binary whose stdout/stderr is a protocol (`vike-tradehub`, `vike-recorder`, the jforex
    /// sidecar). The BINARY logs these — it already owns logging init.
    ///
    /// Producers today:
    ///
    /// * [`NO_SETTINGS_DIRECTORY_WARNING`] — a load handed no settings directory at all: a process
    ///   that resolved no project gets
    ///   `Policy::default()`, which is every ceiling absent and every venue capped `paper`. Unsaid,
    ///   that is indistinguishable from a store that deliberately holds no row, and the
    ///   two want opposite reactions from an operator.
    /// * A WRITTEN reconcile refusal that S2 no longer honours — `reconcile = false` in a `flags`
    ///   ROW ([`crate::mirror::apply_rows`], which performs the same read-before-apply for the same
    ///   reason), or `--reconcile false` on the command line.
    ///   See [`crate::flags::reconcile_refusal_ignored`], which carries the argument; this frame is
    ///   the LAST one that can tell an explicit `false` from an unset value, because the resolved
    ///   field is a `bool`. It goes when `reconcile` stops being a flag at all.
    ///
    /// The disposition is unchanged and applies to both: a warning is deleted WITH the defect it
    /// describes, never left to go stale.
    pub warnings: Vec<String>,
    /// **The settings store could not be read at all, and this says so** — set when and only when
    /// [`load_with_source`] met [`StoreLayer::Unreadable`].
    ///
    /// It is a SECOND channel rather than one more `warnings` line (the refusal is pushed there too)
    /// because a consumer has to be able to REFUSE on it: `vike-cli` keeps every `config`/`secrets`
    /// verb running in this state — that is the JSON incident's lesson, that a refusal must not brick
    /// the tool that diagnoses it — while `trade` and `mcp` refuse, and a scan of free-text warnings
    /// is not a thing a verb may gate on.
    pub store_refusal: Option<String>,

    /// **The store opened and said something ILLEGAL** — the seal's counts do not match its tables,
    /// a row will not parse, or a row names a key the section rejects. Set by
    /// [`crate::mirror::apply_rows`].
    ///
    /// ⚠ **A SEPARATE channel from [`Settings::store_refusal`], because the two mean opposite things
    /// about the same file and want opposite repairs.** `store_refusal` is *this layer could not be
    /// READ*, so the values below are compiled-in defaults and merely unverified. This one is *this
    /// layer WAS read and is not trustworthy*, and there is no other layer to fall back to — so the
    /// values below are whatever the rows still resolved, possibly compiled-in defaults, which for
    /// `policy.max_notional_per_order` means NO CEILING.
    ///
    /// **Who must refuse on it, and who must not.** The diagnostic verbs — `config show`, every
    /// `secrets` verb — keep running, because a refusal that takes down its own diagnosis is the JSON
    /// incident. The surfaces that could ACT on an unsound ceiling refuse: `vike-cli trade`,
    /// `vike-cli mcp`, `vike-cli config check`, and `vike-tradehub` itself
    /// ([`docs/decisions/0069`](../../../docs/decisions/0069-an-unsound-seal-refuses-the-ordering-root-and-nothing-else.md)).
    /// By ruling, the ONLY repair for an unsound store is restoring `vike.db` from the box's nightly
    /// backup — no repair command is built (`docs/decisions/0086`).
    pub seal_refusal: Option<String>,
}

impl Settings {
    /// Record a seal refusal on BOTH channels — the gated one a verb refuses on, and `warnings`,
    /// which is what every root already prints at startup.
    ///
    /// ⚠ **The FIRST refusal wins and later ones append rather than replace.** A store can fail its
    /// integrity check AND then carry an unparsable row, and the first is usually the cause of the
    /// second; overwriting would report the symptom and hide it.
    pub fn mark_seal_refusal(&mut self, why: String) {
        self.warnings.push(why.clone());
        match &mut self.seal_refusal {
            Some(existing) => {
                existing.push_str("\n\nand also: ");
                existing.push_str(&why);
            }
            slot @ None => *slot = Some(why),
        }
    }
}

/// The no-settings-directory warning: [`load_with_cli`] was handed no settings directory, so every
/// ceiling on this process is the compiled-in default and no database could even be opened.
///
/// **Why this is a warning and not an error.** A missing project is the ordinary state of a dev
/// checkout run from the wrong directory and of a container whose bind mount did not land, and
/// refusing to start over it would strand an operator with a daemon that will not boot instead of one
/// that boots capped at `paper` (`docs/decisions/0013-degrade-vs-refuse.md`). What it may not do is
/// stay SILENT: `Policy::default()` is a real, permissive-looking answer that an operator's own
/// `config show` then attributes to nothing at all.
///
/// Held as a constant rather than formatted at the site because two binaries assert on it and a
/// message they match on by substring is a message that must have one spelling.
pub const NO_SETTINGS_DIRECTORY_WARNING: &str = "no settings directory resolved — every ceiling is the compiled-in default and every venue is \
     capped `paper`. Set VIKE_SETTINGS_DIR, or run from a project that has one.";

/// Load the settings, applying every layer in precedence order.
///
/// * `settings_dir` — `<project>/settings`, or `None` to skip that layer entirely (and say so on
///   [`Settings::warnings`] — see [`NO_SETTINGS_DIRECTORY_WARNING`]). Consulted only for the removed
///   `<project>/vike.toml` refusal — see [`crate::removed`] — since this crate opens no settings
///   store of its own.
///
/// Returns [`ConfigError`] naming the offending key on the first layer that fails — including a
/// REMOVED key, which is refused by name rather than ignored, and a REMOVED FILE, the retired
/// `<project>/vike.toml`. Non-fatal resolutions ride [`Settings::warnings`].
pub fn load(settings_dir: Option<&Path>) -> Result<Settings, ConfigError> {
    load_with_cli(settings_dir, &CliOverrides::default())
}

/// [`load`] plus the CLI layer — the highest-precedence one.
///
/// Split out rather than folded into `load`'s signature because most callers have no flags to
/// pass and the design's canonical entry point is the one-argument one. Note there is no
/// `policy` field on [`CliOverrides`] to pass, and no way to add one from outside this crate.
pub fn load_with_cli(
    settings_dir: Option<&Path>,
    cli: &CliOverrides,
) -> Result<Settings, ConfigError> {
    load_with_source(settings_dir, StoreLayer::NotConsulted(NOT_A_ROOT), cli)
}

/// What [`load`] and [`load_with_cli`] declare when they hand [`load_with_source`] no store.
///
/// A named constant rather than a literal at two call sites, because it is the ONE thing a reader
/// of either signature has to be told: these two loaders resolve no store and are therefore not a
/// composition root's entry point. `crates/vike-boot/tests/one_owner.rs` marks both `Owned::Reserved`
/// for exactly this reason, and lists the three library callers that use them legitimately.
const NOT_A_ROOT: &str = "`vike_config::load`/`load_with_cli` resolve no settings store: a composition root calls \
     `vike_boot::boot`, which calls `load_with_source` with the arm it read";

/// **[`load_with_cli`] plus the settings DATABASE** — the whole of what a settings key resolves
/// from, per `docs/decisions/0086`.
///
/// `source` is DATA the BINARY already read (`vike_secrets::read_settings_in`), never opened by this
/// crate — its own manifest says so, and 0057/0086 both give the reason under one database: a handle
/// that reaches the settings rows reaches the `credential` table too.
///
/// # What it REFUSES, and what it hands back as data
///
/// A row that says something ILLEGAL — an unknown key, a broken bound, a tombstone, a `null`, an
/// integer outside the store's range — or a seal whose counts do not match is MARKED on
/// [`Settings::seal_refusal`] rather than returned as an `Err`: a refusal that reaches this
/// function's own return type would take `vike-cli config show` and every `secrets` verb down with
/// the box it is supposed to help diagnose. The surfaces that can ACT on a ceiling
/// (`vike-cli trade`/`mcp`/`config check`, `vike-tradehub` itself) refuse on that mark instead.
///
/// A store that **could not be read at all** is different again and is also NOT an error return: it
/// comes back as [`Settings::store_refusal`], every key falling back to its compiled-in default. The
/// measurement that settles it is at the arm itself: on this tree an unreadable store also means an
/// empty CREDENTIAL map and therefore an all-paper mount, so a hard refusal here would take a daemon
/// down without preventing anything. `vike-cli config check` answers `Level::Fail` for this state,
/// which puts the stop at a deploy pre-flight rather than at a running daemon.
pub fn load_with_source(
    settings_dir: Option<&Path>,
    source: StoreLayer<'_>,
    cli: &CliOverrides,
) -> Result<Settings, ConfigError> {
    // Layer 0 — refuse a REMOVED layer before resolving a single value: `<project>/vike.toml` was
    // an override file one level ABOVE `<project>/settings/`, and its refusal survives 0086
    // untouched — it has no row analogue.
    crate::removed::refuse_removed_project_file(settings_dir)?;

    // Layer 1 — code defaults.
    let mut settings = Settings::default();

    if settings_dir.is_none() {
        // NO settings directory at all: `policy` stays `Policy::default()` — no notional ceiling, no
        // dead-man, no account row. Every one of those is a REFUSAL rather than an arming, so
        // this cannot leak a live order — but it is also invisible, and that invisibility was
        // `docs/ops/kill-switches.md`'s register entry. Deliberately NOT extended to "a directory
        // with no rows in it": that is the ordinary shape of a configured box that has not written
        // ceilings yet, and warning on it every boot is how a warning stops being read. The
        // distinction being drawn is between "you told me nothing" and "I never found your project".
        settings.warnings.push(NO_SETTINGS_DIRECTORY_WARNING.to_string());
    }

    // Layer 1.5 — the settings DATABASE. The ONLY settings layer below the CLI.
    match source {
        StoreLayer::Rows { rows, adopted } => {
            crate::mirror::apply_rows(&mut settings, rows, adopted);
        }
        StoreLayer::Unreadable(why) => {
            // ⚠ **NOT a refusal: resolve WITHOUT it, and MARK.** The measurement that settles this
            // arm: `crates/vike-secrets/src/store/backend.rs`'s `Backend` decides which store answers for a
            // CREDENTIAL on the same file's existence, and a present-but-unopenable store returns an
            // EMPTY credential map. An empty credential map IS the live gate, so an unreadable
            // settings store always co-occurs with an all-paper mount. The hazard a refusal would
            // protect against — armed and uncapped — is unreachable from this arm, and the cost of
            // refusing here is real: it would take down `vike-cli config show` and every `secrets`
            // verb along with the daemon whose credential half already degrades and announces for
            // exactly this state. The ENFORCEMENT POINT is `vike-cli config check`'s `Level::Fail`.
            settings.store_refusal = Some(format!(
                "the settings database could not be read ({why}). Every key resolves to its \
                 compiled-in default until the store can be opened again — there are no settings \
                 files to fall back to. Restore it from the box's nightly backup if this persists."
            ));
            settings.warnings.push(settings.store_refusal.clone().unwrap_or_default());
        }
        StoreLayer::NoDatabase | StoreLayer::TablesAbsent | StoreLayer::NotConsulted(_) => {}
    }

    // There is no environment layer (decision 0111): no key is read from the process environment,
    // and `crate::refuse_removed_env` refuses a variable that used to set one.

    // Layer 2 — CLI. Same three types, same structural exclusion of policy.
    settings.config.apply_cli(cli)?;
    settings.preferences.apply_cli(cli)?;
    settings.flags.apply_cli(cli)?;
    // ...and its own written-refusal check, for the same reason and against the same `Option<bool>`
    // shape. A `--reconcile false` is the shorter-lived of the two origins and the one more
    // likely to be typed by somebody who believes it is the switch.
    if cli.reconcile == Some(false) {
        settings.warnings.push(crate::flags::reconcile_refusal_ignored(
            "`--reconcile false` on the command line",
        ));
    }

    Ok(settings)
}

/// The PARSE half of reading a TOML document, for `crate::profile_risk`'s run-profile reader,
/// since a run/daemon PROFILE remains a document read from disk under 0086 (its *Phasing* leaves
/// the profile plane as a going concern for a separate migration).
pub(crate) fn parse_toml_str<T: for<'de> Deserialize<'de>>(
    file: &Path,
    text: &str,
) -> Result<T, ConfigError> {
    match toml::from_str::<T>(text) {
        Ok(parsed) => Ok(parsed),
        Err(e) => {
            // ⚠ NOT `e.to_string()`: that renders the offending SOURCE LINE verbatim, and a
            // credential pasted into the wrong line would be printed. See
            // `error::redacted_parse_message`.
            let message = redacted_parse_message(text, e);
            Err(ConfigError::Parse {
                file: file.to_path_buf(),
                key: key_from_parse_message(&message),
                message,
            })
        }
    }
}

#[path = "load_tests.rs"]
#[cfg(test)]
mod load_tests;
