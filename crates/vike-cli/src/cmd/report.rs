//! `vike-cli report` — **a tearsheet, from either of TWO sources**: a finished run's own artifacts
//! on this machine, or the live journal a running `vike-tradehub` node is writing.
//!
//! # The two sources, and why one verb carries both
//!
//! Ruling 16 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` states the
//! rule this verb was born to close a hole in: **`vike-cli <X>` asks the backend to do X;
//! `vike-backend <X>` does X here.** Measuring the two rosters found `report` on the backend with no
//! client half at all — and it is the sharper of the two cases the ruling names, because
//! `crates/vike-report/src/tearsheet_cli.rs` renders from the LIVE JOURNAL and reads
//! `VIKE_JOURNAL_DIR`, a directory that exists on the DAEMON'S BOX. So getting a tearsheet over
//! your own live trading meant opening an SSH session to the production server.
//!
//! ⚠ **That left a second hole, and it is the bigger one: THIS verb could not report on a run it
//! had already computed.** A tearsheet over a finished backtest touches no node, no journal and no
//! network — it is a re-render of `<project>/user_data/runs/<id>/`, which is sitting on the disk of
//! the machine the operator is typing on. Four of the ten competitor tools with any backtest
//! surface can do it (Freqtrade's `backtesting-show`, LEAN's `lean report`, QuantRocket's
//! `zipline tearsheet`) and this one could not, while the verb NAMED `report` refused every
//! invocation for want of a daemon-side renderer nobody had built. So the capability was missing,
//! not the verb: `report` gained a second SOURCE rather than the backtest plane gaining a second
//! `show`.
//!
//! **The grammar says which source, and exactly one per invocation:** a positional run SELECTOR is
//! the stored source, `--node <host:port>` is the live one, and naming both is a refusal rather
//! than a precedence rule ([`refuse_a_second_source`]). A precedence would mean an operator who
//! typed both got an answer about one of them with nothing saying which.
//! [`crate::cmd::report::stored`] is the stored renderer; everything below the
//! [`Source::Node`] arm here is the live one, unchanged.
//!
//! # Which daemon the LIVE source asks, and why it needed a new wire verb
//!
//! The journal is written by the TRADING daemon, so this asks a `vike-tradehub` node — not the
//! data server and not the compute one, neither of which has ever seen a fill. That node's
//! protocol had no verb returning fills, trades or a tearsheet
//! (`vike_tradehub_client::proto`'s `Request` enumerates them), so serving this from what
//! exists was not available: the verb is new. `vike_tradehub_client::proto`'s
//! `FEATURE_TEARSHEET` and `vike_tradehub_client::remote_handle`'s `tearsheet` are the wire and
//! client halves, and both shipped with this command.
//!
//! # ⚠ The LIVE server half HAS shipped, and this section said the opposite
//!
//! It read: *"No `vike-tradehub` build advertises the `tearsheet` capability yet: the daemon-side
//! renderer … is a follow-up"*, and argued that advertising a capability before its arm worked
//! would turn a clean client-side refusal into an opaque server error. That argument was right and
//! it was honoured — the arm and the capability string shipped TOGETHER:
//! `crates/vike-tradehub/src/server.rs`'s `Request::Tearsheet` arm folds the journal through
//! `vike_report::tearsheet_from_journal`, its `served_features` pushes `FEATURE_TEARSHEET`,
//! and that file's `the_tearsheet_capability_is_advertised_now_that_the_arm_serves_it` holds the
//! two equal so neither can move without the other.
//!
//! **What that changes for a `--node` caller:** the negotiation now PASSES against any node built
//! from this tree, so the tearsheet is what an operator meets rather than [`failure_lines`]. That
//! refusal is not dead code — it is what an OLDER daemon still gets, which is the case a
//! capability negotiation exists for in the first place. Its wording is unchanged and still exact;
//! what moved is where it is PROVEN, from an integration test against a real node (no node this
//! tree ships can produce the state any more) to this module's own unit tests, which construct the
//! sentence directly.
//!
//! ⚠ **A node that SERVES the verb can still refuse this call, for a different reason and in
//! different words**, and telling the two apart is the operator's whole problem: it may be unable
//! to resolve its own journal directory — started with no settings source, journaling off, or a
//! configured path that is not there. Those are the NODE's sentences and they reach the operator
//! verbatim; that daemon's `tearsheet_reply` is the authority for each. The one thing this side
//! must never do is answer any of them with a fabricated empty tearsheet, which would report a
//! flat account to somebody whose account is not flat.
//!
//! [`failure_lines`] is still written to be worth meeting when it does fire: it carries the
//! client's own sentence (which names the missing capability and that nothing was sent), then the
//! ONE thing that works against such a daemon — the same renderer, run on the box that holds the
//! journal, with the flags this invocation already supplied carried across so nobody has to
//! translate them by hand.
//!
//! ⚠ **What changed with the stored source is that this verb is no longer USELESS on every box.**
//! The line above used to be the whole command. It is now one of two sources, and the other one
//! works everywhere — which is why the refusal's own wording was left exactly as it was: it is
//! still true, and it is still about the node.
//!
//! # Read-only, so the OBSERVE key suffices — and the stored source needs no key at all
//!
//! A tearsheet changes nothing, so the wire verb answers under either scope and the client always
//! connects under `Scope::Read` — one credential, resolved by the dispatcher exactly as
//! `trade status` resolves its own ([`crate::cmd::nodekeys`] argues the precedence). A resolved
//! CONTROL key cannot substitute: the node verifies each scope against its own key, which is why
//! the absent-key message is the observe-specific one.
//!
//! ⚠ The keyring is consulted ONLY on the node arm. A stored run is a file, so a box with no node
//! key, no node and no network reports on its own backtests — and demanding a credential to read a
//! local directory would be the shape of gate this workspace refuses everywhere else.
//!
//! # Output, and the ONE document that is versioned
//!
//! The node answers with the `vike_analytics::LiveTearsheet` JSON. `--json` emits it verbatim;
//! without it that same document is pretty-printed. That is the `backtest` verb's convention, and
//! it WAS a dependency decision rather than a rendering preference: the human table lived in
//! `vike_report`, whose graph is `vike-journal`/`vike-exec`/`vike-data`, and this crate's identity
//! is the light DataFusion-free one `scripts/ci_feature_suite.sh`'s `light-consumers` lane exists
//! to keep, so linking the renderer to format a table would have charged every `vike-cli` user
//! for it.
//!
//! ⚠ **That argument EXPIRED on 2026-09-28, and the behaviour deliberately did not move with it.**
//! The document, its `Display` table and its HTML renderer moved into `vike-analytics`, which this
//! crate already links, so parsing the node's body into a `vike_analytics::LiveTearsheet` and
//! printing its table would now cost nothing. Doing so is an OUTPUT change — the human form of
//! `report --node` would stop being pretty-printed JSON — and it was not ridden in with the move
//! that made it free; it is an open decision, not an oversight.
//!
//! The STORED source's document is this crate's own, so it carries a version and a published shape:
//! `crate::cmd::report::schema` owns both, and `report --schema` prints the JSON Schema with no run,
//! no node and no key. ⚠ The node's body is deliberately NOT versioned by this side — stamping a
//! schema onto bytes this crate did not write would be claiming a contract it cannot keep; that
//! hole is declared in `crate::cmd::report::schema`'s module doc, where the fix (version it beside
//! its producer, in `vike-report`) is named.
//!
//! # What is / is not tested
//!
//! The grammar ([`parse`]), the live rendering ([`render`]) and both halves of the live failure
//! mapping ([`failure_lines`] for the words, [`failure_exit`] for the rung) are PURE and
//! unit-tested below; the stored renderer's own refusals are unit-tested in
//! [`crate::cmd::report::stored`], over real temporary run directories.
//!
//! ⚠ **The failure-mapping tests CONSTRUCT the `io::Error` they then assert on**, which means they
//! pin the wording and can say nothing about whether an invocation reaches it — and any claim about
//! WHICH outcome a `--node` run meets is a claim about reachability.
//! `crates/vike-cli/tests/study_report_refusal_cli.rs` is the half that answers it: the shipped
//! binary against a REAL paper `vike-tradehub` node, stood up with no settings source. That node
//! SERVES the verb and cannot resolve its own journal, so its assertions run the opposite way from
//! the ones this section used to describe — the frame IS sent, the NODE's own sentence must reach
//! the operator verbatim, and the CLIENT's capability sentence must be ABSENT. Read in reverse,
//! those two observations are what a silently-dropped `FEATURE_TEARSHEET` fails on.
//!
//! ⚠ **This section asserted the pre-arm world in three places, each exact when written**: that
//! the node's `served_features` *"genuinely omits `tearsheet`"*; that the test proves, *"by the
//! node's own `Request::Tearsheet` arm staying silent — that nothing was sent"*; and that a
//! SUCCESSFUL node call cannot be tested because *"there is no renderer to answer it"*. The
//! renderer is `crates/vike-tradehub/src/server/tearsheet.rs`'s `tearsheet_reply` and it landed with the
//! advertisement. What is genuinely uncovered END TO END is now the opposite pair: a `--node` call
//! that returns a document (the node's own success path is unit-tested beside the renderer, in that
//! file's `server::tests::tearsheet_reply`, and [`render`] is unit-tested below), and the CAPABILITY
//! refusal, which no node this tree builds can produce any more — that one is pinned by this
//! module's unit tests over [`failure_lines`], a move the e2e test declares on itself.

