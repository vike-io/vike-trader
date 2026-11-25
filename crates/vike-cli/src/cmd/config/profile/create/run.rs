//! `vike-cli config profile create run` — **the run part's writer**: the operator's pre-trade risk
//! budget (`[risk]`), the core's guards (`[guards]`) and sinks (`[sinks]`) written as the RUN part of
//! a stored DAEMON profile, replacing that part and keeping the mounts and `daemon.*`. The second of
//! the one live profile's two writers (decision 0297; `crate::cmd::config::profile::create::daemon` writes
//! the mount part and keeps this one), and the same shape: built from argv, never from a file.
//!
//! # Why this exists
//!
//! Decision 0111 (verdict 4) made the daemon run profile ROWS ONLY: its `--profile` argument is
//! refused by name and `VIKE_RUN_PROFILE` by `vike_config::REMOVED_ENV`. Decision 0297 made those
//! rows the RUN part of the active daemon body (one live profile per trading daemon) and retired the
//! `run` kind: a name stored as a `run` body is refused here with the cure, and a name stored as no
//! daemon body is refused (`profile create daemon` stores one first).
//!
//! # It writes a body and never a selection
//!
//! The run part takes effect with the daemon profile it rides: the ACTIVE one at the daemon's next
//! restart, another one when `config profile activate daemon` selects it (with its mounts). Activating here
//! would let a ceiling edit swap the mount set, so the report says which case this is instead.
//!
//! # The argument grammar IS the document's
//!
//! Every key is a flag spelled as its DOTTED PATH in the run-profile document —
//! `--risk.max_notional_per_order 5000`, `--sinks.journal.snapshot_every 1024`,
//! `--guards.margin_call.maintenance_margin_requirement 0.05` — and becomes ONE `profile_setting` row at that
//! path, its value stored as the TOML scalar `vike_secrets::profile_store::render_run_toml` emits
//! verbatim. So there is no second vocabulary for an operator to learn: the keys are the run profile's
//! own, with `--` in front.
//!
//! The vocabulary is [`run_key`]: the `[risk]` keys are `vike_model::ProfileRisk::keys()` (serde's
//! own field list) and each one's shape is `vike_config::risk_key_kind`, asked of the real parser,
//! so nothing about `[risk]` is re-spelled here; every other key comes from [`DOCUMENT_KEYS`]. This crate links neither `vike-core` nor `vike-execution` (the `light-consumers`
//! lane holds it out of that closure), so the vocabulary is held to `vike_config::ProfileRun` from
//! the test side, BOTH ways (`crates/vike-cli/src/cmd/config/profile/create/tests/run.rs`): every key
//! this verb writes loads through the daemon's own loader, and every leaf of a fully-populated
//! `ProfileRun` is a key this verb writes. A key left off stores NO row, so the daemon's own
//! default stays the daemon's.
//!
//! # What is refused HERE, and what only the daemon refuses
//!
//! Refused at write, before anything is stored: an unknown key, a value of the wrong scalar shape,
//! a key given twice, and a nested table missing the key it cannot exist without
//! ([`TABLE_REQUIRES`]). ⚠ **The semantic refusals of `vike_config::ProfileRun::validate` arrive at
//! the daemon's next start, not here** — `max_leverage < 1.0`, `required_free_buying_power_percent` outside
//! `[0.0, 1.0)` — the same declared residual
//! `crate::cmd::config::profile::activate` carries, for the same linkage reason. The daemon treats a run row
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
//! (`vike_tradehub_core::EngineError::MissingRiskBudget`), and a paper-only box runs without. The
//! venue-owned grid fields (`tick_size`/`lot_size`/`min_qty`/`min_notional`) are refused at that
//! mount too, field by field; a `paper` account takes them.
//!
//! `--dry-run` is the inspection step, and it prints the document the daemon will parse.
//! Re-running the verb under the same name REPLACES the stored run part, so a key left off the
//! second time is gone, not kept.

use std::collections::BTreeMap;
use std::path::Path;

use vike_secrets::profile_store::{
    ProfileKind, ProfileOperatorWrite, ProfileRecord, ProfileStoreError, edit_profiles,
    is_run_part, read_profiles, render_run_toml,
};

use crate::cmd::args;

// The prose half of `config profile create run --help`; the usage line and the option table (one row per
// run-profile key) are `clap`'s, built from the key table below by [`BootstrapRunCommand`].
const LONG_ABOUT: &str = "\
Write the RUN part of a stored daemon profile — the pre-trade risk budget ([risk]), the core's
guards ([guards]) and sinks ([sinks]) — built entirely from these arguments, never from a file.
NAME is the DAEMON profile (`profile create daemon` stores one); its mounts and [daemon] are kept.
The active daemon profile's run part is what `vike-tradehub` builds its ceilings from at its
next start (decisions 0111 and 0297: no --profile, no VIKE_RUN_PROFILE, no `run` profile kind).
Re-running it under the same name replaces the stored run part, so a key left off the second
time is gone.

