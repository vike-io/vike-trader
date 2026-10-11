//! `vike-cli config bootstrap-run` — **the run profile's writer**: the operator's pre-trade risk
//! budget (`[risk]`), the core's guards (`[guards]`) and sinks (`[sinks]`) stored as the rows of
//! ONE `run` body, and made the ACTIVE run profile in the same act. The `run` twin of
//! `crate::cmd::config::bootstrap_daemon`, and the same shape: built from argv, never from a file.
//!
//! # Why this exists
//!
//! Decision 0111 (verdict 4) made the daemon run profile ROWS ONLY: `vike-tradehub` reads the
//! ACTIVE `run` row of the settings database and nothing else — its `--profile` argument is refused
//! by name and `VIKE_RUN_PROFILE` by `vike_config::REMOVED_ENV`. The file-importing
//! `config mirror --profile <file>` went with them. This verb is the writer the owner approved in
//! their place: it builds the `StoredProfile` directly from its arguments.
//!
//! # The argument grammar IS the document's
//!
//! Every key is a flag spelled as its DOTTED PATH in the run-profile document — `--mode live`,
//! `--risk.max_notional_per_order 5000`, `--sinks.journal.dir /var/wal`,
//! `--guards.margin_call.mm_requirement 0.05` — and becomes ONE `profile_setting` row at that
//! path, its value stored as the TOML scalar `vike_secrets::profile_store::render_run_toml` emits
//! verbatim. So there is no second vocabulary for an operator to learn: the keys are the ones
//! `docs/ops/run-profile-live.toml` documents, with `--` in front.
//!
//! The vocabulary is [`run_key`]: the `[risk]` keys are `vike_model::ProfileRisk::keys()` (serde's
//! own field list) and each one's shape is `vike_config::risk_key_kind`, asked of the real parser,
//! so nothing about `[risk]` is re-spelled here; every other key comes from [`DOCUMENT_KEYS`]. This crate links neither `vike-core` nor `vike-exec` (the `light-consumers`
//! lane holds it out of that closure), so the vocabulary is held to `vike_core::RunProfile` from
//! the test side, BOTH ways (`crates/vike-cli/src/cmd/config/tests/bootstrap_run.rs`): every key
//! this verb writes loads through the daemon's own loader, and every leaf of a fully-populated
//! `RunProfile` is a key this verb writes. A key left off stores NO row, so the daemon's own
//! default stays the daemon's.
//!
//! # What is refused HERE, and what only the daemon refuses
//!
//! Refused at write, before anything is stored: an unknown key, a value of the wrong scalar shape,
//! a key given twice, a `mode` outside its three words, a nested table missing the key it cannot
//! exist without ([`TABLE_REQUIRES`]), and a `live` profile missing a pre-trade ceiling a live
//! mount refuses to start without (derived from `vike_config::ceilings::PRE_TRADE_CEILINGS`, never
//! restated). ⚠ **The semantic refusals of `vike_core::RunProfile::validate` arrive at the
//! daemon's next start, not here** — a `live` profile setting a venue-owned grid field,
//! `max_leverage < 1.0`, `required_free_bp_pct` outside `[0.0, 1.0)` — the same declared residual
//! `crate::cmd::config::activate` carries, for the same linkage reason. The daemon treats a run row
//! it cannot load as a HARD startup failure, never as "no run profile".
//!
//! # Why store-then-activate in one verb
//!
//! `bootstrap_daemon`'s argument, unchanged: there is no file to prove against, so a separate
//! `config activate` step would have nothing to check. `--dry-run` is the inspection step, and it
//! prints the document the daemon will parse. Re-running the verb under the same name REPLACES the
//! stored body, so a key left off the second time is gone, not kept.

use std::collections::BTreeMap;
use std::path::Path;

use vike_secrets::profile_store::{
    OperatorWrite, ProfileKind, ProfileRow, StoredProfile, read_profiles, render_run_toml,
    set_active, store_profile,
};

use crate::cmd::args::{self, Flags};