// The stored-run renderer and the published JSON schema of what it emits.
pub(crate) mod schema;
pub(crate) mod stored;

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::nodekeys::{NodeKeyring, OBSERVE_KEY_ENV};
use crate::cmd::report::schema::schema_text;
use crate::cmd::report::stored::{Period, run_stored};
use crate::exit::Exit;

// ⚠ **The live-source paragraph below said the OPPOSITE of what this tree does, and it is the one
// text most operators read.** It was: *"NO NODE SERVES THE LIVE SOURCE YET. The daemon-side
// renderer is a follow-up of ruling 16, so a --node run that REACHES a node ends in a refusal
// naming `vike-backend report` … and nothing goes on the wire."* Exact when it was written, and
// falsified by the commit range that added `crates/vike-tradehub/src/server/tearsheet.rs`'s `tearsheet_reply`
// and the `served_features` push of `FEATURE_TEARSHEET` — which swept this module's own doc (see
// its LIVE-server-half section) and not this constant, so `report --help` went on telling
// operators the capability could not work while the binary served it. The REFUSAL half of the old
// paragraph is KEPT, because [`failure_lines`] is live code and still exact for an older daemon;
// what left is the word YET and the claim that every node refuses.
pub(crate) const USAGE: &str = "\
usage: vike-cli report <run> [--breakdown day|month] [--json]
       vike-cli report --node <host:port> [--seed CASH] [--periods-per-year N] [--json]
       vike-cli report --schema

