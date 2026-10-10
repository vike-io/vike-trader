//! `vike-cli config show` — **settings provenance**: every setting, its EFFECTIVE value, and
//! WHERE that value came from.
//!
//! Phase 0 of the settings-unification design
//! (`docs/superpowers/specs/2026-08-04-settings-unification-design.md`). Settings live in eight
//! mechanisms today with no root and no stated precedence, so "why is this 150 ms?" cannot be
//! answered without grepping source, the credential store, TOML profiles and process env. This
//! command is that answer — and it is deliberately the FIRST phase because it doubles as the
//! regression harness for every later one: as stores move underneath, **this output must stay
//! stable**.
//!
//! Nothing here changes behavior. It reads two catalogs and resolves each against the stores that
//! exist today.
//!
//! # Usage
//!
//! ```text
//! vike-cli config show [--json] [--filter <substr>] [--changed-only] [--section files|env|all]
//! ```
//!
//! - `--section`: which half to print. `files` = the settings rows (the name outlived the files);
//!   `env` = the environment-variable registry; `all` (the default) = both. It exists because the
//!   env half is several hundred rows on its own, and the question that motivated the files half —
//!   *did my `policy` rows get read?* — should not require scrolling past them. ⚠ The row COUNT is
//!   deliberately no longer written here: it was, and the credential grid moved it by more than
//!   half again in one PR. [`vike_ops::settings::all_settings`] is the authority.
//! - `--filter <substr>`: case-insensitive substring match on the setting NAME/KEY or the reading
//!   CRATE/file (so `--filter reconcile`, `--filter bridges/okx` and `--filter policy` all work).
//! - `--changed-only`: only rows something actually configured — the "what have I set?" view.
//! - `--json`: one OBJECT carrying the settings directory, the per-file status, both row sets and
//!   the loader's warnings. (It was a bare ARRAY of the env rows before the files half existed;
//!   nothing in the tree consumed it.)
//!
//! ⚠ **`show` DISCLOSES; it does not judge.** Its exit code says only whether the loader refused,
//! so nothing here can be put in front of a daemon. That is the sibling verb's job —
//! [`crate::cmd::config::check`] resolves the same directory through the same loader and makes the
//! EXIT CODE the product, which is what a systemd `ExecStartPre=` can act on.
//!
//! # Two halves, because there are two kinds of setting
//!
//! **The files half** — the `policy`/`config`/`preferences`/`flags` rows of the settings database
//! (`<project>/settings/db/vike.db`), via [`vike_config::describe_with_source`]. Its provenance is
//! EXACT: an origin is decided by key PRESENCE in a parsed layer, so a `policy` row that sets a key
//! to the same number as the code default still reports `db`. This half is why the command exists
//! in its current shape. [`vike_config::Policy`] implements neither `EnvOverride` nor
//! `CliOverride` — both sealed — so a risk ceiling can ONLY be set in a settings row, and until
//! this half existed **nothing anywhere printed whether those settings had been read**. A typo'd
//! filename or a settings directory resolved from the wrong project produced a silently uncapped
//! node.
//!
//! **The env half** — the [`vike_ops::settings::all_settings`] registry, the machine-gated catalog of
//! every environment variable the workspace reads, resolved against the two stores below.
//!
//! # Precedence, and the one thing this command cannot know
//!
//! For the env half: **process env > the credential store > documented default.** The printed
//! `source` is the store that WON under that order, never the highest store that merely holds a
//! key. Presence is what wins, not non-emptiness: an exported `NAME=` overrides the store with an
//! empty string, and this command reports that honestly (`source = env`, empty value) rather than
//! silently falling through.
//!
//! ⚠ **"The credential store" is the settings database `<project>/settings/db/vike.db`, and
//! whether there IS one is a per-RUN fact this command asks about rather than assuming.** The
//! choice is `vike_secrets::backend_in`'s, asked ONCE in [`store_status`] and carried down as data,
//! so the header's store row, the precedence line, the stranded-row diagnosis, the unmatched-key
//! complement and `--json`'s `secrets` object all describe the store that answered; a per-KEY
//! fallback is the ladder `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids and is
//! not written anywhere here. `crates/vike-cli/src/cmd/config/check.rs`'s `store_findings` is the
//! sibling verb's twin.
//!
//! ⚠ **That order is what the STORES support. It is not a promise that any given reader honors
//! it** — most consumers read exactly ONE store, so a `source` word alone asserts a provenance the
//! tool cannot verify. Each row therefore also prints what its reader DOES:
//!
//! | `READS` | derived from | meaning |
//! |---|---|---|
//! | `env` | `Naming::Literal` / `Naming::Konst` | a direct `env::var` — the PROCESS environment |
//! | `caller-map` | `Naming::MapLookup` | a map the caller supplies; WHICH one is the caller's choice, which the registry's `Medium` column records and this column does not print |
//! | `unknown` | `Naming::Dynamic` | a computed name; the site is allowlisted, not resolved |
//!
//! and a row whose value came ONLY from the credential store while its reader calls `env::var` is
//! named in a warning under the table. That case is real: `VIKE_TRADEHUB_CONTROL_KEY` sits in the
//! store, `vike-cli`'s own `trade`/`mcp` read it with `std::env::var`, and a bare source word would
//! assert a source that is never consulted.
//!
//! ⚠ [`vike_ops::settings::Setting::layer`] is NOT that signal and must not be read as one — this
//! doc used to say it was. `Layer` is computed from the FILE PATH of the read (`main.rs` ⇒
//! `Binary`, a `src/` file ⇒ `Library`, …), so it records where the read LIVES, not which store it
//! consults: when this was written `VIKE_TRADEHUB_CONTROL_KEY` was `Binary` in `vike-app` (reading
//! the credential map) and `Library` in `vike-cli` (reading process env) — the exact inverse of any
//! layer-based rule. (Every row of it is an `Injected` map lookup now.)
//! `Naming` is the signal, because it is the column that distinguishes a direct `env::var` from a
//! map lookup, which is the actual question.
//!
//! # The `READ` column — the one thing this command must never get wrong
//!
//! Attributing a value to a file and printing its origin is an ASSERTION that the value is in
//! force. When nothing reads it, that assertion is positive confirmation of something false, which
//! is strictly worse than an unimplemented feature — an unimplemented feature has no output
//! claiming it works. A clean-install validation found five keys in exactly that state
//! (`flags.tradehub_control`, `config.tradehub_addr`, `config.log_dir`, `config.state_dir`,
//! `preferences.log_file_level`): all displayed as effective, none read by anything.
//!
//! So every row now carries `READ`, straight from [`vike_config::CONSUMPTION`] — the same table
//! `crates/vike-config/tests/settings_are_consumed.rs` gates by opening the claimed consumer's file
//! and looking for the read. A `NO` is followed, under the table, by what reads the key's
//! ENVIRONMENT VARIABLE instead, so the operator is pointed at the thing that works rather than
//! left with a value that does nothing. `policy.*` keys are exempt: they have their own gate
//! (`policy_is_consumed.rs`) and reporting one as unread would warn about an enforced ceiling.
//!
//! ## …and WHICH binary reads it
//!
//! `yes`/`NO` turned out not to be enough, for the same reason `READ` had to exist at all. A later
//! clean install, on a headless box, found `config.state_dir`, `config.store_root` and
//! `preferences.chart_style` all reporting `READ: yes` while their ONLY reader is `vike-app` — the
//! GUI, which will never execute on a tradehub or recorder host. Setting `config.state_dir` there
//! was measured to do nothing, which was CORRECT behaviour (it was vike-app's strategy-state
//! SIDECAR, not the `settings/state/` root the README names) — reported as though it had worked.
//! And `state_dir` is the obvious name for the directory an operator is looking for, so it was
//! precisely the row they landed on. (`config.state_dir` is deleted since: its one reader went with
//! the desktop's local core, and `consumed.rs` records the key's removal where its row stood.)
//!
//! The cell therefore names the BINARY (`desktop`, `tradehub`), DERIVED by
//! [`vike_config::Consumer::binary`] from the consuming file the table already records and the
//! gate above already opens — never a second hand-written column. `yes` survives for a LIBRARY
//! consumer, where the read genuinely belongs to every binary that links it and there is no single
//! honest name to print; `--json` carries the same answer as `read_by`, `null` for both `NO` and
//! the library case (which `consumed` disambiguates).
//!
//! # Redaction
//!
//! This output is meant to be pasted into issues, so **a secret's value never enters the program's
//! own data structures**: redaction happens inside `resolve`/`resolve_file_row`, not in either
//! printer, so the `--json` path cannot leak what the tables hide. A secret-shaped row prints
//! `<set>`/`<unset>` and still reports its true `source`. Store keys that match NO registry row —
//! [`unknown_store_keys`], the complement printed under the env table — are disclosed by KEY COUNT
//! only when they are credential-shaped: never a name, never a value (`vike-cli secrets list` is
//! the surface for names). And the files half is redacted by the SAME name shapes applied to each
//! key's leaf segment, so a credential-shaped field added to `Config` tomorrow is redacted by
//! construction rather than by anyone remembering.
//!
//! ⚠ Read that middle rule for what it is — a rule about the COMPLEMENT, not a promise that this
//! command names no credential key. It never was one (a declared credential row has always printed
//! its name with `<set>`/`<unset>`), and the ⚠ below is what that now amounts to in practice.
//!
//! ⚠ **What the default env table now discloses, stated plainly.** The registry declares the whole
//! `{VENUE}_{TIER}_API_*` grid since it became enumerable data (`vike_model::credential_keys`), so
//! this table NAMES every one of those keys and marks each `<set>` or `<unset>` — i.e. pasting the
//! default output into an issue reveals which venues and which tiers the box holds credentials for.
//! That is a real change and it is recorded here rather than left to be discovered. Three things
//! bound it, and they are why the answer was to state it rather than to hide the rows: no VALUE can
//! leak (every one of those names ends in `_API_KEY`/`_API_SECRET`/`_API_PASSPHRASE`, which
//! [`is_secret`] matches, and this file's own `no_settings_row_leaks` is the standing gate, driven
//! off `all_settings()` so it covers the grid rows automatically); the NAMES are not a new disclosure
//! class, because `vike-cli secrets list` names store keys outright and is documented as the
//! surface that does; and this table was ALREADY disclosing exactly this for whichever grid keys a
//! test fixture happened to spell, so what changed is that an accidental subset became a complete
//! one. **The narrow view for a paste is `--filter`, and it is the only one.** ⚠ `--changed-only`
//! is the WRONG instinct here and reads like the right one: it keeps precisely the rows something
//! actually set (`changed_only_keeps_the_rows_a_store_actually_set` and
//! `changed_only_drops_every_defaulted_row` pin that semantics), so on this table it strips the
//! `<unset>` noise and prints exactly the list of venues and tiers the box holds credentials for —
//! the sensitive half of the disclosure this paragraph exists to record, concentrated. `--filter`
//! narrows by substring and so requires knowing what to exclude, which is a real cost and still the
//! honest answer. (The attribution codes print in the clear, correctly: a builder address or broker
//! tag is public.)
//!
//! # The one thing the two tables cannot show: a key that is in the store and in no registry
//!
//! Both tables are driven by CATALOGS — the settings types and the `all_settings()` registry — so a store
//! key that matches no row of either is not a `<unset>` row, it is **no row at all**. A typo'd
//! variable name therefore looked EXACTLY like one that was never set, which is the same
//! silent-by-construction failure this whole command exists to remove (found by a clean-install
//! validation: `VIKE_NODE_CONTROL_KEY`, a plausible mis-spelling of `VIKE_TRADEHUB_CONTROL_KEY`,
//! sat in the store and appeared nowhere). [`unknown_store_keys`] is the complement, printed under
//! the env table.
//!
//! ⚠ It is split by NAME SHAPE, and the split is the design, not a convenience. Naming every
//! unmatched key would make this command an enumerator of the credential store, which is the one
//! thing the redaction rule above forbids; counting all of them would be useless, because a real
//! store holds bespoke per-venue credentials (`FXCM_{TIER}_USER`, `DUKASCOPY_DEMO1_LOGIN`,
//! per-account ids) that no registry row matches, so on a real box that count is a large,
//! meaningless number. So: credential-SHAPED unmatched keys are counted and never named, everything
//! else is named outright. See [`unknown_store_keys`] for the full argument — including why this
//! count used to be larger still, back when the `{VENUE}_{TIER}_API_*` grid was undeclared.
//!
//! # The stores are read through the workspace's ONE loader
//!
//! `vike_bridge_core::credentials`' binary-facing secrets reader is what every composition root
//! uses, so this command calls it rather than parsing the file itself — a second parser would drift
//! from the first and nothing would catch it, which is precisely the failure this whole program
//! exists to remove. It also takes `VIKE_SETTINGS_DIR` out of the same env sweep, so this command's
//! store is the store every other surface reads; the previous CWD-walking entry point did not, and
//! could name a directory nothing else used. The DIRECTORY itself comes from the dispatcher, which
//! resolved it once for `secrets` and `trade`.
//!
//! Reaching it used to mean linking the venue transport stack (ureq+rustls, tungstenite, the
//! signers, vike-exec) into a CLI whose entire identity is being light. That was a packaging
//! problem, not a reason to copy code. vike-bridge-core carries a `full` feature (default ON, so
//! every bridge is unchanged) that gates exactly the transport/signing modules, and this crate
//! takes it with `default-features = false` — one definition, no transport stack.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cmd::args::{self, Flags};
use crate::cmd::config::check::DirOrigin;

