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
    plan_active_row, select,
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

/// Render one f64 the way TOML spells it, so a value that round-trips through the store is the same
/// value. `{:?}` on an `f64` is Rust's shortest round-tripping form and always carries a `.`, which
/// is what keeps `20.0` from coming back as the integer `20` and changing the parsed type.
fn toml_f64(v: f64) -> String {
    let s = format!("{v:?}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

/// Quote a string as a TOML basic string. The inputs here are venue ids, symbols, intervals, account
/// labels and script paths; escaping is minimal and deliberate rather than general, and anything
/// that would need more than this is refused by the loader the rendered document is handed to.
fn toml_str(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

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
    vike_catalog::AssetClass::SQL_WORDS
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
pub fn parse_mount_asset_class(word: &str) -> Result<vike_catalog::AssetClass, String> {
    vike_catalog::AssetClass::from_sql_word(word).ok_or_else(|| {
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
/// No second validator is written."* So this function renders a document and hands it to
/// `DaemonProfile::from_toml_str`, which means every refusal the file path has — the symbol
/// mutual-exclusion, the strategy capability gate, the params refusals, the duplicate-mount-id
/// check, `deny_unknown_fields` — applies to a row-loaded profile unchanged, including the NEW
/// two-declared-primaries refusal.
///
/// ⚠ It ALWAYS renders the `[[mounts]]` spelling, even for a one-mount profile, and that is a
/// deliberate behaviour difference worth naming rather than hiding: a `[[mounts]]` profile mounts
/// under `DaemonProfile::derived_controller_id` where a single-mount profile keeps the legacy
/// `{venue}__{symbol}__{interval}` triple. A deployment whose selection MOVES to a row therefore
/// gets a different state-sidecar key, which is why [`plan_migration`] never moves a selection.
///
/// # Errors
///
/// Whatever the loader refuses, verbatim.
pub fn rows_to_daemon_profile(stored: &StoredProfile) -> Result<DaemonProfile, String> {
    let mut doc = String::new();
    // ONE `[daemon]` header, whatever the key count: a second header for the same table is a TOML
    // error, so the keys are gathered first and the header written once.
    let mut daemon_keys: Vec<(&str, &String)> = Vec::new();
    for (path, value) in &stored.settings {
        match path.strip_prefix("daemon.") {
            Some(key) => daemon_keys.push((key, value)),
            // ⚠ A setting path this renderer does not know is an ERROR, never a silent drop. A
            // dropped key is the declared-but-unread failure arriving through the store instead of
            // through a file, and it would read as a successful migration.
            None => {
                return Err(format!(
                    "profile `{}` carries setting `{path}`, which this renderer does not know how \
                     to put back into a daemon profile document. Nothing was loaded: a key that \
                     cannot be rendered must not be silently dropped, because the profile would \
                     then mount at a default nobody typed",
                    stored.row.name
                ));
            }
        }
    }
    if !daemon_keys.is_empty() {
        doc.push_str("[daemon]\n");
        for (key, value) in daemon_keys {
            doc.push_str(&format!("{key} = {value}\n"));
        }
    }
    for m in &stored.mounts {
        doc.push_str("\n[[mounts]]\n");
        doc.push_str(&format!("venue = {}\n", toml_str(&m.venue)));
        // ⚠ NOT conditional, unlike every optional key below it: the column is NOT NULL, so a row
        // always carries one and a rendered document that omitted it would round-trip back to a
        // profile that no longer says what it trades — which is the "missing claim becomes a legal
        // value" hazard 0061 made the column mandatory to avoid.
        doc.push_str(&format!("asset_class = {}\n", toml_str(&m.asset_class)));
        if let Some(s) = &m.symbol {
            doc.push_str(&format!("symbol = {}\n", toml_str(s)));
        }
        if let Some(t) = &m.token_id {
            doc.push_str(&format!("token_id = {}\n", toml_str(t)));
        }
        if let Some(v) = &m.interval {
            doc.push_str(&format!("interval = {}\n", toml_str(v)));
        }
        if let Some(v) = m.interval_ms {
            doc.push_str(&format!("interval_ms = {v}\n"));
        }
        if let Some(v) = m.resolution_ts_ms {
            doc.push_str(&format!("resolution_ts_ms = {v}\n"));
        }
        if let Some(v) = m.qty {
            doc.push_str(&format!("qty = {}\n", toml_f64(v)));
        }
        if let Some(v) = m.half_spread {
            doc.push_str(&format!("half_spread = {}\n", toml_f64(v)));
        }
        if let Some(v) = m.tick_size {
            doc.push_str(&format!("tick_size = {}\n", toml_f64(v)));
        }
        if let Some(v) = m.seed_cash {
            doc.push_str(&format!("seed_cash = {}\n", toml_f64(v)));
        }
        if let Some(v) = m.data_only {
            doc.push_str(&format!("data_only = {v}\n"));
        }
        if let Some(v) = &m.account {
            doc.push_str(&format!("account = {}\n", toml_str(v)));
        }
        if m.is_primary {
            doc.push_str("primary = true\n");
        }
        if m.strategy_name.is_some() || m.strategy_rhai.is_some() {
            doc.push_str("\n[mounts.strategy]\n");
            if let Some(v) = &m.strategy_name {
                doc.push_str(&format!("name = {}\n", toml_str(v)));
            }
            if let Some(v) = &m.strategy_rhai {
                doc.push_str(&format!("rhai = {}\n", toml_str(v)));
            }
            let mine: Vec<(&String, &String)> = stored
                .params
                .iter()
                .filter(|((o, _), _)| *o == m.ord)
                .map(|((_, k), v)| (k, v))
                .collect();
            if !mine.is_empty() {
                doc.push_str("\n[mounts.strategy.params]\n");
                for (k, v) in mine {
                    doc.push_str(&format!("{k} = {v}\n"));
                }
            }
        }
    }
    DaemonProfile::from_toml_str(&doc)
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