Render a tearsheet. TWO SOURCES, and exactly one per invocation — naming both is refused rather
than resolved by a precedence nobody can see:

  <run>            a FINISHED run in this project's run directory: a run id, a unique prefix,
                   @last, @last:<kind>, or a @mark. Artifact-only — no engine, no store, no
                   socket, no node, and no credential. This is the source that re-reports work
                   you have already done, on the machine you are typing on.
  --node HOST:PORT the LIVE journal a running vike-tradehub node is writing: the realized-trade
                   round-trips reconstructed from its fill stream. Read-only — it authenticates
                   under the OBSERVE scope and can neither place nor change anything. The journal
                   never crosses the wire; the node renders and returns the answer.

⚠ THE LIVE SOURCE NEEDS A NODE THAT SERVES IT, and any vike-tradehub built from this tree does: it
advertises the `tearsheet` capability and renders from the journal it is writing. An OLDER daemon
does not, and a --node run against one is refused with nothing on the wire, naming `vike-backend
report` — the same renderer, on the box that holds the journal — with these flags already carried
across. A node that DOES serve the verb can still refuse for a reason of its OWN — no command
journal enabled in the environment it was started with, or a journal it cannot read — and those
are that node's own words, reaching you verbatim rather than as a fabricated empty tearsheet. (A
run that reaches no node reports the connection, as any client would; that is a node to fix, not a
capability that is missing.) The stored source above is unaffected and works on every box.