mod activate;
mod bootstrap_daemon;
mod bootstrap_recorder;
mod bootstrap_run;
pub(crate) mod check;
mod mirror;
mod mirror_profile;
mod mirror_recorder;
mod recorder;
mod resolve;
mod retired_env;
mod set;
mod show_env;
mod show_human;
mod show_json;
mod store;
mod venue;

use self::store::execute;

#[cfg(test)]
use self::resolve::{Reads, Source, UnknownKeys, resolve, resolve_all, unknown_store_keys};
#[cfg(test)]
use self::show_human::{print_ceilings, print_human, print_profile_risk};
#[cfg(test)]
use self::show_json::{print_json, settings_json};
#[cfg(test)]
use self::store::store_status;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use vike_config::file_rows;
#[cfg(test)]
use vike_ops::settings::{Naming, Setting, all_settings};

// The redaction SHAPES, and the words a redacted row prints — see the Redaction section below
// for why they live one crate down.
#[cfg(test)]
use vike_config::redact::{REDACTED, SET, UNSET, is_secret};

#[cfg(doc)]
use self::resolve::unknown_store_keys;
#[cfg(doc)]
use self::store::store_status;
#[cfg(doc)]
use vike_config::redact::is_secret;

/// The `config` verb list. Printed by `config --help` and by the unknown-verb arm, so a verb cannot
/// be added without showing up in help.
const USAGE: &str = "\
usage: vike-cli config <verb> [options]

