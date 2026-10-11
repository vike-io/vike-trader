//! `vike-cli config mirror --daemon <file>` — **the DAEMON profile becomes rows, and the migration
//! proves it changed nothing before it writes.**
//!
//! ⚠ **This module had a RUN-profile plane too (`config mirror --profile <file>`), and it is
//! DELETED** (decision 0111, verdict 4): the run profile is rows only, no file is imported, and
//! `crate::cmd::config::bootstrap_run` writes the rows from arguments. Paragraphs below that speak
//! of "both" planes describe the history of the one that is left.
//!
//! This is the half `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s
//! Phase 3 shipped without. That phase landed the store, the schema, the selection resolver
//! (`vike_secrets::profile_store::select` — the owner's *the ROW wins* ruling written once) and the
//! fence, **with no verb that wrote a row**; `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs` recorded
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
//!   reader was forbidden by name (`crates/vike-ops/tests/settings_secrets/profile_risk_readers_gate.rs`). **Building
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
//! Taken verbatim from `crate::cmd::config::mirror_recorder`, and for a sharper reason here:
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
//! `crate::cmd::config::activate`.
//!
//! ⚠ **Why the mirror may not sniff the environment and activate for you.** `config mirror` runs in
//! an operator shell where the daemon's own `--config` argv is not visible. Having the mirror pick
//! a profile out of ITS OWN environment would be 0057's Question 3 answered sideways in the one verb
//! that writes — the same argument that made every profile flag here explicit rather than
//! defaulted.

use std::collections::BTreeMap;
use std::path::Path;

use vike_secrets::profile_store::{
    ActivePlan, MountRow, OperatorWrite, ProfileKind, ProfileRow, StoredProfile, plan_active_row,
    read_profiles, render_daemon_toml, store_profile,
};

/// What a mirror of one profile produced. **Nothing has been written when this exists** — [`fn@write`]
/// is the separate call, so `--dry-run` can stop between them.
#[derive(Debug)]
pub(crate) struct Mirrored {
    /// The rows, ready to store.
    pub stored: StoredProfile,
    /// What [`plan_active_row`] decided about this kind's active row. Always a `Withhold` on a box
    /// this CLI has no in-force evidence for, which is every box.
    pub active: ActivePlan,
    /// One human line per row group, for the report.
    pub summary: Vec<String>,
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
pub(crate) fn rows_from_daemon_profile_text(
    name: &str,
    text: &str,
) -> Result<StoredProfile, String> {
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
    fence(name, text, &doc, &rendered)?;
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
fn fence(name: &str, text: &str, original: &toml::Value, rendered: &str) -> Result<(), String> {
    let back: toml::Value = toml::from_str(rendered).map_err(|e| {
        format!(
            "the rows rendered a document that does not parse ({e}). Nothing was written. This is \
             a defect in the migration, not in your profile — report it with the profile that \
             produced it.\n--- as the rows render it ---\n{rendered}"
        )
    })?;
    if &back != original {
        return Err(format!(
            "REFUSING to store daemon profile `{name}`: the rows do not reproduce the profile they \
             came from, so storing them would change what this box does.\n\nNothing was written. \
             A daemon profile decides which venue, which instrument and which clip size a \
             labelled account trades. It also decides the MOUNT IDENTITY: the single-mount and \
             `[[mounts]]` spellings mount under different state-sidecar and journal-attribution \
             keys, so a spelling difference below is not cosmetic. If the only difference is that \
             the rows render `[[mounts]]` where your file writes top-level keys, add \
             `primary = true` to the mount you mean, or accept the controller-id move \
             deliberately.\n\n--- the profile as given ---\n{}\n--- the profile as the rows \
             render it ---\n{rendered}",
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

/// A float key accepts a TOML integer as well, because **the mirror must never be stricter than the
/// boot** and `serde` takes an integer for an `Option<f64>` field
/// (`crates/vike-config/tests/profile_risk_rows.rs`'s
/// `the_real_parsers_cross_shape_answers_are_pinned`). The stored rendering is normalised to a
/// float literal by the renderer, which is what the round trip then checks.
fn opt_float(v: Option<&toml::Value>, key: &str) -> Result<Option<f64>, String> {
    match v {
        None => Ok(None),
        Some(v) => match v {
            toml::Value::Float(f) => Ok(Some(*f)),
            #[expect(clippy::cast_precision_loss)]
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
/// is pre-checked here for the reason this whole function exists: `crate::cmd::config::mirror`
/// plans every half before it writes any of them, so *"a body that would not reproduce its file
/// refuses the whole run rather than leaving the settings tables mirrored and the profile half
/// not"*. A `--dry-run` that reported *would mirror* for a name `store_profile` was going to refuse
/// would be exactly the positive confirmation of something false this module's fence is built
/// against. The refusal's WORDS are not spelled here: this constructs
/// `vike_secrets::profile_store::ProfileError::NameHeldByAnotherKind`, the same value the store
/// itself returns, so the two rungs cannot say different things.
pub(crate) fn plan(settings_dir: &Path, file: &Path, name: &str) -> Result<Mirrored, String> {
    let text =
        std::fs::read_to_string(file).map_err(|e| format!("reading {}: {e}", file.display()))?;
    let stored = rows_from_daemon_profile_text(name, &text)
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
    // `--daemon-name default` is one flag away from deleting its subscriptions and inheriting its
    // `active` bit onto the DAEMON plane. `config activate` has refused this by name since it was
    // written; `store_profile` refuses it now too, and this is the rung that keeps `--dry-run`
    // honest about it.
    if let Some(p) = &stored_now
        && let Some(held) = p.by_name(name)
        && held.row.kind != ProfileKind::Daemon
    {
        return Err(vike_secrets::profile_store::ProfileError::NameHeldByAnotherKind {
            name: name.to_string(),
            held: held.row.kind.sql_word().to_string(),
            wanted: ProfileKind::Daemon,
        }
        .to_string());
    }
    let already_active =
        stored_now.and_then(|p| p.active(ProfileKind::Daemon).map(|s| s.row.name.clone()));
    // ⚠ `in_force: None` ALWAYS. It is not a conservative guess but the honest answer to a question
    // this process cannot see: `--config` lives on the daemon's `ExecStart=`, which is not visible
    // from an operator shell. So a mirror stores a body and selects nothing, every time, and says
    // so.
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
/// A `String` from the store — on a project with no database, the refusal `vike-cli secrets init`
/// is the answer to. Inside a daemon's own mount namespace it depends on the unit: inside
/// `vike-datahub`'s it is the read-only refusal (`deploy/vike-datahub.service`'s `ReadWritePaths=`
/// names no `settings/db`); inside `vike-tradehub`'s there is none, because that unit grants
/// `settings/db` since the owner's ruling of 2026-09-18 (`deploy/vike-tradehub.service`).
pub(crate) fn write(
    settings_dir: &Path,
    mirrored: &Mirrored,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), String> {
    let write = OperatorWrite::claim("vike-cli config mirror --daemon");
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
pub(crate) fn active_row_note(m: &Mirrored) -> String {
    match &m.active {
        ActivePlan::Write { name } => format!(
            " The daemon profile `{name}` was ALREADY the active row for its kind, so that row is \
             unchanged."
        ),
        ActivePlan::Withhold { reason } => format!(
            " No active daemon row was written — {reason}. Storing a body is not selecting one: \
             `vike-cli config activate daemon <name> --proves <file>` is the deliberate act, and \
             until it is taken this box resolves its daemon profile exactly as it does today."
        ),
    }
}

#[path = "tests/mirror_profile.rs"]
#[cfg(test)]
mod config_mirror_profile_tests;
