//! `vike-cli config mirror --profile <file>` / `--daemon <file>` — **the RUN profile and the DAEMON
//! profile become rows, and the migration proves it changed nothing before it writes.**
//!
//! This is the half `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s
//! Phase 3 shipped without. That phase landed the store, the schema, the selection resolver
//! (`vike_secrets::profile_store::select` — the owner's *the ROW wins* ruling written once) and the
//! fence, **with no verb that wrote a row**; `crates/vike-ops/tests/profile_writer_gate.rs` recorded
//! that as a measurement rather than a disabled gate. The recorder migration was the first writer.
//! These two are the second and third, and between them they are the last two documents in
//! `<project>/settings/` that a daemon still reads off disk.
//!
//! # ⚠ THE WRITER WAS HALF-BUILT BECAUSE IT WAS BUILT ON THE WRONG PLANE
//!
//! There were two independent "profile in the database" mechanisms in this tree, and the old
//! `--profile` flag wrote to the one that cannot bind:
//!
//! * **`profile_risk`** — a DISCLOSURE mirror of one `[risk]` table, keyed by FILE NAME, carrying no
//!   `mode`, no `[guards]`, no `[sinks]` and **no `active` column**. Its own help said *"Nothing
//!   reads the rows: the profile FILE still judges every order"*, and that was not an oversight: a
//!   reader was forbidden by name (`crates/vike-ops/tests/profile_risk_readers_gate.rs`). **Building
//!   one was a test failure by design.**
//! * **`profile_store`** — the Phase-3 plane, with `ProfileKind::{Daemon, Run, Recorder}`, a per-kind
//!   `active` column behind a partial unique index, the selection ruling, the arming fence — and,
//!   decisively, a daemon that **already reads it at boot** for its `daemon` kind.
//!
//! So both files migrate onto the SECOND plane, and the first is retired (its rows stay on the two
//! boxes that carry them; nothing in this tree reads or writes them any more). The run profile's
//! `[risk]` table arrives as `profile_setting` rows at `risk.*`, beside `mode`, `sinks.*` and
//! `guards.*` — one document, one body, one row that says whether it is live.
//!
//! # THE FENCE
//!
//! Taken verbatim from `crate::cmd::config_mirror_recorder`, and for a sharper reason here:
//!
//! > the rows are written only when RENDERING THEM BACK produces a document that parses EQUAL to
//! > the file they came from.
//!
//! A run profile holds the **pre-trade risk ceilings of a live trading daemon**, and its own comment
//! on the live box says what they are for: *"A live mount REFUSES TO START without it."* A daemon
//! profile decides which venue and which clip size a labelled account trades. Comparing typed values
//! would not be enough — both types apply `serde` defaults, so two profiles can be equal as values
//! while one dropped a key the migration failed to carry. Comparing the parsed DOCUMENTS catches
//! exactly that, and it is why every column on the row side means ABSENT rather than "the default".
//!
//! It is also what catches the hazard the daemon renderer declared and could not fix alone: a
//! `[[mounts]]` rendering of the CI box's single-mount file does not parse equal to it, and the two mount
//! under different state-sidecar keys. See `vike_secrets::profile_store::render_daemon_toml`.
//!
//! # …and NO active row is ever written here
//!
//! `plan_active_row` is consulted and REPORTED, and on a box where the CLI has no evidence of what
//! is in force it answers `Withhold { NothingSelectedToday }`. Storing a body is not selecting one.
//! Selection is `vike-cli config activate`, a separate and deliberate verb —
//! `crate::cmd::config_activate`.
//!
//! ⚠ **Why the mirror may not sniff the environment and activate for you.** `config mirror` runs in
//! an operator shell where `VIKE_RUN_PROFILE` (which reaches the daemon through the unit's
//! `EnvironmentFile=`) and the daemon's own `--config` argv are not visible. Having the mirror pick
//! a profile out of ITS OWN environment would be 0057's Question 3 answered sideways in the one verb
//! that writes — the same argument that made the old `--profile` flag explicit rather than defaulted.

use std::collections::BTreeMap;
use std::path::Path;

use vike_secrets::profile_store::{
    ActivePlan, MountRow, OperatorWrite, ProfileKind, ProfileRow, StoredProfile, plan_active_row,
    read_profiles, render_daemon_toml, render_run_toml, store_profile,
};

/// Which document is being lowered. The two share every piece of machinery here except their key
/// vocabulary and their renderer, so the difference is a parameter rather than two modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// `settings/run-live.toml` — `vike_core::RunProfile`.
    Run,
    /// `settings/tradehub.toml` — `vike_tradehub::config::DaemonProfile`.
    Daemon,
}

impl Plane {
    /// The store kind a body of this plane is written under.
    #[must_use]
    pub fn kind(self) -> ProfileKind {
        match self {
            Plane::Run => ProfileKind::Run,
            Plane::Daemon => ProfileKind::Daemon,
        }
    }

    /// The flag that mirrors it, for a refusal that names the operator's own command line.
    fn flag(self) -> &'static str {
        match self {
            Plane::Run => "--profile",
            Plane::Daemon => "--daemon",
        }
    }
}