⚠ A STORED statistic is refused rather than approximated. A run's equity curve is DECIMATED past
`vike_model::runs::MAX_EQUITY_SAMPLES`, and a number recomputed from a thinned curve can disagree
with the run's own report.json. So --breakdown REFUSES a thinned curve (naming the stride), and the
recomputed peak and drawdown are `null` with the reason beside them. `backtest show <run> --export
equity` hands you the stored curve itself, stride and all.

The observe key is resolved like every node key and ONLY for --node: VIKE_TRADEHUB_OBSERVE_KEY in
the process environment first, then the settings database (its node_key table) — the same store
the daemon reads.

options:
  --node HOST:PORT       the node's observe address (selects the LIVE source)
  --breakdown PERIOD     per-period returns off the stored curve: day | month (stored source only)
  --seed CASH            the account's starting capital, which scales the return ratios
                         (live source only; omitted: the node's own default)
  --periods-per-year N   the Sharpe/Sortino/Calmar/CAGR annualization factor
                         (live source only; omitted: the node's own default)
  --json                 the machine-readable document instead of the human rendering. On the live
                         source that is the node's JSON verbatim, unformatted
  --schema               print the JSON Schema for the STORED document and exit — no run, no node,
                         no key. The live source's document belongs to its producer and is not
                         versioned by this side
  -h, --help             this message";

/// Which source this invocation names. Exactly one, decided by [`parse`] — see this module's doc
/// for why there is no precedence between them.
#[derive(Debug, PartialEq)]
enum Source {
    /// A run SELECTOR: a finished run's own directory, on this machine.
    Run(String),
    /// `--node <host:port>`: the live journal of a running `vike-tradehub` node.
    Node(String),
    /// `--schema`: the published shape of the stored document, and nothing else at all.
    Schema,
}

/// The parsed `report` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq)]
struct Args {
    /// Which of the three things this invocation asked for.
    source: Source,
    /// `--seed`: the account's starting capital. `None` ⇒ the node's own default, never a number
    /// invented here — the renderer owns that default and a client guess would silently rescale
    /// every return ratio in the answer. LIVE source only.
    seed: Option<f64>,
    /// `--periods-per-year`: the annualization factor. `None` ⇒ the node's own default, for the
    /// same reason as `seed`. LIVE source only.
    periods_per_year: Option<f64>,
    /// `--breakdown`: bucket the stored curve's returns by period. STORED source only, and refused
    /// outright on a thinned curve — `crate::cmd::report::stored`'s `breakdown_of` carries why.
    breakdown: Option<Period>,
    /// `--json`: the machine-readable document instead of the human rendering.
    json: bool,
}

/// Parse everything after the `report` subcommand. Accepts `--flag value` and `--flag=value` via
/// the shared [`crate::cmd::args`] glue; `-h`/`--help` short-circuits through [`help_requested`]
/// so [`exit_for_parse_error`] turns it into a SUCCESS on stdout.
fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut node: Option<String> = None;
    let mut run: Option<String> = None;
    let mut schema = false;
    let mut seed: Option<f64> = None;
    let mut periods_per_year: Option<f64> = None;
    let mut breakdown: Option<Period> = None;
    let mut json = false;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--node" => node = Some(flags.value(&flag, inline)?),
            "--seed" => {
                let raw = flags.value(&flag, inline)?;
                seed = Some(number(&flag, &raw)?);
            }
            "--periods-per-year" => {
                let raw = flags.value(&flag, inline)?;
                periods_per_year = Some(number(&flag, &raw)?);
            }
            "--breakdown" => {
                let raw = flags.value(&flag, inline)?;
                breakdown = Some(Period::parse(&raw)?);
            }
            "--schema" => {
                no_value(&flag, inline)?;
                schema = true;
            }
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            other if other.starts_with("--") => return Err(format!("unknown argument: {other}")),
            // The run SELECTOR. ⚠ A positional carrying an `=` was split by the shared flag
            // iterator, so it is put back rather than silently truncating a selector somebody
            // typed — the same repair `crate::cmd::backtest`'s `parse_read` makes.
            positional => {
                let token = match inline {
                    Some(v) => format!("{positional}={v}"),
                    None => positional.to_string(),
                };
                if let Some(first) = &run {
                    return Err(format!(
                        "`report` renders ONE run and was given two selectors ('{first}' and \
                         '{token}'). There is no implied comparison here — `vike-cli backtest diff \
                         <a> <b>` is the verb that takes two runs."
                    ));
                }
                run = Some(token);
            }
        }
    }
    let source = claim_source(node, run, schema)?;
    let args = Args { source, seed, periods_per_year, breakdown, json };
    refuse_a_flag_the_source_cannot_use(&args)?;
    Ok(args)
}

