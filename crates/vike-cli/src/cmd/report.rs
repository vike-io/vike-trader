//! `vike-cli report` — **the results of one source**: a finished run on this machine
//! (`report run:<selector>`), or a live account's own fills read from the datahub
//! (`report account:<venue>/<LABEL>`).
//!
//! # ⚠ The journal doors are RETIRED (owner, 2026-11-18)
//!
//! This verb had a second source, `--node <host:port>`, which asked a running `vike-tradehub` to
//! fold the command journal IT writes, and `vike-backend report --journal DIR` did the same fold on
//! the box that held the journal. A journal holds every account of its node and a `FillEvent` names
//! none, so neither door could say WHOSE report it was; the datahub's execution rows carry the
//! account, so a live report is read from there, spelled `report account:<venue>/<LABEL>`. Both
//! journal doors answer [`JOURNAL_DOORS_RETIRED`] on the usage rung with nothing dialed: this
//! verb's `--node` in [`Cli::into_args`], before any key is read, and the multicall's `report` row
//! (`crates/vike-backend/src/main.rs`'s `report_main`) for the other. The flag stays in the parser
//! (hidden from `--help`) so an old script meets the new spelling instead of "unexpected argument".
//!
//! ⚠ **A bare run selector is retired the same day.** One verb, one spelling per source: the stored
//! source is `run:<selector>` ([`run_selector`]), the live one `account:<venue>/<LABEL>`, and a word
//! typed without either prefix is refused naming the spelling that works, never resolved —
//! `report account:…` and `report <id>` must not be told apart by guessing which one a word looks like.
//!
//! # The two sources
//!
//! - **`run:<selector>`** — a run directory under `<project>/user_data/runs/`. A backtest is
//!   re-rendered from what it left on disk ([`crate::cmd::report::stored`]): no engine, no store, no
//!   node, no key. A FORWARD run (`vike_runs::FORWARD_RUN_KIND`) is a strategy mounted live, so its
//!   report is its ACCOUNT's fills over the run's own window, kept to its mount
//!   ([`vike_client::account_report`]).
//! - **`account:<venue>/<LABEL>`** — the account's fills out of the datahub over `--from`/`--to`, its
//!   tier and its ledgers from the tradehub ([`vike_client::account_report`]'s module doc says what
//!   counts; the MCP tool `report` folds through it too). `<venue>` alone, or `<venue>/DEFAULT`, is
//!   the unlabelled account (`crate::cmd::trade::selector`'s grammar).
//!
//! **The grammar says which source, and exactly one per invocation:** naming two is a refusal
//! rather than a precedence rule ([`refuse_a_second_source`]). A precedence would mean an operator
//! who typed both got an answer about one of them with nothing saying which. Two sources are held
//! side by side only when the line says so: `report --compare <A> <B>`
//! ([`crate::cmd::report::comparison`], its alignment in that module's doc).
//!
//! # Keys only where a socket is opened
//!
//! A stored run is a file, so a box with no node key, no node and no network reports on its own
//! backtests: the dispatcher hands this verb its keyrings as CLOSURES ([`Context`]), and only a live
//! source calls them.
//!
//! # Output, and the ONE document that is versioned
//!
//! The STORED source's document is this crate's own, so it carries a version and a published shape:
//! `crate::cmd::report::schema` owns both, and `report --schema` prints the JSON Schema with no run,
//! no node and no key. A live source's `--json` is `vike_analytics::TradingResultsTable`'s document.
//!
//! # What is / is not tested
//!
//! The grammar ([`parse`]) and every refusal it makes are PURE and unit-tested below; the stored
//! renderer's own refusals are unit-tested in [`crate::cmd::report::stored`], over real temporary
//! run directories, and the account read over the in-memory store in
//! [`vike_client::account_report`]. `crates/vike-cli/tests/shipped_cli/study_report_refusal_cli.rs`
//! drives the shipped binary through both retired spellings and proves `--node` dials nothing.

// `report --compare <A> <B>` and the published JSON schema of its document.
pub(crate) mod comparison;
pub(crate) mod comparison_schema;
// The stored-run renderer and the published JSON schema of what it emits.
pub(crate) mod schema;
pub(crate) mod stored;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_client::account_report::{self, LiveFold, LiveQuery};
use vike_client::hubkeys::HubKeyring;
use vike_data::TimestampRange;
use vike_hub_protocol::auth::HubKeys;

use crate::cmd::args::{self, Stop};
use crate::cmd::report::schema::schema_text;
use crate::cmd::report::stored::Period;
use crate::cmd::trade::selector::{self, Book};
use crate::exit::{CliError, CommandResult};

