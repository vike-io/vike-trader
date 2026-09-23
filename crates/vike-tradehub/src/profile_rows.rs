//! `profile_rows` — **the daemon profile as ROWS, the selection the owner ruled on, and the fence
//! that keeps a migration from arming anything.**
//!
//! Phase 3 of `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`. The STORE
//! half lives in `vike_secrets::profile_store` (a leaf crate with no `vike-*` dependency, which is
//! where a schema belongs); this module is the half that knows what a `DaemonProfile` IS, and it is
//! in the one crate that owns that type.
//!
//! # What this module is FOR, in one sentence
//!
//! To make it provable that **a box which is on paper today is on paper after the migration, and a
//! box which is live today is live after it, with the same mounts** — which is the first requirement
//! of the whole phase, because 0057's Question 3 records that writing an active-profile row is
//! itself capable of arming a mount that a missing line was holding.
//!
//! # ⚠ THE SELECTION MECHANISM, MEASURED — and where the record's description needed correcting
//!
//! 0057 Question 3 says *"the active profile is an environment variable in an untracked file and its
//! ABSENCE is the paper gate"*. Read against the code, that is ONE sentence describing TWO different
//! inputs and a third thing that is neither:
//!
//! | what | selected by | absent ⇒ |
//! |---|---|---|
//! | the DAEMON profile (`tradehub.toml`) — the mount set | **`--config <path>`, a REQUIRED argv flag** (`crate::tradehub_cli`'s `parse_args_from`: *"missing required `--config <profile.toml>`"*). There is NO environment variable and NO default path | the daemon cannot start at all |
//! | the RUN profile (`run-live.toml`) — the `[risk]` ceilings | `--profile <path>`, else `VIKE_RUN_PROFILE` (`vike_core::resolve_profile`); on the shipped unit that variable arrives from `EnvironmentFile=-<root>/.env` | on a LIVE arm the mount **REFUSES TO START** — `vike_mount::MountError::MissingRiskBudget`, the daemon exits FAILURE. It does NOT fall back to paper |
//! | **the paper gate** | `flags.tradehub_live` — `<project>/settings/flags.toml`, still overridden by `VIKE_TRADEHUB_LIVE` (`vike_config::arming`'s `TRADEHUB_LIVE_ARMING`) | the PAPER mount |
//!
//! So the paper gate is a FLAG, not a profile selector, and the run profile's absence is a REFUSAL
//! rather than a demotion. Both corrections make the migration hazard **worse**, not better, which
//! is why they are written here rather than left as a footnote: writing an active run-profile row on
//! a box whose live flag is on turns a daemon that exits FAILURE into a daemon that trades. That is
//! not a state anybody would describe as "it was on paper", and it is not one an operator watching
//! `systemctl status` would mistake for a no-op either.
//!
//! [`ArmingOutcome`] is those three inputs folded into the one answer that matters, and
//! [`resolve_arming`] is what the daemon calls to disclose it at startup.

use std::collections::BTreeMap;

use vike_config::ceilings::{CeilingHome, PRE_TRADE_CEILINGS};
use vike_core::RunProfile;
use vike_secrets::profile_store::{
    ActivePlan, MountRow, ProfileKind, ProfileRow, Selected, SelectionSource, StoredProfile,
    plan_active_row, render_daemon_toml, render_run_toml, select,
};

use crate::config::{DaemonProfile, PrimaryMount};

// ---------------------------------------------------------------------------------------------
// Selection — the owner's ruling, applied to each of the two selectors this daemon has
// ---------------------------------------------------------------------------------------------

/// **Which DAEMON profile is live.** The row wins; the required `--config` argument is the rung
/// below it.
///
/// There is deliberately no environment rung: MEASURED, no environment variable selects this
/// document today, so inventing one here would be this module adding a selector rather than moving
/// one.
#[must_use]
pub fn select_daemon_profile(active_row: Option<&str>, config_arg: &str) -> Selected {
    select(active_row, Some(config_arg), None)
}

