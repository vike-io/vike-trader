//! `backtest`'s command line, as `clap` types (decision 0155): [`BacktestArgs`] is the whole
//! program, [`RunArgs`] the run verb (the profile, the search flags, `--addr`, the retired
//! spellings), [`Verb`] the one word that routes BEFORE the profile (`trials`).
//!
//! ⚠ **This file READS the command line and decides nothing.** Every rule that is not the shape of
//! the argv — the search ownership table, the profile-given-twice refusal, the `--store` refusal —
//! stays in the file that owned it, after the parse, over these fields. What `clap` replaced: `vike_utility::argv::{arg, has_flag}` and the four helpers of this
//! file (`parse_addr_flag`, `flag_given`, `PROFILE_PATH_VALUED`, the positional scan), all deleted.
//!
//! ⚠ **The flag VOCABULARY is `vike_model::run_request::flag_vocab`'s, and this struct mirrors it.**
//! `vike-cli backtest run` is the other reader of the same flags, so
//! `crates/vike-backtesthub/src/backtest_cli/tests/args.rs`'s
//! `the_clap_struct_declares_every_flag_the_vocabulary_names` holds the two together: a row the
//! vocabulary adds and this struct forgets reddens there, by name.
//!
//! ⚠ **Two flags are declared only so the refusal that names them can be REACHED.** `--store`
//! (refused on every reader by name, decision 0084) and `--search` are hidden arguments.
//!
//! `trials` is a real subcommand, routed only when the word comes FIRST
//! (`args_conflicts_with_subcommands`): anywhere else it is the positional profile, and
//! [`profile_from_args`] refuses that by name.

use clap::{Args, Parser, Subcommand};
use vike_utility::cli::Program;

use super::usage::SUBCOMMANDS;

/// The program name every refusal and usage line carries.
pub(super) const BIN: &str = "backtesthub";

/// The line `-V`/`--version` prints: `<name> <version>` with the name spelled literally, because
/// `CARGO_PKG_NAME` is the PACKAGE (`vike-backtesthub`) while the binary is `backtesthub`, and a
/// `--version` answering with a name the caller did not invoke is what a bug report cannot use.
/// (No build identity: this crate cannot name `vike-buildinfo`, see `identity_adoption.rs`'s
/// `WITHOUT_IDENTITY`.)
pub(super) fn version_line() -> String {
    format!("{BIN} {}", env!("CARGO_PKG_VERSION"))
}

/// The program, for a parse or for rendering a usage line.
pub(super) fn program(version_line: &str) -> Program<'_> {
    Program::new(BIN, version_line)
}

/// `backtest`'s usage line, for the refusal that dumps it (a bare invocation with nothing to talk
/// about).
pub(super) fn usage_text() -> String {
    let version = version_line();
    let mut command = program(&version).command::<BacktestArgs>();
    command.build();
    command.render_usage().to_string()
}

/// `backtest` — run a profile, search its parameters, serve the backtesthub, and read a finished
/// search.
#[derive(Parser, Debug)]
#[command(
    name = "backtesthub",
    args_conflicts_with_subcommands = true,
    disable_help_subcommand = true,
    after_help = "The history comes from a datahub (decision 0084): `--store` is refused on the run \
                  path; for local files start a key-less one on them first: `vike-backend datahub \
                  --store DIR`."
)]
pub(super) struct BacktestArgs {
    #[command(subcommand)]
    pub verb: Option<Verb>,
    #[command(flatten)]
    pub run: RunArgs,
}

/// The one word that routes before the profile.
#[derive(Subcommand, Debug)]
pub(super) enum Verb {
    /// List the trials of a finished (or interrupted) parameter SEARCH. Reads the run ARTIFACT
    /// only: no store, no profile, no network.
    Trials(TrialsArgs),
}