/// The ONE line both retired journal doors answer with: this verb's `--node`, and `vike-backend
/// report` (`crates/vike-backend/src/main.rs`'s `report_main`), each behind its own program prefix.
pub const JOURNAL_DOORS_RETIRED: &str = "the live-journal report (`vike-cli report --node`, \
     `vike-backend report --journal`) is retired: a live account's report is `vike-cli report \
     account:<venue>/<LABEL>`, read from the datahub";

/// The program name this verb is read under.
const BIN: &str = "vike-cli report";

// The prose half of `report --help`; the usage lines and the option table are `clap`'s, derived
// from [`Cli`].
const LONG_ABOUT: &str = "\
Render the trading results of ONE source:

  run:<run>        a run id, a unique prefix, @last, @last:<kind>, or a @mark, after `run:`.
                   A backtest is re-rendered from its own run directory: no engine, no store, no
                   socket, no node, and no credential. A FORWARD run is its account's fills over
                   the run's own window, kept to its strategy, folded as an account is (it
                   takes --cash and the other account knobs).
  account:<venue>/<LABEL>
                   a live account's own fills, read from the datahub (this box's
                   config.datahub_addr) over --from/--to, its tier and its ledgers from the
                   tradehub (config.node_addr, the observe key). <venue> alone or <venue>/DEFAULT is
                   the unlabelled account. A FOREIGN fill (not placed by this install) counts and is
                   named; a fill recorded before the execution stream carries no account and is left
                   out, and both are said. The curve is the account's REALIZED one from --cash;
                   --mark-to-market marks it against the store's bars.

`report --compare <A> <B>` holds two sources side by side, A the reference: every metric of both
and B − A. An account: side reads --from/--to, else the period of the run on the other side (the
forward test held against its backtest); a live side's curve starts from --cash, else from the
stored backtest's starting equity. Every live rendering says its starting capital and where it
came from. `report --schema --compare` prints that document's shape.

A bare selector (`report @last`) and the live-journal `--node` are RETIRED and refused, each naming
the spelling that replaced it.

⚠ A STORED statistic is refused rather than approximated. A run's equity curve is DECIMATED past
`vike_runs::MAX_EQUITY_SAMPLES`, and a number recomputed from a thinned curve can disagree
with the run's own report.json. So --breakdown REFUSES a thinned curve (naming the stride), and the
recomputed peak and drawdown are `null` with the reason beside them. `backtest show <run> --export
equity` hands you the stored curve itself, stride and all.";

/// `--cash`'s help: the default is `vike_analytics::results::DEFAULT_REPORT_SEED`, the one seed every report door
/// folds an account from.
fn cash_help() -> String {
    format!(
        "the account's starting capital, the realized curve's seed, which scales the return \
         ratios (default {}; an account or a forward run)",
        vike_analytics::results::DEFAULT_REPORT_SEED
    )
}

/// The `report` command line, as `clap` reads it. The SOURCES are one flat list of positionals;
/// which flags each source can use is a rule among the verb's own flags and stays in
/// [`Cli::into_args`], after the parse, in its own words.
#[derive(clap::Parser, Debug)]
#[command(about = "Render the trading results of a run or of a live account", long_about = LONG_ABOUT)]
struct Cli {
    #[arg(
        value_name = "SOURCE",
        help = "run:<selector> (a run in this project's run directory: a run id, a unique prefix, \
                @last, @last:<kind>, or a @mark) | account:<venue>/<LABEL> (a live account)"
    )]
    sources: Vec<String>,
    #[arg(
        long,
        help = "hold TWO sources side by side, A the reference: `report --compare <A> <B>`; with \
                --schema, the comparison document's shape"
    )]
    compare: bool,
    // RETIRED 2026-11-18: read only to answer [`JOURNAL_DOORS_RETIRED`], so hidden from `--help`
    // (help must not advertise a door that refuses).
    #[arg(long, value_name = "HOST:PORT", hide = true)]
    node: Option<String>,
    #[arg(
        long,
        value_name = "DATE",
        help = "an account's first instant read: epoch-ms, or YYYY-MM-DD (that UTC day's start)"
    )]
    from: Option<String>,
    #[arg(
        long,
        value_name = "DATE",
        help = "an account's last instant read, inclusive: epoch-ms, or YYYY-MM-DD (to that UTC \
                day's end)"
    )]
    to: Option<String>,
    #[arg(
        long,
        value_name = "PERIOD",
        help = "per-period returns off the stored curve: day | month (a stored run only)"
    )]
    breakdown: Option<String>,
    #[arg(long, value_name = "CASH", help = cash_help())]
    cash: Option<String>,
    #[arg(
        long,
        value_name = "N",
        help = "the Sharpe/Sortino/Calmar/CAGR annualization factor (default 365 when every venue \
                trades every day, else 252; an account or a forward run)"
    )]
    periods_per_year: Option<String>,
    #[arg(
        long,
        help = "mark the account's curve to market against the store's bars, and back-fill each \
                trade's MAE/MFE from them (an account or a forward run)"
    )]
    mark_to_market: bool,
    #[arg(
        long,
        value_name = "INTERVAL",
        help = "the bar interval --mark-to-market reads (default 1m)"
    )]
    interval: Option<String>,
    #[arg(
        long,
        help = "leave the account's FOREIGN fills out (not placed by this install); they count by \
                default, as on the account's own statement"
    )]
    without_foreign: bool,
    #[arg(
        long,
        value_name = "TEXT",
        help = "the account report's heading, in place of its account, tier and period"
    )]
    name: Option<String>,
    #[arg(long, help = "the machine-readable document instead of the human rendering")]
    json: bool,
    #[arg(
        long,
        value_name = "PATH",
        help = "also write the results as one self-contained HTML page to PATH (the human \
                rendering still prints)"
    )]
    html: Option<PathBuf>,
    #[arg(
        long,
        help = "print the JSON Schema for the STORED document (with --compare, the comparison's) and \
                exit — no run, no node, no key"
    )]
    schema: bool,
}

