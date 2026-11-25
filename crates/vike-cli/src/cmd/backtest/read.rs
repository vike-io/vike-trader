//! The reading sub-verbs: their roster, their one flag parser and their dispatcher.
//!
//! The STORED-RUN half of the backtest plane: `vike-cli backtest ls | show | path | tag | diff |
//! gate | prune`.
//!
//! **Artifact-only — no engine, no store, no socket** (spec §6/§7). These work on a laptop with
//! neither, on runs minted months ago, remotely, or by someone else. The only inputs are
//! `<project>/user_data/runs/` and the marks store beside it, and the only schema is the COMMON
//! manifest `vike_runs` owns.
//!
//! # ⚠ One of the six WRITES, and it writes nothing a run depends on
//!
//! `tag` is the exception to "reading half", and it is deliberately a small one: it appends to a
//! run's OPTIONAL sidecar (`vike_runs::META_FILE`) and to the marks store, and touches
//! neither the manifest nor the report. Nothing it writes can make a run unreadable, which is what
//! lets it sit in this family rather than beside the engine.
//!
//! # The three JUDGING verbs, as a family
//!
//! [`tag`] gives a run a stable second name, [`diff`] shows what moved between two runs on both
//! sides of the arrow — inputs beside outputs — and [`gate`] turns a comparison into an EXIT CODE a
//! CI step branches on. They are one family because they share one operand grammar
//! ([`selector`]) and one pair of documents (`manifest.json` and `report.json`); a verb of the three
//! that accepted a different form in the same position is the fragmentation §15.6 forbids.
//!
//! # `--json`, and what a FAILURE looks like under it
//!
//! The document is emitted on SUCCESS only. A failure is a sentence on stderr plus a rung of
//! [`crate::exit`], and stdout carries nothing at all — the shape every sibling in this crate has
//! (`crate::cmd::data`'s `list_json`, `crate::cmd::secrets`'s `list`, `crate::cmd::init`). It is
//! deliberately NOT an `{"ok": false}` document.
//!
//! `vike_runs::RunScan::diagnostics` go to **stderr** in both modes, for the same reason: one
//! unreadable run directory must not make a machine-readable listing unparseable.

use std::path::Path;

use vike_hub_protocol::auth::HubKeys;

use crate::exit::CommandResult;
use crate::refusals::FlagRefusal;

use super::run_args::{PARAMS_REFUSED, refuse_a_run_flag_on_params};

pub(crate) mod diff;
pub(crate) mod failif;
pub(crate) mod gate;
pub(crate) mod jsondoc;
pub(crate) mod ls;
pub(crate) mod path;
pub(crate) mod prune;
pub(crate) mod selector;
pub(crate) mod show;
pub(crate) mod tag;
pub(crate) mod where_expr;