/// Resolve the three source spellings to exactly one, refusing NONE and MORE THAN ONE by name.
///
/// ⚠ **Neither refusal may become a precedence rule.** An operator who typed a run selector AND
/// `--node` believes both matter; answering about one of them silently is the "silent precedence"
/// the run-selector grammar refuses everywhere else
/// (`crate::cmd::runs::selector`'s `resolve_one` names the candidates rather than picking the most
/// recent). And a bare `vike-cli report` cannot default to either: defaulting to the node would dial
/// an address nobody gave, and defaulting to `@last` would report on whatever happened to run most
/// recently.
fn claim_source(
    node: Option<String>,
    run: Option<String>,
    wants_schema: bool,
) -> Result<Source, String> {
    // Built by pushes rather than by a filtered array, so the REFUSAL names the sources in the same
    // order the usage lists them and the message cannot be assembled from a different roster than
    // the count was taken from.
    let mut named: Vec<&str> = Vec::new();
    if node.is_some() {
        named.push("--node");
    }
    if run.is_some() {
        named.push("<run>");
    }
    if wants_schema {
        named.push("--schema");
    }
    if named.len() > 1 {
        return Err(refuse_a_second_source(&named));
    }
    if wants_schema {
        return Ok(Source::Schema);
    }
    if let Some(node) = node {
        return Ok(Source::Node(node));
    }
    match run {
        Some(selector) => Ok(Source::Run(selector)),
        None => Err(NO_SOURCE.to_string()),
    }
}

/// The sentence a source-less invocation is refused with. A `const` so [`claim_source`] reads as
/// three short decisions rather than as one wrapped string literal, and so the test that pins the
/// wording names the same text the parser returns.
const NO_SOURCE: &str = "`report` needs a SOURCE: a run selector for a finished run on this \
     machine (`vike-cli report @last`), or `--node <host:port>` for a running vike-tradehub node's \
     live journal. `--schema` prints the stored document's shape and needs neither.";

/// The sentence two-or-three sources are refused with. Split out so the wording is pinned by a unit
/// test and so the roster in the message is the one [`claim_source`] actually counted.
fn refuse_a_second_source(named: &[&str]) -> String {
    format!(
        "`report` renders ONE source per invocation and {} were named. Pick one: a run selector \
         reads a finished run's own directory, `--node` asks a running node about its live journal, \
         and `--schema` prints the stored document's shape. They answer different questions, so \
         there is deliberately no precedence between them.",
        named.join(" and ")
    )
}