/// `backtest trials <id>`.
#[derive(Args, Debug)]
pub(super) struct TrialsArgs {
    /// A FULL run id, as `backtest: search saved to …` printed it and as the run directory under
    /// <project>/user_data/runs/ is named. Prefixes, `@last` and marks are the design's shared
    /// SELECTOR GRAMMAR and are not implemented here.
    #[arg(value_name = "SEARCH_RUN_ID")]
    pub id: String,
    /// score (default) | n | return | sharpe | max_drawdown | trades | equity. `score` and `n` come
    /// from the ledger; the rest are read out of each trial's recorded metrics. A trial with no
    /// metric for the field sorts LAST.
    #[arg(long, value_name = "FIELD")]
    pub sort: Option<String>,
    /// Print only the first N rows after sorting. The document's counts still describe the WHOLE
    /// search.
    #[arg(long, value_name = "N")]
    pub top: Option<usize>,
    /// Print the run's own TrialLedgerDocument instead of the table: the same schema report.json holds.
    #[arg(long)]
    pub json: bool,
    /// Write the top-sorted trial's overrides as a [strategy.params] TOML fragment.
    #[arg(long, value_name = "FILE")]
    pub export_params: Option<String>,
}

/// The run verb: `backtest [PROFILE] [flags]`, the daemon (`--addr`) and the listings.
#[derive(Args, Debug, Default)]
pub(super) struct RunArgs {
    /// The harness profile TOML (data slice + engine costs + strategy), as the first POSITIONAL
    /// argument. REQUIRED unless --list/--list-optimizers/--addr. A profile with a [paramscan]
    /// table, or its permanent [sweep] alias, runs a parameter search instead of one backtest.
    //
    // ⚠ Collected as a list so that a second one is refused BY NAME (`profile_from_args`) rather
    // than by a usage error that names no file.
    #[arg(value_name = "PROFILE.toml")]
    pub profile: Vec<String>,
    /// The same thing, the older spelling. KEPT: `vike-cli backtest run --local` SPAWNS this binary
    /// with it. Giving BOTH is an error: they may name two different files, so it is refused
    /// rather than resolved.
    #[arg(long = "profile", value_name = "PATH")]
    pub profile_flag: Option<String>,
    /// STAY ALIVE and serve the seven COMPUTE verbs over the hub protocol (RunBacktest, RunSlice,
    /// RunParamscan, RunWalkforward, RunParamscanProfile, RunWalkforwardProfile,
    /// ListStrategies). The VALUE IS OPTIONAL: a bare --addr serves on the configured address
    /// (the config.backtest_addr row, then 127.0.0.1:7880); --addr HOST:PORT overrides it. The
    /// FLAG is what says 'become a daemon': no profile is ever implied. A non-loopback address is
    /// refused unless the flags.datahub_allow_public_bind row is on AND hub keys are configured:
    /// this server compiles Rhai the client sends.
    #[arg(long, num_args = 0..=1, value_name = "ADDR")]
    pub addr: Option<Option<String>>,
    /// With --addr: ARM the named-run lane, so an OBSERVE-scope peer may run one of this daemon's
    /// compiled-in strategies over a bounded window, and may list them. Off by default: it spends
    /// this box's CPU for a read-only client and publishes the strategy names.
    #[arg(long)]
    pub named_run: bool,
    /// REFUSED on the run path since 2026-09-25: history is read from a datahub. For local files,
    /// start a key-less one on them first: vike-backend datahub --store DIR.
    #[arg(long, num_args = 0..=1, value_name = "DIR", hide = true)]
    pub store: Option<Option<String>>,
    /// Print the report as pretty JSON instead of the human table.
    #[arg(long)]
    pub json: bool,
    /// Print every registered strategy name and exit: no store, no profile needed.
    #[arg(long)]
    pub list: bool,
    /// Print every parameter-search METHOD, one per line, and exit: no store, no profile, no
    /// network. With --json, one object carrying the count and which method runs when --optimizer
    /// is absent. The `--optimizer` twin of --list.
    #[arg(long)]
    pub list_optimizers: bool,
    /// The paramscan ranking metric (default sharpe); `multi` is the composite objective. Ignored,
    /// not an error, on a profile with no [paramscan] table.
    #[arg(long, value_name = "sharpe|return|max_dd|equity|multi")]
    pub rank_by: Option<String>,
    /// The parameter-search METHOD: grid (default, the exhaustive cartesian product), euler
    /// (bounded successive-halving refinement), tpe (Bayesian ask/tell) or genetic (a population
    /// search over the grid's own points, which needs --seed). Replaces the retired --search.
    #[arg(long, value_name = "grid|euler|tpe|genetic")]
    pub optimizer: Option<String>,
    /// Euler halving depth (default 3). EULER ONLY: refused, not discarded, under another method;
    /// past the cap it is refused rather than silently clamped.
    #[arg(long, value_name = "N")]
    pub euler_depth: Option<String>,
    /// TPE trial budget (default 64). TPE ONLY: refused under another method.
    #[arg(long, value_name = "N")]
    pub trials: Option<String>,
    /// The reproducibility seed. TPE AND GENETIC: refused under grid and euler. On tpe it defaults
    /// to 0; on genetic it is REQUIRED, because a genetic run reports one sample of a
    /// distribution and a seed nobody typed is a constant the answer silently depends on.
    #[arg(long, value_name = "S")]
    pub seed: Option<String>,
    /// What a SEARCH leaves behind: `scalars` (default) writes one ledger line per trial under
    /// <project>/user_data/runs/<id>/trials.jsonl; `none` keeps the parent run and no ledger;
    /// `returns` is `scalars` PLUS a bucketed return vector kept per trial in memory, which adds
    /// the anti-overfitting statistics (PBO via CSCV, effective trial count, deflated Sharpe) to
    /// that run's report.json as `overfit.*`. `series` is still refused by name, on retention
    /// grounds.
    #[arg(long, value_name = "none|scalars|returns")]
    pub keep_trials: Option<String>,
    /// Continue an interrupted SEARCH: re-open that parent run, reuse every trial it already holds,
    /// and evaluate only what is missing. Refused if the profile, the store, the optimizer, the
    /// seed or the budget differ from the run being resumed.
    #[arg(long, value_name = "ID")]
    pub resume: Option<String>,
    /// The statistical-significance FLOOR: a trial that closed fewer than N trades is UNRANKABLE
    /// rather than merely penalised, under every method and every --rank-by. 0 (the default)
    /// disarms it, so a sweep matrix can pass the flag unconditionally.
    #[arg(long, value_name = "N")]
    pub min_trades: Option<String>,
    /// Which progress stream a SEARCH emits, on stderr and never stdout: auto (the default: the
    /// throttled human line, and ONLY when stderr is a terminal), none (silent, nothing counted),
    /// json (one newline-delimited event per point, terminal or not, unthrottled).
    #[arg(long, value_name = "auto|none|json")]
    pub progress: Option<String>,
    /// RETIRED and refused by name: the flag is now --optimizer.
    #[arg(long, num_args = 0..=1, hide = true)]
    pub search: Option<Option<String>>,
}

