//! The reading sub-verbs: their roster, their one flag parser and their dispatcher.

use crate::cmd::args::{self, Flags};
use crate::exit::CmdResult;

use super::run_args::{PARAMS_REFUSED, refuse_a_run_flag_on_params};

/// The STORED-RUN sub-verbs, which take a sub-verb TOKEN rather than flags.
///
/// ⚠ **Spec decision 11 — the sub-verb is ALWAYS required — HAS landed**, and this doc said the
/// opposite until it did. Stage 3 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` deleted the bare flag form
/// (`backtest --profile run.toml` is now an exit-2 naming the roster), and what this enum predicted
/// then happened: these arms joined [`Sub`], as its [`Sub::Read`] payload. The PEEK is gone —
/// [`claim_subcommand`] is the one router, and it resolves a reading token through
/// [`ReadSub::from_token`] rather than by re-listing the names.
///
/// It stayed a SEPARATE enum rather than being flattened into [`Sub`] because the reading plane has
/// its own arity rule, its own foreign-flag refusals and its own parser, all of which name this
/// type; [`Sub`]'s own doc carries that argument.
///
/// ⚠ **The name says READ and two members WRITE.** [`ReadSub::Tag`] appends to a run's optional
/// sidecar and to the marks store; it touches no manifest and no report, so nothing it writes can
/// make a run unreadable — which is what lets it sit in this family.
/// `crate::cmd::runs`'s module doc is where that exception is argued. [`ReadSub::Templates`] is the
/// second: it CREATES a `.rhai` file, and it may create only one that does not exist yet
/// (`crate::cmd::strategies`'s `run_templates` argues the no-clobber rule), so like `tag` it can
/// make nothing unreadable.
///
/// ⚠ **The name also says RUN, and the authoring three read no run at all.** `templates` and
/// `script-api` answer from consts compiled into this binary and `script-check` from one file the
/// operator named — no socket, no engine, no store, and no run directory either. They are in this
/// family because they share its parser, its arity rule and its foreign-flag refusals, and they
/// live in `crate::cmd::strategies` rather than under `crate::cmd::runs` because that module's
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
}

/// Every stored-run sub-verb, in the order [`USAGE`] lists them. DERIVED into every message that
/// names the roster — `crate::cmd::data`'s `SUBCOMMANDS` doc carries what the hand-typed copy cost
/// there.
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
];

impl ReadSub {
    /// The name the operator typed, which is also what every refusal names it by.
    pub(crate) fn as_str(self) -> &'static str {
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
        }
    }

    pub(super) fn from_token(token: &str) -> Option<Self> {
        READ_SUBCOMMANDS.iter().copied().find(|s| s.as_str() == token)
    }
}

/// The parsed reading command line. Every field is shared across the reading sub-verbs and
/// refused by name where it does not apply — [`refuse_foreign_read_flags`], and the reason each
/// carries.
#[derive(Debug)]
pub(crate) struct ReadArgs {
    pub(crate) sub: ReadSub,
    /// The selector (`ls` filter, `show`/`path`/`tag`/`gate` target, `diff`'s LEFT operand). See
    /// `crate::cmd::runs::selector`.
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
    /// ⚠ It was a member of `crate::cmd::runs::show`'s `UNBUILT_RENDERERS` until this change,
    /// refused with a sentence blaming a missing renderer. The renderer was never missing:
    /// `vike_analytics::render_html` is exported under no feature at all, and
    /// `vike_analytics::LiveTearsheet::from_report`'s doc names `show.rs` by path as the caller it
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
    /// `--export VALUE` — one of `vike_model::runs`'s stored documents, raw. See
    /// `crate::cmd::runs::show`'s `export_document` for which values are served and why `fills`
    /// is refused by NAME rather than by refusing the flag.
    pub(crate) export: Option<String>,
    // tag
    /// `--add TAG`, REPEATABLE. A label on the run, in its own sidecar.
    pub(crate) add: Vec<String>,
    /// `--note TEXT`. Appended, never replacing — `vike_model::runs::add_tags` owns that rule.
    pub(crate) note: Option<String>,
    /// `--as NAME`. The MARK — the stable second operand `gate --against` takes.
    pub(crate) mark_as: Option<String>,
    // gate
    /// `--against <selector>`, REQUIRED by `gate`. Usually a mark, because a run id moves.
    pub(crate) against: Option<String>,
    /// `--fail-if EXPR`, REQUIRED by `gate`. See `crate::cmd::runs::failif`.
    pub(crate) fail_if: Option<String>,
    // diff
    /// `--all`: show unchanged leaves too. The negation of the default.
    pub(crate) all: bool,
    /// `--changed-only`: the DEFAULT, accepted so a script that says what it means is not refused.
    pub(crate) changed_only: bool,
    /// `--md`: the same rows as a Markdown table.
    pub(crate) md: bool,
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
}

