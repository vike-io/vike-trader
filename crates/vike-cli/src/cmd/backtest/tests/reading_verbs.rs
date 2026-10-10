//! The reading sub-verbs' parser, the surface-against-parser audit and the metric listing.

use super::*;

// ─── the READING sub-verbs (spec §6.1/§6.2/§6.5, §8.3, §8.5) ────────────────────────────────

/// Every reading sub-verb is reachable by the name the usage advertises. Copied from
/// `crate::cmd::data`'s `all_subcommands_are_reachable_by_the_name_they_advertise`, and it earns
/// its place the same way that one did: the hand-written roster it replaced omitted `rm`, a
/// subcommand that had shipped months earlier.
#[test]
fn every_reading_subcommand_is_reachable_by_the_name_it_advertises() {
    for sub in READ_SUBCOMMANDS {
        let argv = minimum_read_line(sub);
        let parsed = parse_read(&argv).unwrap_or_else(|e| panic!("{}: {e}", sub.as_str()));
        assert_eq!(parsed.sub, *sub, "{} parsed as a different subcommand", sub.as_str());
    }
}

/// The MINIMUM command line each reading sub-verb accepts.
///
/// ⚠ The minimum line, not a bare name: three of them REFUSE a command line that would do
/// nothing, so a bare token would prove only that the refusal fires. ONE copy, because two
/// tests drive it — the reachability check above, and
/// [`the_surface_and_the_parsers_agree_about_every_flag_and_sub_verb`] below, which needs a
/// line that already parses before it can ask whether one more flag is accepted on top of it.
fn minimum_read_line(sub: &ReadSub) -> Vec<&'static str> {
    match sub {
        ReadSub::Show | ReadSub::Path => vec![sub.as_str(), "@last"],
        ReadSub::Tag => vec![sub.as_str(), "@last", "--add", "ci"],
        ReadSub::Diff => vec![sub.as_str(), "@last", "1756000000-1-0"],
        ReadSub::Gate => {
            vec![sub.as_str(), "@last", "--against", "@baseline/m", "--fail-if", "sharpe:-5%"]
        }
        other => vec![other.as_str()],
    }
}