/// **Which RUN profile is live.** The row wins, then `--profile`, then `VIKE_RUN_PROFILE` — the
/// lower two being exactly `vike_core::resolve_profile`'s existing order, which this does not
/// change.
///
/// `vars` is the caller-supplied process-environment map. This function reads no environment of its
/// own: `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` is a ratchet, and a library
/// reaching for `env::var` is the thing it ratchets down.
#[must_use]
pub fn select_run_profile(
    active_row: Option<&str>,
    profile_arg: Option<&str>,
    vars: &std::collections::HashMap<String, String>,
) -> Selected {
    select(active_row, profile_arg, vars.get("VIKE_RUN_PROFILE").map(String::as_str))
}

/// The one line a startup disclosure prints for a selection, naming the winner and everything it
/// shadowed.
///
/// §7 of `docs/superpowers/specs/2026-09-13-settings-store-schema-design.md` is the argument: *"a
/// file could never report this, because a file cannot know it lost"*. With the row winning, an
/// operator whose `ExecStart --config` has stopped deciding anything must be TOLD, or the ruling
/// ships the exact defect it ruled against.
#[must_use]
pub fn selection_line(what: &str, sel: &Selected) -> String {
    let Some(value) = sel.value.as_deref() else {
        return format!("{what}: nothing selects one");
    };
    let source = sel.source.map_or("?", SelectionSource::word);
    if sel.shadowed.is_empty() {
        return format!("{what}: `{value}` (from the {source})");
    }
    let lost: Vec<String> =
        sel.shadowed.iter().map(|s| format!("`{}` (the {})", s.value, s.source.word())).collect();
    format!(
        "{what}: `{value}` (from the {source}) — SHADOWING {}, which no longer decides anything",
        lost.join(", ")
    )
}

// ---------------------------------------------------------------------------------------------
// The arming outcome
// ---------------------------------------------------------------------------------------------

/// Why a LIVE arm would refuse to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveRefusal {
    /// The run profile's `[risk]` table does not supply the account-dependent caps a live mount
    /// refuses to start without — including the case where there is NO run profile at all, which is
    /// the same refusal with every key missing.
    ///
    /// The keys are DERIVED from `vike_config::ceilings::PRE_TRADE_CEILINGS`
    /// ([`live_risk_budget_missing`]), never restated.
    MissingRiskBudget(Vec<&'static str>),
    /// A run profile resolved but its `mode` is not `live`, which
    /// `vike_core::RunProfile::risk_for_live_venue_mount` refuses outright — a backtest/paper
    /// profile may legally set venue-owned grid fields that a live mount's hardcoded
    /// `GridSource::VenueFetched` then rejects on every venue arm.
    NotLiveMode(String),
}

/// **What this box actually does, folded from the three inputs that decide it.**
///
/// This is the value the arming-preservation proof compares — the OUTCOME, not the rows. Two stores
/// with completely different contents that both resolve to [`ArmingOutcome::Paper`] are the same
/// answer as far as this phase is concerned, and two that differ here are a migration that changed
/// what the box does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmingOutcome {
    /// `flags.tradehub_live` is off — the PAPER mount, byte-identical to the pre-live daemon: no
    /// `build_node`, no live feed, no credentials.
    Paper,
    /// The live arm is selected and the mount will refuse. **The daemon exits FAILURE and places no
    /// order ever.** Distinct from [`Self::Paper`] deliberately: a box in this state is not trading
    /// and is also not working, and a migration that moved it to [`Self::Live`] would be arming a
    /// mount that has never run.
    LiveRefused(LiveRefusal),
    /// The live arm, armed. Real orders MAY be placed on any venue whose credentials are in the
    /// store.
    Live {
        /// The PRIMARY mount's `venue/symbol`, resolved through
        /// [`DaemonProfile::primary_mount`] — the daemon's singular identity.
        primary: String,
        /// Every mount's `venue/symbol`, in row order. What the operator asked to mount; NOT the
        /// set that arms (`vike_run::build_node`'s own `live_venues` record answers that, and the
        /// two are measured to differ by eight venues on the live box).
        mounts: Vec<String>,
    },
}