/// One source a positional names. Decided by [`source_of`]; exactly one per invocation.
#[derive(Debug, Clone, PartialEq)]
enum Source {
    /// `run:<selector>`: a run's own directory, on this machine. Holds the selector with its `run:`
    /// taken off.
    Run(String),
    /// `account:<venue>/<LABEL>`: a live account.
    Account(Book),
}

/// What this invocation asked for. Decided by [`claim_request`] — see this module's doc for why
/// there is no precedence between two of them.
#[derive(Debug, PartialEq)]
enum Request {
    /// `--schema`: the published shape of the stored document (with `--compare`, of the comparison
    /// document), and nothing else at all.
    Schema { compare: bool },
    /// One source's results.
    One(Source),
    /// `--compare <A> <B>`: two sources' results side by side, A the reference.
    Compare(Source, Source),
}

/// The parsed `report` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq)]
struct Args {
    /// What this invocation asked for.
    request: Request,
    /// `--from`, epoch-ms: an account's first instant read.
    from: Option<i64>,
    /// `--to`, epoch-ms, inclusive: an account's last instant read.
    to: Option<i64>,
    /// `--cash`: the account's starting capital. Parsed so a bad number is refused by name, and
    /// refused on a stored run ([`CASH_IS_LIVE_ONLY`]).
    cash: Option<f64>,
    /// `--periods-per-year`: the annualization factor; the same rule as `cash`.
    periods_per_year: Option<f64>,
    /// `--breakdown`: bucket the stored curve's returns by period. STORED source only, and refused
    /// outright on a thinned curve — `crate::cmd::report::stored`'s `breakdown_of` carries why.
    breakdown: Option<Period>,
    /// `--mark-to-market`: mark an account's curve against the store's bars.
    mark_to_market: bool,
    /// `--interval`: the bars `--mark-to-market` reads.
    interval: Option<String>,
    /// `--without-foreign`: leave an account's foreign fills out.
    without_foreign: bool,
    /// `--name`: an account report's heading.
    name: Option<String>,
    /// `--json`: the machine-readable document instead of the human rendering.
    json: bool,
    /// `--html PATH`: the HTML page, written beside the human rendering.
    html: Option<PathBuf>,
}

/// This verb as a `clap` command, for the published reference (`crate::surface`).
pub(crate) fn command() -> clap::Command {
    args::command::<Cli>(BIN)
}

/// Parse everything after the `report` subcommand. Both `--flag value` and `--flag=value` are
/// `clap`'s; the source and number rules below are this verb's own and keep their own wording.
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, Stop> {
    let cli: Cli = args::parse(BIN, argv)?;
    cli.into_args().map_err(|msg| Stop::message(BIN, msg))
}

