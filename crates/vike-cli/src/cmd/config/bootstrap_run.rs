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
//! Every key is a flag spelled as its DOTTED PATH in the run-profile document —
//! `--risk.max_notional_per_order 5000`, `--sinks.journal.snapshot_every 1024`,
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
//! a key given twice, and a nested table missing the key it cannot exist without
//! ([`TABLE_REQUIRES`]). ⚠ **The semantic refusals of `vike_core::RunProfile::validate` arrive at
//! the daemon's next start, not here** — `max_leverage < 1.0`, `required_free_bp_pct` outside
//! `[0.0, 1.0)` — the same declared residual
//! `crate::cmd::config::activate` carries, for the same linkage reason. The daemon treats a run row
//! it cannot load as a HARD startup failure, never as "no run profile".
//!
//! # It takes no `--mode`, and it WARNS rather than refuses about the live ceilings
//!
//! A run profile states no kind of run and no tier (decision 0156 item 7): where a forward run's
//! orders go is each mounted account's `account.tier`, so `--mode` is an unknown option like any
//! other. The refusal of a `live` body missing a pre-trade ceiling went with it — a body no longer
//! says whether it will reach a `demo`/`live` account — and is a WARNING in the report instead
//! ([`missing_live_ceilings`], derived from `vike_config::ceilings::PRE_TRADE_CEILINGS`, never
//! restated): the mount of every `demo`/`live` account still REFUSES to start without them
//! (`vike_mount::MountError::MissingRiskBudget`), and a paper-only box runs without. The
//! venue-owned grid fields (`tick_size`/`lot_size`/`min_qty`/`min_notional`) are refused at that
//! mount too, field by field; a `paper` account takes them.
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

use crate::cmd::args;

// The prose half of `config bootstrap-run --help`; the usage line and the option table (one row per
// run-profile key) are `clap`'s, built from the key table below by [`BootstrapRunCmd`].
const LONG_ABOUT: &str = "\
Store a RUN profile — the pre-trade risk budget ([risk]), the core's guards ([guards]) and sinks
([sinks]) — built entirely from these arguments, never from a file, and make it the ACTIVE
run profile: the one `vike-tradehub` builds its ceilings from at its next start (decision
0111 — no --profile, no VIKE_RUN_PROFILE). Re-running it under the same name replaces the
stored body in place, so a key left off the second time is gone.

Every key is its dotted path in the run-profile document (docs/ops/run-profile-live.toml):
  --risk.max_notional_per_order 5000 --risk.max_total_exposure 25000
  --sinks.journal.snapshot_every 1024 --guards.max_drawdown 0.15
A run profile states no tier: where orders go is each account's tier
(`vike-cli secrets account set-tier`). A `demo`/`live` account's mount refuses to start without
the account-size ceilings.
The command journal's DIRECTORY is no run-profile key: the `config.journal_dir` row alone
turns the journal on and names it (`vike-cli config set config.journal_dir <dir>`);
[sinks.journal] only tunes the journal that row turns on.";

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
/// [`run_key`]. Document keys deliberately absent too: `name` (the ROW's name is the profile's
/// name), the `[event_source]` / `[broker]` tombstones and the retired `sinks.journal.dir` (the
/// journal's directory is the `config.journal_dir` row alone, decision 0111 amended 2026-10-11),
/// all three refused by name by `RunProfile::validate`. The test side holds this list equal to
/// `vike_core::RunProfile`'s leaves.
pub(super) const DOCUMENT_KEYS: &[(&str, Shape)] = &[
    ("sinks.gui", Shape::Switch),
    ("sinks.recorder", Shape::Switch),
    ("sinks.raw_capture_dir", Shape::Text),
    ("sinks.equity_sample_ms", Shape::Count),
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
pub(super) const TABLE_REQUIRES: &[(&str, &str)] = &[("guards.margin_call", "mm_requirement")];

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
    /// `path → stored TOML scalar`, one entry per key given.
    pub(super) settings: BTreeMap<String, String>,
    pub(super) dry_run: bool,
}