/// The `[risk]` keys a LIVE mount refuses to start without, and which of them this profile is
/// missing.
///
/// **Derived, never restated**: the set is every `vike_config::ceilings::PRE_TRADE_CEILINGS` row
/// whose home is the run profile's `[risk]` table and whose `refuses_live_mount_when_absent` is
/// true. That table is the tree's declared authority for this exact question — its
/// `absent_means` column spells the outcome in words (*"it REFUSES THE MOUNT pre-connect
/// (`MountError::MissingRiskBudget`) and the daemon exits FAILURE"*) — so a new refusing ceiling
/// joins this answer by joining that table, and [`REFUSING_RISK_KEYS`] is the exhaustiveness gate.
#[must_use]
pub fn live_risk_budget_missing(risk: Option<&vike_exec::ProfileRisk>) -> Vec<&'static str> {
    PRE_TRADE_CEILINGS
        .iter()
        .filter(|c| c.home == CeilingHome::RunProfileRisk && c.refuses_live_mount_when_absent)
        .filter(|c| !risk_key_is_set(c.name, risk))
        .map(|c| c.name)
        .collect()
}

/// Every `[risk]` key [`risk_key_is_set`] knows how to read.
///
/// The exhaustiveness gate for [`live_risk_budget_missing`]: a ceiling that refuses a live mount and
/// is NOT in this list would read as permanently missing, which turns every live box into
/// [`ArmingOutcome::LiveRefused`] — fail-safe in direction and catastrophically wrong as an answer.
/// `crates/vike-tradehub/tests/daemon/profile_rows.rs`'s
/// `every_refusing_risk_ceiling_has_a_reader` compares the two.
pub const REFUSING_RISK_KEYS: [&str; 2] = ["max_notional_per_order", "max_total_exposure"];

/// Is one named `[risk]` key set on this profile?
///
/// An ABSENT profile answers `false` for every key, which is exactly
/// `vike_mount::require_live_risk_budget`'s verdict when `make_engine`'s `risk_profile` argument is
/// `None`: every key missing, `profile_supplied: false`, the mount refused.
fn risk_key_is_set(name: &str, risk: Option<&vike_exec::ProfileRisk>) -> bool {
    let Some(r) = risk else { return false };
    match name {
        "max_notional_per_order" => r.max_notional_per_order.is_some(),
        "max_total_exposure" => r.max_total_exposure.is_some(),
        // ⚠ An unknown refusing ceiling reads as MISSING rather than as satisfied. That is the
        // fail-safe direction (a box refuses to start rather than trades uncapped) and it is also
        // wrong as an answer, which is why `REFUSING_RISK_KEYS` is gated rather than trusted.
        _ => false,
    }
}

/// **Fold the three inputs into the one answer.** The daemon calls this at startup to disclose what
/// it is about to do; the arming-preservation proof calls it to compare before and after.
///
/// * `live` — `flags.tradehub_live`, resolved by `vike_config::load` exactly as the daemon resolves
///   it (env > file > default).
/// * `run_profile` — whatever `vike_core::resolve_profile` returned for the winning selection.
/// * `profile` — the `DaemonProfile` the winning selection loaded.
#[must_use]
pub fn resolve_arming(
    live: bool,
    run_profile: Option<&RunProfile>,
    profile: &DaemonProfile,
) -> ArmingOutcome {
    if !live {
        return ArmingOutcome::Paper;
    }
    // The LIVE arm asks `risk_for_live_venue_mount`, not `.risk` — the same production call the
    // daemon's live arm makes, and the reason is that function's own doc: a `paper`/`backtest`
    // profile's `[risk]` table is REFUSED for a live venue mount rather than merged.
    let risk = match run_profile {
        None => None,
        Some(p) => match p.risk_for_live_venue_mount() {
            Ok(r) => Some(r),
            Err(e) => return ArmingOutcome::LiveRefused(LiveRefusal::NotLiveMode(e.to_string())),
        },
    };
    let missing = live_risk_budget_missing(risk);
    if !missing.is_empty() {
        return ArmingOutcome::LiveRefused(LiveRefusal::MissingRiskBudget(missing));
    }
    let rows = profile.mount_rows();
    let mounts: Vec<String> =
        rows.iter().map(|p| format!("{}/{}", p.venue(), p.mount_symbol())).collect();
    let primary = mounts.get(profile.primary_mount().index()).cloned().unwrap_or_default();
    ArmingOutcome::Live { primary, mounts }
}