impl Cli {
    fn into_args(self) -> Result<Args, String> {
        // ⚠ FIRST, before any other rule: whatever else is on the line, the operator reached for
        // the retired door, and the one thing worth telling them is where it went. Nothing is
        // dialed and no key is read.
        if self.node.is_some() {
            return Err(JOURNAL_DOORS_RETIRED.to_string());
        }
        let cash = self.cash.as_deref().map(|raw| number("--cash", raw)).transpose()?;
        let periods_per_year = self
            .periods_per_year
            .as_deref()
            .map(|raw| number("--periods-per-year", raw))
            .transpose()?;
        let breakdown = self.breakdown.as_deref().map(Period::parse).transpose()?;
        let from = self.from.as_deref().map(|raw| instant("--from", raw, false)).transpose()?;
        let to = self.to.as_deref().map(|raw| instant("--to", raw, true)).transpose()?;
        if let (Some(f), Some(t)) = (from, to)
            && f > t
        {
            return Err(format!(
                "--from ({f}) is after --to ({t}): the window would hold nothing, which reads \
                 exactly like an account that never traded"
            ));
        }
        let sources = self
            .sources
            .iter()
            .map(String::as_str)
            .map(source_of)
            .collect::<Result<Vec<_>, _>>()?;
        let request = claim_request(sources, self.compare, self.schema)?;
        let args = Args {
            request,
            from,
            to,
            cash,
            periods_per_year,
            breakdown,
            mark_to_market: self.mark_to_market,
            interval: self.interval,
            without_foreign: self.without_foreign,
            name: self.name,
            json: self.json,
            html: self.html,
        };
        refuse_a_flag_the_request_cannot_use(&args)?;
        Ok(args)
    }
}

/// One positional: `run:<selector>` or `account:<venue>/<LABEL>`, each refused in its own words.
fn source_of(positional: &str) -> Result<Source, String> {
    match positional.strip_prefix("account:") {
        Some("") => Err("`account:` names no account: put <venue>/<LABEL> after it \
                         (<venue>/DEFAULT, or <venue> alone, for the unlabelled one)"
            .to_string()),
        Some(book) => selector::parse(book).map(Source::Account).map_err(|e| e.message),
        None => run_selector(positional).map(Source::Run),
    }
}

/// `run:<selector>` is the stored source's ONE spelling: the selector with its `run:` taken off.
///
/// ⚠ A bare selector is REFUSED (owner, 2026-11-18), naming the spelling that works with the
/// operator's own selector in it, never resolved: a positional that reads as a run id today could
/// read as another source tomorrow. And `run:` with nothing after it is refused too, because the
/// empty string is a prefix of EVERY run id, so a project holding one run would silently answer
/// about it.
fn run_selector(positional: &str) -> Result<String, String> {
    match positional.strip_prefix("run:") {
        Some("") => Err("`run:` names no run: put a run id, a unique prefix, @last, \
                         @last:<kind> or a @mark after it"
            .to_string()),
        Some(selector) => Ok(selector.to_string()),
        None => Err(format!(
            "a bare run selector is retired: spell it `vike-cli report run:{positional}`"
        )),
    }
}

/// How each source spells itself in a refusal.
fn spelling(source: &Source) -> &'static str {
    match source {
        Source::Run(_) => "run:<run>",
        Source::Account(_) => "account:<venue>/<LABEL>",
    }
}

/// Resolve the sources and `--schema` to exactly one request, refusing NONE and MORE THAN ONE by
/// name.
///
/// ⚠ **Neither refusal may become a precedence rule.** An operator who typed a source AND
/// `--schema` believes both matter; answering about one of them silently is the "silent precedence"
/// the run-selector grammar refuses everywhere else
/// (`crate::cmd::backtest::read::selector`'s `resolve_one` names the candidates rather than picking the most
/// recent). And a bare `vike-cli report` cannot default to `@last`, which would report on whatever
/// happened to run most recently.
fn claim_request(
    sources: Vec<Source>,
    compare: bool,
    wants_schema: bool,
) -> Result<Request, String> {
    let count = sources.len();
    let mut sources = sources.into_iter();
    match (sources.next(), sources.next(), compare, wants_schema) {
        (Some(first), _, _, true) => Err(refuse_a_second_source(&[spelling(&first), "--schema"])),
        (None, _, compare, true) => Ok(Request::Schema { compare }),
        (Some(first), Some(second), true, false) if count == 2 => {
            Ok(Request::Compare(first, second))
        }
        (_, _, true, false) => Err(format!(
            "`--compare` holds TWO sources side by side and {count} were named: `vike-cli report \
             --compare <A> <B>`, each a `run:<run>` or an `account:<venue>/<LABEL>`, A the reference"
        )),
        (Some(_), Some(_), false, false) => Err(format!(
            "`report` renders ONE source and was given {count}. There is no implied comparison \
             here: `vike-cli report --compare <A> <B>` holds two side by side."
        )),
        (Some(first), None, false, false) => Ok(Request::One(first)),
        (None, _, false, false) => Err(NO_SOURCE.to_string()),
    }
}

/// The sentence a source-less invocation is refused with. A `const` so [`claim_request`] reads as
/// short decisions rather than as one wrapped string literal, and so the test that pins the
/// wording names the same text the parser returns.
const NO_SOURCE: &str = "`report` needs a SOURCE: `run:<run>` for a run on this machine \
     (`vike-cli report run:@last`) or `account:<venue>/<LABEL>` for a live account, and \
     `--compare <A> <B>` holds two side by side. `--schema` prints the stored document's shape \
     and needs no source.";