/// Everything the reading verbs need, resolved by the dispatcher and handed down whole.
///
/// It is a STRUCT rather than four parameters because five sub-verbs take the same set and the
/// dispatcher resolves all of it in one place — the rule
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` ratchets down: a `src/cmd/` file
/// may not resolve a project, read an environment variable or open the credential store for itself.
pub(crate) struct Context<'a> {
    /// `<project>/user_data/runs`, already `$VIKE_USER_DATA_DIR`-aware. `None` when there is no
    /// project above the working directory at all — an ordinary state, refused per verb with a
    /// sentence rather than assumed away.
    pub(crate) runs_root: Option<&'a Path>,
    /// `<project>/user_data/marks`, resolved beside [`Self::runs_root`] and never under it.
    ///
    /// ⚠ A SIBLING, because a `marks/` directory inside the runs root is — to
    /// `crates/vike-runs/src/scan.rs`'s `scan_runs`, which both readers go through — a run directory
    /// holding no manifest, i.e. a permanent "this run never finished writing" row in every listing.
    /// `vike_paths::state_path::MARKS_SUBDIR` carries the argument.
    ///
    /// `None` for the same reason `runs_root` is: no project above the working directory. A mark
    /// selector then refuses by saying there is nowhere to keep marks, which is a different fact
    /// from a mark that was never set.
    pub(crate) marks_root: Option<&'a Path>,
    /// `config.backtest_addr`, the MIDDLE rung of the compute-daemon address ladder. Only
    /// [`strategies`](crate::cmd::backtest::strategies) dials anything; every other reading verb is
    /// artifact-only.
    pub(crate) configured_address: Option<&'a str>,
    /// This box's device key for the vike-datahub protocol, when it holds one. A key-less dial is legal
    /// for an `Observe`-scope verb and is what an unconfigured laptop does.
    pub(crate) keys: Option<&'a HubKeys>,
    /// `<project>/strategies`, the strategy FILES `backtest params --strategy` reads a header of.
    /// `None` with no project above the working directory.
    pub(crate) strategies_dir: Option<&'a Path>,
    /// `config.saved_runs_per_strategy` as the backtesthub applies it on this box, which [`prune`] lists. `None`
    /// when the row is unset (the rule is off) or there is no project.
    pub(crate) retention: Option<&'a vike_runs::RunRetention>,
}

/// The STORED-RUN sub-verbs, which take a sub-verb TOKEN rather than flags.
///
/// ⚠ **Spec decision 11 — the sub-verb is ALWAYS required — HAS landed**, and this doc said the
/// opposite until it did. Stage 3 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` deleted the bare flag form
/// (`backtest --profile run.toml` is now an exit-2 naming the roster), and what this enum predicted
/// then happened: these arms joined [`super::Sub`], one variant each, all carrying the one [`ReadCommand`]
/// flag set that [`super::Sub::into_read`] names a [`ReadSub`] for. The PEEK is gone, and so is the hand
/// router (`claim_subcommand`) and its token lookup: `clap` claims the sub-verb word.
///
/// It stayed a SEPARATE enum rather than being flattened into [`super::Sub`] because the reading plane has
/// its own arity rule, its own foreign-flag refusals and its own parser, all of which name this
/// type; [`super::Sub`]'s own doc carries that argument.
///
/// ⚠ **The name says READ and two members WRITE.** [`ReadSub::Tag`] appends to a run's optional
/// sidecar and to the marks store; it touches no manifest and no report, so nothing it writes can
/// make a run unreadable — which is what lets it sit in this family.
/// `crate::cmd::backtest::read`'s module doc is where that exception is argued. [`ReadSub::Templates`] is the
/// second: it CREATES a `.rhai` file, and it may create only one that does not exist yet
/// (`crate::cmd::backtest::strategies`'s `run_templates` argues the no-clobber rule), so like `tag` it can
/// make nothing unreadable.
///
/// ⚠ **The name also says RUN, and the authoring three read no run at all.** `templates` and
/// `script-api` answer from consts compiled into this binary and `script-check` from one file the
/// operator named — no socket, no engine, no store, and no run directory either. They are in this
/// family because they share its parser, its arity rule and its foreign-flag refusals, and they
/// live in `crate::cmd::backtest::strategies` rather than under `crate::cmd::backtest::read` because that module's
/// identity is the run directory. Its module doc carries the whole argument, including why each is
/// a sub-verb rather than a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadSub {
    /// `ls [selector]` — the registry listing, the leaderboard and the job list, one verb.
    Ls,
    /// `show <id>` — re-render one stored run with no recompute.
    Show,
    /// `path <id> [FILE]` — one absolute path on stdout and nothing else, so `$(…)` works.
    Path,
    /// `tag <run>` — labels, notes and MARKS. The one member of this family that writes.
    Tag,
    /// `diff <a> <b>` — what differed in the INPUTS beside what differed in the OUTPUTS.
    Diff,
    /// `gate <run> --against <mark> --fail-if EXPR` — the verb whose product is an EXIT CODE.
    Gate,
    /// `params` — a strategy's or a script's tunable knobs, OFFLINE.
    Params,
    /// `strategies` — the roster the SERVER can run. The one reading verb that opens a socket.
    Strategies,
    /// `templates [ID]` — the starter Rhai strategies this binary ships, listed, printed, or saved
    /// to a NEW file. The one member of this family that CREATES a file, and it refuses to
    /// overwrite one.
    Templates,
    /// `script-api` — everything an authored Rhai strategy may CALL, derived from the host's own
    /// registration rather than from a list.
    ScriptApi,
    /// `script-check --script <s.rhai>` — compile one offline. The SECOND member of this family
    /// whose product is an exit code (`gate` is the first), which is why [`execute_read`] returns
    /// a rung.
    ScriptCheck,
    /// `prune --dry-run` — the saved runs `config.saved_runs_per_strategy` would remove, listed. It removes
    /// nothing (`--dry-run` is required): the backtesthub is the one remover (`prune`).
    Prune,
}

/// Every stored-run sub-verb, in the order the plane's help lists them. It is the roster the tests
/// hold the `clap` command to (`the_roster_carries_every_reading_subverb`) and the one
/// `crate::surface`'s `read_subcommands` row publishes, rendered through [`ReadSub::as_str`].
pub(crate) const READ_SUBCOMMANDS: &[ReadSub] = &[
    ReadSub::Ls,
    ReadSub::Show,
    ReadSub::Path,
    ReadSub::Tag,
    ReadSub::Diff,
    ReadSub::Gate,
    ReadSub::Params,
    ReadSub::Strategies,
    ReadSub::Templates,
    ReadSub::ScriptApi,
    ReadSub::ScriptCheck,
    ReadSub::Prune,
];

impl ReadSub {
    /// The name the operator typed, which is also what every refusal names it by. `const`, so the
    /// published roster is rendered from it at compile time.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            ReadSub::Ls => "ls",
            ReadSub::Show => "show",
            ReadSub::Path => "path",
            ReadSub::Tag => "tag",
            ReadSub::Diff => "diff",
            ReadSub::Gate => "gate",
            ReadSub::Params => "params",
            ReadSub::Strategies => "strategies",
            ReadSub::Templates => "templates",
            // ⚠ HYPHENATED, and the first two on this plane. `crate::cmd::data`'s
            // `fetch-starter`/`seed-demo` are the in-crate precedent. A bare `api` would read as
            // "the backtest plane's API" — which is not what it answers — and a bare `check` as
            // "check the profile"; both name the SCRIPT, so both say so.
            ReadSub::ScriptApi => "script-api",
            ReadSub::ScriptCheck => "script-check",
            ReadSub::Prune => "prune",
        }
    }
}