// ---------------------------------------------------------------------------------------------
// The body, as rows
// ---------------------------------------------------------------------------------------------

/// **The asset-class vocabulary the mount schema's `CHECK` is rendered from.**
///
/// `vike_secrets` is a zero-`vike-*`-dependency leaf at layer 15 and cannot name a layer-20 type, so
/// `vike_secrets::profile_store::profile_ddl` spells no asset-class word of its own and takes the
/// list as a parameter — see its doc. **This is the one production site that supplies it**, and the
/// list it supplies is generated from `AssetClass`'s single declaration, so there is no second list
/// to keep in step: adding a variant changes the schema's `CHECK` with no edit here or there.
///
/// `mount_asset_class_vocabulary_is_the_enums_own` is the pin.
#[must_use]
pub fn mount_asset_class_vocabulary() -> &'static [&'static str] {
    vike_model::AssetClass::SQL_WORDS
}

/// Parse a mount's declared asset class, refusing an unknown word BY NAME.
///
/// A word outside the vocabulary is what the schema's `CHECK` would have refused anyway; naming it
/// here means the operator is told which key is wrong rather than being handed a SQL constraint
/// error, and it means the refusal happens before anything is written.
///
/// # Errors
///
/// A message naming the word and the vocabulary.
pub fn parse_mount_asset_class(word: &str) -> Result<vike_model::AssetClass, String> {
    vike_model::AssetClass::from_sql_word(word).ok_or_else(|| {
        format!(
            "mount `asset_class = {word:?}` is not one of the asset classes vike knows. The \
             permitted words are: {}",
            mount_asset_class_vocabulary().join(", ")
        )
    })
}

/// **A `DaemonProfile` as store rows.** The body only — never the `active` bit, which
/// `vike_secrets::profile_store::store_profile` refuses to take from here.
///
/// The `[daemon]` table becomes two `profile_setting` rows at their dotted paths, which is §4.8's
/// shape (*"the daemon profile's summary and shutdown-deadline keys"*).
///
/// # Errors
///
/// ⚠ **A mount that does not name its `asset_class` is REFUSED, and nothing is stored.** That is
/// 0061 Phase 5's *required, not nullable* ruling arriving at the one seam that can enforce it: the
/// TOML key is optional so a profile written before it existed keeps PARSING (the live daemon's own
/// is one), and this is where the bill comes due. The message names the mount and the vocabulary, so
/// the operator's act is to add one line — which is exactly the *"costs one value"* that made the
/// column mandatory rather than nullable.
///
/// Also errors on a word outside the vocabulary — see [`parse_mount_asset_class`].
pub fn daemon_profile_to_rows(
    name: &str,
    profile: &DaemonProfile,
) -> Result<StoredProfile, String> {
    let mut mounts = Vec::new();
    let mut params: BTreeMap<(i64, String), String> = BTreeMap::new();
    let declared_primary = profile.primary_mount();
    for (i, row) in profile.mount_rows().into_iter().enumerate() {
        let ord = i64::try_from(i).unwrap_or(i64::MAX);
        let class = match row.asset_class.as_deref() {
            Some(word) => parse_mount_asset_class(word)?,
            None => {
                return Err(format!(
                    "profile `{name}` mount {i} (venue `{}`, symbol `{}`) does not declare \
                     `asset_class`, and a mount ROW must say which product it trades — \
                     docs/decisions/0061 phase 5. Add one line — `asset_class = \"CryptoPerp\"` — \
                     to that mount's `[[mounts]]` table, or at the top level if this profile uses \
                     the single-mount spelling. The permitted words are: {}",
                    row.venue(),
                    row.mount_symbol(),
                    mount_asset_class_vocabulary().join(", ")
                ));
            }
        };
        let mut m = MountRow::new(ord, row.venue(), class.sql_word());
        // ⚠ The primary is written as a DECLARATION only when the profile made one. An implicit
        // first row is stored with `is_primary = 0` on every row, so the store carries the same
        // "nobody chose" state the file carries and `StoredProfile::primary` answers
        // `ImplicitFirst` — the migration does not promote an accident into a decision.
        m.is_primary = matches!(declared_primary, PrimaryMount::Declared(d) if d == i);
        // Exactly one of the two symbol spellings, which the schema CHECKs. `token_id` is kept for
        // a profile that used it, because that is what every shipped polymarket profile says.
        if row.token_id.is_some() {
            m.token_id = row.token_id.clone();
        } else {
            m.symbol = Some(row.mount_symbol().to_string());
        }
        m.interval = row.interval.clone();
        m.interval_ms = row.interval_ms;
        m.resolution_ts_ms = row.resolution_ts_ms;
        m.qty = row.qty;
        m.half_spread = row.half_spread;
        m.tick_size = row.tick_size;
        m.seed_cash = row.seed_cash;
        m.data_only = row.data_only;
        m.account = row.account.as_ref().and_then(|l| l.text().map(str::to_string));
        if let Some(s) = &row.strategy {
            m.strategy_name = s.name.clone();
            m.strategy_rhai = s.rhai.clone();
            if let Some(t) = s.params.as_table() {
                for (k, v) in t {
                    params.insert((ord, k.clone()), v.to_string());
                }
            }
        }
        mounts.push(m);
    }
    let mut settings = BTreeMap::new();
    settings.insert("daemon.summary_ms".to_string(), profile.daemon.summary_ms.to_string());
    settings.insert(
        "daemon.shutdown_deadline_ms".to_string(),
        profile.daemon.shutdown_deadline_ms.to_string(),
    );
    Ok(StoredProfile {
        row: ProfileRow {
            name: name.to_string(),
            kind: ProfileKind::Daemon,
            active: false,
            note: None,
        },
        mounts,
        params,
        settings,
        // A daemon profile has no recorder body, and this states it by COLUMN SET rather than by
        // kind word — `read_recorder` asks the table, so the two can never disagree.
        recorder: None,
    })
}