/// The sentence two sources are refused with. Split out so the wording is pinned by a unit test and
/// so the roster in the message is the one [`claim_request`] actually counted.
fn refuse_a_second_source(named: &[&str]) -> String {
    format!(
        "`report` renders ONE source per invocation and {} were named. Pick one: a source is a run \
         or an account, and `--schema` prints the stored document's shape. They answer different \
         questions, so there is deliberately no precedence between them.",
        named.join(" and ")
    )
}

/// A flag the request cannot use is refused by name with the reason, never dropped —
/// `crate::cmd::backtest`'s `refuse_foreign_read_flags` carries the argument: an operator who typed
/// it is not looking for "unknown option", they want to know which source takes it.
///
/// ⚠ `--cash` and `--periods-per-year` are refused on a stored run rather than ignored, and that is
/// the load-bearing one. They RESCALE every return ratio in an answer, the stored source
/// recomputes nothing from them, and a silently-dropped `--cash` would hand somebody ratios they
/// believe are scaled to their account — the same defect [`number`] refuses a NaN for, one layer up.
/// A `run:` may be a FORWARD run, whose report is folded and takes them, so on a run they are refused
/// once the run is read ([`refuse_a_live_knob`]); `--from`/`--to` are refused here, as no run takes them.
fn refuse_a_flag_the_request_cannot_use(a: &Args) -> Result<(), String> {
    if a.json && a.html.is_some() {
        return Err("--json and --html are two renderings of one answer: name one".to_string());
    }
    if a.interval.is_some() && !a.mark_to_market {
        return Err(
            "--interval names the bars --mark-to-market reads, and --mark-to-market was not given"
                .to_string(),
        );
    }
    // ⚠ EXHAUSTIVE over [`Request`], with no `_` arm, so a new request cannot be added without
    // deciding what each of these flags means for it. A catch-all here would silently ACCEPT every
    // one of them on the new request, which is the inert-flag defect this function exists to refuse.
    let wrong: Option<(&str, &str)> = match &a.request {
        // ⚠ `--schema` answers about the DOCUMENT, not about a source, so every other flag on the
        // line was written in the belief that it would change the output. None of them can.
        Request::Schema { .. } => first_other_flag(a).map(|flag| (flag, NOTHING_SELECTS_A_SCHEMA)),
        Request::One(Source::Run(_)) => first_window_flag(a),
        Request::One(Source::Account(_)) => {
            a.breakdown.is_some().then_some(("--breakdown", BREAKDOWN_IS_STORED_ONLY))
        }
        // A comparison holds two sides, so ONE run's breakdown and ONE account's heading have no
        // side to go to; an account's knobs apply to its account side, and need one.
        Request::Compare(x, y) => {
            if a.breakdown.is_some() {
                Some(("--breakdown", BREAKDOWN_IS_ONE_RUNS))
            } else if a.name.is_some() {
                Some(("--name", NAME_IS_ONE_ACCOUNTS))
            } else if matches!(x, Source::Account(_)) || matches!(y, Source::Account(_)) {
                None
            } else {
                first_window_flag(a)
            }
        }
    };
    match wrong {
        Some((flag, why)) => Err(format!("{flag} {why}")),
        None => Ok(()),
    }
}

/// `--from`/`--to` on a line with no account, with why a run refuses them.
fn first_window_flag(a: &Args) -> Option<(&'static str, &'static str)> {
    [("--from", a.from.is_some()), ("--to", a.to.is_some())]
        .into_iter()
        .find_map(|(flag, given)| given.then_some((flag, WINDOW_IS_THE_ACCOUNTS)))
}

/// A live knob on a line whose runs turned out to be stored backtests, refused by name with why and with
/// `which` (what the line names): a FORWARD run takes them, so they are refused once the run is read.
fn refuse_a_live_knob(a: &Args, which: &str) -> CommandResult<()> {
    match first_live_flag(a) {
        Some((flag, why)) => Err(CliError::usage(format!("{flag} {why} {which}"))),
        None => Ok(()),
    }
}

/// The first knob on the line that only a LIVE report (an account, a forward run) can use, with why a
/// stored run refuses it.
fn first_live_flag(a: &Args) -> Option<(&'static str, &'static str)> {
    if a.cash.is_some() {
        return Some(("--cash", CASH_IS_LIVE_ONLY));
    }
    if a.periods_per_year.is_some() {
        return Some(("--periods-per-year", PERIODS_PER_YEAR_IS_LIVE_ONLY));
    }
    for (flag, given) in [
        ("--mark-to-market", a.mark_to_market),
        ("--without-foreign", a.without_foreign),
        ("--name", a.name.is_some()),
    ] {
        if given {
            return Some((flag, ACCOUNT_ONLY));
        }
    }
    None
}