/// What a mirror of one profile produced. **Nothing has been written when this exists** — [`write`]
/// is the separate call, so `--dry-run` can stop between them.
#[derive(Debug)]
pub struct Mirrored {
    /// The rows, ready to store.
    pub stored: StoredProfile,
    /// What [`plan_active_row`] decided about this kind's active row. Always a `Withhold` on a box
    /// this CLI has no in-force evidence for, which is every box.
    pub active: ActivePlan,
    /// One human line per row group, for the report.
    pub summary: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// The RUN profile
// ---------------------------------------------------------------------------------------------

/// The two TOMBSTONED tables `vike_core::RunProfile::validate` refuses BY NAME. They are refused
/// HERE too, rather than being stored and refused at the next boot, because a migration that stored
/// them would hand the operator a row for a table the daemon deletes — positive confirmation of
/// something false, arriving through the store instead of through the file.
const RUN_TOMBSTONES: [&str; 2] = ["event_source", "broker"];

/// **Lower a RUN profile's TOML into rows, and REFUSE unless rendering them back reproduces it.**
///
/// Pure over text: nothing is written by this function in any case.
///
/// ⚠ **It works over RAW TOML and never over a parsed `RunProfile`, and that is not a style
/// choice.** An absent key must be an absent ROW — the rule every nullable column on this plane is
/// built on — and a parsed profile has already applied `serde` defaults, so lowering one would write
/// rows for `sinks.gui`, `guards.initial_trading_state`, `risk.window_ms` and
/// `risk.required_free_bp_pct` that the operator's file does not carry, and the round trip would
/// then fail for every profile there is. (`vike-cli` also links neither `vike-core` nor `vike-exec`
/// — the `light-consumers` lane holds it out of that closure — so the parsed type is unavailable
/// here anyway.)
///
/// # Errors
///
/// A `String` naming what could not be carried: a tombstoned table, a `[risk]` key outside
/// `vike_config::PROFILE_RISK_KEYS` or one whose scalar shape disagrees with its row, or a
/// round-trip difference.
pub fn rows_from_run_profile_text(name: &str, text: &str) -> Result<StoredProfile, String> {
    let doc: toml::Value =
        toml::from_str(text).map_err(|e| format!("run profile does not parse: {e}"))?;
    let table = doc.as_table().ok_or("run profile is not a TOML table")?;

    for dead in RUN_TOMBSTONES {
        if table.contains_key(dead) {
            return Err(format!(
                "run profile carries `[{dead}]`, which is no longer part of a run profile — \
                 `vike_core::RunProfile::validate` refuses it BY NAME and this profile would fail \
                 at startup too. Nothing was written. Delete the whole `[{dead}]` table; a \
                 migration that stored it would hand you a ROW for a table the daemon deletes, \
                 which reads as more authoritative than the file line it replaced."
            ));
        }
    }

    // The `[risk]` vocabulary and each key's scalar SHAPE, reused from the roster that is already
    // gated against `vike_exec::ProfileRisk`'s own fields
    // (`crates/vike-config/tests/profile_risk.rs`). Reused rather than re-spelled: a second list
    // here would be free to disagree with the type that judges orders.
    //
    // ⚠ The mirror must never be STRICTER than the boot. `ProfileRiskKey::accepts` lets a `Float`
    // key take a TOML integer because `max_leverage = 3` is a profile the daemon starts on
    // perfectly well, and that asymmetry is MEASURED rather than assumed — see its own doc.
    match table.get(vike_config::RISK_TABLE) {
        None => {}
        Some(toml::Value::Table(risk)) => {
            for (key, value) in risk {
                let Some(row) = vike_config::profile_risk_key(key) else {
                    return Err(format!(
                        "run profile carries `[risk] {key}`, which is not a `vike_exec::ProfileRisk` \
                         field — that type is `deny_unknown_fields`, so this profile would fail at \
                         startup too. Nothing was written. Known `[risk]` keys: {}",
                        vike_config::PROFILE_RISK_KEYS
                            .iter()
                            .map(|k| k.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                };
                if !row.accepts(value) {
                    return Err(format!(
                        "run profile's `[risk] {key}` must be a {}; the profile's own parser \
                         refuses this value too. Nothing was written.",
                        row.kind.as_str()
                    ));
                }
            }
        }
        Some(_) => {
            return Err(
                "run profile's `risk` must be a TOML TABLE — this profile would fail at startup \
                 too. Nothing was written."
                    .to_string(),
            );
        }
    }

    let mut settings = BTreeMap::new();
    flatten_into(table, "", &mut settings)?;
    let stored = StoredProfile {
        row: ProfileRow {
            name: name.to_string(),
            kind: ProfileKind::Run,
            active: false,
            note: None,
        },
        mounts: Vec::new(),
        params: BTreeMap::new(),
        settings,
        recorder: None,
    };
    fence(Plane::Run, name, text, &doc, &render_run_toml(&stored))?;
    Ok(stored)
}

/// Flatten a TOML table into `profile_setting` paths. A leaf is any value that is not a table; its
/// stored form is `toml::Value`'s own rendering, which is the idiom `profile_setting.value` has
/// carried since Phase 3 and which the renderer emits verbatim.
///
/// ⚠ An EMPTY table produces no rows, so it does not survive the round trip and the fence refuses
/// the profile. That is the conservative direction and it is the same cost
/// `config_mirror_recorder` accepts and declares: an empty `[guards]` and an absent one mean the
/// same thing to `serde`, and the fence refuses a shape that would in fact have been harmless
/// rather than reasoning about which differences matter on a document that carries live ceilings.
/// The refusal prints both documents, so the remedy (delete the empty header) is visible in it.
fn flatten_into(
    table: &toml::Table,
    prefix: &str,
    out: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    for (key, value) in table {
        if key.contains('.') {
            return Err(format!(
                "the key `{key}` contains a `.`, and a `profile_setting` row addresses a leaf by \
                 its DOTTED PATH — storing it would make the row unaddressable. Nothing was \
                 written."
            ));
        }
        let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
        match value {
            toml::Value::Table(child) => flatten_into(child, &path, out)?,
            leaf => {
                out.insert(path, leaf.to_string());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The DAEMON profile
// ---------------------------------------------------------------------------------------------

/// Every key a MOUNT may carry, in either spelling. The write-path half of `deny_unknown_fields`,
/// spelled here because `vike-cli` cannot see `vike_tradehub::config` (it links no `vike-tradehub`,
/// and adding the edge would drag the whole daemon into the `light-consumers` lane's graph).
///
/// ⚠ It is not the authority and must not become one: `crates/vike-tradehub/src/config.rs` is. What
/// holds the two together is not this list but the ROUND TRIP — a key this table failed to carry
/// makes the rendered document differ from the file, and [`fence`] refuses the whole run. So a key
/// added upstream and forgotten here is a loud migration failure, never a silently dropped setting.
const MOUNT_KEYS: &[&str] = &[
    "venue",
    "asset_class",
    "symbol",
    "token_id",
    "interval",
    "interval_ms",
    "resolution_ts_ms",
    "qty",
    "half_spread",
    "tick_size",
    "seed_cash",
    "data_only",
    "account",
    "strategy",
    "primary",
];

/// The `[daemon]` table's keys — `vike_tradehub::config::DaemonSettings`.
const DAEMON_KEYS: &[&str] = &["summary_ms", "shutdown_deadline_ms"];

/// The `[strategy]` table's keys — `vike_tradehub::config::StrategyCfg`.
const STRATEGY_KEYS: &[&str] = &["name", "rhai", "params"];

/// **Lower a DAEMON profile's TOML into `mount` / `mount_param` / `profile_setting` rows, and
/// REFUSE unless rendering them back reproduces it.**
///
/// Pure over text. ⚠ Raw TOML again, and here the defect the parsed path has is measurable rather
/// than hypothetical: `DaemonSettings::summary_ms` and `shutdown_deadline_ms` are non-`Option` with
/// `serde` defaults, so the existing typed lowering
/// (`vike_tradehub::profile_rows::daemon_profile_to_rows`) writes both `daemon.*` rows for a file
/// that has no `[daemon]` table at all — and the round trip then fails for every such profile.
/// the CI box's file happens to carry both keys; nothing else does.
///
/// # Errors
///
/// A `String` naming what could not be carried — an unknown key, a wrongly-typed value, a mount
/// that declares no `asset_class` (`docs/decisions/0061` phase 5 made that column mandatory and this
/// is where the bill comes due), or a round-trip difference.
pub fn rows_from_daemon_profile_text(name: &str, text: &str) -> Result<StoredProfile, String> {
    let doc: toml::Value =
        toml::from_str(text).map_err(|e| format!("daemon profile does not parse: {e}"))?;
    let table = doc.as_table().ok_or("daemon profile is not a TOML table")?;

    let mut settings = BTreeMap::new();
    if let Some(d) = table.get("daemon") {
        let d = d.as_table().ok_or("`[daemon]` must be a table")?;
        for (key, value) in d {
            if !DAEMON_KEYS.contains(&key.as_str()) {
                return Err(unknown_key(&format!("daemon.{key}"), DAEMON_KEYS));
            }
            settings.insert(format!("daemon.{key}"), value.to_string());
        }
    }

    let mut mounts = Vec::new();
    let mut params: BTreeMap<(i64, String), String> = BTreeMap::new();
    match table.get("mounts") {
        // The `[[mounts]]` spelling.
        Some(list) => {
            for key in table.keys() {
                if !matches!(key.as_str(), "mounts" | "daemon") {
                    return Err(format!(
                        "daemon profile carries the top-level key `{key}` BESIDE a `[[mounts]]` \
                         array. Those are the two spellings of one thing and a profile uses one of \
                         them: with `[[mounts]]` present, every mount key belongs inside a mount \
                         table. Nothing was written."
                    ));
                }
            }
            let list = list.as_array().ok_or("`mounts` must be an array of tables")?;
            for (i, entry) in list.iter().enumerate() {
                let e = entry.as_table().ok_or("each `[[mounts]]` entry must be a table")?;
                let ord = i64::try_from(i).map_err(|_| "too many mounts".to_string())?;
                mounts.push(mount_row_from(name, ord, e, &mut params)?);
            }
        }
        // The single-mount spelling: the mount keys sit at the top level.
        None => {
            let ord = 0;
            let mut top = table.clone();
            top.remove("daemon");
            mounts.push(mount_row_from(name, ord, &top, &mut params)?);
        }
    }

    let stored = StoredProfile {
        row: ProfileRow {
            name: name.to_string(),
            kind: ProfileKind::Daemon,
            active: false,
            note: None,
        },
        mounts,
        params,
        settings,
        recorder: None,
    };
    let rendered = render_daemon_toml(&stored)?;
    fence(Plane::Daemon, name, text, &doc, &rendered)?;
    Ok(stored)
}

/// One mount table (either spelling) as a [`MountRow`], with its `[strategy.params]` folded into
/// `params`.
fn mount_row_from(
    profile: &str,
    ord: i64,
    t: &toml::Table,
    params: &mut BTreeMap<(i64, String), String>,
) -> Result<MountRow, String> {
    for key in t.keys() {
        if !MOUNT_KEYS.contains(&key.as_str()) {
            return Err(unknown_key(key, MOUNT_KEYS));
        }
    }
    // ⚠ The one value a migration CANNOT derive, and the operator must supply. `mount.asset_class`
    // is NOT NULL with a `CHECK` over the vocabulary, because 0061 phase 5 ruled that a mount which
    // does not say which product it trades is under-specified and a nullable column would make
    // "a missing claim becomes a legal value" permanent for this seam. The TOML key stays optional
    // so a profile written before it existed keeps PARSING; this is where the bill comes due.
    let asset_class = opt_str(t.get("asset_class"), "asset_class")?.ok_or_else(|| {
        format!(
            "profile `{profile}` mount {ord} (venue `{}`) does not declare `asset_class`, and a \
             mount ROW must say which product it trades — docs/decisions/0061 phase 5. Add one \
             line — `asset_class = \"CryptoPerp\"` — to that mount, or at the top level if this \
             profile uses the single-mount spelling. Nothing was written. This migration cannot \
             answer it for you: the file names no product, and guessing one would write a claim \
             nothing will ever disagree with. The permitted words are: {}",
            opt_str(t.get("venue"), "venue").ok().flatten().unwrap_or_default(),
            vike_model::AssetClass::SQL_WORDS.join(", ")
        )
    })?;
    if vike_model::AssetClass::from_sql_word(&asset_class).is_none() {
        return Err(format!(
            "profile `{profile}` mount {ord} declares `asset_class = {asset_class:?}`, which is \
             not an asset class vike knows. Nothing was written. The permitted words are: {}",
            vike_model::AssetClass::SQL_WORDS.join(", ")
        ));
    }
    // ⚠ **`venue` is REQUIRED HERE although the TOML key is optional**, and the refusal is explicit
    // rather than left to the round-trip fence. `MountCfg` defaults it to `"polymarket"`, while a
    // `mount` ROW always names its venue because a NULL could not be told from the default — so a
    // file that omits it can only be stored by WRITING the default into the row, and the rendered
    // document would then carry a `venue` line the operator never typed. The fence catches that
    // (the documents differ), but it catches it as a generic mismatch printing `venue = ""`, which
    // reads as a migration bug rather than as the one-line repair it is.
    let venue = opt_str(t.get("venue"), "venue")?.ok_or_else(|| {
        format!(
            "profile `{profile}` mount {ord} does not declare `venue`. The TOML key is optional — \
             it defaults to `polymarket` — but a mount ROW must name its venue, because a stored \
             NULL could not be told from that default. Nothing was written. Add one line: \
             `venue = \"polymarket\"` (or whichever venue this mount is really for)."
        )
    })?;
    let mut m = MountRow::new(ord, &venue, &asset_class);
    m.is_primary = t.get("primary").and_then(toml::Value::as_bool).unwrap_or(false);
    m.symbol = opt_str(t.get("symbol"), "symbol")?;
    m.token_id = opt_str(t.get("token_id"), "token_id")?;
    m.interval = opt_str(t.get("interval"), "interval")?;
    m.interval_ms = opt_int(t.get("interval_ms"), "interval_ms")?;
    m.resolution_ts_ms = opt_int(t.get("resolution_ts_ms"), "resolution_ts_ms")?;
    m.qty = opt_float(t.get("qty"), "qty")?;
    m.half_spread = opt_float(t.get("half_spread"), "half_spread")?;
    m.tick_size = opt_float(t.get("tick_size"), "tick_size")?;
    m.seed_cash = opt_float(t.get("seed_cash"), "seed_cash")?;
    m.data_only = match t.get("data_only") {
        None => None,
        Some(v) => Some(v.as_bool().ok_or("`data_only` must be a boolean")?),
    };
    m.account = opt_str(t.get("account"), "account")?;
    if let Some(s) = t.get("strategy") {
        let s = s.as_table().ok_or("`[strategy]` must be a table")?;
        for key in s.keys() {
            if !STRATEGY_KEYS.contains(&key.as_str()) {
                return Err(unknown_key(&format!("strategy.{key}"), STRATEGY_KEYS));
            }
        }
        m.strategy_name = opt_str(s.get("name"), "strategy.name")?;
        m.strategy_rhai = opt_str(s.get("rhai"), "strategy.rhai")?;
        if let Some(p) = s.get("params") {
            let p = p.as_table().ok_or("`[strategy.params]` must be a table")?;
            for (k, v) in p {
                params.insert((ord, k.clone()), v.to_string());
            }
        }
    }
    Ok(m)
}

// ---------------------------------------------------------------------------------------------
// Shared machinery
// ---------------------------------------------------------------------------------------------

/// **THE FENCE.** Render the rows back and require the result to PARSE EQUAL to the file they came
/// from.
///
/// A key the lowering failed to carry, a value it coerced, an array it reordered, a `[[mounts]]`
/// spelling where the file wrote top-level keys — all of them surface here, and all of them refuse
/// the whole run with both documents printed.
fn fence(
    plane: Plane,
    name: &str,
    text: &str,
    original: &toml::Value,
    rendered: &str,
) -> Result<(), String> {
    let back: toml::Value = toml::from_str(rendered).map_err(|e| {
        format!(
            "the rows rendered a document that does not parse ({e}). Nothing was written. This is \
             a defect in the migration, not in your profile — report it with the profile that \
             produced it.\n--- as the rows render it ---\n{rendered}"
        )
    })?;
    if &back != original {
        let what = match plane {
            Plane::Run => {
                "A run profile carries the PRE-TRADE RISK CEILINGS of a live mount — a live mount \
                 REFUSES TO START without them — so a body that is not what this box runs today is \
                 a daemon judging orders against numbers nobody typed."
            }
            Plane::Daemon => {
                "A daemon profile decides which venue, which instrument and which clip size a \
                 labelled account trades. It also decides the MOUNT IDENTITY: the single-mount and \
                 `[[mounts]]` spellings mount under different state-sidecar and journal-attribution \
                 keys, so a spelling difference below is not cosmetic. If the only difference is \
                 that the rows render `[[mounts]]` where your file writes top-level keys, add \
                 `primary = true` to the mount you mean, or accept the controller-id move \
                 deliberately."
            }
        };
        return Err(format!(
            "REFUSING to store {} profile `{name}`: the rows do not reproduce the profile they \
             came from, so storing them would change what this box does.\n\nNothing was written. \
             {what}\n\n--- the profile as given ---\n{}\n--- the profile as the rows render it \
             ---\n{rendered}",
            plane.kind().sql_word(),
            text.trim_end()
        ));
    }
    Ok(())
}

/// The unknown-key refusal, naming the accepted set — the same shape `serde`'s own
/// `deny_unknown_fields` message has, because this is that refusal moved to the write path.
fn unknown_key(key: &str, known: &[&str]) -> String {
    format!(
        "profile carries `{key}`, which this migration does not know how to store. Nothing was \
         written. Accepted keys here: {}. If the key is a real one this table has not learned, add \
         it here AND to `vike_secrets::profile_store`'s schema — a key that is silently dropped is \
         a setting an operator believes is armed.",
        known.join(", ")
    )
}

fn opt_str(v: Option<&toml::Value>, key: &str) -> Result<Option<String>, String> {
    match v {
        None => Ok(None),
        Some(v) => Ok(Some(v.as_str().ok_or(format!("`{key}` must be a string"))?.to_string())),
    }
}

fn opt_int(v: Option<&toml::Value>, key: &str) -> Result<Option<i64>, String> {
    match v {
        None => Ok(None),
        Some(v) => Ok(Some(v.as_integer().ok_or(format!("`{key}` must be an integer"))?)),
    }
}

/// A float key accepts a TOML integer as well, for the reason `ProfileRiskKey::accepts` gives about
/// its own asymmetry: **the mirror must never be stricter than the boot**, and `serde` takes an
/// integer for an `Option<f64>` field. The stored rendering is normalised to a float literal by the
/// renderer, which is what the round trip then checks.
fn opt_float(v: Option<&toml::Value>, key: &str) -> Result<Option<f64>, String> {
    match v {
        None => Ok(None),
        Some(v) => match v {
            toml::Value::Float(f) => Ok(Some(*f)),
            #[allow(clippy::cast_precision_loss)]
            toml::Value::Integer(i) => Ok(Some(*i as f64)),
            _ => Err(format!("`{key}` must be a number")),
        },
    }
}

/// Read a profile file, lower it, and decide about the active row. **Writes nothing.**
///
/// # Errors
///
/// A `String` naming the file and what was wrong with it — including the CROSS-KIND refusal, which
/// is pre-checked here for the reason this whole function exists: `crate::cmd::config_mirror`
/// plans every half before it writes any of them, so *"a body that would not reproduce its file
/// refuses the whole run rather than leaving the settings tables mirrored and the profile half
/// not"*. A `--dry-run` that reported *would mirror* for a name `store_profile` was going to refuse
/// would be exactly the positive confirmation of something false this module's fence is built
/// against. The refusal's WORDS are not spelled here: this constructs
/// `vike_secrets::profile_store::ProfileError::NameHeldByAnotherKind`, the same value the store
/// itself returns, so the two rungs cannot say different things.
pub fn plan(
    plane: Plane,
    settings_dir: &Path,
    file: &Path,
    name: &str,
) -> Result<Mirrored, String> {
    let text =
        std::fs::read_to_string(file).map_err(|e| format!("reading {}: {e}", file.display()))?;
    let stored = match plane {
        Plane::Run => rows_from_run_profile_text(name, &text),
        Plane::Daemon => rows_from_daemon_profile_text(name, &text),
    }
    .map_err(|e| format!("{}: {e}", file.display()))?;

    let mut summary = Vec::new();
    for m in &stored.mounts {
        summary.push(format!(
            "mount {} {}/{} [{}]",
            m.ord,
            m.venue,
            m.mount_symbol(),
            m.asset_class
        ));
    }
    for (path, value) in &stored.settings {
        summary.push(format!("{path} = {value}"));
    }

    // What the store already selects for this kind, if anything. An absent database is not an error
    // to ASK — the write below is where that refusal lives, and asking first would make a dry run
    // on an unmigrated box fail for the wrong reason.
    let stored_now = read_profiles(&vike_secrets::db_path_in(settings_dir)).ok();
    // ⚠ THE CROSS-KIND REFUSAL, one step before the store's own. the CI box's store holds an ACTIVE
    // `recorder` profile called `default` (the datahub unit runs `--recorder-profile default`), and
    // `--profile-name default` is one flag away from deleting its subscriptions and inheriting its
    // `active` bit onto the RUN plane. `config activate` has refused this by name since it was
    // written; `store_profile` refuses it now too, and this is the rung that keeps `--dry-run`
    // honest about it.
    if let Some(p) = &stored_now
        && let Some(held) = p.by_name(name)
        && held.row.kind != plane.kind()
    {
        return Err(vike_secrets::profile_store::ProfileError::NameHeldByAnotherKind {
            name: name.to_string(),
            held: held.row.kind.sql_word().to_string(),
            wanted: plane.kind(),
        }
        .to_string());
    }
    let already_active =
        stored_now.and_then(|p| p.active(plane.kind()).map(|s| s.row.name.clone()));
    // ⚠ `in_force: None` ALWAYS. It is not a conservative guess but the honest answer to a question
    // this process cannot see: `VIKE_RUN_PROFILE` reaches the daemon through its unit's
    // `EnvironmentFile=` and `--config` lives on its `ExecStart=`, neither of which is visible from
    // an operator shell. So a mirror stores a body and selects nothing, every time, and says so.
    let active = plan_active_row(None, already_active.as_deref(), name);
    Ok(Mirrored { stored, active, summary })
}

/// Write a planned body. Separate from [`plan`] so `--dry-run` can stop between them.
///
/// `store_profile` PRESERVES the `active` bit it finds, so re-mirroring an already-active profile
/// edits WHAT it is and never WHETHER it runs.
///
/// # Errors
///
/// A `String` from the store — on a project with no database, the refusal `vike-cli secrets migrate`
/// is the answer to; inside a daemon's own mount namespace, the read-only refusal.
pub fn write(
    plane: Plane,
    settings_dir: &Path,
    mirrored: &Mirrored,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), String> {
    let write = OperatorWrite::claim(&format!("vike-cli config mirror {}", plane.flag()));
    store_profile(
        &vike_secrets::db_path_in(settings_dir),
        &mirrored.stored,
        &write,
        now_utc,
        asset_class_words,
    )
    .map_err(|e| e.to_string())
}

/// **The sentence an operator must not have to infer: the body was stored and NOTHING was
/// selected.** Said on every mirror, dry run included.
#[must_use]
pub fn active_row_note(plane: Plane, m: &Mirrored) -> String {
    match &m.active {
        ActivePlan::Write { name } => format!(
            " The {} profile `{name}` was ALREADY the active row for its kind, so that row is \
             unchanged.",
            plane.kind().sql_word()
        ),
        ActivePlan::Withhold { reason } => format!(
            " No active {} row was written — {reason}. Storing a body is not selecting one: \
             `vike-cli config activate {} <name> --proves <file>` is the deliberate act, and until \
             it is taken this box resolves its {} profile exactly as it does today.",
            plane.kind().sql_word(),
            plane.kind().sql_word(),
            plane.kind().sql_word()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the CI box's live `settings/run-live.toml`, VERBATIM apart from the comments (which no key
    /// addresses — see the note in the report).
    const PROD2_RUN: &str = "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 100.0\n\
                             max_total_exposure     = 500.0\n";

    /// the CI box's live `settings/tradehub.toml`, VERBATIM plus the one key the schema requires and the
    /// file does not carry.
    const PROD2_DAEMON: &str = "venue = \"bybit\"\nasset_class = \"CryptoPerp\"\n\
                                token_id = \"BTCUSDT\"\ninterval = \"1m\"\nqty = 0.001\n\
                                half_spread = 0.0005\ntick_size = 0.1\nseed_cash = 1000.0\n\n\
                                [daemon]\nsummary_ms = 60000\nshutdown_deadline_ms = 10000\n";

    /// **EVERY KEY OF THE LIVE RUN PROFILE BECOMES A ROW.** Named individually rather than counted:
    /// a count passes over a swap.
    #[test]
    fn the_live_run_profile_lowers_to_a_row_per_key_and_round_trips() {
        let stored = rows_from_run_profile_text("run-live", PROD2_RUN).expect("must round-trip");
        assert_eq!(stored.row.kind, ProfileKind::Run);
        assert!(!stored.row.active, "a mirror never writes an active row");
        assert_eq!(stored.settings.get("mode").map(String::as_str), Some("\"live\""));
        assert_eq!(
            stored.settings.get("risk.max_notional_per_order").map(String::as_str),
            Some("100.0")
        );
        assert_eq!(
            stored.settings.get("risk.max_total_exposure").map(String::as_str),
            Some("500.0")
        );
        assert_eq!(stored.settings.len(), 3, "and nothing else: {:?}", stored.settings);
        assert!(stored.mounts.is_empty(), "a run profile has no mounts");
    }

    /// **EVERY KEY OF THE LIVE DAEMON PROFILE BECOMES A ROW**, in the single-mount spelling, and
    /// the round trip is what proves the spelling was preserved.
    #[test]
    fn the_live_daemon_profile_lowers_to_one_mount_row_and_round_trips() {
        let stored =
            rows_from_daemon_profile_text("tradehub", PROD2_DAEMON).expect("must round-trip");
        assert_eq!(stored.mounts.len(), 1);
        let m = &stored.mounts[0];
        assert_eq!((m.ord, m.is_primary), (0, false), "the file declares no primary");
        assert_eq!(m.venue, "bybit");
        assert_eq!(m.asset_class, "CryptoPerp");
        assert_eq!(m.token_id.as_deref(), Some("BTCUSDT"), "the file's own spelling is carried");
        assert_eq!(m.symbol, None, "…and the other one stays absent, which the schema CHECKs");
        assert_eq!(m.interval.as_deref(), Some("1m"));
        assert_eq!(m.qty, Some(0.001));
        assert_eq!(m.half_spread, Some(0.0005));
        assert_eq!(m.tick_size, Some(0.1));
        assert_eq!(m.seed_cash, Some(1000.0));
        assert_eq!(
            m.data_only, None,
            "absent, not `false` — a stored default breaks the round trip"
        );
        assert_eq!(m.account, None);
        assert_eq!(m.strategy_name, None);
        assert_eq!(m.strategy_rhai, None);
        assert_eq!(m.interval_ms, None);
        assert_eq!(m.resolution_ts_ms, None);
        assert_eq!(stored.settings.get("daemon.summary_ms").map(String::as_str), Some("60000"));
        assert_eq!(
            stored.settings.get("daemon.shutdown_deadline_ms").map(String::as_str),
            Some("10000")
        );
        assert_eq!(stored.settings.len(), 2);
        assert!(stored.params.is_empty());
    }

    /// **THE SINGLE-MOUNT SPELLING SURVIVES**, which is the state-sidecar defect's fix seen from
    /// the migration's side: the rendered document must be the top-level spelling, not `[[mounts]]`.
    #[test]
    fn a_single_mount_profile_renders_back_as_a_single_mount_profile() {
        let stored =
            rows_from_daemon_profile_text("tradehub", PROD2_DAEMON).expect("must round-trip");
        let rendered = render_daemon_toml(&stored).expect("renders");
        assert!(!rendered.contains("[[mounts]]"), "the spelling moved:\n{rendered}");
        assert!(rendered.contains("venue = \"bybit\""), "{rendered}");
    }

    /// …and a `[[mounts]]` profile keeps ITS spelling.
    #[test]
    fn a_multi_mount_profile_renders_back_as_an_array() {
        let text = "[[mounts]]\nvenue = \"bybit\"\nasset_class = \"CryptoPerp\"\n\
                    symbol = \"BTCUSDT\"\n\n[[mounts]]\nvenue = \"okx\"\n\
                    asset_class = \"CryptoSpot\"\nsymbol = \"BTC-USDT\"\n";
        let stored = rows_from_daemon_profile_text("two", text).expect("must round-trip");
        assert_eq!(stored.mounts.len(), 2);
        assert!(render_daemon_toml(&stored).expect("renders").contains("[[mounts]]"));
    }

    /// **THE FENCE ITSELF, reached and proven** — a difference only the round-trip comparison can
    /// see, not one an earlier `return` catches.
    ///
    /// A ONE-row `[[mounts]]` profile that declares no `primary` is that difference: the rows are
    /// indistinguishable from a single-mount profile's, so the renderer emits the single-mount
    /// spelling and the document is not the one that was read. The two genuinely mount under
    /// different controller ids, so refusing is right — and the message names the repair.
    #[test]
    fn a_body_that_does_not_reproduce_its_profile_refuses_the_write() {
        let text = "[[mounts]]\nvenue = \"bybit\"\nasset_class = \"CryptoPerp\"\n\
                    symbol = \"BTCUSDT\"\n";
        let e = rows_from_daemon_profile_text("one", text)
            .expect_err("a one-row [[mounts]] profile with no declared primary must refuse");
        assert!(e.contains("REFUSING"), "{e}");
        assert!(e.contains("do not reproduce the profile"), "{e}");
        assert!(e.contains("primary = true"), "the refusal names the repair: {e}");
        assert!(e.contains("the profile as given"), "{e}");
        assert!(e.contains("as the rows render it"), "{e}");
        // …and adding the one line it names makes the same profile storable.
        let fixed = format!("{text}primary = true\n");
        assert!(rows_from_daemon_profile_text("one", &fixed).is_ok(), "the named repair must work");
    }

    /// The run plane's own fence case: an EMPTY table renders to nothing, so it does not survive.
    #[test]
    fn an_empty_table_in_a_run_profile_refuses_the_write() {
        let e = rows_from_run_profile_text("p", "mode = \"paper\"\n\n[guards]\n")
            .expect_err("an empty [guards] table does not survive the round trip");
        assert!(e.contains("REFUSING"), "{e}");
    }

    /// A mount that names no product cannot become a row — 0061 phase 5, arriving at the one seam
    /// that can enforce it. The message must say the migration cannot answer it.
    #[test]
    fn a_mount_with_no_asset_class_is_refused_and_says_the_migration_cannot_guess() {
        let e = rows_from_daemon_profile_text(
            "tradehub",
            "venue = \"bybit\"\ntoken_id = \"BTCUSDT\"\n",
        )
        .expect_err("asset_class is NOT NULL");
        assert!(e.contains("asset_class"), "{e}");
        assert!(e.contains("CryptoPerp"), "the message shows the shape of the fix: {e}");
        assert!(e.contains("cannot answer it for you"), "{e}");
        let e = rows_from_daemon_profile_text(
            "tradehub",
            "venue = \"bybit\"\ntoken_id = \"X\"\nasset_class = \"Perp\"\n",
        )
        .expect_err("a word outside the vocabulary is refused by name");
        assert!(e.contains("Perp"), "{e}");
    }

    /// An unknown key is REFUSED BY NAME on both planes — the write-path half of
    /// `deny_unknown_fields`. A dropped key is a setting an operator believes is armed.
    #[test]
    fn an_unknown_key_refuses_the_whole_profile() {
        let e = rows_from_daemon_profile_text(
            "tradehub",
            "venue = \"bybit\"\nsymbol = \"X\"\nasset_class = \"CryptoPerp\"\nqtyy = 1.0\n",
        )
        .expect_err("a misspelled mount key must refuse");
        assert!(e.contains("qtyy"), "{e}");
        assert!(e.contains("Nothing was written"), "{e}");

        let e = rows_from_daemon_profile_text(
            "tradehub",
            "venue = \"bybit\"\nsymbol = \"X\"\nasset_class = \"CryptoPerp\"\n\n[daemon]\n\
             summary_m = 1\n",
        )
        .expect_err("a misspelled [daemon] key must refuse");
        assert!(e.contains("daemon.summary_m"), "names the PATH: {e}");

        // The run plane refuses a `[risk]` key by name, through the roster that is gated against
        // `vike_exec::ProfileRisk`'s own fields.
        let e = rows_from_run_profile_text("p", "mode = \"live\"\n\n[risk]\nmax_levrage = 3.0\n")
            .expect_err("a typo'd [risk] key must refuse");
        assert!(e.contains("max_levrage"), "{e}");
    }

    /// The two tombstoned tables are refused HERE, not stored and refused at the next boot.
    #[test]
    fn a_tombstoned_table_is_refused_by_name() {
        for dead in RUN_TOMBSTONES {
            let text = format!("mode = \"paper\"\n\n[{dead}]\nkind = \"x\"\n");
            let e = rows_from_run_profile_text("p", &text)
                .expect_err("a tombstoned table must be refused, not stored");
            assert!(e.contains(dead), "the refusal names the table: {e}");
            assert!(e.contains("no longer part of a run profile"), "{e}");
            assert!(e.contains("Nothing was written"), "{e}");
        }
    }

    /// A `[risk]` key whose scalar shape disagrees with the roster is refused — and an INTEGER for
    /// a float key is NOT, because the mirror must never be stricter than the boot.
    #[test]
    fn the_risk_shape_check_matches_the_boot_rather_than_being_stricter() {
        assert!(
            rows_from_run_profile_text("p", "mode = \"paper\"\n\n[risk]\nmax_leverage = 3\n")
                .is_ok(),
            "`max_leverage = 3` is a profile the daemon starts on, so the mirror must take it"
        );
        let e = rows_from_run_profile_text(
            "p",
            "mode = \"paper\"\n\n[risk]\nmax_orders_per_window = 3.0\n",
        )
        .expect_err("a float for an integer key is refused by the real parser too");
        assert!(e.contains("max_orders_per_window"), "{e}");
    }

    /// Mixing the two mount spellings is refused rather than resolved — a top-level `venue` beside
    /// a `[[mounts]]` array is two different claims about what this daemon mounts.
    #[test]
    fn the_two_mount_spellings_may_not_be_mixed() {
        let e = rows_from_daemon_profile_text(
            "x",
            "venue = \"bybit\"\n\n[[mounts]]\nvenue = \"okx\"\nasset_class = \"CryptoSpot\"\n\
             symbol = \"BTC-USDT\"\nprimary = true\n",
        )
        .expect_err("the two spellings may not be mixed");
        assert!(e.contains("two spellings"), "{e}");
    }

    /// **A mount that omits `venue` is refused BY NAME**, although the TOML key is optional. The
    /// round-trip fence would catch it anyway — the rendered document carries the default the row
    /// had to store — but as a generic mismatch printing `venue = ""`, which reads as a migration
    /// bug rather than as the one-line repair it is.
    #[test]
    fn a_mount_that_omits_its_venue_is_refused_by_name_rather_than_by_the_fence() {
        let e = rows_from_daemon_profile_text(
            "poly",
            "token_id = \"12345\"\nasset_class = \"PredictionMarket\"\nqty = 20.0\n",
        )
        .expect_err("a mount ROW must name its venue");
        assert!(e.contains("does not declare `venue`"), "{e}");
        assert!(e.contains("polymarket"), "the refusal names the default it will not write: {e}");
        assert!(!e.contains("REFUSING"), "this is the lowering's refusal, not the fence's: {e}");
        // ...and the named repair makes the same profile storable.
        let fixed = rows_from_daemon_profile_text(
            "poly",
            "venue = \"polymarket\"\ntoken_id = \"12345\"\nasset_class = \"PredictionMarket\"\n\
             qty = 20.0\n",
        )
        .expect("the named repair must work");
        assert_eq!(fixed.mounts[0].venue, "polymarket");
        assert_eq!(fixed.mounts[0].token_id.as_deref(), Some("12345"));
    }
}