/// ⚠ **THE EXPORTED TABLE AND THE REAL PARSERS MUST AGREE ABOUT EVERY (flag, sub-verb) PAIR.**
///
/// [`crate::surface::FLAGS`] is hand-maintained, and until this test existed nothing held it
/// against the code it describes: every other gate over that table checks it for internal
/// consistency (a sample matches an arity, a roster resolves, a conditional default names a
/// real flag), and none of those can see a row that is simply WRONG about the parser.
///
/// MEASURED, and the reason this exists: `--json` shipped with `applies_to: &["run"]` and was
/// wrong about the EIGHT reading sub-verbs that also accept it. This plane has TWO parsers —
/// [`parse_run_args`] for `run`, [`parse_read`] for the reading family — and the row had only
/// ever looked at one. The published reference then told readers that a `gate --json` pipeline
/// named a flag that does not exist, and the documentation repository's own recipe gate, which
/// reads `applies_to`, would have REFUSED the correct line. A table that is wrong is worse than
/// no table at all, because everything downstream trusts it.
///
/// So acceptance is measured by DRIVING the parser rather than by reading it: the minimum line
/// for the sub-verb, plus the flag under test (with its own `sample` when it takes a value),
/// and whether the parse returns `Ok` is the answer. `--help` is excluded because it
/// short-circuits through the Err channel by design on every verb, and a
/// [`crate::surface::Status::Unbuilt`] flag is excluded because it accepts nowhere — where its
/// refusal REACHES is a separate field with its own gate.
///
/// ⚠ This is the shape to copy when the `data` and `trade` planes export their surfaces: a
/// per-plane table is only worth what a test that drives the parser it describes is worth.
#[test]
fn the_surface_and_the_parsers_agree_about_every_flag_and_sub_verb() {
    // A flag the parser accepts only ALONGSIDE another one. The probe must supply the
    // companion, or the refusal it gets back is about the missing companion rather than about
    // the flag under test — which would read as "the table is wrong" when the table is right.
    //
    // ⚠ The row is the `--local` form: `run --local --profile P [--engine PATH]` is what the
    // usage advertises, and it names a local engine binary, which means nothing when the work
    // is shipped to a daemon. (`--store DIR` was a second row until it was refused on both arms
    // on 2026-09-25; a retired row is not probed here.) Declared here rather than smoothed
    // over, because the CONDITIONALITY is itself a fact about the surface — and a row added to
    // silence a failure, rather than because a companion is genuinely required, would hide
    // exactly what this gate is for.
    const COMBINATION_GATED: &[(&str, &str)] = &[("--engine", "--local")];

    let accepts = |sub: &str, flag: &crate::surface::FlagRow| -> bool {
        let mut argv: Vec<&str> = if sub == "run" {
            let mut base = vec!["--profile", "p.toml"];
            if let Some((_, companion)) = COMBINATION_GATED.iter().find(|(f, _)| *f == flag.long) {
                base.push(companion);
            }
            base
        } else {
            let Some(read) = READ_SUBCOMMANDS.iter().find(|r| r.as_str() == sub) else {
                panic!("the surface names the sub-verb {sub:?}, which this plane does not have")
            };
            minimum_read_line(read)
        };
        argv.push(flag.long);
        if flag.value == crate::surface::Value::Required {
            argv.push(flag.sample.unwrap_or_else(|| {
                panic!("{} takes a value and the table gives no sample to drive", flag.long)
            }));
        }
        if sub == "run" {
            parse_run_args(argv.iter().map(|s| (*s).to_string())).is_ok()
        } else {
            parse_read(&argv).is_ok()
        }
    };

    // Every disagreement, collected before anything is asserted: a test that stops at the
    // first one turns a table-wide audit into one round trip per row.
    let mut wrong: Vec<String> = Vec::new();
    for flag in crate::surface::FLAGS {
        if flag.long == "--help" || flag.status != crate::surface::Status::Ships {
            continue;
        }
        for sub in crate::surface::SUB_VERBS {
            let declared = flag.applies_to.contains(sub);
            let real = accepts(sub, &flag);
            if declared != real {
                wrong.push(format!(
                    "{} on `{sub}`: the table says {}, the parser {}",
                    flag.long,
                    if declared { "it applies" } else { "it does NOT apply" },
                    if real { "ACCEPTS it" } else { "REFUSES it" },
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "crate::surface::FLAGS disagrees with the parsers about {} (flag, sub-verb) pair(s). \
             The PARSER is the authority — fix `applies_to`, or fix the parser if the table \
             describes the intended behaviour:\n  {}",
        wrong.len(),
        wrong.join("\n  "),
    );
}

/// `tag`'s three writes parse, and `--add` is REPEATABLE — the only repeatable flag in this
/// loop, because two labels are two labels rather than the second replacing the first.
#[test]
fn tag_parses_its_three_writes_and_add_repeats() {
    let a = parse_read(&[
        "tag",
        "@last",
        "--add",
        "ci",
        "--add",
        "fee-fix",
        "--note",
        "n",
        "--as",
        "baseline/m",
    ])
    .unwrap();
    assert_eq!(a.add, vec!["ci".to_string(), "fee-fix".to_string()]);
    assert_eq!(a.note.as_deref(), Some("n"));
    assert_eq!(a.mark_as.as_deref(), Some("baseline/m"));
}

/// ⚠ `diff` takes TWO runs and neither is optional. A one-operand `diff` that silently used
/// `@last` for the other side would compare against whatever happened to run most recently,
/// which is the silent precedence this grammar refuses everywhere else.
#[test]
fn diff_requires_both_operands_and_names_them() {
    let a = parse_read(&["diff", "1756000000-1-0", "@last"]).unwrap();
    assert_eq!(a.selector.as_deref(), Some("1756000000-1-0"));
    assert_eq!(a.file.as_deref(), Some("@last"), "the second positional is the RIGHT operand");
    let e = parse_read(&["diff", "@last"]).unwrap_err();
    assert!(e.contains("TWO runs") || e.contains("two runs"), "{e}");
    assert!(e.contains("<a> <b>"), "…and shows the shape: {e}");
}

/// `templates` takes an OPTIONAL starter id — absent is the roster — and at most one, because
/// a second positional is a shell-quoting accident worth naming rather than ignoring.
#[test]
fn templates_takes_an_optional_starter_id_and_at_most_one() {
    assert_eq!(parse_read(&["templates"]).unwrap().selector, None, "absent is the roster");
    assert_eq!(
        parse_read(&["templates", "sma-cross"]).unwrap().selector.as_deref(),
        Some("sma-cross")
    );
    let e = parse_read(&["templates", "sma-cross", "extra"]).unwrap_err();
    assert!(e.contains("extra"), "{e}");
}

/// ⚠ `--script` is the SECOND flag TWO sub-verbs own — `params` lists what a script declares
/// and `script-check` compiles it — so it is refused on the rest by a named rule whose sentence
/// mentions BOTH owners. Refusing it on `script-check` with the old `params`-only sentence
/// would have sent an operator to the wrong verb, which is the cost a named refusal exists to
/// avoid.
#[test]
fn script_belongs_to_params_and_script_check_and_to_nothing_else() {
    assert_eq!(
        parse_read(&["script-check", "--script", "s.rhai"]).unwrap().script.as_deref(),
        Some("s.rhai")
    );
    assert_eq!(
        parse_read(&["params", "--script", "s.rhai"]).unwrap().script.as_deref(),
        Some("s.rhai")
    );
    let e = parse_read(&["templates", "--script", "s.rhai"]).unwrap_err();
    assert!(e.contains("--script"), "{e}");
    assert!(e.contains("params"), "the refusal names the first owner: {e}");
    assert!(e.contains("script-check"), "…and the second: {e}");
}

/// ⚠ `--write-strategy` is `templates`' alone, and it is deliberately NOT `--out`: that flag
/// OVERWRITES on `ls`/`show` while this one refuses an existing path, and one flag may not
/// carry two clobber policies. So `--out` stays refused on `templates` even though `templates`
/// is the one reading verb that writes a file.
#[test]
fn write_strategy_belongs_to_templates_and_out_still_does_not() {
    assert_eq!(
        parse_read(&["templates", "x", "--write-strategy", "s.rhai"])
            .unwrap()
            .write_strategy
            .as_deref(),
        Some("s.rhai")
    );
    let e = parse_read(&["ls", "--write-strategy", "s.rhai"]).unwrap_err();
    assert!(e.contains("--write-strategy") && e.contains("templates"), "{e}");
    let e = parse_read(&["templates", "--out", "s.rhai"]).unwrap_err();
    assert!(e.contains("--out"), "{e}");
}

/// The authoring three refuse a `run` flag the ordinary way — "unknown option" — because none
/// of them shares `params`' named roster. Asserted so the audit's expectation is written down
/// somewhere a reader will find it.
#[test]
fn the_authoring_subverbs_refuse_a_run_flag_as_unknown() {
    for sub in ["templates", "script-api", "script-check"] {
        let e = parse_read(&[sub, "--cash", "1000"]).unwrap_err();
        assert!(e.contains("--cash"), "{sub}: {e}");
    }
}

/// ⚠ `--trades` is the FIRST flag TWO sub-verbs own — `show` renders the ledger and `diff`
/// compares two of them — so it is refused on every other reading verb rather than living in
/// either roster. (`--script` is the second, and has its own test above.)
#[test]
fn trades_belongs_to_show_and_diff_and_to_nothing_else() {
    assert!(parse_read(&["show", "@last", "--trades"]).unwrap().trades);
    assert!(parse_read(&["diff", "@last", "1756000000-1-0", "--trades"]).unwrap().trades);
    let e = parse_read(&["ls", "--trades"]).unwrap_err();
    assert!(e.contains("--trades"), "{e}");
    assert!(e.contains("show") && e.contains("diff"), "it names both owners: {e}");
}

/// A flag belonging to one of the three JUDGING verbs is refused by name on the others, with
/// the reason — never dropped, and never "unknown option".
#[test]
fn a_judging_flag_on_the_wrong_subverb_is_refused_by_name() {
    let e = parse_read(&["ls", "--as", "baseline/m"]).unwrap_err();
    assert!(e.contains("--as") && e.contains("tag"), "{e}");
    let e = parse_read(&["show", "@last", "--fail-if", "sharpe:-5%"]).unwrap_err();
    assert!(e.contains("--fail-if") && e.contains("gate"), "{e}");
    let e = parse_read(&["show", "@last", "--md"]).unwrap_err();
    assert!(e.contains("--md") && e.contains("diff"), "{e}");
}

/// The roster the usage advertises is DERIVED from the enum, never re-typed —
/// `crate::cmd::data`'s `SUBCOMMANDS` doc carries what the hand copy cost.
///
/// ⚠ It asserts the usage LINE (`backtest ls `), not the bare verb. `"ls"` is a substring of
/// `--cols` and `"params"` of `--list-params`, both of which [`USAGE`] carries for other
/// reasons — so a `contains("ls")` stays green with the whole `backtest ls …` line deleted, and
/// pins nothing while reading as coverage.
#[test]
fn the_usage_names_every_reading_subcommand() {
    for sub in READ_SUBCOMMANDS {
        let line = format!("backtest {} ", sub.as_str());
        assert!(USAGE.contains(&line), "USAGE has no `{line}…` line");
    }
}

/// `path` takes a selector and at most one FILE. A second positional is a shell-quoting accident
/// worth naming, not something to ignore.
#[test]
fn path_takes_a_selector_and_at_most_one_file() {
    assert_eq!(parse_read(&["path", "@last"]).unwrap().selector.as_deref(), Some("@last"));
    assert_eq!(parse_read(&["path", "@last", "report"]).unwrap().file.as_deref(), Some("report"));
    let e = parse_read(&["path", "@last", "report", "extra"]).unwrap_err();
    assert!(e.contains("extra"), "{e}");
}

/// A missing selector is a USAGE error naming the grammar, never a silent `@last`.
#[test]
fn path_without_a_selector_is_a_usage_error() {
    let e = parse_read(&["path"]).unwrap_err();
    assert!(e.contains("path"), "{e}");
}

/// A flag that belongs to a SIBLING sub-verb is refused BY NAME with the reason — never dropped,
/// and never reported as "unknown option", which would say the flag does not exist.
#[test]
fn a_sibling_subverbs_flag_is_refused_by_name() {
    let e = parse_read(&["show", "@last", "--sort", "sharpe"]).unwrap_err();
    assert!(e.contains("--sort"), "{e}");
    assert!(e.contains("listing"), "{e}");

    let e = parse_read(&["ls", "--metrics"]).unwrap_err();
    assert!(e.contains("--metrics"), "{e}");

    let e = parse_read(&["path", "@last", "--out", "x.json"]).unwrap_err();
    assert!(e.contains("--out"), "{e}");
}

/// ⚠ The §6.2 renderers this stage cannot build are refused with what they would NEED, not with
/// "unknown option". `--trades` is deliberately absent from that set: the run record grew
/// `trades.json`, so the flag ships.
#[test]
fn an_unbuilt_renderer_is_refused_with_what_it_would_need() {
    for flag in crate::cmd::runs::show::UNBUILT_RENDERERS {
        let e = parse_read(&["show", "@last", flag]).unwrap_err();
        assert!(e.contains(flag), "{flag}: {e}");
        assert!(!e.contains("unknown option"), "{flag}: {e}");
    }
    assert!(parse_read(&["show", "@last", "--trades"]).unwrap().trades);
}

/// The reading roster claims READING tokens and nothing else — which is what lets
/// [`claim_subcommand`] hand `run` to [`parse_run_args`] and a flag to the refusal that names
/// it as a flag. ⚠ It was a PEEK ahead of a flag-form fall-through when this was written;
/// stage 3 deleted that fall-through, so `--profile` here is no longer "not a sub-verb, carry
/// on" but "not a sub-verb, and a sub-verb is required".
#[test]
fn a_line_that_is_not_a_reading_subverb_is_not_claimed_by_the_read_parser() {
    assert!(ReadSub::from_token("--profile").is_none());
    assert!(ReadSub::from_token("--local").is_none());
    assert!(ReadSub::from_token("run").is_none(), "`run` is the computing sub-verb, not a read");
    assert_eq!(ReadSub::from_token("ls"), Some(ReadSub::Ls));
}

/// The reading sub-verbs are named in the verb's own summary, because that summary is what
/// `scripts/gen_skills.sh` renders into every SKILL.md verb table and what an agent reads before
/// it decides which command to name. A sub-verb that ships and is never advertised is a surface
/// only the source tree knows about.
#[test]
fn the_dispatcher_summary_names_the_reading_subverbs() {
    let summary = crate::COMMANDS
        .iter()
        .find(|(name, _)| *name == "backtest")
        .map(|(_, s)| *s)
        .expect("backtest is a registered command");
    // ⚠ Each sub-verb is asserted in the SPELLING this summary uses — the `ls|show|path`
    // shorthand, and the backticked `params`/`strategies` — rather than as a bare word. A bare
    // `contains("ls")` is the shape that goes green on an unrelated substring (`--cols`,
    // `--list-params`) and stops pinning anything the day the summary is reworded.
    for spelling in ["backtest ls|show|path", "`params`", "`strategies`"] {
        assert!(summary.contains(spelling), "the `backtest` summary omits {spelling}");
    }
    // ...and the ROSTER half, so a SIXTH sub-verb cannot ship unadvertised. The two together
    // are what the spelling list alone could not do: one pins the words, the other pins the set.
    for sub in READ_SUBCOMMANDS {
        assert!(summary.contains(sub.as_str()), "the `backtest` summary omits `{}`", sub.as_str());
    }
    // ⚠ `scripts/gen_skills.sh` renders this into a markdown TABLE CELL, so a literal `|` would
    // break the row. The one spelling allowed is the `ls|show|path` shorthand this summary uses.
    assert!(
        !summary.contains('|') || summary.contains("ls|show|path"),
        "a bare pipe breaks the rendered table: {summary}"
    );
}

/// ⚠ The MCP instructions name `vike-cli backtest run --local --profile run.toml`, and
/// `crate::cmd::mcp`'s `the_instructions_name_only_real_commands` holds every backticked word to
/// this USAGE as a SUBSTRING. Growing USAGE must not drop either flag.
///
/// ⚠ The `run` in that line is a SUB-VERB now, not decoration — decision 11 — and
/// `crate::cmd::mcp`'s `initialize_carries_instructions_that_name_the_surface_beyond_this_one`
/// carries the needle with the sub-verb in it, because a needle stopping at `vike-cli backtest`
/// would have kept passing over an invocation that is now an exit-2.
#[test]
fn the_usage_keeps_the_flags_the_mcp_instructions_name() {
    for word in ["--local", "--profile", "--script", "--json", "--addr"] {
        assert!(USAGE.contains(word), "USAGE dropped `{word}`, which the MCP instructions name");
    }
}

// ---- the metric catalog LISTING ------------------------------------------------------------

/// **`show --metrics-list` needs NO selector, and that is the property the flag exists for.**
///
/// ⚠ The mutation this fails on, in PRODUCTION: delete the `ReadSub::Show if a.metrics_list`
/// arm from [`parse_read`]. The combined `Show | Tag | Gate` arm below it then calls
/// `required_selector`, the first assertion reddens, and the listing becomes reachable only by
/// naming a run it does not read — on a box with no runs directory at all,
/// `crate::cmd::runs::show::run_show` would refuse before it ever printed the catalog.
#[test]
fn the_metric_listing_takes_no_selector_and_keeps_one_if_given() {
    let a = parse_read(&["show", "--metrics-list"]).expect("a listing needs no run");
    assert!(a.metrics_list);
    assert_eq!(a.selector, None, "no selector was given and none is required");

    // A selector given anyway is KEPT and unused — the listing is the answer either way, and
    // refusing it would be a refusal about a token rather than about a mistake.
    let a = parse_read(&["show", "@last", "--metrics-list"]).expect("a selector is allowed");
    assert!(a.metrics_list);
    assert_eq!(a.selector.as_deref(), Some("@last"));

    // …and the ordinary `show` still REQUIRES one, so the exemption is scoped to this flag.
    let e = parse_read(&["show"]).expect_err("a bare `show` names no run");
    assert!(e.contains("run selector"), "{e}");
}

/// ⚠ **The listing is refused beside every flag that renders the RUN, BY NAME, and beside
/// `--json` with its own sentence** — because [`crate::cmd::runs::show::run_show`] returns the
/// catalog from the top, so a combination would silently make the other flag do nothing.
///
/// It iterates the PRODUCTION array
/// (`crate::cmd::runs::show::run_rendering_flags_given`) rather than a literal list of its own,
/// and then asserts the two agree in LENGTH — so a fifth section flag added there without a
/// sample here stops this test with the reason instead of going unrefused.
///
/// ⚠ The mutation this fails on, in PRODUCTION: delete the
/// `crate::cmd::runs::show::refuse_a_listing_beside_a_run_rendering` call from [`parse_read`].
/// Every line below then parses `Ok` and `show <run> --metrics-list --trades` would print the
/// catalog while the operator waited for a ledger.
#[test]
fn the_listing_refuses_every_run_rendering_flag_by_name() {
    let empty = ReadArgs::empty(ReadSub::Show);
    let rendering = crate::cmd::runs::show::run_rendering_flags_given(&empty);
    let samples: Vec<(&str, Vec<&str>)> = vec![
        ("--metrics", vec!["--metrics"]),
        ("--trades", vec!["--trades"]),
        ("--config", vec!["--config"]),
        ("--export", vec!["--export", "trades"]),
        ("--html", vec!["--html"]),
    ];
    assert_eq!(
        samples.len(),
        rendering.len(),
        "a run-rendering flag was added to `run_rendering_flags_given` without a sample here — \
             add its row, do not delete the check"
    );
    for (flag, _) in rendering {
        let (_, argv) = samples
            .iter()
            .find(|(name, _)| *name == flag)
            .unwrap_or_else(|| panic!("{flag} has no sample argv in this test"));
        let mut line = vec!["show", "@last", "--metrics-list"];
        line.extend(argv.iter().copied());
        let e = parse_read(&line).unwrap_err();
        assert!(e.contains("--metrics-list"), "{flag}: the refusal names the listing: {e}");
        assert!(e.contains(flag), "{flag}: …and the flag it collided with: {e}");
        assert!(
            e.contains("two different documents"),
            "{flag}: …and says WHY neither wins rather than picking one: {e}"
        );
    }

    // `--json` is its own case with its own sentence: the listing has no JSON rendering at all,
    // so the message names what DOES answer the machine-readable question — the convention
    // `crate::cmd::runs::show::refuse_an_unbuilt_renderer`'s `--drawdowns` arm follows.
    let e = parse_read(&["show", "--metrics-list", "--json"]).unwrap_err();
    assert!(e.contains("--metrics-list") && e.contains("--json"), "{e}");
    assert!(e.contains("cli.json"), "…and names the asset that does carry the ids: {e}");

    // ⚠ `--out` COMPOSES — it names a file, not a document — so this must NOT be refused.
    assert!(parse_read(&["show", "--metrics-list", "--out", "catalog.txt"]).is_ok());

    // …and the listing is refused on every SIBLING reading verb by name, through `show_only`.
    // Driven off [`READ_SUBCOMMANDS`] and [`minimum_read_line`], so an ELEVENTH reading verb is
    // covered by existing — and each line already parses before the flag is added, or the
    // refusal that came back would be about the missing operand instead.
    for sub in READ_SUBCOMMANDS.iter().filter(|s| **s != ReadSub::Show) {
        let mut line = minimum_read_line(sub);
        line.push("--metrics-list");
        let e = parse_read(&line).unwrap_err();
        assert!(e.contains("--metrics-list"), "on `{}`: {e}", sub.as_str());
    }
}