/// The first flag on the line that is not a SOURCE, or `None`. Only `--schema` asks — it is the one
/// request for which every remaining flag is meaningless, so they are answered by one sentence
/// rather than by many near-identical ones.
fn first_other_flag(a: &Args) -> Option<&'static str> {
    [
        ("--cash", a.cash.is_some()),
        ("--periods-per-year", a.periods_per_year.is_some()),
        ("--breakdown", a.breakdown.is_some()),
        ("--json", a.json),
        ("--from", a.from.is_some()),
        ("--to", a.to.is_some()),
        ("--mark-to-market", a.mark_to_market),
        ("--without-foreign", a.without_foreign),
        ("--name", a.name.is_some()),
        ("--html", a.html.is_some()),
    ]
    .into_iter()
    .find_map(|(flag, given)| given.then_some(flag))
}

/// Why `--cash` cannot be honoured on a stored run. The load-bearing refusal: a silently-dropped
/// figure hands somebody ratios they believe are scaled to their account.
const CASH_IS_LIVE_ONLY: &str = "is a LIVE report's starting capital and scales every return \
     ratio it computes. A stored run recomputes no ratios at all — report.json's own are carried \
     through verbatim — so honouring it is impossible and dropping it silently would hand you \
     ratios you believe were rescaled. A live account's report is `report \
     account:<venue>/<LABEL>` (a forward run takes it too); or re-run the backtest with the cash you \
     meant.";

/// Why `--periods-per-year` cannot be honoured on a stored run: the annualization already happened,
/// and redoing it needs the per-bar returns a decimated curve cannot give back.
const PERIODS_PER_YEAR_IS_LIVE_ONLY: &str = "is a LIVE report's annualization factor, and a stored \
     run's Sharpe was annualized when it RAN. Re-annualizing it here would need the per-bar returns, \
     which the stored curve cannot give back once it is decimated. Set the factor in the profile \
     and re-run.";

/// Why `--from`/`--to` are refused on a run.
const WINDOW_IS_THE_ACCOUNTS: &str = "bounds a live ACCOUNT's read (`account:<venue>/<LABEL>`). A \
     stored run's period is its own, and its curve may be thinned, so it is never cut; a forward \
     run's window is the run's own.";

/// Why an account's knob is refused on a stored run.
const ACCOUNT_ONLY: &str =
    "applies to a live report (`account:<venue>/<LABEL>`, or a forward run).";

/// Why `--breakdown` is refused on an account.
const BREAKDOWN_IS_STORED_ONLY: &str = "buckets a stored run's own curve (series.json), and an \
     account's report has no stored curve: its curve is folded from its fills here.";

/// Why `--breakdown` is refused under `--compare`.
const BREAKDOWN_IS_ONE_RUNS: &str = "buckets ONE stored run's curve, and a comparison holds two \
     sides: `report run:<run> --breakdown day|month` buckets each.";

/// Why `--name` is refused under `--compare`.
const NAME_IS_ONE_ACCOUNTS: &str = "heads ONE account's report, and a comparison names each side \
     by its source and period.";

/// Why nothing at all applies to `--schema`.
const NOTHING_SELECTS_A_SCHEMA: &str = "does not apply to --schema, which prints the stored \
     document's published SHAPE rather than any run's data. The schema is itself JSON and has one \
     form, so there is nothing here for a flag to select.";

/// One numeric flag value, refused by NAME when it does not parse.
///
/// ⚠ A non-finite value is refused too: a NaN or an infinity written as JSON is `null`, and a
/// renderer handed `null` applies its own default while the operator believes they set one.
fn number(flag: &str, raw: &str) -> Result<f64, String> {
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        Ok(_) => Err(format!("{flag} must be a finite number, got {raw:?}")),
        Err(e) => Err(format!("{flag} must be a number, got {raw:?} ({e})")),
    }
}

/// One `--from`/`--to` bound as epoch-ms, through the workspace's one date parse
/// (`vike_model::parse_date_label`: epoch-ms, or `YYYY-MM-DD` at that UTC day's start). An inclusive
/// END named as a date reaches to that day's last millisecond: `--to 2026-11-18` holds the 18th.
fn instant(flag: &str, raw: &str, end_of_day: bool) -> Result<i64, String> {
    let ms = vike_model::parse_date_label(raw)
        .map_err(|e| format!("{flag} {raw:?} is not an instant this verb reads ({e})"))?;
    let a_date = raw.trim().parse::<i64>().is_err();
    Ok(if end_of_day && a_date { ms + vike_model::MS_PER_DAY - 1 } else { ms })
}