Every key is its dotted path in the run-profile document; the option table below lists them:
  --risk.max_notional_per_order 5000 --risk.max_total_exposure 25000
  --sinks.journal.snapshot_every 1024 --guards.max_drawdown 0.15
A run profile states no tier: where orders go is each account's tier
(`vike-cli account set-tier`). A `demo`/`live` account's venue refuses to start without
the account-size ceilings.
The command journal's DIRECTORY is no run-profile key: the `config.journal_dir` row alone
turns the journal on and names it (`vike-cli config set config.journal_dir <dir>`);
[sinks.journal] only tunes the journal that row turns on.
Four keys are RETIRED and are no flags here: sinks.gui, sinks.recorder, sinks.raw_capture_dir
and guards.freshness_ms were stored and validated and then applied by NO mount. A stored body
that still carries one is refused by name by the daemon and by `config profile activate`; re-run this
verb under the same name without it.";

/// The scalar SHAPE a key's value must have. The words are what a refusal prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cmd::config) enum Shape {
    /// A finite number — stored as a TOML float.
    Number,
    /// A whole number, zero or more — stored as a TOML integer.
    Count,
    /// `true` or `false`.
    Switch,
    /// One word of a closed vocabulary — stored as a TOML basic string.
    Word(&'static [&'static str]),
}

/// **Every run-profile key outside `[risk]`**, at its dotted path, in the document's own order.
///
/// `[risk]` is deliberately absent: its keys are `vike_model::ProfileRisk::keys()`, joined in by
/// [`run_key`]. Document keys deliberately absent too: `name` (the ROW's name is the profile's
/// name), the `[event_source]` / `[broker]` tombstones, the retired `sinks.journal.dir` (the
/// journal's directory is the `config.journal_dir` row alone, decision 0111 amended 2026-10-11) and
/// the four keys no mount ever applied (`sinks.gui`, `sinks.recorder`, `sinks.raw_capture_dir`,
/// `guards.freshness_ms`), every one of them refused by name by `ProfileRun::validate`. The test
/// side holds this list equal to `vike_config::ProfileRun`'s leaves.
pub(in crate::cmd::config) const DOCUMENT_KEYS: &[(&str, Shape)] = &[
    ("sinks.equity_sample_ms", Shape::Count),
    // ⚠ `true` journals every live forecast and moves the node's journal to format 32 at the first one
    // (`vike_config`'s `ProfileSinks::forecasts`: a rollback then needs every account flat).
    ("sinks.forecasts", Shape::Switch),
    ("sinks.journal.segment_bytes", Shape::Count),
    ("sinks.journal.flush_every", Shape::Count),
    ("sinks.journal.snapshot_every", Shape::Count),
    // The model's words, matched EXACTLY: the stored TOML is read by serde, whose per-variant alias is
    // the lowercase word and nothing else.
    ("guards.initial_trading_state", Shape::Word(&vike_model::TradingState::WORDS)),
    ("guards.submit_ack_timeout_ms", Shape::Count),
    ("guards.submit_ack_confirm_grace_ms", Shape::Count),
    ("guards.max_drawdown", Shape::Number),
    ("guards.conditionals_on_ticks", Shape::Switch),
    ("guards.margin_call.maintenance_margin_requirement", Shape::Number),
    ("guards.margin_call.warn_fraction", Shape::Number),
    ("guards.margin_call.buffer", Shape::Number),
];

/// A nested table and the key it cannot exist without (the field has no default, so a table
/// written without it fails the daemon's load). Pinned against the daemon's loader by the test
/// side.
pub(in crate::cmd::config) const TABLE_REQUIRES: &[(&str, &str)] =
    &[("guards.margin_call", "maintenance_margin_requirement")];