/// What `--addr` asked for. Three states, because the flag takes an OPTIONAL value.
///
/// ⚠ **The FLAG is what says "become a daemon", never the absence of a profile** — the owner's
/// distinction when they refused `--serve`, `backtest serve`, `backtest listen` and
/// `backtest daemon` on 2026-09-10. `--addr` names a THING (the socket to bind), the way `--out`
/// names a file, and being given one is what makes this process stay alive. So a bare `backtest`
/// with no profile is still the old argument error, not an accidental daemon.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AddressFlag {
    /// No `--addr` at all — run one backtest and exit, exactly as before ruling 7.
    Absent,
    /// `--addr` with no value: serve on the CONFIGURED address. This is the normal use, and it is
    /// the whole point of the value being optional — the owner's *"MAY WE SET THIS SOMEWHERE AND
    /// NOT MENTION IT ALL THE TIME??"*.
    Configured,
    /// `--addr <host:port>` (or `--addr=<host:port>`): serve THERE, overriding every lower rung.
    Explicit(String),
}

impl RunArgs {
    /// What `--addr` asked for. The deployed unit is `backtesthub --addr` with NOTHING after it, and
    /// `clap` reads that as a flag with no value (`num_args(0..=1)`), the next token being taken
    /// as the value only when it is not itself a flag.
    ///
    /// ⚠ A BLANK value never gets here: `vike_utility::cli::Program` refuses it for every argument,
    /// so `--addr ""` in a unit file stops the parse instead of reading as "the configured
    /// address" and binding somewhere the operator did not name.
    pub(super) fn address_flag(&self) -> AddressFlag {
        match &self.addr {
            None => AddressFlag::Absent,
            Some(None) => AddressFlag::Configured,
            Some(Some(value)) => AddressFlag::Explicit(value.clone()),
        }
    }