/// A flag that belongs to the OTHER source is refused by name with the reason, never dropped —
/// `crate::cmd::backtest`'s `refuse_foreign_read_flags` carries the argument: an operator who typed
/// it is not looking for "unknown option", they want to know which source takes it.
///
/// ⚠ `--seed` and `--periods-per-year` are refused on the stored source rather than ignored, and
/// that is the load-bearing one. They RESCALE every return ratio in an answer, the stored source
/// recomputes nothing from them, and a silently-dropped `--seed` would hand somebody ratios they
/// believe are scaled to their account — the same defect [`number`] refuses a NaN for, one layer up.
fn refuse_a_flag_the_source_cannot_use(a: &Args) -> Result<(), String> {
    // ⚠ EXHAUSTIVE over [`Source`], with no `_` arm, so a third source cannot be added without
    // deciding what each of these flags means for it. A catch-all here would silently ACCEPT every
    // one of them on the new source, which is the inert-flag defect this function exists to refuse.
    let wrong: Option<(&str, &str)> = match &a.source {
        Source::Run(_) if a.seed.is_some() => Some(("--seed", SEED_IS_LIVE_ONLY)),
        Source::Run(_) if a.periods_per_year.is_some() => {
            Some(("--periods-per-year", PPY_IS_LIVE_ONLY))
        }
        Source::Run(_) => None,
        Source::Node(_) if a.breakdown.is_some() => Some(("--breakdown", BREAKDOWN_IS_STORED)),
        Source::Node(_) => None,
        // ⚠ `--schema` answers about the DOCUMENT, not about a run or a node, so every other flag
        // on the line was written in the belief that it would change the output. None of them can.
        Source::Schema => first_other_flag(a).map(|flag| (flag, NOTHING_SELECTS_A_SCHEMA)),
    };
    match wrong {
        Some((flag, why)) => Err(format!("{flag} {why}")),
        None => Ok(()),
    }
}

/// The first flag on the line that is not a SOURCE, or `None`. Only `--schema` asks — it is the one
/// source for which every remaining flag is meaningless, so the four are answered by one sentence
/// rather than by four near-identical ones.
fn first_other_flag(a: &Args) -> Option<&'static str> {
    if a.seed.is_some() {
        return Some("--seed");
    }
    if a.periods_per_year.is_some() {
        return Some("--periods-per-year");
    }
    if a.breakdown.is_some() {
        return Some("--breakdown");
    }
    if a.json {
        return Some("--json");
    }
    None
}

/// Why `--seed` cannot be honoured on a stored run. The load-bearing refusal of the four: a
/// silently-dropped seed hands somebody ratios they believe are scaled to their account.
const SEED_IS_LIVE_ONLY: &str = "is the LIVE renderer's starting capital and scales every return \
     ratio it computes. A stored run recomputes no ratios at all — report.json's own are carried \
     through verbatim — so honouring it is impossible and dropping it silently would hand you \
     ratios you believe were rescaled. Use --node for the live journal, or re-run the backtest \
     with the cash you meant.";

/// Why `--periods-per-year` cannot be honoured on a stored run: the annualization already happened,
/// and redoing it needs the per-bar returns a decimated curve cannot give back.
const PPY_IS_LIVE_ONLY: &str = "is the LIVE renderer's annualization factor, and a stored run's \
     Sharpe was annualized when it RAN. Re-annualizing it here would need the per-bar returns, \
     which the stored curve cannot give back once it is decimated. Use --node, or set the factor \
     in the profile and re-run.";

/// Why `--breakdown` cannot be honoured on the live source: there is no stored curve to bucket, and
/// this side does not re-shape the node's own document.
const BREAKDOWN_IS_STORED: &str = "buckets a STORED equity curve by calendar period, and the live \
     source has no stored curve — the node renders its own document and this side does not \
     re-shape it. Name a run selector instead.";

/// Why nothing at all applies to `--schema`.
const NOTHING_SELECTS_A_SCHEMA: &str = "does not apply to --schema, which prints the stored \
     document's published SHAPE rather than any run's data. The schema is itself JSON and has one \
     form, so there is nothing here for a flag to select.";

/// One numeric flag value, refused by NAME when it does not parse.
///
/// ⚠ A non-finite value is refused too, and that is not pedantry: both numbers reach the node as
/// `Option<f64>` on the wire, `serde_json` writes a NaN or an infinity as `null`, and the node
/// would then apply its own default while the operator believes they set one. This is the only
/// place the difference is visible.
fn number(flag: &str, raw: &str) -> Result<f64, String> {
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        Ok(_) => Err(format!("{flag} must be a finite number, got {raw:?}")),
        Err(e) => Err(format!("{flag} must be a number, got {raw:?} ({e})")),
    }
}

/// The NODE's answer, as this command emits it: verbatim under `--json`, pretty-printed otherwise.
///
/// A body that is not valid JSON is an error rather than something to echo — the node's contract
/// is the `LiveTearsheet` document, and passing an unparsed body through would hand a script
/// something it cannot read while reporting success.
fn render(body: &str, json: bool) -> Result<String, String> {
    if json {
        return Ok(body.to_string());
    }
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("the node's tearsheet was not valid JSON: {e}"))?;
    serde_json::to_string_pretty(&value).map_err(|e| format!("cannot format the tearsheet: {e}"))
}