/// The shape of the key at `path`, or `None` when no run-profile key goes by it.
pub(in crate::cmd::config) fn run_key(path: &str) -> Option<Shape> {
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
pub(in crate::cmd::config) fn all_run_keys() -> Vec<String> {
    let mut out: Vec<String> = DOCUMENT_KEYS.iter().map(|(p, _)| (*p).to_string()).collect();
    out.extend(vike_model::ProfileRisk::keys().iter().map(|k| format!("risk.{k}")));
    out
}

/// One operator-typed value as the TOML scalar a `profile_setting` row stores.
///
/// # Errors
///
/// A refusal naming the flag, the value and the shape it must have.
pub(in crate::cmd::config) fn lower_value(
    path: &str,
    shape: Shape,
    raw: &str,
) -> Result<String, String> {
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
pub(in crate::cmd::config) struct BootstrapRunArgs {
    pub(in crate::cmd::config) name: String,
    /// `path → stored TOML scalar`, one entry per key given.
    pub(in crate::cmd::config) settings: BTreeMap<String, String>,
    pub(in crate::cmd::config) dry_run: bool,
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
        Shape::Word(words) => words.join(" | "),
    }
}

/// `config profile create run`'s arguments: a profile NAME, `--dry-run`, and **one option per run-profile
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
pub(crate) struct BootstrapRunCommand {
    names: Vec<String>,
    /// `(dotted path, value as typed)` for each key given, in the key table's order.
    values: Vec<(&'static str, String)>,
    dry_run: bool,
}

impl clap::Args for BootstrapRunCommand {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        let mut cmd = cmd
            .about(
                "Write a daemon profile's RUN part ([risk], [guards], [sinks]) FROM ARGUMENTS, \
                 every key its dotted path",
            )
            .long_about(LONG_ABOUT)
            .arg(
                clap::Arg::new("name")
                    .value_name("NAME")
                    .num_args(0..)
                    .help("the stored DAEMON profile the run part is written into"),
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

impl clap::FromArgMatches for BootstrapRunCommand {
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

impl BootstrapRunCommand {
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

/// Read `config profile create run`'s argv tail through the plane's own parser, so a test meets the
/// grammar an operator does.
#[cfg(test)]
pub(in crate::cmd::config) fn parse_bootstrap_run(
    argv: impl Iterator<Item = String>,
) -> Result<BootstrapRunArgs, args::Stop> {
    use crate::cmd::config::{CreateKind, ProfileVerb, Sub};
    parse_config_verb!(
        argv,
        "profile create run",
        cmd in Sub::Profile { verb: ProfileVerb::Create { kind: CreateKind::Run(cmd) } },
        "vike-cli config profile create run"
    )
}

/// The pre-trade ceilings a `demo`/`live` account's mount refuses to start without that this body
/// does not carry, as `--risk.<key>` flags. ⚠ DERIVED from `PRE_TRADE_CEILINGS`, never restated:
/// the ceilings whose home is the run profile's `[risk]` table and whose absence REFUSES such a
/// mount (`vike_tradehub_core::EngineError::MissingRiskBudget`).
///
/// A WARNING's input, never a refusal (the module doc's "It takes no `--mode`"): a body states no
/// tier, so it cannot know whether it will reach a `demo`/`live` account, and a paper-only box runs
/// without these.
pub(in crate::cmd::config) fn missing_live_ceilings(
    settings: &BTreeMap<String, String>,
) -> Vec<String> {
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
         default, and the venue of every `demo` or `live` account REFUSES TO START without them. \
         A `paper` account runs without; re-run this verb with them before a `demo`/`live` \
         account starts.",
        missing.join(" and ")
    )
}

/// Entry point for `config profile create run`.
pub(crate) fn run(cmd: BootstrapRunCommand, settings_dir: Option<&Path>) -> std::process::ExitCode {
    let args = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config profile create run", msg),
    };
    match bootstrap(&args, settings_dir, vike_model::time::clock::now_secs()) {
        Ok(report) => {
            println!("{report}");
            std::process::ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config profile create run: {msg}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// The refusal of a NAME stored as a retired `run` body (decision 0297): the cure names the active
/// daemon profile, the body these keys belong in now.
fn retired_run_body(name: &str, active_daemon: Option<&str>) -> String {
    let daemon = active_daemon.unwrap_or("<daemon profile>");
    format!(
        "`{name}` is a RETIRED `run` profile (decision 0297): the run part rides the daemon profile \
         now, and no `run` body is written any more. Nothing was written. Write these keys into \
         the daemon profile instead (`vike-cli config profile create run {daemon} …`, the same flags), \
         then deselect the old row: `vike-cli config profile deactivate run`."
    )
}

/// The body this run stores: the daemon body `held` stored under the name, its run part replaced by
/// `settings` and everything else kept. A name stored as no daemon body is refused, in the words
/// the report and the write share (the write asks it again of the body read under its own lock).
fn with_run_part(
    held: Option<&ProfileRecord>,
    name: &str,
    settings: &BTreeMap<String, String>,
    db: &Path,
    active_daemon: Option<&str>,
) -> Result<ProfileRecord, String> {
    match held {
        Some(held) if held.row.kind == ProfileKind::Daemon => {
            let mut stored = held.clone();
            stored.settings.retain(|path, _| !is_run_part(path));
            stored.settings.extend(settings.clone());
            Ok(stored)
        }
        Some(held) if held.row.kind == ProfileKind::Run => {
            Err(retired_run_body(name, active_daemon))
        }
        // `profile.name` is one namespace across the kinds; the words are the store's
        // (`ProfileStoreError::NameHeldByAnotherKind`), constructed, not re-spelled.
        Some(held) => Err(vike_secrets::profile_store::ProfileStoreError::NameHeldByAnotherKind {
            name: name.to_string(),
            held: held.row.kind.sql_word().to_string(),
            wanted: ProfileKind::Daemon,
        }
        .to_string()),
        None => Err(format!(
            "the settings store at {} holds no daemon profile called `{name}`. Nothing was \
             written: the run part is written INTO a stored daemon profile (decision 0297){}. \
             `vike-cli config profile create daemon {name} …` stores one first.",
            db.display(),
            active_daemon.map(|d| format!("; the ACTIVE one is `{d}`")).unwrap_or_default(),
        )),
    }
}

pub(in crate::cmd::config) fn bootstrap(
    args: &BootstrapRunArgs,
    settings_dir: Option<&Path>,
    now_utc: i64,
) -> Result<String, String> {
    let dir = settings_dir.ok_or(
        "no settings directory resolved — there is no store to write a profile into. Set \
         VIKE_SETTINGS_DIR, or run from a project that has one.",
    )?;
    let db = vike_secrets::db_path_in(dir);
    // The run part is written INTO a stored daemon body, so a store this verb cannot read is a
    // refusal: a body it cannot read is one whose mounts it could not keep.
    let profiles = read_profiles(&db).map_err(|e| e.to_string())?;
    let active_daemon = profiles.active(ProfileKind::Daemon).map(|p| p.row.name.as_str());
    let stored = with_run_part(
        profiles.by_name(&args.name),
        &args.name,
        &args.settings,
        &db,
        active_daemon,
    )?;
    let takes_effect = |active: bool| {
        if active {
            format!(
                " `{}` is the ACTIVE daemon profile: the daemon builds its ceilings from this run \
                 part at its next restart.",
                args.name
            )
        } else {
            format!(
                " `{}` is NOT the active daemon profile: this run part takes effect when `vike-cli \
                 config profile activate daemon {}` selects it, with its mounts.",
                args.name, args.name
            )
        }
    };
    let retired = profiles.active(ProfileKind::Run).map_or(String::new(), |run| {
        format!(
            "\n\n⚠ The retired `run` profile `{}` is still ACTIVE, and `vike-tradehub` refuses to \
             start until it is deselected (decision 0297): `vike-cli config profile deactivate run`.",
            run.row.name
        )
    });
    let arming = crate::cmd::config::profile::activate::RUN_ARMING_WARNING;
    let ceilings = live_ceiling_warning(&args.settings);

    if args.dry_run {
        return Ok(format!(
            "would write the run part of daemon profile `{}` ({} key(s)) in {}, keeping its \
             mounts and [daemon] — NOTHING WAS WRITTEN.{}{retired}{ceilings}{arming}\n\n\
             --- the run document the daemon would parse ---\n{}",
            args.name,
            args.settings.len(),
            db.display(),
            takes_effect(stored.row.active),
            render_run_toml(&stored).trim_end()
        ));
    }

    // ONE transaction: the merge happens AGAIN on the body read under the write's own lock, so a
    // `profile create daemon`, or any writer of this body, landing since the read above is kept.
    let write = ProfileOperatorWrite::claim("vike-cli config profile create run");
    let active = edit_profiles(&db, &write, now_utc, vike_model::AssetClass::SQL_WORDS, |t| {
        let held = t.read(&args.name)?;
        let body = with_run_part(held.as_ref(), &args.name, &args.settings, &db, active_daemon)
            .map_err(ProfileStoreError::EditRefused)?;
        t.store(&body)?;
        Ok(body.row.active)
    })
    .map_err(|e| e.to_string())?;

    Ok(format!(
        "wrote the run part of daemon profile `{}` in {} ({} key(s)); its mounts and [daemon] are \
         kept.{}{retired}{ceilings}{arming}\n\nRestart `vike-tradehub` to run it: it \
         reads the profile once, at boot. Re-running this verb under the same name with no key \
         leaves the profile with NO run part (the paper engines then run without an operator \
         budget and a live venue refuses to start).",
        args.name,
        db.display(),
        args.settings.len(),
        takes_effect(active),
    ))
}

#[path = "tests/run.rs"]
#[cfg(test)]
mod config_run_bootstrap_tests;