/// The parsed reading command line. Every field is shared across the reading sub-verbs and
/// refused by name where it does not apply — [`refuse_foreign_read_flags`], and the reason each
/// carries.
#[derive(Debug)]
pub(crate) struct ReadArgs {
    pub(crate) sub: ReadSub,
    /// The selector (`ls` filter, `show`/`path`/`tag`/`gate` target, `diff`'s LEFT operand). See
    /// `selector`.
    pub(crate) selector: Option<String>,
    /// `path`'s optional second positional: which FILE inside the run directory. Also `diff`'s RIGHT
    /// operand — one field rather than two, because in both cases it is "the second positional" and
    /// a second field would need the same per-sub-verb arity rule to decide which was populated.
    pub(crate) file: Option<String>,
    pub(crate) json: bool,
    pub(crate) out: Option<String>,
    // ls
    pub(crate) where_expr: Option<String>,
    pub(crate) sort: Option<String>,
    pub(crate) limit: Option<String>,
    pub(crate) cols: Option<String>,
    // show
    pub(crate) metrics: bool,
    /// `show --metrics-list` — print the METRIC CATALOG (every id, where it is stored, what it
    /// measures, and the declared-absent rows) and stop. It answers a question about the CATALOG
    /// rather than about a run, which is why it takes no selector and reads no runs directory:
    /// `vike_analytics::metric_catalog::metric_list_text` needs neither.
    ///
    /// ⚠ **It is the command THREE shipped refusals in that module already name**, and until this
    /// field existed those three told an operator to run something that exited "unknown option" —
    /// `parse_metric_selection`'s own doc measured that and called wiring the parser without this
    /// flag "the trap". This is the half of that debt this change pays; the SELECTION half
    /// (`--metrics` widened to take a value) is argued there and deliberately not ridden in here.
    pub(crate) metrics_list: bool,
    /// `show --html`: this run's tearsheet as a standalone HTML DOCUMENT, to `--out` or
    /// stdout.
    ///
    /// ⚠ It was a member of `show`'s `UNBUILT_RENDERERS` until this change,
    /// refused with a sentence blaming a missing renderer. The renderer was never missing:
    /// `vike_analytics::render_html` is exported under no feature at all, and
    /// `vike_analytics::TradingResultsTable::from_report`'s doc names `show.rs` by path as the caller it
    /// was built for. What was missing was the EDGE — a vike-report edge when this flag shipped,
    /// and since 2026-09-28 no new edge at all: both moved into vike-analytics, which this crate
    /// already linked for the metric catalog.
    ///
    /// ⚠ A BARE switch, not `--html PATH`: `--out` is already this verb's destination for a
    /// document (`--export` writes through it and `--metrics-list` composes with it), so a path
    /// on the flag would be a second spelling of the same thing. §6.2 spells `--html FILE`; this
    /// deviates deliberately and consistently with the two siblings in the same function.
    pub(crate) html: bool,
    /// `show --trades` renders the ledger; `diff --trades` adds it as a third diff section. Shared
    /// deliberately: it names the same document in both, and a `--trades` that meant two things
    /// would be the drift this one flag loop exists to prevent.
    pub(crate) trades: bool,
    pub(crate) config: bool,
    /// `--export VALUE` — one of `vike_runs`'s stored documents, raw. See
    /// `show`'s `export_document` for which values are served and why `fills`
    /// is refused by NAME rather than by refusing the flag.
    pub(crate) export: Option<String>,
    // tag
    /// `--add TAG`, REPEATABLE. A label on the run, in its own sidecar.
    pub(crate) add: Vec<String>,
    /// `--note TEXT`. Appended, never replacing — `vike_runs::add_tags` owns that rule.
    pub(crate) note: Option<String>,
    /// `--as NAME`. The MARK — the stable second operand `gate --against` takes.
    pub(crate) mark_as: Option<String>,
    // gate
    /// `--against <selector>`, REQUIRED by `gate`. Usually a mark, because a run id moves.
    pub(crate) against: Option<String>,
    /// `--fail-if EXPR`, REQUIRED by `gate`. See `failif`.
    pub(crate) fail_if: Option<String>,
    // diff
    /// `--all`: show unchanged leaves too. The negation of the default.
    pub(crate) all: bool,
    /// `--changed-only`: the DEFAULT, accepted so a script that says what it means is not refused.
    pub(crate) changed_only: bool,
    /// `--markdown`: the same rows as a Markdown table.
    pub(crate) markdown: bool,
    // params
    pub(crate) script: Option<String>,
    pub(crate) strategy: Option<String>,
    // strategies
    pub(crate) addr: Option<String>,
    // templates
    /// `--write-strategy FILE`: save the named starter to a file that does not exist yet.
    ///
    /// ⚠ **It is not `--out`, and that is a correctness choice rather than a naming one.** `--out`
    /// OVERWRITES on `ls` and `show` (`crate::surface`'s own row says so), and the file this one
    /// writes is a strategy somebody then edits — clobbering that from a flag typo is
    /// unrecoverable. So it takes `--write-profile`'s rule (refuse an existing path) and, because
    /// one flag may not carry two clobber policies, it takes its own name too.
    pub(crate) write_strategy: Option<String>,
    // prune
    /// `--dry-run`: list what the saved-run retention would remove. REQUIRED by `prune`, which removes nothing.
    pub(crate) dry_run: bool,
}