/// The `vike-backend report` invocation that does this work ON the node's own box, carrying the
/// numeric flags this invocation supplied.
///
/// ⚠ It is part of the REFUSAL, not an alternative offered casually. Against a daemon that does
/// NOT serve the verb, an operator who typed `vike-cli report --node …` needs the thing that
/// works, spelled correctly — and the flags are carried across rather than left to be re-typed
/// because a `--seed` dropped in translation rescales every return ratio in the answer without
/// saying so. The journal directory is deliberately NOT guessed: it is the node's
/// `config.journal_dir` (or its `VIKE_JOURNAL_DIR`), a fact this side cannot see.
///
/// ⚠ **That clause read *"While no node serves the verb"*.** It was exact until the commit range
/// that shipped `crates/vike-tradehub/src/server/tearsheet.rs`'s `tearsheet_reply` and its `served_features`
/// push of `FEATURE_TEARSHEET`. Nothing in the argument above moved with it: an OLDER daemon still
/// meets this line, and carrying the flags across is still what keeps a hand-translated
/// invocation from silently rescaling every ratio in the answer. Only the SCOPE changed, from
/// every node to a daemon that predates the arm.
fn backend_fallback(args: &Args) -> String {
    let mut line = String::from("  vike-backend report --journal <the node's journal directory>");
    if let Some(seed) = args.seed {
        line.push_str(&format!(" --seed {seed}"));
    }
    if let Some(ppy) = args.periods_per_year {
        line.push_str(&format!(" --periods-per-year {ppy}"));
    }
    if args.json {
        line.push_str(" --json");
    }
    line
}

/// Map the LIVE call's failure onto the lines this command writes to stderr — PURE, so the wording
/// is pinned by a unit test rather than by standing up a node.
///
/// `node` is the address the call was made to. It is a PARAMETER rather than read off `args`
/// because only the [`Source::Node`] arm has one, and a getter that answered `""` for the other two
/// sources would be a function whose return value is a lie on two of three inputs.
///
/// * [`io::ErrorKind::Unsupported`] — the FEATURE-NEGOTIATION refusal, which is what an OLDER
///   daemon answers with. The client's own sentence names the capability and says nothing was sent;
///   the lines added here say what to do instead, on the box that holds the journal. ⚠ This bullet
///   read *"and TODAY THE ONLY OUTCOME of a call that reaches a node (no shipped build advertises
///   `tearsheet`)"*, which was exact until the commit range that added
///   `crates/vike-tradehub/src/server/tearsheet.rs`'s `tearsheet_reply` and its `served_features` push of
///   `FEATURE_TEARSHEET`. Against a node built from this tree the negotiation PASSES, so the
///   outcome is a tearsheet or that node's own `Response::Error` sentence
///   (`vike_tradehub_client::proto`). The arm below is not dead — it is simply no longer the
///   ordinary answer, and reading it as the ordinary one is what would justify leaving the
///   success path untested.
/// * [`io::ErrorKind::PermissionDenied`] — the handshake was refused: the presented observe key
///   did not verify. Points at the key, not the verb.
/// * anything else — an honest transport-shaped report naming the address.
fn failure_lines(node: &str, args: &Args, err: &io::Error) -> Vec<String> {
    match err.kind() {
        io::ErrorKind::Unsupported => vec![
            format!("vike-cli report: {err}"),
            format!(
                "the node at {node} runs a vike-tradehub build with no server-side renderer for \
                 this verb. Until it has one, run the same renderer ON that box:"
            ),
            backend_fallback(args),
        ],
        io::ErrorKind::PermissionDenied => vec![
            format!("vike-cli report: the node at {node} refused the observe handshake: {err}"),
            format!(
                "the presented {OBSERVE_KEY_ENV} does not match that node's — `vike-cli secrets \
                 path` names the store this side read it from"
            ),
        ],
        _ => vec![format!("vike-cli report: cannot query {node}: {err}")],
    }
}