/// `config bootstrap-run`'s usage.
pub(crate) const BOOTSTRAP_RUN_USAGE: &str = "usage: vike-cli config bootstrap-run <name> --mode <backtest|paper|live> \
     [--<table>.<key> <value> ...] [--dry-run]\n\n  Store a RUN profile — the pre-trade risk \
     budget ([risk]), the core's guards ([guards]) and sinks\n  ([sinks]) — built entirely from \
     these arguments, never from a file, and make it the ACTIVE\n  run profile: the one \
     `vike-tradehub` builds its ceilings from at its next start (decision\n  0111 — no \
     --profile, no VIKE_RUN_PROFILE). Re-running it under the same name replaces the\n  stored \
     body in place, so a key left off the second time is gone.\n\n  Every key is its dotted path \
     in the run-profile document (docs/ops/run-profile-live.toml):\n    --mode live \
     --risk.max_notional_per_order 5000 --risk.max_total_exposure 25000\n    \
     --sinks.journal.dir /var/vike/wal --guards.max_drawdown 0.15\n  A `live` profile must \
     carry every ceiling a live mount refuses to start without.\n\n  --dry-run   print the \
     document the daemon would parse, and write nothing";

/// The scalar SHAPE a key's value must have. The words are what a refusal prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shape {
    /// A finite number — stored as a TOML float.
    Number,
    /// A whole number, zero or more — stored as a TOML integer.
    Count,
    /// `true` or `false`.
    Switch,
    /// Free text (a directory) — stored as a TOML basic string.
    Text,
    /// One word of a closed vocabulary — stored as a TOML basic string.
    Word(&'static [&'static str]),
}

/// **Every run-profile key outside `[risk]`**, at its dotted path, in the document's own order.
///
/// `[risk]` is deliberately absent: its keys are `vike_model::ProfileRisk::keys()`, joined in by
/// [`run_key`]. Two document keys are deliberately absent too: `name` (the ROW's name is the
/// profile's name) and the `[event_source]` / `[broker]` tombstones `RunProfile::validate` refuses
/// by name. The test side holds this list equal to `vike_core::RunProfile`'s leaves.
pub(super) const DOCUMENT_KEYS: &[(&str, Shape)] = &[
    ("mode", Shape::Word(&["backtest", "paper", "live"])),
    ("sinks.gui", Shape::Switch),
    ("sinks.recorder", Shape::Switch),
    ("sinks.raw_capture_dir", Shape::Text),
    ("sinks.equity_sample_ms", Shape::Count),
    ("sinks.journal.dir", Shape::Text),
    ("sinks.journal.segment_bytes", Shape::Count),
    ("sinks.journal.flush_every", Shape::Count),
    ("sinks.journal.snapshot_every", Shape::Count),
    ("guards.initial_trading_state", Shape::Word(&["active", "reducing", "halted"])),
    ("guards.submit_ack_timeout_ms", Shape::Count),
    ("guards.submit_ack_confirm_grace_ms", Shape::Count),
    ("guards.max_drawdown", Shape::Number),
    ("guards.conditionals_on_ticks", Shape::Switch),
    ("guards.freshness_ms", Shape::Count),
    ("guards.margin_call.mm_requirement", Shape::Number),
    ("guards.margin_call.warn_fraction", Shape::Number),
    ("guards.margin_call.buffer", Shape::Number),
];

/// A nested table and the key it cannot exist without (the field has no default, so a table
/// written without it fails the daemon's load). Pinned against the daemon's loader by the test
/// side.
pub(super) const TABLE_REQUIRES: &[(&str, &str)] =
    &[("sinks.journal", "dir"), ("guards.margin_call", "mm_requirement")];

/// The shape of the key at `path`, or `None` when no run-profile key goes by it.
pub(super) fn run_key(path: &str) -> Option<Shape> {
    if let Some(name) = path.strip_prefix("risk.") {
        return vike_config::risk_key_kind(name).map(|kind| match kind {
            vike_config::RiskKeyKind::Float => Shape::Number,
            vike_config::RiskKeyKind::Integer => Shape::Count,
            vike_config::RiskKeyKind::Boolean => Shape::Switch,
        });
    }
    DOCUMENT_KEYS.iter().find(|(p, _)| *p == path).map(|(_, s)| *s)
}

/// Every path [`run_key`] answers for, `[risk]` included — for the unknown-key refusal and the
/// test side.
pub(super) fn all_run_keys() -> Vec<String> {
    let mut out: Vec<String> = DOCUMENT_KEYS.iter().map(|(p, _)| (*p).to_string()).collect();
    out.extend(vike_model::ProfileRisk::keys().iter().map(|k| format!("risk.{k}")));
    out
}