impl ReadArgs {
    /// Every flag at its default, for one sub-verb. `parse_read` fills it in and a unit test builds
    /// one directly — a struct literal in both places is two rosters that drift the day a field is
    /// added.
    pub(crate) fn empty(sub: ReadSub) -> Self {
        Self {
            sub,
            selector: None,
            file: None,
            json: false,
            out: None,
            where_expr: None,
            sort: None,
            limit: None,
            cols: None,
            metrics: false,
            metrics_list: false,
            html: false,
            trades: false,
            config: false,
            export: None,
            add: Vec::new(),
            note: None,
            mark_as: None,
            against: None,
            fail_if: None,
            all: false,
            changed_only: false,
            markdown: false,
            script: None,
            strategy: None,
            addr: None,
            write_strategy: None,
            dry_run: false,
        }
    }
}

/// The reading sub-verbs' arguments, as `clap` reads them: the plane's ONE flag set, shared by all
/// eleven sub-verbs, raw.
///
/// ⚠ **One set for eleven verbs, and that is the existing design rather than a shortcut.** A flag
/// that belongs to ANOTHER sub-verb is refused by name, with the reason, by
/// [`refuse_foreign_read_flags`] — `--where` on `show` is not "unknown", it is the `ls` filter, and
/// an operator who typed it wants the verb that takes it. That refusal is published in `cli.json`
/// (`crate::surface`'s `refusals`), so it stays the binary's actual answer; `clap` reads the
/// tokens and the sub-verb word, and the per-verb rules run after the parse as they always did.
// (`//` below the first paragraph: `clap` would print a doc comment as help.)
#[derive(clap::Args, Debug)]
pub(super) struct ReadCommand {
    // The selector (`ls` filter, `show`/`path`/`tag`/`gate` target, `diff`'s LEFT operand), then the
    // second positional: `path`'s FILE and `diff`'s RIGHT operand. Which sub-verb takes how many is
    // the arity rule in [`ReadCommand::into_args`].
    #[arg(
        value_name = "ARG",
        help = "a run selector (<run-id> | <unique-prefix> | @last | @last:<kind> | @mark); `path` takes a FILE inside the run, `diff` a second run, `templates` a starter id"
    )]
    positionals: Vec<String>,
    #[arg(long, help = "machine-readable output")]
    json: bool,
    #[arg(long, value_name = "FILE", help = "ls/show: write the rendered document to FILE")]
    out: Option<String>,
    // ls
    #[arg(long = "where", value_name = "EXPR", help = "ls: keep the runs a predicate holds for")]
    where_expr: Option<String>,
    #[arg(long, value_name = "FIELD[:asc|:desc]", help = "ls: order the rows")]
    sort: Option<String>,
    #[arg(long, value_name = "N", help = "ls: at most N rows")]
    limit: Option<String>,
    #[arg(long, value_name = "a,b,c", help = "ls: the columns to show")]
    cols: Option<String>,
    // show
    #[arg(long, help = "show: the metrics section of one stored run")]
    metrics: bool,
    // ⚠ The catalog LISTING, and it must sit as its own flag rather than fall anywhere near
    // `--metrics`: one selects a SECTION of a stored run, the other enumerates what this tree can
    // measure.
    #[arg(long, help = "show: print the metric CATALOG (no run needed)")]
    metrics_list: bool,
    // ⚠ A BARE switch, not `--html PATH`: `--out` is already this verb's destination for a document.
    #[arg(long, help = "show: this run's whole tearsheet as one HTML page, to --out or stdout")]
    html: bool,
    #[arg(long, help = "show: render the trade ledger; diff: compare it")]
    trades: bool,
    #[arg(long, help = "show: the run's config section")]
    config: bool,
    #[arg(long, value_name = "WHAT", help = "show: one stored document, raw (trades | equity)")]
    export: Option<String>,
    // tag
    // ⚠ REPEATABLE, and the only repeatable flag of the plane's reading verbs: `--add ci --add
    // fee-fix` is two labels, not the second overwriting the first.
    #[arg(
        long,
        value_name = "TAG",
        action = clap::ArgAction::Append,
        help = "tag: label the run; repeatable"
    )]
    add: Vec<String>,
    #[arg(long, value_name = "TEXT", help = "tag: append a note to the run")]
    note: Option<String>,
    #[arg(
        long = "as",
        value_name = "NAME",
        help = "tag: give the run a MARK a later comparison can name"
    )]
    mark_as: Option<String>,
    // gate
    #[arg(
        long,
        value_name = "SELECTOR",
        help = "gate: the baseline every criterion is relative to (usually a mark)"
    )]
    against: Option<String>,
    #[arg(long, value_name = "EXPR", help = "gate: the criteria, e.g. sharpe:-5%,max_dd:+10%")]
    fail_if: Option<String>,
    // diff
    #[arg(long, help = "diff: show unchanged leaves too")]
    all: bool,
    #[arg(long, help = "diff: only what changed (the default, accepted so a script can say it)")]
    changed_only: bool,
    #[arg(long, help = "diff: the same rows as a Markdown table")]
    markdown: bool,
    // params / script-check
    #[arg(long, value_name = "FILE", help = "params/script-check: the Rhai script")]
    script: Option<String>,
    #[arg(long, value_name = "FILE", help = "params: a strategy file's declared keys")]
    strategy: Option<String>,
    // strategies
    #[arg(long, value_name = "HOST:PORT", help = "strategies: the backtesthub to dial")]
    addr: Option<String>,
    // templates
    #[arg(long, value_name = "FILE", help = "templates: save the named starter to a NEW file")]
    write_strategy: Option<String>,
    // prune
    #[arg(long, help = "prune: list the saved runs the retention would remove; remove nothing")]
    dry_run: bool,
    // The flags that exist on a SIBLING sub-verb or are not built yet, registered hidden so they
    // are refused BY NAME rather than as typos. See [`HiddenRefusals`].
    #[command(flatten)]
    hidden: HiddenRefusals,
}