/// The RUNG the same failure exits on — [`failure_lines`]' twin, split out so the words and the
/// number are decided over one `ErrorKind` match and cannot come to disagree.
///
/// The rule is `crates/vike-cli/src/cmd/trade/status.rs`'s `failure_exit`, deliberately
/// unchanged: **THE NODE ANSWERED** ⇒ the pre-existing [`Exit::Failed`] rung, because a node
/// missing a capability, a key that does not verify and a node-side refusal are permanent
/// configuration facts about a REACHABLE box, and a wrapper that retried any of them would loop
/// forever. Only a socket that never got an answer is [`Exit::Connect`].
fn failure_exit(err: &io::Error) -> Exit {
    match err.kind() {
        io::ErrorKind::Unsupported
        | io::ErrorKind::PermissionDenied
        | io::ErrorKind::InvalidData => Exit::Failed,
        _ => Exit::Connect,
    }
}

/// Entry point the dispatcher routes to. `args` is everything AFTER the `report` subcommand.
///
/// `keys` is the [`NodeKeyring`] the dispatcher resolved (process environment first, then the
/// node-key store the daemon itself reads) — the [`Source::Node`] arm uses its observe half, and
/// the other two arms never touch it.
///
/// `runs_root` (`<project>/user_data/runs`) and `marks_root` (its SIBLING, never its child) are the
/// stored source's inputs. They are PARAMETERS for the rule
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` ratchets down: a `src/cmd/` file may
/// not resolve a project for itself, because the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-
/// blind and a second walk would answer about whatever directory the process happens to sit above.
/// `None` is a process with no project above the working directory, refused with a sentence.
pub fn run(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    runs_root: Option<PathBuf>,
    marks_root: Option<PathBuf>,
) -> ExitCode {
    let parsed = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("report", USAGE, &msg),
    };
    match &parsed.source {
        // ⚠ No run, no node, no credential and no project: the published shape of the document is a
        // fact about this BUILD, so it must answer on a box where nothing else here does.
        Source::Schema => {
            println!("{}", schema_text());
            ExitCode::SUCCESS
        }
        Source::Run(selector) => {
            match run_stored(
                runs_root.as_deref(),
                marks_root.as_deref(),
                selector,
                parsed.breakdown,
                parsed.json,
            ) {
                // ⚠ `print!`, not `println!`: `run_stored` guarantees exactly one trailing newline
                // on both of its renderings, the same contract `crate::cmd::runs::show`'s `run_show`
                // relies on. A `println!` here would double it for the human form.
                Ok(out) => {
                    print!("{out}");
                    ExitCode::SUCCESS
                }
                // ⚠ The rung comes from the renderer, which classified the failure where it KNEW
                // what was missing — a run with no curve is `Exit::Empty`, a thinned curve is
                // `Exit::Failed`. Collapsing both to one number here would throw that away.
                Err(e) => {
                    eprintln!("vike-cli report: {}", e.msg);
                    e.exit.into()
                }
            }
        }
        Source::Node(node) => {
            // The observe key SPECIFICALLY: a control key cannot authenticate a read (the node
            // verifies each scope against its own key), so `has_any()` would be the wrong question.
            let Some((key, _origin)) = keys.observe() else {
                eprintln!("{}", keys.observe_absent_message("report"));
                return ExitCode::FAILURE;
            };
            let call = vike_tradehub_client::tearsheet(
                node.as_str(),
                key.as_bytes(),
                parsed.seed,
                parsed.periods_per_year,
            );
            match call {
                Ok(body) => match render(&body, parsed.json) {
                    Ok(out) => {
                        println!("{out}");
                        ExitCode::SUCCESS
                    }
                    Err(msg) => {
                        eprintln!("vike-cli report: {msg}");
                        Exit::Failed.into()
                    }
                },
                Err(e) => {
                    for line in failure_lines(node, &parsed, &e) {
                        eprintln!("{line}");
                    }
                    failure_exit(&e).into()
                }
            }
        }
    }
}

#[path = "report_tests.rs"]
#[cfg(test)]
mod report_tests;