/// Every run-profile key's dotted path, leaked ONCE: `clap` names an argument by a `&'static str`,
/// and the key table is computed at run time (`[risk]`'s keys are `ProfileRisk::keys()`), so the
/// names are built a single time and kept for the life of the process.
fn key_names() -> &'static [&'static str] {
    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        all_run_keys().into_iter().map(|key| &*Box::leak(key.into_boxed_str())).collect()
    })
}

/// What a key's value must look like, in the words `--help` prints under its option.
fn shape_words(shape: Shape) -> String {
    match shape {
        Shape::Number => "a finite number".to_string(),
        Shape::Count => "a whole number, zero or more".to_string(),
        Shape::Switch => "true | false".to_string(),
        Shape::Text => "text".to_string(),
        Shape::Word(words) => words.join(" | "),
    }
}

/// `config bootstrap-run`'s arguments: a profile NAME, `--dry-run`, and **one option per run-profile
/// key, spelled as its dotted path** (`--risk.max_notional_per_order 5000`).
///
/// ⚠ **This is the one `clap` argument set in the binary that is not a `#[derive]`, and that is a
/// fact about its vocabulary rather than about the reader.** The keys are a TABLE
/// ([`DOCUMENT_KEYS`] plus `vike_model::ProfileRisk::keys()`), grown by adding a row in the crate
/// that owns the setting, so a struct field per key would be a hand copy that goes stale the day
/// a ceiling is added — exactly what `all_run_keys` exists to prevent. The command is therefore
/// built from that table, so `--help`, the parser and the table cannot disagree. It is still
/// `clap` all the way down: this impl only registers the arguments and reads them back.
#[derive(Debug)]
pub(crate) struct BootstrapRunCmd {
    names: Vec<String>,
    /// `(dotted path, value as typed)` for each key given, in the key table's order.
    values: Vec<(&'static str, String)>,
    dry_run: bool,
}

impl clap::Args for BootstrapRunCmd {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        let mut cmd = cmd
            .about(
                "Build the RUN profile FROM ARGUMENTS, every key its dotted path, and activate it",
            )
            .long_about(LONG_ABOUT)
            .arg(
                clap::Arg::new("name")
                    .value_name("NAME")
                    .num_args(0..)
                    .help("the profile NAME the body is stored under"),
            )
            .arg(
                clap::Arg::new("dry-run")
                    .long("dry-run")
                    .action(clap::ArgAction::SetTrue)
                    .help("print the document the daemon would parse, and write nothing"),
            );
        for path in key_names() {
            let shape = run_key(path).expect("every listed key has a shape");
            cmd = cmd.arg(
                clap::Arg::new(*path)
                    .long(*path)
                    .value_name("VALUE")
                    .help(format!("the run-profile key `{path}`: {}", shape_words(shape))),
            );
        }
        cmd
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        <Self as clap::Args>::augment_args(cmd)
    }
}

impl clap::FromArgMatches for BootstrapRunCmd {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let names = matches
            .get_many::<String>("name")
            .map(|given| given.cloned().collect())
            .unwrap_or_default();
        let values = key_names()
            .iter()
            .filter_map(|path| matches.get_one::<String>(path).map(|raw| (*path, raw.clone())))
            .collect();
        Ok(Self { names, values, dry_run: matches.get_flag("dry-run") })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = <Self as clap::FromArgMatches>::from_arg_matches(matches)?;
        Ok(())
    }
}