impl ReadArgs {
    /// Every flag at its default, for one sub-verb. [`parse_read`] fills it in and a unit test builds
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
            md: false,
            script: None,
            strategy: None,
            addr: None,
            write_strategy: None,
        }
    }
}

/// ONE flag loop, then per-sub-verb refusals — the shape `crate::cmd::data`'s `parse` has.
pub(super) fn parse_read(argv: &[&str]) -> Result<ReadArgs, String> {
    let mut it = argv.iter().map(|s| (*s).to_string());
    let first = it.next().ok_or_else(|| {
        format!(
            "a subcommand is required ({})",
            READ_SUBCOMMANDS.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | ")
        )
    })?;
    let sub = ReadSub::from_token(&first)
        .ok_or_else(|| format!("unknown `backtest` subcommand '{first}'"))?;

    let mut a = ReadArgs::empty(sub);
    let mut positionals: Vec<String> = Vec::new();

    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--json" => {
                args::no_value(&flag, inline)?;
                a.json = true;
            }
            "--out" => a.out = Some(flags.value(&flag, inline)?),
            "--where" => a.where_expr = Some(flags.value(&flag, inline)?),
            "--sort" => a.sort = Some(flags.value(&flag, inline)?),
            "--limit" => a.limit = Some(flags.value(&flag, inline)?),
            "--cols" => a.cols = Some(flags.value(&flag, inline)?),
            "--metrics" => {
                args::no_value(&flag, inline)?;
                a.metrics = true;
            }
            // ⚠ **The catalog LISTING, and it must sit as its own arm rather than fall anywhere
            // near `--metrics`.** The two are different questions — one selects a SECTION of a
            // stored run, the other enumerates what this tree can measure — and the guard below
            // that refuses their combination is what stops the second silently winning over the
            // first. It is exact-token like every arm in this loop, so `--metrics` does not answer
            // for it and it does not answer for `--metrics`.
            "--metrics-list" => {
                args::no_value(&flag, inline)?;
                a.metrics_list = true;
            }
            // ⚠ A DOCUMENT flag, like `--export` and `--json`, which is why the guard below
            // refuses their combinations rather than this loop ordering them. Bare: the
            // destination is `--out`, and an inline `--html=path` is a usage error rather than a
            // silently ignored value.
            "--html" => {
                args::no_value(&flag, inline)?;
                a.html = true;
            }
            "--trades" => {
                args::no_value(&flag, inline)?;
                a.trades = true;
            }
            "--export" => a.export = Some(flags.value(&flag, inline)?),
            "--config" => {
                args::no_value(&flag, inline)?;
                a.config = true;
            }
            // ⚠ REPEATABLE, and the only repeatable flag in this loop: `--add ci --add fee-fix` is
            // two labels, not the second overwriting the first.
            "--add" => a.add.push(flags.value(&flag, inline)?),
            "--note" => a.note = Some(flags.value(&flag, inline)?),
            "--as" => a.mark_as = Some(flags.value(&flag, inline)?),
            "--against" => a.against = Some(flags.value(&flag, inline)?),
            "--fail-if" => a.fail_if = Some(flags.value(&flag, inline)?),
            "--all" => {
                args::no_value(&flag, inline)?;
                a.all = true;
            }
            "--changed-only" => {
                args::no_value(&flag, inline)?;
                a.changed_only = true;
            }
            "--md" => {
                args::no_value(&flag, inline)?;
                a.md = true;
            }
            "--script" => a.script = Some(flags.value(&flag, inline)?),
            "--strategy" => a.strategy = Some(flags.value(&flag, inline)?),
            "--addr" => a.addr = Some(flags.value(&flag, inline)?),
            "--write-strategy" => a.write_strategy = Some(flags.value(&flag, inline)?),
            // ⚠ The §6.2 flags this stage cannot honour, refused BY NAME rather than falling into
            // "unknown option". They are in the design document an operator is reading from, so
            // "unknown option --html" would be a message about the wrong thing.
            //
            // ⚠ The ROSTER is `crate::cmd::runs::show`'s `UNBUILT_RENDERERS`, matched here rather
            // than re-typed as a pattern: that module owns both the list and the sentence for each
            // entry, and a literal `"--html" | "--breakdown" | …` here would be a second copy that
            // could fall behind the day one of them ships.
            //
            // ⚠ **THIS GUARD IS LAST, AND THE LITERAL ARMS ABOVE IT WIN.** `--metrics`, `--trades`,
            // `--config` and `--export` are matched by name earlier in this same `match`, so a flag
            // put BACK on `UNBUILT_RENDERERS` while its literal arm survives would never reach here:
            // the refusal becomes dead code and the flag silently no-ops. Retiring a flag is
            // therefore TWO edits — add the row there, delete the arm here — and
            // `an_unbuilt_renderer_is_refused_with_what_it_would_need` is what catches the pair
            // coming apart, because it drives every roster entry through this parser.
            other if crate::cmd::runs::show::UNBUILT_RENDERERS.contains(&other) => {
                return Err(crate::cmd::runs::show::refuse_an_unbuilt_renderer(other));
            }
            // ⚠ The RUN-ONLY flags, refused by name on `params` alone — stage 3's tightening, kept
            // when its `parse_params_args` was folded into this loop. It is scoped to that one
            // sub-verb because that is the scope stage 3 argued for and measured; every other
            // reading verb still answers "unknown option", unchanged. Same placement rule as the
            // guard above: AFTER the literal arms, so `--strategy` and `--json` — which this
            // sub-verb accepts — are taken by their own arms and never reach here.
            other if a.sub == ReadSub::Params && PARAMS_REFUSED.contains(&other) => {
                return Err(refuse_a_run_flag_on_params(other));
            }
            "-h" | "--help" => return args::help_requested(),
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            positional => match inline {
                // A positional carrying an `=` was split by the shared flag iterator; put it back
                // rather than silently truncating a selector somebody typed.
                Some(v) => positionals.push(format!("{positional}={v}")),
                None => positionals.push(positional.to_string()),
            },
        }
    }

    let allowed = match sub {
        ReadSub::Ls | ReadSub::Show | ReadSub::Tag | ReadSub::Gate => 1,
        ReadSub::Path | ReadSub::Diff => 2,
        ReadSub::Params | ReadSub::Strategies => 0,
        // `templates` takes an OPTIONAL starter id: absent is the roster, present is that
        // starter. ⚠ `script-check` takes NONE and names its file with `--script`, the same
        // spelling `params` uses for the same thing — a second grammar for "which .rhai file" is
        // exactly what decision 11 refuses across this plane.
        ReadSub::Templates => 1,
        ReadSub::ScriptApi | ReadSub::ScriptCheck => 0,
    };
    if positionals.len() > allowed {
        return Err(format!(
            "unexpected extra argument '{}' — `backtest {}` takes {allowed} positional argument(s)",
            positionals[allowed],
            sub.as_str()
        ));
    }
    match sub {
        // `ls` takes an OPTIONAL selector: absent is every run.
        ReadSub::Ls => a.selector = positionals.first().cloned(),
        // ⚠ `show --metrics-list` takes NO selector, and that is the flag's whole shape: it answers
        // out of the metric CATALOG, so there is no run for a selector to name and no runs
        // directory to read. `templates` makes the same move for the same reason — requiring a
        // positional would put the listing behind a run, on the box where somebody is deciding
        // what to type. A selector given anyway is kept and then unused, because the listing is the
        // answer either way, and refusing it would be a refusal about a token rather than about
        // anything an operator did wrong.
        ReadSub::Show if a.metrics_list => a.selector = positionals.first().cloned(),
        ReadSub::Show | ReadSub::Tag | ReadSub::Gate => {
            a.selector = Some(required_selector(sub, positionals.first())?);
        }
        ReadSub::Path => {
            a.selector = Some(required_selector(sub, positionals.first())?);
            a.file = positionals.get(1).cloned();
        }
        // ⚠ `diff` takes TWO runs and neither is optional. A one-operand `diff` that silently used
        // `@last` as the other side would compare against whatever happened to run most recently,
        // which is the "silent precedence" the selector grammar refuses everywhere else.
        ReadSub::Diff => {
            a.selector = Some(required_selector(sub, positionals.first())?);
            a.file = Some(
                positionals.get(1).filter(|s| !s.trim().is_empty()).cloned().ok_or_else(|| {
                    "`backtest diff` compares TWO runs — `backtest diff <a> <b>`. Name both; \
                         there is no implied second operand."
                        .to_string()
                })?,
            );
        }
        // ⚠ `templates` shares `ls`'s rule rather than `show`'s: an absent positional is the WHOLE
        // roster, not a missing operand. Requiring one would make the listing unreachable, and the
        // listing is the half no competitor's `new-strategy` prints.
        ReadSub::Templates => a.selector = positionals.first().cloned(),
        ReadSub::Params | ReadSub::Strategies | ReadSub::ScriptApi | ReadSub::ScriptCheck => {}
    }
    refuse_foreign_read_flags(&a)?;
    if sub == ReadSub::Show {
        crate::cmd::runs::show::refuse_a_listing_beside_a_run_rendering(&a)?;
        // ⚠ The SECOND of the two document rules, and both are needed because they answer
        // different questions: the one above refuses a CATALOG listing beside a run rendering,
        // this one refuses two RUN documents. They share
        // `crate::cmd::runs::show::run_rendering_flags_given`, which is what stops either from
        // being the file that forgets a flag the other knows about.
        crate::cmd::runs::show::refuse_a_second_document(&a)?;
    }
    if sub == ReadSub::Tag {
        crate::cmd::runs::tag::refuse_a_tag_that_writes_nothing(&a)?;
    }
    if sub == ReadSub::Gate {
        crate::cmd::runs::gate::refuse_an_ungateable_line(&a)?;
    }
    Ok(a)
}