/// The run-only flags (`PARAMS_REFUSED`) and the unbuilt renderers (`UNBUILT_RENDERERS`), declared
/// hidden on every reading sub-verb so that each is refused BY NAME — `--cash` on `params` is not
/// "unknown", it belongs to `run` — with the sentence the published reference carries.
///
/// ⚠ Built from the two const rosters rather than typed, so a flag added to either is registered
/// here by construction, and `an_unbuilt_renderer_is_refused_with_what_it_would_need` /
/// `params_refuses_every_flag_on_the_roster` drive every entry through the real parser.
#[derive(Debug, Default)]
struct HiddenRefusals {
    /// The flags (as typed, with their dashes) that were present.
    given: Vec<&'static str>,
}

impl HiddenRefusals {
    fn roster() -> impl Iterator<Item = &'static str> {
        PARAMS_REFUSED.iter().copied().chain(show::UNBUILT_RENDERERS)
    }
}

impl clap::Args for HiddenRefusals {
    fn augment_args(mut cmd: clap::Command) -> clap::Command {
        for flag in Self::roster() {
            let name = &flag[2..];
            cmd = cmd.arg(clap::Arg::new(name).long(name).hide(true).num_args(0..=1));
        }
        cmd
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        <Self as clap::Args>::augment_args(cmd)
    }
}

impl clap::FromArgMatches for HiddenRefusals {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let given = Self::roster()
            .filter(|flag| {
                matches
                    .value_source(&flag[2..])
                    .is_some_and(|s| s != clap::parser::ValueSource::DefaultValue)
            })
            .collect();
        Ok(Self { given })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = <Self as clap::FromArgMatches>::from_arg_matches(matches)?;
        Ok(())
    }
}