impl BootstrapRunCmd {
    fn into_args(self) -> Result<BootstrapRunArgs, String> {
        let mut name: Option<String> = None;
        for word in self.names {
            if name.is_some() {
                return Err(format!("unexpected extra argument '{word}'"));
            }
            name = Some(word);
        }
        let mut settings: BTreeMap<String, String> = BTreeMap::new();
        for (path, raw) in self.values {
            let shape = run_key(path).expect("every listed key has a shape");
            settings.insert(path.to_string(), lower_value(path, shape, &raw)?);
        }
        let dry_run = self.dry_run;

        let name = name.ok_or("missing the profile NAME")?;
        for (table, key) in TABLE_REQUIRES {
            let prefix = format!("{table}.");
            let required = format!("{table}.{key}");
            if settings.keys().any(|k| k.starts_with(&prefix)) && !settings.contains_key(&required)
            {
                return Err(format!(
                    "a `[{table}]` key was given without --{required}, which that table cannot \
                     exist without — the daemon would refuse the profile at its next start"
                ));
            }
        }
        Ok(BootstrapRunArgs { name, settings, dry_run })
    }
}

/// Read `config bootstrap-run`'s argv tail through the plane's own parser, so a test meets the
/// grammar an operator does.
#[cfg(test)]
pub(super) fn parse_bootstrap_run(
    argv: impl Iterator<Item = String>,
) -> Result<BootstrapRunArgs, args::Stop> {
    let argv = std::iter::once("bootstrap-run".to_string()).chain(argv);
    match crate::cmd::config::parse(argv)? {
        crate::cmd::config::Sub::BootstrapRun(cmd) => {
            cmd.into_args().map_err(|msg| args::Stop::message("vike-cli config bootstrap-run", msg))
        }
        other => unreachable!("`bootstrap-run` parsed as {other:?}"),
    }
}

/// The pre-trade ceilings a `demo`/`live` account's mount refuses to start without that this body
/// does not carry, as `--risk.<key>` flags. ⚠ DERIVED from `PRE_TRADE_CEILINGS`, never restated:
/// the ceilings whose home is the run profile's `[risk]` table and whose absence REFUSES such a
/// mount (`vike_mount::MountError::MissingRiskBudget`).
///
/// A WARNING's input, never a refusal (the module doc's "It takes no `--mode`"): a body states no
/// tier, so it cannot know whether it will reach a `demo`/`live` account, and a paper-only box runs
/// without these.
pub(super) fn missing_live_ceilings(settings: &BTreeMap<String, String>) -> Vec<String> {
    vike_config::ceilings::PRE_TRADE_CEILINGS
        .iter()
        .filter(|c| {
            c.home == vike_config::ceilings::CeilingHome::RunProfileRisk
                && c.refuses_live_mount_when_absent
        })
        .map(|c| format!("--risk.{}", c.name))
        .filter(|flag| !settings.contains_key(&flag[2..]))
        .collect()
}

/// The report's line for [`missing_live_ceilings`]: empty when the body carries every one.
fn live_ceiling_warning(settings: &BTreeMap<String, String>) -> String {
    let missing = missing_live_ceilings(settings);
    if missing.is_empty() {
        return String::new();
    }
    format!(
        "\n\n⚠ This body carries no {} — they depend on your account size, so there is no safe \
         default, and the mount of every `demo` or `live` account REFUSES TO START without them. \
         A `paper` account runs without; re-run this verb with them before a `demo`/`live` \
         account mounts.",
        missing.join(" and ")
    )
}

/// Entry point for `config bootstrap-run`.
pub(crate) fn run(cmd: BootstrapRunCmd, settings_dir: Option<&Path>) -> std::process::ExitCode {
    let args = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config bootstrap-run", msg),
    };
    match bootstrap(&args, settings_dir, vike_model::time::clock::now_secs()) {
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
    let arming = crate::cmd::config::activate::RUN_ARMING_WARNING;
    let ceilings = live_ceiling_warning(&args.settings);

    if args.dry_run {
        return Ok(format!(
            "would store run profile `{}` ({} key(s)) in {} and make it ACTIVE — NOTHING WAS \
             WRITTEN.{}{ceilings}{arming}\n\n--- the document the daemon would parse ---\n{}",
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
        "stored and ACTIVATED run profile `{}` in {} ({} key(s)).{}{ceilings}{arming}\n\n\
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

#[path = "tests/bootstrap_run.rs"]
#[cfg(test)]
mod config_run_bootstrap_tests;