/// One operator-typed value as the TOML scalar a `profile_setting` row stores.
///
/// # Errors
///
/// A refusal naming the flag, the value and the shape it must have.
pub(super) fn lower_value(path: &str, shape: Shape, raw: &str) -> Result<String, String> {
    let bad = |want: &str| format!("--{path} must be {want}, got '{raw}'");
    match shape {
        Shape::Number => match raw.trim().parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(toml::Value::Float(v).to_string()),
            _ => Err(bad("a finite number")),
        },
        Shape::Count => match raw.trim().parse::<i64>() {
            Ok(v) if v >= 0 => Ok(toml::Value::Integer(v).to_string()),
            _ => Err(bad("a whole number, zero or more")),
        },
        Shape::Switch => match raw.trim() {
            "true" => Ok("true".to_string()),
            "false" => Ok("false".to_string()),
            _ => Err(bad("`true` or `false`")),
        },
        Shape::Text => {
            if raw.trim().is_empty() {
                Err(bad("a non-empty value"))
            } else {
                Ok(toml::Value::String(raw.to_string()).to_string())
            }
        }
        Shape::Word(words) => {
            if words.contains(&raw) {
                Ok(toml::Value::String(raw.to_string()).to_string())
            } else {
                Err(bad(&format!("one of {}", words.join(", "))))
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct BootstrapRunArgs {
    pub(super) name: String,
    /// `path → stored TOML scalar`, one entry per key given, `mode` included.
    pub(super) settings: BTreeMap<String, String>,
    pub(super) dry_run: bool,
}

pub(super) fn parse_bootstrap_run(
    args: impl Iterator<Item = String>,
) -> Result<BootstrapRunArgs, String> {
    let mut name: Option<String> = None;
    let mut settings: BTreeMap<String, String> = BTreeMap::new();
    let mut dry_run = false;

    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "-h" | "--help" => return args::help_requested(),
            "--dry-run" => {
                args::no_value(&flag, inline)?;
                dry_run = true;
            }
            other if other.starts_with("--") => {
                let path = &other[2..];
                let Some(shape) = run_key(path) else {
                    return Err(format!(
                        "unknown option '{other}' — a run-profile key is its dotted path in the \
                         document. The keys are: {}",
                        all_run_keys()
                            .iter()
                            .map(|k| format!("--{k}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                };
                let raw = flags.value(&flag, inline)?;
                let stored = lower_value(path, shape, &raw)?;
                if settings.insert(path.to_string(), stored).is_some() {
                    return Err(format!(
                        "{other} given twice — a run profile holds one value per key, and taking \
                         the last silently would hide which one you meant"
                    ));
                }
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'"));
            }
            _ => {
                if let Some(v) = &inline {
                    return Err(format!(
                        "'{flag}={v}' looks like a flag with a value, but the profile NAME is a \
                         plain word"
                    ));
                }
                if name.is_some() {
                    return Err(format!("unexpected extra argument '{flag}'"));
                }
                name = Some(flag.clone());
            }
        }
    }

    let name = name.ok_or("missing the profile NAME")?;
    if !settings.contains_key("mode") {
        return Err(
            "missing --mode <backtest|paper|live>, REQUIRED (a run profile has no default \
                    mode, and a live mount refuses any profile that is not `live`)"
                .to_string(),
        );
    }
    for (table, key) in TABLE_REQUIRES {
        let prefix = format!("{table}.");
        let required = format!("{table}.{key}");
        if settings.keys().any(|k| k.starts_with(&prefix)) && !settings.contains_key(&required) {
            return Err(format!(
                "a `[{table}]` key was given without --{required}, which that table cannot exist \
                 without — the daemon would refuse the profile at its next start"
            ));
        }
    }
    // ⚠ DERIVED from `PRE_TRADE_CEILINGS`, never restated: the ceilings whose home is the run
    // profile's `[risk]` table and whose absence REFUSES a live mount
    // (`vike_mount::MountError::MissingRiskBudget`). A `live` body without them would be stored,
    // activated, and then refuse to start the very mount it exists for.
    if settings.get("mode").map(String::as_str) == Some("\"live\"") {
        let missing: Vec<String> = vike_config::ceilings::PRE_TRADE_CEILINGS
            .iter()
            .filter(|c| {
                c.home == vike_config::ceilings::CeilingHome::RunProfileRisk
                    && c.refuses_live_mount_when_absent
            })
            .map(|c| format!("risk.{}", c.name))
            .filter(|path| !settings.contains_key(path))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "a `live` run profile must carry {} — they depend on your account size, so there \
                 is no safe default, and a live mount refuses to start without them",
                missing.iter().map(|k| format!("--{k}")).collect::<Vec<_>>().join(" and ")
            ));
        }
    }
    Ok(BootstrapRunArgs { name, settings, dry_run })
}

/// Entry point for `config bootstrap-run`.
pub(crate) fn run(
    args: impl Iterator<Item = String>,
    settings_dir: Option<&Path>,
) -> std::process::ExitCode {
    let args = match parse_bootstrap_run(args) {
        Ok(a) => a,
        Err(msg) => {
            return args::exit_for_parse_error("config bootstrap-run", BOOTSTRAP_RUN_USAGE, &msg);
        }
    };
    match bootstrap(&args, settings_dir, now_utc()) {
        Ok(report) => {
            println!("{report}");
            std::process::ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config bootstrap-run: {msg}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// The body this run stores. The `active` bit is never taken from here: `set_active` is the act.
pub(super) fn stored_body(args: &BootstrapRunArgs) -> StoredProfile {
    StoredProfile {
        row: ProfileRow {
            name: args.name.clone(),
            kind: ProfileKind::Run,
            active: false,
            note: None,
        },
        mounts: Vec::new(),
        params: BTreeMap::new(),
        settings: args.settings.clone(),
        recorder: None,
    }
}

fn crossing_line(already: &Option<String>, name: &str, dry_run: bool) -> String {
    match already {
        Some(held) if held == name => {
            let was = if dry_run { "is" } else { "was" };
            format!(" `{name}` {was} already the active run profile; its body is this one now.")
        }
        Some(held) => {
            let verb = if dry_run { "would REPOINT" } else { "REPOINTED" };
            format!(" This {verb} the box's run profile from `{held}` to `{name}`.")
        }
        None => " Nothing selected a run profile in this store, so this is the CROSSING: from the \
                  next restart the daemon's pre-trade ceilings come from this row."
            .to_string(),
    }
}

pub(super) fn bootstrap(
    args: &BootstrapRunArgs,
    settings_dir: Option<&Path>,
    now_utc: i64,
) -> Result<String, String> {
    let dir = settings_dir.ok_or(
        "no settings directory resolved — there is no store to write a profile into. Set \
         VIKE_SETTINGS_DIR, or run from a project that has one.",
    )?;
    let db = vike_secrets::db_path_in(dir);
    let stored = stored_body(args);
    let profiles = read_profiles(&db).ok();
    // The CROSS-KIND refusal one step before the store's own, so `--dry-run` cannot promise a write
    // `store_profile` would refuse: `profile.name` is one namespace across the three kinds. The
    // words are the store's (`ProfileError::NameHeldByAnotherKind`), constructed, not re-spelled.
    if let Some(held) = profiles.as_ref().and_then(|p| p.by_name(&args.name))
        && held.row.kind != ProfileKind::Run
    {
        return Err(vike_secrets::profile_store::ProfileError::NameHeldByAnotherKind {
            name: args.name.clone(),
            held: held.row.kind.sql_word().to_string(),
            wanted: ProfileKind::Run,
        }
        .to_string());
    }
    let already =
        profiles.as_ref().and_then(|p| p.active(ProfileKind::Run).map(|s| s.row.name.clone()));
    let mode = args.settings.get("mode").map_or("?", |m| m.trim_matches('"'));
    let arming = crate::cmd::config::activate::RUN_ARMING_WARNING;

    if args.dry_run {
        return Ok(format!(
            "would store run profile `{}` (mode={mode}, {} key(s)) in {} and make it ACTIVE — \
             NOTHING WAS WRITTEN.{}{arming}\n\n--- the document the daemon would parse ---\n{}",
            args.name,
            args.settings.len(),
            db.display(),
            crossing_line(&already, &args.name, true),
            render_run_toml(&stored).trim_end()
        ));
    }

    let write = OperatorWrite::claim("vike-cli config bootstrap-run");
    store_profile(&db, &stored, &write, now_utc, vike_model::AssetClass::SQL_WORDS)
        .map_err(|e| e.to_string())?;
    set_active(
        &db,
        ProfileKind::Run,
        &args.name,
        &write,
        now_utc,
        vike_model::AssetClass::SQL_WORDS,
    )
    .map_err(|e| e.to_string())?;

    Ok(format!(
        "stored and ACTIVATED run profile `{}` in {} (mode={mode}, {} key(s)).{}{arming}\n\n\
         Restart `vike-tradehub` to run it: it reads the run profile once, at boot. `vike-cli \
         config deactivate run` leaves NO run profile (the paper mount then runs without an \
         operator budget and a live mount refuses to start); re-running this verb under the same \
         name replaces the body in place.",
        args.name,
        db.display(),
        args.settings.len(),
        crossing_line(&already, &args.name, false)
    ))
}

fn now_utc() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[path = "tests/bootstrap_run.rs"]
#[cfg(test)]
mod config_run_bootstrap_tests;