    /// The value written for one of the SEARCH flags the ownership and refusal rules iterate by
    /// name (`--optimizer`, every `METHOD_KNOBS` flag, every `SEARCH_PROPERTY_FLAGS` flag), or
    /// `None` when it was not written.
    ///
    /// ⚠ The names are the TABLES' (`vike_model::run_request::method_knobs::METHOD_KNOBS`,
    /// `SEARCH_PROPERTY_FLAGS`), so a flag a table gains and this match lacks would read as never
    /// written — silently. `crates/vike-backtesthub/src/backtest_cli/tests/search_flags.rs`'s `every_flag_a_search_table_names_is_readable_and_declared_as_valued`
    /// iterates both tables through this function.
    pub(super) fn search_value(&self, flag: &str) -> Option<&str> {
        match flag {
            "--optimizer" => self.optimizer.as_deref(),
            "--euler-depth" => self.euler_depth.as_deref(),
            "--trials" => self.trials.as_deref(),
            "--seed" => self.seed.as_deref(),
            "--keep-trials" => self.keep_trials.as_deref(),
            "--resume" => self.resume.as_deref(),
            "--min-trades" => self.min_trades.as_deref(),
            "--progress" => self.progress.as_deref(),
            _ => None,
        }
    }
}

/// The profile, in either spelling: `backtest my.toml` (ruling 14) or `backtest --profile my.toml`.
///
/// ⚠ **`--profile` CANNOT retire, and that is a wire fact rather than a preference.**
/// `crates/vike-cli/src/cmd/backtest.rs`'s local arm builds `vec!["--profile".into(), …]` and
/// SPAWNS this binary, and `scripts/cli_mcp_smoke.sh` runs it. So the positional is ADDITIVE.
/// (It used to be TWO arms; `vike-cli sweep` was deleted by ruling 13 and its search flags moved
/// onto `vike-cli backtest`, which spawns the same way.)
///
/// ⚠ **BOTH is a REFUSAL, not a precedence rule.** Two spellings that may name two different files
/// is the same defect class as two searcher selectors: picking a winner silently answers a question
/// the operator did not know they had asked.
///
/// `Ok(None)` means "none given" — the caller's own arm, because that is the one case with no
/// specific mistake to name and therefore the one that still prints the usage text.
///
/// The positional scan this replaced skipped a valued flag's VALUE by table
/// (`PROFILE_PATH_VALUED`); `clap` knows which flags take a value, so a flag's value can no longer
/// be read as the profile.
pub(super) fn profile_from_args(run: &RunArgs) -> Result<Option<String>, String> {
    let flagged = run.profile_flag.clone();
    match (flagged, run.profile.as_slice()) {
        // ⚠ `data` IS a positional, so ruling 12's subcommand and ruling 14's profile collide on
        // exactly one word — and the router resolves it by routing only when `data` comes FIRST.
        // That leaves this case: `backtest --optimizer tpe data rm …` reaches here with `data` as
        // the profile, and without this arm it loads a file called `data`, fails on the open and
        // names a path the operator never typed. Refused by name instead, saying where the word
        // goes.
        //
        // The declared cost, per word: a profile literally NAMED `data` or `trials` (no extension)
        // is no longer reachable positionally. `--profile data` still reaches it, which is what the
        // arm says. TABLE-DRIVEN, so a third subcommand joins by adding a row rather than a
        // fourth match arm.
        (None, [p]) if SUBCOMMANDS.iter().any(|(w, _)| *w == p.as_str()) => {
            let (word, shape) =
                SUBCOMMANDS.iter().find(|(w, _)| *w == p.as_str()).expect("just matched");
            Err(format!(
                "`{word}` is a SUBCOMMAND here, not a profile, and it must come FIRST — write \
                 `backtest {word} {shape} …`. If you really meant a profile file called `{word}`, \
                 spell it `--profile {word}`"
            ))
        }
        (Some(p), []) => Ok(Some(p)),
        (None, [p]) => Ok(Some(p.clone())),
        (None, []) => Ok(None),
        (Some(p), [b]) => Err(format!(
            "the profile was given twice — --profile {p:?} and the positional {b:?}. They may name \
             two different files, so this is refused rather than resolved: give it once"
        )),
        (flagged, extra) => Err(format!(
            "more than one profile was given: {}{extra:?}. Give exactly one",
            flagged.map(|p| format!("--profile {p:?} and ")).unwrap_or_default()
        )),
    }
}