/// What a `report` run reads besides its argv: the run directory, and — for a LIVE source only —
/// where the datahub and the tradehub are and how to sign to them.
///
/// The keyrings are CLOSURES so a stored run opens no credential store at all (this module's doc).
pub struct Context<'a> {
    /// `<project>/user_data/runs`, or `None` with no project above the working directory.
    pub runs_root: Option<PathBuf>,
    /// The marks store, the runs root's SIBLING (never its child).
    pub marks_root: Option<PathBuf>,
    /// This box's `config.datahub_addr`, or `None` for the compiled default.
    pub datahub_addr: Option<&'a str>,
    /// This box's datahub keys (its device key), read only when a live source dials.
    pub datahub_keys: &'a dyn Fn() -> Option<HubKeys>,
    /// This box's `config.node_addr`: the tradehub a live source asks for its tier and ledgers.
    pub node_addr: Option<&'a str>,
    /// This box's tradehub keyring (its device key), read only when a live source dials.
    pub hub_keys: &'a dyn Fn() -> HubKeyring,
    /// Now, epoch-ms: the end of an open window.
    pub now_ms: i64,
}

/// Entry point the dispatcher routes to. `argv` is everything AFTER the `report` subcommand.
pub fn run(argv: impl Iterator<Item = String>, ctx: &Context<'_>) -> ExitCode {
    let parsed = match parse(argv) {
        Ok(a) => a,
        Err(stop) => return args::finish(&stop),
    };
    let answer = match &parsed.request {
        // ⚠ No run, no node, no credential and no project: the published shape of the document is a
        // fact about this BUILD, so it must answer on a box where nothing else here does.
        Request::Schema { compare: false } => Ok(format!("{}\n", schema_text())),
        Request::Schema { compare: true } => Ok(format!("{}\n", comparison_schema::schema_text())),
        Request::One(source) => one(ctx, &parsed, source),
        Request::Compare(first, second) => comparison::run_compare(ctx, &parsed, first, second),
    };
    match answer {
        // ⚠ `print!`, not `println!`: every rendering ends with exactly one newline, the contract
        // `crate::cmd::backtest::read::show`'s `run_show` relies on too.
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        // ⚠ The rung comes from where the failure was classified — a run with no curve is
        // `Exit::Empty`, a thinned curve `Exit::Failed`, an unreachable node `Exit::Connect`.
        Err(e) => {
            eprintln!("vike-cli report: {}", e.message);
            e.exit.into()
        }
    }
}

/// One source's answer, the text to print.
fn one(ctx: &Context<'_>, a: &Args, source: &Source) -> CommandResult<String> {
    match source {
        Source::Run(selector) => {
            let run =
                stored::resolve_run(ctx.runs_root.as_deref(), ctx.marks_root.as_deref(), selector)?;
            if run.manifest.kind == vike_runs::FORWARD_RUN_KIND {
                if a.breakdown.is_some() {
                    return Err(CliError::usage(format!(
                        "--breakdown {BREAKDOWN_IS_STORED_ONLY} {} is a forward run, whose report is \
                         its account's fills.",
                        run.run_id
                    )));
                }
                return live(ctx, a, &forward_query(&run, ctx.now_ms)?);
            }
            refuse_a_live_knob(a, &format!("{} is a stored backtest.", run.run_id))?;
            // ⚠ Computed BEFORE anything is printed or written, so a refusal leaves no half answer.
            let text = stored::render_stored(&run, a.breakdown, a.json)?;
            if let Some(path) = &a.html {
                write_html(path, &crate::cmd::backtest::read::show::html_document(&run)?)?;
            }
            Ok(text)
        }
        Source::Account(book) => {
            let query = LiveQuery {
                account: account_report::account_ref(&book.venue, book.label.clone())?,
                window: TimestampRange { start: a.from, end: a.to },
                mount_id: None,
                title: None,
            };
            live(ctx, a, &query)
        }
    }
}

/// The live query of a FORWARD run: its account over `[started_ms, ended_ms]` (now while it still
/// runs), kept to its mount, headed by its strategy file.
fn forward_query(run: &vike_runs::ScannedRun, now_ms: i64) -> CommandResult<LiveQuery> {
    let (forward, ended_ms) = vike_runs::read_forward_run(&run.dir).map_err(|e| {
        CliError::failed(format!("{} is a forward run and cannot be read: {e}", run.run_id))
    })?;
    let book = selector::parse(&forward.account)?;
    Ok(LiveQuery {
        account: account_report::account_ref(&book.venue, book.label)?,
        window: TimestampRange {
            start: Some(forward.started_ms),
            end: Some(ended_ms.unwrap_or(now_ms)),
        },
        mount_id: Some(forward.mount_id),
        title: Some(format!("forward {}", forward.strategy_file)),
    })
}