/// **Store rows back into a `DaemonProfile`, THROUGH THE EXISTING PARSER.**
///
/// 0057's *What is LOST* section requires exactly this: *"Store each value as the TOML rendering of
/// one scalar, assemble a synthetic document, and run it through the existing parse-and-apply path.
/// No second validator is written."* So this function renders a document — through
/// [`vike_secrets::profile_store::render_daemon_toml`], the ONE renderer — and hands it to
/// `DaemonProfile::from_toml_str`, which means every refusal the file path has (the symbol
/// mutual-exclusion, the strategy capability gate, the params refusals, the duplicate-mount-id
/// check, `deny_unknown_fields`, the two-declared-primaries refusal) applies to a row-loaded
/// profile unchanged.
///
/// ⚠ **The rendering MOVED DOWN into `vike-secrets` and this is now two lines**, which is not a
/// tidy-up: `vike-cli` links no `vike-tradehub`, so a migration that writes daemon rows could not
/// reach a renderer up here — and without one it cannot run the round-trip fence that makes writing
/// this body safe. The single-mount spelling that the moved renderer now emits (and the
/// state-sidecar defect that made it mandatory) is argued at [`render_daemon_toml`].
///
/// # Errors
///
/// Whatever the renderer refuses (a `profile_setting` path outside `daemon.`), or whatever the
/// loader refuses, verbatim.
pub fn rows_to_daemon_profile(stored: &StoredProfile) -> Result<DaemonProfile, String> {
    DaemonProfile::from_toml_str(&render_daemon_toml(stored)?)
}