verbs:
  show    every setting, its effective value, and WHERE that value came from
  check   validate this box's settings store + credential store; the EXIT CODE is the product
  set     write ONE row into the settings database, and record it in the change journal

  retired-env  read KEY=VALUE lines on stdin and print the startup refusal for any RETIRED
               variable among them — the deploy pre-flight's judge

  mirror  store a daemon/recorder PROFILE document as rows (there are no settings files any
          more — 0086 — and the run profile is written by bootstrap-run, 0111)

  recorder  print the recorder profile ROWS, leading with the store that answered

  activate    make a STORED profile (run|daemon|recorder) the one this box READS from its next
              restart; --proves <file> is required and the rows must render back to it
  deactivate  clear that kind's active row (no rung is left below it: with no run row a live
              mount refuses to start, with no daemon row the daemon does); the stored bodies are
              untouched, so it is the rollback and needs no redeploy

  bootstrap-daemon  build a daemon profile FROM ARGUMENTS (no file, ever) and activate it — the
                    one act that gets a box with no profile at all to a running paper mount

  bootstrap-recorder  the recorder twin of bootstrap-daemon: build a ONE-subscription recorder
                      profile FROM ARGUMENTS and activate it — the one act that gets a box with no
                      recorder profile at all to a running recording data daemon

  bootstrap-run  build the RUN profile (the [risk] budget, [guards], [sinks]) FROM ARGUMENTS —
                 every key its dotted path, `--risk.max_notional_per_order 5000` — and activate
                 it: the only place the trading daemon reads a run profile from (0111)