/// The fold knobs a live side takes off the line.
fn live_fold(a: &Args) -> LiveFold {
    LiveFold {
        cash: a.cash.unwrap_or(vike_analytics::results::DEFAULT_REPORT_SEED),
        cash_from: cash_from(a).to_string(),
        periods_per_year: a.periods_per_year,
        mark_to_market: a.mark_to_market.then(|| {
            a.interval.clone().unwrap_or_else(|| account_report::DEFAULT_INTERVAL.to_string())
        }),
        without_foreign: a.without_foreign,
    }
}

/// Where the line's starting capital comes from: `--cash`, else the default.
fn cash_from(a: &Args) -> &'static str {
    if a.cash.is_some() { "--cash" } else { "the default; --cash sets it" }
}

/// One live side folded ([`live_side`]) and rendered: the table or its document, and the HTML page.
fn live(ctx: &Context<'_>, a: &Args, query: &LiveQuery) -> CommandResult<String> {
    let mut side = live_side(ctx, query, &live_fold(a))?;
    // `--name` replaces the heading, never the notes: the JSON document carries them in its `name`.
    if let Some(name) = &a.name {
        let named = std::iter::once(name.clone()).chain(side.notes.iter().cloned());
        side.sheet.name = Some(named.collect::<Vec<_>>().join("; "));
    }
    let text = if a.json {
        let doc = serde_json::to_string_pretty(&side.sheet)
            .map_err(|e| CliError::failed(format!("cannot render the results as JSON ({e})")))?;
        format!("{doc}\n")
    } else {
        // Each note on a line of its own under the table (the fees in other assets among them).
        let mut text = side.sheet.to_string();
        for note in &side.notes {
            text.push_str(&format!("{note}\n"));
        }
        text
    };
    if let Some(path) = &a.html {
        // Under the heading, whatever `--name` made of it: the source and period, as every view says
        // them, then each note on its own line.
        let label = side.results.source_label();
        let captions: Vec<&str> =
            label.iter().map(String::as_str).chain(side.notes.iter().map(String::as_str)).collect();
        let html = match &side.runtime {
            Some(runtime) => vike_analytics::render_html_with_stats(
                &side.sheet,
                &captions,
                &side.equity,
                &side.equity_timestamp,
                runtime,
            ),
            None => vike_analytics::render_html(
                &side.sheet,
                &captions,
                &side.equity,
                &side.equity_timestamp,
            ),
        };
        write_html(path, &html)?;
    }
    Ok(text)
}

/// **One live side, read and folded** ([`account_report::read_live_side`]: the tradehub's word on the account, the
/// datahub's fills, the fold) at this box's two addresses. Each note the side carries is said on stderr too, so a
/// `--json` reader sees them beside the document's `name`.
fn live_side(
    ctx: &Context<'_>,
    query: &LiveQuery,
    fold: &LiveFold,
) -> CommandResult<account_report::LiveSide> {
    // The one tradehub ladder with no flag rung (`report`'s `--node` is retired), so the words are this verb's own.
    let node = vike_client::resolved::node_dial_address(None, ctx.node_addr).map_err(|_| {
        CliError::failed(
            "a live report asks the tradehub for the account's tier and its ledgers, at this box's \
             `config.node_addr` row, and the row is not set: `vike-cli backend connect` writes it \
             (or `vike-cli config set config.node_addr HOST:PORT`)",
        )
    })?;
    let address = vike_client::resolved::datahub_dial_address(None, ctx.datahub_addr);
    // A one-shot command: the store keeps the client's own reply bound.
    let store = match (ctx.datahub_keys)() {
        Some(keys) => vike_datahub_client::HistoryStoreDatahub::with_keys(&address, keys),
        None => vike_datahub_client::HistoryStoreDatahub::new(&address),
    };
    let side = account_report::read_live_side(
        &node,
        &(ctx.hub_keys)(),
        &store,
        &address,
        query,
        fold,
        ctx.now_ms,
    )?;
    for note in &side.notes {
        eprintln!("vike-cli report: {note}");
    }
    Ok(side)
}

/// Write an HTML page to `path`, saying where on stderr (the stdout rendering is the answer).
fn write_html(path: &Path, html: &str) -> CommandResult<()> {
    std::fs::write(path, html).map_err(|e| {
        CliError::failed(format!("cannot write the HTML page to {}: {e}", path.display()))
    })?;
    eprintln!("vike-cli report: wrote the HTML page to {}", path.display());
    Ok(())
}

#[path = "report_tests.rs"]
#[cfg(test)]
mod report_tests;