/// **Store rows back into a `RunProfile`, THROUGH THE EXISTING PARSER** — the exact twin of
/// [`rows_to_daemon_profile`], and the READER 0057's Phase 2 never had.
///
/// [`vike_secrets::profile_store::render_run_toml`] assembles the document and
/// `RunProfile::from_toml_str` parses AND validates it, so a row-loaded run profile meets every
/// refusal the file meets: `deny_unknown_fields` on all seven structs (which is what catches a
/// `profile_setting` path naming nothing real — see that renderer's doc for why it carries no
/// vocabulary of its own), the `[event_source]`/`[broker]` tombstones, the `mode = "live"`
/// venue-owned grid refusal, `max_leverage >= 1.0`, `required_free_bp_pct` in `[0.0, 1.0)`. **No
/// second validator is written**, and none may be.
///
/// ⚠ The caller must treat an `Err` as a HARD startup failure, never as "no run profile". Falling
/// through to the file rung would make a corrupt row indistinguishable from an absent one, and an
/// absent one is what a live mount REFUSES on — so the fall-through would convert a refusal into a
/// mount running on the file's ceilings while the operator believes the row is in force.
///
/// # Errors
///
/// Whatever the renderer or the loader refuses, verbatim.
pub fn rows_to_run_profile(stored: &StoredProfile) -> Result<RunProfile, String> {
    RunProfile::from_toml_str(&render_run_toml(stored)).map_err(|e| e.to_string())
}

/// **The WAL sink, from the run profile this daemon actually RESOLVED — not from a second read of
/// whatever `VIKE_RUN_PROFILE` still points at.**
///
/// ⚠ **This exists because the row rung would otherwise make the startup disclosure a lie.**
/// `vike_core::journal_config_from` SHORT-CIRCUITS on `VIKE_RUN_PROFILE`: it re-opens the file that
/// variable names and reads `[sinks].journal` out of it. With a `run` row active and the `.env`
/// line still in place — which is exactly the state the migration leaves a box in, deliberately, so
/// the file rung stays available as a rollback — the daemon prints *"SHADOWING `…run-live.toml`
/// (the environment variable), which no longer decides anything"* while that file goes on deciding
/// the journal sink. Positive confirmation of something false, in the one disclosure the owner's
/// ruling added to prevent it.
///
/// So the resolved profile answers when there is one, and the variable map answers only when there
/// is not.
///
/// ⚠ **On every box that has not crossed this is BYTE-IDENTICAL**, which is measurable rather than
/// hoped: `journal_config_from`'s own profile rung is literally `choose_journal(Some(&p), None,
/// None)`, and `Sinks::journal_config` is the same expression. It differs only where a box passes
/// `--profile` (or activates a row) with NO `VIKE_RUN_PROFILE` set while ALSO setting
/// `VIKE_JOURNAL_DIR` or `config.journal_dir` — a combination no shipped unit is in
/// (`deploy/vike-tradehub.service` comments its `Environment=VIKE_JOURNAL_DIR=` out) and which
/// would previously have journalled to a directory the profile never named.
///
/// ⚠ **That combination used to be described here as one nobody is in, and then left SILENT — and
/// the migration's own completion path walks a box straight into it.** Activate the run row, drop
/// the now-shadowed `VIKE_RUN_PROFILE=` line (which is what the disclosure tells the operator to
/// do), and a box that was journalling from `VIKE_JOURNAL_DIR` / `config.journal_dir` has a
/// resolved profile whose `[sinks]` names no `journal` — so this function answers `None`, the
/// directory rung is never consulted, and the write-ahead command journal is gone with no warning
/// and no log line. The DECISION is still the profile's, which is `choose_journal`'s documented
/// and tested rule (*"a profile can never be silently overridden"*, and a profile that names no
/// journal genuinely means "off"); what was missing was the word SILENTLY.
/// [`journal_rung_shadowed`] is that word, and `crate::tradehub_cli`'s `run` emits it once.
#[must_use]
pub fn journal_config_for(
    profile: Option<&RunProfile>,
    vars: &std::collections::HashMap<String, String>,
) -> Option<vike_core::JournalConfig> {
    match profile {
        Some(p) => p.sinks.journal_config(),
        None => vike_core::journal_config_from(vars),
    }
}