run `vike-cli config <verb> --help` for a verb's own options";

const SHOW_USAGE: &str = "usage: vike-cli config show [--json] [--filter <substr>] \
                          [--changed-only] [--section files|env|all]";

// ---------------------------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------------------------
//
// ⚠ The SHAPES moved DOWN to `vike_config::redact`; the PROOF stayed here. This command was the
// only settings-disclosure surface when they were written, so they lived beside the printer. It is
// not any more — `vike_config::boot` renders the same rows into a daemon's startup log — and a
// second copy of a SECURITY table is the one duplication this tree cannot afford: a venue added to
// one list and not the other prints a live key into a log file.
//
// What could not move is `no_settings_row_leaks`, the test that runs the shapes over the REAL
// `vike_ops::settings::SETTINGS` registry: vike-config sits far below vike-ops and cannot see it.
// So the shapes have one home and the exhaustiveness gate has another, which is the correct split —
// the table is a definition, the registry sweep is evidence about this workspace.

// `SET` / `UNSET` — the two words a redacted row prints — moved DOWN with the shapes and are
// imported at the top of this file. Same argument: `vike_config::boot` prints the same rows into a
// daemon's startup log, and an operator comparing that block against this table is reading ONE
// fact, so two surfaces spelling the sentinel differently would make one value look like two.