fn required_selector(sub: ReadSub, given: Option<&String>) -> Result<String, String> {
    match given {
        Some(s) if !s.trim().is_empty() => Ok(s.clone()),
        _ => Err(format!(
            "`backtest {}` requires a run selector — one of: <run-id> | <unique-prefix> | @last | \
             @last:<kind>",
            sub.as_str()
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
    let diff_only = [("--all", a.all), ("--changed-only", a.changed_only), ("--md", a.md)];
    // ⚠ `--script` LEFT this roster when `script-check` shipped: two sub-verbs own it now, so it is
    // refused below by its own named rule, exactly as `--trades` is. A flag refused twice with two
    // messages is a flag whose two refusals can disagree.
    let params_only = [("--strategy", a.strategy.is_some())];
    let strategies_only = [("--addr", a.addr.is_some())];
    let templates_only = [("--write-strategy", a.write_strategy.is_some())];

    let refuse = |set: &[(&str, bool)], why: &str| -> Result<(), String> {
        for (flag, given) in set {
            if *given {
                return Err(format!(
                    "{flag} does not apply to `backtest {}` — {why}",
                    a.sub.as_str()
                ));
            }
        }
        Ok(())
    };
    if a.sub != ReadSub::Ls {
        refuse(&ls_only, "that flag narrows, orders or shapes a LISTING, and `ls` is the listing")?;
    }
    if a.sub != ReadSub::Show {
        refuse(
            &show_only,
            "that flag selects a SECTION of one stored run, which is what `show` renders",
        )?;
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
        return Err(format!(
            "--html does not apply to `backtest {}` — it renders ONE stored run's whole tearsheet              as an HTML page, which is `show`'s answer beside its --metrics: `backtest show <run>              --html --out sheet.html`",
            a.sub.as_str()
        ));
    }
    if a.sub != ReadSub::Show && a.metrics_list {
        return Err(format!(
            "--metrics-list does not apply to `backtest {}` — it prints the METRIC CATALOG (every \
             id this tree can measure, with what each one means), which is `show`'s answer to \
             \"what can I ask for\" beside its --metrics. It names no run and reads no runs \
             directory: `backtest show --metrics-list`",
            a.sub.as_str()
        ));
    }
    // ⚠ `--trades` is the FIRST flag TWO sub-verbs own (`--script` below is the second), so it is
    // refused on every other reading verb rather than living in either roster: `show --trades`
    // renders the ledger and `diff --trades` compares two of them, which is the same document
    // answering the same question from two sides.
    if !matches!(a.sub, ReadSub::Show | ReadSub::Diff) && a.trades {
        return Err(format!(
            "--trades does not apply to `backtest {}` — it names the stored trade ledger, which \
             `show` renders and `diff` compares",
            a.sub.as_str()
        ));
    }
    if a.sub != ReadSub::Tag {
        refuse(
            &tag_only,
            "that flag WRITES a label, a note or a mark onto a stored run, which is what `tag` does",
        )?;
    }
    if a.sub != ReadSub::Gate {
        refuse(
            &gate_only,
            "that flag names the BASELINE or the criteria a verdict is computed from, and `gate` is \
             the verb whose product is that verdict",
        )?;
    }
    if a.sub != ReadSub::Diff {
        refuse(&diff_only, "that flag shapes a two-run COMPARISON, which is what `diff` renders")?;
    }
    if a.sub != ReadSub::Params {
        refuse(&params_only, "that flag names the built-in strategy whose keys `params` lists")?;
    }
    // ⚠ `--script` is the SECOND flag two sub-verbs own, so it gets a named rule rather than a
    // roster row: `params` lists what a script DECLARES and `script-check` compiles it, which is
    // the same file answering two questions. Refusing it on `script-check` with the `params`
    // sentence would have sent an operator to the wrong verb — the exact cost
    // `crate::cmd::data`'s `refuse_foreign_flags` argues a named refusal exists to avoid.
    if !matches!(a.sub, ReadSub::Params | ReadSub::ScriptCheck) && a.script.is_some() {
        return Err(format!(
            "--script does not apply to `backtest {}` — it names a .rhai FILE, whose knobs \
             `params` lists and which `script-check` compiles",
            a.sub.as_str()
        ));
    }
    if a.sub != ReadSub::Templates {
        refuse(
            &templates_only,
            "that flag SAVES a shipped starter to a new file, and `templates` is the verb that \
             emits one",
        )?;
    }
    if a.sub != ReadSub::Strategies {
        refuse(
            &strategies_only,
            "that flag names the COMPUTE DAEMON, and `strategies` is the one reading verb that \
             dials one — every other reads the run directory on this machine",
        )?;
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
        let hint = if a.sub == ReadSub::Templates {
            " — a starter is SAVED with --write-strategy, which refuses an existing path rather \
             than overwriting one"
        } else {
            ""
        };
        return Err(format!(
            "--out names a FILE to write a rendered document to, and `backtest {}` prints a line \
             the shell can already redirect{hint}",
            a.sub.as_str()
        ));
    }
    Ok(())
}

/// ⚠ **The return is a RUNG, not `()`, and TWO sub-verbs need that.** `gate`'s product IS its exit
/// code: it evaluates every criterion, prints the whole verdict to stdout, and the number beside it
/// is that document's summary (`crate::exit::Exit::Breach`). Routing a breach through `CliError`
/// instead would print one stderr line and throw the document away — §7.1 requires the opposite.
/// `script-check` is the second and the same shape: its product is a compile diagnostic plus a
/// number a save hook branches on, so it too prints and returns rather than erroring (it lands on
/// `crate::exit::Exit::Failed`, and `crate::cmd::strategies`'s `run_script_check` argues why not
/// `Usage` and why not `Breach`). Every other arm answers `Exit::Ok` and is unchanged.
pub(super) fn execute_read(
    a: &ReadArgs,
    ctx: &crate::cmd::runs::Ctx<'_>,
    now: i64,
) -> CmdResult<crate::exit::Exit> {
    let ok = |r: CmdResult<()>| r.map(|()| crate::exit::Exit::Ok);
    match a.sub {
        ReadSub::Ls => ok(crate::cmd::runs::ls::run_ls(ctx, a)),
        ReadSub::Show => ok(crate::cmd::runs::show::run_show(ctx, a)),
        ReadSub::Path => ok(crate::cmd::runs::path::run_path(
            ctx,
            a.selector.as_deref().unwrap_or_default(),
            a.file.as_deref(),
        )),
        ReadSub::Tag => ok(crate::cmd::runs::tag::run_tag(ctx, a, now)),
        ReadSub::Diff => ok(crate::cmd::runs::diff::run_diff(ctx, a)),
        ReadSub::Gate => crate::cmd::runs::gate::run_gate(ctx, a),
        ReadSub::Params => {
            ok(crate::cmd::params::run_params(a.script.as_deref(), a.strategy.as_deref(), a.json))
        }
        ReadSub::Strategies => {
            ok(crate::cmd::strategies::run_strategies(ctx, a.addr.as_deref(), a.json))
        }
        ReadSub::Templates => ok(crate::cmd::strategies::run_templates(
            a.selector.as_deref(),
            a.write_strategy.as_deref(),
            a.json,
        )),
        ReadSub::ScriptApi => ok(crate::cmd::strategies::run_script_api(a.json)),
        // ⚠ The SECOND arm that returns a rung rather than `()`. A script that does not compile is
        // an ANSWER — the diagnostic goes to stdout in full, `--json` document included — and the
        // number beside it is what a save hook or a pre-commit branches on.
        // `crate::cmd::strategies`'s `run_script_check` argues which rung and why not the other two.
        ReadSub::ScriptCheck => {
            crate::cmd::strategies::run_script_check(a.script.as_deref(), a.json)
        }
    }
}