impl ReadCommand {
    /// Everything the reading verbs decide about their own command line AFTER the parse: the
    /// arity of each sub-verb, the flags refused by name, and the per-verb rules.
    pub(super) fn into_args(self, sub: ReadSub) -> Result<ReadArgs, String> {
        // ⚠ Refused BY NAME, before anything else: a flag in the design document an operator is
        // reading from, or a run-only flag typed on `params`, is not "unknown option".
        for flag in &self.hidden.given {
            if show::UNBUILT_RENDERERS.contains(flag) {
                return Err(show::refuse_an_unbuilt_renderer(flag).rendered());
            }
        }
        if let Some(flag) = self.hidden.given.first() {
            return Err(if sub == ReadSub::Params && PARAMS_REFUSED.contains(flag) {
                refuse_a_run_flag_on_params(flag).rendered()
            } else {
                FlagRefusal::UnknownOption { flag: flag.to_string() }.rendered()
            });
        }

        let mut a = ReadArgs::empty(sub);
        a.json = self.json;
        a.out = self.out;
        a.where_expr = self.where_expr;
        a.sort = self.sort;
        a.limit = self.limit;
        a.cols = self.cols;
        a.metrics = self.metrics;
        a.metrics_list = self.metrics_list;
        a.html = self.html;
        a.trades = self.trades;
        a.config = self.config;
        a.export = self.export;
        a.add = self.add;
        a.note = self.note;
        a.mark_as = self.mark_as;
        a.against = self.against;
        a.fail_if = self.fail_if;
        a.all = self.all;
        a.changed_only = self.changed_only;
        a.markdown = self.markdown;
        a.script = self.script;
        a.strategy = self.strategy;
        a.addr = self.addr;
        a.write_strategy = self.write_strategy;
        a.dry_run = self.dry_run;
        let positionals = self.positionals;

        let allowed = match sub {
            ReadSub::Ls | ReadSub::Show | ReadSub::Tag | ReadSub::Gate => 1,
            ReadSub::Path | ReadSub::Diff => 2,
            ReadSub::Params | ReadSub::Strategies => 0,
            // `templates` takes an OPTIONAL starter id: absent is the roster, present is that
            // starter. ⚠ `script-check` takes NONE and names its file with `--script`, the same
            // spelling `params` uses for the same thing — a second grammar for "which .rhai file"
            // is exactly what decision 11 refuses across this plane.
            ReadSub::Templates => 1,
            ReadSub::ScriptApi | ReadSub::ScriptCheck | ReadSub::Prune => 0,
        };
        if positionals.len() > allowed {
            return Err(format!(
                "unexpected extra argument '{}' — `backtest {}` takes {allowed} positional \
                 argument(s)",
                positionals[allowed],
                sub.as_str()
            ));
        }
        match sub {
            // `ls` takes an OPTIONAL selector: absent is every run.
            ReadSub::Ls => a.selector = positionals.first().cloned(),
            // ⚠ `show --metrics-list` takes NO selector, and that is the flag's whole shape: it
            // answers out of the metric CATALOG, so there is no run for a selector to name and no
            // runs directory to read. `templates` makes the same move for the same reason. A
            // selector given anyway is kept and then unused, because the listing is the answer
            // either way, and refusing it would be a refusal about a token rather than about
            // anything an operator did wrong.
            ReadSub::Show if a.metrics_list => a.selector = positionals.first().cloned(),
            ReadSub::Show | ReadSub::Tag | ReadSub::Gate => {
                a.selector = Some(required_selector(sub, positionals.first())?);
            }
            ReadSub::Path => {
                a.selector = Some(required_selector(sub, positionals.first())?);
                a.file = positionals.get(1).cloned();
            }
            // ⚠ `diff` takes TWO runs and neither is optional. A one-operand `diff` that silently
            // used `@last` as the other side would compare against whatever happened to run most
            // recently, which is the "silent precedence" the selector grammar refuses everywhere
            // else.
            ReadSub::Diff => {
                a.selector = Some(required_selector(sub, positionals.first())?);
                a.file =
                    Some(positionals.get(1).filter(|s| !s.trim().is_empty()).cloned().ok_or_else(
                        || {
                            "`backtest diff` compares TWO runs — `backtest diff <a> <b>`. Name \
                             both; there is no implied second operand."
                                .to_string()
                        },
                    )?);
            }
            // ⚠ `templates` shares `ls`'s rule rather than `show`'s: an absent positional is the
            // WHOLE roster, not a missing operand. Requiring one would make the listing
            // unreachable, and the listing is the half no competitor's `new-strategy` prints.
            ReadSub::Templates => a.selector = positionals.first().cloned(),
            ReadSub::Params
            | ReadSub::Strategies
            | ReadSub::ScriptApi
            | ReadSub::ScriptCheck
            | ReadSub::Prune => {}
        }
        refuse_foreign_read_flags(&a)?;
        if sub == ReadSub::Show {
            show::refuse_a_listing_beside_a_run_rendering(&a)?;
            // ⚠ The SECOND of the two document rules, and both are needed because they answer
            // different questions: the one above refuses a CATALOG listing beside a run rendering,
            // this one refuses two RUN documents. They share
            // `show::run_rendering_flags_given`, which is what stops either from
            // being the file that forgets a flag the other knows about.
            show::refuse_a_second_document(&a)?;
        }
        if sub == ReadSub::Tag {
            tag::refuse_a_tag_that_writes_nothing(&a)?;
        }
        if sub == ReadSub::Gate {
            gate::refuse_an_ungateable_line(&a)?;
        }
        if sub == ReadSub::Prune && !a.dry_run {
            return Err(FlagRefusal::PruneWithoutDryRun.rendered());
        }
        Ok(a)
    }
}

/// Read a reading sub-verb's argv — the sub-verb word first, as the operator types it — through
/// the plane's own parser, so a test meets the grammar an operator does.
#[cfg(test)]
pub(super) fn parse_read(argv: &[&str]) -> Result<ReadArgs, crate::cmd::args::Stop> {
    match super::parse(argv.iter().map(|s| (*s).to_string()))? {
        super::Sub::Run(_) => unreachable!("a reading verb parsed as `run`"),
        sub => {
            let (word, cmd) = sub.into_read().expect("every other sub-verb is a reading one");
            cmd.into_args(word).map_err(|msg| crate::cmd::args::Stop::message(super::BIN, msg))
        }
    }
}

fn required_selector(sub: ReadSub, given: Option<&String>) -> Result<String, String> {
    match given {
        Some(s) if !s.trim().is_empty() => Ok(s.clone()),
        _ => Err(format!(
            "`backtest {}` requires a run selector — one of: {}",
            sub.as_str(),
            selector::FORMS
        )),
    }
}