// `REDACTED` — the DEFAULT-column sentinel for a secret row that somehow declares a non-empty
// default — moved down with them (`vike_config::redact::REDACTED`, imported at the top) the day
// the FILES-half row builder became `vike_config::show`; the env half below still uses it.

// ---------------------------------------------------------------------------------------------
// Command entry
// ---------------------------------------------------------------------------------------------

/// Which half to print.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Section {
    /// The settings rows only (the name outlived the files).
    Files,
    /// The environment-variable registry only.
    Env,
    #[default]
    All,
}

impl Section {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "files" => Ok(Section::Files),
            "env" => Ok(Section::Env),
            "all" => Ok(Section::All),
            other => Err(format!("unknown --section '{other}' (expected files|env|all)")),
        }
    }
    fn files(self) -> bool {
        self != Section::Env
    }
    fn env(self) -> bool {
        self != Section::Files
    }
}

/// The parsed `config show` command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    json: bool,
    filter: Option<String>,
    changed_only: bool,
    section: Section,
}

/// Everything the `config` family needs from the dispatcher's ONE boot walk and its ONE clock read.
///
/// It was three positional parameters until `set` landed — a WRITER needs two facts a reader does
/// not (the ledger's home and the instant to stamp a record with), and five positional `Option`s in
/// a row is the signature nobody can read a call site of. The same argument
/// `crate::cmd::secrets`' `Ctx` makes, and the same rule behind it: a `src/cmd/` file reads no
/// environment and performs no second walk of its own.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// THE settings directory the dispatcher already resolved (`VIKE_SETTINGS_DIR`, else the
    /// project walk) — the same value `secrets` and `trade` receive. A parameter rather than a
    /// re-derivation, so this command can never report or WRITE a different directory than the one
    /// every other surface reads.
    pub settings_dir: Option<&'a Path>,
    /// Which of those two rungs answered — an answer only the dispatcher has, and one
    /// [`crate::cmd::config::check`] needs: a NAMED directory that is not there is a refusal, while
    /// a WALKED one that is not there is an unconfigured checkout.
    pub origin: DirOrigin,
    /// That directory's `state` child, off the SAME walk: the change journal's home for
    /// [`crate::cmd::config::set`]. `None` journals nothing and still writes.
    pub state_dir: Option<&'a Path>,
    /// The instant a journal record is stamped with, read once by the dispatcher because
    /// `vike_model::change_journal` reads no clock.
    pub now_ms: i64,
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `config` subcommand — the
/// first of which is the verb (`show`, `check` or `set`).
pub fn run(mut args: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
    let verb = match args.next() {
        Some(v) => v,
        None => {
            eprintln!("vike-cli config: missing verb\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match verb.as_str() {
        "show" => run_show(args, ctx.settings_dir),
        "check" => crate::cmd::config::check::run(args, ctx.settings_dir, ctx.origin),
        "set" => crate::cmd::config::set::run(
            args,
            crate::cmd::config::set::Ctx {
                settings_dir: ctx.settings_dir,
                state_dir: ctx.state_dir,
                now_ms: ctx.now_ms,
            },
        ),

        // The deploy helper's judge — see `crate::cmd::config::retired_env`.
        "retired-env" => crate::cmd::config::retired_env::run(args),

        // The PROFILE mirror (0086's *Phasing* leaves this as a going concern: a run/daemon/
        // recorder profile is still a document, unlike the four settings files this verb used to
        // ALSO carry). It takes the dispatcher's own settings directory like every verb above, and
        // deliberately takes neither `state_dir` nor `now_ms`: a mirror states no new value — it
        // copies a document an operator already filed — so it journals nothing. See
        // `crate::cmd::config::mirror`'s module doc.
        "mirror" => crate::cmd::config::mirror::run(args, ctx.settings_dir),

        // The recorder profile, READ back out of the store. It is the replacement for the
        // `grep '^store' <root>/settings/recorder.toml` an operator ran (and which two shipped
        // units embedded in their troubleshooting comments) — see
        // `crate::cmd::config::recorder`'s module doc for why a read verb was unavoidable rather
        // than a convenience.
        "recorder" => crate::cmd::config::recorder::run(args, ctx.settings_dir),

        // WHICH stored profile this box reads — the owner's *the ROW wins* ruling (0057 Question 3)
        // as an operator act. `crate::cmd::config::activate` carries why it is a SEPARATE verb from
        // `mirror` and why `--proves <file>` is required: a mirror stores a body and cannot see
        // what is in force, so `plan_active_row` could only ever withhold here — the crossing needs
        // a PROOF rather than a guess. Neither journals: they state no VALUE, only which ARTIFACT
        // this box reads.
        "activate" => crate::cmd::config::activate::run_activate(args, ctx.settings_dir),
        "deactivate" => crate::cmd::config::activate::run_deactivate(args, ctx.settings_dir),

        // The bootstrap rung `config activate` cannot serve — see
        // `crate::cmd::config::bootstrap_daemon`'s module doc for why a box with no profile at all
        // needs a writer that takes no file to prove against.
        "bootstrap-daemon" => crate::cmd::config::bootstrap_daemon::run(args, ctx.settings_dir),

        // The recorder twin — see `crate::cmd::config::bootstrap_recorder`'s module doc. Same
        // rung, same reason: `config activate recorder <name> --proves <file>` cannot help a box
        // with no recorder profile at all, because there is no file to prove against.
        "bootstrap-recorder" => crate::cmd::config::bootstrap_recorder::run(args, ctx.settings_dir),

        // The RUN profile's writer, and since decision 0111 its only one: the daemon reads the
        // active `run` row and no file — see `crate::cmd::config::bootstrap_run`'s module doc.
        "bootstrap-run" => crate::cmd::config::bootstrap_run::run(args, ctx.settings_dir),
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!(
                "vike-cli config: unknown verb '{other}' (expected `show`, `check`, `set`, \
                 `retired-env`, `mirror`, `recorder`, `activate`, `deactivate`, \
                 `bootstrap-daemon`, `bootstrap-recorder` or `bootstrap-run`)\n{USAGE}"
            );
            ExitCode::FAILURE
        }
    }
}

/// `config show`: parse flags, read the stores, print.
///
/// ⚠ The two help paths of this command are DIFFERENT code: the bare verb (`config --help`, in
/// [`run`] above) always printed its usage to stdout and exited 0, while `config show --help`
/// arrived here as the shared parser's short-circuit and hit the usage-error arm — exit 1, with the
/// internal token on stderr. That is the drift [`args::exit_for_parse_error`] exists to end.
fn run_show(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("config show", SHOW_USAGE, &msg),
    };
    match execute(&args, settings_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("vike-cli config show: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// Hand-rolled arg parser over the shared [`crate::cmd::args`] glue (no `clap` — this crate adds no
/// dependency): `--flag value` and `--flag=value`.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--json" => {
                args::no_value(&flag, inline)?;
                out.json = true;
            }
            "--changed-only" => {
                args::no_value(&flag, inline)?;
                out.changed_only = true;
            }
            "--filter" => out.filter = Some(flags.value(&flag, inline)?),
            "--section" => out.section = Section::parse(&flags.value(&flag, inline)?)?,
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(out)
}

/// The credential store's own status — **the store that ANSWERED**, not the file this command
/// could always name. Key COUNT only: never a name, never a value.
///
/// ⚠ **Every field describes ONE store** — the path, the presence bit and the loader's key count
/// are three facts about the settings database, so no two of them can describe different stores
/// and read as one confident answer.
struct StoreStatus {
    /// Which store answered — `vike_secrets::backend_in`'s per-RUN answer, asked ONCE in
    /// [`execute`] and carried here as DATA. Nothing below re-probes, so no printed cell can
    /// disagree with any other.
    backend: vike_secrets::Backend,
    /// The store, as a path: the settings database — the only credential store — whether or not it
    /// exists. `None` only when no settings directory resolved at all.
    path: Option<PathBuf>,
    /// Whether that store is on disk. A `Backend::Database` is present by construction — the probe
    /// that produced it IS an `is_file` — and `Backend::Absent` is not.
    present: bool,
    /// How many keys the loader returned, i.e. how many the ANSWERING store holds.
    keys: usize,
}

impl StoreStatus {
    /// The FILE NAME the header's store row is labelled with — `vike.db`, the only credential
    /// store.
    fn label(&self) -> &'static str {
        vike_secrets::DB_FILE
    }

    /// The machine word for WHICH KIND of store answered — the same vocabulary
    /// `vike-cli secrets list --json`'s `kind` field prints, so a tool reading both surfaces is
    /// reading one fact.
    fn kind(&self) -> &'static str {
        match self.backend {
            vike_secrets::Backend::Absent => "absent",
            vike_secrets::Backend::Database(_) => "database",
        }
    }

    /// How the answering store is NAMED in a sentence: its resolved path when there is one, else
    /// the conventional `settings/<file>` shorthand the no-directory arm falls back to.
    fn named(&self) -> String {
        match &self.path {
            Some(p) => p.display().to_string(),
            None => {
                format!("{}/{}", vike_model::paths::state_path::PROJECT_SETTINGS_DIR, self.label())
            }
        }
    }
}

/// **The run profile's `[risk]` values, read off the PROFILE BODY plane, plus WHICH body the box
/// reads.**
///
/// ⚠ The carrier is still `vike_secrets::ProfileRiskSource` / `StoredProfileRisk`, deliberately:
/// those are pure DATA types with no store behind them, every renderer and every JSON field in this
/// file is already built on them, and rebuilding all of that to change where the rows come FROM
/// would be churn with no reader-visible benefit. What is new is [`Self::active`], and it is the
/// half that matters — on this plane a row can BIND.
struct RunProfileRows {
    /// The `[risk]` rows of every stored `run` profile, or the store's own account of why there
    /// are none.
    source: vike_secrets::ProfileRiskSource,
    /// The name of the ACTIVE `run` profile, when one is selected. `Some` means the daemon builds
    /// its pre-trade ceilings from that body, so the rows below are ENFORCEABLE and the block must
    /// say so; `None` means the daemon runs with no run profile at all (decision 0111 left no file
    /// rung below the row).
    active: Option<String>,
}

#[cfg(test)]
mod tests;