/// **The line [`journal_config_for`] owes an operator whose `VIKE_JOURNAL_DIR` rung it just made
/// unreachable** — `None` when the two rungs cannot disagree, which is every box that has not
/// crossed and every CI lane.
///
/// It fires on exactly the state where THIS BINARY's answer differs from the one
/// `vike_core::journal_config_from` would have given over the same map:
///
/// * a run profile is RESOLVED (from a row, or from `--profile`), and
/// * `vars` carries no `VIKE_RUN_PROFILE` — with that variable set, `journal_config_from`
///   short-circuits on it and the directory was already deciding nothing, so reporting it would be
///   reporting a state that predates the row plane rather than one this landing created, and
/// * `vars` carries a non-empty `VIKE_JOURNAL_DIR` — which on this daemon means the variable OR
///   `config.journal_dir` folded into it by `crate::tradehub_cli`'s `journal_vars`.
///
/// Both directions are reported, because both are changes an operator can be wrong about: a
/// profile with no `[sinks.journal]` turns the journal OFF where the directory turned it on, and a
/// profile that names one writes somewhere the directory does not.
///
/// ⚠ It is a NOTE, not a fallback. Falling through to the directory would make a profile
/// overridable by the environment, which is the property `vike_core::run_profile`'s
/// `choose_journal` is written and tested to deny.
#[must_use]
pub fn journal_rung_shadowed(
    profile: Option<&RunProfile>,
    vars: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let p = profile?;
    if vars.get("VIKE_RUN_PROFILE").is_some_and(|v| !v.trim().is_empty()) {
        return None;
    }
    // The CONSTANT, never the literal — `journal_vars_from`'s own note in `crate::tradehub_cli`
    // gives the reason: `vike_config` owns this variable's spelling and the settings registry
    // resolves a read through the indirection. (`VIKE_RUN_PROFILE` above has no such constant and
    // is spelled the way `select_run_profile` already spells it in this file.)
    let dir = vars
        .get(vike_config::config::JOURNAL_DIR_ENV)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    Some(match p.sinks.journal_config() {
        None => format!(
            "the write-ahead command journal is OFF. A run profile is RESOLVED and its [sinks] \
             names no `journal`, and the profile decides — so `{dir}` (VIKE_JOURNAL_DIR, or \
             config.journal_dir folded into it) is not consulted and no longer turns the journal \
             on. Give the profile a [sinks.journal] table if you want it back — `vike-cli config \
             mirror --profile <file>` carries that table into the row — or \
             `vike-cli config deactivate run`, which hands the decision back to the file and \
             directory rungs."
        ),
        Some(c) => format!(
            "the write-ahead command journal follows the RESOLVED run profile and writes to {}. \
             `{dir}` (VIKE_JOURNAL_DIR, or config.journal_dir folded into it) is not consulted \
             while a profile is resolved, so it names no directory this daemon writes to.",
            c.dir.display()
        ),
    })
}

// ---------------------------------------------------------------------------------------------
// The migration, and its fence
// ---------------------------------------------------------------------------------------------

/// What a migration of ONE profile would do.
#[derive(Debug, Clone, PartialEq)]
pub struct MigrationPlan {
    /// The body that will be stored. Always stored — a body is not a selection.
    pub body: StoredProfile,
    /// Whether an `active` row will be written, and if not, why not.
    pub active: ActivePlan,
}

/// **Plan a daemon-profile migration.** The BODY always lands; the ACTIVE row lands only when
/// writing it cannot change what this box does.
///
/// * `name` — the name the body is stored under.
/// * `profile` — the parsed `DaemonProfile`.
/// * `in_force` — the profile name that is selected TODAY with the store's rung removed, i.e. the
///   name the `--config` path maps to, or `None` when nothing does.
/// * `already_active` — what the store's `active` row holds for this kind before the migration.
///
/// The decision itself is `vike_secrets::profile_store::plan_active_row`, which is the one place in
/// the tree that may decide to write an active row. This function exists to make sure the daemon
/// half calls it with the right three arguments rather than to re-decide anything.
///
/// # Errors
///
/// Whatever [`daemon_profile_to_rows`] refuses — today, a mount that does not declare its
/// `asset_class`. ⚠ The refusal comes back as a PLAN that does not exist rather than as a plan that
/// stores less: a migration is all-or-nothing, and a body missing the one column the schema requires
/// could not have been stored anyway.
pub fn plan_migration(
    name: &str,
    profile: &DaemonProfile,
    in_force: Option<&str>,
    already_active: Option<&str>,
) -> Result<MigrationPlan, String> {
    Ok(MigrationPlan {
        body: daemon_profile_to_rows(name, profile)?,
        active: plan_active_row(in_force, already_active, name),
    })
}