/// A flag that exists on a SIBLING sub-verb is refused by name with the reason, never dropped —
/// `crate::cmd::data`'s `refuse_foreign_flags` carries the argument: an operator who typed it is not
/// looking for "unknown option", they want the verb that takes it.
fn refuse_foreign_read_flags(a: &ReadArgs) -> Result<(), String> {
    let ls_only = [
        ("--where", a.where_expr.is_some()),
        ("--sort", a.sort.is_some()),
        ("--limit", a.limit.is_some()),
        ("--cols", a.cols.is_some()),
    ];
    // ⚠ `--trades` is NOT here — `show` and `diff` both own it, so it is refused below by its own
    // named rule rather than by this roster. `--export` IS here: `show` alone serves a stored
    // document raw.
    // ⚠ `--metrics-list` is deliberately NOT here, and this roster's own SENTENCE is why: it says
    // the flag "selects a SECTION of one stored run", which is true of these three and FALSE of a
    // listing — that flag selects nothing and reads no run at all. A refusal that is untrue of the
    // flag it lands on is worse than a generic one, so it gets a named rule below. Note the reason
    // differs from `--trades`' and `--script`'s named rules (two sub-verbs own those); this one is
    // owned by `show` alone and still needs its own words.
    let show_only =
        [("--metrics", a.metrics), ("--config", a.config), ("--export", a.export.is_some())];
    let tag_only =
        [("--add", !a.add.is_empty()), ("--note", a.note.is_some()), ("--as", a.mark_as.is_some())];
    let gate_only = [("--against", a.against.is_some()), ("--fail-if", a.fail_if.is_some())];
    let diff_only =
        [("--all", a.all), ("--changed-only", a.changed_only), ("--markdown", a.markdown)];
    // ⚠ `--script` LEFT this roster when `script-check` shipped: two sub-verbs own it now, so it is
    // refused below by its own named rule, exactly as `--trades` is. A flag refused twice with two
    // messages is a flag whose two refusals can disagree.
    let params_only = [("--strategy", a.strategy.is_some())];
    let strategies_only = [("--addr", a.addr.is_some())];
    let templates_only = [("--write-strategy", a.write_strategy.is_some())];
    let prune_only = [("--dry-run", a.dry_run)];

    // Each roster is refused with its family's variant, built from the flag and the sub-verb.
    let refuse = |set: &[(&'static str, bool)],
                  refusal: fn(&'static str, &'static str) -> FlagRefusal|
     -> Result<(), String> {
        for (flag, given) in set {
            if *given {
                return Err(refusal(flag, a.sub.as_str()).rendered());
            }
        }
        Ok(())
    };
    if a.sub != ReadSub::Ls {
        refuse(&ls_only, |flag, sub| FlagRefusal::OnlyOnLs { flag, sub })?;
    }
    if a.sub != ReadSub::Show {
        refuse(&show_only, |flag, sub| FlagRefusal::OnlyOnShow { flag, sub })?;
    }
    // ⚠ `--metrics-list` is refused by a NAMED rule for a reason none of the other named rules
    // share: the `show_only` sentence above is FALSE of it. That roster tells an operator the flag
    // "selects a SECTION of one stored run", and this one prints the metric CATALOG — no run, no
    // section, no runs directory. Sending somebody to `show` is right; telling them why in words
    // that do not describe their flag is how a refusal stops being evidence for anything.
    // ⚠ `--html` gets a NAMED rule for the same reason `--metrics-list` does: the `show_only`
    // sentence above says the flag "selects a SECTION of one stored run", and this one renders the
    // WHOLE run as a single document. Sending somebody to `show` is right; describing their flag
    // wrongly on the way is how a refusal stops being evidence.
    if a.sub != ReadSub::Show && a.html {
        return Err(FlagRefusal::HtmlOnlyOnShow { sub: a.sub.as_str() }.rendered());
    }
    if a.sub != ReadSub::Show && a.metrics_list {
        return Err(FlagRefusal::MetricsListOnlyOnShow { sub: a.sub.as_str() }.rendered());
    }
    // ⚠ `--trades` is the FIRST flag TWO sub-verbs own (`--script` below is the second), so it is
    // refused on every other reading verb rather than living in either roster: `show --trades`
    // renders the ledger and `diff --trades` compares two of them, which is the same document
    // answering the same question from two sides.
    if !matches!(a.sub, ReadSub::Show | ReadSub::Diff) && a.trades {
        return Err(FlagRefusal::TradesOnlyOnShowOrDiff { sub: a.sub.as_str() }.rendered());
    }
    if a.sub != ReadSub::Tag {
        refuse(&tag_only, |flag, sub| FlagRefusal::OnlyOnTag { flag, sub })?;
    }
    if a.sub != ReadSub::Gate {
        refuse(&gate_only, |flag, sub| FlagRefusal::OnlyOnGate { flag, sub })?;
    }
    if a.sub != ReadSub::Diff {
        refuse(&diff_only, |flag, sub| FlagRefusal::OnlyOnDiff { flag, sub })?;
    }
    if a.sub != ReadSub::Params {
        refuse(&params_only, |flag, sub| FlagRefusal::OnlyOnParams { flag, sub })?;
    }
    // ⚠ `--script` is the SECOND flag two sub-verbs own, so it gets a named rule rather than a
    // roster row: `params` lists what a script DECLARES and `script-check` compiles it, which is
    // the same file answering two questions. Refusing it on `script-check` with the `params`
    // sentence would have sent an operator to the wrong verb — the exact cost
    // `crate::cmd::data`'s `refuse_foreign_flags` argues a named refusal exists to avoid.
    if !matches!(a.sub, ReadSub::Params | ReadSub::ScriptCheck) && a.script.is_some() {
        return Err(FlagRefusal::ScriptOnlyOnParamsOrScriptCheck { sub: a.sub.as_str() }.rendered());
    }
    if a.sub != ReadSub::Templates {
        refuse(&templates_only, |flag, sub| FlagRefusal::OnlyOnTemplates { flag, sub })?;
    }
    if a.sub != ReadSub::Prune {
        refuse(&prune_only, |flag, sub| FlagRefusal::OnlyOnPrune { flag, sub })?;
    }
    if a.sub != ReadSub::Strategies {
        refuse(&strategies_only, |flag, sub| FlagRefusal::OnlyOnStrategies { flag, sub })?;
    }
    // `--out` belongs to the two verbs that emit a RENDERED DOCUMENT nobody would want interleaved
    // with a terminal. `path` prints one line and `strategies` prints a roster; redirecting either
    // is the shell's job. ⚠ `gate` and `diff` are deliberately NOT here either: a gate's verdict is
    // what a CI step reads on stdout beside the rung, and both are already one clean stream a shell
    // redirect handles — the flag would be a second way to do what `>` does.
    // ⚠ `templates` is the one verb that WRITES and is still refused this flag, because its write
    // is not a rendering: `--write-strategy` refuses an existing path while `--out` overwrites, and
    // `ReadArgs::write_strategy` argues why one flag may not carry both policies.
    if !matches!(a.sub, ReadSub::Ls | ReadSub::Show) && a.out.is_some() {
        // ⚠ `templates` gets a POINTER rather than the bare sentence, because an operator typing
        // `--out` there is trying to SAVE a starter and the generic answer ("the shell can
        // redirect") would send them at a `>` that clobbers the strategy they are editing. Every
        // other sub-verb genuinely has nothing but a redirect, so it keeps the plain message.
        return Err(if a.sub == ReadSub::Templates {
            FlagRefusal::OutOnTemplates
        } else {
            FlagRefusal::OutNotARendering { sub: a.sub.as_str() }
        }
        .rendered());
    }
    Ok(())
}

/// ⚠ **The return is a RUNG, not `()`, and TWO sub-verbs need that.** `gate`'s product IS its exit
/// code: it evaluates every criterion, prints the whole verdict to stdout, and the number beside it
/// is that document's summary (`crate::exit::Exit::Breach`). Routing a breach through `CliError`
/// instead would print one stderr line and throw the document away — §7.1 requires the opposite.
/// `script-check` is the second and the same shape: its product is a compile diagnostic plus a
/// number a save hook branches on, so it too prints and returns rather than erroring (it lands on
/// `crate::exit::Exit::Failed`, and `crate::cmd::backtest::strategies`'s `run_script_check` argues why not
/// `Usage` and why not `Breach`). Every other arm answers `Exit::Ok` and is unchanged.
pub(super) fn execute_read(
    a: &ReadArgs,
    ctx: &Context<'_>,
    now: i64,
) -> CommandResult<crate::exit::Exit> {
    let ok = |r: CommandResult<()>| r.map(|()| crate::exit::Exit::Ok);
    match a.sub {
        ReadSub::Ls => ok(ls::run_ls(ctx, a)),
        ReadSub::Show => ok(show::run_show(ctx, a)),
        ReadSub::Path => {
            ok(path::run_path(ctx, a.selector.as_deref().unwrap_or_default(), a.file.as_deref()))
        }
        ReadSub::Tag => ok(tag::run_tag(ctx, a, now)),
        ReadSub::Diff => ok(diff::run_diff(ctx, a)),
        ReadSub::Gate => gate::run_gate(ctx, a),
        ReadSub::Params => ok(crate::cmd::backtest::params::run_params(
            a.script.as_deref(),
            a.strategy.as_deref(),
            ctx.strategies_dir,
            a.json,
        )),
        ReadSub::Strategies => {
            ok(crate::cmd::backtest::strategies::run_strategies(ctx, a.addr.as_deref(), a.json))
        }
        ReadSub::Templates => ok(crate::cmd::backtest::strategies::run_templates(
            a.selector.as_deref(),
            a.write_strategy.as_deref(),
            a.json,
        )),
        ReadSub::ScriptApi => ok(crate::cmd::backtest::strategies::run_script_api(a.json)),
        // ⚠ The SECOND arm that returns a rung rather than `()`. A script that does not compile is
        // an ANSWER — the diagnostic goes to stdout in full, `--json` document included — and the
        // number beside it is what a save hook or a pre-commit branches on.
        // `crate::cmd::backtest::strategies`'s `run_script_check` argues which rung and why not the other two.
        ReadSub::ScriptCheck => {
            crate::cmd::backtest::strategies::run_script_check(a.script.as_deref(), a.json)
        }
        ReadSub::Prune => ok(prune::run_prune(ctx, a)),
    }
}
